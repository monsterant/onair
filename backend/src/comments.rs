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
    http::{HeaderMap, StatusCode},
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{
    AppState,
    auth::{ApiError, CurrentUser},
    guard::client_key,
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

/// 내용 검사: 앞뒤 공백을 떼고 1~300자. 줄바꿈 말고 제어 문자는 거절한다.
fn clean_body(raw: &str) -> Result<String, ApiError> {
    let b = raw.trim();
    if b.chars().any(|c| c.is_control() && c != '\n') {
        return Err(ApiError::BadRequest("댓글에 쓸 수 없는 글자가 있습니다"));
    }
    if b.chars().all(|c| {
        c.is_whitespace() || matches!(c, '\u{200B}'..='\u{200D}' | '\u{2060}' | '\u{FEFF}')
    }) {
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
    headers: HeaderMap,
    Path(broadcast_id): Path<i64>,
    Json(input): Json<CommentInput>,
) -> Result<(StatusCode, Json<Comment>), ApiError> {
    // 도배를 막으려고 로그인과 같은 IP당 1분 횟수 제한을 함께 쓴다
    if !state.auth.limiter.allow(&client_key(&headers)) {
        return Err(ApiError::TooManyRequests);
    }
    let body = clean_body(&input.body)?;
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
        assert!(clean_body("가운데\t탭").is_err());
        assert!(clean_body(&"가".repeat(300)).is_ok());
        assert!(clean_body(&"가".repeat(301)).is_err());
    }
}
