//! 심사자마다 따로 쓰는 테스트 계정 시험 (#35).
//! `#[sqlx::test]`는 시험마다 빈 DB를 새로 만들고 migrations를 적용해 준다(DATABASE_URL의 서버를 쓴다).

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use onair_backend::{
    AppState, app,
    auth::{AuthConfig, cleanup_guests},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

fn router(db: PgPool, demo_login: bool) -> Router {
    app(AppState {
        chat: Default::default(),
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

async fn guest(r: &Router) -> (i64, String, String) {
    let g = call(r, "POST", "/auth/guest", None, None).await;
    assert_eq!(g.status, StatusCode::OK);
    (
        g.json["id"].as_i64().unwrap(),
        g.json["username"].as_str().unwrap().to_string(),
        g.cookie.expect("세션 쿠키"),
    )
}

async fn count(db: &PgPool, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(db).await.unwrap()
}

async fn count_for(db: &PgPool, sql: &'static str, id: i64) -> i64 {
    sqlx::query_scalar(sql)
        .bind(id)
        .fetch_one(db)
        .await
        .unwrap()
}

#[sqlx::test]
async fn guest_login_is_404_when_demo_login_is_off(db: PgPool) {
    let r = router(db.clone(), false);
    let res = call(&r, "POST", "/auth/guest", None, None).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert!(res.cookie.is_none());
    assert_eq!(count(&db, "SELECT count(*) FROM users").await, 0);
}

#[sqlx::test]
async fn each_guest_call_makes_a_new_user_that_cannot_use_a_password(db: PgPool) {
    let r = router(db.clone(), true);
    let (id1, name1, c1) = guest(&r).await;
    let (id2, name2, c2) = guest(&r).await;
    assert_ne!(id1, id2);
    assert_ne!(name1, name2);
    for n in [&name1, &name2] {
        assert!(n.starts_with("guest_"), "{n}");
        assert_eq!(n.len(), "guest_".len() + 12, "{n}");
    }
    assert_eq!(
        call(&r, "GET", "/auth/me", None, Some(&c1)).await.json["username"],
        name1.as_str()
    );
    assert_eq!(
        call(&r, "GET", "/auth/me", None, Some(&c2)).await.json["username"],
        name2.as_str()
    );
    // 테스트 계정은 비밀번호로는 로그인할 수 없다
    for pw in ["!guest-no-password", "password123"] {
        let res = call(
            &r,
            "POST",
            "/auth/login",
            Some(json!({"username": name1, "password": pw})),
            None,
        )
        .await;
        assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    }
}

#[sqlx::test]
async fn signup_cannot_take_guest_names(db: PgPool) {
    let r = router(db.clone(), true);
    for u in ["guest_abc", "guest_0123456789ab", "guestbook"] {
        let res = call(
            &r,
            "POST",
            "/auth/signup",
            Some(json!({"username": u, "password": "password123"})),
            None,
        )
        .await;
        assert_eq!(res.status, StatusCode::BAD_REQUEST, "{u}");
    }
    assert_eq!(count(&db, "SELECT count(*) FROM users").await, 0);
}

#[sqlx::test]
async fn one_guest_cannot_change_another_guests_broadcast(db: PgPool) {
    let r = router(db.clone(), true);
    let (_, _, owner) = guest(&r).await;
    let (_, _, other) = guest(&r).await;
    let b = call(
        &r,
        "POST",
        "/broadcasts",
        Some(json!({"title": "내 방송", "description": ""})),
        Some(&owner),
    )
    .await;
    assert_eq!(b.status, StatusCode::CREATED);
    let id = b.json["id"].as_i64().unwrap();
    let path = format!("/broadcasts/{id}");

    let patch = Some(json!({"title": "남이 바꿈", "description": ""}));
    assert_eq!(
        call(&r, "PATCH", &path, patch, Some(&other)).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&r, "DELETE", &path, None, Some(&other)).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&r, "POST", &format!("{path}/end"), None, Some(&other))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&r, "GET", &format!("{path}/publish"), None, Some(&other))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&r, "GET", &path, None, None).await.json["title"],
        "내 방송"
    );

    assert_eq!(
        call(&r, "DELETE", &path, None, Some(&owner)).await.status,
        StatusCode::NO_CONTENT
    );
}

#[sqlx::test]
async fn cleanup_removes_only_old_guests_that_are_not_live(db: PgPool) {
    let r = router(db.clone(), true);
    let (old_id, _, old_cookie) = guest(&r).await;
    let (live_id, _, live_cookie) = guest(&r).await;
    let (fresh_id, _, _) = guest(&r).await;
    // 일반 회원과 demo1도 오래된 것으로 만든다. 이들은 지우면 안 된다.
    call(
        &r,
        "POST",
        "/auth/signup",
        Some(json!({"username": "alice", "password": "password123"})),
        None,
    )
    .await;
    call(&r, "POST", "/auth/demo/1", None, None).await;

    for c in [&old_cookie, &live_cookie] {
        let b = call(
            &r,
            "POST",
            "/broadcasts",
            Some(json!({"title": "방송", "description": ""})),
            Some(c),
        )
        .await;
        assert_eq!(b.status, StatusCode::CREATED);
    }
    sqlx::query("UPDATE broadcasts SET status = 'live' WHERE owner_id = $1")
        .bind(live_id)
        .execute(&db)
        .await
        .unwrap();
    sqlx::query("UPDATE users SET created_at = now() - interval '25 hours' WHERE id <> $1")
        .bind(fresh_id)
        .execute(&db)
        .await
        .unwrap();

    assert_eq!(cleanup_guests(&db).await.unwrap(), 1);

    let left: Vec<String> = sqlx::query_scalar("SELECT username FROM users ORDER BY id")
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(left.len(), 4, "{left:?}");
    assert!(left.contains(&"alice".to_string()) && left.contains(&"demo1".to_string()));
    // 지운 테스트 계정의 방송과 세션도 함께 지워진다
    assert_eq!(
        count_for(
            &db,
            "SELECT count(*) FROM broadcasts WHERE owner_id = $1",
            old_id
        )
        .await,
        0
    );
    assert_eq!(
        count_for(
            &db,
            "SELECT count(*) FROM sessions WHERE user_id = $1",
            old_id
        )
        .await,
        0
    );
    assert_eq!(
        call(&r, "GET", "/auth/me", None, Some(&old_cookie))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    // 송출 중인 계정은 남는다
    assert_eq!(
        call(&r, "GET", "/auth/me", None, Some(&live_cookie))
            .await
            .status,
        StatusCode::OK
    );

    // 두 번 불러도 더 지울 것이 없다
    assert_eq!(cleanup_guests(&db).await.unwrap(), 0);
}

#[sqlx::test]
async fn cleanup_boundary_is_24_hours(db: PgPool) {
    let r = router(db.clone(), true);
    let (young, _, _) = guest(&r).await;
    let (old, _, _) = guest(&r).await;
    for (id, hours) in [(young, 23), (old, 25)] {
        sqlx::query(
            "UPDATE users SET created_at = now() - make_interval(hours => $2) WHERE id = $1",
        )
        .bind(id)
        .bind(hours)
        .execute(&db)
        .await
        .unwrap();
    }
    assert_eq!(cleanup_guests(&db).await.unwrap(), 1);
    let left: Vec<i64> = sqlx::query_scalar("SELECT id FROM users")
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(left, vec![young]);
}

#[sqlx::test]
async fn cleanup_keeps_real_accounts_named_like_guests(db: PgPool) {
    // 가입 금지 전에 만들어졌을 수 있는 guest_ 이름의 일반 회원(비밀번호 해시가 진짜)과,
    // 이름은 비슷하지만 밑줄이 없는 회원은 지우지 않는다
    for name in ["guest_realuser", "guestxrealuser"] {
        sqlx::query(
            "INSERT INTO users (username, password_hash, created_at)
             VALUES ($1, '$argon2id$v=19$m=19456,t=2,p=1$fake$fake', now() - interval '48 hours')",
        )
        .bind(name)
        .execute(&db)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO users (username, password_hash, created_at)
         VALUES ('guestxtest', '!guest-no-password', now() - interval '48 hours')",
    )
    .execute(&db)
    .await
    .unwrap();
    assert_eq!(cleanup_guests(&db).await.unwrap(), 0);
}

#[sqlx::test]
async fn guest_login_shares_the_login_rate_limit(db: PgPool) {
    let r = router(db.clone(), true);
    // 같은 IP(시험에서는 헤더가 없어 같은 키)로 1분에 20번까지
    for i in 0..20 {
        assert_eq!(
            call(&r, "POST", "/auth/guest", None, None).await.status,
            StatusCode::OK,
            "{i}"
        );
    }
    let over = call(&r, "POST", "/auth/guest", None, None).await;
    assert_eq!(over.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(over.json["error"].is_string());
    // 로그인과 같은 몫이라 로그인도 막힌다
    let login = call(
        &r,
        "POST",
        "/auth/login",
        Some(json!({"username": "nobody", "password": "password123"})),
        None,
    )
    .await;
    assert_eq!(login.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(count(&db, "SELECT count(*) FROM users").await, 20);
}
