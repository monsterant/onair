//! 방송자 즐겨찾기 시험(#102): 로그인한 사람만, 여러 번 해도 같음, 자기 자신·없는 사용자, 목록 정렬과 방송 중 표시, 남의 목록 못 봄.

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use onair_backend::{AppState, app, auth::AuthConfig, chat::Hub};
use serde_json::Value;
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

async fn new_user(db: &PgPool, name: &str) -> i64 {
    sqlx::query_scalar("INSERT INTO users (username, password_hash) VALUES ($1, 'x') RETURNING id")
        .bind(name)
        .fetch_one(db)
        .await
        .unwrap()
}

/// 목록을 (이름, 방송 중이면 방송 제목) 줄로
fn rows(v: &Value) -> Vec<(String, Option<String>)> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["username"].as_str().unwrap().to_string(),
                f["live"]["title"].as_str().map(str::to_string),
            )
        })
        .collect()
}

#[sqlx::test]
async fn follow_unfollow_and_list(db: PgPool) {
    let r = router(db.clone(), Hub::default());
    let (me, me_id) = login(&r, 1).await;
    let (other, other_id) = login(&r, 2).await;
    let zed = new_user(&db, "zed").await;
    let amy = new_user(&db, "amy").await;
    // 비로그인 401
    assert_eq!(
        call(&r, "GET", "/follows", None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&r, "PUT", &format!("/follows/{zed}"), None, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&r, "DELETE", &format!("/follows/{zed}"), None, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    // 자기 자신 400, 없는 사용자 404
    assert_eq!(
        call(&r, "PUT", &format!("/follows/{me_id}"), None, Some(&me))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(&r, "PUT", "/follows/999999999", None, Some(&me))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    // 처음엔 비어 있다
    let (st, v, _) = call(&r, "GET", "/follows", None, Some(&me)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(rows(&v).len(), 0);
    // 즐겨찾기: 두 번 해도 204, 한 줄
    for _ in 0..2 {
        assert_eq!(
            call(&r, "PUT", &format!("/follows/{zed}"), None, Some(&me))
                .await
                .0,
            StatusCode::NO_CONTENT
        );
    }
    assert_eq!(
        call(&r, "PUT", &format!("/follows/{amy}"), None, Some(&me))
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&r, "PUT", &format!("/follows/{other_id}"), None, Some(&me))
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    // 아무도 방송 중이 아니면 이름순
    let (_, v, _) = call(&r, "GET", "/follows", None, Some(&me)).await;
    assert_eq!(
        rows(&v),
        [
            ("amy".to_string(), None),
            ("demo2".to_string(), None),
            ("zed".to_string(), None)
        ]
    );
    assert_eq!(v[0]["user_id"], amy);
    // zed가 방송을 둘 하고 있다(가장 최근에 시작한 것), demo2는 대기 중·종료 방송만 있다
    for (owner, title, status, secs) in [
        (zed, "zed 예전 방송", "live", 100),
        (zed, "zed 새 방송", "live", 10),
        (other_id, "demo2 대기", "ready", 0),
        (other_id, "demo2 종료", "ended", 0),
    ] {
        sqlx::query(
            "INSERT INTO broadcasts (title, owner_id, stream_name, stream_key, status, started_at)
             VALUES ($1, $2, md5(random()::text), md5(random()::text), $3,
                     CASE WHEN $3 = 'ready' THEN NULL ELSE now() - make_interval(secs => $4) END)",
        )
        .bind(title)
        .bind(owner)
        .bind(status)
        .bind(secs as f64)
        .execute(&db)
        .await
        .unwrap();
    }
    let (_, v, _) = call(&r, "GET", "/follows", None, Some(&me)).await;
    assert_eq!(
        rows(&v),
        [
            ("zed".to_string(), Some("zed 새 방송".to_string())),
            ("amy".to_string(), None),
            ("demo2".to_string(), None)
        ]
    );
    assert!(v[0]["live"]["id"].is_i64());
    // 다른 사람은 내 즐겨찾기를 볼 수 없다(자기 목록만 나온다)
    let (_, v, _) = call(&r, "GET", "/follows", None, Some(&other)).await;
    assert_eq!(rows(&v).len(), 0);
    // 해제: 두 번 해도 204, 안 한 사람도 204
    for _ in 0..2 {
        assert_eq!(
            call(&r, "DELETE", &format!("/follows/{zed}"), None, Some(&me))
                .await
                .0,
            StatusCode::NO_CONTENT
        );
    }
    assert_eq!(
        call(&r, "DELETE", "/follows/999999999", None, Some(&me))
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let (_, v, _) = call(&r, "GET", "/follows", None, Some(&me)).await;
    assert_eq!(
        rows(&v),
        [("amy".to_string(), None), ("demo2".to_string(), None)]
    );
    // 방송자 계정이 지워지면 즐겨찾기도 사라진다
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(amy)
        .execute(&db)
        .await
        .unwrap();
    let (_, v, _) = call(&r, "GET", "/follows", None, Some(&me)).await;
    assert_eq!(rows(&v), [("demo2".to_string(), None)]);
}
