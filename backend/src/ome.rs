//! OvenMediaEngine(OME) AdmissionWebhooks 받는 곳 (#10에서 시험한 것을 옮김).
//!
//! OME는 누가 송출(incoming)하거나 시청(outgoing)을 열고 닫을 때마다 여기로 POST를 보낸다.
//! - opening: "열어도 되나요?" → `allowed`로 답한다(#3, #4).
//!   - 송출(incoming): 주소의 송출 키가 그 방송의 키와 같고, 방송이 종료되지 않았을 때만 허락한다.
//!     송출 키는 소유자에게만 보여 주므로, "로그인한 소유자만 송출"이 된다. 허락하면 방송을 `live`로 바꾼다.
//!   - 시청(outgoing): 방송이 `live`일 때만 허락한다. 로그인은 보지 않는다(시청은 누구나).
//!   - 그 밖의 모든 경우(이름 없음, 형식이 이상한 주소, 모르는 방향)는 거절한다.
//! - closing: "닫혔어요" 알림 → 빈 JSON으로 답한다. 송출이 닫히면 `live`를 `ready`로 돌린다.
//!   송출이 실패하거나 LL-HLS로 보던 사람이 나가도 closing이 오지 않을 수 있다(#10 시험).
//!   그래서 15초마다 OME 스트림 목록과 맞춰 본다(`reconcile`).
//!
//! 요청 본문은 `X-OME-Signature` 헤더로 서명돼 온다(HMAC-SHA1, base64 url-safe).
//! 서명이 틀리면 누군가 OME인 척 보낸 것이라 거절한다.

use axum::{
    Json,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use serde_json::{Value, json};
use sha1::Sha1;

#[derive(Deserialize)]
struct Payload {
    request: RequestPart,
}

#[derive(Deserialize)]
struct RequestPart {
    direction: String,
    protocol: String,
    status: String,
    url: String,
}

#[derive(Clone)]
pub struct OmeState {
    pub secret: String,
    pub db: sqlx::PgPool,
}

/// OME가 알려 준 주소에서 방송 이름과 송출 키를 꺼낸다.
/// 예) http://h:3333/app/abc123?direction=whip&key=K → ("abc123", Some("K"))
///     https://h/app/abc123/llhls.m3u8 → ("abc123", None)
/// 이름은 우리가 만든 형식(영문 소문자·숫자 16자)만 받는다. 아니면 None.
pub fn parse_stream(url: &str) -> Option<(String, Option<String>)> {
    let u = url::Url::parse(url).ok()?;
    let mut seg = u.path_segments()?;
    if seg.next()? != "app" {
        return None;
    }
    let name = seg.next()?;
    let valid = name.len() == 16
        && name
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c));
    if !valid {
        return None;
    }
    let key = u
        .query_pairs()
        .find(|(k, _)| k == "key")
        .map(|(_, v)| v.into_owned());
    Some((name.to_string(), key))
}

/// 길이가 같으면 끝까지 다 비교한다(앞 글자가 맞는 만큼 빨리 끝나서 키를 한 글자씩 알아내는 것을 막는다).
fn same_key(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// 로그에 남길 주소. 송출 키가 로그에 남지 않게 물음표 뒤를 뗀다.
fn redact(url: &str) -> &str {
    url.split('?').next().unwrap_or("")
}

/// 열림 요청을 허락할지 정하고, 송출이면 방송 상태를 `live`로 바꾼다.
async fn decide_opening(
    db: &sqlx::PgPool,
    direction: &str,
    url: &str,
) -> Result<bool, sqlx::Error> {
    let Some((name, key)) = parse_stream(url) else {
        return Ok(false);
    };
    match direction {
        "incoming" => {
            let Some(key) = key else { return Ok(false) };
            let row: Option<(String, String)> =
                sqlx::query_as("SELECT stream_key, status FROM broadcasts WHERE stream_name = $1")
                    .bind(&name)
                    .fetch_optional(db)
                    .await?;
            let Some((stored, status)) = row else {
                return Ok(false);
            };
            if !same_key(&key, &stored) || status == "ended" {
                return Ok(false);
            }
            // 종료되지 않은 경우에만 live로 바꾼다. 그 사이 종료됐으면 바뀐 행이 없으니 거절한다.
            let changed = sqlx::query(
                "UPDATE broadcasts SET status = 'live', started_at = now(), updated_at = now()
                  WHERE stream_name = $1 AND status <> 'ended'",
            )
            .bind(&name)
            .execute(db)
            .await?
            .rows_affected();
            Ok(changed == 1)
        }
        "outgoing" => {
            let live: Option<bool> =
                sqlx::query_scalar("SELECT status = 'live' FROM broadcasts WHERE stream_name = $1")
                    .bind(&name)
                    .fetch_optional(db)
                    .await?;
            Ok(live == Some(true))
        }
        _ => Ok(false),
    }
}

/// 송출이 닫히면 `live`였던 방송을 `ready`로 돌린다(종료된 방송은 그대로 둔다).
async fn on_closing(db: &sqlx::PgPool, direction: &str, url: &str) -> Result<(), sqlx::Error> {
    if direction != "incoming" {
        return Ok(());
    }
    if let Some((name, _)) = parse_stream(url) {
        sqlx::query(
            "UPDATE broadcasts SET status = 'ready', updated_at = now()
              WHERE stream_name = $1 AND status = 'live'",
        )
        .bind(&name)
        .execute(db)
        .await?;
    }
    Ok(())
}

/// OME에 실제로 있는 스트림과 DB 상태를 맞춘다(15초마다 main.rs에서 부른다).
/// - DB는 `live`인데 OME에 스트림이 없으면 `ready`로 돌린다. 막 시작해서 아직 목록에 안 잡힌 것을 위해 20초는 기다린다.
/// - 종료된 방송인데 OME에 스트림이 남아 있으면 끊는다.
pub async fn reconcile(db: &sqlx::PgPool, api: &crate::ome_api::OmeApi) -> Result<(), sqlx::Error> {
    let Some(present) = api.list_streams().await else {
        return Ok(());
    };
    let present: Vec<String> = present.into_iter().collect();
    sqlx::query(
        "UPDATE broadcasts SET status = 'ready', updated_at = now()
          WHERE status = 'live' AND started_at < now() - interval '20 seconds'
            AND NOT (stream_name = ANY($1))",
    )
    .bind(&present)
    .execute(db)
    .await?;
    let ended: Vec<String> = sqlx::query_scalar(
        "SELECT stream_name FROM broadcasts WHERE status = 'ended' AND stream_name = ANY($1)",
    )
    .bind(&present)
    .fetch_all(db)
    .await?;
    for name in ended {
        api.stop_stream(&name).await;
    }
    Ok(())
}

/// 저장소에 공개된 예시 키는 모두 이 글자로 시작한다. 이렇게 시작하는 값으로는 서버를 띄우지 않는다.
pub const EXAMPLE_SECRET_PREFIX: &str = "change-me";

/// 서명 키가 쓸 만한지 보고, 앞뒤 공백을 뗀 값을 돌려준다(서명에도 이 값을 쓴다).
/// 비었거나, 16글자보다 짧거나, 공개된 예시 값(대소문자 무시)이면 거절한다.
pub fn check_secret(secret: &str) -> Result<&str, &'static str> {
    let s = secret.trim();
    if s.is_empty() {
        return Err("값이 비어 있습니다. .env에 OME_ADMISSION_SECRET을 넣으세요");
    }
    if s.to_ascii_lowercase().starts_with(EXAMPLE_SECRET_PREFIX) {
        return Err("공개된 예시 값입니다. 임의의 긴 값으로 바꾸세요");
    }
    if s.chars().count() < 16 {
        return Err("16글자보다 짧습니다");
    }
    Ok(s)
}

/// OME 서명 확인. OME 문서: payload를 SecretKey로 HMAC-SHA1 → base64 url-safe.
/// 패딩(`=`) 유무가 문서에 없어서, 패딩을 떼고 비교한다.
pub fn signature_ok(secret: &str, body: &[u8], header: &str) -> bool {
    let Ok(expected) = URL_SAFE_NO_PAD.decode(header.trim_end_matches('=')) else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha1>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&expected).is_ok()
}

pub async fn admission(
    State(state): State<OmeState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, StatusCode> {
    let sig = headers
        .get("x-ome-signature")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    if !signature_ok(&state.secret, &body, sig) {
        tracing::warn!("OME admission: 서명이 맞지 않아 거절");
        return Err(StatusCode::UNAUTHORIZED);
    }

    let payload: Payload = serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
    let r = payload.request;
    let db_err = |e: sqlx::Error| {
        tracing::error!(error = %e, "OME admission: DB 오류");
        StatusCode::INTERNAL_SERVER_ERROR
    };
    match r.status.as_str() {
        // 닫힘 알림에는 빈 JSON으로 답한다(OME 문서).
        "closing" => {
            tracing::info!(direction = %r.direction, protocol = %r.protocol, url = redact(&r.url), "OME closing");
            on_closing(&state.db, &r.direction, &r.url)
                .await
                .map_err(db_err)?;
            Ok(Json(json!({})))
        }
        "opening" => {
            let allowed = decide_opening(&state.db, &r.direction, &r.url)
                .await
                .map_err(db_err)?;
            tracing::info!(
                direction = %r.direction, protocol = %r.protocol, url = redact(&r.url), allowed,
                "OME opening"
            );
            Ok(Json(json!({ "allowed": allowed })))
        }
        // 모르는 값은 허락하지 않는다. 검사를 빠져나가는 길이 되지 않게 하려는 것이다.
        _ => Err(StatusCode::BAD_REQUEST),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sign(secret: &str, body: &[u8]) -> String {
        let mut mac = Hmac::<Sha1>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    }

    #[test]
    fn accepts_correct_signature_with_or_without_padding() {
        let body = br#"{"request":{}}"#;
        let sig = sign("k", body);
        assert!(signature_ok("k", body, &sig));
        assert!(signature_ok("k", body, &format!("{sig}=")));
    }

    #[test]
    fn rejects_empty_short_or_example_secret() {
        assert_eq!(
            check_secret(""),
            Err("값이 비어 있습니다. .env에 OME_ADMISSION_SECRET을 넣으세요")
        );
        assert!(check_secret("   ").is_err());
        // 공백을 붙인 예시 값도 거절(앞뒤 공백을 떼고 본다)
        assert!(check_secret("  change-me-local-only  ").is_err());
        // 공개 예시 값과 그 변형
        assert!(check_secret("change-me").is_err());
        assert!(check_secret("change-me-local-only").is_err());
        assert!(check_secret("CHANGE-ME-LOCAL-ONLY").is_err());
        assert!(check_secret("change-me-local-only1").is_err());
        // 길이 경계: 15글자는 거절, 16글자는 통과. 한글은 바이트가 아니라 글자로 센다.
        assert!(check_secret("abcdefghijklmno").is_err());
        assert_eq!(check_secret("abcdefghijklmnop"), Ok("abcdefghijklmnop"));
        assert!(check_secret("가나다라마바").is_err());
        // 앞뒤 공백은 떼고 돌려준다(서명에 쓸 값)
        assert_eq!(
            check_secret("  a-long-random-secret-1234 \n"),
            Ok("a-long-random-secret-1234")
        );
    }

    #[test]
    fn rejects_wrong_key_or_tampered_body() {
        let body = br#"{"request":{}}"#;
        let sig = sign("k", body);
        assert!(!signature_ok("other", body, &sig));
        assert!(!signature_ok("k", br#"{"request":{"x":1}}"#, &sig));
        assert!(!signature_ok("k", body, "not-base64!!"));
    }
}
