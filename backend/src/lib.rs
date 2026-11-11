//! onair 백엔드: 라우터와 공용 상태를 만든다.
//! main.rs는 설정을 읽고 서버를 띄우기만 하고, 실제 길(라우트)은 여기서 조립한다.
//! 이렇게 나누면 테스트에서 진짜 서버를 띄우지 않고도 라우터를 바로 불러 시험할 수 있다.

pub mod auth;
pub mod broadcasts;
pub mod comments;
pub mod donations;
pub mod guard;
pub mod ome;
pub mod ome_api;
pub mod toss;

use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};
use serde::Serialize;
use sqlx::PgPool;

/// 모든 요청이 함께 쓰는 값: DB 연결 풀(미리 열어 둔 DB 연결 묶음), 인증 설정, OME 관리 API
#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub auth: auth::AuthConfig,
    /// 설정하지 않으면(Default) OME를 부르지 않는다
    pub ome: ome_api::OmeApi,
    /// 토스 결제 API(#41). 설정하지 않으면(Default) 도네이션이 꺼진다
    pub toss: toss::Toss,
}

pub fn app(state: AppState) -> Router {
    // 비밀번호를 확인하거나 세션을 새로 만드는 길: 횟수 제한을 건다(#24 A2)
    let limited = Router::new()
        .route("/auth/signup", post(auth::signup))
        .route("/auth/login", post(auth::login))
        .route("/auth/demo/{n}", post(auth::demo_login))
        .route("/auth/guest", post(auth::guest_login))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            guard::limit_auth,
        ));
    Router::new()
        .route("/health", get(health))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/me", get(auth::me))
        .route(
            "/broadcasts",
            get(broadcasts::list).post(broadcasts::create),
        )
        .route(
            "/broadcasts/{id}",
            get(broadcasts::get)
                .patch(broadcasts::update)
                .delete(broadcasts::delete),
        )
        .route("/broadcasts/{id}/publish", get(broadcasts::publish_info))
        .route("/broadcasts/{id}/end", post(broadcasts::end))
        .route(
            "/broadcasts/{id}/comments",
            get(comments::list).post(comments::create),
        )
        .route(
            "/broadcasts/{id}/donations",
            get(donations::list).post(donations::create),
        )
        .route("/donations/confirm", post(donations::confirm))
        .route("/donations/notify", post(donations::notify))
        .route(
            "/comments/{id}",
            axum::routing::patch(comments::update).delete(comments::delete),
        )
        .merge(limited)
        // 상태를 바꾸는 요청은 같은 사이트에서 온 것만 받는다(#24 A3). OME 웹훅(ome_routes)에는 걸지 않는다.
        .layer(axum::middleware::from_fn(guard::reject_cross_site))
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
