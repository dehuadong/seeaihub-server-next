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

    // 3.5) 0028 的回填：这张老库里已经存在的账户也必须有一个非空名称（账户 id 形式的占位名）——
    // 名称列是非空且无默认值的，回填不成立这张库就迁不过去。
    let backfilled: String = sqlx::query_scalar("SELECT name FROM ledger.accounts WHERE id = $1")
        .bind(account)
        .fetch_one(&pool)
        .await
        .expect("account name backfilled");
    assert_eq!(
        backfilled,
        format!("账户_{}", &account.simple().to_string()[..8]),
        "旧行按账户 id 前 8 位回填"
    );

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
        "SELECT vendor_model_id, carrier_schema, parameter_mapping, base_url, credential_env
         FROM generation.jobs WHERE id = $1",
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
    // 执行入口与凭证名也在这次增量里从渠道行回填：老 Job 不因为库是"先建后补"就缺这两列。
    assert_eq!(
        job_row.try_get::<String, _>("base_url").expect("entry"),
        "https://api.inferera.com",
        "the legacy job must get the entry it was pointed at backfilled"
    );
    assert_eq!(
        job_row
            .try_get::<String, _>("credential_env")
            .expect("credential name"),
        "AIHUBMIX_API_KEY"
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
    wait_until_ready(&client, &base_url, &admin_token).await;
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

    // 3.5) 0028 的回填：老库里的账户在这张库上也要有非空名称。
    let backfilled: String = sqlx::query_scalar("SELECT name FROM ledger.accounts WHERE id = $1")
        .bind(account)
        .fetch_one(&pool)
        .await
        .expect("account name backfilled");
    assert_eq!(
        backfilled,
        format!("账户_{}", &account.simple().to_string()[..8])
    );

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
              native_parameters, carrier_schema, parameter_mapping, adapter_key, provider_model_id,
              base_url, credential_env,
              runtime_revision_id, vendor_model_id, offering_id, channel_id, price_snapshot,
              max_cost_microusd)
         VALUES ($1,$2,'zero-hold','hash','accepted','prompt_only','priced-legacy',
                 '{}'::jsonb,'{}'::jsonb,'{}'::jsonb,'aihubmix-image-v1','priced-legacy',
                 'https://api.inferera.com','AIHUBMIX_API_KEY',
                 $3,$4,$5,$6,'{}'::jsonb,0)",
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
    for index in 0..2 {
        let channel = Uuid::new_v4();
        let offering = Uuid::new_v4();
        let price_plan = Uuid::new_v4();
        // 两条候选各占一个**渠道身份**（地址不同）：一个入口只允许一条供给，两条候选要落在同一档
        // 就得是两个入口——这也正是"同档两条候选"在真实发布里的样子。
        sqlx::query(
            "INSERT INTO supply.channels (id, provider_kind, base_url, credential_env)
             VALUES ($1,'AIHubMix',$2,'AIHUBMIX_API_KEY')",
        )
        .bind(channel)
        .bind(format!("https://api.inferera.com/channel-{index}"))
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
              active, routing_priority, weight,
              adapter_key, provider_model_id, carrier_schema, parameter_mapping, restrictions,
              provider_kind, base_url, credential_env)
         SELECT $1,$2,$3,$4,'weighted-legacy',true,0,3,
                o.adapter_key, o.provider_model_id, o.carrier_schema, o.parameter_mapping, o.restrictions,
                c.provider_kind, c.base_url, c.credential_env
         FROM supply.offerings o JOIN supply.channels c ON c.id = o.channel_id
         WHERE o.id = $3",
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
              active, routing_priority, weight,
              adapter_key, provider_model_id, carrier_schema, parameter_mapping, restrictions,
              provider_kind, base_url, credential_env)
         SELECT $1,$2,$3,$4,'weighted-legacy',true,1,0,
                o.adapter_key, o.provider_model_id, o.carrier_schema, o.parameter_mapping, o.restrictions,
                c.provider_kind, c.base_url, c.credential_env
         FROM supply.offerings o JOIN supply.channels c ON c.id = o.channel_id
         WHERE o.id = $3",
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
              active, routing_priority, weight,
              adapter_key, provider_model_id, carrier_schema, parameter_mapping, restrictions,
              provider_kind, base_url, credential_env)
         SELECT $1,$2,$3,$4,'weighted-legacy',true,1,1,
                o.adapter_key, o.provider_model_id, o.carrier_schema, o.parameter_mapping, o.restrictions,
                c.provider_kind, c.base_url, c.credential_env
         FROM supply.offerings o JOIN supply.channels c ON c.id = o.channel_id
         WHERE o.id = $3",
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

/// **迁移 0014 的增量路径**：供给身份的两条唯一索引要在已建过的库上落下来；库里已经有同身份
/// 重复行时**响亮失败**，不在迁移里静默归并。
///
/// 归并要改挂 `runtime_entries` / `jobs` / `routing_decisions` 的 `offering_id` 与 `channel_id`，
/// 那是已发布修订与已受理 Job 的事实——迁移不改写它们。所以这里验三件事：有重复时迁移报错且
/// 不留半截状态；人工清掉重复之后建得上；建上之后重复身份真的写不进去。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_supply_identity_migration_adds_unique_indexes_and_refuses_duplicates() {
    let (database_url, database_name) = isolated_database_url().await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");
    let migrations = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../migrations");

    // 1) 只应用这次改动之前的迁移。
    let staged = std::env::temp_dir().join(format!("seeai-supply-migrations-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&staged).expect("staging directory");
    for entry in std::fs::read_dir(&migrations).expect("migrations directory") {
        let entry = entry.expect("migration entry");
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".sql") && name.as_str() < "0014" {
            std::fs::copy(entry.path(), staged.join(&name)).expect("copy early migration");
        }
    }
    sqlx::migrate::Migrator::new(staged.clone())
        .await
        .expect("early migrator")
        .run(&pool)
        .await
        .expect("early migrations apply");

    // 2) 老形状留下的同身份重复行：老发布每次都给候选新插一行渠道，所以同一个入口会有多行。
    let identity = "https://api.inferera.com";
    for _ in 0..2 {
        sqlx::query(
            "INSERT INTO supply.channels (id, provider_kind, base_url, credential_env)
             VALUES ($1,'AIHubMix',$2,'AIHUBMIX_API_KEY')",
        )
        .bind(Uuid::new_v4())
        .bind(identity)
        .execute(&pool)
        .await
        .expect("legacy channel row");
    }

    // 3) 有重复行时迁移**报错**，而且不留半截状态：失败的那次不记账、索引也不在。
    let full = sqlx::migrate::Migrator::new(migrations.clone())
        .await
        .expect("migrator");
    assert!(
        full.run(&pool).await.is_err(),
        "同身份重复行必须让迁移响亮失败，而不是被静默归并"
    );
    let applied: i64 =
        sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE version = 14")
            .fetch_one(&pool)
            .await
            .expect("migration ledger");
    assert_eq!(applied, 0, "失败的那一次迁移不该留在账上");
    let index: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_indexes
         WHERE schemaname = 'supply' AND indexname = 'channels_identity'",
    )
    .fetch_one(&pool)
    .await
    .expect("index probe");
    assert_eq!(index, 0, "失败的迁移不该留下半截索引");

    // 4) 重复行由**人**显式清掉（开发库上这一步是重建库），迁移随即建得上。
    sqlx::query("DELETE FROM supply.channels")
        .execute(&pool)
        .await
        .expect("deduplicate by hand");
    full.run(&pool)
        .await
        .expect("the supply identity migration must apply once duplicates are gone");
    for name in ["channels_identity", "offerings_identity"] {
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_indexes WHERE schemaname = 'supply' AND indexname = $1",
        )
        .bind(name)
        .fetch_one(&pool)
        .await
        .expect("index probe");
        assert_eq!(count, 1, "{name} 必须建起来");
    }

    // 5) 身份真的唯一：重复的渠道与重复的供给都写不进去。
    let channel = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO supply.channels (id, provider_kind, base_url, credential_env)
         VALUES ($1,'AIHubMix',$2,'AIHUBMIX_API_KEY')",
    )
    .bind(channel)
    .bind(identity)
    .execute(&pool)
    .await
    .expect("channel fixture");
    let duplicate_channel = sqlx::query(
        "INSERT INTO supply.channels (id, provider_kind, base_url, credential_env)
         VALUES ($1,'AIHubMix',$2,'AIHUBMIX_API_KEY')",
    )
    .bind(Uuid::new_v4())
    .bind(identity)
    .execute(&pool)
    .await;
    assert!(duplicate_channel.is_err(), "同一个入口只允许一行");

    let vendor_model = Uuid::new_v4();
    let contract = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["model", "prompt"],
        "properties": {
            "model": {"const": "supply-identity"},
            "prompt": {"type": "string"}
        }
    });
    sqlx::query(
        "INSERT INTO catalog.vendor_models
             (id, vendor_id, native_model_id, native_revision, capability_schema)
         VALUES ($1,'OpenAI','supply-identity','revision-1',$2)",
    )
    .bind(vendor_model)
    .bind(&contract)
    .execute(&pool)
    .await
    .expect("contract row");
    sqlx::query(
        "INSERT INTO supply.offerings
             (id, vendor_model_id, channel_id, adapter_key, provider_model_id, restrictions,
              carrier_schema, parameter_mapping)
         VALUES ($1,$2,$3,'aihubmix-image-v1','supply-identity','{}'::jsonb,$4,'{}'::jsonb)",
    )
    .bind(Uuid::new_v4())
    .bind(vendor_model)
    .bind(channel)
    .bind(&contract)
    .execute(&pool)
    .await
    .expect("offering fixture");
    let duplicate_offering = sqlx::query(
        "INSERT INTO supply.offerings
             (id, vendor_model_id, channel_id, adapter_key, provider_model_id, restrictions,
              carrier_schema, parameter_mapping)
         VALUES ($1,$2,$3,'aihubmix-image-v1','supply-identity','{}'::jsonb,$4,'{}'::jsonb)",
    )
    .bind(Uuid::new_v4())
    .bind(vendor_model)
    .bind(channel)
    .bind(&contract)
    .execute(&pool)
    .await;
    assert!(
        duplicate_offering.is_err(),
        "同一个模型经同一个入口只允许一条供给"
    );

    pool.close().await;
    let _ = std::fs::remove_dir_all(&staged);
    drop_isolated_database(&database_name).await;
}

/// 名称唯一迁移在**已经存在重名行**的库上跑得通：先去重（保留最早那一行），再建唯一索引。
///
/// 这条是给非空开发库兜底的路径：`0028` 的回填本身不重名，但人可以改名——改名接口在 v3 之前不挡重名。
/// 因此构造方式刻意是"先只应用 0028 之前的迁移、手动造出重名行，再补上 0029"。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_account_name_migration_deduplicates_names_and_adds_a_unique_index() {
    let (database_url, database_name) = isolated_database_url().await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to the isolated database");
    let migrations = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../migrations");

    // 1) 只应用 0028 及更早的迁移：名称列已经存在且非空，但还没有唯一索引。
    let staged = std::env::temp_dir().join(format!("seeai-name-migrations-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&staged).expect("staging directory");
    for entry in std::fs::read_dir(&migrations).expect("migrations directory") {
        let entry = entry.expect("migration entry");
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".sql") && name.as_str() < "0029" {
            std::fs::copy(entry.path(), staged.join(&name)).expect("copy early migration");
        }
    }
    sqlx::migrate::Migrator::new(staged.clone())
        .await
        .expect("early migrator")
        .run(&pool)
        .await
        .expect("early migrations apply");

    // 2) 造出重名：两行完全同名、两行只差大小写、两行都是 100 个字符（后缀不能把它顶出长度上限）。
    let earliest = Uuid::new_v4();
    let same_name = Uuid::new_v4();
    let case_first = Uuid::new_v4();
    let case_second = Uuid::new_v4();
    let longest_first = Uuid::new_v4();
    let longest_second = Uuid::new_v4();
    // 构造性撞名：`crafted` 与 `victim` 同名，而 `squatted` 已经占着 crafted 改名后会得到的名字；
    // 单条 UPDATE 解决不了它（改出来的名字正好撞上另一行），迁移里的循环必须再跑一轮。
    let victim = Uuid::new_v4();
    let crafted = Uuid::new_v4();
    let squatter = Uuid::new_v4();
    let squatted = format!("撞名_{}", crafted.simple());
    let long_name = "长".repeat(100);
    for (id, name, created) in [
        (earliest, "星尘工作室".to_owned(), "2026-10-01T00:00:00Z"),
        (same_name, "星尘工作室".to_owned(), "2026-10-02T00:00:00Z"),
        (
            case_first,
            "GlobalStudio".to_owned(),
            "2026-10-02T00:00:00Z",
        ),
        (
            case_second,
            "globalstudio".to_owned(),
            "2026-10-03T00:00:00Z",
        ),
        (longest_first, long_name.clone(), "2026-10-01T00:00:00Z"),
        (longest_second, long_name.clone(), "2026-10-04T00:00:00Z"),
        (victim, "撞名".to_owned(), "2026-10-05T00:00:00Z"),
        (crafted, "撞名".to_owned(), "2026-10-06T00:00:00Z"),
        (squatter, squatted.clone(), "2026-10-07T00:00:00Z"),
    ] {
        sqlx::query(
            "INSERT INTO ledger.accounts (id, name, balance_microusd, created_at, updated_at)
             VALUES ($1, $2, 0, $3::timestamptz, $3::timestamptz)",
        )
        .bind(id)
        .bind(&name)
        .bind(created)
        .execute(&pool)
        .await
        .expect("duplicate-name fixture");
    }

    // 去重只改名，不动行：迁移前后账户行数必须一致（数量、绑定、标签、资金、密钥与历史都不动）。
    let rows_before: i64 = sqlx::query_scalar("SELECT count(*) FROM ledger.accounts")
        .fetch_one(&pool)
        .await
        .expect("row count");
    assert!(rows_before >= 9, "夹具至少九行：{rows_before}");

    // 3) 补上整批迁移：0029 必须自己去重后把索引建起来。
    sqlx::migrate::Migrator::new(migrations)
        .await
        .expect("migrator")
        .run(&pool)
        .await
        .expect("the name uniqueness migration must apply on a database with duplicates");

    // 4) 库内不再有重名（大小写不敏感），每组最早那一行保留原值，其余各接上自己的 id 片段。
    let rows_after: i64 = sqlx::query_scalar("SELECT count(*) FROM ledger.accounts")
        .fetch_one(&pool)
        .await
        .expect("row count after");
    assert_eq!(rows_after, rows_before, "去重只改名，不删行也不插行");

    // 最终口径是区分大小写：这里断言"没有逐字符完全相同的名称"，只差大小写可以并存（下面另验）。
    let duplicates: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM (
             SELECT name FROM ledger.accounts GROUP BY name HAVING count(*) > 1
         ) AS duplicated",
    )
    .fetch_one(&pool)
    .await
    .expect("duplicate count");
    assert_eq!(duplicates, 0, "迁移后不该还有完全相同的名称");

    for (id, expected) in [
        (earliest, "星尘工作室"),
        (case_first, "GlobalStudio"),
        (longest_first, long_name.as_str()),
    ] {
        let kept: String = sqlx::query_scalar("SELECT name FROM ledger.accounts WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("kept name");
        assert_eq!(kept, expected, "每组最早那一行保留原值");
    }
    for (id, prefix) in [(same_name, "星尘工作室_"), (case_second, "globalstudio_")] {
        let renamed: String = sqlx::query_scalar("SELECT name FROM ledger.accounts WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("renamed row");
        assert!(renamed.starts_with(prefix), "{renamed} 应当接上 id 片段");
        assert!(
            renamed.ends_with(&id.simple().to_string()),
            "{renamed} 应当以完整账户 id 结尾（后缀唯一由它保证）"
        );
    }
    // `0029` 的去重比最终口径更严：只差大小写的那一对当时被拆开了（`globalstudio` → 带 id 后缀）。
    // 这是历史迁移的既成结果，开发阶段不恢复。
    // 100 个字符那一组：被改名的那行仍在上限内（后缀按上限截断后再拼），长度 CHECK 不会被顶穿。
    let renamed_longest: String =
        sqlx::query_scalar("SELECT name FROM ledger.accounts WHERE id = $1")
            .bind(longest_second)
            .fetch_one(&pool)
            .await
            .expect("renamed long row");
    assert_eq!(
        renamed_longest.chars().count(),
        100,
        "后缀不能把名称顶出长度上限"
    );
    assert!(renamed_longest.starts_with(&"长".repeat(67)));
    assert!(renamed_longest.ends_with(&longest_second.simple().to_string()));

    // 4.5) 构造性撞名那一组也收敛了：`crafted` 拿到了带自己 id 的名字，`squatter` 因为撞名又被拆开一层。
    let crafted_name: String = sqlx::query_scalar("SELECT name FROM ledger.accounts WHERE id = $1")
        .bind(crafted)
        .fetch_one(&pool)
        .await
        .expect("crafted row");
    assert!(
        crafted_name.ends_with(&crafted.simple().to_string()),
        "{crafted_name} 应当以 crafted 自己的完整 id 结尾"
    );
    let squatter_name: String =
        sqlx::query_scalar("SELECT name FROM ledger.accounts WHERE id = $1")
            .bind(squatter)
            .fetch_one(&pool)
            .await
            .expect("squatter row");
    assert_ne!(squatter_name, crafted_name, "两行不能同名");
    assert!(
        squatter_name.ends_with(&squatter.simple().to_string()),
        "{squatter_name}：它原本占着的名字被改走后，自己也被拆开一层"
    );

    // 5) 最终索引（`0030` 之后）按**区分大小写**挡重名：完全相同的写入失败，只差大小写的可以并存。
    let duplicate_insert = sqlx::query(
        "INSERT INTO ledger.accounts (id, name, balance_microusd) VALUES ($1, '星尘工作室', 0)",
    )
    .bind(Uuid::new_v4())
    .execute(&pool)
    .await;
    assert!(duplicate_insert.is_err(), "唯一索引必须挡住完全相同的名称");

    let case_variant = sqlx::query(
        "INSERT INTO ledger.accounts (id, name, balance_microusd) VALUES ($1, 'GLOBALSTUDIO', 0)",
    )
    .bind(Uuid::new_v4())
    .execute(&pool)
    .await;
    assert!(
        case_variant.is_ok(),
        "0030 之后只差大小写是另一个名称，不该被唯一索引挡住：{case_variant:?}"
    );

    pool.close().await;
    let _ = std::fs::remove_dir_all(&staged);
    drop_isolated_database(&database_name).await;
}
