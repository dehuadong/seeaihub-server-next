//! 账户金额的**范围边界**要对着真库验：可用额是派生值，它在 64 位整数里表示不出来时，不能把
//! 那条资金语句一起带崩。
//!
//! 用例派生一次性库、跑完整迁移，把一个账户摆到"余额贴着 `i64::MIN`、又有占用"的位置——两个
//! `bigint` 相减会溢出，而余额增加本身合法。断言这次充值照样提交，只是没有可刷新的缓存快照。
//!
//! 跑到这个边界上的是被绕过写入或人为改过的库；正常资金流走不到 `i64::MIN`。
//!
//! 前置：`HTTP_CONTRACT_DATABASE_URL` 指向一个可连的 PostgreSQL，且角色能 `CREATE DATABASE`。

use seeai_application::HubRepository;
use seeai_domain::AccountId;
use seeai_persistence::PgHubRepository;
use sqlx::{AssertSqlSafe, PgPool, Row};
use uuid::Uuid;

async fn isolated_database_url() -> (String, String) {
    let base = std::env::var("HTTP_CONTRACT_DATABASE_URL")
        .expect("HTTP_CONTRACT_DATABASE_URL is required for the ignored balance snapshot test");
    let admin = PgPool::connect(&base)
        .await
        .expect("connect to the provided contract database");
    let name = format!("seeai_balance_{}", Uuid::new_v4().simple());
    sqlx::query(AssertSqlSafe(format!("CREATE DATABASE \"{name}\"")))
        .execute(&admin)
        .await
        .expect("create an isolated balance database");
    admin.close().await;
    let url = match base.rfind('/') {
        Some(index) => format!("{}/{}", &base[..index], name),
        None => panic!("HTTP_CONTRACT_DATABASE_URL must include a database name"),
    };
    (url, name)
}

async fn drop_isolated_database(name: &str) {
    let base = std::env::var("HTTP_CONTRACT_DATABASE_URL").expect("the contract database url");
    let Ok(admin) = PgPool::connect(&base).await else {
        return;
    };
    let _ = sqlx::query(AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)"
    )))
    .execute(&admin)
    .await;
    admin.close().await;
}

/// **可用额算不出来不能拦住资金提交。**
///
/// 摆位：余额 `i64::MIN`、占用一千万微单位。这次充值一百万微单位把余额抬到 `i64::MIN + 1000000`
/// ——金额更新本身合法，而减去占用之后落在 `i64::MIN - 9000000`，溢出。
#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL and a role allowed to CREATE DATABASE; derives a throwaway database"]
async fn a_balance_snapshot_that_does_not_fit_never_blocks_the_credit() {
    let (database_url, database_name) = isolated_database_url().await;
    let repository = PgHubRepository::connect(&database_url, 4)
        .await
        .expect("the isolated database");
    repository.migrate().await.expect("the migrations apply");
    let pool = repository.pool().clone();

    let account_id = AccountId::new();
    sqlx::query(
        "INSERT INTO ledger.accounts (id, balance_microusd, held_microusd, version, kind, name)
         VALUES ($1, $2, 10000000, 7, 'consumer', 'balance snapshot boundary account')",
    )
    .bind(account_id.0)
    .bind(i64::MIN)
    .execute(&pool)
    .await
    .expect("seed the boundary account");

    let credit = repository
        .credit_account(
            account_id,
            1_000_000,
            "boundary-credit-1",
            "balance snapshot test",
        )
        .await;

    let snapshot =
        credit.expect("the credit must commit; only the cache snapshot is unrepresentable");
    assert!(
        snapshot.is_none(),
        "可用额溢出时不给缓存快照，got {snapshot:?}"
    );

    let row = sqlx::query(
        "SELECT balance_microusd, held_microusd, version FROM ledger.accounts WHERE id = $1",
    )
    .bind(account_id.0)
    .fetch_one(&pool)
    .await
    .expect("the account row");
    assert_eq!(
        row.get::<i64, _>("balance_microusd"),
        i64::MIN + 1_000_000,
        "余额照常加上去了：资金提交没有被快照计算拦住"
    );
    assert_eq!(row.get::<i64, _>("held_microusd"), 10_000_000, "占用没被碰");
    assert_eq!(row.get::<i64, _>("version"), 8, "版本照常前进");

    drop(repository);
    drop(pool);
    drop_isolated_database(&database_name).await;
}
