//! DB 연결 풀 안전장치 시험(11-14): 트랜잭션이 남은 연결은 풀에 돌아오면 버려지고, 정상 연결은 다시 쓴다.

use std::time::Duration;

use sqlx::{PgPool, postgres::PgPool as Pool};

/// 시험이 만든 빈 DB에 같은 설정의 풀을 하나 더 만든다. 연결을 1개로 줄여 늘 같은 자리를 다시 받게 한다.
async fn one_connection_pool(db: &PgPool) -> Pool {
    onair_backend::pool_options()
        .max_connections(1)
        .connect_with((*db.connect_options()).clone())
        .await
        .unwrap()
}

async fn pid(pool: &Pool) -> i32 {
    sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test]
async fn healthy_connection_is_reused(db: PgPool) {
    let pool = one_connection_pool(&db).await;
    let first = pid(&pool).await;
    for _ in 0..3 {
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(pid(&pool).await, first, "정상 연결을 버리고 새로 붙었다");
    }
}

#[sqlx::test]
async fn connection_left_inside_a_transaction_is_not_reused(db: PgPool) {
    let pool = one_connection_pool(&db).await;
    let first = {
        let mut conn = pool.acquire().await.unwrap();
        // sqlx가 모르게 트랜잭션을 연다(정리되지 않은 트랜잭션이 남은 상황을 흉내 냄)
        sqlx::raw_sql("BEGIN").execute(&mut *conn).await.unwrap();
        sqlx::query_scalar::<_, i32>("SELECT pg_backend_pid()")
            .fetch_one(&mut *conn)
            .await
            .unwrap()
    };
    // 연결을 돌려주는 일은 뒤에서 돈다. 시각이 달라지도록 조금 기다린다.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_ne!(
        pid(&pool).await,
        first,
        "트랜잭션이 남은 연결을 다시 받았다"
    );
}

#[sqlx::test]
async fn connection_left_in_a_failed_transaction_is_not_reused(db: PgPool) {
    let pool = one_connection_pool(&db).await;
    let first = {
        let mut conn = pool.acquire().await.unwrap();
        let first: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        sqlx::raw_sql("BEGIN").execute(&mut *conn).await.unwrap();
        // 트랜잭션 안에서 오류를 내 "이미 실패한 트랜잭션" 상태로 만든다
        let _ = sqlx::raw_sql("SELECT 1/0").execute(&mut *conn).await;
        first
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_ne!(
        pid(&pool).await,
        first,
        "실패한 트랜잭션이 남은 연결을 다시 받았다"
    );
}

/// sqlx 트랜잭션을 커밋하지 않고 버리면(오류로 일찍 돌아갈 때) sqlx가 ROLLBACK으로 정리하므로 연결을 다시 쓴다.
/// 안전장치가 이런 정상 연결까지 버리지 않는지 본다.
#[sqlx::test]
async fn connection_after_a_dropped_sqlx_transaction_is_reused(db: PgPool) {
    let pool = one_connection_pool(&db).await;
    let first = {
        let mut tx = pool.begin().await.unwrap();
        let first: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        first
        // 여기서 tx가 커밋 없이 버려진다
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(pid(&pool).await, first, "정상적으로 정리된 연결을 버렸다");
}
