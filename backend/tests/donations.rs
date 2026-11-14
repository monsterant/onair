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
    /// Some(코드)면 승인이 그 상태 코드로 실패한다(429 등)
    confirm_error: Arc<Mutex<Option<u16>>>,
    /// true면 승인된 결제에 주문번호 확인 없이 ALREADY_PROCESSED를 돌려준다
    already_first: Arc<Mutex<bool>>,
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
                    // 승인된 결제면 주문번호를 보기 전에 "이미 처리됨"을 돌려주는 경우를 흉내 낸다
                    if *f.already_first.lock().unwrap() && p.status == "DONE" {
                        return err(StatusCode::BAD_REQUEST, "ALREADY_PROCESSED_PAYMENT");
                    }
                    // 진짜 토스처럼 결제창에서 요청한 주문번호·금액과 다르면 거절한다
                    if b["orderId"].as_str() != Some(p.order_id.as_str())
                        || b["amount"].as_i64() != Some(p.amount)
                    {
                        return err(StatusCode::BAD_REQUEST, "INVALID_REQUEST");
                    }
                    if p.status == "DONE" {
                        return err(StatusCode::BAD_REQUEST, "ALREADY_PROCESSED_PAYMENT");
                    }
                    // 진짜 토스처럼 취소·중단된 결제는 승인하지 않는다
                    if p.status != "READY" {
                        return err(StatusCode::BAD_REQUEST, "NOT_AVAILABLE_PAYMENT");
                    }
                    if let Some(code) = *f.confirm_error.lock().unwrap() {
                        return err(StatusCode::from_u16(code).unwrap(), "TOO_MANY_REQUESTS");
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
    router_with_chat(db, toss, Default::default())
}

fn router_with_chat(db: PgPool, toss: Toss, chat: onair_backend::chat::Hub) -> Router {
    app(AppState {
        chat,
        rec: Default::default(),
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
        json!({"total": 0, "count": 0, "top": [], "items": []})
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
async fn chat_announces_a_donation_once(db: PgPool) {
    // #86: 적립된 후원은 그 방송 채팅에 한 번만 안내한다(같은 승인·통지가 여러 번 와도)
    let (fake, toss) = fake_toss().await;
    let hub = onair_backend::chat::Hub::default();
    let r = router_with_chat(db, toss, hub.clone());
    let (bid, donor, oid) = setup(&r, 5000).await;
    // 주문만 만들고 결제 전에는 안내가 없다
    assert!(hub.subscribe(bid).0.is_empty());
    fake.pay("pk_1", &oid, 5000);
    for _ in 0..3 {
        let c = call(
            &r,
            "POST",
            "/donations/confirm",
            confirm_body("pk_1", &oid, 5000),
            Some(&donor),
        )
        .await;
        assert_eq!(c.status, StatusCode::OK);
    }
    let n = call(
        &r,
        "POST",
        "/donations/notify",
        Some(json!({"paymentKey": "pk_1"})),
        None,
    )
    .await;
    assert_eq!(n.status, StatusCode::OK, "{:?}", n.json);
    let (recent, _) = hub.subscribe(bid);
    assert_eq!(recent.len(), 1, "{recent:?}");
    assert_eq!(recent[0].kind, "donation");
    assert_eq!(recent[0].username, "demo2");
    assert_eq!(recent[0].text, "5,000원을 후원했습니다 — 응원해요");
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
    // 취소된 결제로 다시 확인해도 토스가 거절하므로 적립되지 않는다
    let again = call(
        &r,
        "POST",
        "/donations/confirm",
        confirm_body("pk_r", &oid, 3000),
        Some(&donor),
    )
    .await;
    assert_eq!(again.status, StatusCode::BAD_REQUEST);
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

/// #99: 후원 순위. 사람마다 합계, 큰 순서, 동점은 먼저 다다른 사람, 5명까지, 적립된 것만, 탈퇴 사용자 제외
#[sqlx::test]
async fn top_donors_rank_paid_totals_per_person(db: PgPool) {
    let (_fake, toss) = fake_toss().await;
    let r = router(db.clone(), toss);
    let owner = login(&r, 1).await;
    let b = call(
        &r,
        "POST",
        "/broadcasts",
        Some(json!({"title": "순위 시험"})),
        Some(&owner),
    )
    .await;
    let bid = b.json["id"].as_i64().unwrap();
    let other = call(
        &r,
        "POST",
        "/broadcasts",
        Some(json!({"title": "다른 방송"})),
        Some(&owner),
    )
    .await
    .json["id"]
        .as_i64()
        .unwrap();
    // 후원자 7명을 만든다
    let mut users = Vec::new();
    for i in 0..7 {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO users (username, password_hash) VALUES ($1, 'x') RETURNING id",
        )
        .bind(format!("fan{i}"))
        .fetch_one(&db)
        .await
        .unwrap();
        users.push(id);
    }
    let mut n = 0;
    let mut give = |donor: Option<i64>, bid: i64, amount: i64, status: &'static str, secs: i64| {
        n += 1;
        let db = db.clone();
        async move {
            sqlx::query(
                "INSERT INTO donations (order_id, broadcast_id, donor_id, amount, status, payment_key, paid_at)
                 VALUES ($1, $2, $3, $4, $5,
                         CASE WHEN $5 = 'paid' THEN $1 END,
                         CASE WHEN $5 = 'paid' THEN now() - make_interval(secs => $6) END)",
            )
            .bind(format!("o{n}"))
            .bind(bid)
            .bind(donor)
            .bind(amount)
            .bind(status)
            .bind(secs as f64)
            .execute(&db)
            .await
            .unwrap();
        }
    };
    // fan0: 1000 + 4000 = 5000 (두 번 나눠서)
    give(Some(users[0]), bid, 1000, "paid", 100).await;
    give(Some(users[0]), bid, 4000, "paid", 90).await;
    // fan1: 1000(오래 전) + 2000(최근) = 3000, fan2: 3000 한 번(50초 전).
    // 3000에 먼저 다다른 사람은 fan2다. fan1의 첫 후원이 더 오래 전이어도 "마지막 후원 시각"으로 정한다(#100).
    give(Some(users[1]), bid, 1000, "paid", 200).await;
    give(Some(users[1]), bid, 2000, "paid", 10).await;
    give(Some(users[2]), bid, 3000, "paid", 50).await;
    // fan3, fan4, fan5: 1000씩(fan5가 가장 늦음 → 6번째라 빠진다)
    give(Some(users[3]), bid, 1000, "paid", 40).await;
    give(Some(users[4]), bid, 1000, "paid", 30).await;
    give(Some(users[5]), bid, 1000, "paid", 5).await;
    // 적립 안 된 것, 다른 방송, 탈퇴 사용자는 순위에 안 들어간다
    give(Some(users[6]), bid, 1_000_000, "pending", 0).await;
    give(Some(users[6]), bid, 1_000_000, "failed", 0).await;
    give(Some(users[6]), bid, 1_000_000, "canceled", 0).await;
    give(Some(users[6]), other, 1_000_000, "paid", 0).await;
    give(None, bid, 9000, "paid", 0).await;
    let s = summary(&r, bid).await;
    let top: Vec<(String, i64, i64)> = s["top"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["donor_username"].as_str().unwrap().to_string(),
                t["total"].as_i64().unwrap(),
                t["count"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        top,
        [
            ("fan0".to_string(), 5000, 2),
            ("fan2".to_string(), 3000, 1),
            ("fan1".to_string(), 3000, 2),
            ("fan3".to_string(), 1000, 1),
            ("fan4".to_string(), 1000, 1),
        ]
    );
    // 탈퇴 사용자의 후원도 합계에는 남는다
    assert_eq!(s["total"], 23000);
    // 다른 방송의 순위는 따로 센다(fan6만)
    assert_eq!(summary(&r, other).await["top"].as_array().unwrap().len(), 1);
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

#[sqlx::test]
async fn approved_payment_is_credited_even_after_the_order_was_marked_failed(db: PgPool) {
    // 결제창을 두 번 열어 첫 결제(pk_a)는 중단, 두 번째(pk_b)는 승인된 경우.
    // 중단 통지가 먼저 와서 주문이 failed가 됐어도, 승인된 결제는 적립돼야 한다(토스가 돈을 받았다).
    let (fake, toss) = fake_toss().await;
    let r = router(db, toss);
    let (bid, donor, oid) = setup(&r, 4000).await;
    fake.pay("pk_a", &oid, 4000);
    fake.set_status("pk_a", "ABORTED");
    assert_eq!(
        call(
            &r,
            "POST",
            "/donations/notify",
            Some(json!({"paymentKey": "pk_a"})),
            None
        )
        .await
        .status,
        StatusCode::CONFLICT
    );
    fake.pay("pk_b", &oid, 4000);
    let ok = call(
        &r,
        "POST",
        "/donations/confirm",
        confirm_body("pk_b", &oid, 4000),
        Some(&donor),
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK, "{:?}", ok.json);
    assert_eq!(ok.json["status"], "paid");
    assert_eq!(summary(&r, bid).await["total"], 4000);
    // 중단된 결제의 통지가 또 와도 적립된 주문은 그대로다
    call(
        &r,
        "POST",
        "/donations/notify",
        Some(json!({"paymentKey": "pk_a"})),
        None,
    )
    .await;
    assert_eq!(summary(&r, bid).await["count"], 1);
}

#[sqlx::test]
async fn toss_throttling_keeps_the_order_pending(db: PgPool) {
    let (fake, toss) = fake_toss().await;
    let r = router(db.clone(), toss);
    let (bid, donor, oid) = setup(&r, 2000).await;
    fake.pay("pk_429", &oid, 2000);
    *fake.confirm_error.lock().unwrap() = Some(429);
    let busy = call(
        &r,
        "POST",
        "/donations/confirm",
        confirm_body("pk_429", &oid, 2000),
        Some(&donor),
    )
    .await;
    assert_eq!(busy.status, StatusCode::BAD_GATEWAY);
    let status: String = sqlx::query_scalar("SELECT status FROM donations WHERE order_id = $1")
        .bind(&oid)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(status, "pending");
    *fake.confirm_error.lock().unwrap() = None;
    assert_eq!(
        call(
            &r,
            "POST",
            "/donations/confirm",
            confirm_body("pk_429", &oid, 2000),
            Some(&donor)
        )
        .await
        .status,
        StatusCode::OK
    );
    assert_eq!(summary(&r, bid).await["total"], 2000);
}

#[sqlx::test]
async fn donations_stay_when_the_donor_account_is_removed(db: PgPool) {
    let (fake, toss) = fake_toss().await;
    let r = router(db.clone(), toss);
    let (bid, donor, oid) = setup(&r, 6000).await;
    fake.pay("pk_d", &oid, 6000);
    assert_eq!(
        call(
            &r,
            "POST",
            "/donations/confirm",
            confirm_body("pk_d", &oid, 6000),
            Some(&donor)
        )
        .await
        .status,
        StatusCode::OK
    );
    sqlx::query("DELETE FROM users WHERE username = 'demo2'")
        .execute(&db)
        .await
        .unwrap();
    let s = summary(&r, bid).await;
    assert_eq!(s["total"], 6000);
    assert_eq!(s["items"][0]["donor_username"], "(탈퇴한 사용자)");
}

#[sqlx::test]
async fn canceled_after_credit_is_removed_from_total(db: PgPool) {
    let (fake, toss) = fake_toss().await;
    let r = router(db.clone(), toss);
    let (bid, donor, oid) = setup(&r, 8000).await;
    fake.pay("pk_x", &oid, 8000);
    assert_eq!(
        call(
            &r,
            "POST",
            "/donations/confirm",
            confirm_body("pk_x", &oid, 8000),
            Some(&donor)
        )
        .await
        .status,
        StatusCode::OK
    );
    assert_eq!(summary(&r, bid).await["total"], 8000);
    fake.set_status("pk_x", "CANCELED");
    let n = call(
        &r,
        "POST",
        "/donations/notify",
        Some(json!({"paymentKey": "pk_x"})),
        None,
    )
    .await;
    assert_eq!(n.status, StatusCode::OK);
    assert_eq!(n.json["status"], "canceled");
    assert_eq!(summary(&r, bid).await["total"], 0);
    // 다시 와도 그대로
    call(
        &r,
        "POST",
        "/donations/notify",
        Some(json!({"paymentKey": "pk_x"})),
        None,
    )
    .await;
    assert_eq!(summary(&r, bid).await["count"], 0);
}

#[sqlx::test]
async fn db_rejects_amounts_and_messages_outside_the_rules(db: PgPool) {
    let (_, toss) = fake_toss().await;
    let r = router(db.clone(), toss);
    let (bid, _, _) = setup(&r, 1000).await;
    let uid: i64 = sqlx::query_scalar("SELECT id FROM users WHERE username = 'demo2'")
        .fetch_one(&db)
        .await
        .unwrap();
    for (amount, msg) in [(1500_i64, String::new()), (1000, "가".repeat(101))] {
        let res = sqlx::query("INSERT INTO donations (order_id, broadcast_id, donor_id, amount, message) VALUES ($4, $1, $2, $3, $5)")
            .bind(bid).bind(uid).bind(amount).bind(format!("onair-db-{amount}-{}", msg.len())).bind(msg).execute(&db).await;
        assert!(res.is_err(), "{amount}");
    }
}

#[sqlx::test]
async fn retry_after_lost_response_uses_already_processed_and_credits_once(db: PgPool) {
    // 승인은 토스에서 끝났는데 응답을 못 받아 화면이 다시 확인을 보낸 경우
    let (fake, toss) = fake_toss().await;
    let r = router(db, toss);
    let (bid, donor, oid) = setup(&r, 3000).await;
    fake.pay("pk_lost", &oid, 3000);
    fake.set_status("pk_lost", "DONE");
    let ok = call(
        &r,
        "POST",
        "/donations/confirm",
        confirm_body("pk_lost", &oid, 3000),
        Some(&donor),
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK, "{:?}", ok.json);
    assert_eq!(ok.json["status"], "paid");
    assert_eq!(fake.get_calls.load(Ordering::SeqCst), 1);
    assert_eq!(summary(&r, bid).await["total"], 3000);
}

#[sqlx::test]
async fn already_processed_payment_of_another_order_is_not_credited(db: PgPool) {
    let (fake, toss) = fake_toss().await;
    let r = router(db.clone(), toss);
    let (bid, donor, x) = setup(&r, 5000).await;
    let y = order(&r, bid, &donor, 5000).await;
    fake.pay("pk_x", &x, 5000);
    fake.set_status("pk_x", "DONE");
    *fake.already_first.lock().unwrap() = true;
    // 주문 X의 결제 키로 주문 Y를 확인하려 한다
    let res = call(
        &r,
        "POST",
        "/donations/confirm",
        confirm_body("pk_x", &y, 5000),
        Some(&donor),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    let y_status: String = sqlx::query_scalar("SELECT status FROM donations WHERE order_id = $1")
        .bind(&y)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_ne!(y_status, "paid");
    // X는 자기 결제로 적립된다
    assert_eq!(
        call(
            &r,
            "POST",
            "/donations/confirm",
            confirm_body("pk_x", &x, 5000),
            Some(&donor)
        )
        .await
        .status,
        StatusCode::OK
    );
    assert_eq!(summary(&r, bid).await["total"], 5000);
    assert_eq!(summary(&r, bid).await["count"], 1);
}

#[sqlx::test]
async fn second_payment_for_the_same_order_is_not_credited_again(db: PgPool) {
    let (fake, toss) = fake_toss().await;
    let r = router(db, toss);
    let (bid, donor, oid) = setup(&r, 2000).await;
    fake.pay("pk_1", &oid, 2000);
    assert_eq!(
        call(
            &r,
            "POST",
            "/donations/confirm",
            confirm_body("pk_1", &oid, 2000),
            Some(&donor)
        )
        .await
        .status,
        StatusCode::OK
    );
    // 같은 주문으로 결제가 한 번 더 승인됐다
    fake.pay("pk_2", &oid, 2000);
    fake.set_status("pk_2", "DONE");
    let n = call(
        &r,
        "POST",
        "/donations/notify",
        Some(json!({"paymentKey": "pk_2"})),
        None,
    )
    .await;
    assert_eq!(n.status, StatusCode::CONFLICT);
    let s = summary(&r, bid).await;
    assert_eq!(s["total"], 2000);
    assert_eq!(s["count"], 1);
}

/// #127: 막힌 사람은 후원 주문도 403, 후원 메시지에 금칙어가 있으면 400.
/// 주문 뒤 금칙어가 생기면 채팅 안내에서 메시지만 빠지고, 끝난 방송에는 안내가 없다.
#[sqlx::test]
async fn banned_users_and_banned_words_cannot_use_donations(db: PgPool) {
    let (fake, toss) = fake_toss().await;
    let hub = onair_backend::chat::Hub::default();
    let r = router_with_chat(db, toss, hub.clone());
    let owner = login(&r, 1).await;
    let donor = login(&r, 2).await;
    let b = call(
        &r,
        "POST",
        "/broadcasts",
        Some(json!({"title": "후원 제한"})),
        Some(&owner),
    )
    .await;
    let bid = b.json["id"].as_i64().unwrap();
    let me = call(&r, "GET", "/auth/me", None, Some(&donor)).await;
    let donor_id = me.json["id"].as_i64().unwrap();
    let words = format!("/broadcasts/{bid}/banned-words");
    assert_eq!(
        call(
            &r,
            "PUT",
            &words,
            Some(json!({"words": ["광고", "Spam"]})),
            Some(&owner)
        )
        .await
        .status,
        StatusCode::OK
    );
    let order_with = |msg: &'static str| {
        let r = r.clone();
        let donor = donor.clone();
        async move {
            call(
                &r,
                "POST",
                &format!("/broadcasts/{bid}/donations"),
                Some(json!({"amount": 1000, "message": msg})),
                Some(&donor),
            )
            .await
        }
    };
    // 금칙어가 든 메시지는 400(영문은 대소문자 무시). 없는 메시지는 된다.
    let o = order_with("무료 광고합니다").await;
    assert_eq!(o.status, StatusCode::BAD_REQUEST);
    assert!(o.json["error"].as_str().unwrap().contains("막은 단어"));
    assert_eq!(
        order_with("no SPAM here").await.status,
        StatusCode::BAD_REQUEST
    );
    // 막히기 전에 만들어 둔 주문(메시지 있음)
    let early = order_with("미리 쌓은 메시지").await;
    assert_eq!(early.status, StatusCode::CREATED);
    let early_oid = early.json["order_id"].as_str().unwrap().to_string();
    let ok = order_with("응원해요").await;
    assert_eq!(ok.status, StatusCode::CREATED);
    let oid = ok.json["order_id"].as_str().unwrap().to_string();
    // 주문 뒤 "응원"이 금칙어가 되면, 결제가 끝나도 채팅 안내에서 메시지만 빠진다
    assert_eq!(
        call(
            &r,
            "PUT",
            &words,
            Some(json!({"words": ["응원"]})),
            Some(&owner)
        )
        .await
        .status,
        StatusCode::OK
    );
    fake.pay("pk_127", &oid, 1000);
    let c = call(
        &r,
        "POST",
        "/donations/confirm",
        confirm_body("pk_127", &oid, 1000),
        Some(&donor),
    )
    .await;
    assert_eq!(c.status, StatusCode::OK, "{:?}", c.json);
    let (recent, _) = hub.subscribe(bid);
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].text, "1,000원을 후원했습니다");
    // 막힌 사람은 후원 주문도 403
    assert_eq!(
        call(
            &r,
            "PUT",
            &format!("/broadcasts/{bid}/bans/{donor_id}"),
            Some(json!({"ten_minutes": false})),
            Some(&owner)
        )
        .await
        .status,
        StatusCode::NO_CONTENT
    );
    let o = order_with("").await;
    assert_eq!(o.status, StatusCode::FORBIDDEN);
    assert!(o.json["error"].as_str().unwrap().contains("막았습니다"));
    // 막히기 전 주문을 막힌 뒤 결제하면 적립은 되지만 채팅 안내에는 메시지가 없다(#127 리뷰)
    fake.pay("pk_127e", &early_oid, 1000);
    let c = call(
        &r,
        "POST",
        "/donations/confirm",
        confirm_body("pk_127e", &early_oid, 1000),
        Some(&donor),
    )
    .await;
    assert_eq!(c.status, StatusCode::OK, "{:?}", c.json);
    let (recent, _) = hub.subscribe(bid);
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[1].text, "1,000원을 후원했습니다");
    // 풀면 다시 된다. 방송을 끝낸 뒤 결제가 끝나면 적립은 되지만 채팅 안내는 없다.
    assert_eq!(
        call(
            &r,
            "DELETE",
            &format!("/broadcasts/{bid}/bans/{donor_id}"),
            None,
            Some(&owner)
        )
        .await
        .status,
        StatusCode::NO_CONTENT
    );
    let o2 = order_with("").await;
    assert_eq!(o2.status, StatusCode::CREATED);
    let oid2 = o2.json["order_id"].as_str().unwrap().to_string();
    assert_eq!(
        call(
            &r,
            "POST",
            &format!("/broadcasts/{bid}/end"),
            None,
            Some(&owner)
        )
        .await
        .status,
        StatusCode::OK
    );
    fake.pay("pk_127b", &oid2, 1000);
    let c = call(
        &r,
        "POST",
        "/donations/confirm",
        confirm_body("pk_127b", &oid2, 1000),
        Some(&donor),
    )
    .await;
    assert_eq!(c.status, StatusCode::OK, "{:?}", c.json);
    assert_eq!(summary(&r, bid).await["total"], 3000);
    assert_eq!(hub.subscribe(bid).0.len(), 2);
}
