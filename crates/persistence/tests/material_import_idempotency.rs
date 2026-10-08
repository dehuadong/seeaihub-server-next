//! 素材导入的幂等性要对着**真库**验：四张表的匹配键都是库层的唯一索引（`catalog.vendor_models`
//! 的三元键、`supply.channels_identity`、`supply.offerings_identity`）与追加形态的价目表，
//! 这些判据不是内存里能模拟的东西。
//!
//! 用例从 `HTTP_CONTRACT_DATABASE_URL` 派生一个一次性库（仓库里真实数据库用例的统一入口），
//! 跑完整迁移后导入两遍，比对四张表的行数；跑完把这个库删掉，不动那个基库本身。

use seeai_application::HubRepository;
use seeai_persistence::PgHubRepository;
use seeai_persistence::material_import::import_supply_materials;
use serde_json::{Value, json};
use sqlx::{AssertSqlSafe, PgPool, Row};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// 仓库里那份真实素材：导入的输入不是为测试编的形状，而是工程师实际写的那份。
fn material_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/bootstrap")
}

/// 公开文档目录：显式传给导入，用例的工作目录不影响解析。
fn repo_public_docs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../public-docs")
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
///
/// 素材里没有价目表（两条渠道都按上游声明的金额计价），那一列因此恒为 0。
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

    let first = import_supply_materials(&pool, &material_dir(), &repo_public_docs())
        .await
        .expect("the first import");
    let after_first = table_counts(&pool).await;
    // 先判"第一遍真的导进去了"，否则行数不变这件事可能只是"什么都没导"。
    // 两条素材各两家渠道、共四条供给；它们都按上游声明的金额计价，素材里没有价目表。
    assert!(
        first.materials >= 2 && first.offerings >= 4,
        "the material should seed every table: {first:?}"
    );
    assert_eq!(
        after_first.2, first.offerings as i64,
        "one offering row per candidate in the material"
    );

    let second = import_supply_materials(&pool, &material_dir(), &repo_public_docs())
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
    // 两条渠道都按上游声明的金额计价：素材里没有价目表，两遍都不该新建价目表行。
    assert_eq!(
        first.price_plans, 0,
        "upstream_declared materials carry no price plan"
    );
    // 对客参考价目落 `catalog.vendor_models`：一个模型一行，导入照实写回（两遍之后仍是它）。
    let references: Vec<(String, Option<Value>)> = sqlx::query_as(
        r#"
        SELECT native_model_id, consumer_reference_rates
        FROM catalog.vendor_models
        ORDER BY native_model_id
        "#,
    )
    .fetch_all(&pool)
    .await
    .expect("the vendor model rows");
    assert_eq!(references.len(), 2, "two models import");
    for (model, reference) in references {
        let reference = reference.unwrap_or_else(|| panic!("{model} declares the price list"));
        assert_eq!(reference["currency"], json!("USD"), "{model}");
        assert_eq!(
            reference["text_input_microusd_per_million"],
            json!(5_000_000),
            "{model}"
        );
    }
    // 成本币种同样落库：素材写着 `cost_currency: "USD"`，引用式发布与运营界面据此填成本币种
    // （没有 Price Plan、又还没发布过的供给，此前没有任何来源，见工作项 #81）。
    let currencies: Vec<String> = sqlx::query_scalar(
        "SELECT COALESCE(o.cost_currency, '') FROM supply.offerings o ORDER BY o.cost_currency",
    )
    .fetch_all(&pool)
    .await
    .expect("the offering cost currencies");
    assert_eq!(
        currencies,
        vec!["USD".to_owned(); 4],
        "every imported offering declares its cost currency"
    );
    // 可选供给清单带着它回来——界面据此显示币种、发出成本币种。
    let selectable = repository
        .selectable_offerings()
        .await
        .expect("the selectable offerings");
    assert_eq!(selectable.len(), 4, "four supplies are listed");
    for offering in &selectable {
        assert_eq!(
            offering.cost_currency.as_deref(),
            Some("USD"),
            "{} keeps the declared cost currency in the list",
            offering.provider_kind
        );
    }

    drop(pool);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

/// 参考价目**就地更新**：换一份把 AIHubMix 那条的四档金额改过的素材，`supply.offerings` 那一列跟着
/// 变，且不新增任何一行——素材是这一列的属主（`0012` §3）。
#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL; derives a throwaway database"]
async fn a_reference_price_list_change_updates_the_row_in_place() {
    let (database_url, database_name) = isolated_database_url().await;
    let repository = PgHubRepository::connect(&database_url, 2)
        .await
        .expect("the isolated database");
    repository.migrate().await.expect("the migrations apply");
    let pool = repository.pool().clone();

    import_supply_materials(&pool, &material_dir(), &repo_public_docs())
        .await
        .expect("the first import");
    let before = table_counts(&pool).await;

    let edited = edited_material_dir();
    import_supply_materials(&pool, &edited, &repo_public_docs())
        .await
        .expect("the edited import");
    let after = table_counts(&pool).await;
    assert_eq!(before, after, "改参考价目不该新增任何一行");

    let reference: Option<Value> = sqlx::query_scalar(
        r#"
        SELECT consumer_reference_rates
        FROM catalog.vendor_models
        WHERE native_model_id = 'gpt-image-2.5-flare'
        "#,
    )
    .fetch_one(&pool)
    .await
    .expect("the vendor model row");
    let reference = reference.expect("the model declares the reference price list");
    assert_eq!(
        reference["text_input_microusd_per_million"],
        json!(6_000_000),
        "素材改了参考价目，那一列就地更新：{reference}"
    );

    std::fs::remove_dir_all(&edited).expect("the temporary material directory is removed");
    drop(pool);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

/// 仓库素材的临时副本：把 flare 那份 AIHubMix 供给的参考价目改一个数，其余原样。
fn edited_material_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("seeai-material-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).expect("a temporary material directory");
    for name in ["gpt-image-2.5-flare.json", "gpt-image-2.5-sunburst.json"] {
        let text = std::fs::read_to_string(material_dir().join(name)).expect("a material file");
        let mut material: Value = serde_json::from_str(&text).expect("the material is json");
        if name == "gpt-image-2.5-flare.json" {
            material["consumer_reference_rates"]["text_input_microusd_per_million"] =
                json!(6_000_000);
        }
        let edited = serde_json::to_string_pretty(&material).expect("the edited material is json");
        std::fs::write(dir.join(name), edited).expect("the temporary material is written");
    }
    dir
}

/// 文档素材版本：同一内容重复导入复用一行，内容变化追加一行；发布取最近导入的那一行
/// （设计 §持久化与发布：同一内容复用、内容变化追加、发布取最新）。
#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL; derives a throwaway database"]
async fn a_documentation_change_appends_a_material_version() {
    let (database_url, database_name) = isolated_database_url().await;
    let repository = PgHubRepository::connect(&database_url, 2)
        .await
        .expect("the isolated database");
    repository.migrate().await.expect("the migrations apply");
    let pool = repository.pool().clone();

    import_supply_materials(&pool, &material_dir(), &repo_public_docs())
        .await
        .expect("the first import");
    assert_eq!(material_versions(&pool, "gpt-image-2.5-flare").await, 1);

    // 同一内容再导一遍：不产生第二个素材版本。
    import_supply_materials(&pool, &material_dir(), &repo_public_docs())
        .await
        .expect("the second import");
    assert_eq!(material_versions(&pool, "gpt-image-2.5-flare").await, 1);

    // 改一句释义：追加一行，且发布侧取到的最近一行是新内容。
    let edited =
        std::env::temp_dir().join(format!("seeai-doc-material-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&edited).expect("temp material dir");
    let original = std::fs::read_to_string(material_dir().join("gpt-image-2.5-flare.json"))
        .expect("the bootstrap material");
    let mut value: serde_json::Value =
        serde_json::from_str(&original).expect("the material is json");
    value["documentation"]["fields"]["/properties/prompt"] =
        serde_json::json!("改过的提示词释义。");
    std::fs::write(
        edited.join("gpt-image-2.5-flare.json"),
        serde_json::to_string(&value).expect("serialize the edited material"),
    )
    .expect("write the edited material");
    import_supply_materials(&pool, &edited, &repo_public_docs())
        .await
        .expect("the changed import");
    assert_eq!(material_versions(&pool, "gpt-image-2.5-flare").await, 2);
    let latest: String = sqlx::query_scalar(
        r#"
        SELECT m.material->'fields'->>'/properties/prompt'
        FROM publication.model_document_materials m
        JOIN catalog.vendor_models vm ON vm.id = m.vendor_model_id
        WHERE vm.native_model_id = 'gpt-image-2.5-flare'
        ORDER BY m.created_at DESC, m.id DESC
        LIMIT 1
        "#,
    )
    .fetch_one(&pool)
    .await
    .expect("the latest material");
    assert_eq!(latest, "改过的提示词释义。");

    std::fs::remove_dir_all(&edited).expect("clean the temp material dir");
    drop(pool);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

/// 某厂商模型已落库的文档素材版本数。
async fn material_versions(pool: &PgPool, model: &str) -> i64 {
    sqlx::query_scalar(
        r#"
        SELECT count(*) FROM publication.model_document_materials m
        JOIN catalog.vendor_models vm ON vm.id = m.vendor_model_id
        WHERE vm.native_model_id = $1
        "#,
    )
    .bind(model)
    .fetch_one(pool)
    .await
    .expect("material versions")
}

/// 同一 Vendor Model 修订改 `type` 与改合同同一条处置：拒并点名型号，不产生第二行、不改写既有行
/// （Spec 0006 §4.4、A4）。
#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL; derives a throwaway database"]
async fn changing_the_type_of_an_existing_revision_is_rejected() {
    let (database_url, database_name) = isolated_database_url().await;
    let repository = PgHubRepository::connect(&database_url, 2)
        .await
        .expect("the isolated database");
    repository.migrate().await.expect("the migrations apply");
    let pool = repository.pool().clone();

    import_supply_materials(&pool, &material_dir(), &repo_public_docs())
        .await
        .expect("the first import");
    let before = table_counts(&pool).await;

    // 同一身份、只把顶层 type 改成 video：导入必须拒并点名型号，既有行不动。
    let edited =
        std::env::temp_dir().join(format!("seeai-typed-material-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&edited).expect("temp material dir");
    let original = std::fs::read_to_string(material_dir().join("gpt-image-2.5-flare.json"))
        .expect("the bootstrap material");
    let mut value: serde_json::Value =
        serde_json::from_str(&original).expect("the material is json");
    value["type"] = serde_json::json!("video");
    std::fs::write(
        edited.join("gpt-image-2.5-flare.json"),
        serde_json::to_string(&value).expect("serialize the edited material"),
    )
    .expect("write the edited material");

    let error = import_supply_materials(&pool, &edited, &repo_public_docs())
        .await
        .expect_err("changing the type must be rejected");
    assert!(
        error.to_string().contains("gpt-image-2.5-flare"),
        "the error names the vendor model: {error}"
    );
    assert!(
        error.to_string().contains("type is immutable"),
        "the error says the type is immutable: {error}"
    );
    assert_eq!(
        table_counts(&pool).await,
        before,
        "a rejected import must not add or rewrite any row"
    );

    std::fs::remove_dir_all(&edited).expect("clean the temp material dir");
    drop(pool);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

/// 素材缺 `type` 时导入失败并点名型号，库里不产生该 Vendor Model 行（Spec 0006 §4.4、A3）。
#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL; derives a throwaway database"]
async fn a_material_without_a_type_is_rejected_without_writing_a_row() {
    let (database_url, database_name) = isolated_database_url().await;
    let repository = PgHubRepository::connect(&database_url, 2)
        .await
        .expect("the isolated database");
    repository.migrate().await.expect("the migrations apply");
    let pool = repository.pool().clone();
    let before = table_counts(&pool).await;

    let edited = std::env::temp_dir().join(format!(
        "seeai-untyped-material-{}",
        Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&edited).expect("temp material dir");
    let original = std::fs::read_to_string(material_dir().join("gpt-image-2.5-flare.json"))
        .expect("the bootstrap material");
    let mut value: serde_json::Value =
        serde_json::from_str(&original).expect("the material is json");
    value.as_object_mut().expect("object").remove("type");
    std::fs::write(
        edited.join("gpt-image-2.5-flare.json"),
        serde_json::to_string(&value).expect("serialize the untyped material"),
    )
    .expect("write the untyped material");

    let error = import_supply_materials(&pool, &edited, &repo_public_docs())
        .await
        .expect_err("a material without type must be rejected");
    assert!(
        error.to_string().contains("gpt-image-2.5-flare"),
        "the error names the vendor model: {error}"
    );
    assert!(
        error.to_string().contains("does not declare type"),
        "the error says the type is missing: {error}"
    );
    assert_eq!(
        table_counts(&pool).await,
        before,
        "a rejected import must not write any row"
    );

    std::fs::remove_dir_all(&edited).expect("clean the temp material dir");
    drop(pool);
    drop(repository);
    drop_isolated_database(&database_name).await;
}

/// 渠道侧的成本事实（保底表、参考成本）落 `supply.offerings`：素材是它们的属主。
///
/// 运营表单里不出现成本字段（Spec 0001 V-D12），引用式发布只能从这条供给读它们；只认发布命令会让
/// `upstream_declared` 候选的保底表永远进不了修订，受理只剩平台兜底额（工作项 #89）。
#[tokio::test]
#[ignore = "requires a PostgreSQL server via HTTP_CONTRACT_DATABASE_URL; derives a throwaway database"]
async fn the_channel_cost_facts_land_on_the_offering_rows() {
    let (database_url, database_name) = isolated_database_url().await;
    let repository = PgHubRepository::connect(&database_url, 2)
        .await
        .expect("the isolated database");
    repository.migrate().await.expect("the migrations apply");
    let pool = repository.pool().clone();

    import_supply_materials(&pool, &material_dir(), &repo_public_docs())
        .await
        .expect("the material imports");

    let rows = sqlx::query(
        "SELECT vm.native_model_id, o.floor_amounts, o.reference_cost_microusd
         FROM supply.offerings o
         JOIN catalog.vendor_models vm ON vm.id = o.vendor_model_id
         ORDER BY vm.native_model_id, o.provider_model_id",
    )
    .fetch_all(&pool)
    .await
    .expect("the offering rows");

    assert!(!rows.is_empty(), "素材导入必须写出供给行");
    for row in &rows {
        let model: String = row.try_get("native_model_id").expect("native_model_id");
        let floor: Value = row.try_get("floor_amounts").expect("floor_amounts");
        let reference_cost: i64 = row
            .try_get("reference_cost_microusd")
            .expect("reference_cost_microusd");
        assert_eq!(reference_cost, 11_354, "{model} 的参考成本必须落行");
        assert_eq!(
            floor["amounts"]["1K"],
            json!(160_000),
            "{model} 的 1K 保底额必须落行"
        );
        assert_eq!(
            floor["amounts"]["2K"],
            json!(250_000),
            "{model} 的 2K 保底额必须落行"
        );
        assert_eq!(
            floor["cap_microusd"],
            json!(300_000),
            "{model} 的封顶保底值必须落行"
        );
    }

    drop(pool);
    drop(repository);
    drop_isolated_database(&database_name).await;
}
