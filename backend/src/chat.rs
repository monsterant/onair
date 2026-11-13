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
    collections::{HashMap, VecDeque},
    convert::Infallible,
    sync::{Arc, Mutex},
};

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
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
    pub username: String,
    pub text: String,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

struct Room {
    tx: broadcast::Sender<Arc<Message>>,
    recent: VecDeque<Arc<Message>>,
}

#[derive(Default)]
struct Inner {
    rooms: HashMap<i64, Room>,
    next_id: i64,
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
        username: &str,
        text: &str,
    ) -> Arc<Message> {
        let mut inner = self.0.lock().unwrap();
        inner.next_id += 1;
        let msg = Arc::new(Message {
            id: inner.next_id,
            kind,
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

    /// 방송이 지워지면 그 방을 없앤다(메모리가 쌓이지 않게). 듣던 연결은 끝난다.
    pub fn remove(&self, broadcast_id: i64) {
        self.0.lock().unwrap().rooms.remove(&broadcast_id);
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

pub async fn stream(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let found: Option<i64> = sqlx::query_scalar("SELECT id FROM broadcasts WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    found.ok_or(ApiError::NotFound)?;
    let (recent, rx) = state.chat.subscribe(id);
    let first = tokio_stream::iter(recent);
    // 너무 늦어 밀린 메시지(Lagged)는 건너뛴다
    let rest = BroadcastStream::new(rx).filter_map(Result::ok);
    let s = first.chain(rest).map(|m| Ok(event(&m)));
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
    Path(id): Path<i64>,
    Json(input): Json<ChatInput>,
) -> Result<StatusCode, ApiError> {
    let text = clean_text(&input.text)?;
    let status: Option<String> = sqlx::query_scalar("SELECT status FROM broadcasts WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    match status.as_deref() {
        None => return Err(ApiError::NotFound),
        Some("ended") => return Err(ApiError::Conflict("종료된 방송에는 채팅할 수 없습니다")),
        _ => {}
    }
    // 도배를 막는다. 내용 검사를 먼저 해서 거절된 글은 횟수에 넣지 않는다(댓글과 같음).
    if !state.auth.chat_limiter.allow(&format!("user:{}", user.id)) {
        return Err(ApiError::TooManyRequests);
    }
    state.chat.publish(id, "chat", &user.username, &text);
    Ok(StatusCode::NO_CONTENT)
}
