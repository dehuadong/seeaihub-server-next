use super::*;

/// 这次用例的上限：1 元（微单位）。
///
/// 它要**同时**满足两件事：夹具那条候选发布得过（参考成本 11354 微美元，按正常折算率约 ¥0.08），
/// 又小到用例能用"把折算率调差"或"把单价抬一微单位"精确地越过它。
const CEILING_MICROUSD: u64 = 1_000_000;

/// 夹具那条候选**带上定价**：成本护栏判的就是发布数据里的参考成本、成本来源与保底表。
fn priced_token_candidate() -> Value {
    let mut draft = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    draft["reference_cost_microusd"] = json!(11_354);
    draft["cost_basis"] = json!("computed");
    draft["floor_amounts"] = openai_floor_amounts();
    draft
}

/// **发布期的成本护栏**：一条候选按合同允许的最大输出张数算下来可能花掉的钱超过上限，整份发布被拒
/// 并点名是哪条候选；而**恰好等于**上限照发——判据是 `>`，不是 `>=`。
///
/// 拒的是**整份**发布、不是"把超了的那条剔掉"：候选集连同档位与权重是一个整体，替发布者删一条
/// 会让路由悄悄变成另一副样子。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_candidate_that_could_cost_more_than_the_ceiling_is_rejected_at_publication() {
    let harness = Harness::start_with_cost_ceiling(
        priced_token_candidate(),
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
        CEILING_MICROUSD,
    )
    .await;
    let client = Client::new();

    // 这份载体没声明 `n`（一次请求一张），所以按张计价的最坏单次成本就是单价本身。单价 1_000_000
    // 微元恰好等于上限 1_000_000：允许。
    assert_eq!(
        republish_candidate(
            &harness,
            &client,
            unit_candidate("per_image", CEILING_MICROUSD, "CNY"),
            2_000,
        )
        .await,
        StatusCode::OK,
        "恰好落在上限上的候选必须发得出去"
    );

    // 单价抬一微单位：整份发布被拒，报错点名这条候选、它可能花多少、上限是多少。
    let mut over = unit_candidate("per_image", CEILING_MICROUSD + 1, "CNY");
    over["base_url"] = json!(harness.upstream_base_url);
    let body = publication_body(
        Harness::MODEL,
        "route-test-1",
        None,
        vec![over],
        Some(2_000),
    );
    let response = client
        .post(format!("{}/api/v1/runtime-revisions", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .json(&body)
        .send()
        .await
        .expect("an over-ceiling publication");
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "超过上限的整份发布必须被拒"
    );
    let rejected: Value = response.json().await.expect("rejection body");
    assert_eq!(rejected["error"]["code"], json!("validation_error"));
    let message = rejected["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(
        message.contains("offerings[0]"),
        "报错要点名是哪条候选：{rejected}"
    );
    assert!(
        message.contains(&(CEILING_MICROUSD + 1).to_string()),
        "报错要说出它可能花多少：{rejected}"
    );
    assert!(
        message.contains(&CEILING_MICROUSD.to_string()),
        "报错要说出上限是多少：{rejected}"
    );

    // 被拒的那次**没有留下任何东西**：刚刚那条合规的候选再发一次照样成功（发布是原子的，
    // 超上限那次不会写下半份修订）。
    assert_eq!(
        republish_candidate(
            &harness,
            &client,
            unit_candidate("per_image", CEILING_MICROUSD, "CNY"),
            2_000,
        )
        .await,
        StatusCode::OK,
        "被拒的发布不许影响已生效的那一份"
    );

    harness.cleanup().await;
}

/// **受理期的成本护栏**：上限是进程启动时读的一个数，而**已经发布出去的**供给不会自己重判——
/// 上限被调小、或折算率变差之后，真正落地的那一笔在受理时被挡下。
///
/// 这里用**折算率变差**构造"发布时合规、受理时超限"（上游涨价就是同一个形态）：对客是**平台侧
/// 故障**（503 `platform_unavailable`），不是"你余额不足"——客户的余额一分未动，请求本身也没错。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_request_that_would_cost_more_than_the_ceiling_is_a_platform_fault() {
    let harness = Harness::start_with_cost_ceiling(
        priced_token_candidate(),
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
        CEILING_MICROUSD,
    )
    .await;
    let client = Client::new();

    // 上游涨价：折算率从 7.1 跳到 1000 ⇒ 同一条候选（参考成本 11354 微美元）一次要花 ¥11.35，
    // 而发布期判它的时候用的还是那个 7.1。
    let raised = client
        .put(format!("{}/api/v1/fx-rates", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .json(&json!({"currency": "USD", "rate_micros": 1_000_000_000u64}))
        .send()
        .await
        .expect("a weaker fx rate");
    assert_eq!(raised.status(), StatusCode::NO_CONTENT, "折算率随时可改");

    let key = format!("cost-ceiling-{}", Uuid::new_v4());
    let (status, body) = harness
        .sync_json(
            "/v1/images/generations",
            &key,
            route_request(harness.model, "this would cost too much"),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "护栏挡住的是平台自己的成本，对客是平台侧故障：{body}"
    );
    assert_eq!(
        body["error"]["code"],
        json!("platform_unavailable"),
        "与「一条候选都不合格」共用同一条对客语义，不新造码、也不说成余额不足：{body}"
    );
    assert_public_only("成本护栏", &body);

    // 没有留下任何东西：不建 Job、不扣款、不留预授权。
    let account_id = Uuid::parse_str(&harness.account_id).expect("account id");
    let jobs: i64 =
        sqlx::query_scalar("SELECT count(*) FROM generation.jobs WHERE account_id = $1")
            .bind(account_id)
            .fetch_one(&harness.pool)
            .await
            .expect("job count");
    assert_eq!(jobs, 0, "被护栏挡下的请求不许建 Job");
    let holds: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ledger.entries WHERE account_id = $1 AND kind = 'hold'",
    )
    .bind(account_id)
    .fetch_one(&harness.pool)
    .await
    .expect("hold count");
    assert_eq!(holds, 0, "没受理就没有预授权");
    assert_eq!(
        database_balance(&harness, &harness.account_id).await,
        1_000_000,
        "余额一分未动"
    );

    // **反向对照**：把折算率调回正常值，同一个请求照常受理并跑完——被拒确实是因为那道护栏，
    // 而不是这次请求本身有什么毛病。对"上限之内照常服务"这一侧，这就是最强的证据。
    let restored = client
        .put(format!("{}/api/v1/fx-rates", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .json(&json!({"currency": "USD", "rate_micros": 7_100_000u64}))
        .send()
        .await
        .expect("the fx rate back to normal");
    assert_eq!(restored.status(), StatusCode::NO_CONTENT);
    let key = format!("cost-ceiling-ok-{}", Uuid::new_v4());
    let (status, body) = harness
        .sync_json(
            "/v1/images/generations",
            &key,
            route_request(harness.model, "within the ceiling"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "上限之内的同一请求照常跑完：{body}");
    assert_sync_success("成本护栏之内的请求", &body);

    harness.cleanup().await;
}
