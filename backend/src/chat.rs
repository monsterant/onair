//! 실시간 채팅(#86).
//!
//! - 받기: `GET /broadcasts/{id}/chat/stream` — SSE(서버가 연결을 열어 둔 채 새 메시지를 계속 보내 주는 방식).
//!   처음에 최근 메시지(최대 50개)를 보내고, 그다음 새 메시지를 보낸다. 누구나(로그인 없이) 읽는다.
//!   브라우저의 EventSource는 연결이 끊기면 알아서 다시 붙는다. 다시 붙으면 최근 메시지를 또 받으므로 화면이 id로 겹친 것을 버린다.
//! - 보내기: `POST /broadcasts/{id}/chat {"text"}` — 로그인한 사람만. 보낸 사람 이름은 서버가 세션에서 붙인다.
//!   일반 POST라 로그인 쿠키·같은 사이트 검사(CSRF)·오류 JSON을 그대로 쓴다.
//! - 후원 안내: 후원이 실제로 적립된 한 번에만 `kind: "donation"` 메시지를 보낸다(donations.rs).
//!
//! 채팅은 서버 메모리에만 있다. 서버를 다시 켜면 사라진다(댓글·후원은 DB라 남는다).
//! 백엔드가 한 대라는 전제다(여러 대로 늘리면 메모리 방식은 맞지 않는다).

use std::{
    collections::{HashMap, HashSet, VecDeque},
    convert::Infallible,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::sse::{Event, KeepAlive, Sse},
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tokio::sync::broadcast;
use tokio_stream::{Stream, StreamExt, wrappers::BroadcastStream};

use crate::{
    AppState,
    auth::{ApiError, CurrentUser},
    comments::{bidi_control, blank_like},
};

/// 채팅 한 줄의 최대 글자 수
pub const TEXT_MAX: usize = 200;
/// 방송마다 기억하는 최근 메시지 수(늦게 들어온 사람에게 보여 줌)
pub const RECENT_MAX: usize = 50;
/// 받는 쪽이 늦을 때 쌓아 둘 수 있는 메시지 수. 넘치면 그 사람은 밀린 것을 건너뛴다.
const CHANNEL_CAP: usize = 128;

#[derive(Serialize, Debug, Clone)]
pub struct Message {
    /// 서버 전체에서 늘어나는 번호. 화면이 다시 붙었을 때 겹친 것을 버리는 데 쓴다.
    pub id: i64,
    /// "chat" 또는 "donation"(후원 안내)
    pub kind: &'static str,
    /// 보낸 사람 번호(방송 주인 화면의 "제한" 버튼이 쓴다, #87). 후원 안내는 null.
    pub user_id: Option<i64>,
    pub username: String,
    pub text: String,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

struct Room {
    tx: broadcast::Sender<Arc<Message>>,
    recent: VecDeque<Arc<Message>>,
}

struct Inner {
    rooms: HashMap<i64, Room>,
    next_id: i64,
    /// (방송, 사용자) → 마지막으로 채팅한 때. 천천히 모드에 쓴다(#87).
    last_sent: HashMap<(i64, i64), Instant>,
    /// 보낸 곳(IP)마다 지금 열려 있는 채팅 스트림 수(#91)
    streams: HashMap<String, usize>,
    /// 방송 → 서버를 켠 뒤 그 방송에서 채팅을 쓴 사람(#125). 방송 주인이 막을 수 있는 대상을 고른다.
    chatters: HashMap<i64, HashSet<i64>>,
}

/// 방송 하나에 적는 채팅한 사람 수의 상한(#125 리뷰). 방송마다 따로 세서, 한 사람이 자기 방송들을 채워도
/// 다른 방송의 기록은 막히지 않는다.
pub const CHATTERS_PER_BROADCAST: usize = 2_000;

impl Default for Inner {
    /// 메시지 번호는 서버를 켠 시각(마이크로초)에서 시작한다. 서버를 다시 켜도 번호가 앞 번호와 겹치지 않아,
    /// 열려 있던 화면이 새 글을 "이미 본 글"로 버리지 않는다(#86 리뷰).
    fn default() -> Self {
        let now = OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000;
        Self {
            rooms: HashMap::new(),
            next_id: i64::try_from(now).unwrap_or(0),
            last_sent: HashMap::new(),
            streams: HashMap::new(),
            chatters: HashMap::new(),
        }
    }
}

/// 방송별 채팅방. 복사해도 같은 방들을 가리킨다.
#[derive(Clone, Default)]
pub struct Hub(Arc<Mutex<Inner>>);

impl Hub {
    fn room(inner: &mut Inner, broadcast_id: i64) -> &mut Room {
        inner.rooms.entry(broadcast_id).or_insert_with(|| Room {
            tx: broadcast::channel(CHANNEL_CAP).0,
            recent: VecDeque::new(),
        })
    }

    /// 최근 메시지와, 앞으로 올 메시지를 받을 구독. 같은 잠금 안에서 둘을 함께 얻어 사이에 빠지는 메시지가 없다.
    pub fn subscribe(
        &self,
        broadcast_id: i64,
    ) -> (Vec<Arc<Message>>, broadcast::Receiver<Arc<Message>>) {
        let mut inner = self.0.lock().unwrap();
        let room = Self::room(&mut inner, broadcast_id);
        (room.recent.iter().cloned().collect(), room.tx.subscribe())
    }

    /// 메시지를 방에 넣고 지금 듣는 모두에게 보낸다.
    pub fn publish(
        &self,
        broadcast_id: i64,
        kind: &'static str,
        user_id: Option<i64>,
        username: &str,
        text: &str,
    ) -> Arc<Message> {
        let mut inner = self.0.lock().unwrap();
        if let ("chat", Some(uid)) = (kind, user_id) {
            let set = inner.chatters.entry(broadcast_id).or_default();
            if set.len() < CHATTERS_PER_BROADCAST {
                set.insert(uid);
            }
        }
        inner.next_id += 1;
        let msg = Arc::new(Message {
            id: inner.next_id,
            kind,
            user_id,
            username: username.to_string(),
            text: text.to_string(),
            at: OffsetDateTime::now_utc(),
        });
        let room = Self::room(&mut inner, broadcast_id);
        room.recent.push_back(msg.clone());
        while room.recent.len() > RECENT_MAX {
            room.recent.pop_front();
        }
        // 듣는 사람이 없으면 Err지만 괜찮다(최근 목록에는 남았다)
        let _ = room.tx.send(msg.clone());
        msg
    }
}

/// 채팅 스트림 하나를 세는 표. 스트림이 끝나 이 값이 버려지면(Drop) 수를 하나 줄인다(#91).
pub struct StreamSlot {
    hub: Hub,
    key: String,
}

impl Drop for StreamSlot {
    fn drop(&mut self) {
        let mut inner = self.hub.0.lock().unwrap();
        if let Some(n) = inner.streams.get_mut(&self.key) {
            *n -= 1;
            if *n == 0 {
                inner.streams.remove(&self.key);
            }
        }
    }
}

impl Hub {
    /// 보낸 곳(IP) 하나가 동시에 열 수 있는 채팅 스트림을 per_client개로, 서버 전체를 total개로 막는다(#91).
    /// 전체 상한은 IPv6 주소를 바꿔 가거나 여러 IP로 IP당 상한을 넘는 경우를 막는다(#91 리뷰).
    /// 자리가 있으면 그 자리(StreamSlot)를 주고, 없으면 None. 확인과 늘리기를 한 잠금 안에서 한다.
    pub fn open_stream(&self, key: &str, per_client: usize, total: usize) -> Option<StreamSlot> {
        let mut inner = self.0.lock().unwrap();
        let all: usize = inner.streams.values().sum();
        let n = inner.streams.get(key).copied().unwrap_or(0);
        if n >= per_client || all >= total {
            return None;
        }
        *inner.streams.entry(key.to_string()).or_insert(0) += 1;
        Some(StreamSlot {
            hub: self.clone(),
            key: key.to_string(),
        })
    }

    /// 지금 열린 채팅 스트림 수(시험용)
    pub fn open_streams(&self, key: &str) -> usize {
        self.0
            .lock()
            .unwrap()
            .streams
            .get(key)
            .copied()
            .unwrap_or(0)
    }

    /// 방송이 지워지면 그 방을 없앤다(메모리가 쌓이지 않게). 듣던 연결은 끝난다.
    pub fn remove(&self, broadcast_id: i64) {
        let mut inner = self.0.lock().unwrap();
        inner.rooms.remove(&broadcast_id);
        inner.last_sent.retain(|(b, _), _| *b != broadcast_id);
        inner.chatters.remove(&broadcast_id);
    }

    /// DB에 남아 있는 방송만 남기고 나머지 방·기록을 지운다(#125 리뷰). 테스트 계정 정리가 방송을 DB에서
    /// 지울 때(CASCADE) 메모리의 방·천천히 모드·채팅한 사람 기록이 남지 않게 한 시간마다 부른다.
    pub fn retain_broadcasts(&self, alive: &HashSet<i64>) {
        let mut inner = self.0.lock().unwrap();
        inner.rooms.retain(|b, _| alive.contains(b));
        inner.last_sent.retain(|(b, _), _| alive.contains(b));
        inner.chatters.retain(|b, _| alive.contains(b));
    }

    /// 이 사람이 (서버를 켠 뒤) 이 방송에서 채팅을 쓴 적이 있는지(#125)
    pub fn chatted(&self, broadcast_id: i64, user_id: i64) -> bool {
        self.0
            .lock()
            .unwrap()
            .chatters
            .get(&broadcast_id)
            .is_some_and(|s| s.contains(&user_id))
    }

    /// 천천히 모드(#87): 이 사람이 이 방송에서 마지막으로 쓴 지 secs초가 지났으면 true를 돌려주고 지금 시각을 적는다.
    /// 확인과 기록을 한 잠금 안에서 해서, 같은 사람이 동시에 두 번 보내도 하나만 통과한다.
    pub fn slow_ok(&self, broadcast_id: i64, user_id: i64, secs: i32) -> bool {
        let mut inner = self.0.lock().unwrap();
        let wait = Duration::from_secs(u64::try_from(secs).unwrap_or(0));
        let key = (broadcast_id, user_id);
        if let Some(at) = inner.last_sent.get(&key)
            && at.elapsed() < wait
        {
            return false;
        }
        // 오래된 기록은 지운다(쌓이지 않게)
        if inner.last_sent.len() > 10_000 {
            inner
                .last_sent
                .retain(|_, at| at.elapsed() < Duration::from_secs(60));
        }
        inner.last_sent.insert(key, Instant::now());
        true
    }
}

/// 채팅 글 검사: 한 줄로 만들고, 보이지 않는 글자만이거나 너무 길면 거절한다. 댓글 검사와 같은 기준을 쓴다.
pub fn clean_text(raw: &str) -> Result<String, ApiError> {
    let one_line: String = raw
        .chars()
        .map(|c| {
            if c == '\n' || c == '\r' || c == '\t' {
                ' '
            } else {
                c
            }
        })
        .collect();
    let t = one_line.trim();
    if t.chars().any(|c| c.is_control() || bidi_control(c)) {
        return Err(ApiError::BadRequest("채팅에 쓸 수 없는 글자가 있습니다"));
    }
    if t.chars().all(blank_like) {
        return Err(ApiError::BadRequest("채팅 내용을 입력하세요"));
    }
    if t.chars().count() > TEXT_MAX {
        return Err(ApiError::BadRequest("채팅은 200자 이하여야 합니다"));
    }
    Ok(t.to_string())
}

fn event(m: &Message) -> Event {
    Event::default()
        .event("chat")
        .data(serde_json::to_string(m).unwrap_or_default())
}

/// 보낸 곳(IP) 하나가 동시에 열 수 있는 채팅 스트림 수(#91). 한 집·학교처럼 IP를 나눠 쓰는 곳도 넉넉하게.
pub const STREAMS_PER_CLIENT: usize = 20;
/// 서버 전체에서 동시에 열 수 있는 채팅 스트림 수(#91 리뷰). 무료 서버 한 대가 버틸 만큼.
pub const STREAMS_TOTAL: usize = 2_000;

pub async fn stream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let found: Option<i64> = sqlx::query_scalar("SELECT id FROM broadcasts WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    found.ok_or(ApiError::NotFound)?;
    // 한 곳에서 스트림을 수천 개 열어 서버 메모리·파일 핸들을 채우지 못하게 한다(#91)
    let slot = state
        .chat
        .open_stream(
            &crate::guard::client_key(&headers),
            STREAMS_PER_CLIENT,
            STREAMS_TOTAL,
        )
        .ok_or(ApiError::TooManyRequestsBecause(
            "채팅 연결이 너무 많습니다. 다른 창을 닫고 다시 여세요",
        ))?;
    let (recent, rx) = state.chat.subscribe(id);
    // 처음에 짧은 주석 신호를 하나 보낸다. 운영 프록시(Caddy 압축)는 첫 글이 나올 때까지 응답 머리를 보내지 않아,
    // 방이 조용하면 브라우저가 "연결됨"을 늦게 안다(#86 리뷰). 화면은 주석을 무시한다.
    let hello = tokio_stream::iter([Event::default().comment("ok")]);
    let first = tokio_stream::iter(recent).map(|m| event(&m));
    // 너무 늦어 밀린 메시지(Lagged)는 건너뛴다
    let rest = BroadcastStream::new(rx)
        .filter_map(Result::ok)
        .map(|m| event(&m));
    // slot을 스트림 안에 넣어 둔다. 연결이 끊겨 스트림이 버려질 때 slot도 버려져 수가 줄어든다.
    let s = hello.chain(first).chain(rest).map(move |e| {
        let _ = &slot;
        Ok(e)
    });
    // 프록시가 오래 조용한 연결을 끊지 않게 15초마다 빈 신호를 보낸다
    Ok(Sse::new(s).keep_alive(KeepAlive::default()))
}

#[derive(Deserialize)]
pub struct ChatInput {
    pub text: String,
}

pub async fn send(
    State(state): State<AppState>,
    user: CurrentUser,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(input): Json<ChatInput>,
) -> Result<StatusCode, ApiError> {
    let text = clean_text(&input.text)?;
    let row: Option<(String, i32, i64, Vec<String>)> = sqlx::query_as(
        "SELECT status, slow_mode_secs, owner_id, banned_words FROM broadcasts WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;
    let Some((status, slow, owner, words)) = row else {
        return Err(ApiError::NotFound);
    };
    if status == "ended" {
        return Err(ApiError::Conflict("종료된 방송에는 채팅할 수 없습니다"));
    }
    // 방송 주인이 막은 사람은 쓸 수 없다(#87)
    if crate::moderation::banned(&state.db, id, user.id).await? {
        return Err(crate::moderation::BANNED);
    }
    // 방송 주인이 정한 금칙어가 들어간 글은 거절한다(#112). 주인 자신도 같다.
    if crate::moderation::has_banned_word(&text, &words) {
        return Err(ApiError::BadRequest(
            "방송 주인이 막은 단어가 들어 있습니다",
        ));
    }
    // 천천히 모드(#87). 방송 주인은 예외. 횟수 제한보다 먼저 봐서, 여기서 거절된 글은 1분 횟수를 쓰지 않는다(#94).
    if slow > 0 && owner != user.id && !state.chat.slow_ok(id, user.id, slow) {
        return Err(ApiError::TooManyRequestsBecause(
            "천천히 모드가 켜져 있습니다. 잠시 뒤에 다시 쓰세요",
        ));
    }
    // 도배를 막는다. 내용 검사를 먼저 해서 거절된 글은 횟수에 넣지 않는다(댓글과 같음).
    if !state.auth.chat_limiter.allow(&format!("user:{}", user.id)) {
        return Err(ApiError::TooManyRequests);
    }
    // 같은 곳(IP)에서 보내는 양도 센다(#126). 테스트 계정을 새로 만들어 사용자 몫을 피하는 도배를 막는다.
    // 사용자 몫을 먼저 본다. 반대면 자기 몫을 다 쓴 한 사람이 계속 보내 같은 IP의 다른 사람 몫까지 써 버린다(#126 리뷰).
    if let Some(l) = &state.auth.chat_ip_limiter
        && !l.allow(&format!("chat:{}", crate::guard::client_key(&headers)))
    {
        return Err(ApiError::TooManyRequests);
    }
    state
        .chat
        .publish(id, "chat", Some(user.id), &user.username, &text);
    Ok(StatusCode::NO_CONTENT)
}
