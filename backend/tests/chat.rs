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
    // 처음에 연결 신호(주석), 그다음 최근 글
    seen.push_str(&next().await);
    assert!(seen.starts_with(":"), "{seen}");
    while !seen.contains("먼저 쓴 글") {
        seen.push_str(&next().await);
    }
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

#[sqlx::test]
async fn deleting_a_broadcast_clears_its_chat_and_ends_streams(db: PgPool) {
    // 방송을 지우면 채팅방도 지우고, 듣던 연결은 끝난다(#86 변이 시험에서 빠진 것)
    let hub = Hub::default();
    let r = router(db, hub.clone(), 100);
    let (bid, owner) = broadcast(&r).await;
    assert_eq!(
        say(&r, bid, "곧 지워짐", Some(&owner)).await.0,
        StatusCode::NO_CONTENT
    );
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
    let mut body = res.into_body().into_data_stream();
    // 처음 신호와 최근 글을 받는다
    let mut got = String::new();
    while !got.contains("곧 지워짐") {
        let c = tokio::time::timeout(Duration::from_secs(3), body.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        got.push_str(std::str::from_utf8(&c).unwrap());
    }
    let (st, _, _) = call(
        &r,
        "DELETE",
        &format!("/broadcasts/{bid}"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    // 듣던 연결이 끝난다(본문 끝)
    let end = tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(c) = body.next().await {
            c.unwrap();
        }
    })
    .await;
    assert!(end.is_ok(), "방송을 지우면 채팅 연결이 끝나야 한다");
    assert!(hub.subscribe(bid).0.is_empty());
}

#[test]
fn message_ids_do_not_restart_from_one() {
    // 서버를 다시 켜도(새 허브) 번호가 앞 번호보다 커서, 열린 화면이 새 글을 버리지 않는다(#86 리뷰)
    let first = Hub::default().publish(1, "chat", None, "a", "x").id;
    std::thread::sleep(Duration::from_millis(5));
    let second = Hub::default().publish(1, "chat", None, "a", "y").id;
    assert!(first > 1_000_000_000, "{first}");
    assert!(second > first, "{first} {second}");
}

#[test]
fn hub_keeps_only_recent_messages() {
    let hub = Hub::default();
    for i in 0..60 {
        hub.publish(1, "chat", None, "a", &i.to_string());
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

/// #91 리뷰: 서버 전체 상한. 여러 곳을 합쳐 total을 넘으면 어느 곳이든 막히고, 하나 닫으면 다시 열린다.
#[test]
fn chat_streams_have_a_total_cap() {
    let hub = Hub::default();
    let a = hub.open_stream("a", 2, 3).unwrap();
    let _b1 = hub.open_stream("b", 2, 3).unwrap();
    let _b2 = hub.open_stream("b", 2, 3).unwrap();
    // 전체 3개가 찼으니 새 곳(c)도, 아직 1개뿐인 a도 막힌다
    assert!(hub.open_stream("c", 2, 3).is_none());
    assert!(hub.open_stream("a", 2, 3).is_none());
    // a 하나를 닫으면 다시 열린다
    drop(a);
    assert!(hub.open_stream("c", 2, 3).is_some());
}

/// #91: 한 곳(IP)에서 채팅 스트림을 20개까지만 연다. 21번째는 429, 하나 닫으면 다시 열린다. 다른 곳은 따로 센다.
#[sqlx::test]
async fn chat_streams_are_capped_per_client(db: PgPool) {
    use onair_backend::chat::STREAMS_PER_CLIENT;
    let hub = Hub::default();
    let r = router(db, hub.clone(), 20);
    let (bid, _) = broadcast(&r).await;
    let open = |ip: &'static str| {
        let r = r.clone();
        async move {
            r.oneshot(
                Request::get(format!("/broadcasts/{bid}/chat/stream"))
                    .header("x-forwarded-for", ip)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
        }
    };
    // 응답(스트림)을 들고 있는 동안은 열린 것으로 센다
    let mut held = Vec::new();
    for _ in 0..STREAMS_PER_CLIENT {
        let res = open("1.1.1.1").await;
        assert_eq!(res.status(), StatusCode::OK);
        held.push(res);
    }
    assert_eq!(hub.open_streams("1.1.1.1"), STREAMS_PER_CLIENT);
    let res = open("1.1.1.1").await;
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
    // 다른 곳은 따로 센다
    let other = open("2.2.2.2").await;
    assert_eq!(other.status(), StatusCode::OK);
    // 하나를 닫으면(응답을 버리면) 다시 열 수 있다
    held.pop();
    assert_eq!(hub.open_streams("1.1.1.1"), STREAMS_PER_CLIENT - 1);
    let again = open("1.1.1.1").await;
    assert_eq!(again.status(), StatusCode::OK);
    // 모두 닫으면 기록이 비워진다
    drop(again);
    drop(other);
    held.clear();
    assert_eq!(hub.open_streams("1.1.1.1"), 0);
    assert_eq!(hub.open_streams("2.2.2.2"), 0);
}

/// #125 리뷰: 채팅한 사람 기록은 방송마다 상한이 따로다(한 방송을 채워도 다른 방송은 기록된다).
/// DB에 없는 방송의 방·기록은 retain_broadcasts로 지운다.
#[test]
fn chatters_are_capped_per_broadcast_and_pruned() {
    use onair_backend::chat::CHATTERS_PER_BROADCAST;
    let hub = Hub::default();
    for uid in 0..(CHATTERS_PER_BROADCAST as i64 + 10) {
        hub.publish(1, "chat", Some(uid), "u", "x");
    }
    assert!(hub.chatted(1, 0));
    assert!(!hub.chatted(1, CHATTERS_PER_BROADCAST as i64 + 5));
    // 다른 방송은 따로 센다
    hub.publish(2, "chat", Some(7), "u", "x");
    assert!(hub.chatted(2, 7));
    // 후원 안내(사용자 없음)는 적지 않는다
    hub.publish(2, "donation", None, "u", "x");
    // 방송 2만 DB에 남았다면 방송 1의 기록·방은 지워진다
    hub.retain_broadcasts(&[2].into_iter().collect());
    assert!(!hub.chatted(1, 0));
    assert!(hub.subscribe(1).0.is_empty());
    assert!(hub.chatted(2, 7));
}

/// #126: 같은 곳(IP)에서 보내는 채팅·댓글 양은 사용자를 바꿔도 늘지 않는다. 다른 곳은 따로 센다.
#[sqlx::test]
async fn chat_and_comments_are_limited_per_client_across_accounts(db: PgPool) {
    let r = app(AppState {
        chat: Hub::default(),
        rec: Default::default(),
        toss: Default::default(),
        ome: Default::default(),
        db,
        auth: AuthConfig {
            demo_login: true,
            chat_ip_limiter: Some(RateLimiter::new(3, Duration::from_secs(60))),
            comment_ip_limiter: Some(RateLimiter::new(2, Duration::from_secs(60))),
            ..Default::default()
        },
    });
    let (bid, owner) = broadcast(&r).await;
    let a = login(&r, 2).await;
    let send = |text: &'static str, who: String, ip: &'static str| {
        let r = r.clone();
        async move {
            r.oneshot(
                Request::post(format!("/broadcasts/{bid}/chat"))
                    .header(header::COOKIE, format!("onair_session={who}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-forwarded-for", ip)
                    .body(Body::from(json!({ "text": text }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
        }
    };
    // 같은 곳에서 두 사람이 합쳐 3번까지, 4번째는 사람을 바꿔도 429
    assert_eq!(
        send("1", a.clone(), "1.1.1.1").await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        send("2", owner.clone(), "1.1.1.1").await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        send("3", a.clone(), "1.1.1.1").await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        send("4", owner.clone(), "1.1.1.1").await,
        StatusCode::TOO_MANY_REQUESTS
    );
    // 다른 곳은 따로 센다
    assert_eq!(
        send("5", a.clone(), "2.2.2.2").await,
        StatusCode::NO_CONTENT
    );
    // 내용이 틀린 글은 횟수를 쓰지 않는다(빈 글 400)
    assert_eq!(
        send("   ", a.clone(), "3.3.3.3").await,
        StatusCode::BAD_REQUEST
    );
    // 댓글도 같다(2번까지)
    let comment = |who: String| {
        let r = r.clone();
        async move {
            r.oneshot(
                Request::post(format!("/broadcasts/{bid}/comments"))
                    .header(header::COOKIE, format!("onair_session={who}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-forwarded-for", "4.4.4.4")
                    .body(Body::from(json!({ "body": "댓글" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
        }
    };
    assert_eq!(comment(a.clone()).await, StatusCode::CREATED);
    assert_eq!(comment(owner.clone()).await, StatusCode::CREATED);
    assert_eq!(comment(a.clone()).await, StatusCode::TOO_MANY_REQUESTS);
}
