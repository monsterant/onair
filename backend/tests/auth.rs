//! 회원가입·로그인·로그아웃 동작 시험 (#4).
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

fn router(db: PgPool, demo_login: bool) -> Router {
    app(AppState {
        ome: Default::default(),
        db,
        auth: AuthConfig {
            demo_login,
            ..Default::default()
        },
    })
}

struct Res {
    status: StatusCode,
    /// 응답이 준 세션 쿠키 값(있으면)
    cookie: Option<String>,
    set_cookie: Option<String>,
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
    let set_cookie = res
        .headers()
        .get(header::SET_COOKIE)
        .map(|v| v.to_str().unwrap().to_string());
    let cookie = set_cookie.as_ref().and_then(|s| {
        let v = s.split(';').next()?.strip_prefix("onair_session=")?;
        (!v.is_empty()).then(|| v.to_string())
    });
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    Res {
        status,
        cookie,
        set_cookie,
        json: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    }
}

fn creds(u: &str, p: &str) -> Option<Value> {
    Some(json!({ "username": u, "password": p }))
}

#[sqlx::test]
async fn signup_logs_in_and_me_returns_user(db: PgPool) {
    let r = router(db, false);
    let res = call(
        &r,
        "POST",
        "/auth/signup",
        creds("Alice_1", "password123"),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::CREATED);
    assert_eq!(res.json["username"], "alice_1");
    let sc = res.set_cookie.unwrap();
    assert!(sc.contains("HttpOnly"), "{sc}");
    assert!(sc.contains("Secure"), "{sc}");
    assert!(sc.contains("SameSite=Lax"), "{sc}");
    assert!(sc.contains("Path=/"), "{sc}");

    let me = call(&r, "GET", "/auth/me", None, res.cookie.as_deref()).await;
    assert_eq!(me.status, StatusCode::OK);
    assert_eq!(me.json["username"], "alice_1");
}

#[sqlx::test]
async fn duplicate_signup_is_409_even_with_other_case(db: PgPool) {
    let r = router(db, false);
    assert_eq!(
        call(
            &r,
            "POST",
            "/auth/signup",
            creds("bob", "password123"),
            None
        )
        .await
        .status,
        StatusCode::CREATED
    );
    let dup = call(
        &r,
        "POST",
        "/auth/signup",
        creds("BOB", "otherpass99"),
        None,
    )
    .await;
    assert_eq!(dup.status, StatusCode::CONFLICT);
    assert!(dup.cookie.is_none());
}

#[sqlx::test]
async fn concurrent_signups_create_only_one_user(db: PgPool) {
    let r = router(db.clone(), false);
    let mut handles = vec![];
    for _ in 0..8 {
        let r = r.clone();
        handles.push(tokio::spawn(async move {
            call(
                &r,
                "POST",
                "/auth/signup",
                creds("racer", "password123"),
                None,
            )
            .await
            .status
        }));
    }
    let mut created = 0;
    for h in handles {
        let s = h.await.unwrap();
        if s == StatusCode::CREATED {
            created += 1;
        } else {
            assert_eq!(s, StatusCode::CONFLICT);
        }
    }
    assert_eq!(created, 1);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE username = 'racer'")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(n, 1);
}

#[sqlx::test]
async fn invalid_signup_input_is_400(db: PgPool) {
    let r = router(db.clone(), false);
    for (u, p) in [
        ("ab", "password123"),
        ("bad-name", "password123"),
        ("한글", "password123"),
        ("validname", "short"),
        ("demo1", "password123"),
        ("demouser", "password123"),
    ] {
        let res = call(&r, "POST", "/auth/signup", creds(u, p), None).await;
        assert_eq!(res.status, StatusCode::BAD_REQUEST, "{u}/{p}");
        assert!(res.json["error"].is_string());
    }
    // 본문이 JSON이 아니면 axum이 거절한다(4xx)
    let res = call(
        &r,
        "POST",
        "/auth/signup",
        Some(json!("not an object")),
        None,
    )
    .await;
    assert!(res.status.is_client_error());
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

#[sqlx::test]
async fn login_success_and_failures(db: PgPool) {
    let r = router(db, false);
    call(
        &r,
        "POST",
        "/auth/signup",
        creds("carol", "password123"),
        None,
    )
    .await;

    let ok = call(
        &r,
        "POST",
        "/auth/login",
        creds("CAROL", "password123"),
        None,
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK);
    assert!(ok.cookie.is_some());

    let wrong_pw = call(
        &r,
        "POST",
        "/auth/login",
        creds("carol", "password124"),
        None,
    )
    .await;
    let no_user = call(
        &r,
        "POST",
        "/auth/login",
        creds("nobody", "password123"),
        None,
    )
    .await;
    let bad_name = call(&r, "POST", "/auth/login", creds("x", "password123"), None).await;
    for res in [&wrong_pw, &no_user, &bad_name] {
        assert_eq!(res.status, StatusCode::UNAUTHORIZED);
        assert!(res.cookie.is_none());
    }
    // 아이디가 있는지 없는지 메시지로 구분할 수 없다
    assert_eq!(wrong_pw.json, no_user.json);
    assert_eq!(wrong_pw.json, bad_name.json);
}

#[sqlx::test]
async fn me_without_or_with_bad_cookie_is_401(db: PgPool) {
    let r = router(db, false);
    assert_eq!(
        call(&r, "GET", "/auth/me", None, None).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&r, "GET", "/auth/me", None, Some("deadbeef"))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test]
async fn logout_kills_session_and_is_idempotent(db: PgPool) {
    let r = router(db, false);
    let s = call(
        &r,
        "POST",
        "/auth/signup",
        creds("dave", "password123"),
        None,
    )
    .await;
    let c = s.cookie.unwrap();

    let out = call(&r, "POST", "/auth/logout", None, Some(&c)).await;
    assert_eq!(out.status, StatusCode::NO_CONTENT);
    assert!(out.set_cookie.unwrap().contains("Max-Age=0"));
    // 지운 세션의 쿠키를 다시 써도 로그인되지 않는다(서버에서 끊김)
    assert_eq!(
        call(&r, "GET", "/auth/me", None, Some(&c)).await.status,
        StatusCode::UNAUTHORIZED
    );
    // 두 번 해도, 쿠키 없이 해도 204
    assert_eq!(
        call(&r, "POST", "/auth/logout", None, Some(&c))
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&r, "POST", "/auth/logout", None, None).await.status,
        StatusCode::NO_CONTENT
    );
}

#[sqlx::test]
async fn logout_only_ends_that_session(db: PgPool) {
    let r = router(db, false);
    let a = call(
        &r,
        "POST",
        "/auth/signup",
        creds("erin", "password123"),
        None,
    )
    .await
    .cookie
    .unwrap();
    let b = call(
        &r,
        "POST",
        "/auth/login",
        creds("erin", "password123"),
        None,
    )
    .await
    .cookie
    .unwrap();
    call(&r, "POST", "/auth/logout", None, Some(&a)).await;
    assert_eq!(
        call(&r, "GET", "/auth/me", None, Some(&b)).await.status,
        StatusCode::OK
    );
}

#[sqlx::test]
async fn expired_session_is_rejected(db: PgPool) {
    let r = router(db.clone(), false);
    let c = call(
        &r,
        "POST",
        "/auth/signup",
        creds("frank", "password123"),
        None,
    )
    .await
    .cookie
    .unwrap();
    sqlx::query("UPDATE sessions SET expires_at = now() - interval '1 second'")
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(
        call(&r, "GET", "/auth/me", None, Some(&c)).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test]
async fn session_token_is_stored_hashed(db: PgPool) {
    let r = router(db.clone(), false);
    let c = call(
        &r,
        "POST",
        "/auth/signup",
        creds("gina", "password123"),
        None,
    )
    .await
    .cookie
    .unwrap();
    let stored: Vec<u8> = sqlx::query_scalar("SELECT token_hash FROM sessions")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_ne!(stored, c.as_bytes());
    assert_eq!(stored.len(), 32);
    let pw: String = sqlx::query_scalar("SELECT password_hash FROM users")
        .fetch_one(&db)
        .await
        .unwrap();
    assert!(pw.starts_with("$argon2id$"));
    assert!(!pw.contains("password123"));
}

#[sqlx::test]
async fn demo_login_disabled_is_404(db: PgPool) {
    let r = router(db.clone(), false);
    let res = call(&r, "POST", "/auth/demo/1", None, None).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert!(res.cookie.is_none());
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

#[sqlx::test]
async fn demo_login_enabled_two_users_idempotent(db: PgPool) {
    let r = router(db.clone(), true);
    let d1 = call(&r, "POST", "/auth/demo/1", None, None).await;
    assert_eq!(d1.status, StatusCode::OK);
    assert_eq!(d1.json["username"], "demo1");
    let d1b = call(&r, "POST", "/auth/demo/1", None, None).await;
    assert_eq!(d1b.json["id"], d1.json["id"]);
    let d2 = call(&r, "POST", "/auth/demo/2", None, None).await;
    assert_eq!(d2.json["username"], "demo2");
    assert_ne!(d2.json["id"], d1.json["id"]);
    for n in ["0", "3", "abc"] {
        assert!(
            call(&r, "POST", &format!("/auth/demo/{n}"), None, None)
                .await
                .status
                .is_client_error()
        );
    }
    let me = call(&r, "GET", "/auth/me", None, d2.cookie.as_deref()).await;
    assert_eq!(me.json["username"], "demo2");
    // 데모 사용자는 비밀번호로는 로그인할 수 없다
    for pw in ["!demo-no-password", "password123"] {
        assert_eq!(
            call(&r, "POST", "/auth/login", creds("demo1", pw), None)
                .await
                .status,
            StatusCode::UNAUTHORIZED
        );
    }
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(n, 2);
}

// ---------- #24 재수정 시험 ----------

/// 헤더를 마음대로 붙여 보내는 요청
async fn call_h(r: &Router, method: &str, path: &str, headers: &[(&str, &str)]) -> StatusCode {
    let mut req = Request::builder().method(method).uri(path);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    r.clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

#[sqlx::test]
async fn session_cookie_lasts_seven_days(db: PgPool) {
    let r = router(db, true);
    let s = call(
        &r,
        "POST",
        "/auth/signup",
        creds("hana", "password123"),
        None,
    )
    .await;
    let l = call(
        &r,
        "POST",
        "/auth/login",
        creds("hana", "password123"),
        None,
    )
    .await;
    let d = call(&r, "POST", "/auth/demo/1", None, None).await;
    for res in [s, l, d] {
        let sc = res.set_cookie.unwrap();
        assert!(sc.contains("Max-Age=604800"), "{sc}");
    }
}

#[sqlx::test]
async fn relogin_removes_previous_session(db: PgPool) {
    let r = router(db.clone(), true);
    let old = call(&r, "POST", "/auth/demo/1", None, None)
        .await
        .cookie
        .unwrap();
    let new = call(&r, "POST", "/auth/demo/2", None, Some(&old))
        .await
        .cookie
        .unwrap();
    assert_eq!(
        call(&r, "GET", "/auth/me", None, Some(&old)).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&r, "GET", "/auth/me", None, Some(&new)).await.json["username"],
        "demo2"
    );
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(n, 1);
}

#[sqlx::test]
async fn login_clears_expired_sessions_of_that_user(db: PgPool) {
    let r = router(db.clone(), false);
    call(
        &r,
        "POST",
        "/auth/signup",
        creds("ivan", "password123"),
        None,
    )
    .await;
    sqlx::query("UPDATE sessions SET expires_at = now() - interval '1 second'")
        .execute(&db)
        .await
        .unwrap();
    call(
        &r,
        "POST",
        "/auth/login",
        creds("ivan", "password123"),
        None,
    )
    .await;
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(n, 1);
}

#[sqlx::test]
async fn each_cookie_maps_to_its_own_user(db: PgPool) {
    let r = router(db, false);
    let a = call(
        &r,
        "POST",
        "/auth/signup",
        creds("user_a", "password123"),
        None,
    )
    .await
    .cookie
    .unwrap();
    let b = call(
        &r,
        "POST",
        "/auth/signup",
        creds("user_b", "password123"),
        None,
    )
    .await
    .cookie
    .unwrap();
    assert_eq!(
        call(&r, "GET", "/auth/me", None, Some(&a)).await.json["username"],
        "user_a"
    );
    assert_eq!(
        call(&r, "GET", "/auth/me", None, Some(&b)).await.json["username"],
        "user_b"
    );
    // 다른 사람의 세션이 살아 있어도, 틀린 쿠키로는 로그인되지 않는다
    assert_eq!(
        call(&r, "GET", "/auth/me", None, Some("00ff")).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test]
async fn overlong_password_login_is_401_without_cookie(db: PgPool) {
    let r = router(db, false);
    call(
        &r,
        "POST",
        "/auth/signup",
        creds("judy", "password123"),
        None,
    )
    .await;
    let long = "x".repeat(129);
    let res = call(&r, "POST", "/auth/login", creds("judy", &long), None).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    assert!(res.cookie.is_none());
    assert!(onair_backend::auth::login_input_ok(&"x".repeat(128)));
    assert!(!onair_backend::auth::login_input_ok(&long));
}

#[sqlx::test]
async fn cross_site_posts_are_rejected(db: PgPool) {
    let r = router(db.clone(), true);
    let evil = [
        ("host", "onair.example"),
        ("origin", "https://evil.example"),
    ];
    let fetch_cross = [("host", "onair.example"), ("sec-fetch-site", "cross-site")];
    for path in [
        "/auth/demo/1",
        "/auth/logout",
        "/auth/login",
        "/auth/signup",
    ] {
        assert_eq!(
            call_h(&r, "POST", path, &evil).await,
            StatusCode::FORBIDDEN,
            "{path}"
        );
        assert_eq!(
            call_h(&r, "POST", path, &fetch_cross).await,
            StatusCode::FORBIDDEN,
            "{path}"
        );
    }
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(n, 0);
    // 같은 사이트에서 온 요청과 GET은 그대로 된다
    let same = [
        ("host", "onair.example"),
        ("origin", "https://onair.example"),
        ("sec-fetch-site", "same-origin"),
    ];
    assert_eq!(
        call_h(&r, "POST", "/auth/demo/1", &same).await,
        StatusCode::OK
    );
    assert_eq!(call_h(&r, "GET", "/health", &evil).await, StatusCode::OK);
}

#[sqlx::test]
async fn auth_requests_are_rate_limited_per_client(db: PgPool) {
    let r = app(AppState {
        ome: Default::default(),
        db,
        auth: AuthConfig {
            demo_login: true,
            limiter: onair_backend::guard::RateLimiter::new(2, std::time::Duration::from_secs(60)),
            comment_limiter: Default::default(),
        },
    });
    let ip1 = [("x-forwarded-for", "1.1.1.1")];
    let ip2 = [("x-forwarded-for", "2.2.2.2")];
    assert_eq!(
        call_h(&r, "POST", "/auth/demo/1", &ip1).await,
        StatusCode::OK
    );
    assert_eq!(
        call_h(&r, "POST", "/auth/demo/1", &ip1).await,
        StatusCode::OK
    );
    assert_eq!(
        call_h(&r, "POST", "/auth/demo/1", &ip1).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        call_h(&r, "POST", "/auth/login", &ip1).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    // 다른 IP는 따로 센다. 로그인 상태 조회(me)와 로그아웃은 제한하지 않는다.
    assert_eq!(
        call_h(&r, "POST", "/auth/demo/2", &ip2).await,
        StatusCode::OK
    );
    assert_eq!(
        call_h(&r, "GET", "/auth/me", &ip1).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call_h(&r, "POST", "/auth/logout", &ip1).await,
        StatusCode::NO_CONTENT
    );
}
