//! 도네이션 시험 (#41). 토스 결제 API는 가짜 서버를 띄워 흉내 낸다(진짜 토스에 닿지 않는다).
//! 핵심: 같은 승인·통지가 여러 번, 동시에 와도 한 번만 적립되는지, 금액·주문을 서버가 확인하는지.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Path, State},
    http::{HeaderMap, Request, StatusCode, header},
    routing::{get, post},
};
use onair_backend::{AppState, app, auth::AuthConfig, toss::Toss};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

// ---------- 가짜 토스 ----------

#[derive(Clone)]
struct FakePayment {
    order_id: String,
    amount: i64,
    status: String,
}

#[derive(Clone, Default)]
struct FakeToss {
    /// 결제 키 → 결제(결제위젯에서 결제를 마친 상태를 흉내 낸다)
    payments: Arc<Mutex<HashMap<String, FakePayment>>>,
    confirm_calls: Arc<AtomicUsize>,
    get_calls: Arc<AtomicUsize>,
    /// 승인 응답을 늦춘다(동시 요청 시험)
    delay_ms: Arc<Mutex<u64>>,
}

const SECRET: &str = "test_gsk_fake";
fn authorized(h: &HeaderMap) -> bool {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    h.get("authorization").and_then(|v| v.to_str().ok())
        == Some(format!("Basic {}", STANDARD.encode(format!("{SECRET}:"))).as_str())
}

fn payment_json(key: &str, p: &FakePayment) -> Value {
    json!({ "paymentKey": key, "orderId": p.order_id, "totalAmount": p.amount, "status": p.status, "method": "카드" })
}

fn err(code: StatusCode, c: &str) -> (StatusCode, Json<Value>) {
    (code, Json(json!({ "code": c, "message": c })))
}

async fn fake_toss() -> (FakeToss, Toss) {
    let fake = FakeToss::default();
    let r = Router::new()
        .route(
            "/v1/payments/confirm",
            post(
                |State(f): State<FakeToss>, h: HeaderMap, Json(b): Json<Value>| async move {
                    f.confirm_calls.fetch_add(1, Ordering::SeqCst);
                    if !authorized(&h) {
                        return err(StatusCode::UNAUTHORIZED, "UNAUTHORIZED_KEY");
                    }
                    let delay = *f.delay_ms.lock().unwrap();
                    if delay > 0 {
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                    }
                    let key = b["paymentKey"].as_str().unwrap_or_default().to_string();
                    let mut map = f.payments.lock().unwrap();
                    let Some(p) = map.get_mut(&key) else {
                        return err(StatusCode::NOT_FOUND, "NOT_FOUND_PAYMENT_SESSION");
                    };
                    // 진짜 토스처럼 결제창에서 요청한 주문번호·금액과 다르면 거절한다
                    if b["orderId"].as_str() != Some(p.order_id.as_str())
                        || b["amount"].as_i64() != Some(p.amount)
                    {
                        return err(StatusCode::BAD_REQUEST, "INVALID_REQUEST");
                    }
                    if p.status == "DONE" {
                        return err(StatusCode::BAD_REQUEST, "ALREADY_PROCESSED_PAYMENT");
                    }
                    p.status = "DONE".into();
                    (StatusCode::OK, Json(payment_json(&key, p)))
                },
            ),
        )
        .route(
            "/v1/payments/{key}",
            get(
                |State(f): State<FakeToss>, h: HeaderMap, Path(key): Path<String>| async move {
                    f.get_calls.fetch_add(1, Ordering::SeqCst);
                    if !authorized(&h) {
                        return err(StatusCode::UNAUTHORIZED, "UNAUTHORIZED_KEY");
                    }
                    match f.payments.lock().unwrap().get(&key) {
                        Some(p) => (StatusCode::OK, Json(payment_json(&key, p))),
                        None => err(StatusCode::NOT_FOUND, "NOT_FOUND_PAYMENT"),
                    }
                },
            ),
        )
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, r).await.unwrap() });
    (
        fake,
        Toss::new(&format!("http://{addr}"), SECRET, "test_gck_fake"),
    )
}

impl FakeToss {
    /// 결제위젯에서 결제를 마쳤다고 치고 결제 키를 만든다(아직 승인 전: READY)
    fn pay(&self, key: &str, order_id: &str, amount: i64) {
        self.payments.lock().unwrap().insert(
            key.into(),
            FakePayment {
                order_id: order_id.into(),
                amount,
                status: "READY".into(),
            },
        );
    }
    fn set_status(&self, key: &str, status: &str) {
        self.payments.lock().unwrap().get_mut(key).unwrap().status = status.into();
    }
    fn confirms(&self) -> usize {
        self.confirm_calls.load(Ordering::SeqCst)
    }
}

// ---------- 요청 도우미 ----------

fn router(db: PgPool, toss: Toss) -> Router {
    app(AppState {
        toss,
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

async fn login(r: &Router, n: u8) -> String {
    call(r, "POST", &format!("/auth/demo/{n}"), None, None)
        .await
        .cookie
        .unwrap()
}

/// demo1이 방송을 만들고, demo2가 후원 주문을 만든다. (방송 id, demo2 쿠키, 주문번호)
async fn setup(r: &Router, amount: i64) -> (i64, String, String) {
    let owner = login(r, 1).await;
    let donor = login(r, 2).await;
    let b = call(
        r,
        "POST",
        "/broadcasts",
        Some(json!({"title": "후원 시험 방송"})),
        Some(&owner),
    )
    .await;
    let bid = b.json["id"].as_i64().unwrap();
    let order = order(r, bid, &donor, amount).await;
    (bid, donor, order)
}

async fn order(r: &Router, bid: i64, donor: &str, amount: i64) -> String {
    let o = call(
        r,
        "POST",
        &format!("/broadcasts/{bid}/donations"),
        Some(json!({"amount": amount, "message": "응원해요"})),
        Some(donor),
    )
    .await;
    assert_eq!(o.status, StatusCode::CREATED, "{:?}", o.json);
    o.json["order_id"].as_str().unwrap().to_string()
}

fn confirm_body(key: &str, order_id: &str, amount: i64) -> Option<Value> {
    Some(json!({ "paymentKey": key, "orderId": order_id, "amount": amount }))
}

async fn summary(r: &Router, bid: i64) -> Value {
    call(
        r,
        "GET",
        &format!("/broadcasts/{bid}/donations"),
        None,
        None,
    )
    .await
    .json
}

// ---------- 시험 ----------

#[sqlx::test]
async fn checkout_gives_order_and_client_key(db: PgPool) {
    let (_, toss) = fake_toss().await;
    let r = router(db.clone(), toss);
    let owner = login(&r, 1).await;
    let donor = login(&r, 2).await;
    let bid = call(
        &r,
        "POST",
        "/broadcasts",
        Some(json!({"title": "제목"})),
        Some(&owner),
    )
    .await
    .json["id"]
        .as_i64()
        .unwrap();
    let o = call(
        &r,
        "POST",
        &format!("/broadcasts/{bid}/donations"),
        Some(json!({"amount": 3000})),
        Some(&donor),
    )
    .await;
    assert_eq!(o.status, StatusCode::CREATED);
    assert_eq!(o.json["amount"], 3000);
    assert_eq!(o.json["client_key"], "test_gck_fake");
    assert_eq!(o.json["order_name"], "onair 후원: 제목");
    let oid = o.json["order_id"].as_str().unwrap();
    assert!(
        oid.starts_with("onair-") && (6..=64).contains(&oid.len()),
        "{oid}"
    );
    let status: String = sqlx::query_scalar("SELECT status FROM donations WHERE order_id = $1")
        .bind(oid)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(status, "pending");
    // 대기 주문은 내역·합계에 들어가지 않는다
    assert_eq!(
        summary(&r, bid).await,
        json!({"total": 0, "count": 0, "items": []})
    );
}

#[sqlx::test]
async fn confirm_credits_once_even_if_repeated(db: PgPool) {
    let (fake, toss) = fake_toss().await;
    let r = router(db, toss);
    let (bid, donor, oid) = setup(&r, 5000).await;
    fake.pay("pk_1", &oid, 5000);

    let first = call(
        &r,
        "POST",
        "/donations/confirm",
        confirm_body("pk_1", &oid, 5000),
        Some(&donor),
    )
    .await;
    assert_eq!(first.status, StatusCode::OK, "{:?}", first.json);
    assert_eq!(first.json["status"], "paid");
    for _ in 0..3 {
        let again = call(
            &r,
            "POST",
            "/donations/confirm",
            confirm_body("pk_1", &oid, 5000),
            Some(&donor),
        )
        .await;
        assert_eq!(again.status, StatusCode::OK);
        assert_eq!(again.json["status"], "paid");
    }
    // 이미 적립된 주문은 토스를 다시 부르지 않는다
    assert_eq!(fake.confirms(), 1);
    let s = summary(&r, bid).await;
    assert_eq!(s["total"], 5000);
    assert_eq!(s["count"], 1);
    assert_eq!(s["items"][0]["donor_username"], "demo2");
    assert_eq!(s["items"][0]["message"], "응원해요");
    assert!(s["items"][0]["paid_at"].is_string());
}

#[sqlx::test]
async fn repeated_notifications_credit_once(db: PgPool) {
    let (fake, toss) = fake_toss().await;
    let r = router(db, toss);
    let (bid, _, oid) = setup(&r, 10_000).await;
    // 결제는 토스에서 승인됐지만(브라우저가 돌아오지 못한 경우) 우리 주문은 아직 대기
    fake.pay("pk_n", &oid, 10_000);
    fake.set_status("pk_n", "DONE");
    for _ in 0..3 {
        // 통지 본문에 다른 금액·주문을 섞어 보내도 결제 키 말고는 쓰지 않는다
        let n = call(
            &r,
            "POST",
            "/donations/notify",
            Some(json!({"paymentKey": "pk_n", "orderId": "fake", "amount": 1})),
            None,
        )
        .await;
        assert_eq!(n.status, StatusCode::OK, "{:?}", n.json);
        assert_eq!(n.json["status"], "paid");
        assert_eq!(n.json["order_id"], oid.as_str());
    }
    assert_eq!(summary(&r, bid).await["total"], 10_000);
    assert_eq!(summary(&r, bid).await["count"], 1);
}

#[sqlx::test]
async fn confirm_and_notify_together_credit_once(db: PgPool) {
    let (fake, toss) = fake_toss().await;
    let r = router(db, toss);
    let (bid, donor, oid) = setup(&r, 7000).await;
    fake.pay("pk_c", &oid, 7000);
    *fake.delay_ms.lock().unwrap() = 200;
    // 확인 두 번과 통지 한 번이 동시에 온다
    let (a, b, _) = tokio::join!(
        call(
            &r,
            "POST",
            "/donations/confirm",
            confirm_body("pk_c", &oid, 7000),
            Some(&donor)
        ),
        call(
            &r,
            "POST",
            "/donations/confirm",
            confirm_body("pk_c", &oid, 7000),
            Some(&donor)
        ),
        async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            call(
                &r,
                "POST",
                "/donations/notify",
                Some(json!({"paymentKey": "pk_c"})),
                None,
            )
            .await
        }
    );
    // 둘 중 적어도 하나는 성공하고, 어느 쪽도 두 번 적립하지 않는다
    assert!(
        a.status == StatusCode::OK || b.status == StatusCode::OK,
        "{:?} {:?}",
        a.json,
        b.json
    );
    for x in [&a, &b] {
        assert!(
            x.status == StatusCode::OK || x.status == StatusCode::CONFLICT,
            "{:?}",
            x.json
        );
    }
    let s = summary(&r, bid).await;
    assert_eq!(s["total"], 7000);
    assert_eq!(s["count"], 1);
}

#[sqlx::test]
async fn tampered_amount_is_rejected_before_calling_toss(db: PgPool) {
    let (fake, toss) = fake_toss().await;
    let r = router(db, toss);
    let (bid, donor, oid) = setup(&r, 50_000).await;
    fake.pay("pk_t", &oid, 50_000);
    let res = call(
        &r,
        "POST",
        "/donations/confirm",
        confirm_body("pk_t", &oid, 1000),
        Some(&donor),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(fake.confirms(), 0);
    assert_eq!(summary(&r, bid).await["total"], 0);
    // 맞는 금액으로 다시 하면 된다(대기로 남아 있다)
    let ok = call(
        &r,
        "POST",
        "/donations/confirm",
        confirm_body("pk_t", &oid, 50_000),
        Some(&donor),
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK);
}

#[sqlx::test]
async fn payment_that_does_not_match_the_order_is_not_credited(db: PgPool) {
    let (fake, toss) = fake_toss().await;
    let r = router(db.clone(), toss);
    let (bid, _, oid) = setup(&r, 20_000).await;
    // 토스 쪽 결제 금액이 주문과 다르다(다른 결제를 이 주문에 붙이려는 시도)
    fake.pay("pk_m", &oid, 1000);
    fake.set_status("pk_m", "DONE");
    let n = call(
        &r,
        "POST",
        "/donations/notify",
        Some(json!({"paymentKey": "pk_m"})),
        None,
    )
    .await;
    assert_eq!(n.status, StatusCode::BAD_REQUEST);
    let status: String = sqlx::query_scalar("SELECT status FROM donations WHERE order_id = $1")
        .bind(&oid)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(status, "failed");
    assert_eq!(summary(&r, bid).await["total"], 0);
}

#[sqlx::test]
async fn unfinished_or_canceled_payments_are_not_credited(db: PgPool) {
    let (fake, toss) = fake_toss().await;
    let r = router(db.clone(), toss);
    let (bid, donor, oid) = setup(&r, 3000).await;
    fake.pay("pk_r", &oid, 3000); // READY: 아직 승인 전
    let n = call(
        &r,
        "POST",
        "/donations/notify",
        Some(json!({"paymentKey": "pk_r"})),
        None,
    )
    .await;
    assert_eq!(n.status, StatusCode::CONFLICT);
    let status = |db: PgPool, oid: String| async move {
        sqlx::query_scalar::<_, String>("SELECT status FROM donations WHERE order_id = $1")
            .bind(oid)
            .fetch_one(&db)
            .await
            .unwrap()
    };
    assert_eq!(status(db.clone(), oid.clone()).await, "pending");
    fake.set_status("pk_r", "CANCELED");
    assert_eq!(
        call(
            &r,
            "POST",
            "/donations/notify",
            Some(json!({"paymentKey": "pk_r"})),
            None
        )
        .await
        .status,
        StatusCode::CONFLICT
    );
    assert_eq!(status(db.clone(), oid.clone()).await, "failed");
    // 실패한 주문은 다시 확인해도 적립되지 않는다
    let again = call(
        &r,
        "POST",
        "/donations/confirm",
        confirm_body("pk_r", &oid, 3000),
        Some(&donor),
    )
    .await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    assert_eq!(summary(&r, bid).await["total"], 0);
}

#[sqlx::test]
async fn toss_rejection_marks_failed_and_unknown_payment_key_is_404(db: PgPool) {
    let (fake, toss) = fake_toss().await;
    let r = router(db.clone(), toss);
    let (_, donor, oid) = setup(&r, 3000).await;
    // 결제창을 거치지 않은 결제 키로 승인 → 토스가 거절
    let res = call(
        &r,
        "POST",
        "/donations/confirm",
        confirm_body("pk_none", &oid, 3000),
        Some(&donor),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(fake.confirms(), 1);
    let status: String = sqlx::query_scalar("SELECT status FROM donations WHERE order_id = $1")
        .bind(&oid)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(status, "failed");
    // 통지: 모르는 결제 키, 주소를 바꾸려는 결제 키
    for key in ["pk_none", "../confirm", "a/b"] {
        assert_eq!(
            call(
                &r,
                "POST",
                "/donations/notify",
                Some(json!({"paymentKey": key})),
                None
            )
            .await
            .status,
            StatusCode::NOT_FOUND,
            "{key}"
        );
    }
}

#[sqlx::test]
async fn who_can_order_and_confirm(db: PgPool) {
    let (fake, toss) = fake_toss().await;
    let r = router(db, toss);
    let owner = login(&r, 1).await;
    let donor = login(&r, 2).await;
    let bid = call(
        &r,
        "POST",
        "/broadcasts",
        Some(json!({"title": "방송"})),
        Some(&owner),
    )
    .await
    .json["id"]
        .as_i64()
        .unwrap();
    let path = format!("/broadcasts/{bid}/donations");
    // 비로그인 401, 자기 방송 403, 없는 방송 404
    assert_eq!(
        call(&r, "POST", &path, Some(json!({"amount": 1000})), None)
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &r,
            "POST",
            &path,
            Some(json!({"amount": 1000})),
            Some(&owner)
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &r,
            "POST",
            "/broadcasts/999/donations",
            Some(json!({"amount": 1000})),
            Some(&donor)
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    // 금액 규칙
    for bad in [0, 999, 1500, 1_001_000, -1000] {
        assert_eq!(
            call(
                &r,
                "POST",
                &path,
                Some(json!({"amount": bad})),
                Some(&donor)
            )
            .await
            .status,
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }
    assert_eq!(
        call(
            &r,
            "POST",
            &path,
            Some(json!({"amount": 1000, "message": "가".repeat(101)})),
            Some(&donor)
        )
        .await
        .status,
        StatusCode::BAD_REQUEST
    );
    // 남의 주문은 확인할 수 없다
    let oid = order(&r, bid, &donor, 2000).await;
    fake.pay("pk_w", &oid, 2000);
    assert_eq!(
        call(
            &r,
            "POST",
            "/donations/confirm",
            confirm_body("pk_w", &oid, 2000),
            Some(&owner)
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &r,
            "POST",
            "/donations/confirm",
            confirm_body("pk_w", &oid, 2000),
            None
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &r,
            "POST",
            "/donations/confirm",
            confirm_body("pk_w", "onair-없음", 2000),
            Some(&donor)
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(fake.confirms(), 0);
}

#[sqlx::test]
async fn totals_add_up_per_broadcast(db: PgPool) {
    let (fake, toss) = fake_toss().await;
    let r = router(db, toss);
    let (bid, donor, o1) = setup(&r, 1000).await;
    let o2 = order(&r, bid, &donor, 2000).await;
    let o3 = order(&r, bid, &donor, 4000).await; // 승인하지 않음
    for (k, o, a) in [("k1", &o1, 1000), ("k2", &o2, 2000)] {
        fake.pay(k, o, a);
        assert_eq!(
            call(
                &r,
                "POST",
                "/donations/confirm",
                confirm_body(k, o, a),
                Some(&donor)
            )
            .await
            .status,
            StatusCode::OK
        );
    }
    let _ = o3;
    let s = summary(&r, bid).await;
    assert_eq!(s["total"], 3000);
    assert_eq!(s["count"], 2);
    // 최신부터
    assert_eq!(s["items"][0]["amount"], 2000);
    assert_eq!(
        call(&r, "GET", "/broadcasts/999/donations", None, None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test]
async fn donations_are_off_without_toss_keys(db: PgPool) {
    let r = router(db, Toss::default());
    let owner = login(&r, 1).await;
    let donor = login(&r, 2).await;
    let bid = call(
        &r,
        "POST",
        "/broadcasts",
        Some(json!({"title": "방송"})),
        Some(&owner),
    )
    .await
    .json["id"]
        .as_i64()
        .unwrap();
    assert_eq!(
        call(
            &r,
            "POST",
            &format!("/broadcasts/{bid}/donations"),
            Some(json!({"amount": 1000})),
            Some(&donor)
        )
        .await
        .status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        call(
            &r,
            "POST",
            "/donations/notify",
            Some(json!({"paymentKey": "x"})),
            None
        )
        .await
        .status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    // 내역은 볼 수 있다(빈 목록)
    assert_eq!(
        call(
            &r,
            "GET",
            &format!("/broadcasts/{bid}/donations"),
            None,
            None
        )
        .await
        .json["total"],
        0
    );
}
