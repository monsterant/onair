//! 방송자 즐겨찾기 (#102).
//!
//! - 로그인한 사람만 한다(비로그인 401). 즐겨찾기 목록은 나만 본다.
//! - 즐겨찾기·해제는 여러 번 해도 결과가 같다(204).
//! - 목록에는 그 방송자가 지금 방송 중이면 그 방송(번호·제목)을 붙인다. 방송 중인 사람이 먼저.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Serialize;

use crate::{
    AppState,
    auth::{ApiError, CurrentUser},
};

/// 즐겨찾기 목록에 보여 줄 최대 수
const LIST_MAX: i64 = 200;

/// 즐겨찾는 방송자의 지금 방송
#[derive(Serialize, Debug)]
pub struct LiveBroadcast {
    pub id: i64,
    pub title: String,
}

/// 즐겨찾기 한 줄
#[derive(Serialize, Debug)]
pub struct Followed {
    pub user_id: i64,
    pub username: String,
    /// 지금 방송 중이면 그 방송, 아니면 null
    pub live: Option<LiveBroadcast>,
}

/// 즐겨찾기. 이미 했으면 그대로 204. 자기 자신 400, 없는 사용자 404.
pub async fn follow(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(streamer_id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    if streamer_id == user.id {
        return Err(ApiError::BadRequest("자기 자신은 즐겨찾기할 수 없습니다"));
    }
    let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM users WHERE id = $1")
        .bind(streamer_id)
        .fetch_optional(&state.db)
        .await?;
    exists.ok_or(ApiError::NotFound)?;
    // INSERT ... SELECT: 확인과 저장 사이에 그 계정이 지워져도 외래 키 오류(500) 대신 0행이 된다
    sqlx::query(
        "INSERT INTO follows (user_id, streamer_id)
         SELECT $1, u.id FROM users u WHERE u.id = $2
         ON CONFLICT DO NOTHING",
    )
    .bind(user.id)
    .bind(streamer_id)
    .execute(&state.db)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 즐겨찾기 해제. 안 했어도 204.
pub async fn unfollow(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(streamer_id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    sqlx::query("DELETE FROM follows WHERE user_id = $1 AND streamer_id = $2")
        .bind(user.id)
        .bind(streamer_id)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 내 즐겨찾기 목록. 방송 중인 사람 먼저, 그다음 이름순.
pub async fn list(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<Vec<Followed>>, ApiError> {
    // 한 사람이 방송을 여러 개 동시에 하고 있으면 가장 최근에 시작한 것 하나만
    let rows: Vec<(i64, String, Option<i64>, Option<String>)> = sqlx::query_as(
        "SELECT u.id, u.username, b.id, b.title
           FROM follows f
           JOIN users u ON u.id = f.streamer_id
           LEFT JOIN LATERAL (
               SELECT id, title FROM broadcasts
                WHERE owner_id = u.id AND status = 'live'
                ORDER BY started_at DESC NULLS LAST, id DESC
                LIMIT 1
           ) b ON true
          WHERE f.user_id = $1
          ORDER BY (b.id IS NOT NULL) DESC, u.username ASC
          LIMIT $2",
    )
    .bind(user.id)
    .bind(LIST_MAX)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|(user_id, username, live_id, live_title)| Followed {
                user_id,
                username,
                live: live_id
                    .zip(live_title)
                    .map(|(id, title)| LiveBroadcast { id, title }),
            })
            .collect(),
    ))
}
