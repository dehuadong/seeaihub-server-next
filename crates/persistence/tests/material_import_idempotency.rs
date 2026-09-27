//! 素材导入的幂等性要对着**真库**验：四张表的匹配键都是库层的唯一索引（`catalog.vendor_models`
//! 的三元键、`supply.channels_identity`、`supply.offerings_identity`）与追加形态的价目表，
//! 这些判据不是内存里能模拟的东西。
//!
//! 用例从 `HTTP_CONTRACT_DATABASE_URL` 派生一个一次性库（仓库里真实数据库用例的统一入口），
//! 跑完整迁移后导入两遍，比对四张表的行数；跑完把这个库删掉，不动那个基库本身。

use seeai_persistence::PgHubRepository;
use seeai_persistence::material_import::import_supply_materials;
use sqlx::{AssertSqlSafe, PgPool, Row};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// 仓库里那份真实素材：导入的输入不是为测试编的形状，而是工程师实际写的那份。
fn material_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/bootstrap")
}

async fn isolated_database_url() -> (String, String) {
    let base = std::env::var("HTTP_CONTRACT_DATABASE_URL")
        .expect("HTTP_CONTRACT_DATABASE_URL is required for the ignored material import test");
    let admin = PgPool::connect(&base)
        .await
        .expect("connect to the provided contract database");
    let name = format!("seeai_material_{}", Uuid::new_v4().simple());
    sqlx::query(AssertSqlSafe(format!("CREATE DATABASE \"{name}\"")))
        .execute(&admin)
        .await
        .expect("create an isolated material database");
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

/// 四张表的行数，一次读出来：四个数要同时成立（"第二次没多行"是四张表一起判的）。
async fn table_counts(pool: &PgPool) -> (i64, i64, i64, i64) {
    let row = sqlx::query(
        r#"
        SELECT
            (SELECT count(*) FROM catalog.vendor_models) AS vendor_models,
            (SELECT count(*) FROM supply.channels) AS channels,
            (SELECT count(*) FROM supply.offerings) AS offerings,
            (SELECT count(*) FROM pricing.price_plans) AS price_plans
        "#,
    )
    .fetch_one(pool)
    .await
    .expect("table counts");
    (
        row.try_get("vendor_models").expect("vendor_models count"),
        row.try_get("channels").expect("channels count"),
        row.try_get("offerings").expect("offerings count"),
        row.try_get("price_plans").expect("price_plans count"),
    )
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL; derives a throwaway database"]
async fn importing_the_same_material_twice_adds_no_rows() {
    let (database_url, database_name) = isolated_database_url().await;
    let repository = PgHubRepository::connect(&database_url, 2)
        .await
        .expect("the isolated database");
    repository.migrate().await.expect("the migrations apply");
    let pool = repository.pool().clone();

    let first = import_supply_materials(&pool, &material_dir())
        .await
        .expect("the first import");
    let after_first = table_counts(&pool).await;
    // 先判"第一遍真的导进去了"，否则行数不变这件事可能只是"什么都没导"。
    assert!(
        first.materials >= 2 && first.offerings >= 4 && first.price_plans >= 2,
        "the material should seed every table: {first:?}"
    );
    assert_eq!(
        after_first.2, first.offerings as i64,
        "one offering row per candidate in the material"
    );

    let second = import_supply_materials(&pool, &material_dir())
        .await
        .expect("the second import");
    let after_second = table_counts(&pool).await;
    // 这条用例是 `#[ignore]` 的真库用例，判据（四张表的行数）要能在跑它的时候直接读出来。
    println!("vendor_models/channels/offerings/price_plans after the first pass: {after_first:?}");
    println!(
        "vendor_models/channels/offerings/price_plans after the second pass: {after_second:?}; \
         the second pass wrote {second:?}"
    );

    assert_eq!(
        after_first, after_second,
        "the second pass of the same material must not add a row to any of the four tables"
    );
    assert_eq!(
        second.offerings, first.offerings,
        "the second pass sees the same candidates"
    );
    assert_eq!(
        second.price_plans, 0,
        "the second pass reuses the price plan it already wrote"
    );

    drop(pool);
    drop(repository);
    drop_isolated_database(&database_name).await;
}
