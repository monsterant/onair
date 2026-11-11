//! 방송 API 시험 (#3)과 소유자 검사 (#4): 비로그인·소유자·다른 사용자 셋으로 허용과 거절을 본다.
//! OME 관리 API는 가짜 서버를 띄워 "스트림 끊기" 요청이 오는지 본다.

use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Path, State},
    http::{HeaderMap, Request, StatusCode, header},
    routing::{delete, get},
};
use onair_backend::{AppState, app, ome::reconcile, ome_api::OmeApi};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

// ---------- 가짜 OME API ----------

#[derive(Clone, Default)]
struct FakeOme {
    streams: Arc<Mutex<HashSet<String>>>,
    deleted: Arc<Mutex<Vec<String>>>,
    /// Some(코드)면 목록 조회가 그 상태 코드로 실패한다(OME 오류 흉내)
    list_error: Arc<Mutex<Option<u16>>>,
}

/// 진짜 OME처럼 `Authorization: Basic base64(토큰)`이 없으면 401
const FAKE_TOKEN: &str = "fake-token-0123456789";
fn authorized(h: &HeaderMap) -> bool {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    h.get("authorization").and_then(|v| v.to_str().ok())
        == Some(format!("Basic {}", STANDARD.encode(FAKE_TOKEN)).as_str())
}

async fn fake_ome() -> (FakeOme, OmeApi) {
    let fake = FakeOme::default();
    let r = Router::new()
        .route(
            "/v1/vhosts/default/apps/app/streams",
            get(|State(f): State<FakeOme>, h: HeaderMap| async move {
                if !authorized(&h) {
                    return (StatusCode::UNAUTHORIZED, Json(json!({ "statusCode": 401 })));
                }
                if let Some(code) = *f.list_error.lock().unwrap() {
                    return (
                        StatusCode::from_u16(code).unwrap(),
                        Json(json!({ "statusCode": code })),
                    );
                }
                let list: Vec<String> = f.streams.lock().unwrap().iter().cloned().collect();
                (
                    StatusCode::OK,
                    Json(json!({ "statusCode": 200, "message": "OK", "response": list })),
                )
            }),
        )
        .route(
            "/v1/vhosts/default/apps/app/streams/{name}",
            delete(
                |State(f): State<FakeOme>, h: HeaderMap, Path(name): Path<String>| async move {
                    if !authorized(&h) {
                        return StatusCode::UNAUTHORIZED;
                    }
                    f.streams.lock().unwrap().remove(&name);
                    f.deleted.lock().unwrap().push(name);
                    StatusCode::OK
                },
            ),
        )
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, r).await.unwrap() });
    (fake, OmeApi::new(&format!("http://{addr}"), FAKE_TOKEN))
}

// ---------- 요청 도우미 ----------

async fn router(db: PgPool, ome: OmeApi) -> Router {
    app(AppState {
        rec: Default::default(),
        toss: Default::default(),
        db,
        auth: onair_backend::auth::AuthConfig {
            demo_login: true,
            ..Default::default()
        },
        ome,
    })
}

async fn call(
    r: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    cookie: Option<&str>,
) -> (StatusCode, Value) {
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
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// 데모 로그인으로 사용자 쿠키를 얻는다(1=소유자, 2=다른 사용자)
async fn login(r: &Router, n: u8) -> String {
    let req = Request::post(format!("/auth/demo/{n}"))
        .body(Body::empty())
        .unwrap();
    let res = r.clone().oneshot(req).await.unwrap();
    let sc = res
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap();
    sc.split(';')
        .next()
        .unwrap()
        .strip_prefix("onair_session=")
        .unwrap()
        .to_string()
}

async fn create(r: &Router, cookie: &str, title: &str) -> Value {
    let (code, b) = call(
        r,
        "POST",
        "/broadcasts",
        Some(json!({ "title": title, "description": "설명" })),
        Some(cookie),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED, "{b}");
    b
}

// ---------- 세 사용자 허용·거절 ----------

#[sqlx::test]
async fn access_matrix_for_anonymous_owner_and_other(db: PgPool) {
    let (_fake, ome) = fake_ome().await;
    let r = router(db, ome).await;
    let owner = login(&r, 1).await;
    let other = login(&r, 2).await;

    // 등록: 비로그인 401, 로그인 사용자는 가능
    assert_eq!(
        call(
            &r,
            "POST",
            "/broadcasts",
            Some(json!({ "title": "x" })),
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let b = create(&r, &owner, "소유자 방송").await;
    let id = b["id"].as_i64().unwrap();
    assert_eq!(b["owner_username"], "demo1");
    assert_eq!(b["status"], "ready");
    // 응답에 송출 키가 없다
    assert!(b.get("stream_key").is_none());

    let item = format!("/broadcasts/{id}");
    let publish = format!("/broadcasts/{id}/publish");
    let end = format!("/broadcasts/{id}/end");
    let patch = Some(json!({ "title": "바꾼 제목" }));

    // 목록·상세: 누구나
    for c in [None, Some(owner.as_str()), Some(other.as_str())] {
        let (code, list) = call(&r, "GET", "/broadcasts", None, c).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(list.as_array().unwrap().len(), 1);
        assert!(list[0].get("stream_key").is_none());
        assert_eq!(call(&r, "GET", &item, None, c).await.0, StatusCode::OK);
    }

    // 비로그인: 수정·삭제·송출 주소·종료 모두 401
    assert_eq!(
        call(&r, "PATCH", &item, patch.clone(), None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&r, "DELETE", &item, None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&r, "GET", &publish, None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&r, "POST", &end, None, None).await.0,
        StatusCode::UNAUTHORIZED
    );

    // 다른 사용자: 모두 403, 그리고 아무것도 바뀌지 않는다
    assert_eq!(
        call(&r, "PATCH", &item, patch.clone(), Some(&other))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&r, "DELETE", &item, None, Some(&other)).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&r, "GET", &publish, None, Some(&other)).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&r, "POST", &end, None, Some(&other)).await.0,
        StatusCode::FORBIDDEN
    );
    let (_, now) = call(&r, "GET", &item, None, None).await;
    assert_eq!(now["title"], "소유자 방송");
    assert_eq!(now["status"], "ready");

    // 소유자: 모두 가능
    let (code, edited) = call(&r, "PATCH", &item, patch, Some(&owner)).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(edited["title"], "바꾼 제목");
    assert_eq!(edited["description"], "설명");
    let (code, info) = call(&r, "GET", &publish, None, Some(&owner)).await;
    assert_eq!(code, StatusCode::OK);
    let key = info["stream_key"].as_str().unwrap();
    assert_eq!(key.len(), 64);
    assert_eq!(
        info["whip_path"],
        format!(
            "/app/{}?direction=whip&key={key}",
            b["stream_name"].as_str().unwrap()
        )
    );
    assert_eq!(
        call(&r, "POST", &end, None, Some(&owner)).await.1["status"],
        "ended"
    );
    assert_eq!(
        call(&r, "DELETE", &item, None, Some(&owner)).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&r, "GET", &item, None, None).await.0,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test]
async fn missing_broadcast_is_404(db: PgPool) {
    let r = router(db, OmeApi::default()).await;
    let owner = login(&r, 1).await;
    assert_eq!(
        call(&r, "GET", "/broadcasts/999", None, None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &r,
            "PATCH",
            "/broadcasts/999",
            Some(json!({ "title": "x" })),
            Some(&owner)
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&r, "DELETE", "/broadcasts/999", None, Some(&owner))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test]
async fn input_rules(db: PgPool) {
    let r = router(db, OmeApi::default()).await;
    let owner = login(&r, 1).await;
    for body in [
        json!({ "title": "   " }),
        json!({ "title": "가".repeat(101) }),
        json!({ "title": "ok", "description": "a".repeat(1001) }),
    ] {
        let (code, err) = call(&r, "POST", "/broadcasts", Some(body.clone()), Some(&owner)).await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "{body}");
        assert!(err["error"].is_string());
    }
    let b = create(&r, &owner, &"가".repeat(100)).await;
    assert_eq!(b["title"].as_str().unwrap().chars().count(), 100);
    let item = format!("/broadcasts/{}", b["id"]);
    // 수정도 같은 규칙. 빈 제목으로는 못 바꾼다. 아무것도 안 보내면 그대로.
    assert_eq!(
        call(
            &r,
            "PATCH",
            &item,
            Some(json!({ "title": "" })),
            Some(&owner)
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let (code, same) = call(&r, "PATCH", &item, Some(json!({})), Some(&owner)).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(same["title"], b["title"]);
}

#[sqlx::test]
async fn stream_names_and_keys_are_unique_per_broadcast(db: PgPool) {
    let r = router(db, OmeApi::default()).await;
    let owner = login(&r, 1).await;
    let a = create(&r, &owner, "a").await;
    let b = create(&r, &owner, "b").await;
    assert_ne!(a["stream_name"], b["stream_name"]);
    let name = a["stream_name"].as_str().unwrap();
    assert_eq!(name.len(), 16);
    assert!(
        name.chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    );
    let ka = call(
        &r,
        "GET",
        &format!("/broadcasts/{}/publish", a["id"]),
        None,
        Some(&owner),
    )
    .await
    .1;
    let kb = call(
        &r,
        "GET",
        &format!("/broadcasts/{}/publish", b["id"]),
        None,
        Some(&owner),
    )
    .await
    .1;
    assert_ne!(ka["stream_key"], kb["stream_key"]);
}

#[sqlx::test]
async fn list_shows_live_first_then_newest(db: PgPool) {
    let r = router(db.clone(), OmeApi::default()).await;
    let owner = login(&r, 1).await;
    let first = create(&r, &owner, "첫째").await;
    create(&r, &owner, "둘째").await;
    create(&r, &owner, "셋째").await;
    sqlx::query("UPDATE broadcasts SET status = 'live' WHERE id = $1")
        .bind(first["id"].as_i64().unwrap())
        .execute(&db)
        .await
        .unwrap();
    let (_, list) = call(&r, "GET", "/broadcasts", None, None).await;
    let titles: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, ["첫째", "셋째", "둘째"]);
}

// ---------- 종료와 삭제 때 송출 끊기 ----------

#[sqlx::test]
async fn end_marks_ended_and_stops_stream_idempotently(db: PgPool) {
    let (fake, ome) = fake_ome().await;
    let r = router(db.clone(), ome).await;
    let owner = login(&r, 1).await;
    let b = create(&r, &owner, "방송").await;
    let name = b["stream_name"].as_str().unwrap().to_string();
    sqlx::query("UPDATE broadcasts SET status = 'live', started_at = now() WHERE stream_name = $1")
        .bind(&name)
        .execute(&db)
        .await
        .unwrap();
    fake.streams.lock().unwrap().insert(name.clone());

    let end = format!("/broadcasts/{}/end", b["id"]);
    let (code, ended) = call(&r, "POST", &end, None, Some(&owner)).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(ended["status"], "ended");
    assert!(ended["ended_at"].is_string());
    assert_eq!(*fake.deleted.lock().unwrap(), vec![name.clone()]);
    // 다시 불러도 종료 그대로, 종료 시각도 그대로
    let (code, again) = call(&r, "POST", &end, None, Some(&owner)).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(again["ended_at"], ended["ended_at"]);
}

#[sqlx::test]
async fn deleting_any_broadcast_always_stops_its_stream(db: PgPool) {
    // 상태가 ready여도 OME에 스트림이 남아 있을 수 있다(늦은 closing 등). 그래서 삭제할 때는 항상 끊는다(#27 V4).
    let (fake, ome) = fake_ome().await;
    let r = router(db.clone(), ome).await;
    let owner = login(&r, 1).await;
    let live = create(&r, &owner, "방송 중").await;
    let idle = create(&r, &owner, "대기").await;
    sqlx::query("UPDATE broadcasts SET status = 'live' WHERE id = $1")
        .bind(live["id"].as_i64().unwrap())
        .execute(&db)
        .await
        .unwrap();
    for b in [&idle, &live] {
        let (code, _) = call(
            &r,
            "DELETE",
            &format!("/broadcasts/{}", b["id"]),
            None,
            Some(&owner),
        )
        .await;
        assert_eq!(code, StatusCode::NO_CONTENT);
    }
    assert_eq!(
        *fake.deleted.lock().unwrap(),
        vec![
            idle["stream_name"].as_str().unwrap().to_string(),
            live["stream_name"].as_str().unwrap().to_string()
        ]
    );
}

#[sqlx::test]
async fn editing_or_ending_one_broadcast_leaves_owners_others_alone(db: PgPool) {
    let (_fake, ome) = fake_ome().await;
    let r = router(db, ome).await;
    let owner = login(&r, 1).await;
    let a = create(&r, &owner, "A").await;
    let b = create(&r, &owner, "B").await;
    call(
        &r,
        "PATCH",
        &format!("/broadcasts/{}", a["id"]),
        Some(json!({ "title": "A2", "description": "새 설명" })),
        Some(&owner),
    )
    .await;
    call(
        &r,
        "POST",
        &format!("/broadcasts/{}/end", a["id"]),
        None,
        Some(&owner),
    )
    .await;
    let (_, now_b) = call(&r, "GET", &format!("/broadcasts/{}", b["id"]), None, None).await;
    assert_eq!(now_b["title"], "B");
    assert_eq!(now_b["description"], "설명");
    assert_eq!(now_b["status"], "ready");
}

#[sqlx::test]
async fn create_rejects_control_chars_and_invisible_titles(db: PgPool) {
    let r = router(db.clone(), OmeApi::default()).await;
    let owner = login(&r, 1).await;
    for body in [
        json!({ "title": "a\u{0}b" }),
        json!({ "title": "ok", "description": "a\u{0}" }),
        json!({ "title": "\u{200b}\u{200b}" }),
        json!({ "title": "\u{feff} \u{2060}" }),
        json!({ "title": "줄\n바꿈" }),
    ] {
        let (code, err) = call(&r, "POST", "/broadcasts", Some(body.clone()), Some(&owner)).await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "{body}");
        assert!(err["error"].is_string());
    }
    // 설명의 줄바꿈과 탭은 된다
    let (code, b) = call(
        &r,
        "POST",
        "/broadcasts",
        Some(json!({ "title": "ok", "description": "첫 줄\n둘째\t줄" })),
        Some(&owner),
    )
    .await;
    assert_eq!(code, StatusCode::CREATED);
    assert_eq!(b["description"], "첫 줄\n둘째\t줄");
    // PATCH도 같은 규칙
    let (code, _) = call(
        &r,
        "PATCH",
        &format!("/broadcasts/{}", b["id"]),
        Some(json!({ "title": "x\u{0}" })),
        Some(&owner),
    )
    .await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn open_broadcasts_are_capped_per_owner(db: PgPool) {
    let r = router(db, OmeApi::default()).await;
    let owner = login(&r, 1).await;
    let other = login(&r, 2).await;
    let mut ids = vec![];
    for i in 0..5 {
        ids.push(create(&r, &owner, &format!("방송{i}")).await["id"].clone());
    }
    let (code, err) = call(
        &r,
        "POST",
        "/broadcasts",
        Some(json!({ "title": "여섯째" })),
        Some(&owner),
    )
    .await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert!(err["error"].as_str().unwrap().contains("5개"));
    // 다른 사람은 따로 센다
    create(&r, &other, "다른 사람").await;
    // 하나를 끝내면 다시 만들 수 있다
    call(
        &r,
        "POST",
        &format!("/broadcasts/{}/end", ids[0]),
        None,
        Some(&owner),
    )
    .await;
    create(&r, &owner, "여섯째").await;
}

#[sqlx::test]
async fn create_is_rate_limited_per_client(db: PgPool) {
    let r = app(AppState {
        rec: Default::default(),
        toss: Default::default(),
        db,
        auth: onair_backend::auth::AuthConfig {
            demo_login: true,
            limiter: Default::default(),
            // 만들기 횟수는 로그인과 따로 센다(보안 점검 L3)
            comment_limiter: onair_backend::guard::RateLimiter::new(
                2,
                std::time::Duration::from_secs(60),
            ),
            notify_limiter: Default::default(),
        },
        ome: OmeApi::default(),
    });
    // 만들기 2번까지, 3번째 만들기는 429. 로그인은 막히지 않는다.
    let owner = login(&r, 1).await;
    create(&r, &owner, "a").await;
    create(&r, &owner, "b").await;
    let (code, _) = call(
        &r,
        "POST",
        "/broadcasts",
        Some(json!({ "title": "c" })),
        Some(&owner),
    )
    .await;
    assert_eq!(code, StatusCode::TOO_MANY_REQUESTS);
    let again = Request::post("/auth/demo/2").body(Body::empty()).unwrap();
    assert_eq!(
        r.clone().oneshot(again).await.unwrap().status(),
        StatusCode::OK
    );
}

#[sqlx::test]
async fn publish_info_is_not_cached(db: PgPool) {
    let r = router(db, OmeApi::default()).await;
    let owner = login(&r, 1).await;
    let b = create(&r, &owner, "a").await;
    let req = Request::get(format!("/broadcasts/{}/publish", b["id"]))
        .header(header::COOKIE, format!("onair_session={owner}"))
        .body(Body::empty())
        .unwrap();
    let res = r.clone().oneshot(req).await.unwrap();
    assert_eq!(
        res.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
}

#[sqlx::test]
async fn end_still_works_when_ome_is_unreachable(db: PgPool) {
    // 아무도 듣지 않는 주소: OME가 꺼져 있어도 종료 상태는 저장된다(웹훅이 다시 송출을 막는다)
    let r = router(
        db,
        OmeApi::new("http://127.0.0.1:1", "fake-token-0123456789"),
    )
    .await;
    let owner = login(&r, 1).await;
    let b = create(&r, &owner, "방송").await;
    let (code, ended) = call(
        &r,
        "POST",
        &format!("/broadcasts/{}/end", b["id"]),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(ended["status"], "ended");
}

// ---------- 맞춰 보기 ----------

#[sqlx::test]
async fn reconcile_fixes_stale_live_and_stops_ended_streams(db: PgPool) {
    let (fake, ome) = fake_ome().await;
    let r = router(db.clone(), ome.clone()).await;
    let owner = login(&r, 1).await;
    let stale = create(&r, &owner, "끊겼는데 live").await;
    let fresh = create(&r, &owner, "방금 시작").await;
    let real = create(&r, &owner, "진짜 방송 중").await;
    let ended = create(&r, &owner, "종료").await;
    // 늦은 closing 때문에 ready가 됐지만 OME에는 스트림이 살아 있는 방송(#27 V3)
    let revived = create(&r, &owner, "다시 붙음").await;
    let set = |id: &Value, status: &str, ago: i32| {
        let db = db.clone();
        let (id, status) = (id.as_i64().unwrap(), status.to_string());
        async move {
            sqlx::query("UPDATE broadcasts SET status = $1, started_at = now() - make_interval(secs => $2) WHERE id = $3")
                .bind(status)
                .bind(ago as f64)
                .bind(id)
                .execute(&db)
                .await
                .unwrap();
        }
    };
    set(&stale["id"], "live", 60).await;
    set(&fresh["id"], "live", 5).await;
    set(&real["id"], "live", 60).await;
    set(&ended["id"], "ended", 60).await;
    {
        let mut s = fake.streams.lock().unwrap();
        s.insert(real["stream_name"].as_str().unwrap().to_string());
        s.insert(ended["stream_name"].as_str().unwrap().to_string());
        s.insert(revived["stream_name"].as_str().unwrap().to_string());
        // DB에 없는 이름(삭제된 방송 등)도 끊어야 한다(#27 V4)
        s.insert("ffffffffffffffff".to_string());
    }

    reconcile(&db, &ome).await.unwrap();

    let status = |b: &Value| {
        let db = db.clone();
        let id = b["id"].as_i64().unwrap();
        async move {
            sqlx::query_scalar::<_, String>("SELECT status FROM broadcasts WHERE id = $1")
                .bind(id)
                .fetch_one(&db)
                .await
                .unwrap()
        }
    };
    assert_eq!(status(&stale).await, "ready");
    assert_eq!(status(&fresh).await, "live", "20초 안은 기다린다");
    assert_eq!(status(&real).await, "live");
    assert_eq!(status(&ended).await, "ended");
    assert_eq!(
        status(&revived).await,
        "live",
        "OME에 있으면 live로 되돌린다"
    );
    let deleted: HashSet<String> = fake.deleted.lock().unwrap().iter().cloned().collect();
    let expected: HashSet<String> = [ended["stream_name"].as_str().unwrap(), "ffffffffffffffff"]
        .into_iter()
        .map(String::from)
        .collect();
    assert_eq!(deleted, expected);
}

#[sqlx::test]
async fn reconcile_does_nothing_when_ome_answers_error(db: PgPool) {
    // OME가 401·500 등으로 답하면 목록을 모르는 것이다. 이때 live를 ready로 떨어뜨리면 안 된다(#27 V21).
    for code in [401u16, 500] {
        let (fake, ome) = fake_ome().await;
        *fake.list_error.lock().unwrap() = Some(code);
        let r = router(db.clone(), ome.clone()).await;
        let owner = login(&r, 1).await;
        let b = create(&r, &owner, "방송").await;
        sqlx::query("UPDATE broadcasts SET status = 'live', started_at = now() - interval '1 hour' WHERE id = $1")
            .bind(b["id"].as_i64().unwrap())
            .execute(&db)
            .await
            .unwrap();
        reconcile(&db, &ome).await.unwrap();
        let (_, now) = call(&r, "GET", &format!("/broadcasts/{}", b["id"]), None, None).await;
        assert_eq!(now["status"], "live", "OME {code}");
        call(
            &r,
            "DELETE",
            &format!("/broadcasts/{}", b["id"]),
            None,
            Some(&owner),
        )
        .await;
    }
}

#[sqlx::test]
async fn ome_api_client_sends_the_token(db: PgPool) {
    // 가짜 OME는 토큰이 틀리면 401로 거절한다. 같은 서버에 맞는 토큰과 틀린 토큰으로 각각 끊어 본다(#27 V21).
    let (fake, good) = fake_ome().await;
    let bad = OmeApi::new(good.base().unwrap(), "wrong-token-0123456789");
    assert!(
        !format!("{good:?}").contains(FAKE_TOKEN),
        "Debug에 토큰이 보이면 안 된다"
    );
    let r = router(db, good.clone()).await;
    let owner = login(&r, 1).await;
    let b = create(&r, &owner, "방송").await;
    let name = b["stream_name"].as_str().unwrap().to_string();
    bad.stop_stream(&name).await;
    assert!(
        fake.deleted.lock().unwrap().is_empty(),
        "틀린 토큰이면 끊기지 않는다"
    );
    good.stop_stream(&name).await;
    assert_eq!(*fake.deleted.lock().unwrap(), vec![name]);
}

#[sqlx::test]
async fn reconcile_does_nothing_when_ome_unreachable(db: PgPool) {
    let r = router(db.clone(), OmeApi::default()).await;
    let owner = login(&r, 1).await;
    let b = create(&r, &owner, "방송").await;
    sqlx::query("UPDATE broadcasts SET status = 'live', started_at = now() - interval '1 hour'")
        .execute(&db)
        .await
        .unwrap();
    reconcile(
        &db,
        &OmeApi::new("http://127.0.0.1:1", "fake-token-0123456789"),
    )
    .await
    .unwrap();
    let (_, now) = call(&r, "GET", &format!("/broadcasts/{}", b["id"]), None, None).await;
    assert_eq!(now["status"], "live");
}

#[sqlx::test]
async fn owner_can_rotate_the_stream_key(db: PgPool) {
    let (fake, ome) = fake_ome().await;
    let r = router(db.clone(), ome).await;
    let owner = login(&r, 1).await;
    let other = login(&r, 2).await;
    let b = create(&r, &owner, "키 바꾸기").await;
    let id = b["id"].as_i64().unwrap();
    let name = b["stream_name"].as_str().unwrap().to_string();
    let (_, before) = call(
        &r,
        "GET",
        &format!("/broadcasts/{id}/publish"),
        None,
        Some(&owner),
    )
    .await;
    let path = format!("/broadcasts/{id}/stream-key");
    // 비로그인 401, 다른 사용자 403(키는 그대로)
    assert_eq!(
        call(&r, "POST", &path, None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&r, "POST", &path, None, Some(&other)).await.0,
        StatusCode::FORBIDDEN
    );
    let (_, same) = call(
        &r,
        "GET",
        &format!("/broadcasts/{id}/publish"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(same["stream_key"], before["stream_key"]);
    // 소유자: 새 키, 송출 주소도 새 키, 옛 송출은 끊는다
    let (code, after) = call(&r, "POST", &path, None, Some(&owner)).await;
    assert_eq!(code, StatusCode::OK);
    assert_ne!(after["stream_key"], before["stream_key"]);
    assert_eq!(after["stream_key"].as_str().unwrap().len(), 64);
    assert!(
        after["whip_path"]
            .as_str()
            .unwrap()
            .ends_with(after["stream_key"].as_str().unwrap())
    );
    assert!(fake.deleted.lock().unwrap().contains(&name));
    let (_, now) = call(
        &r,
        "GET",
        &format!("/broadcasts/{id}/publish"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(now["stream_key"], after["stream_key"]);
    // 종료된 방송은 바꿀 수 없다
    call(
        &r,
        "POST",
        &format!("/broadcasts/{id}/end"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(
        call(&r, "POST", &path, None, Some(&owner)).await.0,
        StatusCode::CONFLICT
    );
    let _ = db;
}
