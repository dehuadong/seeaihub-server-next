use super::*;

/// 承载面随 Job 冻结：发布换了承载面之后，旧 Job 读到的仍是它受理时那一份。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn carrier_surface_is_frozen_into_the_job() {
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

    let model = "frozen-carrier-model";
    let contract = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "quality": {"type": "string", "enum": ["low", "high"]}
    }));
    // 受理时这条供给能承载 `quality`。
    let accepted_carrier = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "quality": {"type": "string", "enum": ["low", "high"]}
    }));
    let status = publish_with_surfaces(
        &client,
        &base_url,
        &admin_token,
        model,
        "frozen-1",
        contract.clone(),
        vec![("aihubmix-image-v1", accepted_carrier.clone())],
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let key = format!("frozen-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &route_request(model, "frozen carrier"),
    )
    .await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "got {body}");
    let job_id: Uuid =
        sqlx::query_scalar("SELECT id FROM generation.jobs WHERE idempotency_key = $1")
            .bind(&key)
            .fetch_one(&pool)
            .await
            .expect("the request must have created a job");

    // 换一版发布：这条供给**不再**承载 `quality`（收窄了承载面）。
    let narrowed_carrier = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1}
    }));
    let status = publish_with_surfaces(
        &client,
        &base_url,
        &admin_token,
        model,
        "frozen-2",
        contract.clone(),
        vec![("aihubmix-image-v1", narrowed_carrier.clone())],
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // 走真实的读取路径取这台 Job：它读到的承载面仍是受理时那一份。
    let repository = PgHubRepository::connect(&database_url, 2)
        .await
        .expect("repository");
    let claimed = repository
        .claim_next_job("frozen-carrier-worker", chrono::Duration::seconds(30))
        .await
        .expect("claim")
        .expect("the accepted job must be claimable");
    assert_eq!(claimed.job.id.0, job_id);
    assert_eq!(
        claimed.job.offering.carrier_schema, accepted_carrier,
        "the job must keep the carrier surface it was accepted with"
    );
    assert_eq!(claimed.job.offering.capability_schema, contract);

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// 请求**用到的**字段落在合同里、但某条候选的承载面承载不了：该候选落选、换下一条。
///
/// 一条都承载不了时是**平台侧供给问题**：对客必须是平台侧故障（503），不是消费者的参数错（400）。
/// 请求本身违反合同（缺必填）仍然是 400——两者不能混成同一个码。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_field_a_carrier_cannot_carry_skips_it_and_fails_platform_side_when_none_can() {
    let (database_url, database_name) = isolated_database_url().await;
    // 没有 Worker：同步入口只会等到超时，正好用来只看"受理与选路"。
    let (base_url, admin_token, _process) = start_api(&database_url, 1, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;
    let account = create_account(&client, &base_url, &admin_token).await;
    let api_key = issue_key(&client, &base_url, &admin_token, &account).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    let model = "carry-boundary-model";
    // 合同声明了 `quality`（调用方能提交它），但只有一条供给承载得了。
    let contract = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "quality": {"type": "string", "enum": ["low", "high"]}
    }));
    let narrow = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1}
    }));
    let wide = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "quality": {"type": "string", "enum": ["low", "high"]}
    }));

    // ── 用例 1：优先级 0 的候选承载不了 → 落到优先级 1 的候选，判定记录写明原因 ──
    let status = publish_with_surfaces(
        &client,
        &base_url,
        &admin_token,
        model,
        "carry-1",
        contract.clone(),
        vec![
            ("aihubmix-image-v1", narrow.clone()),
            ("apimart-image-v1", wide.clone()),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "两条候选各带自己的承载面");

    let mut request = route_request(model, "carry this");
    request["quality"] = json!("high");
    let key = format!("carry-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::GATEWAY_TIMEOUT,
        "没有 Worker，受理后只会等到超时：{body}"
    );
    let job_id: Uuid =
        sqlx::query_scalar("SELECT id FROM generation.jobs WHERE idempotency_key = $1")
            .bind(&key)
            .fetch_one(&pool)
            .await
            .expect("the request must have been accepted");
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
    let considered = considered.as_array().expect("considered is an array");
    assert_eq!(considered.len(), 2, "两个候选都要进判定记录");
    assert_eq!(considered[0]["eligible"], false);
    assert!(
        considered[0]["skip_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("quality")),
        "落选原因必须写明承载不了哪个字段：{considered:?}"
    );
    assert_eq!(considered[1]["eligible"], true);
    let expected: Uuid = sqlx::query_scalar(
        "SELECT re.offering_id FROM publication.runtime_entries re
         WHERE re.active AND re.gateway_model = $1 AND re.routing_priority = 1",
    )
    .bind(model)
    .fetch_one(&pool)
    .await
    .expect("priority 1 offering");
    assert_eq!(
        chosen, expected,
        "第一条承载不了请求用到的字段，就该落到下一条"
    );

    // ── 用例 2：同一个型号只留承载面窄的那条 → 一条候选都不合格 ──
    let status = publish_with_surfaces(
        &client,
        &base_url,
        &admin_token,
        model,
        "carry-2",
        contract.clone(),
        vec![("aihubmix-image-v1", narrow.clone())],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let jobs_before: i64 = sqlx::query_scalar("SELECT count(*) FROM generation.jobs")
        .fetch_one(&pool)
        .await
        .expect("job count");
    let key = format!("carry-none-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_ne!(
        status,
        StatusCode::BAD_REQUEST,
        "平台承载不了不是消费者的参数错：{body}"
    );
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "平台侧供给问题必须说成平台侧故障：{body}"
    );
    assert_eq!(body["error"]["code"].as_str(), Some("platform_unavailable"));
    assert_public_only("无可用供给", &body);
    let jobs_after: i64 = sqlx::query_scalar("SELECT count(*) FROM generation.jobs")
        .fetch_one(&pool)
        .await
        .expect("job count");
    assert_eq!(
        jobs_after, jobs_before,
        "承载不了是在受理前失败的，不该留下执行记录"
    );

    // ── 用例 3：同一条供给、调用方**没用到** `quality` → 照常受理 ──
    let key = format!("carry-unused-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &route_request(model, "no quality given"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::GATEWAY_TIMEOUT,
        "没用到那个字段就该照常受理：{body}"
    );

    // ── 用例 3b：**合同里没有**的字段照旧丢掉、请求照常受理 ──
    // 与上面"合同有、承载面没有"的处置必须分开：前者丢掉不报错，后者是平台侧故障。
    // 所以判据面真的是合同，而不是这条窄承载面——合同外的字段连承载校验都进不去。
    let key = format!("carry-unknown-{}", Uuid::new_v4());
    let mut request = route_request(model, "a field the contract never declared");
    request["seed"] = json!(7);
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::GATEWAY_TIMEOUT,
        "合同外的字段该丢掉、请求照常受理：{body}"
    );
    let stored: Value = sqlx::query_scalar(
        "SELECT native_parameters FROM generation.jobs WHERE idempotency_key = $1",
    )
    .bind(&key)
    .fetch_one(&pool)
    .await
    .expect("the accepted job must keep its parameters");
    assert!(
        stored.get("seed").is_none(),
        "合同外的字段不许跟着 Job 走去上游：{stored}"
    );

    // ── 用例 4：请求本身违反合同（缺必填）仍然是 400 ──
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        "carry-missing-prompt-0001",
        &json!({"model": model, "quality": "high"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert_eq!(body["error"]["code"].as_str(), Some("validation_error"));
    assert_public_only("缺必填项", &body);

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// 映射声明的**显式默认值**必须出现在发给上游的报文里：调用方没给该字段时由平台补上，
/// 调用方给了就一个字都不改——渠道自己那套默认值（例如上游把水印默认打开）因此再也用不上。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn explicit_defaults_reach_the_upstream_request_body() {
    // 素材形状照旧（AIHubMix 声明得了 `quality`），只是这条供给挂了一份显式默认值：
    // 调用方不给 `quality` 时，平台自己发一个 `low`，而不是让上游按它的默认值走。
    let mut draft = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    draft["capability_schema"]["properties"]["quality"] =
        json!({"type": "string", "enum": ["low", "high"]});
    draft["parameter_mapping"] = json!({"defaults": {"quality": "low"}});
    let harness = Harness::start_with_draft(
        draft,
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;

    // 1) 调用方没给 `quality`：默认值跟着报文上行，也留在内部参数面里。
    let key = format!("defaults-{}", Uuid::new_v4());
    let request = route_request(harness.model, "no quality given");
    let (status, body) = harness
        .sync_json("/v1/images/generations", &key, request.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("显式默认值", &body);
    let submit_body = harness.submit_body("/v1/images/generations");
    assert_eq!(
        submit_body["quality"], "low",
        "默认值必须出现在上游报文里：{submit_body}"
    );
    harness.assert_only_declared_fields(&request);
    let stored: Value = sqlx::query_scalar(
        "SELECT native_parameters FROM generation.jobs WHERE idempotency_key = $1",
    )
    .bind(&key)
    .fetch_one(&harness.pool)
    .await
    .expect("native parameters");
    assert_eq!(
        stored["quality"], "low",
        "Job 里存的就是这次真正发出去的东西：{stored}"
    );
    assert_job_succeeded(&harness, &key).await;

    // 2) 调用方给了：用调用方的值，不被默认值覆盖。
    let key = format!("defaults-given-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "quality given");
    request["quality"] = json!("high");
    let (status, body) = harness
        .sync_json("/v1/images/generations", &key, request.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let submit_body = harness.submit_body("/v1/images/generations");
    assert_eq!(
        submit_body["quality"], "high",
        "调用方给了就用调用方的值：{submit_body}"
    );
    harness.assert_only_declared_fields(&request);
    assert_job_succeeded(&harness, &key).await;
    harness.cleanup().await;
}

/// 合同给"比例 + 档位"、供给要像素：平台在**组装期**查档案换算，线上那个字段是换算后的像素值。
///
/// 这条供给是像素面渠道（线上根本没有 `resolution` 这个名字），它承载得了这次请求全靠映射里
/// 那份尺寸声明：`resolution` 是换算的输入，不是要原样上行的字段。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_size_conversion_reaches_the_upstream_request_body() {
    let mut draft = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    // 合同（模型级）：调用方可以给比例与档位两个字段。
    draft["capability_schema"] = surface_schema(json!({
        "model": {"const": Harness::MODEL},
        "prompt": {"type": "string", "minLength": 1},
        "size": {"type": "string"},
        "resolution": {"type": "string"}
    }));
    // 承载面：这条供给只往线上写 `size` 一个尺寸字段。
    draft["carrier_schema"] = surface_schema(json!({
        "model": {"const": Harness::MODEL},
        "prompt": {"type": "string", "minLength": 1},
        "size": {"type": "string"}
    }));
    draft["parameter_mapping"] = json!({
        "size": {
            "source": ["size", "resolution"],
            "target": "size",
            "form": "pixels",
            "profile": lite_2k_profile()
        }
    });
    let harness = Harness::start_with_draft(
        draft,
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;

    let key = format!("size-converted-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "a 2:3 poster at 2K");
    request["size"] = json!("2:3");
    request["resolution"] = json!("2K");
    let (status, body) = harness
        .sync_json("/v1/images/generations", &key, request.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("尺寸换算", &body);

    let submit_body = harness.submit_body("/v1/images/generations");
    assert_eq!(
        submit_body["size"], "1664x2496",
        "线上那个字段必须是换算后的像素值：{submit_body}"
    );
    assert!(
        submit_body.get("resolution").is_none(),
        "换算的输入字段不再原样上行：{submit_body}"
    );
    harness.assert_only_declared_fields(&request);
    // 内部 Job 里存的就是这次真正发出去的东西：Driver 只看到渠道要的形态。
    let stored: Value = sqlx::query_scalar(
        "SELECT native_parameters FROM generation.jobs WHERE idempotency_key = $1",
    )
    .bind(&key)
    .fetch_one(&harness.pool)
    .await
    .expect("native parameters");
    assert_eq!(stored["size"], "1664x2496", "{stored}");
    assert!(stored.get("resolution").is_none(), "{stored}");
    assert_job_succeeded(&harness, &key).await;
    harness.cleanup().await;
}

/// 档案里缺那一格：这条候选**不合格**（原因写进判定记录），有别的候选就落到它，没有就 503。
///
/// 换算不出不是"参数错"：请求本身完全符合合同（比例与档位都给了），是这条供给的档案里没有
/// 3K 那一格。因此对客只能是平台侧故障，绝不退回调用方给的原值、也绝不猜一个近似值。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_size_combination_the_profile_lacks_makes_the_candidate_ineligible() {
    let (database_url, database_name) = isolated_database_url().await;
    // 没有 Worker：同步入口只会等到超时，正好用来只看"受理与选路"。
    let (base_url, admin_token, _process) = start_api(&database_url, 1, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;
    let account = create_account(&client, &base_url, &admin_token).await;
    let api_key = issue_key(&client, &base_url, &admin_token, &account).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    let model = "size-profile-model";
    let contract = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "size": {"type": "string"},
        "resolution": {"type": "string"}
    }));
    // 像素面供给：档案里只有 2K 那一档。
    let pixel_carrier = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "size": {"type": "string"}
    }));
    let pixel_mapping = json!({
        "size": {
            "source": ["size", "resolution"],
            "target": "size",
            "form": "pixels",
            "profile": lite_2k_profile()
        }
    });
    // 比例 + 档位面供给：两个字段原样承载，不做换算。
    let ratio_carrier = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "size": {"type": "string"},
        "resolution": {"type": "string"}
    }));

    // ── 用例 1：优先级 0 的候选档案里没有 3K → 落到优先级 1 的候选，判定记录写明原因 ──
    let status = publish_with_mappings(
        &client,
        &base_url,
        &admin_token,
        model,
        "size-1",
        contract.clone(),
        vec![
            (
                "aihubmix-image-v1",
                pixel_carrier.clone(),
                pixel_mapping.clone(),
            ),
            ("apimart-image-v1", ratio_carrier.clone(), json!({})),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "两条候选各带自己的尺寸声明");

    let mut request = route_request(model, "a 2:3 poster at 3K");
    request["size"] = json!("2:3");
    request["resolution"] = json!("3K");
    let key = format!("size-skip-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::GATEWAY_TIMEOUT,
        "换算不出只是这条候选不合格，下一条照常受理：{body}"
    );
    let job_id: Uuid =
        sqlx::query_scalar("SELECT id FROM generation.jobs WHERE idempotency_key = $1")
            .bind(&key)
            .fetch_one(&pool)
            .await
            .expect("the request must have been accepted");
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
    let considered = considered.as_array().expect("considered is an array");
    assert_eq!(considered.len(), 2, "两个候选都要进判定记录");
    assert_eq!(considered[0]["eligible"], false);
    assert!(
        considered[0]["skip_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("2:3") && reason.contains("3K")),
        "落选原因必须写明档案缺哪一格：{considered:?}"
    );
    assert_eq!(considered[1]["eligible"], true);
    let expected: Uuid = sqlx::query_scalar(
        "SELECT re.offering_id FROM publication.runtime_entries re
         WHERE re.active AND re.gateway_model = $1 AND re.routing_priority = 1",
    )
    .bind(model)
    .fetch_one(&pool)
    .await
    .expect("priority 1 offering");
    assert_eq!(chosen, expected, "第一条换算不出，就该落到下一条");

    // ── 用例 2：同一个型号只留像素面那条 → 一条候选都不合格 → 平台侧故障 ──
    let status = publish_with_mappings(
        &client,
        &base_url,
        &admin_token,
        model,
        "size-2",
        contract.clone(),
        vec![(
            "aihubmix-image-v1",
            pixel_carrier.clone(),
            pixel_mapping.clone(),
        )],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let jobs_before: i64 = sqlx::query_scalar("SELECT count(*) FROM generation.jobs")
        .fetch_one(&pool)
        .await
        .expect("job count");
    let key = format!("size-none-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_ne!(
        status,
        StatusCode::BAD_REQUEST,
        "档案缺那一格不是消费者的参数错：{body}"
    );
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "平台侧供给问题必须说成平台侧故障：{body}"
    );
    assert_eq!(body["error"]["code"].as_str(), Some("platform_unavailable"));
    assert_public_only("尺寸换算不出", &body);
    let jobs_after: i64 = sqlx::query_scalar("SELECT count(*) FROM generation.jobs")
        .fetch_one(&pool)
        .await
        .expect("job count");
    assert_eq!(
        jobs_after, jobs_before,
        "换算不出是在受理前失败的，不该留下执行记录"
    );

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// `auto` 的语义是"由模型按提示词自己决定最佳比例"：它只原样透传、永不换算。
///
/// 声明了尺寸换算的供给收不了它——那条候选落选（原因写进判定记录），有别的候选就落过去；纯透传的
/// 供给把 `auto` 原样写进 Job。一条候选都收不了时是平台侧故障，不是参数错。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_auto_size_is_passed_through_and_never_converted() {
    let (database_url, database_name) = isolated_database_url().await;
    // 没有 Worker：同步入口只会等到超时，正好用来只看"受理与选路"。
    let (base_url, admin_token, _process) = start_api(&database_url, 1, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;
    let account = create_account(&client, &base_url, &admin_token).await;
    let api_key = issue_key(&client, &base_url, &admin_token, &account).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    let model = "auto-size-model";
    let contract = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "size": {"type": "string"},
        "resolution": {"type": "string"}
    }));
    // 像素面供给：声明了尺寸换算，只收得了具体尺寸。
    let pixel_carrier = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "size": {"type": "string"}
    }));
    let pixel_mapping = json!({
        "size": {
            "source": ["size", "resolution"],
            "target": "size",
            "form": "pixels",
            "profile": lite_2k_profile()
        }
    });
    // 比例 + 档位面供给：尺寸原样承载、不做换算——`auto` 就落在这条上。
    let plain_carrier = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "size": {"type": "string"},
        "resolution": {"type": "string"}
    }));

    // ── 用例 1：换算供给收不了 `auto` → 落到纯透传的候选，`auto` 原样进 Job ──
    let status = publish_with_mappings(
        &client,
        &base_url,
        &admin_token,
        model,
        "auto-1",
        contract.clone(),
        vec![
            (
                "aihubmix-image-v1",
                pixel_carrier.clone(),
                pixel_mapping.clone(),
            ),
            ("apimart-image-v1", plain_carrier.clone(), json!({})),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "两条候选各带自己的尺寸声明");

    let mut request = route_request(model, "let the model pick the size");
    request["size"] = json!("auto");
    let key = format!("auto-skip-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::GATEWAY_TIMEOUT,
        "收不了 `auto` 只是这条候选不合格，下一条照常受理：{body}"
    );
    let job_id: Uuid =
        sqlx::query_scalar("SELECT id FROM generation.jobs WHERE idempotency_key = $1")
            .bind(&key)
            .fetch_one(&pool)
            .await
            .expect("the request must have been accepted");
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
    let considered = considered.as_array().expect("considered is an array");
    assert_eq!(considered.len(), 2, "两个候选都要进判定记录");
    assert_eq!(considered[0]["eligible"], false);
    assert!(
        considered[0]["skip_reason"].as_str().is_some_and(|reason| {
            reason.contains("auto") && reason.contains("cannot be converted")
        }),
        "落选原因必须写明 `auto` 只能原样透传、不能换算：{considered:?}"
    );
    assert_eq!(considered[1]["eligible"], true);
    let expected: Uuid = sqlx::query_scalar(
        "SELECT re.offering_id FROM publication.runtime_entries re
         WHERE re.active AND re.gateway_model = $1 AND re.routing_priority = 1",
    )
    .bind(model)
    .fetch_one(&pool)
    .await
    .expect("priority 1 offering");
    assert_eq!(
        chosen, expected,
        "换算供给收不了 `auto`，就该落到纯透传的那条"
    );
    // Job 里存的就是这次真正要发出去的东西：`auto` 原样，没有被算成一个比例。
    let stored: Value = sqlx::query_scalar(
        "SELECT native_parameters FROM generation.jobs WHERE idempotency_key = $1",
    )
    .bind(&key)
    .fetch_one(&pool)
    .await
    .expect("native parameters");
    assert_eq!(stored["size"], "auto", "`auto` 必须原样上行：{stored}");

    // ── 用例 2：只留换算供给 → 一条候选都收不了 `auto` → 平台侧故障 ──
    let status = publish_with_mappings(
        &client,
        &base_url,
        &admin_token,
        model,
        "auto-2",
        contract.clone(),
        vec![(
            "aihubmix-image-v1",
            pixel_carrier.clone(),
            pixel_mapping.clone(),
        )],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let jobs_before: i64 = sqlx::query_scalar("SELECT count(*) FROM generation.jobs")
        .fetch_one(&pool)
        .await
        .expect("job count");
    let key = format!("auto-none-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_ne!(
        status,
        StatusCode::BAD_REQUEST,
        "收不了 `auto` 不是消费者的参数错：{body}"
    );
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "平台侧供给问题必须说成平台侧故障：{body}"
    );
    assert_eq!(body["error"]["code"].as_str(), Some("platform_unavailable"));
    assert_public_only("收不了 auto", &body);
    let jobs_after: i64 = sqlx::query_scalar("SELECT count(*) FROM generation.jobs")
        .fetch_one(&pool)
        .await
        .expect("job count");
    assert_eq!(
        jobs_after, jobs_before,
        "选不出候选是在受理前失败的，不该留下执行记录"
    );

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// 没声明尺寸换算的供给（纯透传）不过换算函数：`auto` 原样出现在发给上游的报文里。
///
/// 这正是"渠道收 `auto`"的样子（承载面自己声明了那个字段）：平台原样发出去，让模型自己决定最佳
/// 比例，不替它算一个。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_auto_size_reaches_the_upstream_request_body_untouched() {
    let mut draft = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    // 合同与承载面都声明 `size`，但**没有**尺寸换算声明：尺寸原样上行。
    draft["capability_schema"] = surface_schema(json!({
        "model": {"const": Harness::MODEL},
        "prompt": {"type": "string", "minLength": 1},
        "size": {"type": "string"}
    }));
    draft["carrier_schema"] = surface_schema(json!({
        "model": {"const": Harness::MODEL},
        "prompt": {"type": "string", "minLength": 1},
        "size": {"type": "string"}
    }));
    draft["parameter_mapping"] = json!({});
    let harness = Harness::start_with_draft(
        draft,
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;

    let key = format!("auto-pass-through-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "let the model pick the size");
    request["size"] = json!("auto");
    let (status, body) = harness
        .sync_json("/v1/images/generations", &key, request.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("auto 透传", &body);

    let submit_body = harness.submit_body("/v1/images/generations");
    assert_eq!(
        submit_body["size"], "auto",
        "`auto` 必须原样发给上游：{submit_body}"
    );
    harness.assert_only_declared_fields(&request);
    let stored: Value = sqlx::query_scalar(
        "SELECT native_parameters FROM generation.jobs WHERE idempotency_key = $1",
    )
    .bind(&key)
    .fetch_one(&harness.pool)
    .await
    .expect("native parameters");
    assert_eq!(stored["size"], "auto", "{stored}");
    assert_job_succeeded(&harness, &key).await;
    harness.cleanup().await;
}

/// 改名：合同字段承载面承载不了、但改名把它落到承载面声明的名字上时，这条供给照样跑得通。
///
/// 断言两处都是**线上形态**：发给假上游的报文里是线上字段名，Job 里存的也是它——合同字段名
/// 一个都不许上行。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_renamed_field_reaches_the_upstream_under_the_wire_name() {
    // 合同（模型级）：调用方提交的是 `size`（比例）。
    // 承载面：这条供给线上叫 `resolution`（APIMart 那一侧的渠道字段名）。
    let mut draft = candidate("APIMart", "apimart-image-v1", &["prompt_only"]);
    draft["credential_env"] = json!("APIMART_API_KEY");
    draft["capability_schema"] = surface_schema(json!({
        "model": {"const": Harness::MODEL},
        "prompt": {"type": "string", "minLength": 1},
        "size": {"type": "string"}
    }));
    draft["carrier_schema"] = surface_schema(json!({
        "model": {"const": Harness::MODEL},
        "prompt": {"type": "string", "minLength": 1},
        "resolution": {"type": "string"}
    }));
    draft["parameter_mapping"] = json!({"rename": {"size": "resolution"}});
    let harness = Harness::start_with_draft(draft, None, UpstreamBehaviour::apimart(), 64).await;

    let key = format!("renamed-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "a 1:1 poster");
    request["size"] = json!("1:1");
    let (status, body) = harness
        .sync_json("/v1/images/generations", &key, request.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("改名", &body);

    let submit_body = harness.submit_body("/v1/images/generations");
    assert_eq!(
        submit_body["resolution"], "1:1",
        "线上那个字段名才是要发的东西：{submit_body}"
    );
    assert!(
        submit_body.get("size").is_none(),
        "合同字段名不许出现在报文里：{submit_body}"
    );
    harness.assert_only_declared_fields(&request);
    // Job 里存的就是这次真正发出去的东西：合同字段名在受理期就换成了线上名字。
    let stored: Value = sqlx::query_scalar(
        "SELECT native_parameters FROM generation.jobs WHERE idempotency_key = $1",
    )
    .bind(&key)
    .fetch_one(&harness.pool)
    .await
    .expect("native parameters");
    assert_eq!(stored["resolution"], "1:1", "{stored}");
    assert!(stored.get("size").is_none(), "{stored}");
    assert_job_succeeded(&harness, &key).await;
    harness.cleanup().await;
}

/// 取值映射：组装期把合同取值换成线上取值；映射表里没有这个取值 → 该候选**不合格**，原因进
/// `routing_decisions`，有别的候选就落过去、没有就 503（不是 400 参数错）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn an_enum_map_value_reaches_the_upstream_and_an_unmapped_one_skips_the_candidate() {
    let (database_url, database_name) = isolated_database_url().await;
    // 没有 Worker：同步入口只会等到超时，正好用来只看"受理与选路"。
    let (base_url, admin_token, _process) = start_api(&database_url, 1, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;
    let account = create_account(&client, &base_url, &admin_token).await;
    let api_key = issue_key(&client, &base_url, &admin_token, &account).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    let model = "enum-map-model";
    let contract = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "quality": {"type": "string", "enum": ["low", "high"]}
    }));
    let carrier = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "quality": {"type": "string"}
    }));
    // 优先级 0 的候选只把 `high` 映射成线上取值；优先级 1 的候选原样承载取值。
    let mapped = json!({"enum_map": {"quality": {"high": "xhigh"}}});

    let status = publish_with_mappings(
        &client,
        &base_url,
        &admin_token,
        model,
        "enum-1",
        contract.clone(),
        vec![
            ("aihubmix-image-v1", carrier.clone(), mapped.clone()),
            // 两条候选必须来自**两个渠道**：供给身份是"模型 + 渠道"唯一，同渠道两条会塌成一条。
            ("apimart-image-v1", carrier.clone(), json!({})),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "两条候选各带自己的映射");

    let stored_parameters = |key: &str| {
        let pool = pool.clone();
        let key = key.to_owned();
        async move {
            sqlx::query_scalar::<_, Value>(
                "SELECT native_parameters FROM generation.jobs WHERE idempotency_key = $1",
            )
            .bind(&key)
            .fetch_one(&pool)
            .await
            .expect("native parameters")
        }
    };

    // ── 用例 1：映射表里有这个取值 → 报文与 Job 里都是映射后的线上取值 ──
    let key = format!("enum-mapped-{}", Uuid::new_v4());
    let mut request = route_request(model, "a high quality poster");
    request["quality"] = json!("high");
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::GATEWAY_TIMEOUT,
        "没有 Worker，受理后只会等到超时：{body}"
    );
    let stored = stored_parameters(&key).await;
    assert_eq!(
        stored["quality"], "xhigh",
        "Job 里存的必须是映射后的线上取值：{stored}"
    );

    // ── 用例 2：映射表里没有这个取值 → 优先级 0 落选，落到优先级 1，原因写进判定记录 ──
    let key = format!("enum-skip-{}", Uuid::new_v4());
    let mut request = route_request(model, "a low quality poster");
    request["quality"] = json!("low");
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::GATEWAY_TIMEOUT,
        "映射不了只是这条候选不合格，下一条照常受理：{body}"
    );
    let job_id: Uuid =
        sqlx::query_scalar("SELECT id FROM generation.jobs WHERE idempotency_key = $1")
            .bind(&key)
            .fetch_one(&pool)
            .await
            .expect("the request must have been accepted");
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
    let considered = considered.as_array().expect("considered is an array");
    assert_eq!(considered.len(), 2, "两个候选都要进判定记录");
    assert_eq!(considered[0]["eligible"], false);
    assert!(
        considered[0]["skip_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("quality")),
        "落选原因必须写明是哪个字段的取值映射不了：{considered:?}"
    );
    assert_eq!(considered[1]["eligible"], true);
    let expected: Uuid = sqlx::query_scalar(
        "SELECT re.offering_id FROM publication.runtime_entries re
         WHERE re.active AND re.gateway_model = $1 AND re.routing_priority = 1",
    )
    .bind(model)
    .fetch_one(&pool)
    .await
    .expect("priority 1 offering");
    assert_eq!(chosen, expected, "映射不了就该落到下一条");
    let stored = stored_parameters(&key).await;
    assert_eq!(
        stored["quality"], "low",
        "落到的那条候选原样承载这个取值：{stored}"
    );

    // ── 用例 3：同一个型号只留映射表窄的那条 → 一条候选都不合格 → 平台侧故障 ──
    let status = publish_with_mappings(
        &client,
        &base_url,
        &admin_token,
        model,
        "enum-2",
        contract.clone(),
        vec![("aihubmix-image-v1", carrier.clone(), mapped.clone())],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let jobs_before: i64 = sqlx::query_scalar("SELECT count(*) FROM generation.jobs")
        .fetch_one(&pool)
        .await
        .expect("job count");
    let key = format!("enum-none-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_ne!(
        status,
        StatusCode::BAD_REQUEST,
        "映射不了不是消费者的参数错：{body}"
    );
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "平台侧供给问题必须说成平台侧故障：{body}"
    );
    assert_eq!(body["error"]["code"].as_str(), Some("platform_unavailable"));
    assert_public_only("取值映射不出", &body);
    let jobs_after: i64 = sqlx::query_scalar("SELECT count(*) FROM generation.jobs")
        .fetch_one(&pool)
        .await
        .expect("job count");
    assert_eq!(
        jobs_after, jobs_before,
        "映射不出是在受理前失败的，不该留下执行记录"
    );

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// 合同外的**图片字段**不走"合同外字段丢弃"那条：合同没为图留位置时一律 400 `invalid_parameter`。
///
/// 丢图等于悄悄生成一张没有参考图的图（还照样计费），所以既不许丢弃、也不许说成平台侧故障
/// （供给面没问题，是这个模型不接图）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn an_image_the_contract_never_declared_is_rejected_as_an_invalid_parameter() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 1, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;
    let account = create_account(&client, &base_url, &admin_token).await;
    let api_key = issue_key(&client, &base_url, &admin_token, &account).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    let model = "image-boundary-model";
    // 纯文生图：合同与承载面里都没有任何图片字段。
    let contract = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1}
    }));
    let status = publish_with_surfaces(
        &client,
        &base_url,
        &admin_token,
        model,
        "image-boundary-1",
        contract.clone(),
        vec![("aihubmix-image-v1", contract.clone())],
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let jobs_before: i64 = sqlx::query_scalar("SELECT count(*) FROM generation.jobs")
        .fetch_one(&pool)
        .await
        .expect("job count");
    let key = format!("image-boundary-{}", Uuid::new_v4());
    let mut request = route_request(model, "an image the contract never declared");
    request["image"] = json!(png_data_url());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert_eq!(
        body["error"]["code"].as_str(),
        Some("invalid_parameter"),
        "合同没声明的图片字段是参数错：{body}"
    );
    assert_public_only("合同外的图片字段", &body);
    let jobs_after: i64 = sqlx::query_scalar("SELECT count(*) FROM generation.jobs")
        .fetch_one(&pool)
        .await
        .expect("job count");
    assert_eq!(
        jobs_after, jobs_before,
        "带图请求在受理前就被拒了，不该留下执行记录"
    );

    // 同一份请求去掉图：照常受理（没有 Worker，等到超时）。
    let key = format!("image-boundary-none-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &route_request(model, "no image at all"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::GATEWAY_TIMEOUT,
        "没有图就是普通的文生图：{body}"
    );

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// 新形状的 2.5 素材（**一个 Vendor Model 一份文件**）端到端跑一遍：同一个型号只落**一份合同**，
/// 两条供给各带自己的承载面与参数映射，选路按承载面走。
///
/// 四条请求各钉一件事：
/// - 只带 `prompt`：首选的 AIHubMix 承载得了，请求就该落在它身上；
/// - 带 `background` / `output_compression` / `moderation`：承载面按**厂商契约**声明，两家都承载
///   得了这三项（聚合渠道转售上游能力，"渠道没写"不构成"渠道不能"），因此照旧落在首选上，三个
///   字段逐字上行；
/// - 带参考图：先把 AIHubMix 这条供给收窄成只允许文生图（**限制差异由测试自己构造**，不拿承载面
///   字段的有无制造差异），于是它因**分支限制**不合格（判定记录写明原因）、改道 APIMart；合同
///   字段叫 `image`，APIMart 线上叫 `image_urls`，靠改名落到渠道字段名上（报文里不许出现
///   `image`），内联图先经上传接口换成公网 URL；
/// - 带参考图 + 遮罩：同样因分支限制落到 APIMart，`image_urls` 与 `mask_url` 两个渠道名都得上线。
///
/// 两家渠道各起一个进程内假上游：线上形状不同（一家同步回图、一家任务式），所以"报文里到底是
/// 哪个字段名"只能按真正收到请求的那一方来判。全程零外部调用。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_2_5_materials_route_by_carrier_surface_and_wire_names() {
    let (database_url, database_name) = isolated_database_url().await;
    let client = Client::new();
    let aihubmix_calls: UpstreamCalls = Arc::new(Mutex::new(Vec::new()));
    let apimart_calls: UpstreamCalls = Arc::new(Mutex::new(Vec::new()));
    let aihubmix_upstream = start_fake_upstream_with(
        aihubmix_calls.clone(),
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
    )
    .await;
    let apimart_upstream =
        start_fake_upstream_with(apimart_calls.clone(), UpstreamBehaviour::apimart()).await;
    let (base_url, admin_token, _process) = start_api(&database_url, 30, 64).await;
    wait_until_ready(&client, &base_url, &admin_token).await;
    // 这条用例连发四次请求，而素材带着**测试价**（AIHubMix 的四档对客费率 = 成本单价 × 倍率
    // 1.2 × 折算率 7.1；APIMart 那条按上游声明的金额乘同一条乘法）与保底表：受理闸门是
    // "余额 ≥ 保底额"，所以要先把账户充上。
    let (_, api_key) = funded_account(&client, &base_url, &admin_token, 1_000_000).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    // ── 两份素材各自发布：一个 Vendor Model 一份文件，顶层一份合同 + 两条候选 ──
    for material in [
        include_str!("../../../../config/bootstrap/gpt-image-2.5-flare.json"),
        include_str!("../../../../config/bootstrap/gpt-image-2.5-sunburst.json"),
    ] {
        let material: Value = serde_json::from_str(material).expect("material parses");
        assert_eq!(
            material["offerings"]
                .as_array()
                .expect("offerings must be an array")
                .len(),
            2,
            "一份素材两条供给：AIHubMix 首选、APIMart 次之"
        );
        // 上游地址换成这个用例的两个假上游，各按渠道给：凭证仍只从环境变量读。
        let (material, status, body) = publish_2_5_material(
            &client,
            &base_url,
            &admin_token,
            material,
            &aihubmix_upstream.base_url,
            &apimart_upstream.base_url,
        )
        .await;
        let model = material["native_model_id"]
            .as_str()
            .expect("native model id")
            .to_owned();
        // 对客名与厂商原生名是两个角色：目录与受理用前者，合同与合同行用后者。种子素材不写
        // `gateway_model`，按发布期的回退规则取厂商原生名；自命名的例子见命名层那条用例。
        let gateway = material["gateway_model"]
            .as_str()
            .unwrap_or(&model)
            .to_owned();
        assert_eq!(status, StatusCode::OK, "{gateway} 素材必须能发布：{body}");

        // 合同是**模型级唯一一份**：这个型号只落一行，两条候选都挂在它下面。
        let contracts: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM catalog.vendor_models
             WHERE vendor_id = 'OpenAI' AND native_model_id = $1",
        )
        .bind(&model)
        .fetch_one(&pool)
        .await
        .expect("contract count");
        assert_eq!(contracts, 1, "一个 Vendor Model 只能有一份合同");
        let rows = sqlx::query(
            "SELECT re.vendor_model_id, c.provider_kind, o.carrier_schema, o.parameter_mapping,
                    vm.capability_schema
             FROM publication.runtime_entries re
             JOIN supply.offerings o ON o.id = re.offering_id
             JOIN supply.channels c ON c.id = o.channel_id
             JOIN catalog.vendor_models vm ON vm.id = re.vendor_model_id
             WHERE re.active AND re.gateway_model = $1
             ORDER BY re.routing_priority",
        )
        .bind(&gateway)
        .fetch_all(&pool)
        .await
        .expect("candidate rows");
        assert_eq!(rows.len(), 2, "两个渠道都要成为可用候选");
        let vendor_model_ids: Vec<Uuid> = rows
            .iter()
            .map(|row| row.try_get("vendor_model_id").expect("vendor model"))
            .collect();
        assert_eq!(
            vendor_model_ids[0], vendor_model_ids[1],
            "两条候选必须挂在同一份合同（同一个 Vendor Model）上"
        );
        let kinds: Vec<String> = rows
            .iter()
            .map(|row| row.try_get("provider_kind").expect("provider kind"))
            .collect();
        assert_eq!(
            kinds,
            vec!["AIHubMix".to_owned(), "APIMart".to_owned()],
            "下标 0 是首选：AIHubMix"
        );

        let carriers: Vec<Value> = rows
            .iter()
            .map(|row| row.try_get("carrier_schema").expect("carrier"))
            .collect();
        assert_ne!(
            carriers[0], carriers[1],
            "两条供给各带自己的承载面，不是共用一份"
        );
        // 承载面按**厂商契约**声明：聚合渠道转售的就是上游模型的能力，因此 AIHubMix 与 APIMart
        // 一样声明 `background` / `output_compression` / `moderation`，枚举与默认值照厂商契约。
        // 反过来说，承载面声明了就意味着平台会把字段发出去——渠道不接受是渠道报错，不是平台静默
        // 把字段吞掉。
        for name in ["background", "output_compression", "moderation"] {
            assert!(
                carriers[0]["properties"].get(name).is_some(),
                "AIHubMix 的承载面要按厂商契约声明 {name}"
            );
            assert!(
                carriers[1]["properties"].get(name).is_some(),
                "APIMart 的承载面声明了 {name}"
            );
        }
        assert_eq!(
            carriers[0]["properties"]["background"]["enum"],
            json!(["auto", "opaque", "transparent"]),
            "background 的枚举照厂商契约"
        );
        assert_eq!(carriers[0]["properties"]["background"]["default"], "auto");
        assert_eq!(
            carriers[0]["properties"]["output_compression"]["default"],
            100
        );
        assert_eq!(carriers[0]["properties"]["moderation"]["default"], "auto");
        // 参考图两边都声明成数组：AIHubMix 按厂商契约的 edit 面（`file[]`，≤16），APIMart 按
        // 自己的文档（`image_urls`，≤16）——收图上限与这个形态是同一件事，写歪了发布期就拒。
        assert_eq!(
            carriers[0]["properties"]["image"]["type"], "array",
            "AIHubMix 的参考图是数组形态"
        );
        assert_eq!(carriers[0]["properties"]["image"]["maxItems"], 16);
        assert_eq!(carriers[1]["properties"]["image_urls"]["maxItems"], 16);
        // 承载面的每个字段名都要能从合同到达：合同直接声明，或被改名接过去（供给不能凭空多出参数）。
        for (index, row) in rows.iter().enumerate() {
            let carrier: Value = row.try_get("carrier_schema").expect("carrier");
            let contract: Value = row.try_get("capability_schema").expect("contract");
            let mapping: Value = row.try_get("parameter_mapping").expect("mapping");
            let wires: Vec<Value> = mapping["rename"]
                .as_object()
                .map(|renames| renames.values().cloned().collect())
                .unwrap_or_default();
            assert_eq!(
                contract["properties"]["model"]["const"], model,
                "两条候选读到的都是这个型号的合同"
            );
            for name in carrier["properties"]
                .as_object()
                .expect("carrier properties")
                .keys()
            {
                let declared = contract["properties"]
                    .as_object()
                    .expect("contract properties")
                    .contains_key(name);
                let renamed = wires
                    .iter()
                    .any(|wire| wire.as_str() == Some(name.as_str()));
                assert!(declared || renamed, "候选 {index} 的 {name} 必须从合同可达");
            }
        }
        // APIMart 的图片字段靠**改名**接到合同字段上（合同叫 image/mask，线上叫 image_urls/mask_url）。
        let aihubmix_mapping: Value = rows[0].try_get("parameter_mapping").expect("mapping");
        assert_eq!(
            aihubmix_mapping,
            json!({}),
            "AIHubMix 与合同同型（size 都是像素型），不需要映射"
        );
        let apimart_mapping: Value = rows[1].try_get("parameter_mapping").expect("mapping");
        assert_eq!(apimart_mapping["rename"]["image"], "image_urls");
        assert_eq!(apimart_mapping["rename"]["mask"], "mask_url");

        // 对客目录：这个型号必须查得到，`contract` 逐字就是发布的那一份（只有 `model.const`
        // 按对客名替换过）——"库里发布成了"与"调用方按目录建表单建得对"是两件事，这里把后一件
        // 也钉住。目录公开，所以这里照调用方最常见的取法来：不带任何鉴权头。
        let (status, catalog) = get_catalog(&client, &base_url, None).await;
        assert_eq!(status, StatusCode::OK, "{catalog}");
        let entry = catalog["data"]
            .as_array()
            .expect("catalog data")
            .iter()
            .find(|entry| entry["name"].as_str() == Some(gateway.as_str()))
            .unwrap_or_else(|| panic!("{gateway} 必须在目录里：{catalog}"));
        assert_eq!(entry["vendor_id"].as_str(), Some("OpenAI"));
        assert_eq!(entry["revision"], material["native_revision"]);
        assert_eq!(
            entry["contract"],
            consumer_contract(material["capability_schema"].clone(), &gateway),
            "目录里的合同必须是发布的那一份，只有 `model.const` 换成对客名"
        );
        assert!(
            entry["contract"]["properties"]["model"]["const"] == gateway,
            "对客合同里的 model.const 就是调用方要提交的名字：{entry}"
        );
    }

    // 四条请求共用一个真实 Worker：它只领 Job，不知道这次用例在验什么。
    let _worker = spawn_worker_process(&database_url);

    // ── 用例 1：只带 prompt → 首选（AIHubMix）承载得了，就落在它身上 ──
    let key = format!("contract-aihubmix-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &json!({"model": "gpt-image-2.5-flare", "prompt": "雨天窗边的阅读角"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "首选供给必须跑通：{body}");
    assert_sync_success("只带 prompt 的请求", &body);
    let (chosen, considered) = routing_of(&pool, &key).await;
    assert_eq!(
        chosen_provider_kind(&pool, chosen).await,
        "AIHubMix",
        "两条候选都合格时按优先级选第一个：{considered:?}"
    );
    assert!(
        considered
            .iter()
            .all(|candidate| candidate["eligible"] == true),
        "两条候选都该合格：{considered:?}"
    );
    assert_eq!(
        count_calls(&aihubmix_calls, "POST", "/v1/images/generations"),
        1,
        "请求落在 AIHubMix，报文就该发到它的上游"
    );
    assert_eq!(
        count_calls(&apimart_calls, "POST", "/v1/images/generations"),
        0,
        "没落到 APIMart 就不该有它的生成请求"
    );

    // ── 用例 2：带 background / output_compression / moderation → 两家都承载得了，首选照旧 ──
    //
    // 承载面按**厂商契约**声明，这三项不是"APIMart 特有的差异"：AIHubMix 这条供给同样声明了
    // 它们，所以请求落在首选上，三个字段逐字上行。渠道不接受某个取值时表现为渠道报错——平台
    // 不静默丢字段、也不替调用方改值。
    let key = format!("contract-optional-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &json!({
            "model": "gpt-image-2.5-flare",
            "prompt": "白色运动鞋，透明背景",
            "background": "transparent",
            "output_compression": 80,
            "moderation": "low"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "首选供给必须跑通：{body}");
    assert_sync_success("带三个可选参数的请求", &body);
    let (chosen, considered) = routing_of(&pool, &key).await;
    assert_eq!(
        chosen_provider_kind(&pool, chosen).await,
        "AIHubMix",
        "两家都承载得了这三项，首选照旧：{considered:?}"
    );
    assert!(
        considered
            .iter()
            .all(|candidate| candidate["eligible"] == true),
        "两条候选都该合格：{considered:?}"
    );
    let submit = last_submit_body(&aihubmix_calls, "/v1/images/generations");
    assert_eq!(
        submit["background"], "transparent",
        "承载得了的字段必须原样上行：{submit}"
    );
    assert_eq!(submit["output_compression"], 80, "{submit}");
    assert_eq!(submit["moderation"], "low", "{submit}");

    // ── 收窄素材：把 AIHubMix 这条供给的 `allowed_branches` 收成只允许 `prompt_only` ──
    //
    // 落选触发用**测试自己构造的真实限制差异**，不用承载面字段的有无：两家现在按厂商契约声明
    // 同一批字段，靠字段差制造落选会把"渠道转售上游能力"验成相反的样子。收窄 `restrictions`
    // 是它的正当用法（限制只收窄、不放宽），带图请求因此真的落不到这条供给上。合同一字不改，
    // 同一个型号仍是**同一行**合同，替换的是 active 候选集。
    for material in [
        include_str!("../../../../config/bootstrap/gpt-image-2.5-flare.json"),
        include_str!("../../../../config/bootstrap/gpt-image-2.5-sunburst.json"),
    ] {
        let mut variant: Value = serde_json::from_str(material).expect("material parses");
        let model = variant["native_model_id"]
            .as_str()
            .expect("native model id")
            .to_owned();
        // 种子素材不写对客名，按发布期的回退规则取厂商原生名（见命名层那条用例的自命名例子）。
        let gateway = variant["gateway_model"]
            .as_str()
            .unwrap_or(&model)
            .to_owned();
        let mut narrowed = false;
        for offering in variant["offerings"]
            .as_array_mut()
            .expect("offerings must be an array")
        {
            if offering["provider_kind"] == "AIHubMix" {
                // 只走文生图：带图与带遮罩的请求都不该落在它身上；既然不承诺收图，上限就是 0。
                offering["restrictions"] = json!({
                    "allowed_branches": ["prompt_only"],
                    "max_images": 0
                });
                narrowed = true;
            }
        }
        assert!(narrowed, "{model} 的变体必须收窄 AIHubMix 这条供给");
        let (_, status, body) = publish_2_5_material(
            &client,
            &base_url,
            &admin_token,
            variant,
            &aihubmix_upstream.base_url,
            &apimart_upstream.base_url,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{gateway} 的变体素材必须能发布：{body}"
        );
        // 发布即原子替换该模型的 active 候选：现在生效的就是这份收窄过的声明。
        // 替换的对象是**对客名**，所以这里按对客名查。
        let restrictions: Value = sqlx::query_scalar(
            "SELECT o.restrictions FROM publication.runtime_entries re
             JOIN supply.offerings o ON o.id = re.offering_id
             JOIN supply.channels c ON c.id = o.channel_id
             WHERE re.active AND re.gateway_model = $1 AND c.provider_kind = 'AIHubMix'",
        )
        .bind(&gateway)
        .fetch_one(&pool)
        .await
        .expect("the narrowed AIHubMix entry must be active");
        assert_eq!(
            restrictions,
            json!({"allowed_branches": ["prompt_only"], "max_images": 0}),
            "{gateway} 的变体发布后，AIHubMix 这条供给只允许文生图"
        );
    }

    // ── 用例 3：带参考图 → 收窄过的 AIHubMix 因**分支限制**不合格，改道 APIMart，字段按渠道名上行 ──
    let uploads_before = count_calls(&apimart_calls, "POST", "/v1/uploads/images");
    let key = format!("contract-branch-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &json!({
            "model": "gpt-image-2.5-flare",
            "prompt": "保留商品主体，把背景换成米白色摄影棚",
            "image": [png_data_url()]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "改道之后要真的跑通：{body}");
    assert_sync_success("带参考图的请求", &body);
    let (chosen, considered) = routing_of(&pool, &key).await;
    assert_eq!(considered.len(), 2, "两个候选都要进判定记录");
    assert_eq!(
        considered[0]["eligible"], false,
        "AIHubMix 这条供给收窄成只允许文生图，带图请求不该合格：{considered:?}"
    );
    assert!(
        considered[0]["skip_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("image_conditioned")),
        "落选原因必须写明是哪条限制拦下的：{considered:?}"
    );
    assert_eq!(considered[1]["eligible"], true);
    assert_eq!(
        chosen_provider_kind(&pool, chosen).await,
        "APIMart",
        "第一条不合格就该落到下一条：{considered:?}"
    );
    // 内联图先换成渠道要的公网 URL，再按**渠道字段名**装进生成请求。
    let submit = last_submit_body(&apimart_calls, "/v1/images/generations");
    assert!(
        submit.get("image").is_none(),
        "合同字段名 `image` 不许出现在 APIMart 的报文里：{submit}"
    );
    let urls = submit["image_urls"]
        .as_array()
        .unwrap_or_else(|| panic!("APIMart 线上字段名是 image_urls 数组：{submit}"));
    assert_eq!(urls.len(), 1, "{submit}");
    assert!(
        urls[0]
            .as_str()
            .is_some_and(|url| url.starts_with("http://127.0.0.1:")),
        "上传换回来的公网 URL 才该上行：{submit}"
    );
    assert_eq!(
        count_calls(&apimart_calls, "POST", "/v1/uploads/images") - uploads_before,
        1,
        "内联参考图必须先上传换成公网 URL"
    );

    // ── 用例 4：带参考图 + 遮罩 → 遮罩分支同样被收窄掉，两个渠道字段名都上线 ──
    let key = format!("contract-masked-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &json!({
            "model": "gpt-image-2.5-sunburst",
            "prompt": "只改遮罩圈出的背景",
            "image": [png_data_url()],
            "mask": png_data_url()
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "带遮罩的请求要真的跑通：{body}");
    assert_sync_success("带参考图与遮罩的请求", &body);
    let (chosen, considered) = routing_of(&pool, &key).await;
    assert_eq!(
        chosen_provider_kind(&pool, chosen).await,
        "APIMart",
        "遮罩分支同样被收窄掉：{considered:?}"
    );
    assert!(
        considered[0]["skip_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("masked")),
        "落选原因必须写明是哪条限制拦下的：{considered:?}"
    );
    let submit = last_submit_body(&apimart_calls, "/v1/images/generations");
    assert!(
        submit.get("image").is_none() && submit.get("mask").is_none(),
        "合同字段名 `image` / `mask` 都不许出现在 APIMart 的报文里：{submit}"
    );
    assert_eq!(
        submit["image_urls"].as_array().map(Vec::len),
        Some(1),
        "{submit}"
    );
    assert!(
        submit["mask_url"].as_str().is_some(),
        "遮罩要按渠道字段名 `mask_url` 上线：{submit}"
    );
    assert_eq!(
        count_calls(&apimart_calls, "POST", "/v1/uploads/images") - uploads_before,
        3,
        "用例 3 的参考图与用例 4 的参考图、遮罩各上传一次"
    );

    pool.close().await;
    drop_isolated_database(&database_name).await;
}
