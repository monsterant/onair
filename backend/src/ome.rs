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
    /// 송출이 끊기면 그 방송의 녹화를 바로 멈추는 데 쓴다(#47). 설정하지 않으면(Default) 부르지 않는다.
    pub ome: crate::ome_api::OmeApi,
}

/// OME가 알려 준 주소에서 방송 이름과 송출 키를 꺼낸다.
/// 예) http://h:3333/app/abc123?direction=whip&key=K → ("abc123", Some("K"))
///     https://h/app/abc123/llhls.m3u8 → ("abc123", None)
///
/// 주소는 OME가 받은 그대로의 글자로 자른다(#27 V1).
/// - URL 라이브러리는 `/app/A/../B`를 `/app/B`로 정리하지만, OME는 정리하지 않고 A로 스트림을 만든다.
///   그래서 정리된 주소로 검사하면 "B의 키로 A 이름을 차지"할 수 있었다.
/// - 경로는 정확히 `/app/이름` 또는 `/app/이름/파일`만 받는다. `.`, `..`, `%`, `\`, 빈 칸이 있으면 거절한다.
/// - 이름은 우리가 만든 형식(16진수 소문자 16자)만 받는다.
/// - `key`가 두 번 나오면 어느 쪽을 OME가 쓰는지 알 수 없으므로 거절한다.
pub fn parse_stream(url: &str) -> Option<(String, Option<String>)> {
    let (_, after_scheme) = url.split_once("://")?;
    let path_and_query = &after_scheme[after_scheme.find('/')?..];
    if path_and_query.contains(['#', '\\']) {
        return None;
    }
    let (path, query) = match path_and_query.split_once('?') {
        Some((p, q)) => (p, q),
        None => (path_and_query, ""),
    };
    let segs: Vec<&str> = path[1..].split('/').collect();
    let ok_seg = |s: &&str| !s.is_empty() && *s != "." && *s != ".." && !s.contains('%');
    if !segs.iter().all(ok_seg) {
        return None;
    }
    let name = match segs.as_slice() {
        ["app", name] => *name,
        ["app", name, file] if is_plain_file(file) => *name,
        _ => return None,
    };
    let valid = name.len() == 16
        && name
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c));
    if !valid {
        return None;
    }
    let keys: Vec<String> = url::form_urlencoded::parse(query.as_bytes())
        .filter(|(k, _)| k == "key")
        .map(|(_, v)| v.into_owned())
        .collect();
    let key = match keys.len() {
        0 => None,
        1 => keys.into_iter().next(),
        _ => return None,
    };
    Some((name.to_string(), key))
}

/// 시청 주소의 파일 이름(예: llhls.m3u8): 영문·숫자·`_`·`-`·`.`만, 점으로 시작하지 않음
fn is_plain_file(f: &str) -> bool {
    !f.starts_with('.')
        && f.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
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
            // ended 확인은 아래 UPDATE 조건이 맡는다(확인과 갱신 사이에 종료돼도 막히도록 한 곳에서).
            if !same_key(&key, &stored) || status == "ended" {
                return Ok(false);
            }
            // 종료되지 않은 경우에만 live로 바꾼다. 그 사이 종료됐으면 바뀐 행이 없으니 거절한다.
            // 이미 live면 시작 시각을 그대로 둔다. 다시 붙을 때마다 바꾸면 맞춰 보기의 20초 유예가 계속 늘어난다(#27 V18).
            let changed = sqlx::query(
                "UPDATE broadcasts
                    SET started_at = CASE WHEN status = 'live' AND started_at IS NOT NULL
                                          THEN started_at ELSE now() END,
                        status = 'live', updated_at = now()
                  WHERE stream_name = $1 AND status <> 'ended' AND stream_key = $2",
            )
            .bind(&name)
            // 키를 다시 확인한다. 확인과 갱신 사이에 송출 키를 다시 만들었으면 옛 키로는 live가 되지 않는다(#28 리뷰)
            .bind(&stored)
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
async fn on_closing(
    db: &sqlx::PgPool,
    api: &crate::ome_api::OmeApi,
    direction: &str,
    url: &str,
) -> Result<(), sqlx::Error> {
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
        // 송출이 끊기면 그 방송의 녹화를 바로 멈춘다(#47 공격 사냥).
        // 15초 맞춰 보기를 기다리면, 그 사이 다시 붙은 송출을 OME가 같은 녹화·같은 파일에 이어 써서 앞부분을 덮어쓴다.
        let recording: Vec<String> = sqlx::query_scalar(
            "SELECT r.record_id FROM recordings r JOIN broadcasts b ON b.id = r.broadcast_id
              WHERE b.stream_name = $1 AND r.status = 'recording'",
        )
        .bind(&name)
        .fetch_all(db)
        .await?;
        for rid in recording {
            if api.stop_record(&rid).await {
                sqlx::query(
                    "UPDATE recordings SET status = 'finishing', ended_at = now()
                      WHERE record_id = $1 AND status = 'recording'",
                )
                .bind(&rid)
                .execute(db)
                .await?;
            }
        }
    }
    Ok(())
}

/// OME에 실제로 있는 스트림과 DB 상태를 맞춘다(15초마다 main.rs에서 부른다).
/// - DB는 `live`인데 OME에 스트림이 없으면 `ready`로 돌린다. 막 시작해서 아직 목록에 안 잡힌 것을 위해 20초는 기다린다.
/// - DB는 `ready`인데 OME에 스트림이 있으면 `live`로 되돌린다. 늦게 온 closing이 다시 붙은 송출을 ready로 만든 경우다(#27 V3).
/// - OME에 스트림이 있는데 DB에 끝나지 않은 방송이 없으면(종료됨·삭제됨·모르는 이름) 끊는다(#27 V4).
/// - OME 목록을 받지 못하면(꺼짐, 401, 500 등) 아무것도 바꾸지 않는다.
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
    sqlx::query(
        "UPDATE broadcasts SET status = 'live', started_at = now(), updated_at = now()
          WHERE status = 'ready' AND stream_name = ANY($1)",
    )
    .bind(&present)
    .execute(db)
    .await?;
    let orphans: Vec<String> = sqlx::query_scalar(
        "SELECT n FROM unnest($1::text[]) AS n
          WHERE NOT EXISTS (SELECT 1 FROM broadcasts b WHERE b.stream_name = n AND b.status <> 'ended')",
    )
    .bind(&present)
    .fetch_all(db)
    .await?;
    for name in orphans {
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
            on_closing(&state.db, &state.ome, &r.direction, &r.url)
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
