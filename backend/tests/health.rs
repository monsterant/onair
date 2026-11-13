use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use onair_backend::{AppState, app};
use sqlx::postgres::PgPoolOptions;
use std::time::Duration;
use tower::ServiceExt;

/// 아무도 없는 주소의 DB를 가리키게 해서, DB가 죽었을 때 /health가 어떻게 답하는지 본다.
#[tokio::test]
async fn health_reports_db_error_when_db_is_down() {
    let db = PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(300))
        .connect_lazy("postgres://nobody:nothing@127.0.0.1:1/none")
        .unwrap();

    let res = app(AppState {
        chat: Default::default(),
        rec: Default::default(),
        toss: Default::default(),
        ome: Default::default(),
        db,
        auth: Default::default(),
    })
    .oneshot(Request::get("/health").body(Body::empty()).unwrap())
    .await
    .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), 1024).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "ok");
    assert_eq!(json["db"], "error");
}
