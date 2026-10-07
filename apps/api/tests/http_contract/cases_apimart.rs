use super::*;

/// APIMart 的完整驱动流程：提交 → 轮询 → 终态；结果地址原样交回，
/// 计量证据与对账标识留在内部。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn apimart_driver_executes_the_task_flow_against_a_local_upstream() {
    let harness = Harness::start(UpstreamBehaviour::apimart()).await;
    let key = format!("driver-{}", Uuid::new_v4());
    let request = route_request(harness.model, "driver prompt");
    let (status, body) = harness
        .sync_json("/v1/images/generations", &key, request.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("任务式流程", &body);

    let (job_id, state) = harness.job(&key).await;
    assert_eq!(state, "succeeded", "the driver flow must settle the job");
    assert_eq!(
        body["data"]["result"]["images"],
        json!([{
            "url": [format!("{}/result.png", harness.upstream_base_url)],
            "expires_at": 4_000_000_000u64
        }]),
        "结果信封里只有上游给的那个地址与渠道给的过期时刻"
    );
    assert_eq!(harness.count("GET", "/result.png"), 0, "平台不许下载结果图");

    // 计量证据：四分项 usage 落到 attempts.metering_evidence。
    let evidence: Value =
        sqlx::query_scalar("SELECT metering_evidence FROM generation.attempts WHERE job_id = $1")
            .bind(job_id)
            .fetch_one(&harness.pool)
            .await
            .expect("metering evidence");
    assert_eq!(evidence["usage"]["input_text_tokens"], 14);
    assert_eq!(evidence["usage"]["input_image_tokens"], 0);
    assert_eq!(evidence["usage"]["output_image_tokens"], 196);
    assert_eq!(evidence["usage"]["total_tokens"], 210);

    // 对账标识落到 attempts.provider_trace_id 列：上游确认受理（`record_acceptance`）时
    // 随任务句柄一起写下，人工对账据此能找回同一个上游任务。
    let trace_id: Option<String> =
        sqlx::query_scalar("SELECT provider_trace_id FROM generation.attempts WHERE job_id = $1")
            .bind(job_id)
            .fetch_one(&harness.pool)
            .await
            .expect("attempt trace id");
    assert_eq!(
        trace_id.as_deref(),
        Some("task-contract-1"),
        "the upstream task id must be persisted for manual reconciliation"
    );

    // 成本事实：上游终态**直接声明了金额**，所以直接取它（含渠道侧折扣，比自算权威）；
    // 币种是**该供给声明的**那个，不假定 USD。折算值这一片不写——汇率还没有落点。
    let (amount, currency, source, cny) = harness.attempt_cost(job_id).await;
    assert_eq!(source.as_deref(), Some("declared"));
    assert_eq!(amount, Some(11_354), "实测样例 cost = 0.011354");
    assert_eq!(currency.as_deref(), Some("USD"));
    assert_eq!(
        cny,
        Some(80_614),
        "受理时冻结的折算率（USD → CNY 7.1）把上游声明的金额折成人民币，毛利要用它"
    );

    // 采集成本**不改对客金额**：实收仍是该渠道费率 × 实际分项 token
    // （14 文本输入 × 5 + 196 图像输出 × 30 = 5950 微单位，落账取整到 6000），与上游声明的 11354 是两个量。
    assert_eq!(
        harness.captured_microusd(job_id).await,
        -6_000,
        "上游声明的金额只进成本口径，不许动对客实收"
    );

    // Driver 的线上请求：只提交一次，且参数在顶层（无 extra 包装）。
    assert_eq!(
        harness.count("POST", "/v1/images/generations"),
        1,
        "the create request must never be resent"
    );
    let submit_body = harness.submit_body("/v1/images/generations");
    assert_eq!(submit_body["model"], harness.model);
    assert_eq!(submit_body["prompt"], "driver prompt");
    assert!(
        submit_body.get("extra").is_none(),
        "APIMart takes parameters at the top level"
    );
    harness.assert_only_declared_fields(&request);
    assert!(
        harness.count("GET", "/v1/tasks/") >= 1,
        "the driver must poll the task at least once"
    );
    harness.cleanup().await;
}

/// 公网 URL 原样透传（不下载、不上传）；内联 data URL 在受理前被拒。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn apimart_passes_public_urls_through_and_rejects_inline_images() {
    let harness = Harness::start(UpstreamBehaviour::apimart()).await;

    // 1) 公网 URL：原样写进 `image_urls`，一次上传都没有。
    let reference_url = format!("{}/inputs/ref.png", harness.upstream_base_url);
    let key = format!("driver-public-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "edit this image");
    request["image"] = json!(reference_url);
    let (status, body) = harness
        .sync_json("/v1/images/generations", &key, request.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let submit_body = harness.submit_body("/v1/images/generations");
    assert_eq!(
        submit_body["image_urls"],
        json!([reference_url]),
        "公网 URL 必须逐字透传"
    );
    assert_eq!(
        harness.count("POST", "/v1/uploads/images"),
        0,
        "公网 URL 不需要上传"
    );
    assert_eq!(
        harness.count("GET", "/inputs/ref.png"),
        0,
        "平台不下载调用方给的公网参考图"
    );
    harness.assert_only_declared_fields(&request);

    // 2) 内联 data URL + 遮罩：上游只收公网 URL，取值在受理前一律被拒，不调上游、不建记录。
    let creates_before = harness.create_calls();
    let key = format!("driver-inline-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "masked edit");
    request["image_urls"] = json!([inline_png_data_url()]);
    request["mask"] = json!(inline_png_data_url());
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert_eq!(
        body["error"]["code"].as_str(),
        Some("public_image_url_required"),
        "{body}"
    );
    assert_public_only("内联 data URL 与遮罩", &body);
    assert_eq!(
        harness.count("POST", "/v1/uploads/images"),
        0,
        "平台不再把内联图片上传到上游"
    );
    assert_eq!(harness.create_calls(), creates_before, "被拒请求不调上游");
    let jobs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM generation.jobs WHERE idempotency_key_digest = $1",
    )
    .bind(idempotency_key_digest(&key))
    .fetch_one(&harness.pool)
    .await
    .expect("job count");
    assert_eq!(jobs, 0, "被拒的 data URL 不建执行记录");

    // 3) 上游不发结果以外的任何东西：平台也不去取结果。
    assert_eq!(harness.count("GET", "/result.png"), 0);
    harness.cleanup().await;
}
