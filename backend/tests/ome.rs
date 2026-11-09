//! OME AdmissionWebhooks 받는 곳(/ome/admission)의 동작 시험.
//! 진짜 서버나 DB 없이 라우터에 요청을 바로 넣어 본다(tower의 oneshot: 요청 하나를 보내고 응답을 받는다).

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit, Mac};
use onair_backend::{
    ome::{EventLog, OmeState},
    ome_routes,
};
use serde_json::{Value, json};
use sha1::Sha1;
use tower::ServiceExt;

const KEY: &str = "test-secret-for-ome-webhook";

fn router(events: EventLog) -> Router {
    ome_routes(OmeState {
        secret: KEY.into(),
        events,
    })
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

async fn post(r: Router, body: Vec<u8>, sig: Option<String>) -> (StatusCode, Value) {
    let mut req = Request::post("/ome/admission").header("content-type", "application/json");
    if let Some(s) = sig {
        req = req.header("X-OME-Signature", s);
    }
    let res = r
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

fn recorded(events: &EventLog) -> usize {
    events.0.lock().unwrap().len()
}

#[tokio::test]
async fn opening_is_allowed_and_recorded() {
    let events = EventLog::default();
    let body = payload("opening", 1);
    let (code, json) = post(router(events.clone()), body.clone(), Some(sign(KEY, &body))).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(json["allowed"], true);
    let log = events.0.lock().unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].status, "opening");
    assert_eq!(log[0].direction, "incoming");
    assert_eq!(log[0].url, "ws://h:3333/app/s1");
}

#[tokio::test]
async fn closing_gets_empty_object() {
    let body = payload("closing", 1);
    let (code, json) = post(
        router(EventLog::default()),
        body.clone(),
        Some(sign(KEY, &body)),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(json, json!({}));
}

#[tokio::test]
async fn missing_signature_is_401_and_not_recorded() {
    let events = EventLog::default();
    let (code, _) = post(router(events.clone()), payload("opening", 1), None).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED);
    assert_eq!(recorded(&events), 0);
}

#[tokio::test]
async fn wrong_key_or_tampered_body_is_401_and_not_recorded() {
    let events = EventLog::default();
    let body = payload("opening", 1);
    let (code, _) = post(
        router(events.clone()),
        body.clone(),
        Some(sign("other-key", &body)),
    )
    .await;
    assert_eq!(code, StatusCode::UNAUTHORIZED);

    let other = payload("opening", 2);
    let (code, _) = post(router(events.clone()), other, Some(sign(KEY, &body))).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED);
    assert_eq!(recorded(&events), 0);
}

#[tokio::test]
async fn signed_but_broken_json_is_400() {
    let body = br#"{"request":{}}"#.to_vec();
    let (code, _) = post(
        router(EventLog::default()),
        body.clone(),
        Some(sign(KEY, &body)),
    )
    .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn unknown_status_is_rejected_not_allowed() {
    let events = EventLog::default();
    for status in ["garbage", "CLOSING", ""] {
        let body = payload(status, 1);
        let (code, _) = post(router(events.clone()), body.clone(), Some(sign(KEY, &body))).await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "status={status:?}");
    }
    // 거절한 요청은 기록에 남기지 않는다
    assert_eq!(recorded(&events), 0);
}

#[tokio::test]
async fn events_endpoint_lists_recorded_events() {
    let events = EventLog::default();
    let body = payload("opening", 7);
    post(router(events.clone()), body.clone(), Some(sign(KEY, &body))).await;
    let res = router(events)
        .oneshot(Request::get("/ome/events").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let list: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(list.as_array().map(Vec::len), Some(1));
    assert_eq!(list[0]["url"], "ws://h:3333/app/s7");
}

#[tokio::test]
async fn keeps_only_latest_200_events() {
    let events = EventLog::default();
    for n in 0..201 {
        let body = payload("opening", n);
        let (code, _) = post(router(events.clone()), body.clone(), Some(sign(KEY, &body))).await;
        assert_eq!(code, StatusCode::OK);
    }
    let log = events.0.lock().unwrap();
    assert_eq!(log.len(), 200);
    // 가장 오래된 것(s0)이 밀려나고 s1부터 남는다
    assert_eq!(log[0].url, "ws://h:3333/app/s1");
}
