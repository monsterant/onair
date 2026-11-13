//! onair 백엔드: 라우터와 공용 상태를 만든다.
//! main.rs는 설정을 읽고 서버를 띄우기만 하고, 실제 길(라우트)은 여기서 조립한다.
//! 이렇게 나누면 테스트에서 진짜 서버를 띄우지 않고도 라우터를 바로 불러 시험할 수 있다.

pub mod auth;
pub mod broadcasts;
pub mod chat;
pub mod comments;
pub mod donations;
pub mod follows;
pub mod guard;
pub mod moderation;
pub mod ome;
pub mod ome_api;
pub mod recordings;
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
    /// 녹화(#47). 폴더를 설정하지 않으면(Default) 녹화·다시보기 파일이 꺼진다
    pub rec: recordings::RecConfig,
    /// 실시간 채팅방들(#86). 서버 메모리에만 있다.
    pub chat: chat::Hub,
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
        .route("/config", get(config))
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
        // 실시간 채팅(#86): 받기는 SSE, 보내기는 POST
        .route("/broadcasts/{id}/chat/stream", get(chat::stream))
        .route("/broadcasts/{id}/chat", axum::routing::post(chat::send))
        // 시청자 제한·천천히 모드(#87). 방송 주인만.
        .route("/broadcasts/{id}/bans", get(moderation::list))
        .route(
            "/broadcasts/{id}/bans/{user_id}",
            axum::routing::put(moderation::ban).delete(moderation::unban),
        )
        .route(
            "/broadcasts/{id}/slow",
            axum::routing::put(moderation::set_slow),
        )
        // 채팅 금칙어(#112). 방송 주인만.
        .route(
            "/broadcasts/{id}/banned-words",
            get(moderation::get_words).put(moderation::set_words),
        )
        // 공지 고정(#109). 방송 주인만.
        .route(
            "/broadcasts/{id}/notice",
            axum::routing::put(moderation::set_notice),
        )
        // 방송자 즐겨찾기(#102). 로그인한 사람만, 목록은 나만.
        .route("/follows", get(follows::list))
        .route(
            "/follows/{user_id}",
            axum::routing::put(follows::follow).delete(follows::unfollow),
        )
        .route("/broadcasts/{id}/end", post(broadcasts::end))
        .route("/broadcasts/{id}/stream-key", post(broadcasts::rotate_key))
        .route(
            "/broadcasts/{id}/comments",
            get(comments::list).post(comments::create),
        )
        .route(
            "/broadcasts/{id}/donations",
            get(donations::list).post(donations::create),
        )
        .route("/donations/confirm", post(donations::confirm))
        .route("/recordings", get(recordings::list))
        .route("/recordings/{id}", get(recordings::get))
        .route(
            "/broadcasts/{id}/recordings",
            get(recordings::list_for_broadcast),
        )
        .route("/vod/{file}", get(recordings::serve_file))
        .route("/donations/notify", post(donations::notify))
        .route(
            "/comments/{id}",
            axum::routing::patch(comments::update).delete(comments::delete),
        )
        .merge(limited)
        // 없는 길은 {"error"} 404(#25 A12). 기본 처리로 두면 main.rs에서 OME 웹훅 라우터와 합칠 때
        // 그쪽 기본 처리(빈 본문 404)가 쓰여서 JSON이 되지 않는다(#25 리뷰).
        .fallback(|| async { auth::ApiError::NotFound })
        // 상태를 바꾸는 요청은 같은 사이트에서 온 것만 받는다(#24 A3). OME 웹훅(ome_routes)에는 걸지 않는다.
        .layer(axum::middleware::from_fn(guard::reject_cross_site))
        // 기본 오류 응답도 {"error"} 모양으로(#25 A12). 가장 바깥에 둔다.
        .layer(axum::middleware::from_fn(guard::json_errors))
        // 오류 응답까지 포함해 모든 API 응답을 캐시하지 않게(#25 A16). json_errors가 바꾼 응답에도 붙도록 그 바깥에 둔다.
        .layer(axum::middleware::from_fn(guard::no_store))
        .with_state(state)
}

/// DB 구조 변경(migrations 폴더)을 적용한다. 이미 적용된 것은 건너뛴다.
pub async fn migrate(db: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    sqlx::migrate!("./migrations").run(db).await
}

/// 마이그레이션 실패가 기다리면 나아질 수 있는 것인지(#25 A13).
/// - 다시 시도: DB가 아직 뜨는 중이거나 잠깐 끊긴 경우(연결 거부, 시간 초과, 풀에서 연결을 못 받음,
///   DB가 보낸 일시 오류 57P01·57P02·57P03 종료·재시작·뜨는 중, 53300 연결 초과).
/// - 바로 멈춤: 버전 불일치·빠진 버전·SQL 오류·인증 실패·TLS 설정 오류처럼 몇 번을 다시 해도 같은 것.
///   60초를 기다리지 않고 원인을 바로 보여 준다.
pub fn migrate_retryable(e: &sqlx::migrate::MigrateError) -> bool {
    use sqlx::migrate::MigrateError;
    match e {
        MigrateError::Execute(err) | MigrateError::ExecuteMigration(err, _) => match err {
            sqlx::Error::Io(_) | sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed => true,
            sqlx::Error::Database(db) => {
                matches!(
                    db.code().as_deref(),
                    Some("57P01" | "57P02" | "57P03" | "53300")
                )
            }
            _ => false,
        },
        _ => false,
    }
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

/// 화면이 켜고 끌 기능(#25 A7): 테스트 계정 버튼, 도네이션.
#[derive(Serialize)]
struct PublicConfig {
    demo_login: bool,
    donations: bool,
}

async fn config(State(state): State<AppState>) -> Json<PublicConfig> {
    Json(PublicConfig {
        demo_login: state.auth.demo_login,
        donations: state.toss.is_configured(),
    })
}
