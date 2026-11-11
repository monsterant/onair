//! 녹화와 다시보기 시험 (#47). OME 관리 API는 가짜 서버로 흉내 낸다.
//! 가짜 OME는 녹화를 멈추라고 하면 녹화 폴더에 파일을 만든다(진짜 OME처럼).

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Path, State},
    http::{Request, StatusCode, header},
    routing::{get, post},
};
use onair_backend::{
    AppState, app,
    auth::AuthConfig,
    ome_api::OmeApi,
    recordings::{RecConfig, sync},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

/// (스트림, 파일 경로, 출력 이름들)
type Rec = (String, String, Vec<String>);

#[derive(Clone, Default)]
struct FakeOme {
    /// 스트림 이름 → 영상 코덱
    streams: Arc<Mutex<HashMap<String, String>>>,
    /// 녹화 id → (스트림, 파일 경로, 출력 이름들)
    records: Arc<Mutex<HashMap<String, Rec>>>,
    stopped: Arc<Mutex<Vec<String>>>,
    fail_start: Arc<Mutex<bool>>,
    /// 멈출 때 파일을 만들지 않는다(파일이 안 생기는 경우)
    no_file: Arc<Mutex<bool>>,
    dir: Arc<PathBuf>,
}

async fn fake_ome(dir: PathBuf) -> (FakeOme, OmeApi) {
    let fake = FakeOme {
        dir: Arc::new(dir),
        ..Default::default()
    };
    let r = Router::new()
        .route(
            "/v1/vhosts/default/apps/app/streams",
            get(|State(f): State<FakeOme>| async move {
                let list: Vec<String> = f.streams.lock().unwrap().keys().cloned().collect();
                Json(json!({ "statusCode": 200, "response": list }))
            }),
        )
        .route(
            "/v1/vhosts/default/apps/app/streams/{name}",
            get(|State(f): State<FakeOme>, Path(name): Path<String>| async move {
                match f.streams.lock().unwrap().get(&name) {
                    Some(codec) => (
                        StatusCode::OK,
                        Json(json!({ "response": { "input": { "tracks": [
                            { "type": "Audio", "audio": { "codec": "OPUS" } },
                            { "type": "Video", "video": { "codec": codec } }
                        ] } } })),
                    ),
                    None => (StatusCode::NOT_FOUND, Json(json!({ "statusCode": 404 }))),
                }
            }),
        )
        .route(
            "/v1/vhosts/default/apps/{action}",
            post(|State(f): State<FakeOme>, Path(action): Path<String>, Json(b): Json<Value>| async move {
                let id = b["id"].as_str().unwrap_or_default().to_string();
                match action.as_str() {
                    "app:startRecord" => {
                        if *f.fail_start.lock().unwrap() {
                            return StatusCode::INTERNAL_SERVER_ERROR;
                        }
                        let variants = b["stream"]["variantNames"]
                            .as_array()
                            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                            .unwrap_or_default();
                        f.records.lock().unwrap().insert(
                            id,
                            (
                                b["stream"]["name"].as_str().unwrap_or_default().to_string(),
                                b["filePath"].as_str().unwrap_or_default().to_string(),
                                variants,
                            ),
                        );
                        StatusCode::OK
                    }
                    "app:stopRecord" => {
                        f.stopped.lock().unwrap().push(id.clone());
                        if let Some((_, path, _)) = f.records.lock().unwrap().get(&id)
                                && !*f.no_file.lock().unwrap()
                        {
                                let file = f.dir.join(path.trim_start_matches('/'));
                                std::fs::write(file, b"0123456789abcdefghij").unwrap();
                        }
                        StatusCode::OK
                    }
                    _ => StatusCode::NOT_FOUND,
                }
            }),
        )
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, r).await.unwrap() });
    (
        fake,
        OmeApi::new(&format!("http://{addr}"), "fake-token-0123456789"),
    )
}

fn temp_dir() -> PathBuf {
    let mut b = [0u8; 6];
    getrandom::fill(&mut b).unwrap();
    let d = std::env::temp_dir().join(format!("onair-rec-{}", hex::encode(b)));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn router(db: PgPool, ome: OmeApi, dir: Option<PathBuf>) -> Router {
    app(AppState {
        rec: RecConfig { dir },
        toss: Default::default(),
        ome,
        db,
        auth: AuthConfig {
            demo_login: true,
            ..Default::default()
        },
    })
}

async fn call(
    r: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    cookie: Option<&str>,
    range: Option<&str>,
) -> (StatusCode, Value, Vec<u8>, Option<String>) {
    let mut req = Request::builder().method(method).uri(path);
    if let Some(c) = cookie {
        req = req.header(header::COOKIE, format!("onair_session={c}"));
    }
    if let Some(rg) = range {
        req = req.header(header::RANGE, rg);
    }
    let req = match body {
        Some(b) => req
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    let res = r.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let cookie = res
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            s.split(';')
                .next()?
                .strip_prefix("onair_session=")
                .map(str::to_string)
        });
    let bytes = to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        bytes,
        cookie,
    )
}

/// demo1로 방송을 만들고 live로 둔다. (방송 id, 스트림 이름)
async fn live_broadcast(r: &Router, db: &PgPool, title: &str) -> (i64, String) {
    let cookie = call(r, "POST", "/auth/demo/1", None, None, None)
        .await
        .3
        .unwrap();
    let (_, b, _, _) = call(
        r,
        "POST",
        "/broadcasts",
        Some(json!({"title": title})),
        Some(&cookie),
        None,
    )
    .await;
    let id = b["id"].as_i64().unwrap();
    sqlx::query("UPDATE broadcasts SET status = 'live', started_at = now() WHERE id = $1")
        .bind(id)
        .execute(db)
        .await
        .unwrap();
    (id, b["stream_name"].as_str().unwrap().to_string())
}

async fn statuses(db: &PgPool) -> Vec<String> {
    sqlx::query_scalar("SELECT status FROM recordings ORDER BY id")
        .fetch_all(db)
        .await
        .unwrap()
}

#[sqlx::test]
async fn live_broadcast_is_recorded_and_replayable(db: PgPool) {
    let dir = temp_dir();
    let (fake, api) = fake_ome(dir.clone()).await;
    let cfg = RecConfig {
        dir: Some(dir.clone()),
    };
    let r = router(db.clone(), api.clone(), Some(dir.clone()));
    let (bid, stream) = live_broadcast(&r, &db, "녹화 시험").await;
    fake.streams
        .lock()
        .unwrap()
        .insert(stream.clone(), "H264".into());

    // 방송 중 → 녹화 시작(H.264는 mp4 + AAC)
    sync(&db, &api, &cfg).await.unwrap();
    assert_eq!(statuses(&db).await, ["recording"]);
    let recs = fake.records.lock().unwrap().clone();
    let (rec_stream, path, variants) = recs.values().next().unwrap().clone();
    assert_eq!(rec_stream, stream);
    assert!(
        path.starts_with(&format!("/rec-{bid}-")) && path.ends_with(".mp4"),
        "{path}"
    );
    assert_eq!(variants, ["bypass_video", "aac_audio"]);
    // 다시 돌려도 두 번 시작하지 않는다
    sync(&db, &api, &cfg).await.unwrap();
    assert_eq!(fake.records.lock().unwrap().len(), 1);
    // 녹화 중에는 다시보기에 없다
    assert_eq!(
        call(&r, "GET", "/recordings", None, None, None).await.1,
        json!([])
    );

    // 방송 종료 → 멈춤 → 파일 확인 → 다시보기
    sqlx::query("UPDATE broadcasts SET status = 'ended' WHERE id = $1")
        .bind(bid)
        .execute(&db)
        .await
        .unwrap();
    sync(&db, &api, &cfg).await.unwrap();
    assert_eq!(fake.stopped.lock().unwrap().len(), 1);
    let s = statuses(&db).await;
    assert!(s == ["ready"] || s == ["finishing"], "{s:?}");
    sync(&db, &api, &cfg).await.unwrap();
    assert_eq!(statuses(&db).await, ["ready"]);

    let (st, list, _, _) = call(&r, "GET", "/recordings", None, None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(list[0]["title"], "녹화 시험");
    assert_eq!(list[0]["owner_username"], "demo1");
    assert_eq!(list[0]["format"], "mp4");
    assert_eq!(list[0]["bytes"], 20);
    let url = list[0]["url"].as_str().unwrap().to_string();
    let rid = list[0]["id"].as_i64().unwrap();
    assert_eq!(
        call(&r, "GET", &format!("/recordings/{rid}"), None, None, None)
            .await
            .1["url"],
        url.as_str()
    );
    assert_eq!(
        call(
            &r,
            "GET",
            &format!("/broadcasts/{bid}/recordings"),
            None,
            None,
            None
        )
        .await
        .1[0]["id"],
        rid
    );
    // 파일 재생: 전체, 그리고 탐색(Range)
    let (st, _, body, _) = call(&r, "GET", &url, None, None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body, b"0123456789abcdefghij");
    let (st, _, body, _) = call(&r, "GET", &url, None, None, Some("bytes=10-14")).await;
    assert_eq!(st, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, b"abcde");
    let _ = std::fs::remove_dir_all(dir);
}

#[sqlx::test]
async fn vp8_streams_are_recorded_as_webm_with_opus(db: PgPool) {
    let dir = temp_dir();
    let (fake, api) = fake_ome(dir.clone()).await;
    let cfg = RecConfig {
        dir: Some(dir.clone()),
    };
    let r = router(db.clone(), api.clone(), Some(dir.clone()));
    let (_, stream) = live_broadcast(&r, &db, "VP8").await;
    fake.streams.lock().unwrap().insert(stream, "VP8".into());
    sync(&db, &api, &cfg).await.unwrap();
    let (_, path, variants) = fake
        .records
        .lock()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .clone();
    assert!(path.ends_with(".webm"), "{path}");
    assert_eq!(variants, ["bypass_video", "opus_audio"]);
    let format: String = sqlx::query_scalar("SELECT format FROM recordings")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(format, "webm");
    let _ = std::fs::remove_dir_all(dir);
}

#[sqlx::test]
async fn unknown_codec_or_failed_start_leaves_no_recording(db: PgPool) {
    let dir = temp_dir();
    let (fake, api) = fake_ome(dir.clone()).await;
    let cfg = RecConfig {
        dir: Some(dir.clone()),
    };
    let r = router(db.clone(), api.clone(), Some(dir.clone()));
    let (_, stream) = live_broadcast(&r, &db, "코덱").await;
    // 아직 OME에 스트림 정보가 없음(막 붙는 중)
    sync(&db, &api, &cfg).await.unwrap();
    assert!(statuses(&db).await.is_empty());
    // 녹화할 수 없는 코덱
    fake.streams
        .lock()
        .unwrap()
        .insert(stream.clone(), "H265".into());
    sync(&db, &api, &cfg).await.unwrap();
    assert!(statuses(&db).await.is_empty());
    // OME가 녹화 시작을 거절
    fake.streams.lock().unwrap().insert(stream, "H264".into());
    *fake.fail_start.lock().unwrap() = true;
    sync(&db, &api, &cfg).await.unwrap();
    assert!(statuses(&db).await.is_empty());
    // 다음 차례에 OME가 받아 주면 시작된다
    *fake.fail_start.lock().unwrap() = false;
    sync(&db, &api, &cfg).await.unwrap();
    assert_eq!(statuses(&db).await, ["recording"]);
    let _ = std::fs::remove_dir_all(dir);
}

#[sqlx::test]
async fn missing_file_becomes_failed_and_is_not_served(db: PgPool) {
    let dir = temp_dir();
    let (fake, api) = fake_ome(dir.clone()).await;
    let cfg = RecConfig {
        dir: Some(dir.clone()),
    };
    let r = router(db.clone(), api.clone(), Some(dir.clone()));
    let (bid, stream) = live_broadcast(&r, &db, "파일 없음").await;
    fake.streams.lock().unwrap().insert(stream, "H264".into());
    *fake.no_file.lock().unwrap() = true;
    sync(&db, &api, &cfg).await.unwrap();
    sqlx::query("UPDATE broadcasts SET status = 'ready' WHERE id = $1")
        .bind(bid)
        .execute(&db)
        .await
        .unwrap();
    sync(&db, &api, &cfg).await.unwrap();
    assert_eq!(statuses(&db).await, ["finishing"]);
    // 2분이 지나도 파일이 없으면 실패
    sqlx::query("UPDATE recordings SET ended_at = now() - interval '3 minutes'")
        .execute(&db)
        .await
        .unwrap();
    sync(&db, &api, &cfg).await.unwrap();
    assert_eq!(statuses(&db).await, ["failed"]);
    assert_eq!(
        call(&r, "GET", "/recordings", None, None, None).await.1,
        json!([])
    );
    let file: String = sqlx::query_scalar("SELECT file_name FROM recordings")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(
        call(&r, "GET", &format!("/vod/{file}"), None, None, None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[sqlx::test]
async fn long_recordings_are_split(db: PgPool) {
    let dir = temp_dir();
    let (fake, api) = fake_ome(dir.clone()).await;
    let cfg = RecConfig {
        dir: Some(dir.clone()),
    };
    let r = router(db.clone(), api.clone(), Some(dir.clone()));
    let (_, stream) = live_broadcast(&r, &db, "긴 방송").await;
    fake.streams.lock().unwrap().insert(stream, "H264".into());
    sync(&db, &api, &cfg).await.unwrap();
    // 2시간이 넘으면 멈추고, 방송이 계속되면 새 녹화를 시작한다
    sqlx::query("UPDATE recordings SET started_at = now() - interval '121 minutes'")
        .execute(&db)
        .await
        .unwrap();
    sync(&db, &api, &cfg).await.unwrap();
    sync(&db, &api, &cfg).await.unwrap();
    assert_eq!(fake.stopped.lock().unwrap().len(), 1);
    assert_eq!(statuses(&db).await, ["ready", "recording"]);
    let _ = std::fs::remove_dir_all(dir);
}

#[sqlx::test]
async fn total_size_cap_removes_oldest_replays(db: PgPool) {
    let dir = temp_dir();
    let (_, api) = fake_ome(dir.clone()).await;
    let cfg = RecConfig {
        dir: Some(dir.clone()),
    };
    let r = router(db.clone(), api.clone(), Some(dir.clone()));
    let (bid, _) = live_broadcast(&r, &db, "상한").await;
    sqlx::query("UPDATE broadcasts SET status = 'ended' WHERE id = $1")
        .bind(bid)
        .execute(&db)
        .await
        .unwrap();
    // 1.5GB짜리 다시보기 3개(가장 오래된 것이 먼저) → 합계 4.5GB > 3GB → 가장 오래된 것 하나를 지운다
    for (i, ago) in [(1, 30), (2, 20), (3, 10)] {
        let file = format!("rec-{bid}-0000000{i}.mp4");
        std::fs::write(dir.join(&file), b"x").unwrap();
        sqlx::query(
            "INSERT INTO recordings (broadcast_id, record_id, file_name, format, title, owner_username, status, started_at, ended_at, bytes)
             VALUES ($1, $2, $3, 'mp4', 't', 'demo1', 'ready', now() - make_interval(mins => $4), now(), 1610612736)",
        )
        .bind(bid)
        .bind(format!("rec-{bid}-0000000{i}"))
        .bind(&file)
        .bind(ago)
        .execute(&db)
        .await
        .unwrap();
    }
    sync(&db, &api, &cfg).await.unwrap();
    let left: Vec<String> =
        sqlx::query_scalar("SELECT file_name FROM recordings ORDER BY file_name")
            .fetch_all(&db)
            .await
            .unwrap();
    assert_eq!(
        left,
        [
            format!("rec-{bid}-00000002.mp4"),
            format!("rec-{bid}-00000003.mp4")
        ]
    );
    assert!(!dir.join(format!("rec-{bid}-00000001.mp4")).exists());
    assert!(dir.join(format!("rec-{bid}-00000003.mp4")).exists());
    let _ = std::fs::remove_dir_all(dir);
}

#[sqlx::test]
async fn vod_only_serves_ready_files_with_valid_names(db: PgPool) {
    let dir = temp_dir();
    let (_, api) = fake_ome(dir.clone()).await;
    let r = router(db.clone(), api, Some(dir.clone()));
    // 기록 없는 파일, 이름 규칙에 안 맞는 파일, 경로를 빠져나가려는 이름
    std::fs::write(dir.join("rec-1-00000000.mp4"), b"x").unwrap();
    std::fs::write(dir.join("secret.txt"), b"x").unwrap();
    for path in [
        "/vod/rec-1-00000000.mp4",
        "/vod/secret.txt",
        "/vod/..%2Fsecret.txt",
        "/vod/%2E%2E",
    ] {
        assert_eq!(
            call(&r, "GET", path, None, None, None).await.0,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    assert_eq!(
        call(&r, "GET", "/recordings/999", None, None, None).await.0,
        StatusCode::NOT_FOUND
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[sqlx::test]
async fn recording_is_off_without_a_folder(db: PgPool) {
    let dir = temp_dir();
    let (fake, api) = fake_ome(dir.clone()).await;
    let r = router(db.clone(), api.clone(), None);
    let (_, stream) = live_broadcast(&r, &db, "폴더 없음").await;
    fake.streams.lock().unwrap().insert(stream, "H264".into());
    sync(&db, &api, &RecConfig::default()).await.unwrap();
    assert!(statuses(&db).await.is_empty());
    assert!(fake.records.lock().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(dir);
}
