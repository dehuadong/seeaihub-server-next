use super::*;

/// 对客账务读：跑通一笔真实请求之后，**用量**里出现这一笔，且扣费金额与账本 `capture` 条目一致。
///
/// 这是 Spec V-C7 与 C9：客户要看的是"什么时候、什么型号、几张、扣了多少"，不是平台内部的任务号——
/// 所以这条用例同时钉住"有这些字段"与"没有 Job 标识"。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_customer_sees_its_own_usage_with_the_amount_the_ledger_charged() {
    let harness = Harness::start_with_draft(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;
    let client = Client::new();
    assert_eq!(
        republish_priced(
            &harness,
            &client,
            openai_floor_amounts(),
            priced_consumer_rates(),
            2_000
        )
        .await,
        StatusCode::OK,
        "带定价的发布必须成功"
    );

    // 账户 + 密钥（老路子），再把这个**已有账户**配上一个客户登录身份。
    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let email = "usage@example.com";
    let password = "a-long-enough-password";
    let opened = client
        .post(format!("{}/api/v1/customers", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .json(&json!({"email": email, "password": password, "account_id": account_id}))
        .send()
        .await
        .expect("open customer request");
    assert_eq!(opened.status(), StatusCode::CREATED);

    // 跑通一笔请求：Worker 在同步窗口内把它做到终态。
    let _worker = harness.spawn_worker();
    let key = format!("usage-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &route_request(harness.model, "usage contract"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");

    // 客户登录，读自己的用量。
    let session = client
        .post(format!("{}/v1/customer/sessions", harness.base_url))
        .json(&json!({"email": email, "password": password}))
        .send()
        .await
        .expect("customer login request")
        .json::<Value>()
        .await
        .expect("login body")["token"]
        .as_str()
        .expect("session token")
        .to_owned();

    // 夹具自检：这一笔请求确实落成了这个账户名下的一条执行记录（用量为空时先看这里）。
    let jobs: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM generation.jobs WHERE account_id = $1")
            .bind(Uuid::parse_str(&account_id).expect("account id"))
            .fetch_one(&harness.pool)
            .await
            .expect("job count");
    assert_eq!(jobs, 1, "夹具必须先有这一笔执行记录");

    let usage = client
        .get(format!("{}/v1/customer/usage", harness.base_url))
        .bearer_auth(&session)
        .send()
        .await
        .expect("usage request");
    let status = usage.status();
    let usage = usage.json::<Value>().await.expect("usage body");
    assert_eq!(status, StatusCode::OK, "用量读必须成功：{usage}");
    let rows = usage["usage"].as_array().expect("usage array");
    assert_eq!(rows.len(), 1, "这一笔请求必须出现在用量里：{usage}");
    let row = &rows[0];
    assert_eq!(row["status"], json!("succeeded"));
    assert_eq!(row["kind"], json!("generation"));
    assert_eq!(row["image_count"], json!(1));
    assert_eq!(row["gateway_model"], json!(harness.model));
    // **对客不可见**：不出现任务号与内部状态取值。
    let rendered = usage.to_string();
    assert!(
        !rendered.contains("job_id") && !rendered.contains("reconciliation_required"),
        "用量是执行记录的对客投影，不该带任务标识或内部状态：{usage}"
    );

    // 扣费金额与账本那条 `capture` 逐位一致。
    let charged: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(amount_microusd), 0)::bigint FROM ledger.entries \
         WHERE account_id = $1 AND kind = 'capture'",
    )
    .bind(Uuid::parse_str(&account_id).expect("account id"))
    .fetch_one(&harness.pool)
    .await
    .expect("ledger capture sum");
    assert_eq!(
        row["charged_microusd"],
        json!(charged),
        "用量里的扣费必须来自账本"
    );

    harness.cleanup().await;
}

/// 账单汇总按区间**全量**算：把明细的条数上限压到 1，汇总里的请求数仍然是全部。
///
/// 这是 Spec V-C8 的关键：汇总不能随页大小变化，否则"明细求和等于汇总"会随分页摇摆。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_billing_summary_does_not_shrink_with_the_detail_page_size() {
    let harness = Harness::start_with_draft(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;
    let client = Client::new();
    assert_eq!(
        republish_priced(
            &harness,
            &client,
            openai_floor_amounts(),
            priced_consumer_rates(),
            2_000
        )
        .await,
        StatusCode::OK
    );

    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let email = "billing@example.com";
    let password = "a-long-enough-password";
    assert_eq!(
        client
            .post(format!("{}/api/v1/customers", harness.base_url))
            .bearer_auth(&harness.admin_token)
            .json(&json!({"email": email, "password": password, "account_id": account_id}))
            .send()
            .await
            .expect("open customer request")
            .status(),
        StatusCode::CREATED
    );

    let _worker = harness.spawn_worker();
    for index in 0..2 {
        let key = format!("billing-{index}-{}", Uuid::new_v4());
        let (status, body) = post_json(
            &harness.base_url,
            &api_key,
            "/v1/images/generations",
            &key,
            &route_request(harness.model, "billing contract"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "got {body}");
    }

    let session = client
        .post(format!("{}/v1/customer/sessions", harness.base_url))
        .json(&json!({"email": email, "password": password}))
        .send()
        .await
        .expect("customer login request")
        .json::<Value>()
        .await
        .expect("login body")["token"]
        .as_str()
        .expect("session token")
        .to_owned();

    // 明细只给一条。
    let usage = client
        .get(format!("{}/v1/customer/usage?limit=1", harness.base_url))
        .bearer_auth(&session)
        .send()
        .await
        .expect("usage request")
        .json::<Value>()
        .await
        .expect("usage body");
    assert_eq!(usage["count"], json!(1));
    assert_eq!(usage["truncated"], json!(true));

    // 汇总说两次，而且与全量明细的扣费求和一致。
    let billing = client
        .get(format!("{}/v1/customer/billing", harness.base_url))
        .bearer_auth(&session)
        .send()
        .await
        .expect("billing request")
        .json::<Value>()
        .await
        .expect("billing body");
    assert_eq!(billing["requests"], json!(2), "汇总不随明细页大小变化");
    assert_eq!(billing["images"], json!(2));

    let all = client
        .get(format!("{}/v1/customer/usage?limit=100", harness.base_url))
        .bearer_auth(&session)
        .send()
        .await
        .expect("usage request")
        .json::<Value>()
        .await
        .expect("usage body");
    let summed: i64 = all["usage"]
        .as_array()
        .expect("usage array")
        .iter()
        .map(|row| row["charged_microusd"].as_i64().expect("charged"))
        .sum();
    assert_eq!(
        billing["charged_microusd"],
        json!(summed),
        "汇总的扣费总额必须等于全量明细的求和"
    );

    harness.cleanup().await;
}

/// 余额与持有中分开给，且都来自账本（Spec C7）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_customer_reads_balance_and_held_separately() {
    let harness = Harness::start_with_draft(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;
    let client = Client::new();

    let (account_id, _api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 3_000_000).await;
    let email = "account@example.com";
    let password = "a-long-enough-password";
    assert_eq!(
        client
            .post(format!("{}/api/v1/customers", harness.base_url))
            .bearer_auth(&harness.admin_token)
            .json(&json!({"email": email, "password": password, "account_id": account_id}))
            .send()
            .await
            .expect("open customer request")
            .status(),
        StatusCode::CREATED
    );

    let session = client
        .post(format!("{}/v1/customer/sessions", harness.base_url))
        .json(&json!({"email": email, "password": password}))
        .send()
        .await
        .expect("customer login request")
        .json::<Value>()
        .await
        .expect("login body")["token"]
        .as_str()
        .expect("session token")
        .to_owned();

    let account = client
        .get(format!("{}/v1/customer/account", harness.base_url))
        .bearer_auth(&session)
        .send()
        .await
        .expect("account request")
        .json::<Value>()
        .await
        .expect("account body");
    assert_eq!(account["balance_microusd"], json!(3_000_000));
    assert_eq!(account["held_microusd"], json!(0), "没有在飞请求时持有为 0");

    // 流水里能看到那笔充值（Spec C8）。
    let ledger = client
        .get(format!("{}/v1/customer/ledger", harness.base_url))
        .bearer_auth(&session)
        .send()
        .await
        .expect("ledger request")
        .json::<Value>()
        .await
        .expect("ledger body");
    let kinds: Vec<&str> = ledger["entries"]
        .as_array()
        .expect("entries array")
        .iter()
        .map(|entry| entry["kind"].as_str().expect("kind"))
        .collect();
    assert!(
        kinds.contains(&"credit"),
        "充值记录必须出现在流水里：{ledger}"
    );

    harness.cleanup().await;
}
