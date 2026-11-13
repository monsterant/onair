//! 방송별 댓글 (#37, 공모 선택 요구사항 1).
//!
//! 누가 무엇을 할 수 있나
//! | 동작 | 비로그인 | 작성자 | 다른 사용자 |
//! |---|---|---|---|
//! | 목록 | 가능 | 가능 | 가능 |
//! | 작성 | 401 | 가능 | 가능(자기 댓글) |
//! | 수정, 삭제 | 401 | 가능 | 403 |
//!
//! 화면은 몇 초마다 목록을 다시 불러온다(폴링). 웹소켓 채팅보다 단순해서 서버 한 대에서 안전하다.

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

/// 댓글 한 개의 최대 글자 수
pub const BODY_MAX: usize = 300;
/// 목록에 보여 주는 최근 댓글 수
pub const LIST_MAX: i64 = 200;

#[derive(Serialize, Debug, sqlx::FromRow)]
pub struct Comment {
    pub id: i64,
    pub broadcast_id: i64,
    pub author_id: i64,
    pub author_username: String,
    pub body: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub updated_at: Option<OffsetDateTime>,
}

/// 댓글 조회 SQL 앞부분. sqlx는 SQL을 문자열 상수로만 받으므로 concat!으로 붙인다.
macro_rules! select_comment {
    ($rest:literal) => {
        concat!(
            "SELECT c.id, c.broadcast_id, c.author_id, u.username AS author_username,
       c.body, c.created_at, c.updated_at
  FROM comments c JOIN users u ON u.id = c.author_id ",
            $rest
        )
    };
}

#[derive(Deserialize)]
pub struct CommentInput {
    pub body: String,
}

/// 화면에 보이지 않거나 빈칸처럼 보이는 글자. 이것만으로 된 댓글은 빈 댓글로 본다(#37 리뷰).
pub(crate) fn blank_like(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '\u{200B}'..='\u{200F}'          // 폭 없는 공백, 방향 표시
                | '\u{2060}'..='\u{2064}'    // 단어 이음 등
                | '\u{FEFF}'                 // BOM
                | '\u{115F}' | '\u{1160}' | '\u{3164}' | '\u{FFA0}' // 한글 채움 문자
                | '\u{2800}'                 // 빈 점자
                | '\u{180E}'
                | '\u{00AD}'                 // 보이지 않는 줄바꿈 하이픈
                | '\u{034F}'                 // 글자 묶음 이음
        )
}

/// 글자 방향을 바꾸는 문자. 뒤따르는 글을 거꾸로 보이게 해서 다른 내용으로 속일 수 있다(#37 리뷰).
pub(crate) fn bidi_control(c: char) -> bool {
    matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// 내용 검사: 줄바꿈을 \n으로 맞추고 앞뒤 공백을 뗀 뒤 1~300자.
/// 줄바꿈·탭 말고 제어 문자와 글자 방향 문자는 거절한다.
fn clean_body(raw: &str) -> Result<String, ApiError> {
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    let b = normalized.trim();
    if b.chars()
        .any(|c| (c.is_control() && c != '\n' && c != '\t') || bidi_control(c))
    {
        return Err(ApiError::BadRequest("댓글에 쓸 수 없는 글자가 있습니다"));
    }
    if b.chars().all(blank_like) {
        return Err(ApiError::BadRequest("댓글 내용을 입력하세요"));
    }
    if b.chars().count() > BODY_MAX {
        return Err(ApiError::BadRequest("댓글은 300자 이하여야 합니다"));
    }
    Ok(b.to_string())
}

async fn broadcast_exists(state: &AppState, id: i64) -> Result<(), ApiError> {
    let found: Option<i64> = sqlx::query_scalar("SELECT id FROM broadcasts WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    found.map(|_| ()).ok_or(ApiError::NotFound)
}

async fn find(state: &AppState, id: i64) -> Result<Comment, ApiError> {
    sqlx::query_as::<_, Comment>(select_comment!("WHERE c.id = $1"))
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)
}

/// 작성자인지 확인한다. 아니면 403, 댓글이 없으면 404.
async fn authored(state: &AppState, id: i64, user: &CurrentUser) -> Result<Comment, ApiError> {
    let c = find(state, id).await?;
    if c.author_id != user.id {
        return Err(ApiError::Forbidden("댓글 작성자만 할 수 있습니다"));
    }
    Ok(c)
}

/// 방송의 댓글 목록. 누구나 본다. 최근 200개를 오래된 것부터 돌려준다.
pub async fn list(
    State(state): State<AppState>,
    Path(broadcast_id): Path<i64>,
) -> Result<Json<Vec<Comment>>, ApiError> {
    broadcast_exists(&state, broadcast_id).await?;
    let mut rows = sqlx::query_as::<_, Comment>(select_comment!(
        "WHERE c.broadcast_id = $1 ORDER BY c.id DESC LIMIT $2"
    ))
    .bind(broadcast_id)
    .bind(LIST_MAX)
    .fetch_all(&state.db)
    .await?;
    rows.reverse();
    Ok(Json(rows))
}

pub async fn create(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(broadcast_id): Path<i64>,
    Json(input): Json<CommentInput>,
) -> Result<(StatusCode, Json<Comment>), ApiError> {
    let body = clean_body(&input.body)?;
    // 방송 주인이 막은 사람은 댓글도 쓸 수 없다(#87)
    if crate::moderation::banned(&state.db, broadcast_id, user.id).await? {
        return Err(crate::moderation::BANNED);
    }
    // 도배를 막으려고 사용자 하나당 1분 횟수 제한을 건다. 로그인 몫과는 따로 센다(#37 리뷰).
    // 내용 검사를 먼저 해서, 거절된 글은 횟수에 넣지 않는다.
    if !state
        .auth
        .comment_limiter
        .allow(&format!("user:{}", user.id))
    {
        return Err(ApiError::TooManyRequests);
    }
    // 방송이 없으면 외래 키 오류(500) 대신 404. 확인과 저장 사이에 방송이 지워지면 INSERT ... SELECT가 0행이라 404다.
    let id: Option<i64> = sqlx::query_scalar(
        "INSERT INTO comments (broadcast_id, author_id, body)
         SELECT b.id, $2, $3 FROM broadcasts b WHERE b.id = $1
         RETURNING id",
    )
    .bind(broadcast_id)
    .bind(user.id)
    .bind(body)
    .fetch_optional(&state.db)
    .await?;
    let id = id.ok_or(ApiError::NotFound)?;
    Ok((StatusCode::CREATED, Json(find(&state, id).await?)))
}

pub async fn update(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(input): Json<CommentInput>,
) -> Result<Json<Comment>, ApiError> {
    authored(&state, id, &user).await?;
    let body = clean_body(&input.body)?;
    // 작성자 조건을 UPDATE에도 넣어서, 확인과 수정 사이에 무슨 일이 있어도 남의 댓글을 고치지 않게 한다
    sqlx::query(
        "UPDATE comments SET body = $1, updated_at = now() WHERE id = $2 AND author_id = $3",
    )
    .bind(body)
    .bind(id)
    .bind(user.id)
    .execute(&state.db)
    .await?;
    Ok(Json(find(&state, id).await?))
}

pub async fn delete(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    authored(&state, id, &user).await?;
    sqlx::query("DELETE FROM comments WHERE id = $1 AND author_id = $2")
        .bind(id)
        .bind(user.id)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_rules() {
        assert_eq!(clean_body("  안녕하세요 ").unwrap(), "안녕하세요");
        assert_eq!(clean_body("첫 줄\n둘째 줄").unwrap(), "첫 줄\n둘째 줄");
        assert!(clean_body("").is_err());
        assert!(clean_body("   ").is_err());
        assert!(clean_body("\u{200B}\u{FEFF}").is_err());
        assert!(clean_body("널\u{0}글자").is_err());
        assert!(clean_body("탭\t").is_ok()); // 앞뒤 공백(탭 포함)은 잘라 낸다
        assert_eq!(clean_body("가운데\t탭").unwrap(), "가운데\t탭");
        // Windows 줄바꿈(\r\n)과 \r만 쓴 줄바꿈은 \n으로 맞춘다
        assert_eq!(clean_body("a\r\nb\rc").unwrap(), "a\nb\nc");
        // 빈칸처럼 보이는 글자만 있으면 빈 댓글
        for blank in [
            "\u{3164}",
            "\u{2800}\u{2800}",
            "\u{115F}\u{1160}",
            "\u{200E}",
            "\u{00AD}\u{034F}",
            "\u{2061}\u{2062}\u{180E}",
        ] {
            assert!(clean_body(blank).is_err(), "{blank:?}");
        }
        // 글자 방향을 뒤집는 문자는 거절
        assert!(clean_body("abc\u{202E}def").is_err());
        assert!(clean_body("abc\u{2067}def").is_err());
        assert!(clean_body(&"가".repeat(300)).is_ok());
        assert!(clean_body(&"가".repeat(301)).is_err());
    }
}
