use std::time::Duration;

use onair_backend::{AppState, app};
use sqlx::postgres::PgPoolOptions;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    // .env 파일이 있으면 읽는다. 없으면(배포 서버 등) 이미 설정된 환경변수를 그대로 쓴다.
    let _ = dotenvy::dotenv();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL 환경변수가 필요합니다");
    let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into());

    // connect_lazy: 시작할 때 DB에 바로 붙지 않고, 처음 쓸 때 붙는다.
    // DB가 조금 늦게 떠도 서버가 죽지 않게 하려는 것이다. DB 상태는 /health로 확인한다.
    let db = PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(Duration::from_secs(3))
        .connect_lazy(&database_url)
        .expect("DATABASE_URL 형식이 잘못되었습니다");

    // OME가 보내는 요청의 서명 키. docker-compose의 OME_ADMISSION_SECRET과 같아야 한다.
    // 기본값을 두지 않는다. 키가 비거나 공개된 예시 값이면 누구나 서명을 맞춰 가짜 요청을 보낼 수 있어서,
    // 그런 상태로는 아예 시작하지 않는다.
    let raw_secret = std::env::var("OME_ADMISSION_SECRET").unwrap_or_default();
    let ome_secret = match onair_backend::ome::check_secret(&raw_secret) {
        Ok(s) => s.to_string(),
        Err(why) => panic!("OME_ADMISSION_SECRET 확인 실패: {why}"),
    };
    let ome_state = onair_backend::ome::OmeState {
        secret: ome_secret,
        db: db.clone(),
    };

    // OME 관리 API(스트림 끊기, 목록). 둘 다 있을 때만 쓴다. 토큰은 OME Server.xml의 AccessToken과 같은 값이다.
    // OME_API_URL을 넣었으면 토큰도 서명 키와 같은 규칙으로 검사하고, 맞지 않으면 시작하지 않는다(#27 V5).
    // 예전에는 짧은 토큰이면 조용히 꺼져서, 배포에서 "방송 종료 때 송출 끊기"가 몰래 동작하지 않을 수 있었다.
    let api_url = std::env::var("OME_API_URL").unwrap_or_default();
    let ome_api = if api_url.trim().is_empty() {
        tracing::warn!("OME_API_URL이 없어 방송 종료 때 송출을 끊지 못합니다(개발용)");
        Default::default()
    } else {
        let raw_token = std::env::var("OME_API_TOKEN").unwrap_or_default();
        match onair_backend::ome::check_secret(&raw_token) {
            Ok(t) => onair_backend::ome_api::OmeApi::new(api_url.trim(), t),
            Err(why) => panic!("OME_API_TOKEN 확인 실패: {why}"),
        }
    };

    // DB 구조 변경을 적용한다. DB가 아직 뜨는 중일 수 있어서 2초 간격으로 30번까지 다시 시도한다.
    // 끝내 실패하면 표가 없는 채로 돌 수 없으므로 멈춘다.
    let mut tries = 0;
    loop {
        match onair_backend::migrate(&db).await {
            Ok(()) => break,
            Err(e) if tries < 30 => {
                tries += 1;
                tracing::warn!(error = %e, tries, "DB 마이그레이션 실패, 다시 시도");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(e) => panic!("DB 마이그레이션 실패: {e}"),
        }
    }

    // 심사용 테스트 계정 로그인(/auth/demo/1, /auth/demo/2). "true"일 때만 켠다.
    let auth = onair_backend::auth::AuthConfig {
        demo_login: std::env::var("DEMO_LOGIN").is_ok_and(|v| v.trim() == "true"),
        ..Default::default()
    };
    if auth.demo_login {
        tracing::info!("테스트 계정 로그인(DEMO_LOGIN)이 켜져 있습니다");
    }
    // 한 시간마다 24시간 지난 테스트 계정(guest_)을 지운다(#35). 첫 번째는 바로 돈다.
    // DEMO_LOGIN을 나중에 꺼도 남은 테스트 계정이 지워지도록 설정과 상관없이 돌린다(#35 리뷰).
    {
        let db = db.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(3600));
            loop {
                tick.tick().await;
                match onair_backend::auth::cleanup_guests(&db).await {
                    Ok(0) => {}
                    Ok(n) => tracing::info!(count = n, "지난 테스트 계정을 지웠습니다"),
                    Err(e) => tracing::warn!(error = %e, "테스트 계정 정리 실패"),
                }
            }
        });
    }

    // CORS(다른 주소에서 오는 호출 허용)는 켜지 않는다. 화면은 vite 프록시(/api)로 같은 주소에서 부르고,
    // OME 웹훅은 서버끼리 주고받으므로 필요 없다.
    // 15초마다 OME 스트림 목록과 방송 상태를 맞춘다(#10: opening/closing 알림만으로는 상태가 어긋날 수 있다).
    if ome_api.is_configured() {
        let (db, api) = (db.clone(), ome_api.clone());
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(15));
            loop {
                tick.tick().await;
                if let Err(e) = onair_backend::ome::reconcile(&db, &api).await {
                    tracing::warn!(error = %e, "방송 상태 맞춰 보기 실패");
                }
            }
        });
    }

    // 토스 결제(#41, 테스트 모드). 두 키가 다 있을 때만 켠다. 비밀 키는 로그에 남기지 않는다.
    let toss = match (
        std::env::var("TOSS_SECRET_KEY"),
        std::env::var("TOSS_CLIENT_KEY"),
    ) {
        (Ok(s), Ok(c)) if !s.trim().is_empty() && !c.trim().is_empty() => {
            let base = std::env::var("TOSS_API_BASE")
                .unwrap_or_else(|_| "https://api.tosspayments.com".into());
            tracing::info!(%base, "도네이션(토스 결제) 켜짐");
            onair_backend::toss::Toss::new(&base, s.trim(), c.trim())
        }
        _ => {
            tracing::warn!("TOSS_SECRET_KEY·TOSS_CLIENT_KEY가 없어 도네이션이 꺼져 있습니다");
            Default::default()
        }
    };

    let router = app(AppState {
        db,
        auth,
        ome: ome_api,
        toss,
    })
    .merge(onair_backend::ome_routes(ome_state));

    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .expect("주소에 묶지 못했습니다");
    tracing::info!("onair backend listening on {bind_addr}");
    axum::serve(listener, router).await.expect("서버 오류");
}
