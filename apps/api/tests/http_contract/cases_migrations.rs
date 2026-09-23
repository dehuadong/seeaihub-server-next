use super::*;

/// 增量迁移必须在**已经建过库**的环境里跑得通。
///
/// 早期迁移是以"建表"方式被应用的，改它们不会更新已建好的库；本次改动按新迁移增量修改，
/// 因此这里先在只应用了早期迁移的库上建表，再补上整批迁移，确认它能升上来。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn images_pass_through_migration_applies_on_an_existing_database() {
    let (database_url, database_name) = isolated_database_url().await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");
    let migrations = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../migrations");

    // 1) 只应用"这次改动之前"的迁移：把早期 `.sql` 拷到一个临时目录。
    let staged = std::env::temp_dir().join(format!("seeai-early-migrations-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&staged).expect("staging directory");
    for entry in std::fs::read_dir(&migrations).expect("migrations directory") {
        let entry = entry.expect("migration entry");
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".sql") && name.as_str() < "0005" {
            std::fs::copy(entry.path(), staged.join(&name)).expect("copy early migration");
        }
    }
    let early = sqlx::migrate::Migrator::new(staged.clone())
        .await
        .expect("early migrator");
    early.run(&pool).await.expect("early migrations apply");

    // 2) 再应用完整迁移集（含本次的增量）：已应用过的按版本跳过。
    let all = sqlx::migrate::Migrator::new(migrations)
        .await
        .expect("migrator");
    all.run(&pool)
        .await
        .expect("the new migration must apply on an already-built database");

    // 3) 新契约的形状在场，旧资产形状不在。
    let column_exists = |table: &'static str, column: &'static str| {
        let pool = pool.clone();
        async move {
            let count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM information_schema.columns WHERE table_schema = 'generation' AND table_name = $1 AND column_name = $2",
            )
            .bind(table)
            .bind(column)
            .fetch_one(&pool)
            .await
            .expect("column probe");
            count == 1
        }
    };
    assert!(column_exists("jobs", "result_images").await);
    assert!(!column_exists("jobs", "result_asset_ids").await);
    assert!(!column_exists("jobs", "asset_bindings").await);
    let assets_table: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.tables WHERE table_schema = 'generation' AND table_name = 'assets'",
    )
    .fetch_one(&pool)
    .await
    .expect("table probe");
    assert_eq!(assets_table, 0, "资产表必须被删掉");

    pool.close().await;
    let _ = std::fs::remove_dir_all(&staged);
    drop_isolated_database(&database_name).await;
}

/// 增量迁移还要能处理**已经存在的老数据**：同一 (vendor, model, revision) 可能已有多行
/// （老形状按内容分叉），迁移必须自己合并，而不是直接失败。
///
/// 这里先在只应用了早期迁移的库上造出这种数据（两行同一个型号、一个供给与一台 Job 指向
/// 较早那一行），再补上整批迁移，确认合并结果与承载面回填都对。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn vendor_model_contract_migration_merges_existing_duplicate_rows() {
    let (database_url, database_name) = isolated_database_url().await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");
    let migrations = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../migrations");

    // 1) 只应用这次改动之前的迁移（`0006` 之前，含上一轮的 `0005`）。
    let staged = std::env::temp_dir().join(format!("seeai-early-migrations-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&staged).expect("staging directory");
    for entry in std::fs::read_dir(&migrations).expect("migrations directory") {
        let entry = entry.expect("migration entry");
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".sql") && name.as_str() < "0006" {
            std::fs::copy(entry.path(), staged.join(&name)).expect("copy early migration");
        }
    }
    sqlx::migrate::Migrator::new(staged.clone())
        .await
        .expect("early migrator")
        .run(&pool)
        .await
        .expect("early migrations apply");

    // 2) 老形状的数据：同一个型号的两行合同（内容不同，老唯一键含内容哈希所以能并存），
    //    供给与 Job 都指向**较早**的那一行。
    let older = Uuid::new_v4();
    let newer = Uuid::new_v4();
    let channel = Uuid::new_v4();
    let offering = Uuid::new_v4();
    let price_plan = Uuid::new_v4();
    let revision = Uuid::new_v4();
    let account = Uuid::new_v4();
    let job = Uuid::new_v4();
    let older_surface = json!({"surface": "older"});
    let newer_surface = json!({"surface": "newer"});
    sqlx::query(
        "INSERT INTO catalog.vendor_models
             (id, vendor_id, native_model_id, native_revision, capability_schema, schema_hash, created_at)
         VALUES ($1,'OpenAI','legacy-model','legacy-revision',$2,'hash-older', now() - interval '1 hour')",
    )
    .bind(older)
    .bind(&older_surface)
    .execute(&pool)
    .await
    .expect("legacy vendor model row");
    sqlx::query(
        "INSERT INTO catalog.vendor_models
             (id, vendor_id, native_model_id, native_revision, capability_schema, schema_hash, created_at)
         VALUES ($1,'OpenAI','legacy-model','legacy-revision',$2,'hash-newer', now())",
    )
    .bind(newer)
    .bind(&newer_surface)
    .execute(&pool)
    .await
    .expect("newer vendor model row");
    sqlx::query(
        "INSERT INTO supply.channels (id, provider_kind, base_url, credential_env)
         VALUES ($1,'AIHubMix','https://api.inferera.com','AIHUBMIX_API_KEY')",
    )
    .bind(channel)
    .execute(&pool)
    .await
    .expect("channel fixture");
    sqlx::query(
        "INSERT INTO supply.offerings
             (id, vendor_model_id, channel_id, adapter_key, provider_model_id, restrictions)
         VALUES ($1,$2,$3,'aihubmix-image-v1','legacy-model','{}'::jsonb)",
    )
    .bind(offering)
    .bind(older)
    .bind(channel)
    .execute(&pool)
    .await
    .expect("offering fixture");
    sqlx::query(
        "INSERT INTO pricing.price_plans
             (id, offering_id, currency, text_input_microusd_per_million, image_input_microusd_per_million,
              text_output_microusd_per_million, image_output_microusd_per_million, source_url, approved_by)
         VALUES ($1,$2,'USD',0,0,0,0,'https://example.invalid/price','migration-test')",
    )
    .bind(price_plan)
    .bind(offering)
    .execute(&pool)
    .await
    .expect("price plan fixture");
    sqlx::query(
        "INSERT INTO publication.runtime_revisions (id, snapshot, published_by)
         VALUES ($1,'{}'::jsonb,'migration-test')",
    )
    .bind(revision)
    .execute(&pool)
    .await
    .expect("runtime revision fixture");
    sqlx::query(
        "INSERT INTO publication.runtime_entries
             (runtime_revision_id, vendor_model_id, offering_id, price_plan_id, gateway_model, active)
         VALUES ($1,$2,$3,$4,'legacy-model',true)",
    )
    .bind(revision)
    .bind(older)
    .bind(offering)
    .bind(price_plan)
    .execute(&pool)
    .await
    .expect("runtime entry fixture");
    sqlx::query("INSERT INTO ledger.accounts (id, balance_microusd) VALUES ($1, 0)")
        .bind(account)
        .execute(&pool)
        .await
        .expect("account fixture");
    sqlx::query(
        "INSERT INTO generation.jobs
             (id, account_id, idempotency_key, request_hash, state, branch, gateway_model,
              native_parameters, runtime_revision_id, vendor_model_id, offering_id, channel_id,
              price_snapshot, max_cost_microusd)
         VALUES ($1,$2,'legacy-job','hash','accepted','prompt_only','legacy-model',
                 '{}'::jsonb,$3,$4,$5,$6,'{}'::jsonb,1)",
    )
    .bind(job)
    .bind(account)
    .bind(revision)
    .bind(older)
    .bind(offering)
    .bind(channel)
    .execute(&pool)
    .await
    .expect("legacy job fixture");

    // 3) 再应用完整迁移集：合并必须自己跑通，不能因为已有重复行就失败。
    sqlx::migrate::Migrator::new(migrations)
        .await
        .expect("migrator")
        .run(&pool)
        .await
        .expect("the merge migration must apply on an already-built database");

    // 4) 合并结果：只留最新那一行；指向被删行的供给 / 条目 / Job 改挂到它。
    let remaining: Vec<(Uuid, Value)> = sqlx::query(
        "SELECT id, capability_schema FROM catalog.vendor_models WHERE native_model_id = 'legacy-model'",
    )
    .fetch_all(&pool)
    .await
    .expect("merged contract rows")
    .iter()
    .map(|row| {
        (
            row.try_get("id").expect("id"),
            row.try_get("capability_schema").expect("contract"),
        )
    })
    .collect();
    assert_eq!(remaining.len(), 1, "duplicate contract rows must be merged");
    assert_eq!(remaining[0].0, newer, "the newest row must survive");
    assert_eq!(remaining[0].1, newer_surface);

    // 承载面按**它当时指向的那一行**补好：老供给与老 Job 读到的仍是它们当时那份面。
    let offering_row =
        sqlx::query("SELECT vendor_model_id, carrier_schema FROM supply.offerings WHERE id = $1")
            .bind(offering)
            .fetch_one(&pool)
            .await
            .expect("offering after merge");
    assert_eq!(
        offering_row
            .try_get::<Uuid, _>("vendor_model_id")
            .expect("vendor model"),
        newer,
        "the offering must be re-pointed at the surviving contract row"
    );
    assert_eq!(
        offering_row
            .try_get::<Value, _>("carrier_schema")
            .expect("carrier"),
        older_surface
    );
    let job_row = sqlx::query(
        "SELECT vendor_model_id, carrier_schema, parameter_mapping FROM generation.jobs WHERE id = $1",
    )
    .bind(job)
    .fetch_one(&pool)
    .await
    .expect("job after merge");
    assert_eq!(
        job_row
            .try_get::<Uuid, _>("vendor_model_id")
            .expect("vendor model"),
        newer
    );
    assert_eq!(
        job_row
            .try_get::<Value, _>("carrier_schema")
            .expect("carrier"),
        older_surface,
        "the job must keep the carrier surface it was accepted with"
    );
    assert_eq!(
        job_row
            .try_get::<Value, _>("parameter_mapping")
            .expect("mapping"),
        json!({})
    );
    let entry_model: Uuid = sqlx::query_scalar(
        "SELECT vendor_model_id FROM publication.runtime_entries WHERE offering_id = $1",
    )
    .bind(offering)
    .fetch_one(&pool)
    .await
    .expect("runtime entry after merge");
    assert_eq!(entry_model, newer);

    // 唯一键与列的形状：内容哈希不再是身份的一部分，它本身也不在了。
    let schema_hash_columns: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.columns
         WHERE table_schema = 'catalog' AND table_name = 'vendor_models' AND column_name = 'schema_hash'",
    )
    .fetch_one(&pool)
    .await
    .expect("schema_hash probe");
    assert_eq!(schema_hash_columns, 0, "内容哈希不再参与身份");
    let duplicate_insert = sqlx::query(
        "INSERT INTO catalog.vendor_models
             (id, vendor_id, native_model_id, native_revision, capability_schema)
         VALUES ($1,'OpenAI','legacy-model','legacy-revision','{}'::jsonb)",
    )
    .bind(Uuid::new_v4())
    .execute(&pool)
    .await;
    assert!(
        duplicate_insert.is_err(),
        "the unique key must be (vendor, model, revision) now"
    );

    pool.close().await;
    let _ = std::fs::remove_dir_all(&staged);
    drop_isolated_database(&database_name).await;
}

/// 增量迁移：命名两列与运维开关表要在**已经建过库、已经有发布数据**的环境里落下来。
///
/// 先在只应用了早期迁移的库上造出"已经发布过一个型号"的数据（修订 + 一条生效条目），
/// 再补上整批迁移，确认：
/// - 修订上回填出对客名与它挂的那行合同，两列非空；
/// - 运维开关按既有生效名字回填出一行（`enabled = true`）；
/// - 迁移后**立刻可读、可停用**：管理端列得出来，也关得掉——不用重新发布一次。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn gateway_model_naming_migration_backfills_existing_publications() {
    let (database_url, database_name) = isolated_database_url().await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");
    let migrations = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../migrations");

    // 1) 只应用这次改动之前的迁移（`0007` 之前，含上一轮的合同/承载面拆分）。
    let staged = std::env::temp_dir().join(format!("seeai-early-migrations-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&staged).expect("staging directory");
    for entry in std::fs::read_dir(&migrations).expect("migrations directory") {
        let entry = entry.expect("migration entry");
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".sql") && name.as_str() < "0007" {
            std::fs::copy(entry.path(), staged.join(&name)).expect("copy early migration");
        }
    }
    sqlx::migrate::Migrator::new(staged.clone())
        .await
        .expect("early migrator")
        .run(&pool)
        .await
        .expect("early migrations apply");

    // 2) 老数据：一个已经发布过的型号，按**这次改动之前**的形状落库
    //    （合同 + 渠道 + 供给 + 计价 + 修订 + 一条生效条目）。
    let vendor_model = Uuid::new_v4();
    let channel = Uuid::new_v4();
    let offering = Uuid::new_v4();
    let price_plan = Uuid::new_v4();
    let revision = Uuid::new_v4();
    let contract = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["model", "prompt"],
        "properties": {
            "model": {"const": "legacy-name"},
            "prompt": {"type": "string"}
        }
    });
    sqlx::query(
        "INSERT INTO catalog.vendor_models
             (id, vendor_id, native_model_id, native_revision, capability_schema)
         VALUES ($1,'OpenAI','legacy-name','legacy-revision',$2)",
    )
    .bind(vendor_model)
    .bind(&contract)
    .execute(&pool)
    .await
    .expect("legacy contract row");
    sqlx::query(
        "INSERT INTO supply.channels (id, provider_kind, base_url, credential_env)
         VALUES ($1,'AIHubMix','https://api.inferera.com','AIHUBMIX_API_KEY')",
    )
    .bind(channel)
    .execute(&pool)
    .await
    .expect("channel fixture");
    sqlx::query(
        "INSERT INTO supply.offerings
             (id, vendor_model_id, channel_id, adapter_key, provider_model_id, restrictions,
              carrier_schema, parameter_mapping)
         VALUES ($1,$2,$3,'aihubmix-image-v1','legacy-name','{}'::jsonb,$4,'{}'::jsonb)",
    )
    .bind(offering)
    .bind(vendor_model)
    .bind(channel)
    .bind(&contract)
    .execute(&pool)
    .await
    .expect("offering fixture");
    sqlx::query(
        "INSERT INTO pricing.price_plans
             (id, offering_id, currency, text_input_microusd_per_million, image_input_microusd_per_million,
              text_output_microusd_per_million, image_output_microusd_per_million, source_url, approved_by)
         VALUES ($1,$2,'USD',0,0,0,0,'https://example.invalid/price','migration-test')",
    )
    .bind(price_plan)
    .bind(offering)
    .execute(&pool)
    .await
    .expect("price plan fixture");
    sqlx::query(
        "INSERT INTO publication.runtime_revisions (id, snapshot, published_by)
         VALUES ($1,'{}'::jsonb,'migration-test')",
    )
    .bind(revision)
    .execute(&pool)
    .await
    .expect("runtime revision fixture");
    sqlx::query(
        "INSERT INTO publication.runtime_entries
             (runtime_revision_id, vendor_model_id, offering_id, price_plan_id, gateway_model, active)
         VALUES ($1,$2,$3,$4,'legacy-name',true)",
    )
    .bind(revision)
    .bind(vendor_model)
    .bind(offering)
    .bind(price_plan)
    .execute(&pool)
    .await
    .expect("runtime entry fixture");

    // 3) 补上整批迁移：命名两列与开关表必须自己跑通，不能因为已有数据就失败。
    sqlx::migrate::Migrator::new(migrations)
        .await
        .expect("migrator")
        .run(&pool)
        .await
        .expect("the naming migration must apply on an already-built database");

    // 4) 回填结果：修订上的对客名与它挂的那行合同，两列都非空。
    let row = sqlx::query(
        "SELECT gateway_model, vendor_model_id FROM publication.runtime_revisions WHERE id = $1",
    )
    .bind(revision)
    .fetch_one(&pool)
    .await
    .expect("runtime revision after migration");
    let gateway_model: String = row.try_get("gateway_model").expect("gateway model");
    let vendor_model_id: Uuid = row.try_get("vendor_model_id").expect("vendor model id");
    assert_eq!(gateway_model, "legacy-name");
    assert_eq!(vendor_model_id, vendor_model, "修订要指向它挂的那行合同");
    for column in ["gateway_model", "vendor_model_id"] {
        let nullable: String = sqlx::query_scalar(
            "SELECT is_nullable FROM information_schema.columns
             WHERE table_schema = 'publication' AND table_name = 'runtime_revisions'
               AND column_name = $1",
        )
        .bind(column)
        .fetch_one(&pool)
        .await
        .expect("column probe");
        assert_eq!(nullable, "NO", "{column} 在既有行上必须非空");
    }
    let (switch, enabled): (String, bool) = {
        let row = sqlx::query(
            "SELECT gateway_model, enabled FROM publication.gateway_models
             WHERE gateway_model = 'legacy-name'",
        )
        .fetch_one(&pool)
        .await
        .expect("backfilled switch row");
        (
            row.try_get("gateway_model").expect("gateway model"),
            row.try_get("enabled").expect("enabled"),
        )
    };
    assert_eq!(switch, "legacy-name");
    assert!(enabled, "既有生效名字回填成启用");

    // 5) 迁移后立刻可读、可停用：走管理端接口（不起 Worker，也不连上游）。
    let (base_url, admin_token, _process) = start_api(&database_url, 1, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url).await;
    let (status, admin) = get_gateway_models(&client, &base_url, Some(&admin_token)).await;
    assert_eq!(status, StatusCode::OK, "{admin}");
    let view = &admin["gateway_models"][0];
    assert_eq!(view["gateway_model"], "legacy-name");
    assert_eq!(view["enabled"], true);
    assert_eq!(view["vendor_id"], "OpenAI");
    assert_eq!(view["native_model_id"], "legacy-name");
    assert_eq!(view["candidates"][0]["provider_kind"], "AIHubMix");
    assert_eq!(view["candidates"][0]["routing_priority"], 0);
    assert_eq!(
        patch_gateway_model(&client, &base_url, &admin_token, "legacy-name", false).await,
        StatusCode::NO_CONTENT,
        "迁移回填出来的名字必须停得掉，不用重新发布一次"
    );
    let (_, admin) = get_gateway_models(&client, &base_url, Some(&admin_token)).await;
    assert_eq!(admin["gateway_models"][0]["enabled"], false);

    pool.close().await;
    let _ = std::fs::remove_dir_all(&staged);
    drop_isolated_database(&database_name).await;
}

/// **迁移 0009 的增量路径**：旧库（只应用 0009 之前的迁移）上的数据在迁移后逐字不变，
/// 定价列留 NULL（旧修订没有定价），三处约束被放宽，汇率表落成空的。
///
/// 三处放宽不是顺手做的：不透支与"保底额可为 0"在库层面直接报错，而它们是这套口径的前提。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_pricing_migration_relaxes_the_balance_checks_on_an_existing_database() {
    let (database_url, database_name) = isolated_database_url().await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");
    let migrations = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../migrations");

    // 1) 只应用这次改动之前的迁移。
    let staged = std::env::temp_dir().join(format!("seeai-pricing-migrations-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&staged).expect("staging directory");
    for entry in std::fs::read_dir(&migrations).expect("migrations directory") {
        let entry = entry.expect("migration entry");
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".sql") && name.as_str() < "0009" {
            std::fs::copy(entry.path(), staged.join(&name)).expect("copy early migration");
        }
    }
    sqlx::migrate::Migrator::new(staged.clone())
        .await
        .expect("early migrator")
        .run(&pool)
        .await
        .expect("early migrations apply");

    // 2) 旧数据：一个已经发布过、**没有定价**的型号，外加一个余额为 0 的账户。
    let vendor_model = Uuid::new_v4();
    let channel = Uuid::new_v4();
    let offering = Uuid::new_v4();
    let price_plan = Uuid::new_v4();
    let revision = Uuid::new_v4();
    let account = Uuid::new_v4();
    let contract = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["model", "prompt"],
        "properties": {
            "model": {"const": "priced-legacy"},
            "prompt": {"type": "string"}
        }
    });
    sqlx::query(
        "INSERT INTO catalog.vendor_models
             (id, vendor_id, native_model_id, native_revision, capability_schema)
         VALUES ($1,'OpenAI','priced-legacy','legacy-revision',$2)",
    )
    .bind(vendor_model)
    .bind(&contract)
    .execute(&pool)
    .await
    .expect("legacy contract row");
    sqlx::query(
        "INSERT INTO supply.channels (id, provider_kind, base_url, credential_env)
         VALUES ($1,'AIHubMix','https://api.inferera.com','AIHUBMIX_API_KEY')",
    )
    .bind(channel)
    .execute(&pool)
    .await
    .expect("channel fixture");
    sqlx::query(
        "INSERT INTO supply.offerings
             (id, vendor_model_id, channel_id, adapter_key, provider_model_id, restrictions,
              carrier_schema, parameter_mapping)
         VALUES ($1,$2,$3,'aihubmix-image-v1','priced-legacy','{}'::jsonb,$4,'{}'::jsonb)",
    )
    .bind(offering)
    .bind(vendor_model)
    .bind(channel)
    .bind(&contract)
    .execute(&pool)
    .await
    .expect("offering fixture");
    sqlx::query(
        "INSERT INTO pricing.price_plans
             (id, offering_id, currency, text_input_microusd_per_million, image_input_microusd_per_million,
              text_output_microusd_per_million, image_output_microusd_per_million, source_url, approved_by)
         VALUES ($1,$2,'USD',5,8,10,30,'https://example.invalid/price','migration-test')",
    )
    .bind(price_plan)
    .bind(offering)
    .execute(&pool)
    .await
    .expect("price plan fixture");
    sqlx::query(
        "INSERT INTO publication.runtime_revisions
             (id, snapshot, published_by, gateway_model, vendor_model_id)
         VALUES ($1,'{}'::jsonb,'migration-test','priced-legacy',$2)",
    )
    .bind(revision)
    .bind(vendor_model)
    .execute(&pool)
    .await
    .expect("runtime revision fixture");
    sqlx::query(
        "INSERT INTO publication.runtime_entries
             (runtime_revision_id, vendor_model_id, offering_id, price_plan_id, gateway_model, active)
         VALUES ($1,$2,$3,$4,'priced-legacy',true)",
    )
    .bind(revision)
    .bind(vendor_model)
    .bind(offering)
    .bind(price_plan)
    .execute(&pool)
    .await
    .expect("runtime entry fixture");
    sqlx::query("INSERT INTO ledger.accounts (id, balance_microusd) VALUES ($1, 0)")
        .bind(account)
        .execute(&pool)
        .await
        .expect("account fixture");

    // 3) 补上整批迁移：定价列、汇率表与三处放宽都必须自己跑通。
    sqlx::migrate::Migrator::new(migrations)
        .await
        .expect("migrator")
        .run(&pool)
        .await
        .expect("the pricing migration must apply on an already-built database");

    // 4) 旧修订的定价列全部为 NULL：它没有定价，受理与结算走旧口径。
    let row = sqlx::query(
        "SELECT markup_bps, reference_cost_microusd, cost_currency, consumer_rates_cny,
                cost_basis, tier_prices, floor_amounts
         FROM publication.runtime_revisions WHERE id = $1",
    )
    .bind(revision)
    .fetch_one(&pool)
    .await
    .expect("runtime revision after migration");
    for column in [
        "markup_bps",
        "reference_cost_microusd",
        "cost_currency",
        "consumer_rates_cny",
        "cost_basis",
        "tier_prices",
        "floor_amounts",
    ] {
        assert!(
            row.try_get::<Option<Value>, _>(column)
                .expect("column probe")
                .is_none(),
            "{column} 在旧修订上必须留 NULL（不回填）"
        );
    }

    // 5) 汇率表落成空的：数值是外部事实，由管理员录入，迁移不预置任何一行。
    let rates: i64 = sqlx::query_scalar("SELECT count(*) FROM pricing.fx_rates")
        .fetch_one(&pool)
        .await
        .expect("fx rates");
    assert_eq!(rates, 0);

    // 6) 三处约束已放宽：余额可为负、保底额与预授权额可为 0。
    sqlx::query("UPDATE ledger.accounts SET balance_microusd = -1 WHERE id = $1")
        .bind(account)
        .execute(&pool)
        .await
        .expect("透支要能把余额扣成负数");
    sqlx::query(
        "INSERT INTO generation.jobs
             (id, account_id, idempotency_key, request_hash, state, branch, gateway_model,
              native_parameters, carrier_schema, parameter_mapping, runtime_revision_id,
              vendor_model_id, offering_id, channel_id, price_snapshot, max_cost_microusd)
         VALUES ($1,$2,'zero-hold','hash','accepted','prompt_only','priced-legacy',
                 '{}'::jsonb,'{}'::jsonb,'{}'::jsonb,$3,$4,$5,$6,'{}'::jsonb,0)",
    )
    .bind(Uuid::new_v4())
    .bind(account)
    .bind(revision)
    .bind(vendor_model)
    .bind(offering)
    .bind(channel)
    .execute(&pool)
    .await
    .expect("保底额可为 0");
    let job_id: Uuid =
        sqlx::query_scalar("SELECT id FROM generation.jobs WHERE idempotency_key = 'zero-hold'")
            .fetch_one(&pool)
            .await
            .expect("job");
    sqlx::query(
        "INSERT INTO ledger.holds (id, account_id, job_id, amount_microusd, status)
         VALUES ($1,$2,$3,0,'active')",
    )
    .bind(Uuid::new_v4())
    .bind(account)
    .bind(job_id)
    .execute(&pool)
    .await
    .expect("零保底额要能落下来");

    pool.close().await;
    let _ = std::fs::remove_dir_all(&staged);
    drop_isolated_database(&database_name).await;
}

/// **迁移 0010 的增量路径**：旧库（只应用 0010 之前的迁移）上的候选条目迁移后逐字不变、
/// 权重取默认 1；唯一索引换成"同一网关模型下同一条供给只能有一行"，**同一档因此可以有两条候选**。
///
/// 这条不是顺手做的：旧索引按 `(native_model_id, routing_priority)` 唯一，等价于"同一档只能有
/// 一条候选"——档内按权重分流要先有第二条候选，旧索引先把这条路堵死了。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_routing_weight_migration_keeps_existing_entries_and_allows_shared_tiers() {
    let (database_url, database_name) = isolated_database_url().await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");
    let migrations = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../migrations");

    // 1) 只应用这次改动之前的迁移。
    let staged = std::env::temp_dir().join(format!("seeai-weight-migrations-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&staged).expect("staging directory");
    for entry in std::fs::read_dir(&migrations).expect("migrations directory") {
        let entry = entry.expect("migration entry");
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".sql") && name.as_str() < "0010" {
            std::fs::copy(entry.path(), staged.join(&name)).expect("copy early migration");
        }
    }
    sqlx::migrate::Migrator::new(staged.clone())
        .await
        .expect("early migrator")
        .run(&pool)
        .await
        .expect("early migrations apply");

    // 2) 旧数据：一个已经发布过、只有一条候选的型号；另备一条供给给"同档第二条候选"用。
    let contract = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["model", "prompt"],
        "properties": {
            "model": {"const": "weighted-legacy"},
            "prompt": {"type": "string"}
        }
    });
    let mut offerings = Vec::new();
    // 合同是**模型级**唯一一份：同一型号的两条候选共用这一行，各自带自己的渠道、供给与价格计划。
    let vendor_model = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO catalog.vendor_models
             (id, vendor_id, native_model_id, native_revision, capability_schema)
         VALUES ($1,'OpenAI','weighted-legacy','legacy-revision',$2)",
    )
    .bind(vendor_model)
    .bind(&contract)
    .execute(&pool)
    .await
    .expect("legacy contract row");
    for _ in 0..2 {
        let channel = Uuid::new_v4();
        let offering = Uuid::new_v4();
        let price_plan = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO supply.channels (id, provider_kind, base_url, credential_env)
             VALUES ($1,'AIHubMix','https://api.inferera.com','AIHUBMIX_API_KEY')",
        )
        .bind(channel)
        .execute(&pool)
        .await
        .expect("channel fixture");
        sqlx::query(
            "INSERT INTO supply.offerings
                 (id, vendor_model_id, channel_id, adapter_key, provider_model_id, restrictions,
                  carrier_schema, parameter_mapping)
             VALUES ($1,$2,$3,'aihubmix-image-v1','weighted-legacy','{}'::jsonb,$4,'{}'::jsonb)",
        )
        .bind(offering)
        .bind(vendor_model)
        .bind(channel)
        .bind(&contract)
        .execute(&pool)
        .await
        .expect("offering fixture");
        sqlx::query(
            "INSERT INTO pricing.price_plans
                 (id, offering_id, currency, text_input_microusd_per_million, image_input_microusd_per_million,
                  text_output_microusd_per_million, image_output_microusd_per_million, source_url, approved_by)
             VALUES ($1,$2,'USD',5,8,10,30,'https://example.invalid/price','migration-test')",
        )
        .bind(price_plan)
        .bind(offering)
        .execute(&pool)
        .await
        .expect("price plan fixture");
        offerings.push((offering, price_plan));
    }
    let (offering, price_plan) = offerings[0];
    let revision = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO publication.runtime_revisions
             (id, snapshot, published_by, gateway_model, vendor_model_id)
         VALUES ($1,'{}'::jsonb,'migration-test','weighted-legacy',$2)",
    )
    .bind(revision)
    .bind(vendor_model)
    .execute(&pool)
    .await
    .expect("runtime revision fixture");
    sqlx::query(
        "INSERT INTO publication.runtime_entries
             (runtime_revision_id, vendor_model_id, offering_id, price_plan_id, gateway_model,
              active, routing_priority)
         VALUES ($1,$2,$3,$4,'weighted-legacy',true,0)",
    )
    .bind(revision)
    .bind(vendor_model)
    .bind(offering)
    .bind(price_plan)
    .execute(&pool)
    .await
    .expect("runtime entry fixture");

    // 3) 补上整批迁移。
    sqlx::migrate::Migrator::new(migrations)
        .await
        .expect("migrator")
        .run(&pool)
        .await
        .expect("the routing weight migration must apply on an already-built database");

    // 4) 旧条目逐字不变，权重取默认 1（不回填、不改写）。
    let row = sqlx::query(
        "SELECT routing_priority, weight FROM publication.runtime_entries
         WHERE runtime_revision_id = $1 AND offering_id = $2",
    )
    .bind(revision)
    .bind(offering)
    .fetch_one(&pool)
    .await
    .expect("legacy entry after migration");
    assert_eq!(
        row.try_get::<i32, _>("routing_priority").expect("priority"),
        0
    );
    assert_eq!(row.try_get::<i32, _>("weight").expect("weight"), 1);

    // 5) 旧索引已换成新的那条。
    let old_index: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_indexes
         WHERE schemaname = 'publication' AND indexname = 'one_active_entry_per_model_and_priority'",
    )
    .fetch_one(&pool)
    .await
    .expect("old index probe");
    assert_eq!(old_index, 0, "旧索引必须消失");
    let new_index: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_indexes
         WHERE schemaname = 'publication' AND indexname = 'one_active_entry_per_model_and_offering'",
    )
    .fetch_one(&pool)
    .await
    .expect("new index probe");
    assert_eq!(new_index, 1, "新索引必须建起来");

    // 6) 同一档现在可以有第二条候选（旧索引下这一条会撞唯一约束）。
    let (second_offering, second_price_plan) = offerings[1];
    sqlx::query(
        "INSERT INTO publication.runtime_entries
             (runtime_revision_id, vendor_model_id, offering_id, price_plan_id, gateway_model,
              active, routing_priority, weight)
         VALUES ($1,$2,$3,$4,'weighted-legacy',true,0,3)",
    )
    .bind(revision)
    .bind(vendor_model)
    .bind(second_offering)
    .bind(second_price_plan)
    .execute(&pool)
    .await
    .expect("同一档的第二条候选必须能落下来");

    // 7) 权重必须是正整数；同一网关模型下同一条供给只允许一行 active。
    let zero_weight = sqlx::query(
        "INSERT INTO publication.runtime_entries
             (runtime_revision_id, vendor_model_id, offering_id, price_plan_id, gateway_model,
              active, routing_priority, weight)
         VALUES ($1,$2,$3,$4,'weighted-legacy',true,1,0)",
    )
    .bind(revision)
    .bind(vendor_model)
    .bind(second_offering)
    .bind(second_price_plan)
    .execute(&pool)
    .await;
    assert!(zero_weight.is_err(), "权重 0 必须在库层被拒");
    let duplicate = sqlx::query(
        "INSERT INTO publication.runtime_entries
             (runtime_revision_id, vendor_model_id, offering_id, price_plan_id, gateway_model,
              active, routing_priority, weight)
         VALUES ($1,$2,$3,$4,'weighted-legacy',true,1,1)",
    )
    .bind(revision)
    .bind(vendor_model)
    .bind(second_offering)
    .bind(second_price_plan)
    .execute(&pool)
    .await;
    assert!(
        duplicate.is_err(),
        "同一网关模型下同一条供给的 active 条目只能有一条"
    );

    pool.close().await;
    let _ = std::fs::remove_dir_all(&staged);
    drop_isolated_database(&database_name).await;
}
