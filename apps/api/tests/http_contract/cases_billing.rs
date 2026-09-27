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

    // 判据要求用量里出现这一笔的**时间**：它是这一笔受理的时刻（执行记录的 `created_at`），不是
    // 这次查询的时刻——所以断言它与库里那条执行记录逐位相同，"看起来像最近"证明不了是同一笔。
    let job_created_at: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT created_at FROM generation.jobs WHERE account_id = $1")
            .bind(Uuid::parse_str(&account_id).expect("account id"))
            .fetch_one(&harness.pool)
            .await
            .expect("job created_at");
    let created_at = row["created_at"]
        .as_str()
        .expect("用量要给出时间，否则「什么时候发生的」就查不到");
    assert_eq!(
        chrono::DateTime::parse_from_rfc3339(created_at)
            .expect("时间要是 RFC 3339")
            .with_timezone(&chrono::Utc),
        job_created_at,
        "用量里的时间必须是这一笔执行记录的受理时刻：{usage}"
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

/// 账单口径的边界（V-C8）：
///
/// - 区间是**半开**的 `[since, until)`：`until` 放在请求**之前**，那一次就不该被算进来；
/// - 扣费总额**只计扣费与调整**：预授权 `hold` 与它的 `release` 是一进一出，算进来会得到
///   "平台占用过多少"而不是"扣了多少"——这条用例按类型分别求和来钉住它。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_billing_window_is_half_open_and_ignores_holds() {
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

    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let email = "window@example.com";
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

    // 窗口边界取在请求的**前**与**后**：请求之后再取一次"现在"，用它当 `until` 就该是 0 条。
    let before = chrono::Utc::now();
    let _worker = harness.spawn_worker();
    let key = format!("window-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &route_request(harness.model, "window contract"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let after = chrono::Utc::now();

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
        .expect("token")
        .to_owned();

    // 区间包住那一次：算进来。
    //
    // 时间戳按**秒精度 UTC** 给：`to_rfc3339()` 会带上纳秒（`...465153900+00:00`），服务端不认。
    let stamp = |instant: chrono::DateTime<chrono::Utc>| {
        instant.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    };
    let included = client
        .get(format!(
            "{}/v1/customer/billing?since={}&until={}",
            harness.base_url,
            stamp(before),
            stamp(after + chrono::Duration::seconds(1))
        ))
        .bearer_auth(&session)
        .send()
        .await
        .expect("billing request")
        .json::<Value>()
        .await
        .expect("billing body");
    assert_eq!(included["requests"], json!(1), "区间包住时应当算进来");

    // `until` 在请求之前：半开区间把它排除在外。
    let excluded = client
        .get(format!(
            "{}/v1/customer/billing?until={}",
            harness.base_url,
            stamp(before)
        ))
        .bearer_auth(&session)
        .send()
        .await
        .expect("billing request")
        .json::<Value>()
        .await
        .expect("billing body");
    assert_eq!(
        excluded["requests"],
        json!(0),
        "until 在请求之前时不该算进来"
    );
    assert_eq!(excluded["charged_microusd"], json!(0));

    // 库里按类型分别求和：扣费总额等于 `capture`，**不是** `capture + hold + release`。
    let sums: Vec<(String, i64)> = sqlx::query_as(
        "SELECT kind, COALESCE(SUM(amount_microusd), 0)::bigint FROM ledger.entries \
         WHERE account_id = $1 GROUP BY kind ORDER BY kind",
    )
    .bind(Uuid::parse_str(&account_id).expect("account id"))
    .fetch_all(&harness.pool)
    .await
    .expect("ledger sums");

    let capture = sums
        .iter()
        .find(|(kind, _)| kind == "capture")
        .map(|(_, total)| *total)
        .unwrap_or(0);
    let holds = sums
        .iter()
        .filter(|(kind, _)| kind == "hold" || kind == "release")
        .map(|(_, total)| *total)
        .sum::<i64>();
    assert!(capture < 0, "夹具应当真的扣了一笔：{sums:?}");
    assert_eq!(holds, 0, "hold 与 release 应当正好抵消：{sums:?}");
    assert_eq!(
        included["charged_microusd"],
        json!(capture),
        "扣费总额必须等于 capture 的求和，不能把 hold/release 算进来"
    );

    // **明细与汇总必须同一条区间口径**：同一个 `[since, until)` 下，`ledger` 里落在这个窗口内的
    // 扣费条目之和，要等于 `billing` 报的扣费总额。两处口径一旦分叉（一处含端点、一处不含），
    // 边界上那一笔就会被一边算进去、另一边不算——而那种分叉只在条目恰好落在边界上时才显形。
    let upper = stamp(after + chrono::Duration::seconds(1));
    let ledger = client
        .get(format!(
            "{}/v1/customer/ledger?since={}&until={}&limit=100",
            harness.base_url,
            stamp(before),
            upper
        ))
        .bearer_auth(&session)
        .send()
        .await
        .expect("ledger request")
        .json::<Value>()
        .await
        .expect("ledger body");
    let ledger_charges: i64 = ledger["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .filter(|entry| matches!(entry["kind"].as_str(), Some("capture") | Some("adjustment")))
        .map(|entry| entry["amount_microusd"].as_i64().expect("amount"))
        .sum();
    assert_eq!(
        json!(ledger_charges),
        included["charged_microusd"],
        "同一区间下明细里扣费条目之和必须等于汇总报的扣费总额：ledger={ledger} billing={included}"
    );

    // 上界**不含**：把它压到请求**之后**、但早于"再往后一点"的位置时，那笔扣费已经在界内；
    // 而压到请求**之前**时它必须在界外——`ledger` 与 `billing` 要同时给出 0。
    let ledger_before = client
        .get(format!(
            "{}/v1/customer/ledger?until={}&limit=100",
            harness.base_url,
            stamp(before)
        ))
        .bearer_auth(&session)
        .send()
        .await
        .expect("ledger request")
        .json::<Value>()
        .await
        .expect("ledger body");
    let charges_before: i64 = ledger_before["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .filter(|entry| matches!(entry["kind"].as_str(), Some("capture") | Some("adjustment")))
        .map(|entry| entry["amount_microusd"].as_i64().expect("amount"))
        .sum();
    assert_eq!(
        charges_before, 0,
        "界外的扣费不该出现在明细里：{ledger_before}"
    );
    assert_eq!(
        excluded["charged_microusd"],
        json!(0),
        "汇总同一条口径：{excluded}"
    );
    assert_eq!(
        ledger_before["total"].as_u64(),
        Some(ledger_before["count"].as_u64().expect("count")),
        "空结果时 total 与 count 一致（都是 0）：{ledger_before}"
    );

    harness.cleanup().await;
}

/// 客户吊销自己的密钥之后，用那把密钥调**对客生成接口**被拒（V-C4）。
///
/// 判据点名的就是这个端点：`/v1/account` 只证明读接口不认它了，而生成接口前面排着余额闸门与
/// 受理——钱不够也回拒。两者混在一起就分不出这次被拒是不是因为密钥吊销了。所以这里先给账户充够钱、
/// 用**同一把密钥**跑通一笔生成（钱与路由因此都不是拒绝的理由），再吊销、再打同一个端点。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_customer_revoked_key_is_rejected_at_the_generation_entry() {
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

    let (account_id, _) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let email = "revoked-key@example.com";
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

    // 客户自己发一把密钥：明文与标识都在这一次响应里，吊销要用标识，而库里只有明文摘要。
    let issued = client
        .post(format!("{}/v1/customer/api-keys", harness.base_url))
        .bearer_auth(&session)
        .json(&json!({"label": "revoked-at-the-generation-entry"}))
        .send()
        .await
        .expect("issue key request");
    assert_eq!(issued.status(), StatusCode::CREATED);
    let issued = issued.json::<Value>().await.expect("issue body");
    let api_key = issued["api_key"]
        .as_str()
        .expect("plaintext key")
        .to_owned();
    let key_id = issued["key_id"].as_str().expect("key id").to_owned();

    // 吊销之前先跑通一笔：这个账户的钱与这条路由到这里都不再是拒绝的理由。
    let _worker = harness.spawn_worker();
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &format!("revoke-entry-before-{}", Uuid::new_v4()),
        &route_request(harness.model, "before revocation"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "吊销前这把密钥必须能受理：{body}");

    let revoked = client
        .delete(format!(
            "{}/v1/customer/api-keys/{key_id}",
            harness.base_url
        ))
        .bearer_auth(&session)
        .send()
        .await
        .expect("revoke request");
    assert_eq!(revoked.status(), StatusCode::NO_CONTENT);

    let (denied, denied_body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &format!("revoke-entry-after-{}", Uuid::new_v4()),
        &route_request(harness.model, "after revocation"),
    )
    .await;
    assert_eq!(
        denied,
        StatusCode::UNAUTHORIZED,
        "吊销后同一把密钥必须在对客生成接口被拒：{denied_body}"
    );
    // 拒绝的理由必须是"密钥无效"而不是"钱不够"：余额闸门回 402 `insufficient_balance`，
    // 所以这里要的是那个码，不是随便一个非 200。
    assert_eq!(
        denied_body["error"]["code"].as_str(),
        Some("invalid_api_key"),
        "{denied_body}"
    );
    // 密钥无效要在余额闸门与受理**之前**就被拒：这个账户名下不该多出第二条执行记录，
    // 上游也一次都不该被碰到。
    let jobs: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM generation.jobs WHERE account_id = $1")
            .bind(Uuid::parse_str(&account_id).expect("account id"))
            .fetch_one(&harness.pool)
            .await
            .expect("job count");
    assert_eq!(jobs, 1, "被拒的请求不该留下第二条执行记录");

    harness.cleanup().await;
}
