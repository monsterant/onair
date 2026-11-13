//! 시청자 제한·천천히 모드 (#87).
//!
//! - 방송 주인만 한다. 다른 사람은 403, 비로그인은 401(서버에서 소유자 검사).
//! - 제한: 특정 사용자의 그 방송 채팅·댓글 쓰기를 10분 또는 해제할 때까지 막는다. 읽기는 그대로 된다.
//!   제한 목록은 DB(broadcast_bans)에 있어 서버를 다시 켜도 남는다.
//! - 천천히 모드: 사람마다 N초(5·10·30)에 한 번만 채팅. 0이면 끔. 방송 주인은 예외.
//!   "마지막으로 쓴 시각"은 서버 메모리(chat::Hub)에 둔다. 서버를 다시 켜면 처음부터 센다.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{
    AppState,
    auth::{ApiError, CurrentUser},
};

/// 정해진 제한 시간(분). None은 해제할 때까지.
pub const BAN_MINUTES: i64 = 10;
/// 고를 수 있는 천천히 모드(초)
pub const SLOW_CHOICES: [i32; 4] = [0, 5, 10, 30];

/// 이 사용자가 지금 이 방송에서 막혀 있는지
pub async fn banned(db: &sqlx::PgPool, broadcast_id: i64, user_id: i64) -> Result<bool, ApiError> {
    let hit: Option<i32> = sqlx::query_scalar(
        "SELECT 1 FROM broadcast_bans
          WHERE broadcast_id = $1 AND user_id = $2 AND (until IS NULL OR until > now())",
    )
    .bind(broadcast_id)
    .bind(user_id)
    .fetch_optional(db)
    .await?;
    Ok(hit.is_some())
}

/// 막힌 사람이 쓰려 할 때의 오류
pub const BANNED: ApiError = ApiError::Forbidden("방송자가 이 방송에서 글쓰기를 막았습니다");

/// 방송 주인인지 확인한다. 아니면 403, 방송이 없으면 404.
async fn owner_of(state: &AppState, id: i64, user: &CurrentUser) -> Result<(), ApiError> {
    let owner: Option<i64> = sqlx::query_scalar("SELECT owner_id FROM broadcasts WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    match owner {
        None => Err(ApiError::NotFound),
        Some(o) if o != user.id => Err(ApiError::Forbidden("방송 소유자만 할 수 있습니다")),
        Some(_) => Ok(()),
    }
}

#[derive(Serialize, Debug, sqlx::FromRow)]
pub struct Ban {
    pub user_id: i64,
    pub username: String,
    /// 이 시각까지. null이면 해제할 때까지.
    #[serde(with = "time::serde::rfc3339::option")]
    pub until: Option<OffsetDateTime>,
}

/// 지금 막혀 있는 사람들(끝난 제한은 빼고). 방송 주인만 본다.
pub async fn list(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<Vec<Ban>>, ApiError> {
    owner_of(&state, id, &user).await?;
    let rows = sqlx::query_as::<_, Ban>(
        "SELECT b.user_id, u.username, b.until FROM broadcast_bans b JOIN users u ON u.id = b.user_id
          WHERE b.broadcast_id = $1 AND (b.until IS NULL OR b.until > now())
          ORDER BY b.created_at",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct BanInput {
    /// true면 10분, false면 해제할 때까지
    #[serde(default)]
    pub ten_minutes: bool,
}

/// 막는다(이미 막혀 있으면 기간을 새로 정한다). 자기 자신은 막을 수 없다.
pub async fn ban(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, target)): Path<(i64, i64)>,
    Json(input): Json<BanInput>,
) -> Result<StatusCode, ApiError> {
    owner_of(&state, id, &user).await?;
    if target == user.id {
        return Err(ApiError::BadRequest("자기 자신은 제한할 수 없습니다"));
    }
    let minutes: Option<i64> = input.ten_minutes.then_some(BAN_MINUTES);
    // 대상 사용자가 없으면(외래 키) 0행이라 404
    let done = sqlx::query(
        "INSERT INTO broadcast_bans (broadcast_id, user_id, until)
         SELECT $1, u.id, CASE WHEN $3::bigint IS NULL THEN NULL ELSE now() + make_interval(mins => $3::int) END
           FROM users u WHERE u.id = $2
         ON CONFLICT (broadcast_id, user_id)
         DO UPDATE SET until = EXCLUDED.until, created_at = now()",
    )
    .bind(id)
    .bind(target)
    .bind(minutes)
    .execute(&state.db)
    .await?
    .rows_affected();
    if done == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

/// 제한을 푼다. 없어도 204(여러 번 불러도 결과가 같다).
pub async fn unban(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, target)): Path<(i64, i64)>,
) -> Result<StatusCode, ApiError> {
    owner_of(&state, id, &user).await?;
    sqlx::query("DELETE FROM broadcast_bans WHERE broadcast_id = $1 AND user_id = $2")
        .bind(id)
        .bind(target)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 금칙어 최대 개수와 한 단어 최대 글자 수(#112)
pub const BANNED_WORDS_MAX: usize = 30;
pub const BANNED_WORD_LEN: usize = 20;
/// 받는 목록 자체의 최대 길이(빈 칸·같은 단어를 거르기 전, #117)
pub const BANNED_WORDS_INPUT_MAX: usize = 100;

#[derive(Deserialize)]
pub struct WordsInput {
    pub words: Vec<String>,
}

/// 금칙어 목록을 다듬는다: 앞뒤 빈칸을 빼고 소문자로, 빈 것은 버리고, 같은 것은 하나만(처음 순서 유지).
/// 30개·20자를 넘거나 제어 문자·글자 방향 문자가 있으면 400.
pub fn clean_words(raw: &[String]) -> Result<Vec<String>, ApiError> {
    // 아주 긴 목록은 다듬기 전에 거절한다. 같은 단어 거르기가 개수의 제곱만큼 느려 서버를 묶을 수 있다(#117).
    if raw.len() > BANNED_WORDS_INPUT_MAX {
        return Err(ApiError::BadRequest("금칙어는 30개까지입니다"));
    }
    let mut out: Vec<String> = Vec::new();
    for w in raw {
        let w = w.trim();
        // 글자 수는 소문자로 바꾸기 전에 센다. 터키어 İ처럼 바꾸면 길어지는 글자가 있다(#117).
        if w.chars().count() > BANNED_WORD_LEN {
            return Err(ApiError::BadRequest("금칙어는 한 단어에 20자까지입니다"));
        }
        let w = w.to_lowercase();
        if w.chars()
            .any(|c| c.is_control() || crate::comments::bidi_control(c))
        {
            return Err(ApiError::BadRequest("금칙어에 쓸 수 없는 글자가 있습니다"));
        }
        if w.chars().all(crate::comments::blank_like) {
            continue;
        }
        if !out.contains(&w) {
            out.push(w);
        }
    }
    if out.len() > BANNED_WORDS_MAX {
        return Err(ApiError::BadRequest("금칙어는 30개까지입니다"));
    }
    Ok(out)
}

/// 채팅 글에 금칙어가 들어 있는지(대소문자 무시, #112)
pub fn has_banned_word(text: &str, words: &[String]) -> bool {
    let t = text.to_lowercase();
    words
        .iter()
        .any(|w| !w.is_empty() && t.contains(w.as_str()))
}

/// 금칙어 목록(방송 주인만 본다, #112)
pub async fn get_words(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<Vec<String>>, ApiError> {
    owner_of(&state, id, &user).await?;
    let words: Vec<String> =
        sqlx::query_scalar("SELECT banned_words FROM broadcasts WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.db)
            .await?
            .ok_or(ApiError::NotFound)?;
    Ok(Json(words))
}

/// 금칙어 목록을 통째로 바꾼다(빈 목록이면 모두 지움, #112). 방송 주인만.
pub async fn set_words(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(input): Json<WordsInput>,
) -> Result<Json<Vec<String>>, ApiError> {
    owner_of(&state, id, &user).await?;
    let words = clean_words(&input.words)?;
    sqlx::query("UPDATE broadcasts SET banned_words = $2 WHERE id = $1")
        .bind(id)
        .bind(&words)
        .execute(&state.db)
        .await?;
    Ok(Json(words))
}

/// 공지 최대 글자 수(#109)
pub const NOTICE_MAX: usize = 100;

#[derive(Deserialize)]
pub struct NoticeInput {
    pub text: String,
}

/// 공지를 다듬는다. 한 줄로 만들고(줄바꿈·탭은 빈칸), 앞뒤 빈칸을 뺀다.
/// 보이지 않는 글자만 있으면 빈 공지(내림). 제어 문자·글자 방향을 바꾸는 문자는 400.
pub fn clean_notice(raw: &str) -> Result<String, ApiError> {
    // \r\n은 빈칸 하나로. 줄 구분 문자(U+2028·U+2029)도 줄바꿈처럼 빈칸으로(#115)
    let one_line: String = raw
        .replace("\r\n", " ")
        .chars()
        .map(|c| {
            if matches!(c, '\n' | '\r' | '\t' | '\u{2028}' | '\u{2029}') {
                ' '
            } else {
                c
            }
        })
        .collect();
    let t = one_line.trim();
    if t.chars()
        .any(|c| c.is_control() || crate::comments::bidi_control(c))
    {
        return Err(ApiError::BadRequest("공지에 쓸 수 없는 글자가 있습니다"));
    }
    if t.chars().all(crate::comments::blank_like) {
        return Ok(String::new());
    }
    if t.chars().count() > NOTICE_MAX {
        return Err(ApiError::BadRequest("공지는 100자 이하여야 합니다"));
    }
    Ok(t.to_string())
}

/// 채팅 칸 위 공지를 정한다(#109). 빈 값이면 내린다. 방송 주인만.
pub async fn set_notice(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(input): Json<NoticeInput>,
) -> Result<StatusCode, ApiError> {
    owner_of(&state, id, &user).await?;
    let text = clean_notice(&input.text)?;
    sqlx::query("UPDATE broadcasts SET notice = $2 WHERE id = $1")
        .bind(id)
        .bind(text)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct SlowInput {
    pub secs: i32,
}

/// 천천히 모드를 정한다(0·5·10·30초).
pub async fn set_slow(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(input): Json<SlowInput>,
) -> Result<StatusCode, ApiError> {
    owner_of(&state, id, &user).await?;
    if !SLOW_CHOICES.contains(&input.secs) {
        return Err(ApiError::BadRequest(
            "천천히 모드는 0·5·10·30초 중에서 고르세요",
        ));
    }
    sqlx::query("UPDATE broadcasts SET slow_mode_secs = $2 WHERE id = $1")
        .bind(id)
        .bind(input.secs)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
