//! 해시 계산 허가가 "계산이 끝날 때" 풀리는지 본다(#24 2바퀴).
//! 허가 수는 프로그램 전체가 함께 쓰는 값이라, 다른 시험과 섞이지 않게 이 파일에 시험을 하나만 둔다.

use std::time::Duration;

use axum::{
    body::Body,
    http::{Request, header},
};
use onair_backend::{AppState, app, auth::free_hash_slots};
use sqlx::PgPool;
use tower::ServiceExt;

#[sqlx::test]
async fn dropped_request_keeps_hash_slot_until_hash_finishes(db: PgPool) {
    let r = app(AppState {
        chat: Default::default(),
        rec: Default::default(),
        toss: Default::default(),
        ome: Default::default(),
        db,
        auth: Default::default(),
    });
    let req = Request::post("/auth/signup")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"username":"slowpoke","password":"password123"}"#,
        ))
        .unwrap();
    // 해시 계산(수백 ms)이 끝나기 전에 요청을 버린다(클라이언트가 연결을 끊은 것과 같다)
    let dropped = tokio::time::timeout(Duration::from_millis(30), r.oneshot(req)).await;
    assert!(dropped.is_err(), "해시가 30ms 안에 끝나 버렸다");
    // 계산은 아직 돌고 있으니 허가 하나는 계속 쥐고 있어야 한다
    assert_eq!(free_hash_slots(), 1);
    // 계산이 끝나면 돌려준다
    for _ in 0..100 {
        if free_hash_slots() == 2 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("해시 허가가 5초 안에 돌아오지 않았다");
}
