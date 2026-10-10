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
        // 余额缓存调得很长：真走缓存的话，下面读到的就会是那个错数。
        CacheFixture::start(CacheSettings {
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
        own["balance_points"].as_i64(),
        Some(db_balance / 1_000),
        "对客面读的是库里的 {db_balance} 微单位（积分 = 微单位 / 1000），不是缓存里那个错的 7：{own}"
    );
    assert_eq!(
        own["held_points"].as_i64(),
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

/// **写穿**：充值、受理预授权扣减、结算三条路径都在数据库提交之后由后台队列把余额写进缓存。
///
/// 一次用例把三条路径都走一遍：充值后缓存在有限窗口内变成充值后的值；不跑 Worker 发一次请求
/// （同步入口超时，但 Job 已经受理、预授权已经扣），缓存跟着变成"初始 − 保底额"；再起 Worker 把
/// 同一个 Job 跑完，缓存变成结算后的余额。每一步都与数据库逐位比对。
///
/// 写穿搬进后台队列之后，**"接口返回时缓存已经是新值"不再是合同**（`0008` §7.3）。所以这里等的
/// 是"缓存版本追上数据库那一行"，而不是"返回即有"。
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
        publish_cache_priced(&harness).await,
        StatusCode::OK,
        "带定价的发布必须成功"
    );

    // ① 充值：提交后由后台队列写出去。写穿不再等在这次请求里，所以这里等它落定，而不是
    // 假设"返回即有"。
    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let cached = await_write_through(&harness, &account_id).await;
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
    let cached = await_write_through(&harness, &account_id).await;
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
    assert_eq!(
        settled,
        1_000_000 - 97_000,
        "实收按上游声明的金额加价算，落账取整到整积分"
    );
    let cached = await_write_through(&harness, &account_id).await;
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
/// **写穿不占用响应等待**：把余额写回卡在假 Redis 上，充值接口仍然立刻返回。
///
/// 这条直接验 A1 的「请求路径少两次 Redis 往返」：写回被停住时接口还能返回，就说明它没等在那里。
/// 缓存命令上限放宽到 10 秒，所以"接口等了写回"会明显超过 2 秒的上限而被抓住。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_stalled_balance_write_does_not_hold_up_the_response() {
    let harness = Harness::start_with_cache(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
        30,
        CacheFixture::start(CacheSettings {
            operation_timeout_ms: 10_000,
            ..CacheSettings::default()
        })
        .await,
    )
    .await;
    let client = Client::new();
    assert_eq!(publish_cache_priced(&harness).await, StatusCode::OK);
    let (account_id, _api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    // 先等开户与首充那两次写回落定，闸门才一定抓到下面这一笔。
    await_write_through(&harness, &account_id).await;

    harness.cache().hold_next_balance_write();
    let started = tokio::time::Instant::now();
    let response = client
        .post(format!(
            "{}/api/v1/accounts/{account_id}/credits",
            harness.base_url
        ))
        .bearer_auth(&harness.admin_token)
        .json(&json!({
            "amount_microusd": 500_000_u64,
            "business_key": format!("cache-stalled-{}", Uuid::new_v4()),
        }))
        .send()
        .await
        .expect("credit");
    let elapsed = started.elapsed();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(
        elapsed < Duration::from_secs(2),
        "充值不该等余额写回，却用了 {elapsed:?}"
    );
    // 写回确实被停住了：不然上面那句可能只是因为压根没发生写回。
    harness.cache().wait_for_balance_write_hold().await;

    harness.cleanup().await;
}

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
        assert_eq!(publish_cache_priced(&harness).await, StatusCode::OK);
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
    assert_eq!(
        with_cache.0, -97_000,
        "实收按上游声明的金额加价算，落账取整到整积分"
    );
    assert_eq!(with_cache.1, 1_000_000 - 97_000);
}

/// **陈旧缓存不得拒绝**：缓存里的余额偏低（来源是对账写回，或写穿但已经很旧）→ 判定交给数据库，
/// 请求照常成功。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_stale_balance_entry_never_rejects() {
    let settings = CacheSettings::default().with_reconcile_interval(300_000);
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
    assert_eq!(publish_cache_priced(&harness).await, StatusCode::OK);
    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let fresh = await_write_through(&harness, &account_id).await;
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

    // ② 来源是写穿路径，但写入时间是一小时前（已经陈旧）。
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
        1_000_000 - 2 * 97_000
    );
    assert!(
        audit_events(&harness, "balance.precheck_rejected")
            .await
            .is_empty(),
        "没有发生凭缓存的拒绝"
    );

    harness.cleanup().await;
}

/// **缓存说"不够"也不得单独拒**：缓存里是一条刚写穿但可用额低于保底额的快照——例如充值的写回
/// 丢了，缓存落后于数据库。受理必须交给数据库条件更新确认：
/// 数据库说够就照常受理、照常扣费，缓存说什么都不决定结果。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_fresh_cache_shortfall_does_not_reject_without_the_database() {
    let settings = CacheSettings::default().with_reconcile_interval(300_000);
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
    assert_eq!(publish_cache_priced(&harness).await, StatusCode::OK);
    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let fresh = await_write_through(&harness, &account_id).await;
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
        1_000_000 - 97_000,
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
    let settings = CacheSettings::default().with_reconcile_interval(300_000);
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
    assert_eq!(publish_cache_priced(&harness).await, StatusCode::OK);
    // 余额低于 2K 档的保底额 ¥0.25：数据库这一侧本来就不够。
    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 100_000).await;
    let fresh = await_write_through(&harness, &account_id).await;
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
    assert_eq!(publish_cache_priced(&harness).await, StatusCode::OK);
    let (account_id, _api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    // 先等开户与首充那两次写回落定。不等就伪造缓存，晚到的那次写回会把伪造的版本覆盖掉，
    // 后面这笔充值的版本反而比缓存里的高——闸门会放行，用例随机失败。
    await_write_through(&harness, &account_id).await;
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
    //
    // 断言前要等到这次写回**处理完**，而不只是"命令到达"：版本判断发生在假 Redis 回完 `GET`
    // 之后，只等到达就断言的话，闸门即使坏了也来得及通过。
    //
    // 判据是**下一条余额命令的到达**：写回队列单线程顺序处理，第 2 笔的 `GET` 只可能在第 1 笔的
    // `write_balance` 返回之后发出。同账户两笔不会互相合并——第 1 笔到达时队列已经把它取走了，
    // 第 2 笔进的是下一轮。
    let arrivals = harness.cache().balance_write_arrivals();
    for round in 1..=2 {
        let response = client
            .post(format!(
                "{}/api/v1/accounts/{account_id}/credits",
                harness.base_url
            ))
            .bearer_auth(&harness.admin_token)
            .json(&json!({
                "amount_microusd": 500_000_u64,
                "business_key": format!("cache-order-{round}-{}", Uuid::new_v4()),
            }))
            .send()
            .await
            .expect("credit");
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        // 第 1 笔到达即证明队列已经取走它；第 2 笔到达即证明第 1 笔已经处理完。
        harness
            .cache()
            .wait_for_balance_write_arrivals(arrivals + round)
            .await;
    }
    let cached = harness.cache().balance(&account_id).expect("缓存还在");
    assert_eq!(
        cached["version"],
        json!(999),
        "旧版本的写回被挡下，缓存里仍是那条高版本快照：{cached}"
    );
    assert_eq!(cached["balance_microusd"], json!(777));
    assert_eq!(
        database_balance(&harness, &account_id).await,
        2_000_000,
        "数据库照常记下这两笔充值"
    );

    harness.cleanup().await;
}

/// **重放不受缓存余额影响**：缓存里说不够也不产生 402，同一个幂等键重发仍去重成原来那个 Job，
/// 不新建、不扣款；"重发同一个键"不因缓存说什么而改变行为。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_replayed_request_is_never_refused_by_the_cached_balance() {
    let settings = CacheSettings::default().with_reconcile_interval(300_000);
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
    assert_eq!(publish_cache_priced(&harness).await, StatusCode::OK);
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
    let cached = await_write_through(&harness, &account_id).await;
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

/// **定时对账兜底**：把缓存里的余额改错 → 对账以数据库为准覆盖，并留下审计。
///
/// 覆盖之后的来源标记是 `reconciler`，与写穿路径写下的值分得开。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_reconciler_overwrites_corrupted_entries_from_the_database() {
    // 对账周期 1 秒：用例等得起。
    let settings = CacheSettings::default().with_reconcile_interval(1_000);
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
    assert_eq!(publish_cache_priced(&harness).await, StatusCode::OK);
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
    let fresh = await_write_through(&harness, &account_id).await;
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
        "对账写回的值来源是 reconciler"
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

/// **平台不按 API Key 的请求速率拒绝**（Spec 0005 A14、Spec 0004 A11）：同一把密钥连发 **61** 次
/// 生成请求（原来的每分钟上限是 60）全部被受理，没有一次 `rate_limit_exceeded`。
///
/// 61 次分四波并发发出（每波 16 条，低于渠道全局未决上限），整段跑在同一个 60 秒窗口里；串行
/// 发完要等每条的同步窗口，会把它们分散到窗口之外，"同一窗口"这个前提就不成立了。
///
/// 它同时证明缓存里不再有每密钥的速率计数：那一层已经没有地方可落。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn many_requests_in_one_window_are_all_accepted() {
    const REQUESTS: usize = 61;
    const WAVE: usize = 16;

    // 本机执行名额与渠道未决上限都抬到 64：这条用例要验的是"没有按请求速率的拒绝"，
    // 不该被容量闸门挡住（那两者回 503，与本用例的判据无关）。
    let harness = Harness::build(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
        1,
        CaseSettings {
            api: ApiProcessSettings {
                cache: Some(CacheFixture::start(CacheSettings::default()).await),
                // 本机执行名额按"内存预算 ÷ 单次预留"推导：给足预算，并发 16 就不会撞容量。
                max_memory_bytes: Some(64 * 1024 * 1024 * 1024),
                channel_max_in_flight: Some(64),
                ..ApiProcessSettings::default()
            },
            markup_bps: None,
        },
    )
    .await;
    let client = Client::new();
    assert_eq!(publish_cache_priced(&harness).await, StatusCode::OK);
    let (account_id, api_key) = funded_account(
        &client,
        &harness.base_url,
        &harness.admin_token,
        200_000_000,
    )
    .await;

    let started = std::time::Instant::now();
    let mut sent = 0;
    while sent < REQUESTS {
        let mut handles = Vec::new();
        for index in sent..(sent + WAVE).min(REQUESTS) {
            let base_url = harness.base_url.clone();
            let api_key = api_key.clone();
            let key = format!("no-rate-limit-{index}-{}", Uuid::new_v4());
            let request = route_request(harness.model, &format!("burst {index}"));
            handles.push(tokio::spawn(async move {
                post_json(
                    &base_url,
                    &api_key,
                    "/v1/images/generations",
                    &key,
                    &request,
                )
                .await
            }));
        }
        for (offset, handle) in handles.into_iter().enumerate() {
            let (status, body) = handle.await.expect("a burst request");
            assert_ne!(
                status,
                StatusCode::TOO_MANY_REQUESTS,
                "第 {} 次被拒了：{body}",
                sent + offset
            );
            assert_ne!(
                body["error"]["code"],
                json!("rate_limit_exceeded"),
                "不该再有按请求速率的拒绝：{body}"
            );
            // 受理之后在上游那一步失败是预期的；容量与余额那两种说明根本没被受理。
            assert_ne!(
                body["error"]["code"],
                json!("platform_unavailable"),
                "第 {} 次没被受理（容量）：{status} {body}",
                sent + offset
            );
            assert_ne!(
                body["error"]["code"],
                json!("insufficient_balance"),
                "第 {} 次没被受理（余额）：{status} {body}",
                sent + offset
            );
        }
        sent += WAVE;
    }

    // 每一次都被受理、每一次留下一个 Job。
    let jobs: i64 =
        sqlx::query_scalar("SELECT count(*) FROM generation.jobs WHERE account_id = $1")
            .bind(Uuid::parse_str(&account_id).expect("account id"))
            .fetch_one(&harness.pool)
            .await
            .expect("job count");
    assert_eq!(jobs, REQUESTS as i64, "61 次都要被受理");

    // 61 次要落在同一个 60 秒窗口里：跨窗口的话"每窗口 60 次"这种限流也能放过它们。
    assert!(
        started.elapsed() < std::time::Duration::from_secs(60),
        "这批请求要在同一个 60 秒窗口里跑完，实际用了 {:?}",
        started.elapsed()
    );

    // 缓存里没有按密钥的速率计数：公开鉴权端点的失败尝试键（`rate_limit:auth:…`）不算。
    let counters = {
        let state = harness.cache().state.lock().expect("cache state lock");
        state
            .keys()
            .filter(|key| key.starts_with("rate_limit:") && !key.starts_with("rate_limit:auth:"))
            .count()
    };
    assert_eq!(counters, 0, "缓存里不再有按密钥的速率计数");

    harness.cleanup().await;
}
