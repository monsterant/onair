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
    let ome_secret =
        std::env::var("OME_ADMISSION_SECRET").unwrap_or_else(|_| "change-me-local-only".into());
    let ome_state = onair_backend::ome::OmeState {
        secret: ome_secret,
        events: Default::default(),
    };

    // 시험 화면(5173)에서 OME 이벤트 목록을 부를 수 있게 다른 주소 호출(CORS)을 연다. 시험용.
    let cors = tower_http::cors::CorsLayer::permissive();
    let router = app(AppState { db })
        .merge(onair_backend::ome_routes(ome_state))
        .layer(cors);

    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .expect("주소에 묶지 못했습니다");
    tracing::info!("onair backend listening on {bind_addr}");
    axum::serve(listener, router).await.expect("서버 오류");
}
