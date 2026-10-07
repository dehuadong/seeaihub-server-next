use super::*;

/// 参数面以**选中候选的声明面**为准：声明过的参数原样上行，没声明的一律在上游请求体里消失。
///
/// 三条一起钉：
/// - 候选声明了 `quality`：调用方给的 `"high"` 逐字出现在发给假上游的请求体里；
/// - 候选没声明 `image_with_roles`（渠道文档里的一手参数）、`seed`、`foo`：请求照常 200、Job 照常
///   跑到成功，但这三个名字在发给假上游的报文里**一个字都没有**；
/// - 必填项在场照旧：同一份候选下发一个缺 `prompt` 的请求仍然是 400。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn declared_parameters_go_upstream_and_undeclared_ones_never_leave_the_platform() {
    // 用真实素材的声明面（AIHubMix 声明了 `quality`），只把上游地址换成假上游。
    let harness =
        Harness::start_with_bootstrap(UpstreamBehaviour::aihubmix(SyncImageShape::Url), 64).await;
    let key = format!("driver-declared-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "declared parameters only");
    request["quality"] = json!("high");
    // 三个都没被这份候选声明：一手图片参数、平台没声明的普通参数、纯属多余的字段。
    request["image_with_roles"] = json!([{
        "role": "reference",
        "url": format!("{}/inputs/roles.png", harness.upstream_base_url)
    }]);
    request["seed"] = json!(7);
    request["foo"] = json!({"a": 1});
    let (status, body) = harness
        .sync_json("/v1/images/generations", &key, request.clone())
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "多带一个没声明的参数不该让整次请求失败：{body}"
    );
    assert_sync_success("声明面过滤", &body);
    let submit_body = harness.submit_body("/v1/images/generations");
    // 模型专属字段在这条渠道上进 extra 容器（承载面就这么声明，映射把合同字段指进去）。
    assert_eq!(
        submit_body["extra"]["quality"], "high",
        "声明过的参数原样上行：{submit_body}"
    );
    for dropped in ["image_with_roles", "seed", "foo"] {
        assert!(
            submit_body.get(dropped).is_none(),
            "候选没声明的 `{dropped}` 绝不能出现在上游请求体里：{submit_body}"
        );
    }
    // 它不是平台装载的参考图：平台不去取它。
    assert_eq!(
        harness.count("GET", "/inputs/roles.png"),
        0,
        "平台不把没声明的参数当参考图去取"
    );
    // 平台自己产生的字段仍在声明面内，且调用方带的未声明名字一个都没上行。
    harness.assert_only_declared_fields(&request);
    assert_job_succeeded(&harness, &key).await;

    // 必填项在场照旧：`prompt` 缺了就是 400（丢参数不等于不要必填）。
    let key = format!("driver-declared-missing-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &key,
        &json!({"model": harness.model, "quality": "high", "seed": 7}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert_eq!(body["error"]["code"].as_str(), Some("validation_error"));
    assert_public_only("缺必填项", &body);
    harness.cleanup().await;
}

/// 真素材的声明面同时钉住另一件容易漏的事：**平台装载的图片参数不会被过滤掉**。
///
/// APIMart 的 Profile 把参考图声明成 `image_urls`（数组），受理期先按声明面过滤、再把调用方给的图
/// 落到这个名字上。过滤若按"调用方原来带了什么"来做，这次请求就没有 `image_urls` 可用——图会
/// 静默丢掉。这里断言它照旧出现在发给假上游的请求体里，且没声明的名字一个都不留。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn filtering_never_drops_the_images_the_platform_places() {
    let harness = Harness::start_with_bootstrap(UpstreamBehaviour::apimart(), 64).await;
    let key = format!("driver-placed-images-{}", Uuid::new_v4());
    let reference_url = format!("{}/inputs/ref.png", harness.upstream_base_url);
    let mut request = route_request(harness.model, "the placed image survives the filter");
    request["image_urls"] = json!([reference_url.clone()]);
    request["image_with_roles"] =
        json!([{"role": "reference", "url": "https://example.invalid/other.png"}]);
    request["seed"] = json!(7);
    let (status, body) = harness
        .sync_json("/v1/images/generations", &key, request.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("平台装载的图不被过滤", &body);
    let submit_body = harness.submit_body("/v1/images/generations");
    assert_eq!(
        submit_body["image_urls"],
        json!([reference_url]),
        "装载进来的参考图必须落在候选声明的名字上：{submit_body}"
    );
    for dropped in ["image_with_roles", "seed"] {
        assert!(
            submit_body.get(dropped).is_none(),
            "候选没声明的 `{dropped}` 绝不能出现在上游请求体里：{submit_body}"
        );
    }
    harness.assert_only_declared_fields(&request);
    assert_job_succeeded(&harness, &key).await;
    harness.cleanup().await;
}

/// 编辑路径（`/v1/images/edits`，与 `generations` 同一套 JSON 解码）走同一套参数过滤：
/// 声明过的参数原样到上游，没声明的到此为止。
///
/// AIHubMix 的编辑请求与文生图走同一个端点、同一个 JSON 请求体：模型专属字段落 `extra` 容器、
/// 参考图落 `images`。这里断言两个方向——声明过的 `quality` 进了发给假上游的请求体，而 `seed` 与
/// `image_with_roles` 在整份请求体里都不出现。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_edit_path_keeps_declared_parameters_and_drops_undeclared_ones() {
    let harness =
        Harness::start_with_bootstrap(UpstreamBehaviour::aihubmix(SyncImageShape::Base64), 64)
            .await;
    let key = format!("edits-declared-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "an edit with a declared parameter");
    request["quality"] = json!("high");
    request["seed"] = json!(7);
    request["image_with_roles"] =
        json!([{"role": "reference", "url": "https://example.invalid/a.png"}]);
    request["image"] = json!(harness.png_url());
    let (status, body) = harness.sync_json("/v1/images/edits", &key, request).await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("编辑路径的声明面过滤", &body);
    let submitted = harness.submit_body("/ai/v1/images/generations");
    assert_eq!(
        submitted["extra"]["quality"], "high",
        "声明过的 `quality` 必须进请求体：{submitted}"
    );
    for dropped in ["seed", "image_with_roles"] {
        assert!(
            submitted.get(dropped).is_none() && submitted["extra"].get(dropped).is_none(),
            "候选没声明的 `{dropped}` 绝不能出现在发给上游的请求体里：{submitted}"
        );
    }
    assert_eq!(
        submitted["images"],
        json!([harness.png_url()]),
        "平台装载的参考图走 `images`：{submitted}"
    );
    assert_job_succeeded(&harness, &key).await;
    harness.cleanup().await;
}

/// 两个图片入口共用一套解码：同一个 JSON 请求发给哪个路径，行为都一致（Spec 0005 §1）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn both_image_entries_accept_the_same_json_request() {
    let harness = Harness::start(UpstreamBehaviour::aihubmix(SyncImageShape::Base64)).await;

    // 1) 参考图走 `image_urls`：两个路径都出图。
    for path in ["/v1/images/generations", "/v1/images/edits"] {
        let key = format!("edits-text-{}", Uuid::new_v4());
        let mut request = route_request(harness.model, "edit through a text field");
        request["image_urls"] = json!([harness.png_url()]);
        let (status, body) = harness.sync_json(path, &key, request).await;
        assert_eq!(status, StatusCode::OK, "{path}: got {body}");
        assert_sync_success("文本部件参考图", &body);
    }

    // 2) `image` 与 `image_urls` 同时给非空值：同义字段含糊，受理前 400，两个路径同判。
    for path in ["/v1/images/generations", "/v1/images/edits"] {
        let key = format!("edits-text-conflict-{}", Uuid::new_v4());
        let mut request = route_request(harness.model, "both synonyms as text fields");
        request["image"] = json!("https://example.invalid/a.png");
        request["image_urls"] = json!(["https://example.invalid/b.png"]);
        let (status, body) = harness.sync_json(path, &key, request).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}: got {body}");
        assert_eq!(body["error"]["code"].as_str(), Some("invalid_parameter"));
        assert_public_only("文本部件的同义字段冲突", &body);
    }
    harness.cleanup().await;
}

/// 任务**查询**的瞬时失败可以重试，Job 最终仍成功。
///
/// 查询是幂等读，重试它不会造成重复副作用；这与"创建请求绝不重发"并不冲突。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn transient_query_failure_is_retried_and_the_job_still_succeeds() {
    let behaviour = UpstreamBehaviour {
        query_failures: 1,
        ..UpstreamBehaviour::apimart()
    };
    let outcome = run_driver_attempt(behaviour).await;
    assert_eq!(
        outcome.job_state, "succeeded",
        "a transient query failure must be retried, not turned into a job failure"
    );
    assert_eq!(
        outcome.submits, 1,
        "the create request must never be resent"
    );
    assert!(
        outcome.polls >= 2,
        "expected at least two queries (one failure + one success), got {}",
        outcome.polls
    );
    outcome.harness.cleanup().await;
}

/// 未在文档中出现的状态值必须**继续轮询**，不得当失败。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn unknown_task_status_keeps_polling_instead_of_failing() {
    let behaviour = UpstreamBehaviour {
        unknown_status_times: 1,
        ..UpstreamBehaviour::apimart()
    };
    let outcome = run_driver_attempt(behaviour).await;
    assert_eq!(
        outcome.job_state, "succeeded",
        "an undocumented status must not be treated as failure"
    );
    assert!(
        outcome.polls >= 2,
        "the driver must keep polling after an unknown status, got {} queries",
        outcome.polls
    );
    outcome.harness.cleanup().await;
}

/// 提交之后失败（轮询始终不通）必须进对账，**并且留下 task id**。
///
/// 对账的人能做的唯一一件事就是拿这个 id 去上游查；没有它，对账就是盲的。
/// 但这个 id 只留在内部：对客一个字的提示都没有。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn post_acceptance_failure_keeps_the_task_id_for_reconciliation() {
    // 查询永远 500：有界重试耗尽后仍失败 ⇒ 已确认生成、但取不到结果 ⇒ 对账。
    let behaviour = UpstreamBehaviour {
        query_failures: 99,
        ..UpstreamBehaviour::apimart()
    };
    let harness = Harness::start(behaviour).await;
    let key = format!("driver-reconcile-{}", Uuid::new_v4());
    let (status, body) = harness
        .sync_json(
            "/v1/images/generations",
            &key,
            route_request(harness.model, "driver prompt"),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "got {body}");
    assert_eq!(body["error"]["code"].as_str(), Some("outcome_unknown"));
    assert_public_only("受理状态不明", &body);

    let (job_id, state) = harness.job(&key).await;
    assert_eq!(
        state, "reconciliation_required",
        "a post-acceptance failure must go to reconciliation"
    );
    // 对账中的执行没有结果载荷：载荷从来不落库，这条路径连响应里也没有。
    assert_eq!(body["error"]["code"].as_str(), Some("outcome_unknown"));
    assert_eq!(
        harness.count("POST", "/v1/images/generations"),
        1,
        "the create request must never be resent, not even for reconciliation"
    );

    let trace_id: Option<String> =
        sqlx::query_scalar("SELECT provider_trace_id FROM generation.attempts WHERE job_id = $1")
            .bind(job_id)
            .fetch_one(&harness.pool)
            .await
            .expect("attempt row");
    assert_eq!(
        trace_id.as_deref(),
        Some("task-contract-1"),
        "the upstream task id must survive into the attempt so a human can look it up"
    );

    // 而且它必须能从**对账列表接口**看到，而不是只能翻数据库（那是内部运营面）。
    let cases: Value = Client::new()
        .get(format!("{}/api/v1/reconciliation-cases", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .send()
        .await
        .expect("reconciliation cases")
        .json()
        .await
        .expect("cases JSON");
    let case = cases
        .as_array()
        .and_then(|list| list.first())
        .expect("one open reconciliation case");
    assert_eq!(
        case["provider_trace_id"].as_str(),
        Some("task-contract-1"),
        "the case list must expose the trace id, got {case}"
    );
    harness.cleanup().await;
}

/// 合同为**输出张数**声明的上界就是调用方的界：越界在受理前被拒（**参数问题**，400），不建 Job、
/// 不扣款；恰好等于上界照常跑完。
///
/// 用**真素材**起夹具：`n` 的上界写在型号的合同里（顶层 `capability_schema`），各型号不同——平台
/// 没有一个写死的数。夹具那条手写的最小合同根本不声明 `n`，用它验不出这件事。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_requested_image_count_beyond_the_contracts_maximum_is_rejected_before_acceptance() {
    let harness =
        Harness::start_with_bootstrap(UpstreamBehaviour::aihubmix(SyncImageShape::Url), 64).await;
    let account_id = Uuid::parse_str(&harness.account_id).expect("account id");
    // 保底额按请求张数缩放（#35）：`n = 10` 那笔要冻 10 张的钱，夹具账户那 ¥1 不够。先充足以把
    // "合同上界"这条判据与"余额够不够"这条判据分开。
    let client = Client::new();
    let credited = client
        .post(format!(
            "{}/api/v1/accounts/{}/credits",
            harness.base_url, harness.account_id
        ))
        .bearer_auth(&harness.admin_token)
        .json(&json!({
            "amount_microusd": 10_000_000,
            "business_key": "n-boundary"
        }))
        .send()
        .await
        .expect("credit request");
    assert!(
        credited.status().is_success(),
        "充值必须成功：{}",
        credited.status()
    );

    let mut over = route_request(harness.model, "one image too many");
    over["n"] = json!(11);
    let (status, body) = harness
        .sync_json(
            "/v1/images/generations",
            &format!("contract-n-over-{}", Uuid::new_v4()),
            over,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "合同说最多 10 张，11 张是调用方的参数问题：{body}"
    );
    assert_eq!(body["error"]["code"], json!("invalid_parameter"), "{body}");
    assert_public_only("合同上界", &body);
    let jobs: i64 =
        sqlx::query_scalar("SELECT count(*) FROM generation.jobs WHERE account_id = $1")
            .bind(account_id)
            .fetch_one(&harness.pool)
            .await
            .expect("job count");
    assert_eq!(jobs, 0, "越界的请求不许建 Job");

    let mut boundary = route_request(harness.model, "exactly the declared maximum");
    boundary["n"] = json!(10);
    let (status, body) = harness
        .sync_json(
            "/v1/images/generations",
            &format!("contract-n-ok-{}", Uuid::new_v4()),
            boundary,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "恰好等于上界必须放行：{body}");
    assert_sync_success("合同上界之内", &body);

    harness.cleanup().await;
}

/// 输出张数落在**合同之内、这条候选承载面声明的上界之外**时，按承载面的上界发出去：受理照常，
/// 不判候选不合格、不返回 503。
///
/// 界是两个数：合同是**调用方**的界面（这里声明 10），承载面是**这条候选**的能力面（这里声明 4）。
/// 调用方给 6 表达的是"最多 6 张"，而这条候选最多出 4 张——按 4 张发、按实际产出的张数结算，
/// 比把一次合法请求判成平台侧故障更贴合那个意思。请求超过**合同**那 10 张时仍是 400（见上一条）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn an_image_count_above_the_carriers_maximum_is_capped_at_that_maximum() {
    // 候选用测试构造体那一份（渠道、计价、地址齐全），只把**承载面**换成声明 `n` 最多 4 张的一份；
    // 合同另外给（声明最多 10 张），这样才能构造出"在合同之内、在承载面之外"这个区间。
    let mut draft = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    draft["carrier_schema"] = surface_schema(json!({
        "model": {"const": "placeholder"},
        "prompt": {"type": "string", "minLength": 1},
        "n": {"type": "integer", "minimum": 1, "maximum": 4, "default": 1}
    }));
    let contract = surface_schema(json!({
        "model": {"const": "placeholder"},
        "prompt": {"type": "string", "minLength": 1},
        "n": {"type": "integer", "minimum": 1, "maximum": 10, "default": 1}
    }));
    let harness = Harness::start_with_draft(
        draft,
        Some(contract),
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;

    let key = format!("carrier-n-cap-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "six images, the carrier stops at four");
    request["n"] = json!(6);
    let (status, body) = harness
        .sync_json("/v1/images/generations", &key, request)
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "合同之内、承载面之外不该被拒：{body}"
    );
    // 假上游一次只回一张图（它不按请求的 `n` 出图）：这里验的是"请求照常跑完、结果照原形回"，
    // 出图张数与受理时那个数没有保证关系——结算按实际产出算，正是这条裁决的前提。
    assert_sync_success("按承载面的上界发出", &body);
    // 产出张数按**实际产出**给，不按请求的 `n`（Spec 0005 §8 A13）：假上游只回一张。
    let images = body["data"]["result"]["images"]
        .as_array()
        .expect("result.images is an array");
    assert_eq!(
        images.len(),
        1,
        "产出张数按实际产出给，不按请求的 6：{body}"
    );

    let submit = harness.submit_body("/v1/images/generations");
    assert_eq!(
        submit["n"],
        json!(4),
        "上线文里必须是这条候选声明的上限，不是调用方给的 6：{submit}"
    );
    let (_, state) = harness.job(&key).await;
    assert_eq!(state, "succeeded", "夹过的请求照常跑完");

    harness.cleanup().await;
}

// ── RFC 0018 §2.2：请求结构的有界解析 ─────────────────────────────────────────

/// 一份结构超限的请求在**受理前**被拒：400 `invalid_parameter`，不建 Job、不占 Hold、上游一次
/// 都不调。四条计数（容器层数、节点数、对象字段数、累计字符串字节）的对外表现是同一条。
async fn assert_structured_request_is_rejected(harness: &Harness, key: &str, request: Value) {
    let (status, body) = harness
        .sync_json("/v1/images/generations", key, request)
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "结构超限是参数问题：{body}"
    );
    assert_eq!(
        body["error"]["code"].as_str(),
        Some("invalid_parameter"),
        "{body}"
    );
    assert_public_only("结构超限", &body);
    let account_id = Uuid::parse_str(&harness.account_id).expect("the fixture account id");
    let jobs: i64 =
        sqlx::query_scalar("SELECT count(*) FROM generation.jobs WHERE account_id = $1")
            .bind(account_id)
            .fetch_one(&harness.pool)
            .await
            .expect("job count");
    assert_eq!(jobs, 0, "结构超限的请求不许建执行记录");
    assert_eq!(harness.create_calls(), 0, "结构超限的请求不许碰上游");
}

/// 节点密集的两种形态（标量数组、单字段对象数组）在受理前被拒。
///
/// 这两种正是把解析结构抬到远超预留的形态：16 MiB 的 `[{"a":0},…]` 曾能把堆抬到约 2.0 GiB。现在
/// 节点数在解析过程中就被核对，超限即失败，不建记录。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn node_dense_requests_are_rejected_before_acceptance() {
    let harness = Harness::start(UpstreamBehaviour::aihubmix(SyncImageShape::Url)).await;

    let mut scalars = route_request(harness.model, "a scalar node flood");
    scalars["x"] = json!(vec![0_u64; 4096]);
    assert_structured_request_is_rejected(
        &harness,
        &format!("structure-scalars-{}", Uuid::new_v4()),
        scalars,
    )
    .await;

    let mut objects = route_request(harness.model, "a small-object node flood");
    objects["x"] = Value::Array((0..4096).map(|_| json!({"a": 0})).collect());
    assert_structured_request_is_rejected(
        &harness,
        &format!("structure-objects-{}", Uuid::new_v4()),
        objects,
    )
    .await;

    harness.cleanup().await;
}

/// 深嵌套在受理前被拒。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn deeply_nested_requests_are_rejected_before_acceptance() {
    let harness = Harness::start(UpstreamBehaviour::aihubmix(SyncImageShape::Url)).await;
    let mut nested = json!(0);
    for _ in 0..256 {
        nested = json!([nested]);
    }
    let mut request = route_request(harness.model, "nesting far past the supported shape");
    request["x"] = nested;
    assert_structured_request_is_rejected(
        &harness,
        &format!("structure-depth-{}", Uuid::new_v4()),
        request,
    )
    .await;
    harness.cleanup().await;
}

/// 一个对象的字段数超限在受理前被拒。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn wide_requests_are_rejected_before_acceptance() {
    let harness = Harness::start(UpstreamBehaviour::aihubmix(SyncImageShape::Url)).await;
    let mut request = route_request(harness.model, "too many top-level fields");
    for index in 0..300 {
        request[format!("field_{index}")] = json!(index);
    }
    assert_structured_request_is_rejected(
        &harness,
        &format!("structure-fields-{}", Uuid::new_v4()),
        request,
    )
    .await;
    harness.cleanup().await;
}

/// 累计字符串字节超限在受理前被拒。
///
/// 缺省上限等于 16 MiB 的 wire 上限，而解码后的字符串字节恒 ≤ wire 字节，所以它永远不会是缺省
/// 配置下先被撞到的一条——这条用例把上限收紧到 64 字节来验计数本身（生产配置只允许收紧）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn oversized_string_data_is_rejected_before_acceptance() {
    let harness = Harness::start_direct_with(
        candidate(
            "AIHubMix",
            "aihubmix-image-v1",
            &["prompt_only", "image_conditioned", "masked"],
        ),
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
        30,
        ApiProcessSettings {
            request_json_max_string_bytes: Some(64),
            ..ApiProcessSettings::default()
        },
    )
    .await;
    let mut request = route_request(harness.model, &"s".repeat(256));
    request["prompt"] = json!("s".repeat(256));
    assert_structured_request_is_rejected(
        &harness,
        &format!("structure-strings-{}", Uuid::new_v4()),
        request,
    )
    .await;
    harness.cleanup().await;
}

/// 边界之内的合法大请求仍然通过：近 1 MiB 的长参数照旧走完受理与生成。
///
/// 16 MiB 的边界由 `crates/domain` 的 `a_full_size_image_request_still_parses` 与 API 的内存探针
/// （`apps/api/src/tests.rs`）在解析层钉住；这条用例验的是它端到端不被结构上限误伤。生成入口收敛
/// 为只收公网 URL 后，参考图字节不再随正文进来，改用近上限的长参数承载同样的正文规模。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_large_but_bounded_request_still_passes() {
    let harness =
        Harness::start_with_bootstrap(UpstreamBehaviour::aihubmix(SyncImageShape::Url), 64).await;
    let key = format!("structure-large-ok-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "a large but legitimate request");
    // 近 1 MiB 的 `prompt`：声明的普通参数，正文因此贴近结构上限；参考图仍是一个普通公网 URL。
    request["prompt"] = json!("p".repeat(1024 * 1024));
    request["image"] = json!(harness.png_url());
    let (status, body) = harness
        .sync_json("/v1/images/generations", &key, request)
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "结构上限之内的合法大请求必须照旧通过：{body}"
    );
    assert_sync_success("边界之内的合法大请求", &body);
    assert_job_succeeded(&harness, &key).await;
    harness.cleanup().await;
}
