//! onair 백엔드: 라우터와 공용 상태를 만든다.
//! main.rs는 설정을 읽고 서버를 띄우기만 하고, 실제 길(라우트)은 여기서 조립한다.
//! 이렇게 나누면 테스트에서 진짜 서버를 띄우지 않고도 라우터를 바로 불러 시험할 수 있다.

pub mod auth;
pub mod ome;

use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};
use serde::Serialize;
use sqlx::PgPool;

/// 모든 요청이 함께 쓰는 값: DB 연결 풀(미리 열어 둔 DB 연결 묶음)과 인증 설정
#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub auth: auth::AuthConfig,
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/auth/signup", post(auth::signup))
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/me", get(auth::me))
        .route("/auth/demo/{n}", post(auth::demo_login))
        .with_state(state)
}

/// DB 구조 변경(migrations 폴더)을 적용한다. 이미 적용된 것은 건너뛴다.
pub async fn migrate(db: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    sqlx::migrate!("./migrations").run(db).await
}

/// OME AdmissionWebhooks 길. DB가 필요 없어서 상태(서명 키)를 따로 받는다.
pub fn ome_routes(state: ome::OmeState) -> Router {
    Router::new()
        .route("/ome/admission", post(ome::admission))
        .with_state(state)
}

#[derive(Serialize, Debug, PartialEq)]
pub struct Health {
    pub status: &'static str,
    pub db: &'static str,
}

/// 서버가 살아 있는지, DB에 닿는지 알려 준다.
/// DB가 죽어 있어도 서버 자체는 살아 있으므로 200을 돌려주고, db 칸으로 상태를 알린다.
async fn health(State(state): State<AppState>) -> Json<Health> {
    let db = match sqlx::query("SELECT 1").execute(&state.db).await {
        Ok(_) => "ok",
        Err(_) => "error",
    };
    Json(Health { status: "ok", db })
}
