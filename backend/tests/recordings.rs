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
    /// 스트림 이름 → WebRTC 시청자 수(통계 API, #85)
    viewers: Arc<Mutex<HashMap<String, i64>>>,
    /// 녹화 id → (스트림, 파일 경로, 출력 이름들)
    records: Arc<Mutex<HashMap<String, Rec>>>,
    stopped: Arc<Mutex<Vec<String>>>,
    fail_start: Arc<Mutex<bool>>,
    /// 멈출 때 파일을 만들지 않는다(파일이 안 생기는 경우)
    no_file: Arc<Mutex<bool>>,
    /// 녹화 멈춤이 실패한다(OME 잠깐 끊김)
    fail_stop: Arc<Mutex<bool>>,
    /// 앞으로 이 횟수만큼 멈춤을 실패시킨다(다시 시도 경로 시험, #62 L4)
    fail_stops_left: Arc<Mutex<u32>>,
    /// 멈춤 요청을 받은 횟수(실패 포함)
    stop_calls: Arc<Mutex<u32>>,
    /// 녹화 목록 조회가 실패한다
    records_unavailable: Arc<Mutex<bool>>,
    /// 영상 코덱을 물어본 횟수
    codec_calls: Arc<Mutex<u32>>,
    /// 녹화 멈춤에 답하기 전에 기다리는 시간(ms). 진짜 OME가 웹훅 답을 기다리는 동안 늦게 답하는 것을 흉내 낸다.
    stop_delay_ms: Arc<Mutex<u64>>,
    /// 녹화를 거는 순간 송출이 닫힌 것처럼 방송을 ready로 바꾼다(#59 리뷰)
    close_on_start: Arc<Mutex<Option<PgPool>>>,
    /// 녹화를 거는 순간 이 SQL을 실행한다(제목 수정, 다시 송출 등을 흉내, #62)
    sql_on_start: Arc<Mutex<Option<(PgPool, &'static str)>>>,
    /// 코덱을 묻는 순간(녹화 기록을 넣기 직전) 이 SQL을 실행한다(#62 L2)
    sql_on_codec: Arc<Mutex<Option<(PgPool, &'static str)>>>,
    dir: Arc<PathBuf>,
}

async fn fake_ome(dir: PathBuf) -> (FakeOme, OmeApi) {
    let fake = FakeOme {
        dir: Arc::new(dir),
        ..Default::default()
    };
    let r = Router::new()
        .route(
            "/v1/stats/current/vhosts/default/apps/app/streams/{name}",
            get(|State(f): State<FakeOme>, Path(name): Path<String>| async move {
                match f.viewers.lock().unwrap().get(&name) {
                    // 실제 OME처럼 LL-HLS 수는 시청자와 상관없이 0이 아니게 둔다
                    Some(n) => (
                        StatusCode::OK,
                        Json(json!({ "response": { "connections": { "webrtc": n, "llhls": 8, "file": 1 } } })),
                    ),
                    None => (StatusCode::NOT_FOUND, Json(json!({ "statusCode": 404 }))),
                }
            }),
        )
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
                *f.codec_calls.lock().unwrap() += 1;
                let hook = f.sql_on_codec.lock().unwrap().take();
                if let Some((db, sql)) = hook {
                    sqlx::query(sql).execute(&db).await.unwrap();
                }
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
                let ok = |c: StatusCode| (c, Json(json!({ "statusCode": c.as_u16() })));
                match action.as_str() {
                    // 지금 들고 있는 녹화 목록(멈추지 않은 것)
                    "app:records" => {
                        if *f.records_unavailable.lock().unwrap() {
                            return ok(StatusCode::INTERNAL_SERVER_ERROR);
                        }
                        let stopped = f.stopped.lock().unwrap().clone();
                        let list: Vec<Value> = f
                            .records
                            .lock()
                            .unwrap()
                            .keys()
                            .filter(|k| !stopped.contains(k))
                            .map(|k| json!({ "id": k, "state": "recording" }))
                            .collect();
                        (StatusCode::OK, Json(json!({ "statusCode": 200, "response": list })))
                    }
                    "app:startRecord" => {
                        if *f.fail_start.lock().unwrap() {
                            return ok(StatusCode::INTERNAL_SERVER_ERROR);
                        }
                        let hook = f.sql_on_start.lock().unwrap().take();
                        if let Some((db, sql)) = hook {
                            sqlx::query(sql).execute(&db).await.unwrap();
                        }
                        let close = f.close_on_start.lock().unwrap().clone();
                        if let Some(db) = close {
                            sqlx::query("UPDATE broadcasts SET status = 'ready', updated_at = now()")
                                .execute(&db)
                                .await
                                .unwrap();
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
                        ok(StatusCode::OK)
                    }
                    "app:stopRecord" => {
                        let delay = *f.stop_delay_ms.lock().unwrap();
                        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                        *f.stop_calls.lock().unwrap() += 1;
                        if *f.fail_stop.lock().unwrap() {
                            return ok(StatusCode::INTERNAL_SERVER_ERROR);
                        }
                        {
                            let mut left = f.fail_stops_left.lock().unwrap();
                            if *left > 0 {
                                *left -= 1;
                                return ok(StatusCode::INTERNAL_SERVER_ERROR);
                            }
                        }
                        f.stopped.lock().unwrap().push(id.clone());
                        if let Some((_, path, _)) = f.records.lock().unwrap().get(&id)
                                && !*f.no_file.lock().unwrap()
                        {
                                let file = f.dir.join(path.trim_start_matches('/'));
                                std::fs::write(file, fake_video()).unwrap();
                        }
                        ok(StatusCode::OK)
                    }
                    _ => ok(StatusCode::NOT_FOUND),
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

/// 가짜 녹화 파일 내용(70KB, 앞 20바이트는 알아보기 쉬운 글자). 64KB보다 작으면 너무 짧은 녹화로 본다.
fn fake_video() -> Vec<u8> {
    let mut v = b"0123456789abcdefghij".to_vec();
    v.resize(70_000, 46);
    v
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
    assert_eq!(list[0]["bytes"], 70_000);
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
    assert_eq!(body, fake_video());
    let (st, _, body, _) = call(&r, "GET", &url, None, None, Some("bytes=10-14")).await;
    assert_eq!(st, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, b"abcde");
    // 영상은 쓸 때마다 서버에 확인하는 캐시(no-store 아님), 목록 같은 API는 캐시하지 않음(#25 A16)
    let cache = |path: String| {
        let r = r.clone();
        async move {
            let res = r
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            res.headers()
                .get(header::CACHE_CONTROL)
                .map(|v| v.to_str().unwrap().to_string())
        }
    };
    assert_eq!(
        cache(url.clone()).await.as_deref(),
        Some("private, no-cache")
    );
    assert_eq!(
        cache("/recordings".into()).await.as_deref(),
        Some("no-store")
    );
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

#[sqlx::test]
async fn failed_stop_keeps_recording_and_retries(db: PgPool) {
    let dir = temp_dir();
    let (fake, api) = fake_ome(dir.clone()).await;
    let cfg = RecConfig {
        dir: Some(dir.clone()),
    };
    let r = router(db.clone(), api.clone(), Some(dir.clone()));
    let (bid, stream) = live_broadcast(&r, &db, "멈춤 실패").await;
    fake.streams.lock().unwrap().insert(stream, "H264".into());
    sync(&db, &api, &cfg).await.unwrap();
    sqlx::query("UPDATE broadcasts SET status = 'ended' WHERE id = $1")
        .bind(bid)
        .execute(&db)
        .await
        .unwrap();
    // OME가 멈춤을 거절하면 녹화 중으로 남는다(새 녹화가 옆에서 시작되지 않는다)
    *fake.fail_stop.lock().unwrap() = true;
    sync(&db, &api, &cfg).await.unwrap();
    assert_eq!(statuses(&db).await, ["recording"]);
    // 다음 차례에 다시 멈춘다
    *fake.fail_stop.lock().unwrap() = false;
    sync(&db, &api, &cfg).await.unwrap();
    sync(&db, &api, &cfg).await.unwrap();
    assert_eq!(statuses(&db).await, ["ready"]);
    assert_eq!(fake.records.lock().unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[sqlx::test]
async fn recording_started_with_the_broadcast_is_kept(db: PgPool) {
    // #62 조건의 경계: 녹화 시작 시각이 방송 시작 시각과 같으면 지금 송출의 녹화다. 멈추지 않는다(#71 R4).
    let dir = temp_dir();
    let (fake, api) = fake_ome(dir.clone()).await;
    let cfg = RecConfig {
        dir: Some(dir.clone()),
    };
    let r = router(db.clone(), api.clone(), Some(dir.clone()));
    let (bid, stream) = live_broadcast(&r, &db, "같은 시각").await;
    fake.streams.lock().unwrap().insert(stream, "H264".into());
    sync(&db, &api, &cfg).await.unwrap();
    sqlx::query(
        "UPDATE recordings SET started_at = (SELECT started_at FROM broadcasts WHERE id = $1)",
    )
    .bind(bid)
    .execute(&db)
    .await
    .unwrap();
    sync(&db, &api, &cfg).await.unwrap();
    assert!(fake.stopped.lock().unwrap().is_empty());
    assert_eq!(statuses(&db).await, ["recording"]);
    let _ = std::fs::remove_dir_all(dir);
}

#[sqlx::test]
async fn missing_replay_file_is_not_cacheable(db: PgPool) {
    // 기록은 ready인데 파일이 없으면 404이고, 확인 후 캐시(private, no-cache)가 아니라 no-store다(#71 R1)
    let dir = temp_dir();
    let (_, api) = fake_ome(dir.clone()).await;
    let r = router(db.clone(), api, Some(dir.clone()));
    let (bid, _) = live_broadcast(&r, &db, "파일 없음").await;
    sqlx::query(
        "INSERT INTO recordings (broadcast_id, record_id, file_name, format, title, owner_username, status, bytes, ended_at)
         VALUES ($1, 'rec-x', 'rec-1-00000001.mp4', 'mp4', 'x', 'demo1', 'ready', 70000, now())",
    )
    .bind(bid)
    .execute(&db)
    .await
    .unwrap();
    let res = r
        .clone()
        .oneshot(
            Request::builder()
                .uri("/vod/rec-1-00000001.mp4")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        res.headers()
            .get(header::CACHE_CONTROL)
            .map(|v| v.to_str().unwrap()),
        Some("no-store")
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[sqlx::test]
async fn recording_left_from_an_earlier_publish_is_stopped(db: PgPool) {
    // #62 M2: 송출이 닫힌 뒤 멈춤이 계속 실패하고, 그사이 방송자가 다시 붙었다.
    // 지난 송출의 녹화는 멈추고, 지금 송출을 새로 녹화한다(한 파일에 이어 붙지 않는다).
    let dir = temp_dir();
    let (fake, api) = fake_ome(dir.clone()).await;
    let cfg = RecConfig {
        dir: Some(dir.clone()),
    };
    let r = router(db.clone(), api.clone(), Some(dir.clone()));
    let (bid, stream) = live_broadcast(&r, &db, "다시 붙음").await;
    fake.streams.lock().unwrap().insert(stream, "H264".into());
    sync(&db, &api, &cfg).await.unwrap();
    assert_eq!(statuses(&db).await, ["recording"]);
    let first: String = sqlx::query_scalar("SELECT record_id FROM recordings")
        .fetch_one(&db)
        .await
        .unwrap();
    // 송출이 닫혔는데 멈춤이 실패했다
    sqlx::query("UPDATE recordings SET started_at = now() - interval '1 minute'")
        .execute(&db)
        .await
        .unwrap();
    sqlx::query("UPDATE broadcasts SET status = 'ready' WHERE id = $1")
        .bind(bid)
        .execute(&db)
        .await
        .unwrap();
    *fake.fail_stop.lock().unwrap() = true;
    sync(&db, &api, &cfg).await.unwrap();
    assert_eq!(statuses(&db).await, ["recording"]);
    // 방송자가 다시 붙었다(opening 웹훅처럼 시작 시각이 새로 정해진다). 이번에는 멈춤이 된다.
    sqlx::query("UPDATE broadcasts SET status = 'live', started_at = now(), updated_at = now() WHERE id = $1")
        .bind(bid)
        .execute(&db)
        .await
        .unwrap();
    *fake.fail_stop.lock().unwrap() = false;
    sync(&db, &api, &cfg).await.unwrap();
    assert!(fake.stopped.lock().unwrap().contains(&first));
    // 지난 녹화는 끝났고, 지금 송출은 새 녹화로 간다
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT record_id, status FROM recordings ORDER BY id")
            .fetch_all(&db)
            .await
            .unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0].0, first);
    assert_ne!(rows[0].1, "recording");
    assert_eq!(rows[1].1, "recording");
    let _ = std::fs::remove_dir_all(dir);
}

/// 송출이 닫힌 뒤의 녹화 멈춤(stop_after_closing)을 시험하려고 녹화 중인 기록 하나를 만든다
async fn one_recording(db: &PgPool) -> (FakeOme, OmeApi, PathBuf, String) {
    let dir = temp_dir();
    let (fake, api) = fake_ome(dir.clone()).await;
    let cfg = RecConfig {
        dir: Some(dir.clone()),
    };
    let r = router(db.clone(), api.clone(), Some(dir.clone()));
    let (_, stream) = live_broadcast(&r, db, "닫힌 뒤 멈춤").await;
    fake.streams.lock().unwrap().insert(stream, "H264".into());
    sync(db, &api, &cfg).await.unwrap();
    let rid: String = sqlx::query_scalar("SELECT record_id FROM recordings")
        .fetch_one(db)
        .await
        .unwrap();
    (fake, api, dir, rid)
}

#[sqlx::test]
async fn stop_after_closing_retries_until_ome_stops(db: PgPool) {
    // #62 L4: OME가 멈춤을 두 번 거절해도 세 번째에 멈추고 기록을 finishing으로 바꾼다
    let (fake, api, dir, rid) = one_recording(&db).await;
    *fake.fail_stops_left.lock().unwrap() = 2;
    let kick = onair_backend::recordings::Kick::default();
    onair_backend::ome::stop_after_closing(db.clone(), api, kick.clone(), vec![rid.clone()]).await;
    assert_eq!(*fake.stop_calls.lock().unwrap(), 3);
    assert_eq!(fake.stopped.lock().unwrap().as_slice(), [rid]);
    assert_eq!(statuses(&db).await, ["finishing"]);
    // 끝나면 녹화 맞춰 보기를 깨운다
    tokio::time::timeout(std::time::Duration::from_secs(1), kick.wait())
        .await
        .expect("kick");
    let _ = std::fs::remove_dir_all(dir);
}

#[sqlx::test]
async fn stop_after_closing_gives_up_after_three_tries(db: PgPool) {
    // #62 L4: 세 번 모두 실패하면 녹화 중으로 남기고(다음 맞춰 보기의 stop_finished가 다시 멈춘다) 깨우기는 보낸다
    let (fake, api, dir, rid) = one_recording(&db).await;
    *fake.fail_stop.lock().unwrap() = true;
    let kick = onair_backend::recordings::Kick::default();
    onair_backend::ome::stop_after_closing(db.clone(), api, kick.clone(), vec![rid]).await;
    assert_eq!(*fake.stop_calls.lock().unwrap(), 3);
    assert_eq!(statuses(&db).await, ["recording"]);
    tokio::time::timeout(std::time::Duration::from_secs(1), kick.wait())
        .await
        .expect("kick");
    let _ = std::fs::remove_dir_all(dir);
}

#[sqlx::test]
async fn live_broadcasts_show_webrtc_viewer_count(db: PgPool) {
    // #85: 방송 중이면 OME 통계의 WebRTC 접속 수를 시청자 수로 보인다. LL-HLS 수는 쓰지 않는다.
    let dir = temp_dir();
    let (fake, api) = fake_ome(dir.clone()).await;
    let r = router(db.clone(), api, None);
    let (bid, stream) = live_broadcast(&r, &db, "시청자 수").await;
    fake.viewers.lock().unwrap().insert(stream.clone(), 3);
    let (st, b, _, _) = call(&r, "GET", &format!("/broadcasts/{bid}"), None, None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(b["viewers"], 3);
    let (_, list, _, _) = call(&r, "GET", "/broadcasts", None, None, None).await;
    assert_eq!(list[0]["viewers"], 3);
    // 5초 동안은 모아 둔 값을 쓴다(OME에 매번 묻지 않음)
    fake.viewers.lock().unwrap().insert(stream, 5);
    assert_eq!(
        call(&r, "GET", &format!("/broadcasts/{bid}"), None, None, None)
            .await
            .1["viewers"],
        3
    );
    // 방송 중이 아니면 null
    sqlx::query("UPDATE broadcasts SET status = 'ready' WHERE id = $1")
        .bind(bid)
        .execute(&db)
        .await
        .unwrap();
    assert!(
        call(&r, "GET", &format!("/broadcasts/{bid}"), None, None, None)
            .await
            .1["viewers"]
            .is_null()
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[sqlx::test]
async fn viewer_count_is_null_when_ome_has_no_stats(db: PgPool) {
    // OME가 통계를 주지 않으면(스트림이 아직 없음 등) 시청자 수는 null이고 방송 조회는 그대로 된다
    let dir = temp_dir();
    let (_, api) = fake_ome(dir.clone()).await;
    let r = router(db.clone(), api, None);
    let (bid, _) = live_broadcast(&r, &db, "통계 없음").await;
    let (st, b, _, _) = call(&r, "GET", &format!("/broadcasts/{bid}"), None, None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(b["viewers"].is_null());
    assert_eq!(b["status"], "live");
    let _ = std::fs::remove_dir_all(dir);
}

#[sqlx::test]
async fn recording_lost_by_ome_is_restarted(db: PgPool) {
    let dir = temp_dir();
    let (fake, api) = fake_ome(dir.clone()).await;
    let cfg = RecConfig {
        dir: Some(dir.clone()),
    };
    let r = router(db.clone(), api.clone(), Some(dir.clone()));
    let (_, stream) = live_broadcast(&r, &db, "OME 재시작").await;
    fake.streams.lock().unwrap().insert(stream, "H264".into());
    sync(&db, &api, &cfg).await.unwrap();
    // OME가 다시 켜져서 녹화 목록을 잊었다. 같은 송출 안에서 1분째 녹화 중이었다
    // (방송 시작 시각도 함께 당긴다. 녹화가 방송보다 먼저 시작했으면 지난 송출의 녹화로 보고 멈춘다, #62)
    sqlx::query("UPDATE broadcasts SET started_at = now() - interval '2 minutes'")
        .execute(&db)
        .await
        .unwrap();
    sqlx::query("UPDATE recordings SET started_at = now() - interval '1 minute'")
        .execute(&db)
        .await
        .unwrap();
    fake.records.lock().unwrap().clear();
    // 목록을 물어볼 수 없으면 아무것도 바꾸지 않는다
    *fake.records_unavailable.lock().unwrap() = true;
    sync(&db, &api, &cfg).await.unwrap();
    assert_eq!(statuses(&db).await, ["recording"]);
    *fake.records_unavailable.lock().unwrap() = false;
    sync(&db, &api, &cfg).await.unwrap();
    // 잃어버린 녹화는 끝난 것으로 보고, 방송이 계속되므로 새 녹화를 시작한다
    let s = statuses(&db).await;
    assert_eq!(s.len(), 2, "{s:?}");
    assert_eq!(s[1], "recording");
    assert!(s[0] == "finishing" || s[0] == "failed", "{s:?}");
    let _ = std::fs::remove_dir_all(dir);
}

#[sqlx::test]
async fn at_most_three_recordings_at_once(db: PgPool) {
    let dir = temp_dir();
    let (fake, api) = fake_ome(dir.clone()).await;
    let cfg = RecConfig {
        dir: Some(dir.clone()),
    };
    let r = router(db.clone(), api.clone(), Some(dir.clone()));
    for i in 0..4 {
        let (_, stream) = live_broadcast(&r, &db, &format!("방송 {i}")).await;
        fake.streams.lock().unwrap().insert(stream, "H264".into());
    }
    sync(&db, &api, &cfg).await.unwrap();
    assert_eq!(statuses(&db).await.len(), 3);
    let _ = std::fs::remove_dir_all(dir);
}

#[sqlx::test]
async fn deleting_a_broadcast_stops_its_recording_and_files_are_cleaned(db: PgPool) {
    let dir = temp_dir();
    let (fake, api) = fake_ome(dir.clone()).await;
    let cfg = RecConfig {
        dir: Some(dir.clone()),
    };
    let r = router(db.clone(), api.clone(), Some(dir.clone()));
    let cookie = call(&r, "POST", "/auth/demo/1", None, None, None)
        .await
        .3
        .unwrap();
    let (bid, stream) = live_broadcast(&r, &db, "삭제").await;
    fake.streams.lock().unwrap().insert(stream, "H264".into());
    sync(&db, &api, &cfg).await.unwrap();
    assert_eq!(
        call(
            &r,
            "DELETE",
            &format!("/broadcasts/{bid}"),
            None,
            Some(&cookie),
            None
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(fake.stopped.lock().unwrap().len(), 1, "녹화를 먼저 멈춘다");
    assert!(statuses(&db).await.is_empty());
    // 기록 없는 녹화 파일: 10분이 지나면 지운다. 방금 생긴 파일은 남긴다(쓰는 중일 수 있다)
    let file = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .find(|e| e.file_name().to_string_lossy().starts_with("rec-"))
        .unwrap()
        .path();
    sync(&db, &api, &cfg).await.unwrap();
    assert!(file.exists());
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(11 * 60);
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(old)
        .unwrap();
    std::fs::write(dir.join("notes.txt"), b"keep").unwrap();
    sync(&db, &api, &cfg).await.unwrap();
    assert!(!file.exists());
    assert!(
        dir.join("notes.txt").exists(),
        "규칙에 맞지 않는 파일은 건드리지 않는다"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[sqlx::test]
async fn cleanup_keeps_known_old_files_and_removes_stale_temp_files(db: PgPool) {
    let dir = temp_dir();
    let (_, api) = fake_ome(dir.clone()).await;
    let cfg = RecConfig {
        dir: Some(dir.clone()),
    };
    let r = router(db.clone(), api.clone(), Some(dir.clone()));
    let (bid, _) = live_broadcast(&r, &db, "정리").await;
    sqlx::query("UPDATE broadcasts SET status = 'ended' WHERE id = $1")
        .bind(bid)
        .execute(&db)
        .await
        .unwrap();
    let ready = format!("rec-{bid}-0000000a.mp4");
    sqlx::query(
        "INSERT INTO recordings (broadcast_id, record_id, file_name, format, title, owner_username, status, ended_at, bytes)
         VALUES ($1, $2, $3, 'mp4', 't', 'demo1', 'ready', now(), 1)",
    )
    .bind(bid)
    .bind(format!("rec-{bid}-0000000a"))
    .bind(&ready)
    .execute(&db)
    .await
    .unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(4 * 60 * 60);
    for name in [
        ready.as_str(),
        "tmp_rec-1-00000000.mp4",
        "00aec8f16dfeefdc.xml",
        "rec-1-0000000b.mp4",
    ] {
        std::fs::write(dir.join(name), b"x").unwrap();
        std::fs::File::options()
            .write(true)
            .open(dir.join(name))
            .unwrap()
            .set_modified(old)
            .unwrap();
    }
    std::fs::write(dir.join("tmp_fresh.mp4"), b"x").unwrap();
    sync(&db, &api, &cfg).await.unwrap();
    assert!(dir.join(&ready).exists(), "다시보기 파일은 오래돼도 남긴다");
    assert!(!dir.join("tmp_rec-1-00000000.mp4").exists());
    assert!(!dir.join("00aec8f16dfeefdc.xml").exists());
    assert!(
        !dir.join("rec-1-0000000b.mp4").exists(),
        "기록 없는 녹화 파일"
    );
    assert!(
        dir.join("tmp_fresh.mp4").exists(),
        "쓰는 중일 수 있는 새 임시 파일은 남긴다"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[sqlx::test]
async fn too_short_recordings_are_not_listed(db: PgPool) {
    let dir = temp_dir();
    let (fake, api) = fake_ome(dir.clone()).await;
    let cfg = RecConfig {
        dir: Some(dir.clone()),
    };
    let r = router(db.clone(), api.clone(), Some(dir.clone()));
    let (bid, stream) = live_broadcast(&r, &db, "짧음").await;
    fake.streams.lock().unwrap().insert(stream, "H264".into());
    *fake.no_file.lock().unwrap() = true;
    sync(&db, &api, &cfg).await.unwrap();
    let file: String = sqlx::query_scalar("SELECT file_name FROM recordings")
        .fetch_one(&db)
        .await
        .unwrap();
    std::fs::write(dir.join(&file), vec![0u8; 10_000]).unwrap();
    sqlx::query("UPDATE broadcasts SET status = 'ended' WHERE id = $1")
        .bind(bid)
        .execute(&db)
        .await
        .unwrap();
    sync(&db, &api, &cfg).await.unwrap();
    sync(&db, &api, &cfg).await.unwrap();
    assert_eq!(statuses(&db).await, ["failed"]);
    assert_eq!(
        call(&r, "GET", "/recordings", None, None, None).await.1,
        json!([])
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[sqlx::test]
async fn skipped_broadcasts_do_not_use_up_recording_slots(db: PgPool) {
    let dir = temp_dir();
    let (fake, api) = fake_ome(dir.clone()).await;
    let cfg = RecConfig {
        dir: Some(dir.clone()),
    };
    let r = router(db.clone(), api.clone(), Some(dir.clone()));
    // 먼저 시작한 방송 3개는 녹화할 수 없는 코덱, 나중 방송 1개는 H.264
    for i in 0..3 {
        let (_, stream) = live_broadcast(&r, &db, &format!("H265 {i}")).await;
        fake.streams.lock().unwrap().insert(stream, "H265".into());
    }
    let (late, stream) = live_broadcast(&r, &db, "나중 방송").await;
    fake.streams.lock().unwrap().insert(stream, "H264".into());
    sync(&db, &api, &cfg).await.unwrap();
    let rec: Vec<i64> = sqlx::query_scalar("SELECT broadcast_id FROM recordings")
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(rec, [late]);
    let _ = std::fs::remove_dir_all(dir);
}

#[sqlx::test]
async fn start_soon_starts_as_soon_as_the_codec_is_known(db: PgPool) {
    use onair_backend::recordings::start_soon;
    use std::time::{Duration, Instant};
    let dir = temp_dir();
    let (fake, api) = fake_ome(dir.clone()).await;
    let cfg = RecConfig { dir: Some(dir) };
    let r = router(db.clone(), api.clone(), cfg.dir.clone());
    let (_, stream) = live_broadcast(&r, &db, "바로 녹화").await;
    // 웹훅 순간엔 OME에 스트림이 없어 코덱을 모른다. 0.15초 뒤에 생긴다.
    let f = fake.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        f.streams.lock().unwrap().insert(stream, "H264".into());
    });
    let t = Instant::now();
    start_soon(&db, &api, &cfg, Duration::from_millis(50), 20)
        .await
        .unwrap();
    // 코덱이 생기자마자 녹화가 시작되고, 남은 시도는 하지 않는다
    assert_eq!(statuses(&db).await, ["recording"]);
    assert!(
        t.elapsed() < Duration::from_millis(600),
        "{:?}",
        t.elapsed()
    );
}

#[sqlx::test]
async fn start_soon_gives_up_after_its_tries(db: PgPool) {
    use onair_backend::recordings::start_soon;
    use std::time::Duration;
    let dir = temp_dir();
    let (_fake, api) = fake_ome(dir.clone()).await;
    let cfg = RecConfig { dir: Some(dir) };
    let r = router(db.clone(), api.clone(), cfg.dir.clone());
    live_broadcast(&r, &db, "코덱 없음").await;
    // 코덱을 끝내 모르면 정해진 횟수만 시도하고 끝난다(15초 맞춰 보기가 이어서 맡는다)
    // 시도 사이에는 쉰다(쉬지 않으면 OME가 코덱을 알기도 전에 10번을 다 써 버린다, #59 변이 시험)
    let t = std::time::Instant::now();
    tokio::time::timeout(
        Duration::from_secs(2),
        start_soon(&db, &api, &cfg, Duration::from_millis(20), 5),
    )
    .await
    .expect("정해진 횟수 뒤에 끝나야 한다")
    .unwrap();
    assert!(statuses(&db).await.is_empty());
    assert_eq!(*_fake.codec_calls.lock().unwrap(), 5);
    assert!(
        t.elapsed() >= Duration::from_millis(80),
        "{:?}",
        t.elapsed()
    );
}

#[sqlx::test]
async fn start_soon_does_not_wait_when_all_recording_slots_are_full(db: PgPool) {
    use onair_backend::recordings::start_soon;
    use std::time::Duration;
    let dir = temp_dir();
    let (fake, api) = fake_ome(dir.clone()).await;
    let cfg = RecConfig { dir: Some(dir) };
    let r = router(db.clone(), api.clone(), cfg.dir.clone());
    for i in 0..4 {
        let (_, s) = live_broadcast(&r, &db, &format!("방송{i}")).await;
        fake.streams.lock().unwrap().insert(s, "H264".into());
    }
    // 4개 중 3개만 녹화할 수 있다. 남은 1개를 위해 헛되이 10번 돌지 않는다.
    // 자리가 없으면 쉬지 않고 바로 끝난다(전에는 100ms씩 9번 더 쉬어 0.9초 넘게 걸렸다, #59 2바퀴)
    let t = std::time::Instant::now();
    start_soon(&db, &api, &cfg, Duration::from_millis(100), 10)
        .await
        .unwrap();
    assert_eq!(statuses(&db).await.len(), 3);
    assert!(
        t.elapsed() < Duration::from_millis(500),
        "{:?}",
        t.elapsed()
    );
    assert_eq!(*fake.codec_calls.lock().unwrap(), 3);
}

#[sqlx::test]
async fn recording_started_while_the_stream_closes_is_stopped_at_once(db: PgPool) {
    let dir = temp_dir();
    let (fake, api) = fake_ome(dir.clone()).await;
    let cfg = RecConfig { dir: Some(dir) };
    let r = router(db.clone(), api.clone(), cfg.dir.clone());
    let (_, stream) = live_broadcast(&r, &db, "닫히는 송출").await;
    fake.streams.lock().unwrap().insert(stream, "H264".into());
    // 녹화를 거는 순간 송출이 닫힌다 → 그 녹화는 바로 멈춘다(다시 붙은 송출이 같은 파일에 이어 써지지 않게)
    *fake.close_on_start.lock().unwrap() = Some(db.clone());
    sync(&db, &api, &cfg).await.unwrap();
    assert_eq!(fake.stopped.lock().unwrap().len(), 1);
    let s = statuses(&db).await;
    assert!(s == ["finishing"] || s == ["ready"], "{s:?}");
}

#[sqlx::test]
async fn editing_the_title_while_recording_starts_keeps_the_recording(db: PgPool) {
    // #62 L1: 녹화를 거는 순간 제목을 고쳐도(updated_at만 바뀜) 막 건 녹화를 멈추지 않는다
    let dir = temp_dir();
    let (fake, api) = fake_ome(dir.clone()).await;
    let cfg = RecConfig {
        dir: Some(dir.clone()),
    };
    let r = router(db.clone(), api.clone(), Some(dir.clone()));
    let (_, stream) = live_broadcast(&r, &db, "제목 수정").await;
    fake.streams.lock().unwrap().insert(stream, "H264".into());
    *fake.sql_on_start.lock().unwrap() = Some((
        db.clone(),
        "UPDATE broadcasts SET title = '바뀐 제목', updated_at = now() + interval '1 second'",
    ));
    sync(&db, &api, &cfg).await.unwrap();
    assert!(fake.stopped.lock().unwrap().is_empty());
    assert_eq!(statuses(&db).await, ["recording"]);
    let _ = std::fs::remove_dir_all(dir);
}

#[sqlx::test]
async fn republishing_while_recording_starts_stops_that_recording(db: PgPool) {
    // 녹화를 거는 사이 송출이 닫혔다가 다시 열리면(시작 시각이 새로 정해짐) 그 녹화는 바로 멈춘다(#59, #62)
    let dir = temp_dir();
    let (fake, api) = fake_ome(dir.clone()).await;
    let cfg = RecConfig {
        dir: Some(dir.clone()),
    };
    let r = router(db.clone(), api.clone(), Some(dir.clone()));
    let (_, stream) = live_broadcast(&r, &db, "다시 송출").await;
    fake.streams.lock().unwrap().insert(stream, "H264".into());
    *fake.sql_on_start.lock().unwrap() = Some((
        db.clone(),
        "UPDATE broadcasts SET status = 'live', started_at = now() + interval '1 second', updated_at = now()",
    ));
    sync(&db, &api, &cfg).await.unwrap();
    assert_eq!(fake.stopped.lock().unwrap().len(), 1);
    let s = statuses(&db).await;
    assert!(s == ["finishing"] || s == ["ready"], "{s:?}");
    let _ = std::fs::remove_dir_all(dir);
}

#[sqlx::test]
async fn editing_the_title_before_insert_still_records(db: PgPool) {
    // #62 L1(기록 넣는 쪽): 방송을 읽은 뒤 기록을 넣기 전에 제목만 고쳐도 녹화한다
    let dir = temp_dir();
    let (fake, api) = fake_ome(dir.clone()).await;
    let cfg = RecConfig {
        dir: Some(dir.clone()),
    };
    let r = router(db.clone(), api.clone(), Some(dir.clone()));
    let (_, stream) = live_broadcast(&r, &db, "넣기 전 제목 수정").await;
    fake.streams.lock().unwrap().insert(stream, "H264".into());
    *fake.sql_on_codec.lock().unwrap() = Some((
        db.clone(),
        "UPDATE broadcasts SET title = '바뀐 제목', updated_at = now() + interval '1 second'",
    ));
    sync(&db, &api, &cfg).await.unwrap();
    assert_eq!(statuses(&db).await, ["recording"]);
    assert!(fake.stopped.lock().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[sqlx::test]
async fn broadcast_changed_before_insert_gets_no_recording(db: PgPool) {
    // #62 L2: 방송을 읽은 뒤 기록을 넣기 전에 송출이 다시 열리면 기록을 넣지 않는다(녹화도 걸지 않는다)
    let dir = temp_dir();
    let (fake, api) = fake_ome(dir.clone()).await;
    let cfg = RecConfig {
        dir: Some(dir.clone()),
    };
    let r = router(db.clone(), api.clone(), Some(dir.clone()));
    let (_, stream) = live_broadcast(&r, &db, "넣기 전 바뀜").await;
    fake.streams.lock().unwrap().insert(stream, "H264".into());
    *fake.sql_on_codec.lock().unwrap() = Some((
        db.clone(),
        "UPDATE broadcasts SET started_at = now() + interval '1 second'",
    ));
    sync(&db, &api, &cfg).await.unwrap();
    assert!(statuses(&db).await.is_empty());
    assert!(fake.records.lock().unwrap().is_empty());
    // 다음 차례에는 지금 송출로 녹화한다
    sync(&db, &api, &cfg).await.unwrap();
    assert_eq!(statuses(&db).await, ["recording"]);
    let _ = std::fs::remove_dir_all(dir);
}

#[sqlx::test]
async fn closing_webhook_answers_at_once_and_stops_the_recording_afterwards(db: PgPool) {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use hmac::{Hmac, KeyInit, Mac};
    use std::time::{Duration, Instant};
    let dir = temp_dir();
    let (fake, api) = fake_ome(dir.clone()).await;
    let cfg = RecConfig { dir: Some(dir) };
    let r = router(db.clone(), api.clone(), cfg.dir.clone());
    let (_, stream) = live_broadcast(&r, &db, "닫힘 시험").await;
    fake.streams
        .lock()
        .unwrap()
        .insert(stream.clone(), "H264".into());
    sync(&db, &api, &cfg).await.unwrap();
    assert_eq!(statuses(&db).await, ["recording"]);

    // OME가 녹화 멈춤에 늦게 답한다(웹훅 답을 기다리는 동안). 웹훅은 그걸 기다리지 않고 바로 답해야 한다.
    *fake.stop_delay_ms.lock().unwrap() = 700;
    let kick = onair_backend::recordings::Kick::default();
    let secret = "test-secret-for-ome-webhook";
    let hooks = onair_backend::ome_routes(onair_backend::ome::OmeState {
        secret: secret.into(),
        db: db.clone(),
        ome: api.clone(),
        kick: kick.clone(),
    });
    let body = json!({
        "client": {"address": "1.2.3.4", "port": 1},
        "request": {"direction": "incoming", "protocol": "webrtc", "status": "closing",
                    "url": format!("http://h:3333/app/{stream}"), "time": "2026-11-11T00:00:00.000+09:00"}
    })
    .to_string();
    let mut mac = Hmac::<sha1::Sha1>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body.as_bytes());
    let sig = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    let t = Instant::now();
    let res = hooks
        .oneshot(
            Request::post("/ome/admission")
                .header("content-type", "application/json")
                .header("X-OME-Signature", sig)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(
        t.elapsed() < Duration::from_millis(500),
        "{:?}",
        t.elapsed()
    );
    // 답한 뒤에 녹화가 멈추고, 멈춘 뒤 녹화 서둘러 신호가 온다(그 사이 다시 붙은 송출을 바로 녹화)
    tokio::time::timeout(Duration::from_secs(3), kick.wait())
        .await
        .expect("멈춘 뒤 신호");
    assert_eq!(fake.stopped.lock().unwrap().len(), 1);
    let s = statuses(&db).await;
    assert!(s == ["finishing"] || s == ["ready"], "{s:?}");
}
