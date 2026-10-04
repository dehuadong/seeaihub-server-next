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
    // 预授权是同一次请求内的中间态：上游慢下来，占用才有可观察的窗口。
    let harness = Harness::start_with_cache(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour {
            delay_ms: 3_000,
            ..UpstreamBehaviour::aihubmix(SyncImageShape::Url)
        },
        64,
        30,
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
    assert_eq!(
        cached["held_microusd"],
        json!(0),
        "还没有任何占用：{cached}"
    );
    assert_eq!(
        cached["available_microusd"],
        json!(1_000_000),
        "可用额 = 已结算余额 − 占用：{cached}"
    );
    assert_eq!(
        cached["version"],
        json!(database_version(&harness, &account_id).await),
        "快照的版本就是数据库那一行的版本：{cached}"
    );
    assert_eq!(cached["source"], json!("db_commit"));
    assert_eq!(
        cached["balance_microusd"],
        json!(database_balance(&harness, &account_id).await),
        "缓存里的值与数据库逐位一致"
    );

    // ② 受理（预授权扣减）：这一次执行还在飞（上游慢），预授权已经占着、余额还没被结算动过。
    let key = format!("cache-hold-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "cache contract");
    request["size"] = json!("2K");
    request["quality"] = json!("low");
    let in_flight = tokio::spawn({
        let base_url = harness.base_url.clone();
        let api_key = api_key.clone();
        let key = key.clone();
        let body = request.clone();
        async move { post_json(&base_url, &api_key, "/v1/images/generations", &key, &body).await }
    });
    let mut job_id = None;
    for _ in 0..200 {
        let found: Option<Uuid> = sqlx::query_scalar(
            "SELECT j.id FROM generation.jobs j
             JOIN ledger.holds h ON h.job_id = j.id AND h.status = 'active'
             WHERE j.idempotency_key_digest = $1",
        )
        .bind(idempotency_key_digest(&key))
        .fetch_optional(&harness.pool)
        .await
        .expect("active hold lookup");
        if let Some(id) = found {
            job_id = Some(id);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let job_id = job_id.expect("受理之后必须看得到 active 的预授权");
    let cached = harness.cache().balance(&account_id).expect("受理之后缓存");
    assert_eq!(
        cached["balance_microusd"],
        json!(1_000_000),
        "受理只增加占用（记在 held），不改已结算余额"
    );
    assert_eq!(
        cached["held_microusd"],
        json!(250_000),
        "2K 档的保底额记在占用里：{cached}"
    );
    assert_eq!(
        cached["available_microusd"],
        json!(750_000),
        "可用额 = 已结算余额 − 占用：{cached}"
    );
    assert_eq!(
        cached["balance_microusd"],
        json!(database_balance(&harness, &account_id).await)
    );
    assert_eq!(
        cached["version"],
        json!(database_version(&harness, &account_id).await),
        "受理把版本推进到数据库那一行：{cached}"
    );

    // ③ 结算：这一次请求自己跑完，缓存变成实收之后的余额。
    let (status, body) = in_flight.await.expect("the in-flight request");
    assert_eq!(status, StatusCode::OK, "got {body}");
    let (_, state) = harness.job(&key).await;
    assert_eq!(state, "succeeded");
    let _ = job_id;
    let settled = database_balance(&harness, &account_id).await;
    assert_eq!(settled, 1_000_000 - 43_680, "实收按对客费率向量算");
    let cached = harness.cache().balance(&account_id).expect("结算之后缓存");
    assert_eq!(cached["balance_microusd"], json!(settled));
    assert_eq!(
        cached["held_microusd"],
        json!(0),
        "结算把占用结清：{cached}"
    );
    assert_eq!(
        cached["available_microusd"],
        json!(settled),
        "占用归零后可用额等于已结算余额：{cached}"
    );
    assert_eq!(
        cached["version"],
        json!(database_version(&harness, &account_id).await),
        "结算把版本推进到数据库那一行：{cached}"
    );
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
        let (job_id, state) = harness.job(&key).await;
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

/// **陈旧缓存不得拒绝**：缓存里的余额偏低，且超出新鲜窗口（或来源是对账写回）→ 不提示、不拒绝，
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

/// **新鲜缓存说"不够"也不得单独拒**：缓存里是一条新鲜（写穿来源、写入时间在窗口内）但可用额
/// 低于保底额的快照——例如充值的写回丢了，缓存落后于数据库。受理必须交给数据库条件更新确认：
/// 数据库说够就照常受理、照常扣费，缓存只留下一条"很可能不够"的日志。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_fresh_cache_shortfall_does_not_reject_without_the_database() {
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
    // 缓存落后于充值：新鲜、写穿来源，但可用额比 2K 档的保底额 ¥0.25 还少；数据库说够。
    harness
        .cache()
        .corrupt_balance(&account_id, 1, "db_commit", written_at);

    let key = format!("cache-short-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "fresh cache shortfall");
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
    assert_eq!(
        status,
        StatusCode::OK,
        "缓存说不够不能单独拒，必须由数据库确认：{body}"
    );
    // 判定交给了数据库：真的建了 Job、真的扣了实收，也没有"凭缓存拒绝"的痕迹。
    assert_eq!(
        database_balance(&harness, &account_id).await,
        1_000_000 - 43_680,
        "数据库确认够并照常结算"
    );
    assert!(
        audit_events(&harness, "balance.precheck_rejected")
            .await
            .is_empty(),
        "不再有凭缓存的拒绝审计"
    );

    harness.cleanup().await;
}

/// **缓存说"够"也不得单独受理**：缓存里是一条新鲜且可用额充足的快照，数据库却已经不够——
/// 402 必须由数据库条件更新确认，缓存显示的充足不能代替它（`0002` §4、`0013` §3）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_cache_that_says_there_is_enough_still_lets_the_database_refuse() {
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
    // 余额低于 2K 档的保底额 ¥0.25：数据库这一侧本来就不够。
    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 100_000).await;
    let fresh = harness.cache().balance(&account_id).expect("充值之后缓存");
    let written_at = fresh["written_at"].clone();
    // 缓存被改成一个充足、新鲜的数：它不能成为"可以受理"的依据。
    harness
        .cache()
        .corrupt_balance(&account_id, 10_000_000, "db_commit", written_at);

    let key = format!("cache-enough-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "cache says enough");
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
    assert_eq!(
        status,
        StatusCode::PAYMENT_REQUIRED,
        "数据库条件更新确认不足：{body}"
    );
    assert_eq!(body["error"]["code"], json!("insufficient_balance"));
    let jobs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM generation.jobs WHERE idempotency_key_digest = $1",
    )
    .bind(idempotency_key_digest(&key))
    .fetch_one(&harness.pool)
    .await
    .expect("job count");
    assert_eq!(jobs, 0, "被拒的受理不建 Job");
    assert_eq!(
        database_balance(&harness, &account_id).await,
        100_000,
        "余额没动"
    );

    harness.cleanup().await;
}

/// **倒序写回不得覆盖新值**：缓存里已经是一条版本更高的快照（模拟后提交的事务先写回），
/// 再发生一次版本更低的写回时，闸门必须拒绝它——否则并发提交后的异步写回会把新值盖成旧值。
///
/// 这里用充值触发写回：先把缓存伪造成版本 999 的快照，再充值（开户后版本是 0，这一笔充到 1）。写回被挡下，
/// 缓存里那条高版本快照原样留着。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn an_older_snapshot_cannot_overwrite_a_newer_cached_version() {
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
    let (account_id, _api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    // 缓存里放一条版本远高于数据库的快照：它代表"后提交的那次已经写回来了"。
    harness.cache().put_balance(
        &account_id,
        json!({
            "balance_microusd": 777,
            "held_microusd": 0,
            "available_microusd": 777,
            "version": 999,
            "written_at": chrono::Utc::now().to_rfc3339(),
            "source": "db_commit",
        }),
    );
    // 再充一笔：数据库版本只到 1，写回是"旧版本"，必须被拒绝。
    let response = client
        .post(format!(
            "{}/api/v1/accounts/{account_id}/credits",
            harness.base_url
        ))
        .bearer_auth(&harness.admin_token)
        .json(&json!({
            "amount_microusd": 500_000_u64,
            "business_key": format!("cache-order-{}", Uuid::new_v4()),
        }))
        .send()
        .await
        .expect("credit");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let cached = harness.cache().balance(&account_id).expect("缓存还在");
    assert_eq!(
        cached["version"],
        json!(999),
        "旧版本的写回被挡下，缓存里仍是那条高版本快照：{cached}"
    );
    assert_eq!(cached["balance_microusd"], json!(777));
    assert_eq!(
        database_balance(&harness, &account_id).await,
        1_500_000,
        "数据库照常记下这次充值"
    );

    harness.cleanup().await;
}

/// **重放不受余额预检影响**：预检只提示、不产生 402，同一个幂等键重发仍去重成原来那个 Job，
/// 不新建、不扣款；"重发同一个键"不因缓存说什么而改变行为。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_replayed_request_is_never_refused_by_the_balance_precheck() {
    let settings = CacheSettings::default().with_windows(30_000, 300_000);
    let harness = Harness::start_with_cache(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour {
            delay_ms: 3_000,
            ..UpstreamBehaviour::aihubmix(SyncImageShape::Url)
        },
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
    // 余额刚好够扣一次保底额（¥0.30 ≥ ¥0.25）：受理之后余额就低于保底额了。
    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 300_000).await;

    let key = format!("cache-replay-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "replay contract");
    request["size"] = json!("2K");
    request["quality"] = json!("low");
    // 第一笔在飞（上游慢）：预授权已经占着，重放要在这个窗口里发生。
    let first = tokio::spawn({
        let base_url = harness.base_url.clone();
        let api_key = api_key.clone();
        let key = key.clone();
        let body = request.clone();
        async move { post_json(&base_url, &api_key, "/v1/images/generations", &key, &body).await }
    });
    let mut in_flight = false;
    for _ in 0..200 {
        let found: Option<Uuid> = sqlx::query_scalar(
            "SELECT j.id FROM generation.jobs j
             JOIN ledger.holds h ON h.job_id = j.id AND h.status = 'active'
             WHERE j.idempotency_key_digest = $1",
        )
        .bind(idempotency_key_digest(&key))
        .fetch_optional(&harness.pool)
        .await
        .expect("active hold lookup");
        if found.is_some() {
            in_flight = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(in_flight, "第一笔必须在飞，重放才有意义");
    // 受理**不改变已结算余额**（占用记在 `held`），所以缓存里仍是 300000。
    let cached = harness.cache().balance(&account_id).expect("受理之后缓存");
    assert_eq!(
        cached["balance_microusd"],
        json!(300_000),
        "受理只增加占用，不改已结算余额"
    );
    // 把缓存改到**低于保底额**、来源仍是写穿（新鲜）：这才是"凭缓存可以提前拒绝"的形态。
    harness
        .cache()
        .corrupt_balance(&account_id, 1, "db_commit", cached["written_at"].clone());

    // 同一个键立刻重发：它命中原记录、不重新占用，所以不因为缓存说"不够"而被拒（也不是 402）。
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_ne!(
        status,
        StatusCode::PAYMENT_REQUIRED,
        "重放不得被预检拒：{body}"
    );
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "原记录还在执行，重放回 request_in_progress：{body}"
    );
    let jobs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM generation.jobs WHERE account_id = $1 AND idempotency_key_digest = $2",
    )
    .bind(Uuid::parse_str(&account_id).expect("account id"))
    .bind(idempotency_key_digest(&key))
    .fetch_one(&harness.pool)
    .await
    .expect("job count");
    assert_eq!(jobs, 1, "重放去重成原来那个 Job");
    assert_eq!(
        database_balance(&harness, &account_id).await,
        300_000,
        "重放不扣款：已结算余额没动"
    );
    assert!(
        audit_events(&harness, "balance.precheck_rejected")
            .await
            .is_empty(),
        "重放没有发生凭缓存的拒绝"
    );

    first.abort();
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

    // 跑一次受理与结算，再把缓存里的余额改错。
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
    harness.cleanup().await;
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
    assert!(
        next.0.is_server_error(),
        "别的窗口里照常受理（受理之后在上游那一步失败）：{}",
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
    // 预授权是同一次请求内的中间态：两次受理最后都收尾了，所以占用归零、余额没被预授权动过
    // （实收按对客费率向量结算，两次都失败在上游时实收为零）。被限流拒的那次连 Job 都没有。
    let _ = hold;
    assert_eq!(database_balance(&harness, &account_id).await, 1_000_000);
    let held_total: i64 =
        sqlx::query_scalar("SELECT held_microusd FROM ledger.accounts WHERE id = $1")
            .bind(Uuid::parse_str(&account_id).expect("account id"))
            .fetch_one(&harness.pool)
            .await
            .expect("account held");
    assert_eq!(held_total, 0, "在飞结束后占用应当归零");
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
    let jobs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM generation.jobs WHERE idempotency_key_digest = $1",
    )
    .bind(idempotency_key_digest(&key))
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
