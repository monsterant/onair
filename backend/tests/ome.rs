//! OME AdmissionWebhooks 받는 곳(/ome/admission)의 동작 시험.
//! 진짜 서버나 DB 없이 라우터에 요청을 바로 넣어 본다(tower의 oneshot: 요청 하나를 보내고 응답을 받는다).

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit, Mac};
use onair_backend::{ome::OmeState, ome_routes};
use serde_json::{Value, json};
use sha1::Sha1;
use tower::ServiceExt;

const KEY: &str = "test-secret-for-ome-webhook";

fn router() -> Router {
    ome_routes(OmeState { secret: KEY.into() })
}

fn sign(key: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha1>::new_from_slice(key.as_bytes()).unwrap();
    mac.update(body);
    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

fn payload(status: &str, n: usize) -> Vec<u8> {
    json!({
        "client": {"address": "1.2.3.4", "port": 1},
        "request": {
            "direction": "incoming",
            "protocol": "webrtc",
            "status": status,
            "url": format!("ws://h:3333/app/s{n}"),
            "time": "2026-11-09T00:00:00.000+09:00"
        }
    })
    .to_string()
    .into_bytes()
}

async fn post(body: Vec<u8>, sig: Option<String>) -> (StatusCode, Value) {
    let mut req = Request::post("/ome/admission").header("content-type", "application/json");
    if let Some(s) = sig {
        req = req.header("X-OME-Signature", s);
    }
    let res = router()
        .oneshot(req.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn opening_is_allowed() {
    let body = payload("opening", 1);
    let (code, json) = post(body.clone(), Some(sign(KEY, &body))).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(json, json!({ "allowed": true }));
}

#[tokio::test]
async fn closing_gets_empty_object() {
    let body = payload("closing", 1);
    let (code, json) = post(body.clone(), Some(sign(KEY, &body))).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(json, json!({}));
}

#[tokio::test]
async fn missing_signature_is_401() {
    let (code, json) = post(payload("opening", 1), None).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED);
    assert_eq!(json, Value::Null);
}

#[tokio::test]
async fn wrong_key_or_tampered_body_is_401() {
    let body = payload("opening", 1);
    let (code, _) = post(body.clone(), Some(sign("other-key", &body))).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED);

    // 다른 본문에 붙어 있던 서명(본문을 바꿔치기한 경우)
    let (code, _) = post(payload("opening", 2), Some(sign(KEY, &body))).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn signed_but_broken_json_is_400() {
    let body = br#"{"request":{}}"#.to_vec();
    let (code, _) = post(body.clone(), Some(sign(KEY, &body))).await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn unknown_status_is_rejected_not_allowed() {
    for status in ["garbage", "CLOSING", ""] {
        let body = payload(status, 1);
        let (code, json) = post(body.clone(), Some(sign(KEY, &body))).await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "status={status:?}");
        assert_ne!(json["allowed"], true, "status={status:?}");
    }
}
