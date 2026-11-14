//! 도네이션 (#41, 공모 선택 요구사항 2). 토스페이먼츠 테스트 모드.
//!
//! 흐름
//! 1. 후원자가 금액·메시지를 고르면 서버가 대기(pending) 주문을 만든다: `POST /broadcasts/{id}/donations`
//! 2. 화면이 토스 결제위젯을 연다. 결제가 끝나면 토스가 성공 주소로 paymentKey·orderId·amount를 붙여 돌려보낸다.
//! 3. 화면이 그 값을 서버에 넘긴다: `POST /donations/confirm`
//!    - 서버는 금액이 대기 주문과 같은지 먼저 보고(바꿔치기 방지), 토스 승인 API를 부른다.
//!    - 토스 응답의 주문번호·금액·상태(DONE)가 대기 주문과 맞을 때만 적립(paid)한다.
//! 4. 승인 통지: `POST /donations/notify {paymentKey}`
//!    - 본문은 믿지 않는다. 토스에서 결제를 다시 조회해서 3과 같은 방식으로 적립한다.
//!    - 가맹점 계정이 없어 토스 웹훅을 등록할 수 없어서, 웹훅이 할 일을 이 길이 대신한다.
//!
//! 한 번만 적립하기(같은 승인·통지가 여러 번, 동시에 와도)
//! - 적립은 `UPDATE ... SET status='paid' WHERE id=$1 AND status='pending'` 한 문장이다.
//!   DB가 행을 잠그므로 동시에 두 요청이 와도 하나만 1행을 바꾸고, 다른 하나는 0행이 된다.
//! - 0행이면 다시 읽어서, 이미 같은 결제 키로 paid면 성공으로 돌려준다(멱등: 몇 번 불러도 결과가 같다).
//! - 결제 키는 고유(UNIQUE)라 같은 결제가 다른 주문에 적립될 수 없다.
//! - 토스 호출은 트랜잭션 밖에서 한다. 네트워크를 기다리는 동안 DB 잠금을 붙잡지 않으려는 것이다.

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{
    AppState,
    auth::{ApiError, CurrentUser},
    guard::client_key,
    toss::{Payment, TossError},
};

pub const AMOUNT_MIN: i64 = 1_000;
pub const AMOUNT_MAX: i64 = 1_000_000;
pub const AMOUNT_STEP: i64 = 1_000;
pub const MESSAGE_MAX: usize = 100;

/// 후원 내역 한 줄(누구나 본다)
#[derive(Serialize, Debug, sqlx::FromRow)]
pub struct DonationItem {
    pub id: i64,
    pub donor_username: String,
    pub amount: i64,
    pub message: String,
    #[serde(with = "time::serde::rfc3339::option")]
    pub paid_at: Option<OffsetDateTime>,
}

/// 후원 순위 한 줄(#99). 한 사람이 이 방송에 적립한 합계.
#[derive(Serialize, Debug, sqlx::FromRow)]
pub struct TopDonor {
    pub donor_username: String,
    pub total: i64,
    pub count: i64,
}

/// 후원 순위에 보여 줄 사람 수(#99)
pub const TOP_DONORS: i64 = 5;

#[derive(Serialize, Debug)]
pub struct DonationList {
    /// 적립된 후원의 합계(원)
    pub total: i64,
    pub count: i64,
    /// 후원 순위(#99). 합계가 큰 사람부터 5명
    pub top: Vec<TopDonor>,
    /// 최근 100개, 최신부터
    pub items: Vec<DonationItem>,
}

/// 주문(후원자 본인에게 보여 주는 상태)
#[derive(Serialize, Debug, sqlx::FromRow)]
pub struct Donation {
    pub id: i64,
    pub order_id: String,
    /// 방송이 지워지면 None(#128). 후원 기록은 남긴다.
    pub broadcast_id: Option<i64>,
    /// 후원자가 탈퇴(테스트 계정 정리 등)하면 None. 후원 기록과 합계는 남긴다.
    pub donor_id: Option<i64>,
    pub amount: i64,
    pub message: String,
    pub status: String,
    #[serde(skip)]
    pub payment_key: Option<String>,
}

const SELECT_DONATION: &str =
    "SELECT id, order_id, broadcast_id, donor_id, amount, message, status, payment_key
  FROM donations WHERE order_id = $1";

// ---------- 입력 검사 ----------

pub fn check_amount(amount: i64) -> Result<(), ApiError> {
    if !(AMOUNT_MIN..=AMOUNT_MAX).contains(&amount) || amount % AMOUNT_STEP != 0 {
        return Err(ApiError::BadRequest(
            "후원 금액은 1,000원부터 1,000,000원까지 1,000원 단위입니다",
        ));
    }
    Ok(())
}

fn clean_message(raw: &str) -> Result<String, ApiError> {
    let m = raw.replace("\r\n", "\n").replace('\r', "\n");
    let m = m.trim();
    if m.chars()
        .any(|c| (c.is_control() && c != '\n' && c != '\t') || crate::comments::bidi_control(c))
    {
        return Err(ApiError::BadRequest("메시지에 쓸 수 없는 글자가 있습니다"));
    }
    if m.chars().count() > MESSAGE_MAX {
        return Err(ApiError::BadRequest("메시지는 100자 이하여야 합니다"));
    }
    // 빈칸처럼 보이는 글자만 있으면 메시지가 없는 것으로 본다
    if m.chars().all(crate::comments::blank_like) {
        return Ok(String::new());
    }
    Ok(m.to_string())
}

// ---------- 1. 주문 만들기 ----------

#[derive(Deserialize)]
pub struct NewDonation {
    pub amount: i64,
    #[serde(default)]
    pub message: String,
}

/// 결제창에 넘길 값
#[derive(Serialize, Debug)]
pub struct Checkout {
    pub order_id: String,
    pub order_name: String,
    pub amount: i64,
    /// 결제위젯 클라이언트 키(공개 값)
    pub client_key: String,
    /// 결제위젯이 구매자를 구분하는 값(우리 사용자 id로 만든다)
    pub customer_key: String,
}

pub async fn create(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(broadcast_id): Path<i64>,
    Json(input): Json<NewDonation>,
) -> Result<(StatusCode, Json<Checkout>), ApiError> {
    let Some(client_key) = state.toss.client_key() else {
        return Err(ApiError::Unavailable("결제가 설정되지 않았습니다"));
    };
    check_amount(input.amount)?;
    let message = clean_message(&input.message)?;
    let row: Option<(i64, String, Vec<String>)> =
        sqlx::query_as("SELECT owner_id, title, banned_words FROM broadcasts WHERE id = $1")
            .bind(broadcast_id)
            .fetch_optional(&state.db)
            .await?;
    let (owner_id, title, words) = row.ok_or(ApiError::NotFound)?;
    if owner_id == user.id {
        return Err(ApiError::Forbidden("자기 방송에는 후원할 수 없습니다"));
    }
    // 방송 주인이 막은 사람은 후원도 못 한다(#127). 테스트 결제는 공짜라 후원 메시지가 채팅 제한을 피하는 길이 된다.
    if crate::moderation::banned(&state.db, broadcast_id, user.id).await? {
        return Err(crate::moderation::BANNED);
    }
    // 후원 메시지에도 금칙어를 적용한다(#127). 결제창을 열기 전에 거절한다.
    if crate::moderation::has_banned_word(&message, &words) {
        return Err(ApiError::BadRequest(
            "방송 주인이 막은 단어가 들어 있습니다",
        ));
    }
    // 대기 주문을 쌓아 DB를 채우는 것을 막는다(사용자 하나당 1분 20번, 댓글과 같은 몫)
    if !state
        .auth
        .comment_limiter
        .allow(&format!("donate:{}", user.id))
    {
        return Err(ApiError::TooManyRequests);
    }
    // 토스 주문번호 규칙: 6~64자, 영문·숫자·-·_. 문서용 테스트 상점은 여러 사람이 같이 쓰므로 겹치지 않게 임의 값으로.
    let order_id = format!("onair-{}", crate::broadcasts::random_hex(16)?);
    let inserted = sqlx::query(
        "INSERT INTO donations (order_id, broadcast_id, donor_id, amount, message)
         SELECT $1, b.id, $3, $4, $5 FROM broadcasts b WHERE b.id = $2",
    )
    .bind(&order_id)
    .bind(broadcast_id)
    .bind(user.id)
    .bind(input.amount)
    .bind(&message)
    .execute(&state.db)
    .await?;
    // 확인과 저장 사이에 방송이 지워졌으면 주문이 생기지 않았다. 결제창을 열지 않게 404(#41 리뷰)
    if inserted.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    let name: String = title.chars().take(40).collect();
    Ok((
        StatusCode::CREATED,
        Json(Checkout {
            order_id,
            order_name: format!("onair 후원: {name}"),
            amount: input.amount,
            client_key: client_key.to_string(),
            // 토스 권고: 구매자 키는 짐작할 수 없는 값으로(사용자 번호를 그대로 쓰지 않는다, #41 리뷰)
            customer_key: format!("onair-c-{}", crate::broadcasts::random_hex(12)?),
        }),
    ))
}

// ---------- 2. 승인 확인과 적립 ----------

async fn find(state: &AppState, order_id: &str) -> Result<Option<Donation>, ApiError> {
    Ok(sqlx::query_as::<_, Donation>(SELECT_DONATION)
        .bind(order_id)
        .fetch_optional(&state.db)
        .await?)
}

async fn mark_failed(state: &AppState, id: i64) -> Result<(), ApiError> {
    sqlx::query("UPDATE donations SET status = 'failed' WHERE id = $1 AND status = 'pending'")
        .bind(id)
        .execute(&state.db)
        .await?;
    Ok(())
}

/// 토스가 확인해 준 결제를 대기 주문과 맞춰 보고 한 번만 적립한다.
/// 주문번호·금액이 맞고 DONE인 결제는 주문이 failed여도 적립한다. 토스가 이미 돈을 받았기 때문이다.
/// (예: 결제창을 두 번 열어 첫 결제는 취소, 두 번째는 승인된 경우. 취소 통지가 먼저 와서 failed가 됐어도 승인된 결제를 잃지 않게, #41 리뷰)
async fn settle(state: &AppState, d: &Donation, p: &Payment) -> Result<Donation, ApiError> {
    if p.order_id != d.order_id || p.total_amount != d.amount {
        tracing::warn!(order_id = %d.order_id, "토스 결제와 주문이 맞지 않음");
        mark_failed(state, d.id).await?;
        return Err(ApiError::BadRequest("결제 정보가 주문과 맞지 않습니다"));
    }
    // 적립한 결제가 나중에 전액 취소(환불)됐으면 합계에서 뺀다(같은 결제 키일 때만, #41 리뷰).
    // 부분 취소(PARTIAL_CANCELED)는 전액을 빼면 틀리므로 그대로 둔다. 부분 환불은 이 서비스에서 일어나지 않는다.
    if p.status == "CANCELED"
        && d.status == "paid"
        && d.payment_key.as_deref() == Some(p.payment_key.as_str())
    {
        sqlx::query(
            "UPDATE donations SET status = 'canceled'
              WHERE id = $1 AND status = 'paid' AND payment_key = $2",
        )
        .bind(d.id)
        .bind(&p.payment_key)
        .execute(&state.db)
        .await?;
        return find(state, &d.order_id).await?.ok_or(ApiError::NotFound);
    }
    if p.status != "DONE" {
        // 아직 승인 전(READY 등)이면 대기로 두고, 취소·만료면 실패로 둔다
        if matches!(
            p.status.as_str(),
            "CANCELED" | "PARTIAL_CANCELED" | "ABORTED" | "EXPIRED"
        ) {
            mark_failed(state, d.id).await?;
        }
        return Err(ApiError::Conflict("결제가 완료되지 않았습니다"));
    }
    let updated = sqlx::query(
        "UPDATE donations SET status = 'paid', payment_key = $1, paid_at = now()
          WHERE id = $2 AND status IN ('pending', 'failed')",
    )
    .bind(&p.payment_key)
    .bind(d.id)
    .execute(&state.db)
    .await;
    match updated {
        // 이 요청이 실제로 적립했을 때(1줄이 바뀜)만 채팅에 후원 안내를 보낸다(#86).
        // 같은 승인·통지가 여러 번 와도 두 번째부터는 0줄이라 안내는 한 번이다.
        Ok(r) if r.rows_affected() == 1 => announce(state, d).await,
        Ok(_) => {}
        // 같은 결제 키가 이미 다른 주문에 적립돼 있다(있어서는 안 되는 일). 이 주문은 적립하지 않는다.
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            return Err(ApiError::Conflict("이미 다른 후원에 쓰인 결제입니다"));
        }
        Err(e) => return Err(e.into()),
    }
    // 내가 바꿨든 다른 요청이 먼저 바꿨든, 지금 상태를 다시 읽어서 판단한다
    let now = find(state, &d.order_id).await?.ok_or(ApiError::NotFound)?;
    match now.status.as_str() {
        "paid" if now.payment_key.as_deref() == Some(p.payment_key.as_str()) => Ok(now),
        "paid" => Err(ApiError::Conflict("다른 결제로 이미 처리된 후원입니다")),
        _ => Err(ApiError::Conflict("처리할 수 없는 후원 상태입니다")),
    }
}

/// 후원 안내를 그 방송 채팅에 보낸다(#86). 실패해도 적립에는 영향이 없다.
async fn announce(state: &AppState, d: &Donation) {
    // 방송이 지워진 뒤 결제가 끝났으면 안내할 채팅방이 없다(#128)
    let Some(broadcast_id) = d.broadcast_id else {
        return;
    };
    let donor: Option<String> = match d.donor_id {
        Some(id) => sqlx::query_scalar("SELECT username FROM users WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten(),
        None => None,
    };
    let name = donor.unwrap_or_else(|| "알 수 없는 사용자".into());
    // 끝난 방송에는 채팅 안내를 올리지 않는다(#127, 채팅도 끝난 방송에는 쓸 수 없다).
    // 주문 뒤 결제 사이에 금칙어가 바뀌었을 수 있어 지금 목록으로 한 번 더 본다. 들어 있으면 메시지만 뺀다.
    let row: Option<(String, Vec<String>)> =
        match sqlx::query_as("SELECT status, banned_words FROM broadcasts WHERE id = $1")
            .bind(broadcast_id)
            .fetch_optional(&state.db)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("후원 안내용 방송 조회 실패, 안내를 건너뜀: {e}");
                return;
            }
        };
    let Some((status, words)) = row else {
        return;
    };
    if status == "ended" {
        return;
    }
    // 막히기 전에 만든 주문을 막힌 뒤 결제해도 메시지는 채팅에 내보내지 않는다(#127 리뷰). 금액 안내만 남긴다.
    let donor_banned = match d.donor_id {
        Some(id) => crate::moderation::banned(&state.db, broadcast_id, id)
            .await
            .unwrap_or(true),
        None => false,
    };
    let mut text = format!("{}원을 후원했습니다", won(d.amount));
    if !d.message.is_empty()
        && !donor_banned
        && !crate::moderation::has_banned_word(&d.message, &words)
    {
        text.push_str(&format!(" — {}", d.message));
    }
    state
        .chat
        .publish(broadcast_id, "donation", None, &name, &text);
}

/// 1234567 → "1,234,567"
fn won(n: i64) -> String {
    let s = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmInput {
    pub payment_key: String,
    pub order_id: String,
    pub amount: i64,
}

/// 결제창에서 돌아온 값으로 승인한다. 후원자 본인만 부를 수 있다. 여러 번 불러도 한 번만 적립된다.
pub async fn confirm(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(input): Json<ConfirmInput>,
) -> Result<Json<Donation>, ApiError> {
    if !state.toss.is_configured() {
        return Err(ApiError::Unavailable("결제가 설정되지 않았습니다"));
    }
    let d = find(&state, &input.order_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if d.donor_id != Some(user.id) {
        return Err(ApiError::Forbidden("후원한 사람만 확인할 수 있습니다"));
    }
    match d.status.as_str() {
        // 이미 적립됨: 토스를 다시 부르지 않고 그대로 돌려준다
        "paid" if d.payment_key.as_deref() == Some(input.payment_key.as_str()) => {
            return Ok(Json(d));
        }
        "paid" => return Err(ApiError::Conflict("다른 결제로 이미 처리된 후원입니다")),
        _ => {}
    }
    // 화면이 보낸 금액이 주문 금액과 다르면 토스를 부르기 전에 거절한다(금액 바꿔치기 방지)
    if input.amount != d.amount {
        return Err(ApiError::BadRequest("결제 금액이 주문과 다릅니다"));
    }
    let payment = match state
        .toss
        .confirm(&input.payment_key, &d.order_id, d.amount)
        .await
    {
        Ok(p) => p,
        // 이미 승인된 결제(같은 승인을 두 번 보냄): 토스에서 다시 조회해서 판단한다
        Err(TossError::AlreadyProcessed) => state
            .toss
            .get_payment(&input.payment_key)
            .await
            .map_err(|_| {
                ApiError::BadGateway("결제 서비스에 닿지 못했습니다. 잠시 뒤 다시 시도하세요")
            })?,
        Err(TossError::Rejected { code, message }) => {
            tracing::warn!(%code, %message, order_id = %d.order_id, "토스가 승인을 거절함");
            mark_failed(&state, d.id).await?;
            return Err(ApiError::BadRequest("결제가 승인되지 않았습니다"));
        }
        Err(TossError::Unreachable) => {
            return Err(ApiError::BadGateway(
                "결제 서비스에 닿지 못했습니다. 잠시 뒤 다시 시도하세요",
            ));
        }
    };
    Ok(Json(settle(&state, &d, &payment).await?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotifyInput {
    pub payment_key: String,
}

#[derive(Serialize)]
pub struct NotifyResult {
    pub order_id: String,
    pub status: String,
}

/// 승인 통지. 본문의 결제 키 말고는 아무것도 믿지 않고, 토스에서 결제를 다시 조회해서 적립한다.
/// 로그인 없이 부를 수 있다(결제 서비스가 서버끼리 보내는 통지를 흉내 낸다). 같은 통지가 여러 번 와도 한 번만 적립된다.
pub async fn notify(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<NotifyInput>,
) -> Result<Json<NotifyResult>, ApiError> {
    if !state.toss.is_configured() {
        return Err(ApiError::Unavailable("결제가 설정되지 않았습니다"));
    }
    // 이 길은 부를 때마다 토스에 조회를 보내므로 IP당 횟수를 제한한다.
    // 로그인 없이 부르는 길이라 다른 기능(댓글·후원)과 몫을 나누지 않는다(#41 리뷰)
    if !state
        .auth
        .notify_limiter
        .allow(&format!("notify:{}", client_key(&headers)))
    {
        return Err(ApiError::TooManyRequests);
    }
    let payment = match state.toss.get_payment(&input.payment_key).await {
        Ok(p) => p,
        Err(TossError::Unreachable) => {
            return Err(ApiError::BadGateway("결제 서비스에 닿지 못했습니다"));
        }
        Err(_) => return Err(ApiError::NotFound),
    };
    let d = find(&state, &payment.order_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let done = settle(&state, &d, &payment).await?;
    Ok(Json(NotifyResult {
        order_id: done.order_id,
        status: done.status,
    }))
}

// ---------- 3. 내역과 합계 ----------

pub async fn list(
    State(state): State<AppState>,
    Path(broadcast_id): Path<i64>,
) -> Result<Json<DonationList>, ApiError> {
    let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM broadcasts WHERE id = $1")
        .bind(broadcast_id)
        .fetch_optional(&state.db)
        .await?;
    exists.ok_or(ApiError::NotFound)?;
    let (total, count): (i64, i64) = sqlx::query_as(
        "SELECT COALESCE(SUM(amount), 0)::BIGINT, COUNT(*) FROM donations
          WHERE broadcast_id = $1 AND status = 'paid'",
    )
    .bind(broadcast_id)
    .fetch_one(&state.db)
    .await?;
    let items = sqlx::query_as::<_, DonationItem>(
        "SELECT d.id, COALESCE(u.username, '(탈퇴한 사용자)') AS donor_username, d.amount, d.message, d.paid_at
           FROM donations d LEFT JOIN users u ON u.id = d.donor_id
          WHERE d.broadcast_id = $1 AND d.status = 'paid'
          ORDER BY d.paid_at DESC, d.id DESC LIMIT 100",
    )
    .bind(broadcast_id)
    .fetch_all(&state.db)
    .await?;
    // 후원 순위(#99): 적립된 것만 사람마다 더한다. 합계가 같으면 그 금액에 먼저 다다른 사람(마지막 후원이 이른 사람)이 앞.
    // 탈퇴한 사용자(donor_id NULL)는 뺀다. 여러 사람이 한 줄로 합쳐져 틀린 순위가 되기 때문이다.
    let top = sqlx::query_as::<_, TopDonor>(
        "SELECT u.username AS donor_username, SUM(d.amount)::BIGINT AS total, COUNT(*) AS count
           FROM donations d JOIN users u ON u.id = d.donor_id
          WHERE d.broadcast_id = $1 AND d.status = 'paid'
          GROUP BY u.id, u.username
          ORDER BY total DESC, MAX(d.paid_at) ASC, u.id ASC
          LIMIT $2",
    )
    .bind(broadcast_id)
    .bind(TOP_DONORS)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(DonationList {
        total,
        count,
        top,
        items,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amount_rules() {
        for ok in [1_000, 5_000, 1_000_000] {
            assert!(check_amount(ok).is_ok(), "{ok}");
        }
        for bad in [0, -1_000, 999, 1_500, 1_001_000, i64::MAX] {
            assert!(check_amount(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn message_rules() {
        assert_eq!(clean_message("  응원해요 ").unwrap(), "응원해요");
        assert_eq!(clean_message("").unwrap(), "");
        assert_eq!(clean_message("\u{3164}").unwrap(), "");
        assert_eq!(clean_message("a\r\nb").unwrap(), "a\nb");
        assert!(clean_message("널\u{0}").is_err());
        assert!(clean_message("abc\u{202E}def").is_err());
        assert!(clean_message(&"가".repeat(100)).is_ok());
        assert!(clean_message(&"가".repeat(101)).is_err());
    }
}
