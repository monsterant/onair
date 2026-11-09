//! OvenMediaEngine(OME) AdmissionWebhooks 받는 곳 (#10 시험).
//!
//! OME는 누가 송출(incoming)하거나 시청(outgoing)을 열고 닫을 때마다 여기로 POST를 보낸다.
//! - opening: "열어도 되나요?" → `allowed`로 답한다. 시험 단계에서는 모두 허락한다.
//! - closing: "닫혔어요" 알림 → 빈 JSON으로 답한다. 방송자가 탭을 닫았을 때 서버가 알 수 있는지 여기서 확인한다.
//!
//! 요청 본문은 `X-OME-Signature` 헤더로 서명돼 온다(HMAC-SHA1, base64 url-safe).
//! 서명이 틀리면 누군가 OME인 척 보낸 것이라 거절한다.

use std::sync::{Arc, Mutex};

use axum::{
    Json,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha1::Sha1;

/// 시험 화면에서 보려고 최근 이벤트를 메모리에 모아 둔다. 서버를 다시 켜면 사라진다(시험용).
#[derive(Clone, Default)]
pub struct EventLog(pub Arc<Mutex<Vec<AdmissionEvent>>>);

#[derive(Clone, Serialize, Debug)]
pub struct AdmissionEvent {
    pub direction: String,
    pub protocol: String,
    pub status: String,
    pub url: String,
    pub time: String,
}

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
    time: String,
}

#[derive(Clone)]
pub struct OmeState {
    pub secret: String,
    pub events: EventLog,
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
    tracing::info!(
        direction = %r.direction, protocol = %r.protocol, status = %r.status, url = %r.url,
        "OME admission"
    );
    // 답을 먼저 정한다. 모르는 status는 기록하지 않고 거절한다.
    let reply = match r.status.as_str() {
        // 닫힘 알림에는 빈 JSON으로 답한다(OME 문서).
        "closing" => json!({}),
        // 시험 단계: 열림은 모두 허락. 실제 권한 검사(방송 소유자 확인)는 #4에서 넣는다.
        "opening" => json!({ "allowed": true, "reason": "spike: allow all" }),
        // 모르는 값은 허락하지 않는다. 검사를 빠져나가는 길이 되지 않게 하려는 것이다.
        _ => return Err(StatusCode::BAD_REQUEST),
    };

    if let Ok(mut log) = state.events.0.lock() {
        log.push(AdmissionEvent {
            direction: r.direction,
            protocol: r.protocol,
            status: r.status,
            url: r.url,
            time: r.time,
        });
        let len = log.len();
        if len > 200 {
            log.drain(0..len - 200);
        }
    }
    Ok(Json(reply))
}

pub async fn events(State(state): State<OmeState>) -> Json<Vec<AdmissionEvent>> {
    Json(state.events.0.lock().map(|l| l.clone()).unwrap_or_default())
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
