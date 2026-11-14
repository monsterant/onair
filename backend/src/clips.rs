//! 다시보기 장면 저장(#111). 다시보기의 한 구간(최대 2분)을 링크로 남긴다.
//! 영상 파일은 자르지 않고 재생 위치(시작·끝)만 저장한다. 장면을 열면 그 구간만 재생하고 끝에서 멈춘다.
//! - 누구나 장면 목록과 장면을 본다(다시보기가 누구나 보는 것과 같음).
//! - 로그인한 사람만 만들고, 만든 사람만 지운다.

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
    recordings::RecordingView,
};

/// 장면 하나의 최대 길이(밀리초)
pub const MAX_CLIP_MS: i64 = 120_000;
/// 장면 제목 최대 글자 수
pub const TITLE_MAX: usize = 50;
/// 다시보기 하나에 만들 수 있는 장면 수(DB를 채우지 못하게)
pub const CLIPS_PER_RECORDING: i64 = 200;

#[derive(Serialize, sqlx::FromRow)]
pub struct Clip {
    pub id: i64,
    pub recording_id: i64,
    pub user_id: i64,
    pub username: String,
    pub title: String,
    #[serde(skip)]
    pub start_ms: i32,
    #[serde(skip)]
    pub end_ms: i32,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// 화면에 보내는 모양: 시작·끝은 초(소수 셋째 자리까지)
#[derive(Serialize)]
pub struct ClipOut {
    #[serde(flatten)]
    pub clip: Clip,
    pub start: f64,
    pub end: f64,
}

impl From<Clip> for ClipOut {
    fn from(clip: Clip) -> Self {
        let (start, end) = (clip.start_ms as f64 / 1000.0, clip.end_ms as f64 / 1000.0);
        Self { clip, start, end }
    }
}

/// 장면 화면용: 장면과 그 다시보기
#[derive(Serialize)]
pub struct ClipView {
    #[serde(flatten)]
    pub clip: ClipOut,
    pub recording: RecordingView,
}

#[derive(Deserialize)]
pub struct NewClip {
    pub title: String,
    /// 시작·끝(초)
    pub start: f64,
    pub end: f64,
}

/// 장면 조회 SQL 앞부분. sqlx는 SQL을 문자열 상수로만 받으므로(SQL 끼워 넣기 방지) concat!으로 붙인다.
macro_rules! select_clip {
    ($rest:literal) => {
        concat!(
            "SELECT c.id, c.recording_id, c.user_id, u.username, c.title, c.start_ms, c.end_ms, c.created_at FROM clips c JOIN users u ON u.id = c.user_id ",
            $rest
        )
    };
}

/// 제목을 다듬는다: 앞뒤 빈칸 빼기, 1~50자, 제어 문자·글자 방향 문자 금지(댓글과 같은 기준)
fn clean_title(raw: &str) -> Result<String, ApiError> {
    let t = raw.trim();
    if t.chars()
        .any(|c| c.is_control() || crate::comments::bidi_control(c))
    {
        return Err(ApiError::BadRequest(
            "장면 제목에 쓸 수 없는 글자가 있습니다",
        ));
    }
    if t.chars().all(crate::comments::blank_like) {
        return Err(ApiError::BadRequest("장면 제목을 입력하세요"));
    }
    if t.chars().count() > TITLE_MAX {
        return Err(ApiError::BadRequest("장면 제목은 50자 이하여야 합니다"));
    }
    Ok(t.to_string())
}

/// 시작·끝(초)을 밀리초로 바꾸고 검사한다. 끝은 다시보기 길이를 넘지 않아야 한다(1초 여유).
fn check_range(start: f64, end: f64, recording_ms: i64) -> Result<(i32, i32), ApiError> {
    if !start.is_finite() || !end.is_finite() || start < 0.0 {
        return Err(ApiError::BadRequest("장면 구간이 올바르지 않습니다"));
    }
    let (s, e) = (
        (start * 1000.0).round() as i64,
        (end * 1000.0).round() as i64,
    );
    if e <= s {
        return Err(ApiError::BadRequest("장면 끝은 시작보다 뒤여야 합니다"));
    }
    if e - s > MAX_CLIP_MS {
        return Err(ApiError::BadRequest("장면은 2분까지입니다"));
    }
    if e > recording_ms + 1000 {
        return Err(ApiError::BadRequest("장면 끝이 다시보기 길이를 넘습니다"));
    }
    Ok((s as i32, e as i32))
}

/// 다시보기의 장면 목록(새것부터 100개). 누구나.
pub async fn list(
    State(state): State<AppState>,
    Path(recording_id): Path<i64>,
) -> Result<Json<Vec<ClipOut>>, ApiError> {
    crate::recordings::find_ready(&state.db, recording_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let rows: Vec<Clip> = sqlx::query_as(select_clip!(
        "WHERE c.recording_id = $1 ORDER BY c.created_at DESC, c.id DESC LIMIT 100"
    ))
    .bind(recording_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows.into_iter().map(ClipOut::from).collect()))
}

/// 장면 만들기. 로그인한 사람만.
pub async fn create(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(recording_id): Path<i64>,
    Json(input): Json<NewClip>,
) -> Result<(StatusCode, Json<ClipOut>), ApiError> {
    let rec = crate::recordings::find_ready(&state.db, recording_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let title = clean_title(&input.title)?;
    let recording_ms = rec
        .rec
        .ended_at
        .map(|e| (e - rec.rec.started_at).whole_milliseconds() as i64)
        .unwrap_or(0);
    let (start_ms, end_ms) = check_range(input.start, input.end, recording_ms)?;
    // 도배를 막는다(댓글과 같은 몫: 사용자 하나당 1분 20번, 따로 센다)
    if !state
        .auth
        .comment_limiter
        .allow(&format!("clip:{}", user.id))
    {
        return Err(ApiError::TooManyRequests);
    }
    // 다시보기 하나에 장면이 너무 많으면 막는다. 세기와 넣기를 한 문장으로 해서 동시에 와도 넘지 않게 한다.
    let id: Option<i64> = sqlx::query_scalar(
        "INSERT INTO clips (recording_id, user_id, title, start_ms, end_ms)
         SELECT $1, $2, $3, $4, $5
          WHERE (SELECT count(*) FROM clips WHERE recording_id = $1) < $6
         RETURNING id",
    )
    .bind(recording_id)
    .bind(user.id)
    .bind(&title)
    .bind(start_ms)
    .bind(end_ms)
    .bind(CLIPS_PER_RECORDING)
    .fetch_optional(&state.db)
    .await?;
    let id = id.ok_or(ApiError::Conflict(
        "이 다시보기에는 장면을 더 만들 수 없습니다",
    ))?;
    let clip: Clip = sqlx::query_as(select_clip!("WHERE c.id = $1"))
        .bind(id)
        .fetch_one(&state.db)
        .await?;
    Ok((StatusCode::CREATED, Json(clip.into())))
}

/// 장면 하나와 그 다시보기. 누구나. 다시보기가 없어졌으면 404.
pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<ClipView>, ApiError> {
    let clip: Clip = sqlx::query_as(select_clip!("WHERE c.id = $1"))
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    let recording = crate::recordings::find_ready(&state.db, clip.recording_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(ClipView {
        clip: clip.into(),
        recording,
    }))
}

/// 장면 지우기. 만든 사람만(남은 403).
pub async fn delete(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    let owner: Option<i64> = sqlx::query_scalar("SELECT user_id FROM clips WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    match owner {
        None => Err(ApiError::NotFound),
        Some(uid) if uid != user.id => {
            Err(ApiError::Forbidden("장면을 만든 사람만 지울 수 있습니다"))
        }
        Some(_) => {
            sqlx::query("DELETE FROM clips WHERE id = $1 AND user_id = $2")
                .bind(id)
                .bind(user.id)
                .execute(&state.db)
                .await?;
            Ok(StatusCode::NO_CONTENT)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_rules() {
        // 30초짜리 다시보기
        assert_eq!(check_range(1.0, 5.5, 30_000).unwrap(), (1000, 5500));
        assert!(check_range(5.0, 5.0, 30_000).is_err(), "끝이 시작과 같음");
        assert!(check_range(-1.0, 5.0, 30_000).is_err(), "음수");
        assert!(check_range(0.0, f64::NAN, 30_000).is_err(), "숫자가 아님");
        assert!(check_range(0.0, 31.5, 30_000).is_err(), "다시보기보다 김");
        assert!(check_range(0.0, 30.9, 30_000).is_ok(), "1초 여유");
        assert!(check_range(0.0, 120.0, 600_000).is_ok(), "정확히 2분");
        assert!(check_range(0.0, 120.1, 600_000).is_err(), "2분 넘음");
    }

    #[test]
    fn title_rules() {
        assert_eq!(clean_title("  명장면 ").unwrap(), "명장면");
        assert!(clean_title("   ").is_err());
        assert!(clean_title(&"가".repeat(50)).is_ok());
        assert!(clean_title(&"가".repeat(51)).is_err());
        assert!(clean_title("a\u{202E}b").is_err());
    }
}
