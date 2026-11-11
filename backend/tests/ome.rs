//! OME AdmissionWebhooks 받는 곳(/ome/admission)의 동작 시험.
//! 라우터에 요청을 바로 넣어 본다(tower의 oneshot). 송출·시청 허락은 DB의 방송을 보고 정하므로 `#[sqlx::test]`를 쓴다.

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit, Mac};
use onair_backend::{
    ome::{OmeState, parse_stream},
    ome_routes,
};
use serde_json::{Value, json};
use sha1::Sha1;
use sqlx::PgPool;
use tower::ServiceExt;

const KEY: &str = "test-secret-for-ome-webhook";
const NAME: &str = "0123456789abcdef";
const SKEY: &str = "k3y0000000000000000000000000000000000000000000000000000000000001";

fn router(db: PgPool) -> Router {
    ome_routes(OmeState {
        secret: KEY.into(),
        db,
        ome: Default::default(),
        kick: Default::default(),
    })
}

fn sign(key: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha1>::new_from_slice(key.as_bytes()).unwrap();
    mac.update(body);
    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

fn payload(direction: &str, status: &str, url: &str) -> Vec<u8> {
    json!({
        "client": {"address": "1.2.3.4", "port": 1},
        "request": {
            "direction": direction,
            "protocol": "webrtc",
            "status": status,
            "url": url,
            "time": "2026-11-09T00:00:00.000+09:00"
        }
    })
    .to_string()
    .into_bytes()
}

fn publish_url(name: &str, key: &str) -> String {
    format!("http://h:3333/app/{name}?direction=whip&key={key}")
}

async fn post(r: &Router, body: Vec<u8>, sig: Option<String>) -> (StatusCode, Value) {
    let mut req = Request::post("/ome/admission").header("content-type", "application/json");
    if let Some(s) = sig {
        req = req.header("X-OME-Signature", s);
    }
    let res = r
        .clone()
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

async fn signed(r: &Router, direction: &str, status: &str, url: &str) -> (StatusCode, Value) {
    let body = payload(direction, status, url);
    let sig = sign(KEY, &body);
    post(r, body, Some(sig)).await
}

/// 방송 하나를 만든다(소유자 1명 포함)
async fn seed(db: &PgPool, status: &str) {
    sqlx::query("INSERT INTO users (username, password_hash) VALUES ('owner', 'x')")
        .execute(db)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO broadcasts (owner_id, title, stream_name, stream_key, status)
         SELECT id, '방송', $1, $2, $3 FROM users WHERE username = 'owner'",
    )
    .bind(NAME)
    .bind(SKEY)
    .bind(status)
    .execute(db)
    .await
    .unwrap();
}

async fn status_of(db: &PgPool) -> String {
    sqlx::query_scalar("SELECT status FROM broadcasts WHERE stream_name = $1")
        .bind(NAME)
        .fetch_one(db)
        .await
        .unwrap()
}

// ---------- 서명 ----------

#[sqlx::test]
async fn missing_or_wrong_signature_is_401(db: PgPool) {
    seed(&db, "ready").await;
    let r = router(db.clone());
    let body = payload("incoming", "opening", &publish_url(NAME, SKEY));
    let (code, _) = post(&r, body.clone(), None).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED);
    let (code, _) = post(&r, body.clone(), Some(sign("other-key", &body))).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED);
    // 다른 본문에 붙어 있던 서명(본문 바꿔치기)
    let other = payload("incoming", "opening", &publish_url(NAME, "x"));
    let (code, _) = post(&r, other, Some(sign(KEY, &body))).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED);
    // 서명이 틀린 요청은 상태를 바꾸지 않는다
    assert_eq!(status_of(&db).await, "ready");
}

#[sqlx::test]
async fn signed_but_broken_json_is_400(db: PgPool) {
    let body = br#"{"request":{}}"#.to_vec();
    let (code, _) = post(&router(db), body.clone(), Some(sign(KEY, &body))).await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn unknown_status_is_rejected(db: PgPool) {
    seed(&db, "ready").await;
    let r = router(db.clone());
    for status in ["garbage", "CLOSING", ""] {
        let (code, json) = signed(&r, "incoming", status, &publish_url(NAME, SKEY)).await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "status={status:?}");
        assert_ne!(json["allowed"], true);
    }
    assert_eq!(status_of(&db).await, "ready");
}

// ---------- 송출 허락 ----------

#[sqlx::test]
async fn publish_with_right_key_is_allowed_and_goes_live(db: PgPool) {
    seed(&db, "ready").await;
    let r = router(db.clone());
    let (code, json) = signed(&r, "incoming", "opening", &publish_url(NAME, SKEY)).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(json, json!({ "allowed": true }));
    assert_eq!(status_of(&db).await, "live");
}

#[sqlx::test]
async fn publish_with_wrong_or_missing_key_is_denied(db: PgPool) {
    seed(&db, "ready").await;
    let r = router(db.clone());
    let wrong = format!("{}0", &SKEY[..SKEY.len() - 1]);
    for url in [
        publish_url(NAME, &wrong),
        publish_url(NAME, ""),
        format!("http://h:3333/app/{NAME}?direction=whip"),
        publish_url(NAME, &SKEY[..10]),
    ] {
        let (code, json) = signed(&r, "incoming", "opening", &url).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(json, json!({ "allowed": false }), "{url}");
    }
    assert_eq!(status_of(&db).await, "ready");
}

#[sqlx::test]
async fn ended_broadcast_cannot_publish_again(db: PgPool) {
    seed(&db, "ended").await;
    let (_, json) = signed(
        &router(db.clone()),
        "incoming",
        "opening",
        &publish_url(NAME, SKEY),
    )
    .await;
    assert_eq!(json, json!({ "allowed": false }));
    assert_eq!(status_of(&db).await, "ended");
}

#[sqlx::test]
async fn unknown_or_malformed_stream_is_denied(db: PgPool) {
    seed(&db, "live").await;
    let r = router(db);
    for url in [
        publish_url("ffffffffffffffff", SKEY),
        publish_url("0123456789ABCDEF", SKEY),
        publish_url("한글이름", SKEY),
        format!("http://h:3333/other/{NAME}?key={SKEY}"),
        "not a url".to_string(),
    ] {
        for dir in ["incoming", "outgoing"] {
            let (_, json) = signed(&r, dir, "opening", &url).await;
            assert_eq!(json, json!({ "allowed": false }), "{dir} {url}");
        }
    }
    // 모르는 방향도 거절
    let (_, json) = signed(&r, "sideways", "opening", &publish_url(NAME, SKEY)).await;
    assert_eq!(json, json!({ "allowed": false }));
}

// ---------- 시청 허락 ----------

#[sqlx::test]
async fn viewing_is_allowed_only_while_live(db: PgPool) {
    seed(&db, "ready").await;
    let r = router(db.clone());
    let webrtc = format!("ws://h:3333/app/{NAME}");
    let llhls = format!("http://h:3333/app/{NAME}/llhls.m3u8");
    for url in [&webrtc, &llhls] {
        let (_, json) = signed(&r, "outgoing", "opening", url).await;
        assert_eq!(json, json!({ "allowed": false }), "ready: {url}");
    }
    sqlx::query("UPDATE broadcasts SET status = 'live'")
        .execute(&db)
        .await
        .unwrap();
    for url in [&webrtc, &llhls] {
        let (_, json) = signed(&r, "outgoing", "opening", url).await;
        assert_eq!(json, json!({ "allowed": true }), "live: {url}");
    }
    sqlx::query("UPDATE broadcasts SET status = 'ended'")
        .execute(&db)
        .await
        .unwrap();
    let (_, json) = signed(&r, "outgoing", "opening", &webrtc).await;
    assert_eq!(json, json!({ "allowed": false }));
}

// ---------- 닫힘 ----------

#[sqlx::test]
async fn publish_closing_returns_live_to_ready_but_keeps_ended(db: PgPool) {
    seed(&db, "live").await;
    let r = router(db.clone());
    let (code, json) = signed(&r, "incoming", "closing", &publish_url(NAME, SKEY)).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(json, json!({}));
    assert_eq!(status_of(&db).await, "ready");

    // 시청자가 나가는 것은 방송 상태를 바꾸지 않는다
    sqlx::query("UPDATE broadcasts SET status = 'live'")
        .execute(&db)
        .await
        .unwrap();
    signed(
        &r,
        "outgoing",
        "closing",
        &format!("ws://h:3333/app/{NAME}"),
    )
    .await;
    assert_eq!(status_of(&db).await, "live");

    // 종료된 방송은 closing이 와도 종료 그대로
    sqlx::query("UPDATE broadcasts SET status = 'ended'")
        .execute(&db)
        .await
        .unwrap();
    signed(&r, "incoming", "closing", &publish_url(NAME, SKEY)).await;
    assert_eq!(status_of(&db).await, "ended");
}

// ---------- 주소 해석 ----------

#[test]
fn parse_stream_examples() {
    assert_eq!(
        parse_stream(&publish_url(NAME, "abc")),
        Some((NAME.to_string(), Some("abc".to_string())))
    );
    assert_eq!(
        parse_stream(&format!("https://x/app/{NAME}/llhls.m3u8?_HLS_msn=3")),
        Some((NAME.to_string(), None))
    );
    assert_eq!(
        parse_stream(&format!("ws://x:3333/app/{NAME}")),
        Some((NAME.to_string(), None))
    );
    assert_eq!(parse_stream("ws://x/app/short"), None);
    assert_eq!(parse_stream(&format!("ws://x/app2/{NAME}")), None);
    assert_eq!(parse_stream(""), None);
}

// ---------- #27 재수정 ----------

#[test]
fn parse_stream_rejects_path_tricks_and_bad_names() {
    let other = "fedcba9876543210";
    for url in [
        // OME는 정리하지 않은 원래 경로로 이름을 정한다. `..`·`.`·인코딩·빈 칸은 모두 거절(V1)
        format!("http://h:3333/app/{other}/../{NAME}?direction=whip&key=k"),
        format!("http://h:3333/app/./{NAME}?direction=whip&key=k"),
        format!("http://h:3333/app/{NAME}/..?key=k"),
        format!("http://h:3333/app/%2e%2e/{NAME}?key=k"),
        format!("http://h:3333/app/{other}/%2e%2e/{NAME}?key=k"),
        format!("http://h:3333/app//{NAME}?key=k"),
        format!("http://h:3333//app/{NAME}?key=k"),
        format!("http://h:3333/app/{NAME}/?key=k"),
        format!("http://h:3333/app\\{NAME}?key=k"),
        format!("http://h:3333/app/{NAME}/a/b?key=k"),
        format!("http://h:3333/app/{NAME}/.hidden"),
        // key가 두 번
        format!("http://h:3333/app/{NAME}?key=a&key=b"),
        // 이름 형식: 대문자, 17자, 15자
        "ws://x/app/0123456789ABCDEF".to_string(),
        "ws://x/app/0123456789abcdef0".to_string(),
        "ws://x/app/0123456789abcde".to_string(),
    ] {
        assert_eq!(parse_stream(&url), None, "{url}");
    }
    // 정상
    assert_eq!(
        parse_stream(&format!(
            "https://x/app/{NAME}/chunklist_0_video_llhls.m3u8?_HLS_part=1"
        )),
        Some((NAME.to_string(), None))
    );
}

#[sqlx::test]
async fn dot_dot_publish_cannot_take_another_name(db: PgPool) {
    // 내 방송(NAME)의 키로 남의 이름 앞에 `..`를 붙여도 거절되고, 내 방송 상태도 바뀌지 않는다(V1)
    seed(&db, "ready").await;
    let url = format!("http://h:3333/app/fedcba9876543210/../{NAME}?direction=whip&key={SKEY}");
    let (_, json) = signed(&router(db.clone()), "incoming", "opening", &url).await;
    assert_eq!(json, json!({ "allowed": false }));
    assert_eq!(status_of(&db).await, "ready");
}

#[sqlx::test]
async fn opening_sets_started_at_once_and_keeps_it_while_live(db: PgPool) {
    seed(&db, "ready").await;
    let r = router(db.clone());
    let started = || async {
        sqlx::query_scalar::<_, Option<time::OffsetDateTime>>(
            "SELECT started_at FROM broadcasts WHERE stream_name = $1",
        )
        .bind(NAME)
        .fetch_one(&db)
        .await
        .unwrap()
    };
    assert!(started().await.is_none());
    signed(&r, "incoming", "opening", &publish_url(NAME, SKEY)).await;
    let first = started().await.expect("송출이 열리면 시작 시각이 생긴다");
    // 이미 live인데 다시 opening이 와도(재연결) 시작 시각은 그대로(V18)
    sqlx::query("UPDATE broadcasts SET started_at = started_at - interval '1 minute'")
        .execute(&db)
        .await
        .unwrap();
    let before = started().await.unwrap();
    signed(&r, "incoming", "opening", &publish_url(NAME, SKEY)).await;
    assert_eq!(started().await.unwrap(), before);
    assert!(before < first);
}

#[sqlx::test]
async fn old_key_is_denied_after_rotation(db: PgPool) {
    // 송출 키를 새로 만든 뒤에는 옛 키로 송출할 수 없고 새 키로는 된다(#28)
    seed(&db, "ready").await;
    let r = router(db.clone());
    let new_key = format!("{}2", &SKEY[..SKEY.len() - 1]);
    sqlx::query("UPDATE broadcasts SET stream_key = $1")
        .bind(&new_key)
        .execute(&db)
        .await
        .unwrap();
    let (_, old) = signed(&r, "incoming", "opening", &publish_url(NAME, SKEY)).await;
    assert_eq!(old, json!({ "allowed": false }));
    assert_eq!(status_of(&db).await, "ready");
    let (_, new) = signed(&r, "incoming", "opening", &publish_url(NAME, &new_key)).await;
    assert_eq!(new, json!({ "allowed": true }));
    assert_eq!(status_of(&db).await, "live");
}

// ---------- 녹화 바로 시작 신호 (#59) ----------

/// 신호가 왔는지 잠깐(0.2초) 기다려 본다
async fn kicked(kick: &onair_backend::recordings::Kick) -> bool {
    tokio::time::timeout(std::time::Duration::from_millis(200), kick.wait())
        .await
        .is_ok()
}

#[sqlx::test]
async fn allowed_publish_kicks_recording_but_rejected_or_viewing_does_not(db: PgPool) {
    seed(&db, "ready").await;
    let kick = onair_backend::recordings::Kick::default();
    let r = ome_routes(OmeState {
        secret: KEY.into(),
        db: db.clone(),
        ome: Default::default(),
        kick: kick.clone(),
    });
    // 틀린 키: 거절, 신호 없음
    let (_, v) = signed(&r, "incoming", "opening", &publish_url(NAME, "wrong")).await;
    assert_eq!(v["allowed"], false);
    assert!(!kicked(&kick).await);
    // 맞는 키: 허락, 신호 있음
    let (_, v) = signed(&r, "incoming", "opening", &publish_url(NAME, SKEY)).await;
    assert_eq!(v["allowed"], true);
    assert!(kicked(&kick).await);
    // 시청(outgoing)은 허락돼도 신호 없음
    let (_, v) = signed(
        &r,
        "outgoing",
        "opening",
        &format!("https://h/app/{NAME}/llhls.m3u8"),
    )
    .await;
    assert_eq!(v["allowed"], true);
    assert!(!kicked(&kick).await);
}
