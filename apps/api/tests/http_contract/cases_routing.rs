use super::*;

/// 多 Offering 路由的端到端验证。
///
/// 需要独立空库（会发布自己的候选集合）。**不启动 Worker**：路由选择发生在
/// `create_job` 之前的 API 进程内，而 `create_job` 不调用上游——因此本测试
/// **不产生任何外部调用**，同时仍能验证选中顺序与"无合格候选时零上游调用"。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn multiple_active_offerings_route_by_priority() {
    let (database_url, database_name) = isolated_database_url().await;
    // 同步入口会等到超时（没有 Worker）：给小值，别让用例白等。
    let (base_url, admin_token, _process) = start_api(&database_url, 1, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;
    let account = create_account(&client, &base_url, &admin_token).await;
    let api_key = issue_key(&client, &base_url, &admin_token, &account).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    // ── 用例 1：两个都合格的候选 → 选中优先级最小的那个 ──
    let model = "route-model-a";
    let published = publish_candidates(
        &client,
        &base_url,
        &admin_token,
        model,
        None,
        vec![
            candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
            candidate("APIMart", "apimart-image-v1", &["prompt_only"]),
        ],
    )
    .await;
    assert_eq!(
        published,
        StatusCode::OK,
        "multi-offering publish must succeed"
    );

    let key = format!("route-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &route_request(model, "route prompt"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::GATEWAY_TIMEOUT,
        "no worker runs, so the sync entry times out: {body}"
    );
    let (job_id, _, _) = {
        let row = sqlx::query("SELECT id FROM generation.jobs WHERE idempotency_key = $1")
            .bind(&key)
            .fetch_one(&pool)
            .await
            .expect("the request must have created a job");
        let id: Uuid = row.try_get("id").expect("job id");
        (id, (), ())
    };

    // 判定记录与 Job 同事务写入。
    let (chosen, considered): (Uuid, Value) = {
        let row = sqlx::query(
            "SELECT chosen_offering_id, considered FROM generation.routing_decisions WHERE job_id = $1",
        )
        .bind(job_id)
        .fetch_one(&pool)
        .await
        .expect("routing decision row must exist");
        (
            row.try_get("chosen_offering_id").expect("chosen"),
            row.try_get("considered").expect("considered"),
        )
    };
    // `considered` 记录了两个候选各自的 priority 与 eligible。
    let considered = considered.as_array().expect("considered is an array");
    assert_eq!(considered.len(), 2, "both candidates must be considered");
    assert_eq!(considered[0]["routing_priority"], 0);
    assert_eq!(considered[0]["provider_kind"], "AIHubMix");
    assert_eq!(considered[0]["eligible"], true);
    // 选中项就是优先级 0 的那个候选。
    let expected: Uuid = sqlx::query_scalar(
        "SELECT re.offering_id FROM publication.runtime_entries re
         WHERE re.active AND re.gateway_model = $1 AND re.routing_priority = 0",
    )
    .bind(model)
    .fetch_one(&pool)
    .await
    .expect("priority 0 offering");
    assert_eq!(
        chosen, expected,
        "the first eligible candidate must be chosen"
    );
    // Job 固化了被选中的 Offering 与 Channel。
    let (job_offering, job_channel): (Uuid, Uuid) = {
        let row = sqlx::query("SELECT offering_id, channel_id FROM generation.jobs WHERE id = $1")
            .bind(job_id)
            .fetch_one(&pool)
            .await
            .expect("job row");
        (
            row.try_get("offering_id").expect("offering"),
            row.try_get("channel_id").expect("channel"),
        )
    };
    assert_eq!(job_offering, chosen);
    let chosen_channel: Uuid =
        sqlx::query_scalar("SELECT channel_id FROM supply.offerings WHERE id = $1")
            .bind(chosen)
            .fetch_one(&pool)
            .await
            .expect("chosen offering channel");
    assert_eq!(job_channel, chosen_channel);

    // ── 任务创建失败时，判定记录与 Job 两边都不留下 ──
    // 余额不足在**受理前**拒绝：换一个余额低于服务端预授权额的账户来验（预授权额由服务端定，
    // 调用方自报不了，所以这里用"钱不够"而不是"自报一个很大的上限"）。
    let decisions_before: i64 =
        sqlx::query_scalar("SELECT count(*) FROM generation.routing_decisions")
            .fetch_one(&pool)
            .await
            .expect("decision count");
    let jobs_before: i64 = sqlx::query_scalar("SELECT count(*) FROM generation.jobs")
        .fetch_one(&pool)
        .await
        .expect("job count");
    let poor_account = create_account_with_credit(&client, &base_url, &admin_token, 1_000).await;
    let poor_key = issue_key(&client, &base_url, &admin_token, &poor_account).await;
    let (doomed, doomed_body) = post_json(
        &base_url,
        &poor_key,
        "/v1/images/generations",
        "doomed-request-0001",
        &route_request(model, "over budget"),
    )
    .await;
    assert_eq!(
        doomed,
        StatusCode::PAYMENT_REQUIRED,
        "an unaffordable request must be rejected before acceptance: {doomed_body}"
    );
    let decisions_after: i64 =
        sqlx::query_scalar("SELECT count(*) FROM generation.routing_decisions")
            .fetch_one(&pool)
            .await
            .expect("decision count");
    let jobs_after: i64 = sqlx::query_scalar("SELECT count(*) FROM generation.jobs")
        .fetch_one(&pool)
        .await
        .expect("job count");
    assert_eq!(
        (decisions_after, jobs_after),
        (decisions_before, jobs_before),
        "a rejected creation must leave neither a job nor a routing decision"
    );

    // ── 补充：幂等重放不重复写判定记录 ──
    // 同一个幂等键重放会返回同一条记录，判定记录也应只有一条（它反映"受理时"的判定）。
    let replay_key = format!("replay-{}", Uuid::new_v4());
    let replay_request = route_request(model, "replayed");
    for _ in 0..2 {
        let (status, body) = post_json(
            &base_url,
            &api_key,
            "/v1/images/generations",
            &replay_key,
            &replay_request,
        )
        .await;
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "got {body}");
    }
    let replay_job: Uuid =
        sqlx::query_scalar("SELECT id FROM generation.jobs WHERE idempotency_key = $1")
            .bind(&replay_key)
            .fetch_one(&pool)
            .await
            .expect("replayed job");
    let replay_decisions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM generation.routing_decisions WHERE job_id = $1")
            .bind(replay_job)
            .fetch_one(&pool)
            .await
            .expect("replay decision count");
    assert_eq!(
        replay_decisions, 1,
        "an idempotent replay must not write a second routing decision"
    );

    // ── 用例 3：再发布一次即原子替换该型号的全部 active 候选 ──
    let published = publish_candidates(
        &client,
        &base_url,
        &admin_token,
        model,
        None,
        vec![candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"])],
    )
    .await;
    assert_eq!(published, StatusCode::OK);
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM publication.runtime_entries WHERE active AND gateway_model = $1",
    )
    .bind(model)
    .fetch_one(&pool)
    .await
    .expect("active entries");
    assert_eq!(
        active, 1,
        "republishing must atomically replace the model's active candidates"
    );
    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// 权重与路由日志：**同一档内**按权重确定性分流，判定记录足以重建结论。
///
/// 需要独立空库（会发布自己的候选集合）。**不启动 Worker**：选路发生在 `create_job` 之前，
/// `create_job` 不调用上游——因此本用例**不产生任何外部调用**（同步入口等不到终态，按超时返回，
/// 而 Job 与判定记录都已经落库，正是这里要看的东西）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn routing_weight_splits_within_a_tier_and_is_replayable() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 1, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    // 账户用**固定 id**：分摊的输入里有账户，固定下来这批幂等键的分流结果就是确定的，
    // 用例因此可以逐条断言"落点等于按 (账户, 幂等键) 重算的结果"，而不是只能断言一个大概比例。
    let account_id = Uuid::from_u128(0x5eea_0000_0000_0000_0000_0000_0000_0002);
    sqlx::query("INSERT INTO ledger.accounts (id, balance_microusd) VALUES ($1, 100000000)")
        .bind(account_id)
        .execute(&pool)
        .await
        .expect("fixed account");
    let api_key = issue_key(&client, &base_url, &admin_token, &account_id.to_string()).await;

    // ── 用例 1：同一档两条候选，权重 1:3 ──
    let model = "weight-model-a";
    let mut light = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    light["routing_priority"] = json!(0);
    light["weight"] = json!(1);
    let mut heavy = candidate("APIMart", "apimart-image-v1", &["prompt_only"]);
    heavy["routing_priority"] = json!(0);
    heavy["weight"] = json!(3);
    assert_eq!(
        publish_candidates(
            &client,
            &base_url,
            &admin_token,
            model,
            None,
            vec![light, heavy]
        )
        .await,
        StatusCode::OK,
        "两条候选必须能落在同一档（旧唯一索引不允许，本片换掉了它）"
    );

    let tier = active_tier(&pool, model, 0).await;
    assert_eq!(tier.len(), 2, "同一档两条候选");
    let total: u64 = tier.iter().map(|(_, weight)| u64::from(*weight)).sum();
    assert_eq!(total, 4, "权重 1:3，合计 4");

    let mut chosen_by_key = Vec::new();
    for index in 0..16 {
        let key = format!("weight-split-{index}");
        let (status, body) = post_json(
            &base_url,
            &api_key,
            "/v1/images/generations",
            &key,
            &route_request(model, "weighted prompt"),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::GATEWAY_TIMEOUT,
            "没有 Worker，受理后只会等到超时：{body}"
        );
        let (chosen, considered) = routing_of(&pool, &key).await;
        assert_eq!(
            chosen,
            expected_weight_split(account_id, &key, &tier),
            "落点必须等于按 (账户, 幂等键) 与权重重算的结果：{considered:?}"
        );
        assert_eq!(considered.len(), 2, "两个候选都要进判定记录");
        let draw = considered[0]["weight_draw"]
            .as_u64()
            .expect("分流落点必须记下来");
        assert_eq!(
            draw,
            weight_draw_of(account_id, &key) % total,
            "分流落点必须等于哈希映射到该档权重之和以内的位置"
        );
        assert!(
            considered
                .iter()
                .all(|item| item["weight_draw"].as_u64() == Some(draw)),
            "分流落点是本次判定一个数，逐项同值：{considered:?}"
        );
        // 判定记录自己就能重建结论：档位、权重、落点都在里面，不需要再算一遍哈希。
        assert_eq!(
            rebuild_split_from_decision(&considered, draw),
            chosen,
            "判定记录必须足以重建选中项：{considered:?}"
        );
        for item in &considered {
            assert_eq!(item["routing_priority"], 0, "{item}");
            assert_eq!(item["eligible"], true, "{item}");
            assert!(
                matches!(item["weight"].as_u64(), Some(1) | Some(3)),
                "每条候选自己的权重也要进判定记录：{item}"
            );
        }
        chosen_by_key.push((key, chosen));
    }
    // 固定账户 + 固定幂等键 ⇒ 这是确定的结果，不是概率断言：权重 1:3 下两条候选都该被分到过。
    let distinct: std::collections::BTreeSet<Uuid> =
        chosen_by_key.iter().map(|(_, chosen)| *chosen).collect();
    assert_eq!(distinct.len(), 2, "权重 1:3 下两条候选都该被分到过");

    // ── 用例 2：同一幂等键重放 → 分到同一条候选，且去重成原 Job ──
    let (replay_key, chosen) = chosen_by_key[0].clone();
    let jobs_before: i64 = sqlx::query_scalar("SELECT count(*) FROM generation.jobs")
        .fetch_one(&pool)
        .await
        .expect("job count");
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &replay_key,
        &route_request(model, "weighted prompt"),
    )
    .await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "got {body}");
    let (replayed, considered) = routing_of(&pool, &replay_key).await;
    assert_eq!(replayed, chosen, "同一幂等键重放必须落同一条候选");
    assert_eq!(considered.len(), 2, "重放不新写判定记录");
    let jobs_after: i64 = sqlx::query_scalar("SELECT count(*) FROM generation.jobs")
        .fetch_one(&pool)
        .await
        .expect("job count");
    assert_eq!(
        jobs_after, jobs_before,
        "同一幂等键重放必须去重成原 Job，不新建、不重复计费"
    );

    // ── 用例 3：跨档时权重**不改变**档位顺序 ──
    let model = "weight-model-b";
    let mut preferred = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    preferred["routing_priority"] = json!(0);
    preferred["weight"] = json!(1);
    let mut fallback = candidate("APIMart", "apimart-image-v1", &["prompt_only"]);
    fallback["routing_priority"] = json!(1);
    fallback["weight"] = json!(1000);
    assert_eq!(
        publish_candidates(
            &client,
            &base_url,
            &admin_token,
            model,
            None,
            vec![preferred, fallback]
        )
        .await,
        StatusCode::OK
    );
    let first_tier = active_tier(&pool, model, 0).await;
    assert_eq!(first_tier.len(), 1, "档 0 只有一条候选");
    for index in 0..4 {
        let key = format!("weight-tier-{index}");
        let (status, body) = post_json(
            &base_url,
            &api_key,
            "/v1/images/generations",
            &key,
            &route_request(model, "tier prompt"),
        )
        .await;
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "got {body}");
        let (chosen, _) = routing_of(&pool, &key).await;
        assert_eq!(
            chosen, first_tier[0].0,
            "档 0 有合格候选时，档 1 的权重再大也轮不到"
        );
    }

    // ── 用例 4：全部档都不合格 → 503 platform_unavailable（不是参数错），且不留下 Job 与判定记录 ──
    //
    // 落选用**真实的能力差异**制造：两条候选的承载面都声明了参考图（合同因此允许这次请求），
    // 但各自的 `restrictions` 被收窄成只允许文生图——带图请求于是两条都不合格。
    // 不用"合同里没有 image"来制造落选：那会让请求在合同校验这一步就 400，验不到选路。
    let model = "weight-model-c";
    let mut narrow_first = candidate(
        "AIHubMix",
        "aihubmix-image-v1",
        &["prompt_only", "image_conditioned"],
    );
    narrow_first["routing_priority"] = json!(0);
    narrow_first["restrictions"] = json!({"allowed_branches": ["prompt_only"], "max_images": 0});
    let mut narrow_second = candidate(
        "AIHubMix",
        "aihubmix-image-v1",
        &["prompt_only", "image_conditioned"],
    );
    narrow_second["routing_priority"] = json!(1);
    narrow_second["restrictions"] = json!({"allowed_branches": ["prompt_only"], "max_images": 0});
    assert_eq!(
        publish_candidates(
            &client,
            &base_url,
            &admin_token,
            model,
            None,
            vec![narrow_first, narrow_second]
        )
        .await,
        StatusCode::OK
    );
    let mut request = route_request(model, "an edit neither candidate can carry");
    request["image"] = json!(png_data_url());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        "weight-ineligible-0001",
        &request,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "所有档都不合格是平台侧故障，不是参数错：{body}"
    );
    assert_eq!(
        body["error"]["code"].as_str(),
        Some("platform_unavailable"),
        "{body}"
    );
    let jobs: i64 =
        sqlx::query_scalar("SELECT count(*) FROM generation.jobs WHERE gateway_model = $1")
            .bind(model)
            .fetch_one(&pool)
            .await
            .expect("job count");
    let decisions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM generation.routing_decisions rd
         JOIN generation.jobs j ON j.id = rd.job_id WHERE j.gateway_model = $1",
    )
    .bind(model)
    .fetch_one(&pool)
    .await
    .expect("decision count");
    assert_eq!(
        (jobs, decisions),
        (0, 0),
        "全不合格必须在受理前失败：不建 Job、不写判定记录"
    );

    // ── 用例 5：同一档里不合格的那条**不进分摊**——权重写得再大也换不来一次选中 ──
    //
    // 与用例 4 的区别：这里只让**一条**候选不合格，另一条合格。若不合格的候选也参与分摊，
    // 权重 1000 会让它拿到几乎全部分流；断言"每次都落在合格那条"就是这条硬约束的证据。
    let model = "weight-model-d";
    let mut heavy_but_ineligible = candidate(
        "AIHubMix",
        "aihubmix-image-v1",
        &["prompt_only", "image_conditioned"],
    );
    heavy_but_ineligible["routing_priority"] = json!(0);
    heavy_but_ineligible["weight"] = json!(1000);
    heavy_but_ineligible["restrictions"] =
        json!({"allowed_branches": ["prompt_only"], "max_images": 0});
    let mut light_but_eligible = candidate(
        "AIHubMix",
        "aihubmix-image-v1",
        &["prompt_only", "image_conditioned"],
    );
    light_but_eligible["routing_priority"] = json!(0);
    light_but_eligible["weight"] = json!(1);
    assert_eq!(
        publish_candidates(
            &client,
            &base_url,
            &admin_token,
            model,
            None,
            vec![heavy_but_ineligible, light_but_eligible]
        )
        .await,
        StatusCode::OK
    );
    let mut request = route_request(model, "an edit only the light candidate can carry");
    request["image"] = json!(png_data_url());
    for index in 0..4 {
        let key = format!("weight-eligibility-{index}");
        let (status, body) = post_json(
            &base_url,
            &api_key,
            "/v1/images/generations",
            &key,
            &request,
        )
        .await;
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "got {body}");
        let (chosen, considered) = routing_of(&pool, &key).await;
        let eligible: Vec<Uuid> = considered
            .iter()
            .filter(|item| item["eligible"] == true)
            .map(|item| {
                Uuid::parse_str(item["offering_id"].as_str().expect("offering id")).expect("uuid")
            })
            .collect();
        assert_eq!(eligible.len(), 1, "只有一条候选合格：{considered:?}");
        assert_eq!(
            chosen, eligible[0],
            "不合格的候选不得因为权重大而被选中：{considered:?}"
        );
        let skipped = considered
            .iter()
            .find(|item| item["eligible"] == false)
            .expect("the heavy candidate must be recorded as ineligible");
        assert_eq!(skipped["weight"], 1000, "{skipped}");
        assert!(
            skipped["skip_reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("branch") || reason.contains("image")),
            "落选原因要写清楚：{skipped}"
        );
    }

    // ── 用例 6：权重 0 与负档位在发布期就被拒（不是库层约束错，也不是"分不到"） ──
    let mut zero_weight = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    zero_weight["weight"] = json!(0);
    let status = publish_candidates(
        &client,
        &base_url,
        &admin_token,
        "weight-model-invalid",
        None,
        vec![zero_weight],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "权重 0 必须在发布期被拒");

    let mut negative_priority = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    negative_priority["routing_priority"] = json!(-1);
    let status = publish_candidates(
        &client,
        &base_url,
        &admin_token,
        "weight-model-invalid",
        None,
        vec![negative_priority],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "负档位必须在发布期被拒");

    // ── 用例 7：管理员读接口列出权重 ──
    let response = client
        .get(format!("{base_url}/api/v1/gateway-models"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("gateway model list");
    assert_eq!(response.status(), StatusCode::OK);
    let listed: Value = response.json().await.expect("gateway model JSON");
    let entry = listed["gateway_models"]
        .as_array()
        .expect("gateway_models")
        .iter()
        .find(|entry| entry["gateway_model"].as_str() == Some("weight-model-a"))
        .expect("weight-model-a must be listed");
    let mut weights: Vec<(String, u64)> = entry["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .map(|candidate| {
            (
                candidate["provider_kind"]
                    .as_str()
                    .expect("provider kind")
                    .to_owned(),
                candidate["weight"].as_u64().expect("weight"),
            )
        })
        .collect();
    // 档内按 `offering_id` 定序，而 offering_id 每次发布都是新的：这里比的是**集合**，
    // 顺序由上面那条选路用例负责（它按同一个定序重算落点）。
    weights.sort();
    assert_eq!(
        weights,
        vec![("AIHubMix".to_owned(), 1), ("APIMart".to_owned(), 3)],
        "管理员读接口要列出每个候选的权重：{entry}"
    );

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// 路由策略的**输入面**：账户标签与两种策略要吃的表，都能经管理员 API 写入并读回。
///
/// 策略改变选路结果本身由应用层用例钉住（`least_cost_compares_discounted_estimates` /
/// `user_tag_takes_the_mapped_candidate_and_falls_back_when_it_cannot_carry`）；这里验的是它们
/// 的输入进得去、出得来，以及标签那条 401/404 的边界。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn route_policy_inputs_round_trip_through_the_admin_api() {
    let harness = Harness::start(UpstreamBehaviour::aihubmix(SyncImageShape::Url)).await;
    let client = Client::new();
    publish_bootstrap(&client, &harness.base_url, &harness.admin_token).await;
    let account_id = create_account(&client, &harness.base_url, &harness.admin_token).await;

    // 设账户标签：写库；无凭证 401；不存在的账户 404。
    let response = client
        .put(format!(
            "{}/api/v1/accounts/{account_id}/tag",
            harness.base_url
        ))
        .bearer_auth(&harness.admin_token)
        .json(&json!({"tag": "vip"}))
        .send()
        .await
        .expect("set account tag");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let stored: Option<String> =
        sqlx::query_scalar("SELECT tag FROM ledger.accounts WHERE id = $1::uuid")
            .bind(&account_id)
            .fetch_one(&harness.pool)
            .await
            .expect("account row");
    assert_eq!(stored.as_deref(), Some("vip"), "标签要真的落到账户那一行");

    let response = client
        .put(format!(
            "{}/api/v1/accounts/{account_id}/tag",
            harness.base_url
        ))
        .json(&json!({"tag": "vip"}))
        .send()
        .await
        .expect("set account tag without a token");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let unknown = Uuid::new_v4();
    let response = client
        .put(format!(
            "{}/api/v1/accounts/{unknown}/tag",
            harness.base_url
        ))
        .bearer_auth(&harness.admin_token)
        .json(&json!({"tag": "vip"}))
        .send()
        .await
        .expect("set tag on an unknown account");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // 写 `user_tag` 策略：带上"标签 → 候选"的映射，读回来要逐字一致。
    let mapped_offering = Uuid::new_v4().to_string();
    let response = client
        .put(format!("{}/api/v1/route-policies", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .json(&json!({
            "strategy": "user_tag",
            "tag_channel_map": {"vip": mapped_offering},
        }))
        .send()
        .await
        .expect("user_tag policy");
    assert_eq!(response.status(), StatusCode::OK);
    let written: Value = response.json().await.expect("policy JSON");
    assert_eq!(written["route_policy"]["strategy"], json!("user_tag"));
    assert_eq!(
        written["route_policy"]["tag_channel_map"]["vip"], mapped_offering,
        "映射要原样存回来：{written}"
    );

    // 写 `least_cost` 策略：带上折扣率表。
    let response = client
        .put(format!("{}/api/v1/route-policies", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .json(&json!({
            "gateway_model": harness.model,
            "strategy": "least_cost",
            "discount_rates": {mapped_offering.clone(): 8_000},
        }))
        .send()
        .await
        .expect("least_cost policy");
    assert_eq!(response.status(), StatusCode::OK);
    let written: Value = response.json().await.expect("policy JSON");
    assert_eq!(
        written["route_policy"]["discount_rates"][mapped_offering.as_str()],
        json!(8_000),
        "折扣率要原样存回来：{written}"
    );

    let response = client
        .get(format!("{}/api/v1/route-policies", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .send()
        .await
        .expect("policy list");
    let listed: Value = response.json().await.expect("policy JSON");
    assert_eq!(
        listed["route_policies"]
            .as_array()
            .expect("route_policies")
            .len(),
        2,
        "全局一条 + 该模型一条：{listed}"
    );

    harness.cleanup().await;
}

/// 路由策略是**运营配置**：管理员能读写、写入即刻生效，而且**不产生新修订**。
///
/// 策略真的改变选路结果由应用层用例钉住（同一个候选集合、两种策略给出可观察的差别：
/// `weighted_random_ignores_tiers_and_replays_to_the_same_candidate`）；这里验的是管理面与
/// "不进不可变修订"这条——它是"改策略不需要重发"的全部含义。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn route_policies_are_runtime_configuration_and_do_not_touch_revisions() {
    let harness = Harness::start(UpstreamBehaviour::aihubmix(SyncImageShape::Url)).await;
    let client = Client::new();
    publish_bootstrap(&client, &harness.base_url, &harness.admin_token).await;
    let revisions_before: i64 =
        sqlx::query_scalar("SELECT count(*) FROM publication.runtime_revisions")
            .fetch_one(&harness.pool)
            .await
            .expect("revision count");

    // 没配置策略时清单是空的：零配置就是"没有这一层"。
    let response = client
        .get(format!("{}/api/v1/route-policies", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .send()
        .await
        .expect("policy list");
    assert_eq!(response.status(), StatusCode::OK);
    let listed: Value = response.json().await.expect("policy JSON");
    assert_eq!(
        listed["route_policies"],
        json!([]),
        "零配置时不该有任何策略行"
    );

    // 写全局那条。
    let response = client
        .put(format!("{}/api/v1/route-policies", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .json(&json!({"strategy": "weighted_random"}))
        .send()
        .await
        .expect("global policy");
    assert_eq!(response.status(), StatusCode::OK);
    let written: Value = response.json().await.expect("policy JSON");
    assert_eq!(written["route_policy"]["gateway_model"], Value::Null);
    assert_eq!(
        written["route_policy"]["strategy"],
        json!("weighted_random")
    );
    let first_version = written["route_policy"]["version"].clone();

    // 再写同一个作用域：版本必须换新——缓存靠它判断自己是不是旧的。
    let response = client
        .put(format!("{}/api/v1/route-policies", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .json(&json!({"strategy": "weighted_random"}))
        .send()
        .await
        .expect("global policy again");
    assert_eq!(response.status(), StatusCode::OK);
    let rewritten: Value = response.json().await.expect("policy JSON");
    assert_ne!(
        rewritten["route_policy"]["version"], first_version,
        "每次写入都要换版本标识，否则缓存分辨不出改过了"
    );

    // 按模型覆盖：只影响那个网关模型。
    let response = client
        .put(format!("{}/api/v1/route-policies", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .json(&json!({"gateway_model": harness.model, "strategy": "priority_failover"}))
        .send()
        .await
        .expect("model policy");
    assert_eq!(response.status(), StatusCode::OK);

    let response = client
        .get(format!("{}/api/v1/route-policies", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .send()
        .await
        .expect("policy list");
    let listed: Value = response.json().await.expect("policy JSON");
    assert_eq!(
        listed["route_policies"]
            .as_array()
            .expect("route_policies")
            .len(),
        2,
        "全局一条 + 该模型的覆盖一条：{listed}"
    );

    // 本层没有的策略：明确拒绝，不悄悄落成默认——落成默认会把"配置没生效"伪装成生效。
    let response = client
        .put(format!("{}/api/v1/route-policies", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .json(&json!({"strategy": "cheapest_by_latency"}))
        .send()
        .await
        .expect("unsupported strategy");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // 无管理员凭证：401。
    let response = client
        .get(format!("{}/api/v1/route-policies", harness.base_url))
        .send()
        .await
        .expect("policy list without a token");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // **改策略不发修订**。
    let revisions_after: i64 =
        sqlx::query_scalar("SELECT count(*) FROM publication.runtime_revisions")
            .fetch_one(&harness.pool)
            .await
            .expect("revision count");
    assert_eq!(
        revisions_after, revisions_before,
        "策略是运行期配置：写它不该产生新修订"
    );

    harness.cleanup().await;
}
