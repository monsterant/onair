//! 오류 응답 모양과 작은 API 다듬기 시험 (#25).

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use onair_backend::{
    AppState, app,
    auth::{AuthConfig, cleanup_sessions},
};
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;

fn router(db: PgPool, demo_login: bool) -> Router {
    app(AppState {
        rec: Default::default(),
        toss: Default::default(),
        ome: Default::default(),
        db,
        auth: AuthConfig {
            demo_login,
            ..Default::default()
        },
    })
}

async fn send(
    r: &Router,
    req: Request<Body>,
) -> (StatusCode, Option<String>, Value, Option<String>) {
    let res = r.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let ct = res
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_string());
    let allow = res
        .headers()
        .get(header::ALLOW)
        .map(|v| v.to_str().unwrap().to_string());
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    (
        status,
        ct,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        allow,
    )
}

fn json_req(method: &str, path: &str, ct: Option<&str>, body: &str) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(path);
    if let Some(c) = ct {
        b = b.header(header::CONTENT_TYPE, c);
    }
    b.body(Body::from(body.to_string())).unwrap()
}

#[sqlx::test]
async fn framework_errors_come_back_as_json(db: PgPool) {
    let r = router(db, true);
    let cases = [
        // 본문이 JSON이 아님
        (
            json_req("POST", "/auth/login", Some("application/json"), "{not json"),
            StatusCode::BAD_REQUEST,
        ),
        // Content-Type 없음
        (
            json_req(
                "POST",
                "/auth/login",
                None,
                r#"{"username":"a","password":"b"}"#,
            ),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
        // 칸이 빠짐
        (
            json_req(
                "POST",
                "/auth/login",
                Some("application/json"),
                r#"{"username":"a"}"#,
            ),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        // 경로의 숫자 형식이 틀림
        (
            json_req("GET", "/broadcasts/abc", None, ""),
            StatusCode::BAD_REQUEST,
        ),
        (
            json_req("GET", "/recordings/99999999999999999999", None, ""),
            StatusCode::BAD_REQUEST,
        ),
        // 없는 길
        (
            json_req("GET", "/no-such-path", None, ""),
            StatusCode::NOT_FOUND,
        ),
    ];
    for (req, want) in cases {
        let path = req.uri().to_string();
        let (status, ct, json, _) = send(&r, req).await;
        assert_eq!(status, want, "{path}");
        assert_eq!(ct.as_deref(), Some("application/json"), "{path}");
        assert!(
            json["error"].as_str().is_some_and(|m| !m.is_empty()),
            "{path}: {json}"
        );
    }
}

#[sqlx::test]
async fn wrong_method_is_json_and_keeps_allow_header(db: PgPool) {
    let r = router(db, true);
    let (status, ct, json, allow) = send(&r, json_req("PUT", "/auth/login", None, "")).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(ct.as_deref(), Some("application/json"));
    assert!(json["error"].is_string());
    assert!(allow.is_some_and(|a| a.contains("POST")));
}

#[sqlx::test]
async fn existing_json_errors_are_left_alone(db: PgPool) {
    let r = router(db, true);
    let (status, _, json, _) = send(&r, json_req("GET", "/broadcasts/999", None, "")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json["error"], "찾을 수 없습니다");
    let (status, _, json, _) = send(
        &r,
        json_req(
            "POST",
            "/auth/signup",
            Some("application/json"),
            r#"{"username":"a","password":"password123"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"], "아이디는 3~20자여야 합니다");
}

#[sqlx::test]
async fn demo_login_accepts_only_exact_numbers(db: PgPool) {
    let r = router(db, true);
    // %31은 주소 규칙상 "1"과 같은 글자라 1로 받는다(정상)
    for n in ["01", "+1", "1.0", "3", "0", "-1"] {
        let (status, ct, _, _) =
            send(&r, json_req("POST", &format!("/auth/demo/{n}"), None, "")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{n}");
        assert_eq!(ct.as_deref(), Some("application/json"), "{n}");
    }
    let (status, _, json, _) = send(&r, json_req("POST", "/auth/demo/2", None, "")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["username"], "demo2");
}

#[sqlx::test]
async fn config_tells_the_screen_what_is_on(db: PgPool) {
    let (_, _, on, _) = send(
        &router(db.clone(), true),
        json_req("GET", "/config", None, ""),
    )
    .await;
    assert_eq!(on["demo_login"], true);
    assert_eq!(on["donations"], false);
    let (_, _, off, _) = send(&router(db, false), json_req("GET", "/config", None, "")).await;
    assert_eq!(off["demo_login"], false);
}

#[sqlx::test]
async fn expired_sessions_of_all_users_are_cleaned(db: PgPool) {
    let r = router(db.clone(), true);
    for n in ["1", "2"] {
        send(&r, json_req("POST", &format!("/auth/demo/{n}"), None, "")).await;
    }
    // demo1의 세션만 만료시킨다
    sqlx::query(
        "UPDATE sessions SET expires_at = now() - interval '1 minute'
          WHERE user_id = (SELECT id FROM users WHERE username = 'demo1')",
    )
    .execute(&db)
    .await
    .unwrap();
    assert_eq!(cleanup_sessions(&db).await.unwrap(), 1);
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(left, 1);
}

#[sqlx::test]
async fn unknown_path_is_json_in_the_production_router(db: PgPool) {
    // main.rs와 똑같이 OME 웹훅 라우터와 합친 라우터로 본다
    let r =
        router(db.clone(), true).merge(onair_backend::ome_routes(onair_backend::ome::OmeState {
            secret: "test-secret-for-ome-webhook".into(),
            db,
            ome: Default::default(),
            kick: Default::default(),
        }));
    let (status, ct, json, _) = send(&r, json_req("GET", "/no-such-path", None, "")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(ct.as_deref(), Some("application/json"));
    assert_eq!(json["error"], "찾을 수 없습니다");
    // HEAD 오류 응답에는 본문이 없다
    let res = r
        .clone()
        .oneshot(
            Request::builder()
                .method("HEAD")
                .uri("/no-such-path")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert!(
        to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap()
            .is_empty()
    );
    // OME 웹훅은 그대로 있다(서명이 없으면 401)
    let (status, _, _, _) = send(
        &r,
        json_req("POST", "/ome/admission", Some("application/json"), "{}"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
