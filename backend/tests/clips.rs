//! 다시보기 장면 저장 시험(#111)

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
        chat: Default::default(),
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

async fn login(r: &Router, n: u8) -> String {
    call(r, "POST", &format!("/auth/demo/{n}"), None, None)
        .await
        .2
        .unwrap()
}

/// 60초짜리 다시보기 하나(status로 준비 상태를 정한다)
async fn recording(r: &Router, db: &PgPool, status: &str) -> i64 {
    let owner = login(r, 1).await;
    let (_, b, _) = call(
        r,
        "POST",
        "/broadcasts",
        Some(json!({"title": "장면 시험"})),
        Some(&owner),
    )
    .await;
    let bid = b["id"].as_i64().unwrap();
    sqlx::query_scalar(
        "INSERT INTO recordings (broadcast_id, record_id, file_name, format, title, owner_username, status, started_at, ended_at, bytes)
         VALUES ($1, $2, $3, 'mp4', '장면 시험', 'demo1', $4, now() - interval '60 seconds', now(), 100000) RETURNING id",
    )
    .bind(bid)
    .bind(format!("rec-{bid}-{status}"))
    .bind(format!("rec-{bid}-{status}.mp4"))
    .bind(status)
    .fetch_one(db)
    .await
    .unwrap()
}

#[sqlx::test]
async fn clips_are_created_by_logged_in_users_and_seen_by_everyone(db: PgPool) {
    let r = router(db.clone());
    let rid = recording(&r, &db, "ready").await;
    let maker = login(&r, 2).await;
    let path = format!("/recordings/{rid}/clips");
    // 로그인 안 하면 만들 수 없다
    let body = json!({"title": "  명장면  ", "start": 10.25, "end": 40.5});
    assert_eq!(
        call(&r, "POST", &path, Some(body.clone()), None).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (st, c, _) = call(&r, "POST", &path, Some(body), Some(&maker)).await;
    assert_eq!(st, StatusCode::CREATED, "{c}");
    assert_eq!(c["title"], "명장면");
    assert_eq!(c["start"], 10.25);
    assert_eq!(c["end"], 40.5);
    assert_eq!(c["username"], "demo2");
    let cid = c["id"].as_i64().unwrap();
    // 누구나 목록과 장면을 본다. 장면에는 다시보기 재생 주소가 함께 온다.
    let (st, list, _) = call(&r, "GET", &path, None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    let (st, one, _) = call(&r, "GET", &format!("/clips/{cid}"), None, None).await;
    assert_eq!(st, StatusCode::OK, "{one}");
    assert_eq!(one["recording"]["id"], rid);
    assert!(
        one["recording"]["url"]
            .as_str()
            .unwrap()
            .starts_with("/vod/")
    );
}

#[sqlx::test]
async fn only_the_maker_deletes_a_clip(db: PgPool) {
    let r = router(db.clone());
    let rid = recording(&r, &db, "ready").await;
    let maker = login(&r, 2).await;
    let other = login(&r, 1).await;
    let (_, c, _) = call(
        &r,
        "POST",
        &format!("/recordings/{rid}/clips"),
        Some(json!({"title": "내 장면", "start": 0, "end": 5})),
        Some(&maker),
    )
    .await;
    let path = format!("/clips/{}", c["id"]);
    assert_eq!(
        call(&r, "DELETE", &path, None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&r, "DELETE", &path, None, Some(&other)).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&r, "DELETE", &path, None, Some(&maker)).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&r, "GET", &path, None, None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&r, "DELETE", &path, None, Some(&maker)).await.0,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test]
async fn clip_range_and_recording_are_checked(db: PgPool) {
    let r = router(db.clone());
    let rid = recording(&r, &db, "ready").await;
    let not_ready = recording(&r, &db, "finishing").await;
    let maker = login(&r, 2).await;
    let make = |rid: i64, start: f64, end: f64| {
        let r = r.clone();
        let maker = maker.clone();
        async move {
            call(
                &r,
                "POST",
                &format!("/recordings/{rid}/clips"),
                Some(json!({"title": "장면", "start": start, "end": end})),
                Some(&maker),
            )
            .await
            .0
        }
    };
    assert_eq!(
        make(rid, 5.0, 5.0).await,
        StatusCode::BAD_REQUEST,
        "끝이 시작과 같음"
    );
    assert_eq!(
        make(rid, 10.0, 5.0).await,
        StatusCode::BAD_REQUEST,
        "끝이 앞"
    );
    assert_eq!(
        make(rid, 30.0, 62.0).await,
        StatusCode::BAD_REQUEST,
        "60초 다시보기를 넘음"
    );
    assert_eq!(
        make(rid, 0.0, 60.5).await,
        StatusCode::CREATED,
        "1초 여유 안"
    );
    assert_eq!(
        make(not_ready, 0.0, 5.0).await,
        StatusCode::NOT_FOUND,
        "준비되지 않은 녹화"
    );
    assert_eq!(
        call(
            &r,
            "GET",
            &format!("/recordings/{not_ready}/clips"),
            None,
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    // 제목이 비면 400
    let (st, _, _) = call(
        &r,
        "POST",
        &format!("/recordings/{rid}/clips"),
        Some(json!({"title": "   ", "start": 0, "end": 5})),
        Some(&maker),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn deleting_the_recording_removes_its_clips(db: PgPool) {
    let r = router(db.clone());
    let rid = recording(&r, &db, "ready").await;
    let maker = login(&r, 2).await;
    let (_, c, _) = call(
        &r,
        "POST",
        &format!("/recordings/{rid}/clips"),
        Some(json!({"title": "장면", "start": 0, "end": 5})),
        Some(&maker),
    )
    .await;
    sqlx::query("DELETE FROM recordings WHERE id = $1")
        .bind(rid)
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(
        call(&r, "GET", &format!("/clips/{}", c["id"]), None, None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM clips")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(left, 0);
}
