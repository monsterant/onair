//! 토스페이먼츠 결제 API 클라이언트 (#41, 도네이션).
//!
//! - 테스트 모드 키만 쓴다. 실제 돈은 나가지 않는다.
//! - 비밀 키는 TOSS_SECRET_KEY 환경변수로 받는다. 저장소에는 넣지 않는다.
//! - 키를 설정하지 않으면(로컬 개발, 시험) 결제 기능이 꺼진다.
//! - 시험에서는 TOSS_API_BASE를 가짜 서버 주소로 바꿔서 토스에 닿지 않고 시험한다.
//!
//! 쓰는 API (https://docs.tosspayments.com/reference)
//! - POST /v1/payments/confirm: 결제 승인. 결제창에서 돌아온 paymentKey·orderId·amount를 보낸다.
//! - GET /v1/payments/{paymentKey}: 결제 조회. 통지를 받았을 때 본문을 믿지 않고 다시 확인하는 데 쓴다.

use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::json;

#[derive(Clone, Default)]
pub struct Toss {
    inner: Option<Inner>,
}

/// 로그에 찍혀도 비밀 키가 보이지 않게 한다
impl std::fmt::Debug for Toss {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Toss")
            .field("base", &self.inner.as_ref().map(|i| i.base.as_str()))
            .finish()
    }
}

#[derive(Clone)]
struct Inner {
    base: String,
    auth: String,
    client_key: String,
    http: reqwest::Client,
}

/// 토스가 돌려주는 결제 정보 중 우리가 확인하는 칸
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Payment {
    pub payment_key: String,
    pub order_id: String,
    pub total_amount: i64,
    /// DONE이면 승인 완료
    pub status: String,
}

#[derive(Debug)]
pub enum TossError {
    /// 이미 승인된 결제를 다시 승인하려 함(ALREADY_PROCESSED_PAYMENT). 조회해서 판단한다.
    AlreadyProcessed,
    /// 토스가 거절함(코드와 사람이 읽는 문구)
    Rejected { code: String, message: String },
    /// 토스에 닿지 못했거나 응답을 읽지 못함. 주문은 대기로 남겨 다시 시도할 수 있게 한다.
    Unreachable,
}

#[derive(Deserialize)]
struct ErrorBody {
    code: String,
    message: String,
}

/// HTTP 클라이언트(reqwest)를 만들기 전에 암호 방식(ring)을 프로그램 전체 기본값으로 정한다.
/// reqwest를 `rustls-no-provider`로 쓰므로, 이것을 하지 않으면 클라이언트를 만들 때 프로그램이 멈춘다.
/// 여러 번 불러도 된다(이미 정해져 있으면 그대로 둔다).
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// 이 프로그램이 쓰는 HTTP 클라이언트. ring 암호 + 프로그램에 들어 있는 루트 인증서 목록(webpki-roots)을 쓴다.
/// 운영체제 인증서 저장소를 찾지 않는다. 배포 이미지(debian-slim)에는 인증서 저장소가 없어서,
/// 기본 방식으로 만들면 시작하자마자 멈춘다(#44, 2026-11-11 배포 장애).
pub fn http_client(timeout: Duration) -> reqwest::Client {
    install_crypto_provider();
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let tls = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("TLS 설정 실패")
    .with_root_certificates(roots)
    .with_no_client_auth();
    reqwest::Client::builder()
        .use_preconfigured_tls(tls)
        .timeout(timeout)
        .build()
        .expect("HTTP 클라이언트를 만들지 못했습니다")
}

impl Toss {
    /// `base`: 예) https://api.tosspayments.com, `secret_key`: test_gsk_... , `client_key`: test_gck_...
    pub fn new(base: &str, secret_key: &str, client_key: &str) -> Self {
        let http = http_client(Duration::from_secs(10));
        Self {
            inner: Some(Inner {
                base: base.trim_end_matches('/').to_string(),
                // 토스는 "비밀키:"(콜론까지)를 base64로 바꾼 Basic 인증을 쓴다
                auth: format!("Basic {}", STANDARD.encode(format!("{secret_key}:"))),
                client_key: client_key.to_string(),
                http,
            }),
        }
    }

    pub fn is_configured(&self) -> bool {
        self.inner.is_some()
    }

    /// 화면의 결제위젯에 넘기는 클라이언트 키(공개해도 되는 값)
    pub fn client_key(&self) -> Option<&str> {
        self.inner.as_ref().map(|i| i.client_key.as_str())
    }

    /// 결제를 승인한다
    pub async fn confirm(
        &self,
        payment_key: &str,
        order_id: &str,
        amount: i64,
    ) -> Result<Payment, TossError> {
        let i = self.inner.as_ref().ok_or(TossError::Unreachable)?;
        let res = i
            .http
            .post(format!("{}/v1/payments/confirm", i.base))
            .header("Authorization", &i.auth)
            .json(&json!({ "paymentKey": payment_key, "orderId": order_id, "amount": amount }))
            .send()
            .await;
        read(res).await
    }

    /// 결제를 조회한다
    pub async fn get_payment(&self, payment_key: &str) -> Result<Payment, TossError> {
        let i = self.inner.as_ref().ok_or(TossError::Unreachable)?;
        // paymentKey는 주소에 들어가므로 영문·숫자·-_ 말고는 받지 않는다(주소 조작 방지)
        if payment_key.is_empty()
            || !payment_key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(TossError::Rejected {
                code: "INVALID_PAYMENT_KEY".into(),
                message: "결제 키 형식이 맞지 않습니다".into(),
            });
        }
        let res = i
            .http
            .get(format!("{}/v1/payments/{payment_key}", i.base))
            .header("Authorization", &i.auth)
            .send()
            .await;
        read(res).await
    }
}

async fn read(res: reqwest::Result<reqwest::Response>) -> Result<Payment, TossError> {
    let r = res.map_err(|e| {
        tracing::warn!(error = %e, "토스 API에 닿지 못함");
        TossError::Unreachable
    })?;
    if r.status().is_success() {
        return r.json::<Payment>().await.map_err(|e| {
            tracing::warn!(error = %e, "토스 응답을 읽지 못함");
            TossError::Unreachable
        });
    }
    let status = r.status().as_u16();
    // 키 문제(401)와 요청이 너무 많음(429)은 결제 자체의 거절이 아니다. 주문을 실패로 만들지 않고 다시 시도하게 한다(#41 리뷰)
    if matches!(status, 401 | 429) {
        tracing::warn!(status, "토스가 키·횟수 문제로 거절함");
        return Err(TossError::Unreachable);
    }
    if r.status().is_server_error() {
        tracing::warn!(status, "토스 서버 오류");
        return Err(TossError::Unreachable);
    }
    match r.json::<ErrorBody>().await {
        Ok(e) if e.code == "ALREADY_PROCESSED_PAYMENT" => Err(TossError::AlreadyProcessed),
        // 403 중에서 키·권한 문제만 다시 시도로 보고, 카드사 거절(REJECT_CARD_... 등)은 결제 거절로 본다(#41 2바퀴)
        Ok(e)
            if status == 403
                && matches!(e.code.as_str(), "FORBIDDEN_REQUEST" | "UNAUTHORIZED_KEY") =>
        {
            tracing::warn!(code = %e.code, "토스가 권한 문제로 거절함");
            Err(TossError::Unreachable)
        }
        Ok(e) => Err(TossError::Rejected {
            code: e.code,
            message: e.message,
        }),
        Err(_) => Err(TossError::Unreachable),
    }
}
