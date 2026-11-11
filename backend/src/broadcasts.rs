//! 방송 등록·수정·삭제·목록·상세 (#3)와 소유자 검사 (#4).
//!
//! 누가 무엇을 할 수 있나
//! | 동작 | 비로그인 | 소유자 | 다른 사용자 |
//! |---|---|---|---|
//! | 목록, 상세 | 가능 | 가능 | 가능 |
//! | 등록 | 401 | 가능 | 가능(자기 방송) |
//! | 수정, 삭제, 종료, 송출 주소 보기 | 401 | 가능 | 403 |

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{
    AppState,
    auth::{ApiError, CurrentUser},
    guard::client_key,
};

pub const TITLE_MAX: usize = 100;
pub const DESCRIPTION_MAX: usize = 1000;

/// 화면에 내보내는 방송 정보. 송출 키는 들어 있지 않다.
#[derive(Serialize, Debug, sqlx::FromRow)]
pub struct Broadcast {
    pub id: i64,
    pub title: String,
    pub description: String,
    pub status: String,
    pub stream_name: String,
    pub owner_id: i64,
    pub owner_username: String,
    #[serde(with = "time::serde::rfc3339::option")]
    pub started_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub ended_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// 방송 조회 SQL 앞부분. sqlx는 SQL을 문자열 상수로만 받으므로(SQL 끼워 넣기 방지) concat!으로 붙인다.
macro_rules! select_broadcast {
    ($rest:literal) => {
        concat!(
            "SELECT b.id, b.title, b.description, b.status, b.stream_name, b.owner_id,
       u.username AS owner_username, b.started_at, b.ended_at, b.created_at
  FROM broadcasts b JOIN users u ON u.id = b.owner_id ",
            $rest
        )
    };
}

// ---------- 입력 검사 ----------

/// 화면에 보이지 않는 글자(폭 없는 공백 등). 이것만으로 된 제목은 빈 제목으로 본다.
fn invisible(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\u{200B}'..='\u{200D}' | '\u{2060}' | '\u{FEFF}')
}

fn clean_title(raw: &str) -> Result<String, ApiError> {
    let t = raw.trim();
    // 제어 문자(NUL 등)는 DB가 받지 않거나 화면을 깨뜨린다. 500 대신 400으로 거절한다(#27 V8).
    if t.chars().any(char::is_control) {
        return Err(ApiError::BadRequest("제목에 쓸 수 없는 글자가 있습니다"));
    }
    if t.chars().all(invisible) {
        return Err(ApiError::BadRequest("제목을 입력하세요"));
    }
    if t.chars().count() > TITLE_MAX {
        return Err(ApiError::BadRequest("제목은 100자 이하여야 합니다"));
    }
    Ok(t.to_string())
}

fn clean_description(raw: &str) -> Result<String, ApiError> {
    let d = raw.trim();
    // 설명은 줄바꿈과 탭만 허락하고 나머지 제어 문자는 거절한다.
    if d.chars()
        .any(|c| c.is_control() && c != '\n' && c != '\r' && c != '\t')
    {
        return Err(ApiError::BadRequest("설명에 쓸 수 없는 글자가 있습니다"));
    }
    if d.chars().count() > DESCRIPTION_MAX {
        return Err(ApiError::BadRequest("설명은 1000자 이하여야 합니다"));
    }
    Ok(d.to_string())
}

/// 한 사람이 동시에 가질 수 있는 끝나지 않은(대기·방송 중) 방송 수(#27 V7)
pub const OPEN_BROADCASTS_MAX: i64 = 5;

/// 임의의 바이트를 16진수 글자로. 이름·키 만들기에 쓴다.
pub(crate) fn random_hex(bytes: usize) -> Result<String, ApiError> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|_| ApiError::Internal)?;
    Ok(hex::encode(buf))
}

// ---------- 조회 ----------

pub async fn list(State(state): State<AppState>) -> Result<Json<Vec<Broadcast>>, ApiError> {
    // 방송 중을 먼저, 그다음 최신순. 화면 한 장에 충분한 100개까지.
    let rows = sqlx::query_as::<_, Broadcast>(select_broadcast!(
        "ORDER BY (b.status = 'live') DESC, b.created_at DESC, b.id DESC LIMIT 100"
    ))
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows))
}

async fn find(state: &AppState, id: i64) -> Result<Broadcast, ApiError> {
    sqlx::query_as::<_, Broadcast>(select_broadcast!("WHERE b.id = $1"))
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)
}

pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Broadcast>, ApiError> {
    Ok(Json(find(&state, id).await?))
}

/// 소유자인지 확인한다. 아니면 403, 방송이 없으면 404.
async fn owned(state: &AppState, id: i64, user: &CurrentUser) -> Result<Broadcast, ApiError> {
    let b = find(state, id).await?;
    if b.owner_id != user.id {
        return Err(ApiError::Forbidden("방송 소유자만 할 수 있습니다"));
    }
    Ok(b)
}

// ---------- 등록·수정·삭제 ----------

#[derive(Deserialize)]
pub struct NewBroadcast {
    pub title: String,
    #[serde(default)]
    pub description: String,
}

pub async fn create(
    State(state): State<AppState>,
    user: CurrentUser,
    headers: HeaderMap,
    Json(input): Json<NewBroadcast>,
) -> Result<(StatusCode, Json<Broadcast>), ApiError> {
    // 만들기도 IP 하나당 1분 횟수 제한을 건다(#27 V7: 대량 생성으로 목록 밀어내기 방지).
    // 로그인 몫과는 따로 센다. 같이 세면 같은 와이파이의 심사자들이 방송을 만들다 로그인이 막힐 수 있다(보안 점검 L3).
    if !state
        .auth
        .comment_limiter
        .allow(&format!("create:{}", client_key(&headers)))
    {
        return Err(ApiError::TooManyRequests);
    }
    let title = clean_title(&input.title)?;
    let description = clean_description(&input.description)?;
    let open: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM broadcasts WHERE owner_id = $1 AND status <> 'ended'",
    )
    .bind(user.id)
    .fetch_one(&state.db)
    .await?;
    if open >= OPEN_BROADCASTS_MAX {
        return Err(ApiError::Conflict(
            "끝나지 않은 방송은 5개까지 만들 수 있습니다. 쓰지 않는 방송을 종료하거나 삭제하세요",
        ));
    }
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO broadcasts (owner_id, title, description, stream_name, stream_key)
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(user.id)
    .bind(&title)
    .bind(&description)
    .bind(random_hex(8)?)
    .bind(random_hex(32)?)
    .fetch_one(&state.db)
    .await?;
    Ok((StatusCode::CREATED, Json(find(&state, id).await?)))
}

#[derive(Deserialize)]
pub struct EditBroadcast {
    pub title: Option<String>,
    pub description: Option<String>,
}

pub async fn update(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(input): Json<EditBroadcast>,
) -> Result<Json<Broadcast>, ApiError> {
    owned(&state, id, &user).await?;
    let title = input.title.as_deref().map(clean_title).transpose()?;
    let description = input
        .description
        .as_deref()
        .map(clean_description)
        .transpose()?;
    // 소유자 조건을 UPDATE에도 넣어서, 확인과 수정 사이에 소유자가 바뀌어도 남의 방송을 고치지 않게 한다.
    sqlx::query(
        "UPDATE broadcasts SET title = COALESCE($1, title), description = COALESCE($2, description),
                updated_at = now()
          WHERE id = $3 AND owner_id = $4",
    )
    .bind(title)
    .bind(description)
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
    let b = owned(&state, id, &user).await?;
    // 녹화 중이면 먼저 멈춘다(#47 리뷰). 행이 지워지면 맞춰 보기가 이 녹화를 다시 찾지 못한다. 남은 파일은 정리 작업이 지운다.
    let recording: Vec<String> = sqlx::query_scalar(
        "SELECT record_id FROM recordings WHERE broadcast_id = $1 AND status = 'recording'",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    for rid in recording {
        state.ome.stop_record(&rid).await;
    }
    // 행을 먼저 지우고 그다음 송출을 끊는다. 상태와 상관없이 항상 끊는다(#27 V4).
    // 지운 뒤에는 웹훅이 이름을 찾지 못해 다시 송출할 수 없다. 끊기가 실패해도 맞춰 보기가 DB에 없는 스트림을 끊는다.
    sqlx::query("DELETE FROM broadcasts WHERE id = $1 AND owner_id = $2")
        .bind(id)
        .bind(user.id)
        .execute(&state.db)
        .await?;
    state.ome.stop_stream(&b.stream_name).await;
    Ok(StatusCode::NO_CONTENT)
}

/// 소유자가 방송을 끝낸다. 상태를 `ended`로 바꾸고 OME 송출을 끊는다. 끝난 방송은 다시 송출할 수 없다.
/// 이미 끝난 방송에 다시 불러도 결과는 같다.
pub async fn end(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<Broadcast>, ApiError> {
    let b = owned(&state, id, &user).await?;
    sqlx::query(
        "UPDATE broadcasts SET status = 'ended', ended_at = COALESCE(ended_at, now()), updated_at = now()
          WHERE id = $1 AND owner_id = $2",
    )
    .bind(id)
    .bind(user.id)
    .execute(&state.db)
    .await?;
    // 상태를 먼저 바꾼 뒤 끊는다. 끊긴 뒤 송출 프로그램이 다시 붙으려 해도 웹훅이 ended를 보고 거절한다.
    state.ome.stop_stream(&b.stream_name).await;
    Ok(Json(find(&state, id).await?))
}

// ---------- 송출 주소 ----------

#[derive(Serialize)]
pub struct PublishInfo {
    pub stream_name: String,
    pub stream_key: String,
    /// 브라우저 송출(WHIP) 경로. 화면이 자기 주소(https://도메인) 뒤에 붙여 쓴다.
    pub whip_path: String,
}

pub async fn publish_info(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<([(header::HeaderName, &'static str); 1], Json<PublishInfo>), ApiError> {
    owned(&state, id, &user).await?;
    let (stream_name, stream_key): (String, String) =
        sqlx::query_as("SELECT stream_name, stream_key FROM broadcasts WHERE id = $1")
            .bind(id)
            .fetch_one(&state.db)
            .await?;
    let whip_path = format!("/app/{stream_name}?direction=whip&key={stream_key}");
    // 송출 키가 든 응답은 브라우저·중간 캐시에 남기지 않는다(#27 V20)
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(PublishInfo {
            stream_name,
            stream_key,
            whip_path,
        }),
    ))
}

/// 송출 키를 새로 만든다(#28). 소유자만. 키가 샜을 때 방송을 새로 만들지 않고 바꿀 수 있다.
/// - 옛 키로 지금 송출 중이면 끊는다. 다시 붙으려 해도 웹훅이 새 키와 비교해 거절한다.
/// - 종료된 방송은 송출할 수 없으니 바꾸지 않는다(409).
pub async fn rotate_key(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<([(header::HeaderName, &'static str); 1], Json<PublishInfo>), ApiError> {
    let b = owned(&state, id, &user).await?;
    if b.status == "ended" {
        return Err(ApiError::Conflict(
            "종료된 방송은 송출 키를 바꿀 수 없습니다",
        ));
    }
    let key = random_hex(32)?;
    // 소유자 조건을 UPDATE에도 넣는다(확인과 바꾸기 사이에 무슨 일이 있어도 남의 방송을 바꾸지 않게)
    let changed = sqlx::query(
        "UPDATE broadcasts SET stream_key = $1, updated_at = now()
          WHERE id = $2 AND owner_id = $3 AND status <> 'ended'",
    )
    .bind(&key)
    .bind(id)
    .bind(user.id)
    .execute(&state.db)
    .await?;
    if changed.rows_affected() == 0 {
        // 그 사이 지워졌으면 404(find가 돌려준다), 종료됐으면 409(#28 리뷰)
        find(&state, id).await?;
        return Err(ApiError::Conflict(
            "종료된 방송은 송출 키를 바꿀 수 없습니다",
        ));
    }
    // 옛 키로 붙어 있는 송출을 끊는다
    state.ome.stop_stream(&b.stream_name).await;
    let whip_path = format!("/app/{}?direction=whip&key={key}", b.stream_name);
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(PublishInfo {
            stream_name: b.stream_name,
            stream_key: key,
            whip_path,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_and_description_rules() {
        assert!(clean_title("   ").is_err());
        assert_eq!(clean_title("  첫 방송 ").unwrap(), "첫 방송");
        assert!(clean_title(&"가".repeat(100)).is_ok());
        assert!(clean_title(&"가".repeat(101)).is_err());
        assert_eq!(clean_description("").unwrap(), "");
        assert!(clean_description(&"a".repeat(1000)).is_ok());
        assert!(clean_description(&"a".repeat(1001)).is_err());
    }

    #[test]
    fn random_names_are_hex_and_unique() {
        let a = random_hex(8).unwrap();
        assert_eq!(a.len(), 16);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert_ne!(a, random_hex(8).unwrap());
    }
}
