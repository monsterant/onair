//! 해시 자리를 정해진 시간까지만 기다리는지 본다(#136).
//! 해시 자리 수는 프로그램 전체가 함께 쓰는 값이라, 다른 시험과 섞이지 않게 이 파일에 시험을 하나만 둔다.

use std::time::Duration;

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use onair_backend::{AppState, app, auth::AuthConfig};
use sqlx::PgPool;
use tower::ServiceExt;

#[sqlx::test]
async fn signups_beyond_hash_slots_get_503_instead_of_waiting_forever(db: PgPool) {
    let r = app(AppState {
        chat: Default::default(),
        rec: Default::default(),
        toss: Default::default(),
        ome: Default::default(),
        db,
        auth: AuthConfig {
            // 해시 한 번은 수백 ms 걸린다. 10ms만 기다리게 하면, 자리(2개)를 못 얻은 요청은 503이어야 한다.
            hash_wait: Some(Duration::from_millis(10)),
            ..Default::default()
        },
    });
    let mut tasks = Vec::new();
    for i in 0..6 {
        let r = r.clone();
        tasks.push(tokio::spawn(async move {
            let req = Request::post("/auth/signup")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{"username":"crowd_{i}","password":"password123"}}"#
                )))
                .unwrap();
            r.oneshot(req).await.unwrap().status()
        }));
    }
    let mut created = 0;
    let mut busy = 0;
    for t in tasks {
        match t.await.unwrap() {
            StatusCode::CREATED => created += 1,
            StatusCode::SERVICE_UNAVAILABLE => busy += 1,
            other => panic!("예상하지 못한 응답 {other}"),
        }
    }
    // 자리는 2개라 동시에 2개까지만 계산한다. 나머지는 기다리지 않고 503
    assert!((1..=2).contains(&created), "created={created} busy={busy}");
    assert_eq!(created + busy, 6);
}
