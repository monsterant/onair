//! 방송별 댓글 시험 (#37).
//! `#[sqlx::test]`는 시험마다 빈 DB를 새로 만들고 migrations를 적용해 준다(DATABASE_URL의 서버를 쓴다).

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use onair_backend::{AppState, app, auth::AuthConfig};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

fn router(db: PgPool) -> Router {
    app(AppState {
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

struct Res {
    status: StatusCode,
    cookie: Option<String>,
    json: Value,
}

async fn call(
    r: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    cookie: Option<&str>,
) -> Res {
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
        })
        .filter(|v| !v.is_empty());
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    Res {
        status,
        cookie,
        json: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    }
}

/// 데모 사용자 n(1, 2)으로 로그인하고 세션 쿠키를 돌려준다
async fn login(r: &Router, n: u8) -> String {
    call(r, "POST", &format!("/auth/demo/{n}"), None, None)
        .await
        .cookie
        .unwrap()
}

async fn broadcast(r: &Router, cookie: &str) -> i64 {
    let b = call(
        r,
        "POST",
        "/broadcasts",
        Some(json!({"title": "방송", "description": ""})),
        Some(cookie),
    )
    .await;
    assert_eq!(b.status, StatusCode::CREATED);
    b.json["id"].as_i64().unwrap()
}

fn body(text: &str) -> Option<Value> {
    Some(json!({ "body": text }))
}

#[sqlx::test]
async fn access_matrix_for_anonymous_author_and_other(db: PgPool) {
    let r = router(db);
    let author = login(&r, 1).await;
    let other = login(&r, 2).await;
    let bid = broadcast(&r, &author).await;
    let list = format!("/broadcasts/{bid}/comments");

    // 비로그인: 목록은 보지만 쓰지 못한다
    assert_eq!(call(&r, "GET", &list, None, None).await.json, json!([]));
    assert_eq!(
        call(&r, "POST", &list, body("안녕"), None).await.status,
        StatusCode::UNAUTHORIZED
    );

    // 작성자: 쓴다
    let c = call(&r, "POST", &list, body("  첫 댓글 "), Some(&author)).await;
    assert_eq!(c.status, StatusCode::CREATED);
    assert_eq!(c.json["body"], "첫 댓글");
    assert_eq!(c.json["author_username"], "demo1");
    assert_eq!(c.json["broadcast_id"], bid);
    assert!(c.json["updated_at"].is_null());
    let cid = c.json["id"].as_i64().unwrap();
    let one = format!("/comments/{cid}");

    // 다른 사용자: 자기 댓글은 쓰지만 남의 댓글은 고치거나 지우지 못한다
    assert_eq!(
        call(&r, "POST", &list, body("다른 사람"), Some(&other))
            .await
            .status,
        StatusCode::CREATED
    );
    assert_eq!(
        call(&r, "PATCH", &one, body("남이 고침"), Some(&other))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&r, "DELETE", &one, None, Some(&other)).await.status,
        StatusCode::FORBIDDEN
    );
    // 비로그인도 고치거나 지우지 못한다
    assert_eq!(
        call(&r, "PATCH", &one, body("익명"), None).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&r, "DELETE", &one, None, None).await.status,
        StatusCode::UNAUTHORIZED
    );
    let now = call(&r, "GET", &list, None, None).await.json;
    assert_eq!(
        now[0]["body"], "첫 댓글",
        "거절된 요청이 아무것도 바꾸지 않아야 한다"
    );

    // 작성자: 고치고 지운다
    let u = call(&r, "PATCH", &one, body("고친 댓글"), Some(&author)).await;
    assert_eq!(u.status, StatusCode::OK);
    assert_eq!(u.json["body"], "고친 댓글");
    assert!(u.json["updated_at"].is_string());
    assert_eq!(
        call(&r, "DELETE", &one, None, Some(&author)).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&r, "DELETE", &one, None, Some(&author)).await.status,
        StatusCode::NOT_FOUND
    );

    let left = call(&r, "GET", &list, None, None).await.json;
    assert_eq!(left.as_array().unwrap().len(), 1);
    assert_eq!(left[0]["author_username"], "demo2");
}

#[sqlx::test]
async fn list_is_oldest_first_and_per_broadcast(db: PgPool) {
    let r = router(db);
    let me = login(&r, 1).await;
    let a = broadcast(&r, &me).await;
    let b = broadcast(&r, &me).await;
    for t in ["하나", "둘", "셋"] {
        call(
            &r,
            "POST",
            &format!("/broadcasts/{a}/comments"),
            body(t),
            Some(&me),
        )
        .await;
    }
    call(
        &r,
        "POST",
        &format!("/broadcasts/{b}/comments"),
        body("다른 방송"),
        Some(&me),
    )
    .await;
    let list = call(&r, "GET", &format!("/broadcasts/{a}/comments"), None, None)
        .await
        .json;
    let bodies: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["body"].as_str().unwrap())
        .collect();
    assert_eq!(bodies, ["하나", "둘", "셋"]);
}

#[sqlx::test]
async fn list_keeps_only_the_latest_200(db: PgPool) {
    let r = router(db.clone());
    let me = login(&r, 1).await;
    let bid = broadcast(&r, &me).await;
    let uid: i64 = sqlx::query_scalar("SELECT id FROM users WHERE username = 'demo1'")
        .fetch_one(&db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO comments (broadcast_id, author_id, body) SELECT $1, $2, 'c' || g FROM generate_series(1, 205) g ORDER BY g")
        .bind(bid)
        .bind(uid)
        .execute(&db)
        .await
        .unwrap();
    let list = call(
        &r,
        "GET",
        &format!("/broadcasts/{bid}/comments"),
        None,
        None,
    )
    .await
    .json;
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 200);
    assert_eq!(list[0]["body"], "c6");
    assert_eq!(list[199]["body"], "c205");
}

#[sqlx::test]
async fn missing_broadcast_or_comment_is_404(db: PgPool) {
    let r = router(db);
    let me = login(&r, 1).await;
    assert_eq!(
        call(&r, "GET", "/broadcasts/999/comments", None, None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &r,
            "POST",
            "/broadcasts/999/comments",
            body("없음"),
            Some(&me)
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&r, "PATCH", "/comments/999", body("없음"), Some(&me))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&r, "DELETE", "/comments/999", None, Some(&me))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test]
async fn input_is_checked_on_create_and_update(db: PgPool) {
    let r = router(db);
    let me = login(&r, 1).await;
    let bid = broadcast(&r, &me).await;
    let list = format!("/broadcasts/{bid}/comments");
    let long = "가".repeat(301);
    for bad in ["", "   ", "\u{200B}", "널\u{0}", long.as_str()] {
        let res = call(&r, "POST", &list, body(bad), Some(&me)).await;
        assert_eq!(res.status, StatusCode::BAD_REQUEST, "{bad:?}");
        assert!(res.json["error"].is_string());
    }
    assert_eq!(
        call(&r, "POST", &list, body(&"가".repeat(300)), Some(&me))
            .await
            .status,
        StatusCode::CREATED
    );
    let cid = call(&r, "GET", &list, None, None).await.json[0]["id"]
        .as_i64()
        .unwrap();
    assert_eq!(
        call(
            &r,
            "PATCH",
            &format!("/comments/{cid}"),
            body(""),
            Some(&me)
        )
        .await
        .status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(&r, "POST", &list, Some(json!({})), Some(&me))
            .await
            .status
            .as_u16(),
        422
    );
}

#[sqlx::test]
async fn deleting_the_broadcast_or_author_removes_comments(db: PgPool) {
    let r = router(db.clone());
    let owner = login(&r, 1).await;
    let other = call(&r, "POST", "/auth/demo/2", None, None).await;
    let guest_cookie = other.cookie.unwrap();
    let guest_id = other.json["id"].as_i64().unwrap();
    let a = broadcast(&r, &owner).await;
    let b = broadcast(&r, &owner).await;
    call(
        &r,
        "POST",
        &format!("/broadcasts/{a}/comments"),
        body("a의 댓글"),
        Some(&owner),
    )
    .await;
    call(
        &r,
        "POST",
        &format!("/broadcasts/{b}/comments"),
        body("b의 댓글"),
        Some(&guest_cookie),
    )
    .await;

    assert_eq!(
        call(
            &r,
            "DELETE",
            &format!("/broadcasts/{a}"),
            None,
            Some(&owner)
        )
        .await
        .status,
        StatusCode::NO_CONTENT
    );
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM comments")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(n, 1);

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(guest_id)
        .execute(&db)
        .await
        .unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM comments")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

#[sqlx::test]
async fn ended_broadcast_still_takes_comments(db: PgPool) {
    let r = router(db);
    let me = login(&r, 1).await;
    let bid = broadcast(&r, &me).await;
    call(
        &r,
        "POST",
        &format!("/broadcasts/{bid}/end"),
        None,
        Some(&me),
    )
    .await;
    let res = call(
        &r,
        "POST",
        &format!("/broadcasts/{bid}/comments"),
        body("다시보기 감상"),
        Some(&me),
    )
    .await;
    assert_eq!(res.status, StatusCode::CREATED);
}

#[sqlx::test]
async fn comment_rate_limit_is_per_user_and_separate_from_login(db: PgPool) {
    let r = router(db);
    let a = login(&r, 1).await;
    let b = login(&r, 2).await;
    let bid = broadcast(&r, &a).await;
    let list = format!("/broadcasts/{bid}/comments");
    // 내용 검사에 걸린 글은 횟수에 넣지 않는다
    for _ in 0..5 {
        assert_eq!(
            call(&r, "POST", &list, body(""), Some(&a)).await.status,
            StatusCode::BAD_REQUEST
        );
    }
    for i in 0..20 {
        assert_eq!(
            call(&r, "POST", &list, body(&format!("댓글 {i}")), Some(&a))
                .await
                .status,
            StatusCode::CREATED,
            "{i}"
        );
    }
    let over = call(&r, "POST", &list, body("21번째"), Some(&a)).await;
    assert_eq!(over.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(over.json["error"].is_string());
    // 다른 사용자는 자기 몫이 따로 있다
    assert_eq!(
        call(&r, "POST", &list, body("B의 댓글"), Some(&b))
            .await
            .status,
        StatusCode::CREATED
    );
    // 로그인 몫과 따로라 같은 곳(IP)에서 로그인이 막히지 않는다
    assert_eq!(
        call(&r, "POST", "/auth/demo/1", None, None).await.status,
        StatusCode::OK
    );
}
