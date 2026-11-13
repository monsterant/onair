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
