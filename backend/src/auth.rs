//! 회원가입·로그인·로그아웃과 로그인 상태 확인 (#4).
//!
//! 로그인 상태는 "서버 세션 + HttpOnly 쿠키"로 관리한다.
//! - 로그인하면 서버가 임의의 토큰을 만들어 쿠키(`onair_session`)로 브라우저에 준다.
//! - DB `sessions` 표에는 토큰 원문이 아니라 SHA-256 해시만 저장한다.
//! - 요청이 오면 쿠키의 토큰을 해시해서 표에서 찾는다. 없거나 만료됐으면 로그인하지 않은 것이다.
//! - 로그아웃하면 표에서 지우므로 그 쿠키는 바로 쓸 수 없게 된다(JWT와 다른 점).

use argon2::{Argon2, PasswordHasher, PasswordVerifier};
use axum::{
    Json,
    extract::{FromRequestParts, Path, State},
    http::{StatusCode, request::Parts},
    response::{IntoResponse, Response},
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::AppState;

pub const SESSION_COOKIE: &str = "onair_session";
/// 로그인 유지 기간
pub const SESSION_DAYS: i64 = 7;

/// 인증 관련 설정. main.rs가 환경변수에서 읽는다.
#[derive(Clone, Debug, Default)]
pub struct AuthConfig {
    /// true면 테스트 계정 로그인이 열린다(`/auth/guest`, 공모 6절). `/auth/demo/{1,2}`도 이 값을 본다.
    pub demo_login: bool,
    /// true면 누구나 같은 계정이 되는 공용 데모(`/auth/demo/{1,2}`)만 닫는다(#130). 테스트 계정(`/auth/guest`)은 그대로.
    /// 저장소가 공개되면 누구나 demo1로 방송을 바꾸고 송출할 수 있어서, 운영에서는 닫는다.
    pub shared_demo_off: bool,
    /// 로그인·가입·데모 로그인 횟수 제한(IP 하나당). 기본은 1분에 20번.
    pub limiter: crate::guard::RateLimiter,
    /// 댓글 작성 횟수 제한(사용자 하나당). 기본은 1분에 20번.
    /// 로그인 몫과 따로 둔다. 같이 쓰면 댓글을 많이 쓴 사람 때문에 같은 IP의 로그인이 막힌다(#37 리뷰).
    pub comment_limiter: crate::guard::RateLimiter,
    /// 채팅 횟수 제한(사용자 하나당, #86). 기본은 1분에 20번(3초에 한 번꼴). 댓글 몫과 따로 센다.
    pub chat_limiter: crate::guard::RateLimiter,
    /// 채팅·댓글 횟수 제한(보낸 곳 IP 하나당, #126). 사용자 몫과 따로 센다. 테스트 계정을 새로 만들어 사용자 몫을
    /// 피해도 같은 곳에서 보내는 양은 늘지 않게 한다. None이면 걸지 않는다(시험 기본값). 운영 값은 main.rs.
    pub chat_ip_limiter: Option<crate::guard::RateLimiter>,
    pub comment_ip_limiter: Option<crate::guard::RateLimiter>,
    /// 결제 승인 통지 횟수 제한(IP 하나당). 로그인 없이 부르는 길이라 따로 둔다(#41 리뷰).
    pub notify_limiter: crate::guard::RateLimiter,
    /// 비밀번호 해시 자리를 기다리는 최대 시간(#136). None이면 5초. 넘으면 503으로 돌려보낸다.
    /// 시험에서 짧게 주려고 설정으로 뺐다.
    pub hash_wait: Option<std::time::Duration>,
    /// 로그인 실패 횟수 제한(아이디 하나당, #136). IP 몫만 있으면 IP를 바꿔 가며 한 아이디의 비밀번호를 계속 맞혀 볼 수 있다.
    /// 실패할 때만 센다. None이면 걸지 않는다(시험 기본값). 운영 값은 main.rs.
    pub login_fail_limiter: Option<crate::guard::RateLimiter>,
}

/// 데모 사용자 아이디. 일반 가입에서는 `demo`로 시작하는 아이디를 쓸 수 없다.
pub const DEMO_USERS: [&str; 2] = ["demo1", "demo2"];

// ---------- 오류 ----------

/// API 오류. 화면이 읽을 수 있게 `{"error": "..."}` 모양으로 돌려준다.
#[derive(Debug)]
pub enum ApiError {
    BadRequest(&'static str),
    Unauthorized(&'static str),
    Forbidden(&'static str),
    Conflict(&'static str),
    NotFound,
    /// 404에 이유를 붙일 때(#125)
    NotFoundBecause(&'static str),
    TooManyRequests,
    /// 429에 이유를 붙일 때(천천히 모드 등, #87)
    TooManyRequestsBecause(&'static str),
    Internal,
    /// 503: 기능이 설정되지 않음(예: 결제 키 없음)
    Unavailable(&'static str),
    /// 502: 바깥 서비스(결제 등)에 닿지 못함
    BadGateway(&'static str),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (code, msg) = match self {
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            ApiError::Unauthorized(m) => (StatusCode::UNAUTHORIZED, m),
            ApiError::Forbidden(m) => (StatusCode::FORBIDDEN, m),
            ApiError::Conflict(m) => (StatusCode::CONFLICT, m),
            ApiError::NotFound => (StatusCode::NOT_FOUND, "찾을 수 없습니다"),
            ApiError::NotFoundBecause(m) => (StatusCode::NOT_FOUND, m),
            ApiError::TooManyRequests => (
                StatusCode::TOO_MANY_REQUESTS,
                "요청이 너무 많습니다. 잠시 뒤에 다시 시도하세요",
            ),
            ApiError::TooManyRequestsBecause(m) => (StatusCode::TOO_MANY_REQUESTS, m),
            ApiError::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "서버 오류입니다"),
            ApiError::Unavailable(m) => (StatusCode::SERVICE_UNAVAILABLE, m),
            ApiError::BadGateway(m) => (StatusCode::BAD_GATEWAY, m),
        };
        (code, Json(json!({ "error": msg }))).into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        tracing::error!(error = %e, "DB 오류");
        ApiError::Internal
    }
}

// ---------- 입력 검사 ----------

/// 아이디를 소문자로 바꾸고 규칙에 맞는지 본다: 영문 소문자·숫자·_ 3~20자.
pub fn normalize_username(raw: &str) -> Result<String, &'static str> {
    let u = raw.trim().to_ascii_lowercase();
    let len = u.chars().count();
    if !(3..=20).contains(&len) {
        return Err("아이디는 3~20자여야 합니다");
    }
    if !u
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err("아이디는 영문, 숫자, _만 쓸 수 있습니다");
    }
    Ok(u)
}

/// 비밀번호 길이 검사: 8~128자. 앞뒤 공백도 비밀번호의 일부로 본다.
pub fn check_password(pw: &str) -> Result<(), &'static str> {
    let len = pw.chars().count();
    if len < 8 {
        return Err("비밀번호는 8자 이상이어야 합니다");
    }
    if len > 128 {
        return Err("비밀번호는 128자 이하여야 합니다");
    }
    Ok(())
}

/// 로그인 때 비밀번호 길이 검사: 128자를 넘으면 해시를 계산하지 않고 바로 실패로 본다.
pub fn login_input_ok(pw: &str) -> bool {
    pw.chars().count() <= 128
}

// ---------- 비밀번호·토큰 ----------

fn hash_password(pw: &str) -> Result<String, ApiError> {
    Argon2::default()
        .hash_password(pw.as_bytes())
        .map(|h| h.to_string())
        .map_err(|_| ApiError::Internal)
}

fn verify_password(pw: &str, stored: &str) -> bool {
    Argon2::default()
        .verify_password(pw.as_bytes(), stored)
        .is_ok()
}

/// 아이디가 없을 때도 같은 시간만큼 계산하려고 쓰는 해시. 응답 시간으로 아이디가 있는지 알아내지 못하게 한다.
fn dummy_hash() -> &'static str {
    use std::sync::OnceLock;
    static H: OnceLock<String> = OnceLock::new();
    H.get_or_init(|| hash_password("dummy-password-for-timing").unwrap_or_default())
}

/// argon2는 한 번에 약 0.3초와 메모리 19MiB를 쓴다(#24 A1).
/// - 비동기 작업 스레드에서 바로 돌리면 그동안 그 스레드의 다른 요청(OME 웹훅 포함)이 멈춘다.
///   그래서 따로 떼어 둔 스레드(spawn_blocking)에서 돌린다.
/// - 동시에 도는 계산은 2개까지만 허락한다. 나머지는 차례를 기다린다(2GB 서버의 CPU·메모리 보호).
static HASH_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

// 허가(slot)는 계산하는 스레드 안으로 옮겨서 계산이 끝날 때 돌려준다.
// 요청이 중간에 끊기면 async 쪽은 버려지지만 계산은 끝까지 돈다. 허가를 async 쪽에 두면
// 그 순간 허가가 풀려 동시 계산이 2개를 넘을 수 있다(#24 2바퀴).

/// 해시 자리를 기다리는 기본 최대 시간(#136)
const HASH_WAIT_DEFAULT: std::time::Duration = std::time::Duration::from_secs(5);

/// 해시 자리를 정해진 시간까지만 기다린다(#136). 전에는 끝없이 기다려서, 여러 곳에서 로그인·가입을 몰아 보내면
/// 줄이 길어져 진짜 사용자의 로그인이 한참 늦어졌다. 넘으면 503으로 바로 돌려보낸다.
async fn hash_slot(
    wait: Option<std::time::Duration>,
) -> Result<tokio::sync::SemaphorePermit<'static>, ApiError> {
    match tokio::time::timeout(wait.unwrap_or(HASH_WAIT_DEFAULT), HASH_SLOTS.acquire()).await {
        Ok(Ok(slot)) => Ok(slot),
        Ok(Err(_)) => Err(ApiError::Internal),
        Err(_) => Err(ApiError::Unavailable(
            "지금 로그인·가입 요청이 많습니다. 잠시 뒤 다시 시도하세요",
        )),
    }
}

async fn hash_password_async(
    pw: String,
    wait: Option<std::time::Duration>,
) -> Result<String, ApiError> {
    let slot = hash_slot(wait).await?;
    tokio::task::spawn_blocking(move || {
        let _slot = slot;
        hash_password(&pw)
    })
    .await
    .map_err(|_| ApiError::Internal)?
}

/// 비밀번호 확인. `stored`가 None이면(아이디가 없을 때) 가짜 해시로 같은 시간만큼 계산하고 false를 돌려준다.
/// 자리를 기다리다 시간이 넘으면 Err(503). 이때는 비밀번호를 확인하지 않았으니 로그인 실패로 세지 않는다.
async fn verify_password_async(
    pw: String,
    stored: Option<String>,
    wait: Option<std::time::Duration>,
) -> Result<bool, ApiError> {
    let slot = hash_slot(wait).await?;
    Ok(tokio::task::spawn_blocking(move || {
        let _slot = slot;
        match stored {
            Some(s) => verify_password(&pw, &s),
            None => {
                verify_password(&pw, dummy_hash());
                false
            }
        }
    })
    .await
    .unwrap_or(false))
}

/// 시험용: 지금 비어 있는 해시 허가 수(최대 2)
pub fn free_hash_slots() -> usize {
    HASH_SLOTS.available_permits()
}

/// 32바이트 임의 토큰(쿠키에 넣는 값)과 그 SHA-256(DB에 넣는 값)
fn new_token() -> Result<(String, Vec<u8>), ApiError> {
    let mut raw = [0u8; 32];
    getrandom::fill(&mut raw).map_err(|_| ApiError::Internal)?;
    let token = hex::encode(raw);
    let hash = token_hash(&token);
    Ok((token, hash))
}

fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

fn session_cookie(token: String) -> Cookie<'static> {
    Cookie::build((SESSION_COOKIE, token))
        .http_only(true)
        .secure(true)
        .same_site(SameSite::Lax)
        .path("/")
        .max_age(time::Duration::days(SESSION_DAYS))
        .build()
}

fn removal_cookie() -> Cookie<'static> {
    Cookie::build((SESSION_COOKIE, ""))
        .http_only(true)
        .secure(true)
        .same_site(SameSite::Lax)
        .path("/")
        .max_age(time::Duration::ZERO)
        .build()
}

/// 세션을 만들고 쿠키를 붙인다.
/// - 요청에 이미 세션 쿠키가 있으면 그 세션을 먼저 지운다. 다시 로그인했을 때 이전 세션이 서버에 남지 않게 한다(#24 A4).
/// - 이 사용자의 만료된 세션도 함께 지운다.
async fn start_session(
    state: &AppState,
    jar: CookieJar,
    user_id: i64,
) -> Result<CookieJar, ApiError> {
    let (token, hash) = new_token()?;
    if let Some(old) = jar.get(SESSION_COOKIE) {
        sqlx::query("DELETE FROM sessions WHERE token_hash = $1")
            .bind(token_hash(old.value()))
            .execute(&state.db)
            .await?;
    }
    sqlx::query("DELETE FROM sessions WHERE user_id = $1 AND expires_at <= now()")
        .bind(user_id)
        .execute(&state.db)
        .await?;
    sqlx::query(
        "INSERT INTO sessions (token_hash, user_id, expires_at)
         VALUES ($1, $2, now() + make_interval(days => $3))",
    )
    .bind(&hash)
    .bind(user_id)
    .bind(SESSION_DAYS as i32)
    .execute(&state.db)
    .await?;
    Ok(jar.add(session_cookie(token)))
}

// ---------- 로그인한 사용자 꺼내기 ----------

/// 요청에서 로그인한 사용자를 꺼낸다. 핸들러 인자에 넣으면 로그인하지 않은 요청은 401로 끝난다.
#[derive(Debug, Clone, Serialize)]
pub struct CurrentUser {
    pub id: i64,
    pub username: String,
}

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let jar = CookieJar::from_headers(&parts.headers);
        let token = jar
            .get(SESSION_COOKIE)
            .map(|c| c.value().to_owned())
            .ok_or(ApiError::Unauthorized("로그인이 필요합니다"))?;
        let row: Option<(i64, String)> = sqlx::query_as(
            "SELECT u.id, u.username FROM sessions s JOIN users u ON u.id = s.user_id
             WHERE s.token_hash = $1 AND s.expires_at > now()",
        )
        .bind(token_hash(&token))
        .fetch_optional(&state.db)
        .await?;
        let (id, username) = row.ok_or(ApiError::Unauthorized("로그인이 필요합니다"))?;
        Ok(CurrentUser { id, username })
    }
}

// ---------- 핸들러 ----------

#[derive(Deserialize)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

#[derive(Serialize)]
pub struct Me {
    pub id: i64,
    pub username: String,
}

pub async fn signup(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(c): Json<Credentials>,
) -> Result<(StatusCode, CookieJar, Json<Me>), ApiError> {
    let username = normalize_username(&c.username).map_err(ApiError::BadRequest)?;
    if username.starts_with("demo") {
        return Err(ApiError::BadRequest(
            "demo로 시작하는 아이디는 쓸 수 없습니다",
        ));
    }
    // guest로 시작하는 아이디는 테스트 계정 몫이다. 24시간 뒤 정리 대상이라 일반 가입에 주지 않는다(#35)
    if username.starts_with("guest") {
        return Err(ApiError::BadRequest(
            "guest로 시작하는 아이디는 쓸 수 없습니다",
        ));
    }
    check_password(&c.password).map_err(ApiError::BadRequest)?;
    let hash = hash_password_async(c.password, state.auth.hash_wait).await?;

    // 같은 아이디가 있으면 아무것도 넣지 않는다. 동시에 두 요청이 와도 UNIQUE 제약 덕분에 하나만 들어간다.
    let id: Option<i64> = sqlx::query_scalar(
        "INSERT INTO users (username, password_hash) VALUES ($1, $2)
         ON CONFLICT (username) DO NOTHING RETURNING id",
    )
    .bind(&username)
    .bind(&hash)
    .fetch_optional(&state.db)
    .await?;
    let id = id.ok_or(ApiError::Conflict("이미 있는 아이디입니다"))?;

    let jar = start_session(&state, jar, id).await?;
    Ok((StatusCode::CREATED, jar, Json(Me { id, username })))
}

pub async fn login(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(c): Json<Credentials>,
) -> Result<(CookieJar, Json<Me>), ApiError> {
    const FAIL: ApiError = ApiError::Unauthorized("아이디 또는 비밀번호가 틀렸습니다");
    // 아주 긴 비밀번호로 해시 계산을 오래 시키지 못하게, 계산 전에 먼저 거른다.
    if !login_input_ok(&c.password) {
        return Err(FAIL);
    }
    // 같은 아이디로 너무 많이 틀렸으면 비밀번호를 확인하지 않고 거절한다(#136).
    // 없는 아이디도 똑같이 세므로, 막혔다는 것으로 어떤 아이디가 있는지는 알 수 없다.
    // 형식이 맞는 아이디(정규화한 값)만 센다. 형식이 틀린 아이디는 로그인할 수 없고, 아주 긴 글로
    // 횟수표 메모리를 채우지 못하게 한다(#136 리뷰).
    let fail_key = normalize_username(&c.username)
        .ok()
        .map(|u| format!("login:{u}"));
    let blocked = || match (&state.auth.login_fail_limiter, &fail_key) {
        (Some(l), Some(k)) => l.blocked(k),
        _ => false,
    };
    if blocked() {
        return Err(ApiError::TooManyRequests);
    }
    // 형식이 틀린 아이디도, 없는 아이디도 같은 시간을 들여 같은 실패 메시지로 돌려준다(어떤 아이디가 있는지 알려 주지 않는다).
    let row: Option<(i64, String)> = match normalize_username(&c.username) {
        Ok(username) => {
            sqlx::query_as("SELECT id, password_hash FROM users WHERE username = $1")
                .bind(&username)
                .fetch_optional(&state.db)
                .await?
        }
        Err(_) => None,
    };
    let (id, stored) = match row {
        Some((id, stored)) => (Some(id), Some(stored)),
        None => (None, None),
    };
    let ok = verify_password_async(c.password, stored, state.auth.hash_wait).await?;
    let (Some(id), true) = (id, ok) else {
        if let (Some(l), Some(k)) = (&state.auth.login_fail_limiter, &fail_key) {
            l.allow(k);
        }
        return Err(FAIL);
    };
    // 비밀번호 확인을 기다리는 사이(동시에 2개만 확인한다) 다른 요청들이 실패 횟수를 채웠을 수 있다.
    // 한꺼번에 몰아 보낸 요청이 줄을 서서 맞는 비밀번호를 찾지 못하게, 성공으로 처리하기 전에 한 번 더 본다(#136 리뷰).
    if blocked() {
        return Err(ApiError::TooManyRequests);
    }
    let username = normalize_username(&c.username).map_err(|_| FAIL)?;
    let jar = start_session(&state, jar, id).await?;
    Ok((jar, Json(Me { id, username })))
}

/// 로그아웃: 세션을 DB에서 지우고 쿠키도 지운다. 로그인하지 않았어도 204로 끝낸다(여러 번 불러도 결과가 같다).
pub async fn logout(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<(StatusCode, CookieJar), ApiError> {
    if let Some(c) = jar.get(SESSION_COOKIE) {
        sqlx::query("DELETE FROM sessions WHERE token_hash = $1")
            .bind(token_hash(c.value()))
            .execute(&state.db)
            .await?;
    }
    Ok((StatusCode::NO_CONTENT, jar.add(removal_cookie())))
}

pub async fn me(user: CurrentUser) -> Json<Me> {
    Json(Me {
        id: user.id,
        username: user.username,
    })
}

#[derive(Serialize)]
pub struct SessionInfo {
    /// 로그인하지 않았거나 세션이 끝났으면 null
    pub user: Option<Me>,
}

/// 지금 로그인 상태를 늘 200으로 알려 준다(#134). 화면은 페이지를 바꿀 때마다 부른다.
/// `/auth/me`는 로그인하지 않으면 401이라, 로그인하지 않은 방문자의 개발자 도구에 빨간 오류가 계속 찍혔다.
/// `/auth/me`(401)는 그대로 둔다. 로그아웃·세션 만료 시험과 API 약속이 그것을 쓴다.
/// DB 오류처럼 로그인 여부와 상관없는 실패는 그대로 오류로 돌려준다.
pub async fn session(user: Result<CurrentUser, ApiError>) -> Result<Json<SessionInfo>, ApiError> {
    match user {
        Ok(u) => Ok(Json(SessionInfo {
            user: Some(Me {
                id: u.id,
                username: u.username,
            }),
        })),
        Err(ApiError::Unauthorized(_)) => Ok(Json(SessionInfo { user: None })),
        Err(e) => Err(e),
    }
}

/// 공용 데모 계정(demo1·demo2)으로 바로 로그인한다. `DEMO_LOGIN=true`이고 `SHARED_DEMO_OFF`가 아닐 때만 열린다(#130).
/// n=1이면 demo1, n=2이면 demo2. 소유자와 다른 사용자를 둘 다 시험할 수 있게 두 명을 둔다.
/// 데모 사용자의 비밀번호는 쓸 수 없는 값이라, 이 길로만 로그인할 수 있다.
pub async fn demo_login(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(n): Path<String>,
) -> Result<(CookieJar, Json<Me>), ApiError> {
    if !state.auth.demo_login || state.auth.shared_demo_off {
        return Err(ApiError::NotFound);
    }
    // "1"과 "2"만 받는다. 01, +1 같은 다른 표기는 없는 길로 본다(#25 A15). %31은 주소 규칙상 "1"과 같은 글자라 1로 받는다.
    let username = match n.as_str() {
        "1" => DEMO_USERS[0],
        "2" => DEMO_USERS[1],
        _ => return Err(ApiError::NotFound),
    };
    // 없으면 만들고, 있으면 그대로 쓴다. 여러 번 불러도 사용자는 하나다.
    sqlx::query(
        "INSERT INTO users (username, password_hash) VALUES ($1, '!demo-no-password')
         ON CONFLICT (username) DO NOTHING",
    )
    .bind(username)
    .execute(&state.db)
    .await?;
    let id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE username = $1")
        .bind(username)
        .fetch_one(&state.db)
        .await?;
    let jar = start_session(&state, jar, id).await?;
    Ok((
        jar,
        Json(Me {
            id,
            username: username.to_string(),
        }),
    ))
}

/// 테스트 계정 아이디 앞부분. 일반 가입에서는 `guest`로 시작하는 아이디를 쓸 수 없다(#35).
pub const GUEST_PREFIX: &str = "guest_";
/// 테스트 계정을 남겨 두는 시간
pub const GUEST_KEEP_HOURS: i32 = 24;

/// 심사자마다 따로 쓰는 테스트 계정으로 바로 로그인한다(#35). `DEMO_LOGIN=true`일 때만 열린다.
/// - 부를 때마다 새 사용자 `guest_<16진수 12자>`를 만든다. 심사자끼리 방송 수 한도·송출 키·삭제가 섞이지 않는다.
/// - 비밀번호는 쓸 수 없는 값이라 이 길로만 로그인할 수 있다.
/// - 다른 사용자 권한을 보려면 시크릿 창에서 한 번 더 부르면 된다(쿠키가 따로라 다른 사용자가 된다).
pub async fn guest_login(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<(CookieJar, Json<Me>), ApiError> {
    if !state.auth.demo_login {
        return Err(ApiError::NotFound);
    }
    // 이름이 겹칠 확률은 매우 낮지만(48비트), 겹치면 새 이름으로 몇 번 더 시도한다
    for _ in 0..3 {
        let username = format!("{GUEST_PREFIX}{}", crate::broadcasts::random_hex(6)?);
        let id: Option<i64> = sqlx::query_scalar(
            "INSERT INTO users (username, password_hash) VALUES ($1, '!guest-no-password')
             ON CONFLICT (username) DO NOTHING RETURNING id",
        )
        .bind(&username)
        .fetch_optional(&state.db)
        .await?;
        if let Some(id) = id {
            let jar = start_session(&state, jar, id).await?;
            return Ok((jar, Json(Me { id, username })));
        }
    }
    Err(ApiError::Internal)
}

/// 모든 사용자의 만료된 세션을 지운다(#25 A11). 한 시간마다 부른다.
pub async fn cleanup_sessions(db: &sqlx::PgPool) -> Result<u64, sqlx::Error> {
    Ok(
        sqlx::query("DELETE FROM sessions WHERE expires_at <= now()")
            .execute(db)
            .await?
            .rows_affected(),
    )
}

/// 만든 지 24시간이 지난 테스트 계정을 지운다(#35). 방송·세션은 외래 키(ON DELETE CASCADE)로 함께 지워진다.
/// 지금 송출 중(live)인 방송이 있는 계정은 남긴다(송출 도중에 끊기지 않게).
/// 송출이 잠깐 끊겨 대기(ready)가 된 몇 초 사이에 지우지 않게, 시작한 적이 있고 10분 안에 상태가 바뀐 방송이 있는 계정도 남긴다(#136).
pub async fn cleanup_guests(db: &sqlx::PgPool) -> Result<u64, sqlx::Error> {
    let done = sqlx::query(
        // 이름만 보지 않고, guest_login만 쓰는 비밀번호 표시 값도 함께 본다.
        // 이 변경 전에 guest_로 가입한 일반 회원이 있어도 지우지 않게 한다(#35 리뷰).
        r"DELETE FROM users u
          WHERE u.username LIKE 'guest\_%'
            AND u.password_hash = '!guest-no-password'
            AND u.created_at < now() - make_interval(hours => $1)
            AND NOT EXISTS (SELECT 1 FROM broadcasts b WHERE b.owner_id = u.id
                              AND (b.status = 'live'
                                   OR (b.status = 'ready' AND b.started_at IS NOT NULL
                                       AND b.updated_at > now() - interval '10 minutes')))",
    )
    .bind(GUEST_KEEP_HOURS)
    .execute(db)
    .await?;
    Ok(done.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn username_rules() {
        assert_eq!(normalize_username("  Alice_01 "), Ok("alice_01".into()));
        assert!(normalize_username("ab").is_err());
        assert!(normalize_username("abc").is_ok());
        assert!(normalize_username(&"a".repeat(20)).is_ok());
        assert!(normalize_username(&"a".repeat(21)).is_err());
        assert!(normalize_username("한글아이디").is_err());
        assert!(normalize_username("bad-name").is_err());
        assert!(normalize_username("bad name").is_err());
    }

    #[test]
    fn password_rules() {
        assert!(check_password("1234567").is_err());
        assert!(check_password("12345678").is_ok());
        assert!(check_password(&"x".repeat(128)).is_ok());
        assert!(check_password(&"x".repeat(129)).is_err());
        // 한글은 글자 수로 센다
        assert!(check_password("가나다라마바사아").is_ok());
    }

    #[test]
    fn hash_roundtrip_and_demo_hash_never_verifies() {
        let h = hash_password("correct horse").unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(verify_password("correct horse", &h));
        assert!(!verify_password("wrong horse", &h));
        assert!(!verify_password("", "!demo-no-password"));
        assert!(!verify_password("!demo-no-password", "!demo-no-password"));
    }

    #[test]
    fn token_is_random_and_hash_matches() {
        let (t1, h1) = new_token().unwrap();
        let (t2, _) = new_token().unwrap();
        assert_eq!(t1.len(), 64);
        assert_ne!(t1, t2);
        assert_eq!(h1, token_hash(&t1));
    }
}
