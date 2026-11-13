//! 시청자 제한·천천히 모드 시험(#87): 방송 주인만, 막힌 사람은 채팅·댓글을 못 씀, 천천히 모드.

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use onair_backend::{AppState, app, auth::AuthConfig, chat::Hub};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

fn router(db: PgPool, hub: Hub) -> Router {
    app(AppState {
        chat: hub,
        rec: Default::default(),
        toss: Default::default(),
        ome: Default::default(),
        db,
        auth: AuthConfig {
            demo_login: true,
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

async fn login(r: &Router, n: u8) -> (String, i64) {
    let (_, me, c) = call(r, "POST", &format!("/auth/demo/{n}"), None, None).await;
    (c.unwrap(), me["id"].as_i64().unwrap())
}

/// demo1(주인)이 방송을 만든다. (방송 id, 주인 쿠키, 주인 id, demo2 쿠키, demo2 id)
async fn setup(r: &Router) -> (i64, String, i64, String, i64) {
    let (owner, owner_id) = login(r, 1).await;
    let (other, other_id) = login(r, 2).await;
    let (st, b, _) = call(
        r,
        "POST",
        "/broadcasts",
        Some(json!({"title": "제한 시험"})),
        Some(&owner),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    (b["id"].as_i64().unwrap(), owner, owner_id, other, other_id)
}

async fn chat(r: &Router, bid: i64, text: &str, c: &str) -> (StatusCode, Value) {
    let (st, j, _) = call(
        r,
        "POST",
        &format!("/broadcasts/{bid}/chat"),
        Some(json!({"text": text})),
        Some(c),
    )
    .await;
    (st, j)
}

async fn comment(r: &Router, bid: i64, text: &str, c: &str) -> StatusCode {
    call(
        r,
        "POST",
        &format!("/broadcasts/{bid}/comments"),
        Some(json!({"body": text})),
        Some(c),
    )
    .await
    .0
}

#[sqlx::test]
async fn only_the_owner_can_ban_and_banned_users_cannot_write(db: PgPool) {
    let hub = Hub::default();
    let r = router(db.clone(), hub.clone());
    let (bid, owner, owner_id, other, other_id) = setup(&r).await;
    // 채팅 메시지에 보낸 사람 번호가 들어 있다(주인 화면의 제한 버튼이 쓴다)
    assert_eq!(
        chat(&r, bid, "막히기 전", &other).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(hub.subscribe(bid).0[0].user_id, Some(other_id));
    let ban = format!("/broadcasts/{bid}/bans/{other_id}");
    let ten = Some(json!({"ten_minutes": true}));
    // 비로그인 401, 다른 사용자 403(남을 막거나 목록을 볼 수 없다)
    assert_eq!(
        call(&r, "PUT", &ban, ten.clone(), None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &r,
            "PUT",
            &format!("/broadcasts/{bid}/bans/{owner_id}"),
            ten.clone(),
            Some(&other)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &r,
            "GET",
            &format!("/broadcasts/{bid}/bans"),
            None,
            Some(&other)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&r, "GET", &format!("/broadcasts/{bid}/bans"), None, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    // 주인이 10분 막는다
    assert_eq!(
        call(&r, "PUT", &ban, ten.clone(), Some(&owner)).await.0,
        StatusCode::NO_CONTENT
    );
    let (st, list, _) = call(
        &r,
        "GET",
        &format!("/broadcasts/{bid}/bans"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(list[0]["user_id"], other_id);
    assert_eq!(list[0]["username"], "demo2");
    assert!(list[0]["until"].is_string());
    // 막힌 사람은 채팅도 댓글도 못 쓴다(403과 이유). 읽기는 된다.
    let (st, j) = chat(&r, bid, "막힌 뒤", &other).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert!(j["error"].as_str().unwrap().contains("막았습니다"));
    assert_eq!(
        comment(&r, bid, "막힌 뒤 댓글", &other).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &r,
            "GET",
            &format!("/broadcasts/{bid}/comments"),
            None,
            Some(&other)
        )
        .await
        .0,
        StatusCode::OK
    );
    // 주인은 그대로 쓴다
    assert_eq!(
        chat(&r, bid, "주인", &owner).await.0,
        StatusCode::NO_CONTENT
    );
    // 풀면 다시 쓴다. 여러 번 풀어도 204.
    assert_eq!(
        call(&r, "DELETE", &ban, None, Some(&owner)).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&r, "DELETE", &ban, None, Some(&owner)).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        chat(&r, bid, "풀린 뒤", &other).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        comment(&r, bid, "풀린 뒤 댓글", &other).await,
        StatusCode::CREATED
    );
    // 해제할 때까지 막기(until null), 기한이 지난 제한은 효과가 없다
    assert_eq!(
        call(
            &r,
            "PUT",
            &ban,
            Some(json!({"ten_minutes": false})),
            Some(&owner)
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    let (_, list, _) = call(
        &r,
        "GET",
        &format!("/broadcasts/{bid}/bans"),
        None,
        Some(&owner),
    )
    .await;
    assert!(list[0]["until"].is_null());
    assert_eq!(
        chat(&r, bid, "계속 막힘", &other).await.0,
        StatusCode::FORBIDDEN
    );
    sqlx::query("UPDATE broadcast_bans SET until = now() - interval '1 second'")
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(
        chat(&r, bid, "기한 지남", &other).await.0,
        StatusCode::NO_CONTENT
    );
    let (_, list, _) = call(
        &r,
        "GET",
        &format!("/broadcasts/{bid}/bans"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(list.as_array().unwrap().len(), 0);
}

#[sqlx::test]
async fn ban_input_errors(db: PgPool) {
    let r = router(db, Hub::default());
    let (bid, owner, owner_id, _, _) = setup(&r).await;
    let ten = Some(json!({"ten_minutes": true}));
    // 자기 자신은 막을 수 없다
    assert_eq!(
        call(
            &r,
            "PUT",
            &format!("/broadcasts/{bid}/bans/{owner_id}"),
            ten.clone(),
            Some(&owner)
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    // 없는 사용자, 없는 방송은 404
    assert_eq!(
        call(
            &r,
            "PUT",
            &format!("/broadcasts/{bid}/bans/999999"),
            ten.clone(),
            Some(&owner)
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&r, "PUT", "/broadcasts/999999/bans/1", ten, Some(&owner))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test]
async fn slow_mode_limits_viewers_but_not_the_owner(db: PgPool) {
    let r = router(db, Hub::default());
    let (bid, owner, _, other, _) = setup(&r).await;
    let slow = format!("/broadcasts/{bid}/slow");
    // 주인만, 정해진 값만
    assert_eq!(
        call(&r, "PUT", &slow, Some(json!({"secs": 10})), Some(&other))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&r, "PUT", &slow, Some(json!({"secs": 7})), Some(&owner))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(&r, "PUT", &slow, Some(json!({"secs": 10})), Some(&owner))
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let (_, b, _) = call(&r, "GET", &format!("/broadcasts/{bid}"), None, None).await;
    assert_eq!(b["slow_mode_secs"], 10);
    // 시청자: 처음은 되고 바로 다음은 429와 이유
    assert_eq!(
        chat(&r, bid, "하나", &other).await.0,
        StatusCode::NO_CONTENT
    );
    let (st, j) = chat(&r, bid, "둘", &other).await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
    assert!(j["error"].as_str().unwrap().contains("천천히 모드"));
    // 주인은 예외
    assert_eq!(
        chat(&r, bid, "주인 하나", &owner).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        chat(&r, bid, "주인 둘", &owner).await.0,
        StatusCode::NO_CONTENT
    );
    // 끄면 바로 다시 쓴다
    assert_eq!(
        call(&r, "PUT", &slow, Some(json!({"secs": 0})), Some(&owner))
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(chat(&r, bid, "셋", &other).await.0, StatusCode::NO_CONTENT);
}

#[test]
fn slow_ok_waits_per_user_and_broadcast() {
    let hub = Hub::default();
    assert!(hub.slow_ok(1, 10, 5));
    assert!(!hub.slow_ok(1, 10, 5));
    // 다른 사람, 다른 방송은 따로 센다
    assert!(hub.slow_ok(1, 11, 5));
    assert!(hub.slow_ok(2, 10, 5));
    // 방송을 지우면 기록도 지운다
    hub.remove(1);
    assert!(hub.slow_ok(1, 10, 5));
}

/// #94: 주인만 풀 수 있다, 막힌 사람은 댓글을 고칠 수 없다(지우기는 된다), 제한은 그 방송에만 걸린다.
#[sqlx::test]
async fn unban_is_owner_only_and_bans_are_per_broadcast(db: PgPool) {
    let r = router(db, Hub::default());
    let (bid, owner, _, other, other_id) = setup(&r).await;
    let (st, b2, _) = call(
        &r,
        "POST",
        "/broadcasts",
        Some(json!({"title": "다른 방송"})),
        Some(&owner),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let bid2 = b2["id"].as_i64().unwrap();
    // 막히기 전에 댓글을 하나 쓴다
    let (st, c, _) = call(
        &r,
        "POST",
        &format!("/broadcasts/{bid}/comments"),
        Some(json!({"body": "막히기 전 댓글"})),
        Some(&other),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    let cid = c["id"].as_i64().unwrap();
    let ban = format!("/broadcasts/{bid}/bans/{other_id}");
    assert_eq!(
        call(
            &r,
            "PUT",
            &ban,
            Some(json!({"ten_minutes": false})),
            Some(&owner)
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    // 막힌 사람이 스스로 풀거나, 비로그인이 풀 수 없다. 그 뒤에도 계속 막혀 있다.
    assert_eq!(
        call(&r, "DELETE", &ban, None, Some(&other)).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&r, "DELETE", &ban, None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        chat(&r, bid, "풀었나", &other).await.0,
        StatusCode::FORBIDDEN
    );
    // 천천히 모드도 비로그인은 401
    assert_eq!(
        call(
            &r,
            "PUT",
            &format!("/broadcasts/{bid}/slow"),
            Some(json!({"secs": 5})),
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    // 예전 댓글을 고칠 수 없다(403). 지우기는 된다.
    let (st, j, _) = call(
        &r,
        "PATCH",
        &format!("/comments/{cid}"),
        Some(json!({"body": "막힌 뒤 고침"})),
        Some(&other),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert!(j["error"].as_str().unwrap().contains("막았습니다"));
    // 다른 방송에서는 그대로 쓴다. 그 방송의 제한 목록은 비어 있다.
    assert_eq!(
        chat(&r, bid2, "여기는 됨", &other).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        comment(&r, bid2, "여기는 됨", &other).await,
        StatusCode::CREATED
    );
    let (_, list, _) = call(
        &r,
        "GET",
        &format!("/broadcasts/{bid2}/bans"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(list.as_array().unwrap().len(), 0);
    assert_eq!(
        call(
            &r,
            "DELETE",
            &format!("/comments/{cid}"),
            None,
            Some(&other)
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
}

/// #94: 천천히 모드에 걸린 채팅은 1분 횟수를 쓰지 않는다
#[sqlx::test]
async fn slow_mode_rejections_do_not_use_the_rate_limit(db: PgPool) {
    let hub = Hub::default();
    let r = router(db, hub.clone());
    let (bid, owner, _, other, _) = setup(&r).await;
    assert_eq!(
        call(
            &r,
            "PUT",
            &format!("/broadcasts/{bid}/slow"),
            Some(json!({"secs": 5})),
            Some(&owner)
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        chat(&r, bid, "첫 글", &other).await.0,
        StatusCode::NO_CONTENT
    );
    // 30번 거절되어도 모두 천천히 모드 안내다(횟수 제한 안내가 섞이지 않는다)
    for _ in 0..30 {
        let (st, j) = chat(&r, bid, "또", &other).await;
        assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
        assert!(j["error"].as_str().unwrap().contains("천천히 모드"), "{j}");
    }
    // 천천히 모드를 끄면 바로 쓴다(1분 횟수가 남아 있다)
    assert_eq!(
        call(
            &r,
            "PUT",
            &format!("/broadcasts/{bid}/slow"),
            Some(json!({"secs": 0})),
            Some(&owner)
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        chat(&r, bid, "끈 뒤", &other).await.0,
        StatusCode::NO_CONTENT
    );
}

/// #109: 공지 고정. 주인만, 한 줄로 다듬고, 빈 값은 내림, 100자·제어 문자는 400, 방송 정보에 보인다.
#[sqlx::test]
async fn owner_can_pin_a_notice(db: PgPool) {
    let r = router(db, Hub::default());
    let (bid, owner, _, other, _) = setup(&r).await;
    let notice = format!("/broadcasts/{bid}/notice");
    let put = |text: &str, c: Option<&str>| {
        let r = r.clone();
        let notice = notice.clone();
        let body = json!({ "text": text });
        let c = c.map(str::to_string);
        async move { call(&r, "PUT", &notice, Some(body), c.as_deref()).await.0 }
    };
    let get = || {
        let r = r.clone();
        async move {
            call(&r, "GET", &format!("/broadcasts/{bid}"), None, None)
                .await
                .1["notice"]
                .as_str()
                .unwrap()
                .to_string()
        }
    };
    // 처음엔 없다
    assert_eq!(get().await, "");
    // 비로그인 401, 다른 사람 403. 잘못된 글을 보내도 주인 확인이 먼저다(403, #115)
    assert_eq!(put("공지", None).await, StatusCode::UNAUTHORIZED);
    assert_eq!(put("공지", Some(&other)).await, StatusCode::FORBIDDEN);
    assert_eq!(put("널\u{0}", Some(&other)).await, StatusCode::FORBIDDEN);
    assert_eq!(get().await, "");
    // 주인: 한 줄로 다듬어 저장, 목록에도 보인다
    assert_eq!(
        put("  오늘은\n9시까지  ", Some(&owner)).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(get().await, "오늘은 9시까지");
    let (_, list, _) = call(&r, "GET", "/broadcasts", None, None).await;
    assert_eq!(list[0]["notice"], "오늘은 9시까지");
    // 100자까지 되고 101자·제어 문자·글자 방향 문자는 400(공지는 그대로)
    assert_eq!(
        put(&"가".repeat(100), Some(&owner)).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        put(&"가".repeat(101), Some(&owner)).await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(put("널\u{0}", Some(&owner)).await, StatusCode::BAD_REQUEST);
    // NUL은 DB도 받지 않으니, DB가 받는 제어 문자(벨)로도 400인지 본다(#115)
    assert_eq!(put("벨\u{7}", Some(&owner)).await, StatusCode::BAD_REQUEST);
    assert_eq!(
        put("abc\u{202E}def", Some(&owner)).await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(get().await, "가".repeat(100));
    // 탭·\r\n·줄 구분 문자는 빈칸 하나씩(#115)
    assert_eq!(
        put("a\tb\r\nc\u{2028}d", Some(&owner)).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(get().await, "a b c d");
    // 다른 방송의 공지는 그대로다(#115). demo2가 자기 방송을 만든다.
    let (_, b2, _) = call(
        &r,
        "POST",
        "/broadcasts",
        Some(json!({"title": "다른 방송"})),
        Some(&other),
    )
    .await;
    let bid2 = b2["id"].as_i64().unwrap();
    assert_eq!(put("내 공지", Some(&owner)).await, StatusCode::NO_CONTENT);
    let (_, v, _) = call(&r, "GET", &format!("/broadcasts/{bid2}"), None, None).await;
    assert_eq!(v["notice"], "");
    // 빈 값·보이지 않는 글자만이면 내린다
    assert_eq!(
        put("  \u{200B} ", Some(&owner)).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(get().await, "");
    // 없는 방송 404
    assert_eq!(
        call(
            &r,
            "PUT",
            "/broadcasts/999999/notice",
            Some(json!({"text": "x"})),
            Some(&owner)
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

/// #112: 채팅 금칙어. 주인만 보고 바꾼다, 다듬기, 들어간 채팅은 400(대소문자 무시, 주인도), 다른 방송은 상관없음.
#[sqlx::test]
async fn banned_words_block_chat(db: PgPool) {
    let r = router(db, Hub::default());
    let (bid, owner, _, other, _) = setup(&r).await;
    let path = format!("/broadcasts/{bid}/banned-words");
    // 비로그인 401, 다른 사람 403(보기·바꾸기 모두). 잘못된 목록을 보내도 주인 확인이 먼저다.
    assert_eq!(
        call(&r, "GET", &path, None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&r, "GET", &path, None, Some(&other)).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &r,
            "PUT",
            &path,
            Some(json!({"words": ["x"]})),
            Some(&other)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &r,
            "PUT",
            &path,
            Some(json!({"words": ["\u{0}"]})),
            Some(&other)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    // 처음엔 비어 있다
    let (st, v, _) = call(&r, "GET", &path, None, Some(&owner)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v, json!([]));
    // 주인: 다듬어서 저장(빈칸·대문자·빈 단어·같은 단어)
    let (st, v, _) = call(
        &r,
        "PUT",
        &path,
        Some(json!({"words": ["  Spam ", "", "spam", "광고", "  "]})),
        Some(&owner),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v, json!(["spam", "광고"]));
    assert_eq!(
        call(&r, "GET", &path, None, Some(&owner)).await.1,
        json!(["spam", "광고"])
    );
    // 들어간 채팅은 400(대소문자 무시), 주인도 같다. 안 들어간 채팅은 된다.
    let (st, j) = chat(&r, bid, "무료 SPAM 사이트", &other).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(j["error"].as_str().unwrap().contains("막은 단어"));
    assert_eq!(
        chat(&r, bid, "광고합니다", &owner).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        chat(&r, bid, "안녕하세요", &other).await.0,
        StatusCode::NO_CONTENT
    );
    // 다른 방송에서는 막지 않는다
    let (_, b2, _) = call(
        &r,
        "POST",
        "/broadcasts",
        Some(json!({"title": "둘"})),
        Some(&other),
    )
    .await;
    let bid2 = b2["id"].as_i64().unwrap();
    assert_eq!(
        chat(&r, bid2, "spam", &other).await.0,
        StatusCode::NO_CONTENT
    );
    // 30개·20자를 넘거나 제어 문자는 400, 목록은 그대로
    let many: Vec<String> = (0..31).map(|i| format!("w{i}")).collect();
    assert_eq!(
        call(
            &r,
            "PUT",
            &path,
            Some(json!({ "words": many })),
            Some(&owner)
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let thirty: Vec<String> = (0..30).map(|i| format!("w{i}")).collect();
    assert_eq!(
        call(
            &r,
            "PUT",
            &path,
            Some(json!({ "words": thirty })),
            Some(&owner)
        )
        .await
        .0,
        StatusCode::OK
    );
    // 저장해도 다른 방송의 금칙어는 그대로다(#117)
    assert_eq!(
        call(
            &r,
            "GET",
            &format!("/broadcasts/{bid2}/banned-words"),
            None,
            Some(&other)
        )
        .await
        .1,
        json!([])
    );
    // 받는 목록이 100개를 넘으면 같은 단어여도 400. 100개(같은 단어 반복)는 된다(#117)
    let many_same: Vec<&str> = vec!["w1"; 101];
    assert_eq!(
        call(
            &r,
            "PUT",
            &path,
            Some(json!({ "words": many_same })),
            Some(&owner)
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let hundred_same: Vec<&str> = vec!["w1"; 100];
    assert_eq!(
        call(
            &r,
            "PUT",
            &path,
            Some(json!({ "words": hundred_same })),
            Some(&owner)
        )
        .await
        .1,
        json!(["w1"])
    );
    // 글자 방향 문자는 400(#117)
    assert_eq!(
        call(
            &r,
            "PUT",
            &path,
            Some(json!({"words": ["a\u{202E}b"]})),
            Some(&owner)
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    // 글자 수는 소문자로 바꾸기 전에 센다: İ 20개는 된다(#117)
    assert_eq!(
        call(
            &r,
            "PUT",
            &path,
            Some(json!({"words": ["\u{130}".repeat(20)]})),
            Some(&owner)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &r,
            "PUT",
            &path,
            Some(json!({"words": ["가".repeat(21)]})),
            Some(&owner)
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(
            &r,
            "PUT",
            &path,
            Some(json!({"words": ["가".repeat(20)]})),
            Some(&owner)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &r,
            "PUT",
            &path,
            Some(json!({"words": ["벨\u{7}"]})),
            Some(&owner)
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(&r, "GET", &path, None, Some(&owner)).await.1,
        json!(["가".repeat(20)])
    );
    // 빈 목록이면 모두 지운다. 다른 방송의 목록은 그대로(비어 있음)
    assert_eq!(
        call(&r, "PUT", &path, Some(json!({"words": []})), Some(&owner))
            .await
            .1,
        json!([])
    );
    assert_eq!(
        chat(&r, bid, "spam 광고", &other).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(
            &r,
            "GET",
            &format!("/broadcasts/{bid2}/banned-words"),
            None,
            Some(&other)
        )
        .await
        .1,
        json!([])
    );
    // 없는 방송 404
    assert_eq!(
        call(
            &r,
            "GET",
            "/broadcasts/999999/banned-words",
            None,
            Some(&owner)
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}
