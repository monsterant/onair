//! OME 관리 API 클라이언트 (#3).
//!
//! 백엔드가 OME에게 "이 스트림 끊어", "지금 스트림 뭐 있어?"를 묻는 데 쓴다.
//! - OME API(8081)는 Docker 안쪽에서만 열고 인터넷에는 열지 않는다. 접근 토큰은 OME_API_TOKEN 환경변수로 받는다.
//! - 주소를 설정하지 않으면(로컬 개발, 시험) 아무 일도 하지 않는다.
//! - OME가 응답하지 않아도 방송 API는 실패시키지 않는다. 끊기는 웹훅(종료된 방송 거절)과 맞춰 보기가 한 번 더 챙긴다.

use std::{collections::HashSet, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::Deserialize;

#[derive(Clone, Default)]
pub struct OmeApi {
    inner: Option<Inner>,
}

/// 로그에 찍혀도 토큰이 보이지 않게 주소만 보여 준다
impl std::fmt::Debug for OmeApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OmeApi")
            .field("base", &self.base())
            .finish()
    }
}

#[derive(Clone)]
struct Inner {
    base: String,
    auth: String,
    http: reqwest::Client,
}

#[derive(Deserialize)]
struct StreamList {
    response: Vec<String>,
}

impl OmeApi {
    /// `base`: 예) http://ome:8081, `token`: OME Server.xml의 AccessToken과 같은 값
    pub fn new(base: &str, token: &str) -> Self {
        crate::toss::install_crypto_provider();
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .expect("HTTP 클라이언트를 만들지 못했습니다");
        Self {
            inner: Some(Inner {
                base: base.trim_end_matches('/').to_string(),
                // OME API는 Basic 인증에 "토큰을 base64로 바꾼 값"을 쓴다.
                auth: format!("Basic {}", STANDARD.encode(token)),
                http,
            }),
        }
    }

    pub fn is_configured(&self) -> bool {
        self.inner.is_some()
    }

    /// 설정된 OME API 주소(시험·로그용). 토큰은 돌려주지 않는다.
    pub fn base(&self) -> Option<&str> {
        self.inner.as_ref().map(|i| i.base.as_str())
    }

    fn streams_url(base: &str) -> String {
        format!("{base}/v1/vhosts/default/apps/app/streams")
    }

    /// 스트림을 끊는다. 실패해도 오류를 돌려주지 않고 기록만 남긴다.
    pub async fn stop_stream(&self, name: &str) {
        let Some(i) = &self.inner else { return };
        let url = format!("{}/{}", Self::streams_url(&i.base), name);
        match i
            .http
            .delete(&url)
            .header("Authorization", &i.auth)
            .send()
            .await
        {
            Ok(r) if r.status().is_success() || r.status() == reqwest::StatusCode::NOT_FOUND => {}
            Ok(r) => tracing::warn!(status = %r.status(), name, "OME 스트림 끊기 실패"),
            Err(e) => tracing::warn!(error = %e, name, "OME API에 닿지 못함"),
        }
    }

    /// 지금 OME에 있는 스트림 이름들. 물어볼 수 없으면 None(그때는 상태를 건드리지 않는다).
    pub async fn list_streams(&self) -> Option<HashSet<String>> {
        let i = self.inner.as_ref()?;
        let r = i
            .http
            .get(Self::streams_url(&i.base))
            .header("Authorization", &i.auth)
            .send()
            .await
            .ok()?;
        if !r.status().is_success() {
            tracing::warn!(status = %r.status(), "OME 스트림 목록 조회 실패");
            return None;
        }
        let list: StreamList = r.json().await.ok()?;
        Some(list.response.into_iter().collect())
    }
}
