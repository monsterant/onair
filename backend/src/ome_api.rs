//! OME 관리 API 클라이언트 (#3).
//!
//! 백엔드가 OME에게 "이 스트림 끊어", "지금 스트림 뭐 있어?"를 묻는 데 쓴다.
//! - OME API(8081)는 Docker 안쪽에서만 열고 인터넷에는 열지 않는다. 접근 토큰은 OME_API_TOKEN 환경변수로 받는다.
//! - 주소를 설정하지 않으면(로컬 개발, 시험) 아무 일도 하지 않는다.
//! - OME가 응답하지 않아도 방송 API는 실패시키지 않는다. 끊기는 웹훅(종료된 방송 거절)과 맞춰 보기가 한 번 더 챙긴다.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::Deserialize;

#[derive(Clone, Default)]
pub struct OmeApi {
    inner: Option<Inner>,
    /// 시청자 수를 잠깐 모아 둔다(#85). 목록을 자주 불러도 OME에 매번 묻지 않게.
    viewers_cache: Arc<Mutex<ViewersCache>>,
}

/// 스트림 이름 → (물어본 때, 시청자 수)
type ViewersCache = HashMap<String, (Instant, Option<i64>)>;

/// 시청자 수를 모아 두는 시간
const VIEWERS_TTL: Duration = Duration::from_secs(5);
/// 시청자 수 요청 하나를 기다리는 시간. 다른 OME 요청(3초)보다 짧게 둔다(#88 R1: OME가 멈추면 목록이 느려짐).
const VIEWERS_TIMEOUT: Duration = Duration::from_millis(800);

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
        let http = crate::toss::http_client(Duration::from_secs(3));
        Self {
            inner: Some(Inner {
                base: base.trim_end_matches('/').to_string(),
                // OME API는 Basic 인증에 "토큰을 base64로 바꾼 값"을 쓴다.
                auth: format!("Basic {}", STANDARD.encode(token)),
                http,
            }),
            viewers_cache: Default::default(),
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
    /// 스트림을 끊는다. 끊었거나 원래 없으면(404) true, OME가 실패하거나 닿지 않으면 false(#131).
    /// OME API가 설정되지 않았으면(로컬 등) 할 일이 없으니 true.
    pub async fn stop_stream(&self, name: &str) -> bool {
        let Some(i) = &self.inner else { return true };
        let url = format!("{}/{}", Self::streams_url(&i.base), name);
        match i
            .http
            .delete(&url)
            .header("Authorization", &i.auth)
            .send()
            .await
        {
            Ok(r) if r.status().is_success() || r.status() == reqwest::StatusCode::NOT_FOUND => {
                true
            }
            Ok(r) => {
                tracing::warn!(status = %r.status(), name, "OME 스트림 끊기 실패");
                false
            }
            Err(e) => {
                tracing::warn!(error = %e, name, "OME API에 닿지 못함");
                false
            }
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

    /// 지금 이 스트림을 WebRTC로 보는 사람 수(#85). 물어볼 수 없으면 None.
    /// OME 통계의 LL-HLS 접속 수는 시청자가 없어도 0이 아니어서(로컬 확인: 시청자 0명에 8) 세지 않는다.
    /// 방송자는 송출만 하고 자기 방송을 WebRTC로 보지 않으므로 들어가지 않는다.
    /// 결과(실패 포함)는 5초 동안 모아 둔다. OME가 느리거나 꺼져 있어도 목록이 매번 기다리지 않게.
    pub async fn viewers(&self, name: &str) -> Option<i64> {
        let i = self.inner.as_ref()?;
        // 모아 둔 값이 새것이면 쓴다. 오래됐으면, 묻기 전에 시각을 먼저 새로 적어 둔다(#88 R1).
        // 그러면 묻는 동안 들어온 다른 요청은 기다리지 않고 예전 값을 쓴다(OME를 한꺼번에 여러 번 부르지 않음).
        let stale = {
            let mut cache = self.viewers_cache.lock().unwrap();
            match cache.get(name) {
                Some((at, v)) if at.elapsed() < VIEWERS_TTL => return *v,
                Some((_, v)) => {
                    let old = *v;
                    cache.insert(name.to_string(), (Instant::now(), old));
                    old
                }
                None => {
                    cache.insert(name.to_string(), (Instant::now(), None));
                    None
                }
            }
        };
        let url = format!(
            "{}/v1/stats/current/vhosts/default/apps/app/streams/{}",
            i.base, name
        );
        let got = async {
            let r = i
                .http
                .get(&url)
                .header("Authorization", &i.auth)
                .timeout(VIEWERS_TIMEOUT)
                .send()
                .await
                .ok()?;
            if !r.status().is_success() {
                return None;
            }
            let v: serde_json::Value = r.json().await.ok()?;
            v["response"]["connections"]["webrtc"].as_i64()
        }
        .await;
        // 시간 초과처럼 답을 못 받았으면 예전 값을 그대로 둔다(잠깐 끊긴 것으로 숫자가 사라지지 않게).
        // 스트림이 없다고(404 등) 답한 것도 None이라 구분하지 않는다. 방송이 끝나면 화면이 숫자를 숨긴다.
        let got = got.or(stale);
        let mut cache = self.viewers_cache.lock().unwrap();
        // 끝난 스트림 이름이 계속 쌓이지 않게 오래된 것은 지운다
        cache.retain(|_, (at, _)| at.elapsed() < VIEWERS_TTL * 12);
        cache.insert(name.to_string(), (Instant::now(), got));
        got
    }

    fn app_url(base: &str, action: &str) -> String {
        format!("{base}/v1/vhosts/default/apps/app:{action}")
    }

    /// 스트림의 영상 코덱(예: "H264", "VP8"). 녹화 파일 형식을 정하는 데 쓴다(#47). 모르면 None.
    pub async fn video_codec(&self, name: &str) -> Option<String> {
        let i = self.inner.as_ref()?;
        let url = format!("{}/{}", Self::streams_url(&i.base), name);
        let r = i
            .http
            .get(&url)
            .header("Authorization", &i.auth)
            .send()
            .await
            .ok()?;
        if !r.status().is_success() {
            return None;
        }
        let v: serde_json::Value = r.json().await.ok()?;
        v["response"]["input"]["tracks"]
            .as_array()?
            .iter()
            .find(|t| {
                t["type"]
                    .as_str()
                    .is_some_and(|x| x.eq_ignore_ascii_case("video"))
            })
            .and_then(|t| t["video"]["codec"].as_str())
            .map(str::to_string)
    }

    /// 녹화를 시작한다(#47). `file`은 녹화 폴더 안의 파일 이름, `variants`는 넣을 영상·소리 출력 이름.
    /// OME는 스트림이 아직 없어도 받아 두었다가 스트림이 오면 시작한다. 성공하면 true.
    pub async fn start_record(
        &self,
        id: &str,
        stream: &str,
        file: &str,
        variants: &[&str],
    ) -> bool {
        let Some(i) = &self.inner else { return false };
        let body = serde_json::json!({
            "id": id,
            "stream": { "name": stream, "variantNames": variants },
            "filePath": format!("/{file}"),
        });
        match i
            .http
            .post(Self::app_url(&i.base, "startRecord"))
            .header("Authorization", &i.auth)
            .json(&body)
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => true,
            Ok(r) => {
                tracing::warn!(status = %r.status(), id, "OME 녹화 시작 실패");
                false
            }
            Err(e) => {
                tracing::warn!(error = %e, id, "OME API에 닿지 못함(녹화 시작)");
                false
            }
        }
    }

    /// 녹화를 멈춘다(#47). 멈췄으면(또는 이미 없는 녹화면) true. OME에 닿지 못했거나 거절하면 false(다음에 다시 시도).
    pub async fn stop_record(&self, id: &str) -> bool {
        let Some(i) = &self.inner else { return false };
        match i
            .http
            .post(Self::app_url(&i.base, "stopRecord"))
            .header("Authorization", &i.auth)
            .json(&serde_json::json!({ "id": id }))
            .send()
            .await
        {
            Ok(r) if r.status().is_success() || r.status() == reqwest::StatusCode::NOT_FOUND => {
                true
            }
            Ok(r) => {
                tracing::warn!(status = %r.status(), id, "OME 녹화 멈춤 실패");
                false
            }
            Err(e) => {
                tracing::warn!(error = %e, id, "OME API에 닿지 못함(녹화 멈춤)");
                false
            }
        }
    }

    /// OME가 지금 들고 있는 녹화 id들(#47). 물어볼 수 없으면 None(그때는 아무것도 바꾸지 않는다).
    pub async fn records(&self) -> Option<HashSet<String>> {
        let i = self.inner.as_ref()?;
        let r = i
            .http
            .post(Self::app_url(&i.base, "records"))
            .header("Authorization", &i.auth)
            .json(&serde_json::json!({}))
            .send()
            .await
            .ok()?;
        if !r.status().is_success() {
            return None;
        }
        let v: serde_json::Value = r.json().await.ok()?;
        Some(
            v["response"]
                .as_array()?
                .iter()
                .filter_map(|x| x["id"].as_str().map(str::to_string))
                .collect(),
        )
    }
}
