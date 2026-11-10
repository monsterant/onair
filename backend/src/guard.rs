//! 요청을 핸들러보다 먼저 거르는 장치 (#24).
//!
//! 1. 다른 사이트에서 보낸 요청 거절(CSRF 방어, A3)
//!    - 쿠키 SameSite=Lax는 "다른 사이트에서 보낸 POST에 쿠키를 싣지 않는 것"만 막는다.
//!      응답에 실린 Set-Cookie는 브라우저가 받아들인다.
//!    - 그래서 악성 사이트가 폼으로 `/auth/demo/1`을 보내 방문자를 몰래 demo1로 로그인시키거나,
//!      `/auth/logout`으로 쿠키를 지울 수 있었다.
//!    - 브라우저는 다른 사이트에서 보낸 POST에 `Origin`과 `Sec-Fetch-Site: cross-site`를 붙인다.
//!      상태를 바꾸는 요청(GET·HEAD·OPTIONS가 아닌 것)에서 이 둘을 보고 거절한다.
//! 2. 인증 요청 횟수 제한(A2)
//!    - 로그인·가입·데모 로그인은 IP 하나당 1분에 20번까지만 받는다. 비밀번호 무차별 대입과 대량 가입을 늦춘다.
//!    - IP는 Caddy가 붙여 주는 `X-Forwarded-For`의 마지막 값을 쓴다.
//!      Caddy는 믿지 않는 곳에서 온 이 헤더를 지우고 실제 접속 주소로 다시 쓰므로 위조할 수 없다.
//!      (배포에서는 백엔드가 인터넷에 열려 있지 않고 Caddy만 거친다.)
//!    - 서버 메모리에만 세므로 서버를 다시 켜면 처음부터 센다.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    extract::{Request, State},
    http::{HeaderMap, Method, header},
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::{AppState, auth::ApiError};

// ---------- 1. 다른 사이트 요청 거절 ----------

/// 상태를 바꾸는 요청이 같은 사이트에서 왔는지 본다.
pub fn same_origin(method: &Method, headers: &HeaderMap) -> bool {
    if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return true;
    }
    let get = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    if get("sec-fetch-site") == Some("cross-site") {
        return false;
    }
    match get(header::ORIGIN.as_str()) {
        // Origin이 없으면(curl, 서버끼리 호출, 오래된 브라우저) 쿠키가 실린 브라우저 공격이 아니라고 본다.
        None => true,
        Some(origin) => {
            let origin_host = origin
                .split_once("://")
                .map(|(_, rest)| rest)
                .unwrap_or("null");
            get(header::HOST.as_str()).is_some_and(|host| host.eq_ignore_ascii_case(origin_host))
        }
    }
}

pub async fn reject_cross_site(req: Request, next: Next) -> Response {
    if same_origin(req.method(), req.headers()) {
        next.run(req).await
    } else {
        ApiError::Forbidden("다른 사이트에서 보낸 요청은 받지 않습니다").into_response()
    }
}

// ---------- 2. 횟수 제한 ----------

/// 정해진 시간(창) 안에 키 하나당 받을 수 있는 요청 수를 센다.
#[derive(Clone, Debug)]
pub struct RateLimiter {
    counts: Arc<Mutex<HashMap<String, (Instant, u32)>>>,
    max: u32,
    window: Duration,
}

impl RateLimiter {
    pub fn new(max: u32, window: Duration) -> Self {
        Self {
            counts: Default::default(),
            max,
            window,
        }
    }

    /// 이번 요청을 받아도 되면 true. 받으면 횟수를 1 늘린다.
    pub fn allow(&self, key: &str) -> bool {
        let now = Instant::now();
        let Ok(mut map) = self.counts.lock() else {
            return true;
        };
        // 기록이 많이 쌓이면 창이 지난 것을 지운다(메모리가 끝없이 늘지 않게).
        if map.len() > 10_000 {
            map.retain(|_, (start, _)| now.duration_since(*start) < self.window);
        }
        let entry = map.entry(key.to_string()).or_insert((now, 0));
        if now.duration_since(entry.0) >= self.window {
            *entry = (now, 0);
        }
        if entry.1 >= self.max {
            return false;
        }
        entry.1 += 1;
        true
    }
}

impl Default for RateLimiter {
    /// 인증 요청: IP 하나당 1분에 20번
    fn default() -> Self {
        Self::new(20, Duration::from_secs(60))
    }
}

/// 요청을 보낸 곳. Caddy가 붙인 X-Forwarded-For의 마지막 값, 없으면 "direct"
pub fn client_key(headers: &HeaderMap) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.rsplit(',').next())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "direct".into())
}

pub async fn limit_auth(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if state.auth.limiter.allow(&client_key(req.headers())) {
        next.run(req).await
    } else {
        ApiError::TooManyRequests.into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn h(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in pairs {
            m.insert(*k, HeaderValue::from_static(v));
        }
        m
    }

    #[test]
    fn same_origin_rules() {
        let post = Method::POST;
        // GET은 언제나 통과
        assert!(same_origin(
            &Method::GET,
            &h(&[("sec-fetch-site", "cross-site")])
        ));
        // Origin 없음: 통과
        assert!(same_origin(&post, &h(&[("host", "a.example")])));
        // 같은 사이트
        assert!(same_origin(
            &post,
            &h(&[
                ("host", "a.example"),
                ("origin", "https://a.example"),
                ("sec-fetch-site", "same-origin")
            ])
        ));
        assert!(same_origin(
            &post,
            &h(&[
                ("host", "localhost:5173"),
                ("origin", "http://localhost:5173")
            ])
        ));
        // 다른 사이트
        assert!(!same_origin(
            &post,
            &h(&[("host", "a.example"), ("origin", "https://evil.example")])
        ));
        assert!(!same_origin(
            &post,
            &h(&[("host", "a.example"), ("sec-fetch-site", "cross-site")])
        ));
        assert!(!same_origin(
            &post,
            &h(&[("host", "a.example"), ("origin", "null")])
        ));
        // 포트가 다르면 다른 곳
        assert!(!same_origin(
            &post,
            &h(&[("host", "a.example"), ("origin", "https://a.example:8443")])
        ));
        // Host가 없으면 비교할 수 없으니 거절
        assert!(!same_origin(&post, &h(&[("origin", "https://a.example")])));
    }

    #[test]
    fn limiter_counts_per_key_and_resets() {
        let l = RateLimiter::new(2, Duration::from_millis(50));
        assert!(l.allow("a"));
        assert!(l.allow("a"));
        assert!(!l.allow("a"));
        assert!(l.allow("b"));
        std::thread::sleep(Duration::from_millis(60));
        assert!(l.allow("a"));
    }

    #[test]
    fn client_key_uses_last_forwarded_value() {
        assert_eq!(client_key(&h(&[])), "direct");
        assert_eq!(client_key(&h(&[("x-forwarded-for", "1.1.1.1")])), "1.1.1.1");
        assert_eq!(
            client_key(&h(&[("x-forwarded-for", "9.9.9.9, 2.2.2.2")])),
            "2.2.2.2"
        );
    }
}
