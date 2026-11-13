//! 실시간 채팅 시험(#86): 누가 쓸 수 있는지, 글 검사, 받기(SSE).

use std::time::Duration;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use onair_backend::{AppState, app, auth::AuthConfig, chat::Hub, guard::RateLimiter};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio_stream::StreamExt;
use tower::ServiceExt;

fn router(db: PgPool, hub: Hub, chat_limit: u32) -> Router {
    app(AppState {
        chat: hub,
        rec: Default::default(),
        toss: Default::default(),
        ome: Default::default(),
        db,
        auth: AuthConfig {
            demo_login: true,
            chat_limiter: RateLimiter::new(chat_limit, Duration::from_secs(60)),
            ..Default::default()
        },
    })
}

async fn call(
    r: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    cookie: Option<&str>,
) -> (StatusCode, Value, Option<String>) {
    let mut req = Request::builder().method(method).uri(path);
    if let Some(c) = cookie {
        req = req.header(header::COOKIE, format!("onair_session={c}"));
    }
    let req = match body {
        Some(b) => req
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    let res = r.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let cookie = res
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            s.split(';')
                .next()?
                .strip_prefix("onair_session=")
                .map(str::to_string)
        });
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        cookie,
    )
}

async fn login(r: &Router, n: u8) -> String {
    call(r, "POST", &format!("/auth/demo/{n}"), None, None)
        .await
        .2
        .unwrap()
}

/// demo1이 방송을 만든다. (방송 id, demo1 쿠키)
async fn broadcast(r: &Router) -> (i64, String) {
    let owner = login(r, 1).await;
    let (st, b, _) = call(
        r,
        "POST",
        "/broadcasts",
        Some(json!({"title": "채팅 시험"})),
        Some(&owner),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    (b["id"].as_i64().unwrap(), owner)
}

async fn say(r: &Router, bid: i64, text: &str, cookie: Option<&str>) -> (StatusCode, Value) {
    let (st, j, _) = call(
        r,
        "POST",
        &format!("/broadcasts/{bid}/chat"),
        Some(json!({ "text": text })),
        cookie,
    )
    .await;
    (st, j)
}

#[sqlx::test]
async fn who_can_chat_and_what_is_accepted(db: PgPool) {
    let hub = Hub::default();
    let r = router(db.clone(), hub.clone(), 100);
    let (bid, owner) = broadcast(&r).await;
    let other = login(&r, 2).await;
    // 비로그인은 쓸 수 없다
    assert_eq!(say(&r, bid, "안녕", None).await.0, StatusCode::UNAUTHORIZED);
    // 로그인한 사람은 누구나(방송자, 다른 사용자) 쓴다
    assert_eq!(
        say(&r, bid, "  안녕하세요  ", Some(&other)).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        say(&r, bid, "반가워요", Some(&owner)).await.0,
        StatusCode::NO_CONTENT
    );
    // 이름은 서버가 세션에서 붙이고, 글은 앞뒤 공백을 정리한다. 줄바꿈은 한 줄로.
    assert_eq!(
        say(&r, bid, "첫 줄\n둘째 줄", Some(&other)).await.0,
        StatusCode::NO_CONTENT
    );
    let (recent, _) = hub.subscribe(bid);
    let got: Vec<(String, String, &str)> = recent
        .iter()
        .map(|m| (m.username.clone(), m.text.clone(), m.kind))
        .collect();
    assert_eq!(
        got,
        [
            ("demo2".to_string(), "안녕하세요".to_string(), "chat"),
            ("demo1".to_string(), "반가워요".to_string(), "chat"),
            ("demo2".to_string(), "첫 줄 둘째 줄".to_string(), "chat"),
        ]
    );
    // 빈 글, 보이지 않는 글자만, 200자 넘음, 제어 문자는 400
    for bad in [
        "",
        "   ",
        "\u{200b}\u{3000}",
        &"가".repeat(201),
        "a\u{0007}b",
        "a\u{202e}b",
    ] {
        let (st, j) = say(&r, bid, bad, Some(&other)).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{bad:?}");
        assert!(j["error"].is_string());
    }
    assert_eq!(
        say(&r, bid, &"가".repeat(200), Some(&other)).await.0,
        StatusCode::NO_CONTENT
    );
    // 없는 방송 404
    assert_eq!(
        say(&r, 999_999, "안녕", Some(&other)).await.0,
        StatusCode::NOT_FOUND
    );
    // 종료된 방송 409
    sqlx::query("UPDATE broadcasts SET status = 'ended' WHERE id = $1")
        .bind(bid)
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(
        say(&r, bid, "끝났나요", Some(&other)).await.0,
        StatusCode::CONFLICT
    );
}

#[sqlx::test]
async fn chat_is_rate_limited_per_user(db: PgPool) {
    let hub = Hub::default();
    let r = router(db, hub, 2);
    let (bid, owner) = broadcast(&r).await;
    let other = login(&r, 2).await;
    // 내용 검사에 걸린 글은 횟수에 넣지 않는다
    assert_eq!(
        say(&r, bid, "", Some(&other)).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        say(&r, bid, "1", Some(&other)).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        say(&r, bid, "2", Some(&other)).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        say(&r, bid, "3", Some(&other)).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
    // 다른 사용자는 자기 몫이 따로 있다
    assert_eq!(
        say(&r, bid, "나는 돼요", Some(&owner)).await.0,
        StatusCode::NO_CONTENT
    );
}

#[sqlx::test]
async fn stream_sends_recent_then_new_messages(db: PgPool) {
    let hub = Hub::default();
    let r = router(db, hub, 100);
    let (bid, owner) = broadcast(&r).await;
    assert_eq!(
        say(&r, bid, "먼저 쓴 글", Some(&owner)).await.0,
        StatusCode::NO_CONTENT
    );
    // 없는 방송의 스트림은 404
    let (st, _, _) = call(&r, "GET", "/broadcasts/999999/chat/stream", None, None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    // 로그인 없이 받는다
    let res = r
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/broadcasts/{bid}/chat/stream"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(
        res.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    let mut body = res.into_body().into_data_stream();
    let mut seen = String::new();
    let mut next = async || {
        let chunk = tokio::time::timeout(Duration::from_secs(3), body.next())
            .await
            .expect("시간 안에 메시지가 와야 한다")
            .unwrap()
            .unwrap();
        String::from_utf8(chunk.to_vec()).unwrap()
    };
    // 처음에 최근 글
    seen.push_str(&next().await);
    assert!(seen.contains("먼저 쓴 글"), "{seen}");
    assert!(seen.contains("event: chat"), "{seen}");
    // 연결한 뒤에 쓴 글도 바로 온다
    assert_eq!(
        say(&r, bid, "새 글", Some(&owner)).await.0,
        StatusCode::NO_CONTENT
    );
    let more = next().await;
    assert!(more.contains("새 글"), "{more}");
    assert!(more.contains("\"username\":\"demo1\""), "{more}");
}

#[test]
fn hub_keeps_only_recent_messages() {
    let hub = Hub::default();
    for i in 0..60 {
        hub.publish(1, "chat", "a", &i.to_string());
    }
    let (recent, _) = hub.subscribe(1);
    assert_eq!(recent.len(), onair_backend::chat::RECENT_MAX);
    assert_eq!(recent[0].text, "10");
    assert_eq!(recent.last().unwrap().text, "59");
    // 다른 방송과 섞이지 않는다
    assert!(hub.subscribe(2).0.is_empty());
    // id는 늘어나기만 한다(화면이 다시 붙을 때 겹친 것을 버리는 데 씀)
    assert!(recent.windows(2).all(|w| w[0].id < w[1].id));
    hub.remove(1);
    assert!(hub.subscribe(1).0.is_empty());
}
