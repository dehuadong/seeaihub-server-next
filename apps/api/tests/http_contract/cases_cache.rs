use super::*;

/// **管理员读余额读的是数据库那一行，不是缓存**。
///
/// 这条读服务于运营查看与对账：缓存里的值可能滞后、也可能来自对账覆盖，用它当答案会把
/// "账实不符"读成"账实相符"。所以这里先把缓存改成一个错的数，读回来的仍必须是库里的值。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_admin_reads_a_balance_from_the_database_not_the_cache() {
    let harness = Harness::start_with_cache(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
        30,
        CacheFixture::start(CacheSettings::default()).await,
    )
    .await;
    let client = Client::new();
    let (account_id, _api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 375_000).await;

    // 缓存里放一个错的数：读余额若走缓存，就会读到它而不是库里的 375000。
    let fresh = harness
        .cache()
        .balance(&account_id)
        .expect("建账户之后缓存里应有余额");
    let written_at = fresh["written_at"].clone();
    harness
        .cache()
        .corrupt_balance(&account_id, 7, "db_commit", written_at);

    let response = client
        .get(format!("{}/api/v1/accounts/{account_id}", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .send()
        .await
        .expect("admin balance read");
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.expect("balance JSON");
    assert_eq!(
        body["balance_microusd"],
        json!(375_000),
        "读的是数据库那一行，不是缓存里的 7：{body}"
    );
    assert!(
        body["updated_at"].is_string(),
        "要一起给出写入时刻，运营才看得出这个数是什么时候的：{body}"
    );

    let in_db: i64 =
        sqlx::query_scalar("SELECT balance_microusd FROM ledger.accounts WHERE id = $1::uuid")
            .bind(&account_id)
            .fetch_one(&harness.pool)
            .await
            .expect("account row");
    assert_eq!(body["balance_microusd"].as_i64(), Some(in_db));

    // 没有管理员凭证：403；账户不存在：404。
    let response = client
        .get(format!("{}/api/v1/accounts/{account_id}", harness.base_url))
        .send()
        .await
        .expect("unauthenticated read");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let unknown = Uuid::new_v4();
    let response = client
        .get(format!("{}/api/v1/accounts/{unknown}", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .send()
        .await
        .expect("unknown account read");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    harness.cleanup().await;
}

/// **流水与对客账户面读的也是数据库那一行，不是缓存**。
///
/// 与管理员读余额同一条口径、同一个理由：缓存里的值可能滞后、也可能刚被对账覆盖写回，而这两条
/// 读的用途正是让人查看与核对"这笔钱到底扣没扣"。所以先把缓存改成一个错的数，两个接口读回来的
/// 仍必须都是库里的值。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_ledger_view_and_the_consumer_account_read_the_database_not_the_cache() {
    let harness = Harness::start_with_cache(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
        30,
        // 余额缓存与 route 缓存都调得很长：真走缓存的话，下面读到的就会是那个错数。
        CacheFixture::start(CacheSettings {
            route_ttl_seconds: 600,
            balance_ttl_seconds: 600,
            ..CacheSettings::default()
        })
        .await,
    )
    .await;
    let client = Client::new();
    // 夹具账户已经有余额，照着它发一把对客 Key。
    let api_key = issue_key(
        &client,
        &harness.base_url,
        &harness.admin_token,
        &harness.account_id,
    )
    .await;
    let account_id = harness.account_id.clone();

    // 先把缓存改成一个错数。
    let fresh = harness
        .cache()
        .balance(&account_id)
        .expect("建账户之后缓存里应有余额");
    let written_at = fresh["written_at"].clone();
    harness
        .cache()
        .corrupt_balance(&account_id, 7, "db_commit", written_at);

    let db_balance = database_balance(&harness, &account_id).await;
    assert_ne!(db_balance, 7, "库里的值与缓存里的错数要分得开，才试得出来");

    let entries: Value = client
        .get(format!(
            "{}/api/v1/accounts/{account_id}/entries",
            harness.base_url
        ))
        .bearer_auth(&harness.admin_token)
        .send()
        .await
        .expect("ledger entries")
        .json()
        .await
        .expect("ledger entries JSON");
    assert_eq!(
        entries["count"].as_u64(),
        Some(1),
        "账户只有充值那一条：{entries}"
    );
    assert_eq!(
        entries["entries"][0]["kind"].as_str(),
        Some("credit"),
        "流水里看到的是账本上真实的那一条：{entries}"
    );
    let credited = entries["entries"][0]["amount_microusd"]
        .as_i64()
        .expect("credit amount");
    assert_eq!(
        credited, db_balance,
        "账本上的充值额就是库里的余额：{entries}"
    );

    let own: Value = client
        .get(format!("{}/v1/account", harness.base_url))
        .bearer_auth(&api_key)
        .send()
        .await
        .expect("consumer account")
        .json()
        .await
        .expect("consumer account JSON");
    assert_eq!(
        own["balance_microusd"].as_i64(),
        Some(db_balance),
        "对客面读的是库里的 {db_balance}，不是缓存里那个错的 7：{own}"
    );
    assert_eq!(
        own["held_microusd"].as_i64(),
        Some(0),
        "没有任何持有中的预授权：{own}"
    );
    assert_eq!(
        harness.cache().balance(&account_id).expect("cache entry")["balance_microusd"],
        json!(7),
        "缓存里那个错数还在：这两个接口没有去动它（读不写回）"
    );

    harness.cleanup().await;
}

/// **写穿**：充值、受理预授权扣减、结算三条路径都在数据库提交之后把余额写进缓存。
///
/// 一次用例把三条路径都走一遍：充值后缓存立刻是充值后的值；不跑 Worker 发一次请求（同步入口
/// 超时，但 Job 已经受理、预授权已经扣），缓存跟着变成"初始 − 保底额"；再起 Worker 把同一个 Job
/// 跑完，缓存变成结算后的余额。每一步都与数据库逐位比对。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn cache_write_through_makes_the_balance_visible_after_every_write() {
    let harness = Harness::start_with_cache(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
        1,
        CacheFixture::start(CacheSettings::default()).await,
    )
    .await;
    let client = Client::new();
    assert_eq!(
        publish_cache_priced(&harness, priced_consumer_rates()).await,
        StatusCode::OK,
        "带定价的发布必须成功"
    );

    // ① 充值：提交后立刻可见。
    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let cached = harness
        .cache()
        .balance(&account_id)
        .expect("充值之后缓存里必须立刻有余额");
    assert_eq!(cached["balance_microusd"], json!(1_000_000));
    assert_eq!(cached["source"], json!("db_commit"));
    assert_eq!(
        cached["balance_microusd"],
        json!(database_balance(&harness, &account_id).await),
        "缓存里的值与数据库逐位一致"
    );

    // ② 受理（预授权扣减）：不跑 Worker，同步入口 1 秒后超时；Job 已受理、保底额已扣。
    let key = format!("cache-hold-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "cache contract");
    request["size"] = json!("2K");
    request["quality"] = json!("low");
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "got {body}");
    let (job_id, state, _) = harness.job(&key).await;
    assert_eq!(state, "accepted");
    let cached = harness.cache().balance(&account_id).expect("受理之后缓存");
    assert_eq!(
        cached["balance_microusd"],
        json!(1_000_000 - 250_000),
        "缓存跟着变成扣掉保底额之后的值"
    );
    assert_eq!(
        cached["balance_microusd"],
        json!(database_balance(&harness, &account_id).await)
    );

    // route 缓存也建起来了，且带着**当前生效修订**的标识。
    let cached_route = harness
        .cache()
        .route(harness.model)
        .expect("受理之后 route 缓存必须建起来");
    let effective: Uuid =
        sqlx::query_scalar("SELECT runtime_revision_id FROM generation.jobs WHERE id = $1")
            .bind(job_id)
            .fetch_one(&harness.pool)
            .await
            .expect("job revision");
    assert_eq!(
        cached_route["runtime_revision_id"],
        json!(effective.to_string()),
        "route 缓存带的是写它那次发布的修订标识"
    );

    // ③ 结算：起 Worker 把同一个 Job 跑完，缓存变成实收之后的余额。
    let _worker = harness.spawn_worker();
    wait_for_job_state(&harness, &key, "succeeded").await;
    let settled = database_balance(&harness, &account_id).await;
    assert_eq!(settled, 1_000_000 - 43_680, "实收按对客费率向量算");
    let cached = harness.cache().balance(&account_id).expect("结算之后缓存");
    assert_eq!(cached["balance_microusd"], json!(settled));
    assert_eq!(cached["source"], json!("db_commit"));

    harness.cleanup().await;
}

/// **停掉缓存服务，结果逐位相同**：同一场景跑两遍（配了缓存但把服务关掉 / 完全不配缓存），
/// 实收、最终余额、Job 终态与对客响应体都逐位相同——降级是"全部回源数据库"，不是"另一条路径"。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn stopping_the_cache_leaves_acceptance_and_settlement_bit_identical() {
    /// 跑一遍完整场景，回读可比对的四个数。
    async fn run(cache: Option<CacheFixture>) -> (i64, i64, String, Vec<(Vec<String>, bool)>) {
        let draft = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
        let harness = match cache {
            Some(cache) => {
                Harness::start_with_cache(
                    draft,
                    None,
                    UpstreamBehaviour::aihubmix(SyncImageShape::Url),
                    64,
                    30,
                    cache,
                )
                .await
            }
            None => {
                Harness::start_with_draft(
                    draft,
                    None,
                    UpstreamBehaviour::aihubmix(SyncImageShape::Url),
                    64,
                )
                .await
            }
        };
        let client = Client::new();
        assert_eq!(
            publish_cache_priced(&harness, priced_consumer_rates()).await,
            StatusCode::OK
        );
        let (account_id, api_key) =
            funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
        // 充值把缓存写起来之后再把缓存服务关掉：这时"缓存里有一条值、服务却不可用"。
        if let Some(cache) = harness.cache.as_ref() {
            assert!(cache.balance(&account_id).is_some());
            cache.stop();
        }
        let _worker = harness.spawn_worker();
        let key = format!("cache-down-{}", Uuid::new_v4());
        let mut request = route_request(harness.model, "cache down contract");
        request["size"] = json!("2K");
        request["quality"] = json!("low");
        let (status, body) = post_json(
            &harness.base_url,
            &api_key,
            "/v1/images/generations",
            &key,
            &request,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "got {body}");
        let (job_id, state, _) = harness.job(&key).await;
        let captured = harness.captured_microusd(job_id).await;
        let balance = database_balance(&harness, &account_id).await;
        let shape = comparable_response(&body);
        harness.cleanup().await;
        (captured, balance, state, shape)
    }

    let with_cache = run(Some(CacheFixture::start(CacheSettings::default()).await)).await;
    let without_cache = run(None).await;
    assert_eq!(
        with_cache, without_cache,
        "缓存不可用与完全没有缓存必须逐位相同（实收、余额、终态、响应体）"
    );
    assert_eq!(with_cache.0, -43_680, "实收按对客费率向量算");
    assert_eq!(with_cache.1, 1_000_000 - 43_680);
}

/// **route 缓存陈旧不可用**：发布新修订之后让失效失败（或手工把值里的修订标识改旧）→ 受理
/// **回源数据库**读到新候选集，选路与定价都用新修订那一份。
///
/// 这条验的是"陈旧可检"：正确性不依赖"发布后的失效一定成功"。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_stale_route_cache_falls_back_to_the_database() {
    let harness = Harness::start_with_cache(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
        30,
        CacheFixture::start(CacheSettings::default()).await,
    )
    .await;
    let client = Client::new();
    assert_eq!(
        publish_cache_priced(&harness, priced_consumer_rates()).await,
        StatusCode::OK
    );
    let (_, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;

    // 先受理一次，把 route 缓存写起来（带着第一版修订的标识）。
    let first_key = format!("cache-route-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "route cache contract");
    request["size"] = json!("2K");
    request["quality"] = json!("low");
    let _worker = harness.spawn_worker();
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &first_key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let stale = harness.cache().route(harness.model).expect("route 缓存");
    assert_eq!(
        frozen_snapshot(&harness.pool, &first_key).await["consumer_rates_cny"],
        priced_consumer_rates()
    );

    // 换一份对客费率重发修订，并让**失效失败**：缓存里留着的还是第一版那份候选集。
    harness.cache().set_fail_writes(true);
    let mut higher = priced_consumer_rates();
    higher["image_output_micros_per_million"] = json!(440_000_000);
    assert_eq!(
        publish_cache_priced(&harness, higher.clone()).await,
        StatusCode::OK
    );
    harness.cache().set_fail_writes(false);
    assert_eq!(
        harness.cache().route(harness.model).expect("旧值还在"),
        stale,
        "失效失败了，缓存里留着的还是旧值（这正是要检出的情形）"
    );

    // 再受理一次：修订标识对不上 ⇒ 回源数据库，用新修订的定价。
    let second_key = format!("cache-route-next-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &second_key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        frozen_snapshot(&harness.pool, &second_key).await["consumer_rates_cny"],
        higher,
        "陈旧缓存必须回源，用新修订那一份定价"
    );
    assert_ne!(
        harness.cache().route(harness.model).expect("重建后的缓存"),
        stale,
        "回源之后缓存被重建成新修订那一份"
    );

    // 第二种陈旧形态：手工把值里的修订标识改旧，结果同样回源。
    let mut forged = harness.cache().route(harness.model).expect("缓存");
    forged["runtime_revision_id"] = json!(Uuid::new_v4().to_string());
    harness
        .cache()
        .put(&format!("route:{}", harness.model), &forged);
    let third_key = format!("cache-route-forged-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &third_key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        frozen_snapshot(&harness.pool, &third_key).await["consumer_rates_cny"],
        higher
    );

    harness.cleanup().await;
}

/// **陈旧缓存不得拒绝**：缓存里的余额偏低，但超出新鲜窗口（或来源是对账写回）→ 不提前拒绝，
/// 判定交给数据库，请求照常成功。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_stale_balance_entry_never_rejects() {
    let settings = CacheSettings::default().with_windows(30_000, 300_000);
    let harness = Harness::start_with_cache(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
        30,
        CacheFixture::start(settings).await,
    )
    .await;
    let client = Client::new();
    assert_eq!(
        publish_cache_priced(&harness, priced_consumer_rates()).await,
        StatusCode::OK
    );
    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let fresh = harness.cache().balance(&account_id).expect("充值之后缓存");
    let written_at = fresh["written_at"].clone();

    // ① 来源是对账写回：它只保证"与数据库一致"，不构成"刚有一笔钱变动过"的证据。
    harness
        .cache()
        .corrupt_balance(&account_id, 1, "reconciler", written_at);
    let _worker = harness.spawn_worker();
    let mut request = route_request(harness.model, "stale cache contract");
    request["size"] = json!("2K");
    request["quality"] = json!("low");
    let first_key = format!("cache-stale-reconciler-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &first_key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "对账写回的值不得用于拒绝：{body}");

    // ② 来源是写穿路径，但写入时间在窗口之外（一小时前）。
    let long_ago = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
    harness
        .cache()
        .corrupt_balance(&account_id, 1, "db_commit", json!(long_ago));
    let second_key = format!("cache-stale-old-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &second_key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "陈旧缓存不得拒绝：{body}");

    // 两次都真的扣了钱（判定交给了数据库），而且没有留下任何"凭缓存拒绝"的审计。
    assert_eq!(
        database_balance(&harness, &account_id).await,
        1_000_000 - 2 * 43_680
    );
    assert!(
        audit_events(&harness, "balance.precheck_rejected")
            .await
            .is_empty(),
        "没有发生凭缓存的拒绝"
    );

    harness.cleanup().await;
}

/// **误拒有审计**：缓存**新鲜**（来源写穿、写入时间在窗口内）且余额低于保底额 → 提前返回
/// 402，不建 Job、不扣款，同时留下一条审计（缓存余额、写入时间、来源与本次保底额）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_fresh_cache_rejection_is_audited() {
    let settings = CacheSettings::default().with_windows(30_000, 300_000);
    let harness = Harness::start_with_cache(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
        30,
        CacheFixture::start(settings).await,
    )
    .await;
    let client = Client::new();
    assert_eq!(
        publish_cache_priced(&harness, priced_consumer_rates()).await,
        StatusCode::OK
    );
    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let fresh = harness.cache().balance(&account_id).expect("充值之后缓存");
    let written_at = fresh["written_at"].clone();
    // 缓存说"不够"（比 2K 档的保底额 ¥0.25 还少），数据库说"够"——这正是要能解释清楚的那一次。
    harness
        .cache()
        .corrupt_balance(&account_id, 1, "db_commit", written_at.clone());

    let key = format!("cache-reject-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "fresh cache rejection");
    request["size"] = json!("2K");
    request["quality"] = json!("low");
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::PAYMENT_REQUIRED, "got {body}");
    assert_eq!(body["error"]["code"], json!("insufficient_balance"));

    // 拒绝没有副作用：不建 Job、不扣款。
    let jobs: i64 =
        sqlx::query_scalar("SELECT count(*) FROM generation.jobs WHERE idempotency_key = $1")
            .bind(&key)
            .fetch_one(&harness.pool)
            .await
            .expect("job count");
    assert_eq!(jobs, 0, "凭缓存拒绝不建 Job");
    assert_eq!(database_balance(&harness, &account_id).await, 1_000_000);

    // 但必须留下一条能解释"为什么拒了这个客户"的审计。
    let events = audit_events(&harness, "balance.precheck_rejected").await;
    assert_eq!(events.len(), 1, "凭缓存拒绝必须留审计");
    assert_eq!(events[0]["cached_balance_microusd"], json!(1));
    assert_eq!(events[0]["cached_source"], json!("db_commit"));
    assert_eq!(events[0]["cached_written_at"], written_at);
    assert_eq!(events[0]["hold_microusd"], json!(250_000));
    assert_eq!(events[0]["gateway_model"], json!(harness.model));

    harness.cleanup().await;
}

/// **重放不受余额预检管辖**：同一个幂等键重发会去重成原来那个 Job，不新建、不扣款，所以哪怕
/// 缓存新鲜且余额已经低于保底额，也不能凭它回 402——否则"重发同一个键"就变成看余额脸色的行为。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_replayed_request_is_never_refused_by_the_balance_precheck() {
    let settings = CacheSettings::default().with_windows(30_000, 300_000);
    let harness = Harness::start_with_cache(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
        1,
        CacheFixture::start(settings).await,
    )
    .await;
    let client = Client::new();
    assert_eq!(
        publish_cache_priced(&harness, priced_consumer_rates()).await,
        StatusCode::OK
    );
    // 余额刚好够扣一次保底额（¥0.30 ≥ ¥0.25）：受理之后余额就低于保底额了。
    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 300_000).await;

    let key = format!("cache-replay-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "replay contract");
    request["size"] = json!("2K");
    request["quality"] = json!("low");
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "got {body}");
    let cached = harness.cache().balance(&account_id).expect("受理之后缓存");
    assert_eq!(
        cached["balance_microusd"],
        json!(50_000),
        "缓存新鲜，且已经低于 2K 档的保底额"
    );

    // 同一个键立刻重发：去重成原来那个 Job，不因为缓存说"不够"而被拒。
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::GATEWAY_TIMEOUT,
        "重放不得被预检拒：{body}"
    );
    let jobs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM generation.jobs WHERE account_id = $1 AND idempotency_key = $2",
    )
    .bind(Uuid::parse_str(&account_id).expect("account id"))
    .bind(&key)
    .fetch_one(&harness.pool)
    .await
    .expect("job count");
    assert_eq!(jobs, 1, "重放去重成原来那个 Job");
    assert_eq!(
        database_balance(&harness, &account_id).await,
        50_000,
        "重放不扣款"
    );
    assert!(
        audit_events(&harness, "balance.precheck_rejected")
            .await
            .is_empty(),
        "重放没有发生凭缓存的拒绝"
    );

    harness.cleanup().await;
}

/// **定时对账兜底**：把缓存里的余额与候选集改错 → 对账以数据库为准覆盖，并留下审计。
///
/// 覆盖之后的来源标记是 `reconciler`——它**不再**能用于提前拒绝（见上一条用例的口径）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_reconciler_overwrites_corrupted_entries_from_the_database() {
    // 对账周期 1 秒、新鲜窗口 200 毫秒：用例等得起，而且满足"窗口显著小于周期"。
    let settings = CacheSettings::default().with_windows(200, 1_000);
    let harness = Harness::start_with_cache(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
        30,
        CacheFixture::start(settings).await,
    )
    .await;
    let client = Client::new();
    assert_eq!(
        publish_cache_priced(&harness, priced_consumer_rates()).await,
        StatusCode::OK
    );
    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;

    // 先受理一次把 route 缓存写起来，再把余额与候选集都改错。
    let _worker = harness.spawn_worker();
    let key = format!("cache-reconcile-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "reconcile contract");
    request["size"] = json!("2K");
    request["quality"] = json!("low");
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let settled = database_balance(&harness, &account_id).await;
    let fresh = harness.cache().balance(&account_id).expect("结算之后缓存");
    harness
        .cache()
        .corrupt_balance(&account_id, 1, "db_commit", fresh["written_at"].clone());
    let mut forged_route = harness.cache().route(harness.model).expect("route 缓存");
    forged_route["runtime_revision_id"] = json!(Uuid::new_v4().to_string());
    harness
        .cache()
        .put(&format!("route:{}", harness.model), &forged_route);

    let corrected = harness
        .cache()
        .wait_for_balance(&account_id, settled)
        .await
        .expect("定时对账必须把缓存余额覆盖回数据库的值");
    assert_eq!(
        corrected["source"],
        json!("reconciler"),
        "对账写回的值来源是 reconciler（因此不再能用于提前拒绝）"
    );
    assert_eq!(corrected["balance_microusd"], json!(settled));

    // 候选集那条：对账校正它以当前生效修订为准（这里直接把它拿掉，下一次受理回源重建）。
    for _ in 0..200 {
        if harness.cache().route(harness.model).is_none() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        harness.cache().route(harness.model).is_none(),
        "对账必须把陈旧候选集拿掉"
    );

    let balance_events = audit_events(&harness, "cache.balance_corrected").await;
    assert!(
        !balance_events.is_empty(),
        "覆盖缓存必须留审计（运营要能发现缓存被动过）"
    );
    assert_eq!(balance_events[0]["cached_balance_microusd"], json!(1));
    assert_eq!(
        balance_events[0]["database_balance_microusd"],
        json!(settled)
    );
    assert!(
        !audit_events(&harness, "cache.route_invalidated")
            .await
            .is_empty(),
        "候选集被校正也要留审计"
    );

    harness.cleanup().await;
}

/// **停用立刻生效，且不依赖那次失效**：route 缓存里留着"停用前"那份候选集（修订标识也没变，
/// 下一次受理照样命中它），停用的那条照样取不到。
///
/// 两条候选（优先级 0 的 AIHubMix、优先级 1 的 APIMart）：停用前者的**供给**之后必须落到后者；
/// 再停掉后者的**渠道**，两条就全不合格，对客是 503 平台侧故障——不是"模型不存在"（缓存里那份
/// 候选集还列着它们）。全程不跑 Worker：选路发生在调用上游之前，结论从判定记录读，零外部调用。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_disabled_offering_is_not_routed_to_while_the_route_cache_still_lists_it() {
    let harness = Harness::start_with_cache(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
        1,
        CacheFixture::start(CacheSettings::default()).await,
    )
    .await;
    let client = Client::new();
    // 地址都指向夹具那台假上游；这次不跑 Worker，所以没有任何请求真的发出去。
    let mut primary = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    primary["base_url"] = json!(harness.upstream_base_url);
    let mut secondary = candidate("APIMart", "apimart-image-v1", &["prompt_only"]);
    secondary["base_url"] = json!(harness.upstream_base_url);
    assert_eq!(
        publish_candidates(
            &client,
            &harness.base_url,
            &harness.admin_token,
            harness.model,
            None,
            vec![primary, secondary],
        )
        .await,
        StatusCode::OK,
        "两条候选的发布必须成功"
    );
    let candidates: Vec<(Uuid, i32, Uuid)> = sqlx::query(
        "SELECT re.offering_id, re.routing_priority, o.channel_id
         FROM publication.runtime_entries re
         JOIN supply.offerings o ON o.id = re.offering_id
         WHERE re.active AND re.gateway_model = $1 ORDER BY re.routing_priority ASC",
    )
    .bind(harness.model)
    .fetch_all(&harness.pool)
    .await
    .expect("这次发布的两条候选必须可读")
    .iter()
    .map(|row| {
        (
            row.try_get("offering_id").expect("offering id"),
            row.try_get("routing_priority").expect("priority"),
            row.try_get("channel_id").expect("channel id"),
        )
    })
    .collect();
    assert_eq!(candidates.len(), 2, "这次发布写入两条候选");
    let (primary_offering, secondary_offering) = (candidates[0].0, candidates[1].0);
    let secondary_channel = candidates[1].2;
    assert_ne!(
        candidates[0].2, secondary_channel,
        "两条候选必须落在两个渠道上，否则下面那一支验不到渠道的开关"
    );

    // ① 先受理一次：route 缓存因此写下"停用前"那份候选集（两条都在）。
    let first_key = format!("cache-disable-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &first_key,
        &route_request(harness.model, "before the switch"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::GATEWAY_TIMEOUT,
        "没有 Worker，受理之后只会等到超时：{body}"
    );
    assert_eq!(
        chosen_offering(&harness, &first_key).await,
        primary_offering,
        "停用之前走优先级最小的那条"
    );
    let cached = harness
        .cache()
        .route(harness.model)
        .expect("受理之后候选集必须在缓存里");
    assert_eq!(cached["candidates"].as_array().expect("候选集").len(), 2);

    // ② 停用优先级 0 那条，并让这次**失效失败**：缓存里留着的仍是"停用前"那份候选集，修订标识
    //    也没变 ⇒ 下一次受理照样命中它。停用能不能生效，因此与这次失效无关。
    harness.cache().set_fail_writes(true);
    assert_eq!(
        patch_offering(
            &client,
            &harness.base_url,
            &harness.admin_token,
            primary_offering,
            false
        )
        .await,
        StatusCode::NO_CONTENT
    );
    harness.cache().set_fail_writes(false);
    let stale = harness
        .cache()
        .route(harness.model)
        .expect("失效没成功，缓存里那份还在");
    assert_eq!(
        stale["runtime_revision_id"], cached["runtime_revision_id"],
        "启停不改变修订标识，缓存里那份仍然'看起来是新的'"
    );
    assert_eq!(
        stale["candidates"].as_array().expect("候选集").len(),
        2,
        "缓存里仍然列着刚被停用的那条"
    );

    // ③ 立刻受理：被停用的那条不进合格集合，落到另一条；判定记录写明它为什么落选。
    let second_key = format!("cache-disable-next-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &second_key,
        &route_request(harness.model, "after the switch"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::GATEWAY_TIMEOUT,
        "受理必须成功（落到另一条合格候选）：{body}"
    );
    assert_eq!(
        chosen_offering(&harness, &second_key).await,
        secondary_offering,
        "被停用的那条不得被选中"
    );
    let (second_job, _, _) = harness.job(&second_key).await;
    let considered: Value =
        sqlx::query_scalar("SELECT considered FROM generation.routing_decisions WHERE job_id = $1")
            .bind(second_job)
            .fetch_one(&harness.pool)
            .await
            .expect("受理必然写下判定记录");
    let considered = considered.as_array().expect("considered is an array");
    assert_eq!(considered.len(), 2, "两条候选都要进判定记录");
    assert_eq!(considered[0]["eligible"], json!(false));
    assert_eq!(
        considered[0]["skip_reason"],
        json!("this offering or its channel is disabled"),
        "落选原因要写明是停用"
    );
    assert_eq!(considered[1]["eligible"], json!(true));

    // ④ 停掉另一条候选的**渠道**（同样让失效失败）：判据是"供给自己启用且它的渠道也启用"，
    //    渠道这一列同样在这条复核里判。两条候选于是全不合格 —— 缓存里那份还列着它们，对客是
    //    平台侧故障 503，不是"模型不存在"。
    harness.cache().set_fail_writes(true);
    assert_eq!(
        patch_channel(
            &client,
            &harness.base_url,
            &harness.admin_token,
            secondary_channel,
            false
        )
        .await,
        StatusCode::NO_CONTENT
    );
    harness.cache().set_fail_writes(false);
    assert_eq!(
        harness
            .cache()
            .route(harness.model)
            .expect("失效没成功，缓存里那份还在")["candidates"]
            .as_array()
            .expect("候选集")
            .len(),
        2,
        "缓存里仍然列着两条候选"
    );
    let jobs_before: i64 = sqlx::query_scalar("SELECT count(*) FROM generation.jobs")
        .fetch_one(&harness.pool)
        .await
        .expect("job count");
    let third_key = format!("cache-disable-none-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &third_key,
        &route_request(harness.model, "every candidate is disabled"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "一条合格的都没有是平台侧故障：{body}"
    );
    assert_eq!(body["error"]["code"], json!("platform_unavailable"));
    let jobs_after: i64 = sqlx::query_scalar("SELECT count(*) FROM generation.jobs")
        .fetch_one(&harness.pool)
        .await
        .expect("job count");
    assert_eq!(jobs_after, jobs_before, "被拒的受理不该留下 Job");

    harness.cleanup().await;
}

/// 这次受理最终选了哪条供给（判定记录与 Job 同事务写入）。
async fn chosen_offering(harness: &Harness, key: &str) -> Uuid {
    let (job_id, _, _) = harness.job(key).await;
    sqlx::query_scalar(
        "SELECT chosen_offering_id FROM generation.routing_decisions WHERE job_id = $1",
    )
    .bind(job_id)
    .fetch_one(&harness.pool)
    .await
    .expect("受理必然写下判定记录")
}

/// **每把密钥的每分钟请求数**：窗口内超过上限 ⇒ 429 且带 `Retry-After`；到了别的窗口恢复正常。
///
/// 上限调到 1 次，第二次请求就能看到越限那条路，不必发满默认的 60 次。
///
/// 窗口边界按钟点等分，所以这条用例先**对齐到窗口开头**再发请求：贴着边界跑的话，第二次请求
/// 可能落到下一个窗口里，断言就会时好时坏（实测约 15 次里坏 1 次）。对齐之后，同一窗口内的判定
/// 是确定的——不需要为此把窗口调大或者改判据。对齐用的钟与窗口长度都与实现同一套。
///
/// "别的窗口"用**删掉这条计数**来构造，而不是等窗口过去：键里带着窗口序号，计数消失就等于到了
/// 别的窗口；删掉之后从 1 重新数起，它同时验了各窗口各算各的。
///
/// 计数落在**缓存**里，所以夹具可以直接读它、删它：这也顺带证明判定真的走了缓存。
///
/// 窗口取 60 秒而不是 10 秒：第一次请求在窗口里要等同步窗口到期（装置给的下限是 10 秒），窗口太短
/// 的话第二次请求会落到**下一个**窗口里，那时计数从 1 重新数起，越限就看不到了。窗口长度是本用例
/// 自己选的，放松它不改变要验的判定。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn requests_above_the_per_key_rate_limit_are_rejected_with_retry_after() {
    const WINDOW_MS: i64 = 60_000;
    // 离边界太近就先过去：给"两次请求 + 清理"留出足够余量，不至于刚对齐就撞上下一条边界。
    const MARGIN_MS: i64 = 30_000;

    let harness = Harness::start_with_cache_and_rate_limit(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
        1,
        CacheFixture::start(CacheSettings::default()).await,
        ApiRateLimit::once_per(WINDOW_MS as u64),
    )
    .await;
    let client = Client::new();
    assert_eq!(
        publish_cache_priced(&harness, priced_consumer_rates()).await,
        StatusCode::OK
    );
    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;

    let now_ms = chrono::Utc::now().timestamp_millis();
    let until_boundary = WINDOW_MS - now_ms.rem_euclid(WINDOW_MS);
    if until_boundary < MARGIN_MS {
        eprintln!("对齐到下一个限流窗口：还要等 {until_boundary} 毫秒");
        tokio::time::sleep(Duration::from_millis(until_boundary as u64)).await;
    }

    let first = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &format!("rate-first-{}", Uuid::new_v4()),
        &route_request(harness.model, "the first request of this window"),
    )
    .await;
    assert_eq!(first.0, StatusCode::GATEWAY_TIMEOUT, "got {}", first.1);
    // 计数落缓存：键里带着"哪把密钥 + 哪个窗口"，值是那个窗口已经数到几。
    let counter_key = {
        let state = harness.cache().state.lock().expect("cache state lock");
        let (key, value) = state
            .iter()
            .find(|(key, _)| key.starts_with("rate_limit:"))
            .map(|(key, entry)| (key.clone(), entry.value.clone()))
            .expect("速率计数必须落在缓存里");
        assert_eq!(
            serde_json::from_str::<Value>(&value).expect("cache values are JSON")["count"],
            json!(1),
            "第一次请求数到 1：{value}"
        );
        key
    };

    // 同一个窗口里的第二次：越限。对客要能把它与"并发超限"分开，并知道多久之后能再来。
    let response = client
        .post(format!("{}/v1/images/generations", harness.base_url))
        .bearer_auth(&api_key)
        .header("idempotency-key", format!("rate-second-{}", Uuid::new_v4()))
        .json(&route_request(
            harness.model,
            "the second request of this window",
        ))
        .send()
        .await
        .expect("rate limited request");
    let status = response.status();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body: Value =
        serde_json::from_str(&response.text().await.expect("rate limited response body"))
            .expect("rate limited response is JSON");
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "got {body}");
    // 与"并发超限"用不同的码：那个是"上一个还没跑完"，这个是"这一分钟发得太密"。
    assert_eq!(body["error"]["code"], json!("rate_limit_exceeded"));
    let retry_after = retry_after.expect("越限必须给出 Retry-After");
    let seconds: u64 = retry_after.parse().expect("Retry-After 是秒数");
    assert!(
        (1..=u64::try_from(WINDOW_MS / 1_000).expect("window in seconds")).contains(&seconds),
        "Retry-After 是到下一个窗口的秒数，落在 1..=窗口长度 里，实得 {seconds}"
    );

    // 别的窗口：这条计数不在，于是从 1 重新数起——请求照常受理。
    harness.cache().delete(&counter_key);
    let next = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &format!("rate-next-window-{}", Uuid::new_v4()),
        &route_request(harness.model, "the next window"),
    )
    .await;
    assert_eq!(
        next.0,
        StatusCode::GATEWAY_TIMEOUT,
        "别的窗口里照常受理（不跑 Worker，所以同步窗口超时）：{}",
        next.1
    );

    // 两次受理各占一笔预授权：只有被限流拒掉的那一次既没建 Job 也没扣款。确切数由定价决定，
    // 所以从库里读出来比对着算。
    let hold: i64 =
        sqlx::query_scalar("SELECT max_cost_microusd FROM generation.jobs WHERE account_id = $1")
            .bind(Uuid::parse_str(&account_id).expect("account id"))
            .fetch_one(&harness.pool)
            .await
            .expect("受理必然记下这次的预授权额");
    assert_eq!(
        database_balance(&harness, &account_id).await,
        1_000_000 - 2 * hold
    );
    let jobs: i64 =
        sqlx::query_scalar("SELECT count(*) FROM generation.jobs WHERE account_id = $1")
            .bind(Uuid::parse_str(&account_id).expect("account id"))
            .fetch_one(&harness.pool)
            .await
            .expect("job count");
    assert_eq!(
        jobs, 2,
        "受理了两次（本窗口一次、别的窗口一次），越限拒掉的那次没有留下 Job"
    );

    harness.cleanup().await;
}

/// **缓存不可用时限流放行**：计数已经数到上限，但缓存服务停掉之后同一个窗口里的请求照样过去。
///
/// 这条是"限流是保护不是准入"的落地：读不到计数就当作这个窗口还没数过。反过来（读不到就拒）
/// 会让加速层的一次降级把全部请求拒掉——一次降级放大成一次故障。
///
/// 判定分两层：响应**不是** 429，而且缓存里那条计数的值也没被改过——写入同样失败了，请求却
/// 照样走到了钱那一关。因此它证明的是"缓存整条不可用时限流放行"，而不是"计数恰好没读到"。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn an_unavailable_cache_lets_requests_through_instead_of_rate_limiting_them() {
    let harness = Harness::start_with_cache_and_rate_limit(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
        1,
        CacheFixture::start(CacheSettings::default()).await,
        ApiRateLimit::once_per(3_000),
    )
    .await;
    let client = Client::new();
    assert_eq!(
        publish_cache_priced(&harness, priced_consumer_rates()).await,
        StatusCode::OK
    );
    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;

    // 第一个请求把速率计数数到 1（上限也是 1），并占住一笔预授权。
    let (first, first_body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &format!("limit-down-first-{}", Uuid::new_v4()),
        &route_request(harness.model, "fills this window"),
    )
    .await;
    assert_eq!(first, StatusCode::GATEWAY_TIMEOUT, "got {first_body}");
    let hold: i64 =
        sqlx::query_scalar("SELECT max_cost_microusd FROM generation.jobs WHERE account_id = $1")
            .bind(Uuid::parse_str(&account_id).expect("account id"))
            .fetch_one(&harness.pool)
            .await
            .expect("受理必然记下这次的预授权额");

    // 记下这条计数（键 + 值），等会儿要比对"缓存不可用期间的请求没有改动它"。
    let counter = {
        let state = harness.cache().state.lock().expect("cache state lock");
        state
            .iter()
            .find(|(key, _)| key.starts_with("rate_limit:"))
            .map(|(key, entry)| (key.clone(), entry.value.clone()))
            .expect("速率计数必须落在缓存里")
    };

    harness.cache().stop();
    // 把余额压到保底额以下（而不是靠"充值数减保底额"这种算术）：这样第二个请求能走到扣款那一关，
    // 但一定扣不动——它被拒只能是钱的事，不会是限流。
    assert!(hold > 1, "保底额太小，构造不出'钱不够'的情形：{hold}");
    sqlx::query("UPDATE ledger.accounts SET balance_microusd = $2 WHERE id = $1")
        .bind(Uuid::parse_str(&account_id).expect("account id"))
        .bind(hold - 1)
        .execute(&harness.pool)
        .await
        .expect("lower the balance below the hold");

    // 同一个窗口里的第二次：计数读不出来，于是**放行**。
    let key = format!("limit-down-second-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &route_request(harness.model, "the cache is gone"),
    )
    .await;
    assert_ne!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "缓存不可用时限流必须放行：{body}"
    );
    assert_eq!(
        body["error"]["code"],
        json!("insufficient_balance"),
        "它必须真的走到扣款那一步：{body}"
    );
    let jobs: i64 =
        sqlx::query_scalar("SELECT count(*) FROM generation.jobs WHERE idempotency_key = $1")
            .bind(&key)
            .fetch_one(&harness.pool)
            .await
            .expect("job count");
    assert_eq!(
        jobs, 0,
        "余额不够的受理不建 Job，但它是被钱拒的，不是被限流拒的"
    );
    let counter_now = harness.cache().raw(&counter.0);
    assert_eq!(
        counter_now.as_deref(),
        Some(counter.1.as_str()),
        "缓存不可用期间写入也失败：计数没有被改动，请求却照样过去了"
    );

    harness.cleanup().await;
}
