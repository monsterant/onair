//! 녹화와 다시보기 (#47, 공모 선택 요구사항 3).
//!
//! 흐름
//! - 15초마다 도는 맞춰 보기(main.rs)에서 `sync`를 부른다.
//! - 송출이 막 허락되면(opening 웹훅) `Kick` 신호로 15초를 기다리지 않고 바로 `start_soon`을 돌린다(#59).
//!   그러지 않으면 방송 앞부분 0~15초가 녹화에 빠졌다.
//!   1. 방송 중(live)인데 녹화 중이 아닌 방송 → OME에 영상 코덱을 묻고 녹화를 시작한다(H.264는 mp4, VP8은 webm).
//!   2. 녹화 중인데 방송이 끝났거나(live 아님) 2시간이 넘은 녹화 → OME에 멈추라고 한다(finishing).
//!   3. finishing인 녹화 → 파일이 생겼으면 ready(다시보기 가능), 2분이 지나도 없으면 failed.
//!   4. 다시보기 파일 합계가 3GB를 넘으면 오래된 것부터 지운다. 기록이 없는 파일(방송 삭제 등)도 지운다.
//! - 방송 정보(제목, 방송자)와 파일 이름은 DB에 남으므로 서버를 다시 켜도 목록이 그대로다.
//! - 파일은 `/vod/{파일 이름}`으로 내보낸다. 이어 보기·탐색(Range 요청)을 지원한다. 누구나 본다(생방송 시청과 같다).

use std::{
    path::{Path as FsPath, PathBuf},
    time::{Duration, SystemTime},
};

use axum::{
    Json,
    extract::{Path, Request, State},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use time::OffsetDateTime;
use tower::ServiceExt;
use tower_http::services::ServeFile;

use crate::{AppState, auth::ApiError, ome_api::OmeApi};

/// 녹화 하나의 최대 길이. 넘으면 멈추고, 방송이 계속되면 새 녹화로 이어 간다.
pub const MAX_RECORDING: Duration = Duration::from_secs(2 * 60 * 60);
/// 다시보기(다 된 녹화) 몫. 넘으면 오래된 다시보기부터 지운다.
pub const TOTAL_CAP_BYTES: i64 = 3 * 1024 * 1024 * 1024;
/// 녹화 폴더 전체(녹화 중인 파일 포함) 비상 한도. 넘으면 녹화 중인 것도 멈추고 새로 시작하지 않는다(서버 디스크 보호).
/// 다시보기를 지워서 녹화 중인 파일 자리를 만들지는 않는다(#47 2바퀴).
pub const HARD_CAP_BYTES: u64 = 4608 * 1024 * 1024;
/// 멈춘 뒤 파일을 기다리는 시간
pub const FINISH_WAIT: Duration = Duration::from_secs(120);
/// 이보다 작은 파일(1초 남짓)은 다시보기로 올리지 않는다(#47 공격 사냥: 거의 빈 녹화가 목록에 뜸)
pub const MIN_BYTES: u64 = 64 * 1024;
/// 동시에 녹화하는 방송 수 상한(서버 디스크·CPU 보호). 넘으면 다음 차례까지 기다린다.
pub const MAX_CONCURRENT: i64 = 3;
// 그리고 한 사람(방송 주인)은 동시에 1개만 녹화한다(#132, start_missing·waiting 조건). 한 사람이 방송 여러 개를
// 동시에 송출해 서버 전체 자리(3개)를 다 차지하면 다른 사람 방송이 녹화되지 않기 때문이다.

/// "녹화를 서둘러 시작하라"는 신호(#59). 웹훅이 보내고 맞춰 보기 반복문(main.rs)이 받는다.
/// tokio `Notify`(기다리는 쪽을 깨우는 종): 받는 쪽이 아직 기다리지 않을 때 보내도 한 번은 기억해 둔다.
#[derive(Clone, Debug, Default)]
pub struct Kick(std::sync::Arc<tokio::sync::Notify>);

impl Kick {
    pub fn send(&self) {
        self.0.notify_one();
    }
    pub async fn wait(&self) {
        self.0.notified().await;
    }
}

/// 신호를 받은 뒤 녹화 시작을 다시 시도하는 간격과 횟수(#59).
/// 웹훅 순간엔 OME에 스트림이 아직 없어 영상 코덱을 모른다. 보통 1~3초 안에 알게 된다.
pub const SOON_STEP: Duration = Duration::from_secs(1);
pub const SOON_TRIES: u32 = 10;

/// 송출이 막 붙었을 때 부른다. `step` 간격으로 최대 `tries`번 `sync`를 돌리고,
/// 녹화를 기다리는 방송이 없어지면 바로 멈춘다.
pub async fn start_soon(
    db: &sqlx::PgPool,
    api: &OmeApi,
    cfg: &RecConfig,
    step: Duration,
    tries: u32,
) -> Result<(), sqlx::Error> {
    if cfg.dir.is_none() || !api.is_configured() {
        return Ok(());
    }
    for _ in 0..tries {
        sync(db, api, cfg).await?;
        if !waiting(db).await? {
            break;
        }
        tokio::time::sleep(step).await;
    }
    Ok(())
}

/// 방송 중인데 아직 녹화가 없는 방송이 있고, 동시 녹화 자리도 남았는지.
/// 주인이 이미 다른 방송을 녹화 중이면 그 방송은 기다리지 않는다(start_missing과 같은 조건, #132).
/// 자리가 없으면 다시 시도해도 시작할 수 없으니 기다리지 않는다(#59 리뷰).
async fn waiting(db: &sqlx::PgPool) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM recordings WHERE status = 'recording') < $1
            AND EXISTS (SELECT 1 FROM broadcasts b WHERE b.status = 'live'
              AND NOT EXISTS (SELECT 1 FROM recordings r WHERE r.broadcast_id = b.id AND r.status = 'recording')
              AND NOT EXISTS (SELECT 1 FROM recordings r JOIN broadcasts o ON o.id = r.broadcast_id
                               WHERE o.owner_id = b.owner_id AND r.status = 'recording'))",
    )
    .bind(MAX_CONCURRENT)
    .fetch_one(db)
    .await
}

/// 녹화 설정. 폴더가 없으면(Default) 녹화를 하지 않는다.
#[derive(Clone, Debug, Default)]
pub struct RecConfig {
    pub dir: Option<PathBuf>,
}

#[derive(Serialize, Debug, sqlx::FromRow)]
pub struct Recording {
    pub id: i64,
    pub broadcast_id: i64,
    pub title: String,
    pub owner_username: String,
    pub format: String,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub ended_at: Option<OffsetDateTime>,
    pub bytes: i64,
    #[serde(skip)]
    pub file_name: String,
}

#[derive(Serialize, Debug)]
pub struct RecordingView {
    #[serde(flatten)]
    pub rec: Recording,
    /// 재생 주소(화면은 /api를 붙여 부른다)
    pub url: String,
}

impl From<Recording> for RecordingView {
    fn from(rec: Recording) -> Self {
        let url = format!("/vod/{}", rec.file_name);
        Self { rec, url }
    }
}

/// 다시보기 조회 SQL 앞부분. sqlx는 SQL을 문자열 상수로만 받으므로 concat!으로 붙인다.
macro_rules! select_ready {
    ($rest:literal) => {
        concat!(
            "SELECT id, broadcast_id, title, owner_username, format, started_at, ended_at, bytes, file_name
  FROM recordings WHERE status = 'ready' ",
            $rest
        )
    };
}

/// 녹화 파일 이름 규칙: rec-<방송 id>-<16진수 8자>.<mp4|webm>. 이 모양이 아니면 내보내지도 지우지도 않는다.
pub fn valid_file_name(name: &str) -> bool {
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return false;
    };
    if ext != "mp4" && ext != "webm" {
        return false;
    }
    let mut parts = stem.splitn(3, '-');
    parts.next() == Some("rec")
        && parts
            .next()
            .is_some_and(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()))
        && parts.next().is_some_and(|h| {
            h.len() == 8
                && h.chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        })
}

/// 코덱 이름으로 파일 형식과 녹화에 넣을 출력(영상 그대로 + 소리)을 고른다.
/// - H.264: mp4 + AAC 소리(LL-HLS용으로 이미 만드는 소리라 추가 변환 비용이 없다)
/// - VP8: webm + Opus 소리(mp4에는 VP8을 넣을 수 없다)
pub fn format_for(codec: &str) -> Option<(&'static str, [&'static str; 2])> {
    match codec.to_ascii_uppercase().as_str() {
        "H264" => Some(("mp4", ["bypass_video", "aac_audio"])),
        "VP8" => Some(("webm", ["bypass_video", "opus_audio"])),
        _ => None,
    }
}

// ---------- 맞춰 보기 ----------

/// 녹화 상태를 맞춘다. 15초마다 부른다.
pub async fn sync(db: &sqlx::PgPool, api: &OmeApi, cfg: &RecConfig) -> Result<(), sqlx::Error> {
    let Some(dir) = cfg.dir.as_deref() else {
        return Ok(());
    };
    if !api.is_configured() {
        return Ok(());
    }
    // OME가 다시 켜지면 녹화 목록을 잊는다. 우리 기록은 녹화 중인데 OME에 없으면 끝난 것으로 보고 새로 시작하게 한다.
    forget_lost(db, api).await?;
    stop_finished(db, api).await?;
    settle_finishing(db, dir).await?;
    enforce_cap(db, dir).await?;
    remove_orphan_files(db, dir).await?;
    // 녹화 폴더 실제 합계가 비상 한도를 넘으면 녹화 중인 것도 멈추고 새로 시작하지 않는다(#47 리뷰)
    if dir_bytes(dir) >= HARD_CAP_BYTES {
        tracing::warn!("녹화 폴더가 비상 한도를 넘어 녹화를 멈춥니다");
        stop_all(db, api).await?;
    } else {
        start_missing(db, api).await?;
    }
    Ok(())
}

/// 녹화 폴더의 실제 파일 합계(녹화 중인 파일 포함)
fn dir_bytes(dir: &FsPath) -> u64 {
    std::fs::read_dir(dir)
        .map(|it| {
            it.flatten()
                .filter_map(|e| e.metadata().ok())
                .filter(|m| m.is_file())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

/// 우리 기록은 녹화 중인데 OME에 그 녹화가 없으면(OME 재시작 등) 끝난 것으로 본다(#47 리뷰)
async fn forget_lost(db: &sqlx::PgPool, api: &OmeApi) -> Result<(), sqlx::Error> {
    let Some(present) = api.records().await else {
        return Ok(());
    };
    let present: Vec<String> = present.into_iter().collect();
    sqlx::query(
        "UPDATE recordings SET status = 'finishing', ended_at = now()
          WHERE status = 'recording' AND started_at < now() - interval '30 seconds'
            AND NOT (record_id = ANY($1))",
    )
    .bind(&present)
    .execute(db)
    .await?;
    Ok(())
}

/// 디스크 비상: 녹화 중인 것을 모두 멈춘다
async fn stop_all(db: &sqlx::PgPool, api: &OmeApi) -> Result<(), sqlx::Error> {
    let rows: Vec<String> =
        sqlx::query_scalar("SELECT record_id FROM recordings WHERE status = 'recording'")
            .fetch_all(db)
            .await?;
    for rid in rows {
        if api.stop_record(&rid).await {
            sqlx::query(
                "UPDATE recordings SET status = 'finishing', ended_at = now()
                  WHERE record_id = $1 AND status = 'recording'",
            )
            .bind(&rid)
            .execute(db)
            .await?;
        }
    }
    Ok(())
}

/// 1. 방송 중인데 녹화 중이 아닌 방송의 녹화를 시작한다(동시 녹화 수 상한 안에서, 주인마다 1개, #132)
async fn start_missing(db: &sqlx::PgPool, api: &OmeApi) -> Result<(), sqlx::Error> {
    let active: i64 =
        sqlx::query_scalar("SELECT count(*) FROM recordings WHERE status = 'recording'")
            .fetch_one(db)
            .await?;
    let room = (MAX_CONCURRENT - active).max(0);
    // started_at: 송출이 새로 열릴 때(live가 아니다가 live가 될 때)만 바뀐다. 아래에서 "그 사이 송출이 바뀌었는지" 보는 데 쓴다(#59 리뷰).
    // 송출이 닫히면 status가 바뀐다. 예전에는 updated_at을 봤는데, 제목만 고쳐도 바뀌어서
    // 막 건 녹화를 멈추고 다시 거는 일이 있었다(#62 L1). 그래서 status와 started_at을 함께 본다.
    // 못 잡는 경우: 이미 live인 방송에 closing 없이 opening이 다시 오면 started_at이 그대로다(ome.rs, #27 V18).
    // closing이 늦게라도 오면 on_closing이 녹화를 멈추므로, closing이 아예 사라진 때(백엔드 재시작 중 등)만 남는다.
    // 그때는 이미 돌던 녹화도 같은 문제라(#62 리뷰) 이 검사로 따로 막지 않는다.
    // 이미 녹화 중인 방송이 있는 사람의 방송은 고르지 않는다(사람마다 동시 녹화 1개, #132)
    let rows: Vec<(i64, String, String, String, OffsetDateTime, i64)> = sqlx::query_as(
        "SELECT b.id, b.stream_name, b.title, u.username, b.started_at, b.owner_id FROM broadcasts b JOIN users u ON u.id = b.owner_id
          WHERE b.status = 'live' AND b.started_at IS NOT NULL
            AND NOT EXISTS (SELECT 1 FROM recordings r WHERE r.broadcast_id = b.id AND r.status = 'recording')
            AND NOT EXISTS (SELECT 1 FROM recordings r JOIN broadcasts o ON o.id = r.broadcast_id
                             WHERE o.owner_id = b.owner_id AND r.status = 'recording')
          ORDER BY b.started_at",
    )
    .fetch_all(db)
    .await?;
    // 실제로 시작한 수만 센다. 코덱을 몰라 건너뛴 방송이 자리를 차지해 새 방송이 계속 밀리지 않게 한다(#47 2바퀴)
    let mut started = 0_i64;
    // 이번 차례에 녹화를 시작한 사람. 같은 사람의 다른 방송은 이번에 건너뛴다(#132)
    let mut started_owners = std::collections::HashSet::new();
    for (bid, stream, title, owner, seen, owner_id) in rows {
        if started >= room {
            break;
        }
        if started_owners.contains(&owner_id) {
            continue;
        }
        // 코덱을 모르면(스트림이 막 붙는 중) 다음 차례에 다시 본다
        let Some(codec) = api.video_codec(&stream).await else {
            continue;
        };
        let Some((ext, variants)) = format_for(&codec) else {
            tracing::warn!(%codec, broadcast = bid, "녹화할 수 없는 영상 코덱");
            continue;
        };
        let Ok(hex) = crate::broadcasts::random_hex(4) else {
            continue;
        };
        let record_id = format!("rec-{bid}-{hex}");
        let file = format!("{record_id}.{ext}");
        // 기록을 먼저 남긴다(같은 방송에 녹화 중은 하나뿐이라는 DB 규칙이 겹침을 막는다). OME가 거절하면 지운다.
        // 위에서 읽은 뒤 송출이 닫혔거나 다시 열렸으면(status나 started_at이 바뀜) 넣지 않는다. 닫히는 송출에 녹화를 걸면
        // 곧바로 다시 붙은 송출까지 같은 파일에 이어 써질 수 있다(#47 R1과 같은 문제, #59 리뷰).
        let inserted = sqlx::query(
            "INSERT INTO recordings (broadcast_id, record_id, file_name, format, title, owner_username)
             SELECT $1, $2, $3, $4, $5, $6
              WHERE EXISTS (SELECT 1 FROM broadcasts WHERE id = $1 AND status = 'live' AND started_at = $7)
             ON CONFLICT DO NOTHING",
        )
        .bind(bid)
        .bind(&record_id)
        .bind(&file)
        .bind(ext)
        .bind(&title)
        .bind(&owner)
        .bind(seen)
        .execute(db)
        .await;
        // 그 사이 방송이 지워졌으면(외래 키 오류) 이 방송만 건너뛰고 다른 방송은 계속 본다(#59 공격 사냥)
        let inserted = match inserted {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, broadcast = bid, "녹화 기록을 넣지 못해 건너뜀");
                continue;
            }
        };
        if inserted.rows_affected() == 0 {
            continue;
        }
        if !api
            .start_record(&record_id, &stream, &file, &variants)
            .await
        {
            sqlx::query("DELETE FROM recordings WHERE record_id = $1")
                .bind(&record_id)
                .execute(db)
                .await?;
        } else {
            started += 1;
            started_owners.insert(owner_id);
            tracing::info!(broadcast = bid, %file, "녹화 시작");
            // 녹화를 거는 동안 송출이 닫혔거나 다시 열렸으면 바로 멈춘다(같은 이유, #59 리뷰).
            // 다음 맞춰 보기가 지금 송출로 새 녹화를 시작한다.
            let same: Option<bool> = sqlx::query_scalar(
                "SELECT status = 'live' AND started_at = $2 FROM broadcasts WHERE id = $1",
            )
            .bind(bid)
            .bind(seen)
            .fetch_optional(db)
            .await?;
            if same != Some(true) && api.stop_record(&record_id).await {
                sqlx::query(
                    "UPDATE recordings SET status = 'finishing', ended_at = now()
                      WHERE record_id = $1 AND status = 'recording'",
                )
                .bind(&record_id)
                .execute(db)
                .await?;
                tracing::info!(broadcast = bid, %file, "녹화 거는 사이 송출이 바뀌어 멈춤");
            }
        }
    }
    Ok(())
}

/// 2. 방송이 끝났거나 너무 길어진 녹화를 멈춘다
async fn stop_finished(db: &sqlx::PgPool, api: &OmeApi) -> Result<(), sqlx::Error> {
    // 멈출 녹화: 방송이 live가 아님, 너무 오래됨(상한), 또는 지난 송출의 녹화(#62 M2).
    // 송출이 닫힌 뒤 멈춤이 계속 실패하고 그사이 방송자가 다시 붙으면, 방송은 다시 live라 첫 조건에 걸리지 않는다.
    // 다시 live가 될 때 방송 시작 시각(b.started_at)이 새로 정해지므로(ome.rs), 그보다 먼저 시작한 녹화는 지난 송출의 것이다.
    // 그대로 두면 상한(2시간)까지 한 파일에 이어지고, 같은 방송에 녹화 중은 하나뿐이라 새 녹화도 못 건다.
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT r.record_id FROM recordings r JOIN broadcasts b ON b.id = r.broadcast_id
          WHERE r.status = 'recording'
            AND (b.status <> 'live' OR r.started_at < now() - make_interval(secs => $1)
                 OR r.started_at < b.started_at)",
    )
    .bind(MAX_RECORDING.as_secs_f64())
    .fetch_all(db)
    .await?;
    for record_id in rows {
        // OME가 멈췄다고 답할 때만 넘어간다. 실패하면 녹화 중으로 두고 다음 차례에 다시 멈춘다(#47 리뷰:
        // 넘어가 버리면 OME는 계속 쓰는데 새 녹화가 옆에서 또 시작된다)
        if !api.stop_record(&record_id).await {
            continue;
        }
        sqlx::query(
            "UPDATE recordings SET status = 'finishing', ended_at = now()
              WHERE record_id = $1 AND status = 'recording'",
        )
        .bind(&record_id)
        .execute(db)
        .await?;
        tracing::info!(%record_id, "녹화 멈춤");
    }
    Ok(())
}

fn file_size(dir: &FsPath, name: &str) -> Option<u64> {
    std::fs::metadata(dir.join(name))
        .ok()
        .filter(|m| m.is_file())
        .map(|m| m.len())
}

/// 3. 멈춘 녹화의 파일이 생겼는지 본다
async fn settle_finishing(db: &sqlx::PgPool, dir: &FsPath) -> Result<(), sqlx::Error> {
    let rows: Vec<(i64, String, bool)> = sqlx::query_as(
        "SELECT id, file_name, ended_at < now() - make_interval(secs => $1) FROM recordings
          WHERE status = 'finishing'",
    )
    .bind(FINISH_WAIT.as_secs_f64())
    .fetch_all(db)
    .await?;
    for (id, file, waited_long) in rows {
        match file_size(dir, &file) {
            Some(bytes) if bytes >= MIN_BYTES => {
                sqlx::query("UPDATE recordings SET status = 'ready', bytes = $2 WHERE id = $1 AND status = 'finishing'")
                    .bind(id)
                    .bind(bytes as i64)
                    .execute(db)
                    .await?;
            }
            // 너무 짧은 녹화: 다시보기에 올리지 않는다(파일은 정리 작업이 지운다)
            Some(bytes) if bytes > 0 => {
                tracing::info!(%file, bytes, "너무 짧은 녹화라 올리지 않음");
                sqlx::query("UPDATE recordings SET status = 'failed' WHERE id = $1 AND status = 'finishing'")
                    .bind(id)
                    .execute(db)
                    .await?;
            }
            _ if waited_long => {
                tracing::warn!(%file, "녹화 파일이 생기지 않음");
                sqlx::query("UPDATE recordings SET status = 'failed' WHERE id = $1 AND status = 'finishing'")
                    .bind(id)
                    .execute(db)
                    .await?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// 4. 다시보기 합계가 몫을 넘으면 테스트 계정의 다시보기부터, 그다음 오래된 다시보기부터 지운다.
///
/// 테스트 계정의 녹화로 3GB를 채워 일반 회원의 다시보기가 밀려 지워지지 않게 한다(#132).
/// 테스트 계정은 auth::cleanup_guests와 같은 기준(이름 + 비밀번호 표시 값)으로 가린다.
async fn enforce_cap(db: &sqlx::PgPool, dir: &FsPath) -> Result<(), sqlx::Error> {
    let rows: Vec<(i64, String, i64)> = sqlx::query_as(
        r"SELECT r.id, r.file_name, r.bytes FROM recordings r
            JOIN broadcasts b ON b.id = r.broadcast_id JOIN users u ON u.id = b.owner_id
          WHERE r.status = 'ready'
          ORDER BY (u.username LIKE 'guest_%' AND u.password_hash = '!guest-no-password') DESC,
                   r.started_at ASC, r.id ASC",
    )
    .fetch_all(db)
    .await?;
    // 다시보기 몫만 센다. 녹화 중인 파일 때문에 다시보기를 지우지 않는다(그쪽은 비상 한도가 맡는다)
    let mut total: i64 = rows.iter().map(|r| r.2).sum();
    for (id, file, bytes) in rows {
        if total <= TOTAL_CAP_BYTES {
            break;
        }
        total -= bytes;
        {
            tracing::info!(%file, "디스크 상한을 넘어 다시보기를 지움");
            sqlx::query("DELETE FROM recordings WHERE id = $1")
                .bind(id)
                .execute(db)
                .await?;
            if valid_file_name(&file) {
                let _ = std::fs::remove_file(dir.join(&file));
            }
        }
    }
    Ok(())
}

/// 기록이 없는 녹화 파일(방송 삭제, 실패 등)과 오래된 임시 파일을 지운다
async fn remove_orphan_files(db: &sqlx::PgPool, dir: &FsPath) -> Result<(), sqlx::Error> {
    let known: Vec<String> =
        sqlx::query_scalar("SELECT file_name FROM recordings WHERE status <> 'failed'")
            .fetch_all(db)
            .await?;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    let now = SystemTime::now();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let age = e
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| now.duration_since(t).ok())
            .unwrap_or_default();
        // 녹화 파일: 기록이 없고 10분 넘게 안 바뀐 것. 임시 파일(OME가 쓰는 중): 3시간 넘게 안 바뀐 것.
        let orphan_rec =
            valid_file_name(&name) && !known.contains(&name) && age > Duration::from_secs(600);
        let stale_tmp = name.starts_with("tmp_") && age > Duration::from_secs(3 * 60 * 60);
        // OME가 남기는 녹화 정보 파일(<스트림 이름>.xml). 다시보기에는 쓰지 않는다. 녹화 중에는 바뀌므로 1시간 지난 것만.
        let stale_info = name
            .strip_suffix(".xml")
            .is_some_and(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit()))
            && age > Duration::from_secs(60 * 60);
        if orphan_rec || stale_tmp || stale_info {
            let _ = std::fs::remove_file(e.path());
        }
    }
    Ok(())
}

// ---------- API ----------

/// 다시보기 목록(최근 100개). 누구나 본다.
pub async fn list(State(state): State<AppState>) -> Result<Json<Vec<RecordingView>>, ApiError> {
    let rows = sqlx::query_as::<_, Recording>(select_ready!(
        "ORDER BY started_at DESC, id DESC LIMIT 100"
    ))
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

/// 방송 하나의 다시보기 목록
pub async fn list_for_broadcast(
    State(state): State<AppState>,
    Path(broadcast_id): Path<i64>,
) -> Result<Json<Vec<RecordingView>>, ApiError> {
    let rows = sqlx::query_as::<_, Recording>(select_ready!(
        "AND broadcast_id = $1 ORDER BY started_at DESC, id DESC LIMIT 50"
    ))
    .bind(broadcast_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<RecordingView>, ApiError> {
    let rec = sqlx::query_as::<_, Recording>(select_ready!("AND id = $1"))
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(rec.into()))
}

/// 녹화 파일을 내보낸다. 다시보기 가능(ready)한 파일만, 이름 규칙에 맞는 것만. Range 요청(탐색)을 지원한다.
pub async fn serve_file(
    State(state): State<AppState>,
    Path(file): Path<String>,
    req: Request,
) -> Response {
    let Some(dir) = state.rec.dir.as_deref() else {
        return ApiError::NotFound.into_response();
    };
    if !valid_file_name(&file) {
        return ApiError::NotFound.into_response();
    }
    let ready: Result<Option<i64>, _> =
        sqlx::query_scalar("SELECT id FROM recordings WHERE file_name = $1 AND status = 'ready'")
            .bind(&file)
            .fetch_optional(&state.db)
            .await;
    match ready {
        Ok(Some(_)) => {}
        Ok(None) => return ApiError::NotFound.into_response(),
        Err(e) => return ApiError::from(e).into_response(),
    }
    match ServeFile::new(dir.join(&file)).oneshot(req).await {
        // 영상은 브라우저가 받아 둔 조각을 다시 쓸 수 있게 하되, 쓸 때마다 서버에 확인한다.
        // 녹화를 지우면 다음 확인에서 404가 되어 옛 영상이 남지 않는다(#25 A16의 no-store 대신).
        // 오류(파일 없음 404, 범위 오류 416)에는 붙이지 않는다. 그러면 바깥의 no_store가 붙는다(#71 R1).
        Ok(res) => {
            let mut res = res.into_response();
            let s = res.status();
            if s.is_success() || s == axum::http::StatusCode::NOT_MODIFIED {
                res.headers_mut().insert(
                    axum::http::header::CACHE_CONTROL,
                    axum::http::HeaderValue::from_static("private, no-cache"),
                );
            }
            res
        }
        Err(_) => ApiError::Internal.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_name_rules() {
        assert!(valid_file_name("rec-12-0a1b2c3d.mp4"));
        assert!(valid_file_name("rec-1-ffffffff.webm"));
        for bad in [
            "rec-12-0a1b2c3d.mkv",
            "rec-12-0A1B2C3D.mp4",
            "rec--0a1b2c3d.mp4",
            "rec-x-0a1b2c3d.mp4",
            "rec-12-0a1b2c3.mp4",
            "../rec-12-0a1b2c3d.mp4",
            "rec-12-0a1b2c3d.mp4/..",
            "tmp_rec-12-0a1b2c3d.mp4",
            "rec-12-0a1b2c3d",
            "",
        ] {
            assert!(!valid_file_name(bad), "{bad}");
        }
    }

    #[test]
    fn format_by_codec() {
        assert_eq!(format_for("H264").unwrap().0, "mp4");
        assert_eq!(format_for("h264").unwrap().1, ["bypass_video", "aac_audio"]);
        assert_eq!(format_for("VP8").unwrap().0, "webm");
        assert!(format_for("H265").is_none());
    }
}
