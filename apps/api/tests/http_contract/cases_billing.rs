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
        republish_priced(&harness, &client, openai_floor_amounts(), 2_000).await,
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
    assert_eq!(row["status"], json!("completed"));
    assert_eq!(row["kind"], json!("generation"));
    // 产出张数落在 `image_count`：这一笔实际产出一张，用量读的就是那个落点。
    assert_eq!(row["type"], json!("image"), "{usage}");
    assert_eq!(row["usage"]["images"], json!(1), "{usage}");
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
        row["charged_points"],
        json!(charged / 1_000),
        "用量里的扣费必须来自账本（积分 = 微单位 / 1000）"
    );
    // 生成响应与用量记录是同一条事实（Spec 0005 §8 A13）：`id` 是同一个调用标识，
    // `cost` 是同一条扣费。
    assert_eq!(
        body["data"]["id"], row["id"],
        "生成响应的 id 必须与用量记录的调用标识一致：{body}"
    );
    // 响应的 `cost` 是正数金额，用量记录的扣费按既有约定带符号（扣费为负）：比金额。
    assert_eq!(
        body["data"]["cost"].as_i64(),
        row["charged_points"].as_i64().map(i64::abs),
        "生成响应的 cost 必须等于用量记录里那一笔扣费的金额：{body}"
    );

    // 管理端读同一条记录：类型与用量与客户侧一致（Spec A2）。
    let admin: Value = client
        .get(format!(
            "{}/api/v1/accounts/{}/usage",
            harness.base_url, account_id
        ))
        .bearer_auth(&harness.admin_token)
        .send()
        .await
        .expect("admin usage request")
        .json()
        .await
        .expect("admin usage body");
    assert_eq!(admin["usage"][0]["type"], row["type"], "{admin}");
    assert_eq!(admin["usage"][0]["usage"], row["usage"], "{admin}");

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
        republish_priced(&harness, &client, openai_floor_amounts(), 2_000).await,
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
    assert_eq!(
        billing["usage"]["images"],
        json!(2),
        "两次请求各产出一张，汇总按落点算"
    );

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
        .map(|row| row["charged_points"].as_i64().expect("charged"))
        .sum();
    assert_eq!(
        billing["charged_points"],
        json!(summed),
        "汇总的扣费总额必须等于全量明细的求和"
    );
    let summed_images: i64 = all["usage"]
        .as_array()
        .expect("usage array")
        .iter()
        .map(|row| row["usage"]["images"].as_i64().expect("image count"))
        .sum();
    assert_eq!(
        billing["usage"]["images"],
        json!(summed_images),
        "汇总的产出张数必须等于全量明细的求和"
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
    assert_eq!(account["balance_points"], json!(3_000));
    assert_eq!(account["held_points"], json!(0), "没有在飞请求时持有为 0");

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
        republish_priced(&harness, &client, openai_floor_amounts(), 2_000).await,
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
    assert_eq!(excluded["charged_points"], json!(0));

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
        included["charged_points"],
        json!(capture / 1_000),
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
        .map(|entry| entry["amount_points"].as_i64().expect("amount"))
        .sum();
    assert_eq!(
        json!(ledger_charges),
        included["charged_points"],
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
        .map(|entry| entry["amount_points"].as_i64().expect("amount"))
        .sum();
    assert_eq!(
        charges_before, 0,
        "界外的扣费不该出现在明细里：{ledger_before}"
    );
    assert_eq!(
        excluded["charged_points"],
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
        republish_priced(&harness, &client, openai_floor_amounts(), 2_000).await,
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

/// 跨 UTC 日结算的扣费计入**结算日**，已完成请求按终态时刻归属（A8）。
///
/// 用例先把请求**受理在前天 23:50**（不起 Worker，同步入口超时，Job 停在受理态），再把受理时刻改到
/// 前天，然后才起 Worker 在**今天**把它结算掉。受理日与结算日因此真的错开；按 UTC 自然日分别读
/// 用量、账单与每日合计：受理那天查不到它，结算那天才查到——归属看终态时刻，每日合计看结算日，
/// 都不是受理时刻。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_settlement_after_midnight_is_billed_on_the_day_it_settled() {
    let harness = Harness::start_with_draft(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;
    let client = Client::new();
    assert_eq!(
        republish_priced(&harness, &client, openai_floor_amounts(), 2_000).await,
        StatusCode::OK,
        "带定价的发布必须成功"
    );

    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let email = "cross-day@example.com";
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

    // 受理与结算在同一次请求里完成（直接执行）；跨天形态由**回填受理时刻**造出来。
    let key = format!("cross-day-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &route_request(harness.model, "settles after midnight"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "直接执行必须跑完并结算：{body}");
    let (job_id, state) = harness.job(&key).await;
    assert_eq!(state, "succeeded", "夹具必须已经结算：{body}");

    // 受理时刻改到**前天 23:50**：结算已经发生（终态时刻与 capture 同事务落库），受理时刻与
    // 结算时刻因此真的跨了 UTC 自然日，按受理日入桶与按结算日入桶才会落到不同的行。
    sqlx::query(
        "UPDATE generation.jobs
         SET created_at = ((date_trunc('day', now() AT TIME ZONE 'UTC') - interval '2 days')
                           + interval '23 hours 50 minutes') AT TIME ZONE 'UTC'
         WHERE id = $1",
    )
    .bind(job_id)
    .execute(&harness.pool)
    .await
    .expect("backdate the acceptance time before settling");

    let (terminal_at, capture_at): (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>) =
        sqlx::query_as(
            "SELECT j.terminal_at, e.created_at
             FROM generation.jobs j
             JOIN ledger.entries e ON e.job_id = j.id AND e.kind = 'capture'
             WHERE j.id = $1",
        )
        .bind(job_id)
        .fetch_one(&harness.pool)
        .await
        .expect("terminal and capture times");
    assert_eq!(
        terminal_at, capture_at,
        "成功结算的终态时刻必须与 capture 的入账时刻同事务"
    );
    let capture = harness.captured_microusd(job_id).await;
    assert!(capture < 0, "夹具应当真的扣了一笔：{capture}");

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

    let today = chrono::Utc::now().date_naive();
    let midnight = |offset: i64| {
        let date = today - chrono::Duration::days(offset);
        let naive = date.and_hms_opt(0, 0, 0).expect("midnight");
        chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(naive, chrono::Utc)
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    };
    // 受理日是前天，结算日（终态时刻所在）是今天，`next_day` 是明天。
    let (request_day, settled_day, next_day) = (midnight(2), midnight(0), midnight(-1));

    let read_billing = |since: &str, until: &str| {
        let url = format!(
            "{}/v1/customer/billing?since={since}&until={until}",
            harness.base_url
        );
        let client = client.clone();
        let session = session.clone();
        async move {
            client
                .get(url)
                .bearer_auth(&session)
                .send()
                .await
                .expect("billing request")
                .json::<Value>()
                .await
                .expect("billing body")
        }
    };

    let request_day_billing = read_billing(&request_day, &settled_day).await;
    assert_eq!(
        request_day_billing["requests"],
        json!(0),
        "跨天结算的那一笔不该落在受理日：{request_day_billing}"
    );
    assert_eq!(request_day_billing["charged_points"], json!(0));
    assert!(
        request_day_billing["usage"].get("images").is_none(),
        "受理日没有已完成的图片请求，用量键缺省：{request_day_billing}"
    );

    let settled_day_billing = read_billing(&settled_day, &next_day).await;
    assert_eq!(
        settled_day_billing["requests"],
        json!(1),
        "跨天结算的那一笔必须落在结算日：{settled_day_billing}"
    );
    assert_eq!(
        settled_day_billing["usage"]["images"],
        json!(1),
        "结算日的那笔产出一张，随终态时刻入桶"
    );
    assert_eq!(
        settled_day_billing["charged_points"],
        json!(capture / 1_000),
        "结算日的扣费就是那笔 capture"
    );

    // 每日合计那一行：实收必须落在**结算日**，受理日那一行不受影响（A8）。受理时刻是这次结算
    // **之前**改的，所以"按受理日入桶"的实现会把这笔实收写到前天，下面第一处 `day` 断言就会失败。
    let daily_rows: Vec<(chrono::NaiveDate, i64)> = sqlx::query_as(
        "SELECT day, settled_microusd FROM ledger.daily_spend WHERE account_id = $1 ORDER BY day",
    )
    .bind(Uuid::parse_str(&account_id).expect("account id"))
    .fetch_all(&harness.pool)
    .await
    .expect("daily spend rows");
    assert_eq!(
        daily_rows.len(),
        1,
        "这个账户只有一笔结算，每日合计只该有一行：{daily_rows:?}"
    );
    assert_eq!(
        daily_rows[0].0, today,
        "实收必须入结算日那一行，而不是受理日：{daily_rows:?}"
    );
    assert_eq!(
        daily_rows[0].1, -capture,
        "结算日那一行的合计就是这笔实收（正数记实收）：{daily_rows:?}"
    );
    let request_day_total: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(settled_microusd), 0)::bigint FROM ledger.daily_spend
         WHERE account_id = $1 AND day = $2",
    )
    .bind(Uuid::parse_str(&account_id).expect("account id"))
    .bind(today - chrono::Duration::days(2))
    .fetch_one(&harness.pool)
    .await
    .expect("acceptance-day daily total");
    assert_eq!(request_day_total, 0, "受理日那一行不该被这笔跨天结算改到");

    let request_day_usage = client
        .get(format!(
            "{}/v1/customer/usage?since={request_day}&until={settled_day}&limit=100",
            harness.base_url
        ))
        .bearer_auth(&session)
        .send()
        .await
        .expect("usage request")
        .json::<Value>()
        .await
        .expect("usage body");
    assert_eq!(
        request_day_usage["count"],
        json!(0),
        "跨天结算的请求不该按受理时刻归到受理日：{request_day_usage}"
    );
    let settled_day_usage = client
        .get(format!(
            "{}/v1/customer/usage?since={settled_day}&until={next_day}&limit=100",
            harness.base_url
        ))
        .bearer_auth(&session)
        .send()
        .await
        .expect("usage request")
        .json::<Value>()
        .await
        .expect("usage body");
    assert_eq!(settled_day_usage["count"], json!(1));
    let row = &settled_day_usage["usage"][0];
    assert_eq!(row["status"], json!("completed"));
    assert_eq!(row["charged_points"], json!(capture / 1_000));
    let row_terminal_at = row["terminal_at"].as_str().expect("用量行要给出终态时刻");
    assert_eq!(
        chrono::DateTime::parse_from_rfc3339(row_terminal_at)
            .expect("时间要是 RFC 3339")
            .with_timezone(&chrono::Utc),
        terminal_at,
        "用量里的终态时刻必须与库里那一笔一致：{settled_day_usage}"
    );

    let combined = read_billing(&request_day, &next_day).await;
    assert_eq!(combined["requests"], json!(1));
    assert_eq!(combined["charged_points"], json!(capture / 1_000));

    harness.cleanup().await;
}

/// 未完成的 Job 出现在用量里（按受理时刻、`terminal_at` 为空），但不进账单的请求数与已扣费额。
///
/// 用一次"上游终态没有结果图"的执行把 Job 停在 `reconciliation_required`：它不是终态，`terminal_at`
/// 为空，正好走用量查询里 `terminal_at IS NULL` 那条按受理时刻过滤的分支（现在没有别的用例走它）。
/// 对客状态怎么收敛是控制台/身份那条线的事（#47 已记录），这里只钉"出现与不计入"。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn an_unfinished_job_shows_up_in_usage_but_not_in_billing() {
    let mut behaviour = UpstreamBehaviour::apimart();
    behaviour.terminal_without_images = true;
    let harness = Harness::start(behaviour).await;
    let client = Client::new();

    let account_id = harness.account_id.clone();
    let email = "unfinished-usage@example.com";
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

    // 一次执行：上游终态没有结果图 ⇒ Job 停在 `reconciliation_required`，不是终态、`terminal_at` 为空。
    let key = format!("unfinished-usage-{}", Uuid::new_v4());
    let (status, body) = harness
        .sync_json(
            "/v1/images/generations",
            &key,
            route_request(harness.model, "unfinished usage contract"),
        )
        .await;
    assert_ne!(status, StatusCode::OK, "这次执行不该成功：{body}");
    let (job_id, state) = harness.job(&key).await;
    assert_eq!(
        state, "reconciliation_required",
        "夹具必须留下一个未完成的 Job"
    );
    let (terminal_at, created_at): (
        Option<chrono::DateTime<chrono::Utc>>,
        chrono::DateTime<chrono::Utc>,
    ) = sqlx::query_as("SELECT terminal_at, created_at FROM generation.jobs WHERE id = $1")
        .bind(job_id)
        .fetch_one(&harness.pool)
        .await
        .expect("job times");
    assert!(terminal_at.is_none(), "对账态不是终态，终态时刻必须为空");

    // 夹具自检：这一笔没有 capture，账本上因此没有它的扣费。
    let charged: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(e.amount_microusd), 0)::bigint
         FROM ledger.entries e
         WHERE e.job_id = $1 AND e.kind = 'capture'",
    )
    .bind(job_id)
    .fetch_one(&harness.pool)
    .await
    .expect("job capture sum");
    assert_eq!(charged, 0, "未完成的 Job 不该有 capture");

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

    let stamp = |instant: chrono::DateTime<chrono::Utc>| {
        instant.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    };
    let since = stamp(created_at - chrono::Duration::minutes(1));
    let until = stamp(created_at + chrono::Duration::minutes(1));

    // 用量：窗口包住受理时刻，这一笔按受理时刻出现；终态时刻为空、扣费为 0。
    let usage = client
        .get(format!(
            "{}/v1/customer/usage?since={since}&until={until}&limit=100",
            harness.base_url
        ))
        .bearer_auth(&session)
        .send()
        .await
        .expect("usage request")
        .json::<Value>()
        .await
        .expect("usage body");
    assert_eq!(
        usage["count"],
        json!(1),
        "未完成的 Job 必须按受理时刻出现在用量里：{usage}"
    );
    let row = &usage["usage"][0];
    assert_eq!(
        row["terminal_at"],
        Value::Null,
        "未完成就没有终态时刻：{usage}"
    );
    // 结果还没定、仍需对账的请求，对客要显示"处理中"（Spec C9）：并进"未产出"会让客户以为这一笔
    // 已经失败，而它还可能补回成功。
    assert_eq!(
        row["status"],
        json!("pending"),
        "对账中的请求对客显示处理中：{usage}"
    );
    assert_eq!(row["charged_points"], json!(0), "没结算就没有扣费：{usage}");
    assert_eq!(row["type"], json!("image"), "{usage}");
    assert_eq!(row["usage"]["images"], json!(0), "没有交付结果图：{usage}");

    // 窗口挪到受理之后：`terminal_at IS NULL` 分支按受理时刻过滤，这一笔不该漏进来。
    let after = stamp(chrono::Utc::now() + chrono::Duration::hours(1));
    let later = stamp(chrono::Utc::now() + chrono::Duration::hours(2));
    let future_usage = client
        .get(format!(
            "{}/v1/customer/usage?since={after}&until={later}&limit=100",
            harness.base_url
        ))
        .bearer_auth(&session)
        .send()
        .await
        .expect("usage request")
        .json::<Value>()
        .await
        .expect("usage body");
    assert_eq!(
        future_usage["count"],
        json!(0),
        "处理中按受理时刻归属，受理之后的窗口不该有它：{future_usage}"
    );

    // 账单：未完成的 Job 不计入已完成请求数，也不增加已扣费额。**不传窗口**读——受理分支的
    // `terminal_at IS NOT NULL` 一旦被去掉，不传窗口时这个 Job 就会被算成一次已完成请求。
    let billing = client
        .get(format!("{}/v1/customer/billing", harness.base_url))
        .bearer_auth(&session)
        .send()
        .await
        .expect("billing request")
        .json::<Value>()
        .await
        .expect("billing body");
    assert_eq!(
        billing["requests"],
        json!(0),
        "处理中的 Job 不计入已完成请求数：{billing}"
    );
    assert!(
        billing["usage"].get("images").is_none(),
        "没有已完成的图片请求，用量键缺省：{billing}"
    );
    assert_eq!(
        billing["charged_points"],
        json!(0),
        "没结算就没有已扣费额：{billing}"
    );

    harness.cleanup().await;
}

/// 已完成用量的逐笔扣费等于实际扣费流水；正式调整单独列示后与账单净额相等（A8）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn usage_charges_match_the_capture_entries_and_adjustments_stay_separate() {
    let harness = Harness::start_with_draft(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;
    let client = Client::new();
    assert_eq!(
        republish_priced(&harness, &client, openai_floor_amounts(), 2_000).await,
        StatusCode::OK,
        "带定价的发布必须成功"
    );

    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let email = "capture-and-adjustment@example.com";
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

    let mut keys = Vec::new();
    for index in 0..2 {
        let key = format!("capture-sum-{index}-{}", Uuid::new_v4());
        let (status, body) = post_json(
            &harness.base_url,
            &api_key,
            "/v1/images/generations",
            &key,
            &route_request(harness.model, "capture sum contract"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "got {body}");
        keys.push(key);
    }

    // 真实路径上终态时刻与 `capture` 入账时刻由同一事务确定、逐位相等；这里把其中一笔**人为错开**
    // ——终态落在昨天，capture 落在前天。不这样错开，"逐笔扣费退回按流水入账时刻过滤"这条回归
    // 也能通过；错开之后，下面按结算日窗口读用量就能把回归逼出来（`0013` §5）。
    let (shifted_job_id, shifted_state) = harness.job(&keys[0]).await;
    assert_eq!(shifted_state, "succeeded", "错开时刻的那一笔必须已结算");
    let shifted_capture = harness.captured_microusd(shifted_job_id).await;
    assert!(
        shifted_capture < 0,
        "夹具应当真的扣了一笔：{shifted_capture}"
    );
    sqlx::query(
        "UPDATE generation.jobs
         SET created_at = ((date_trunc('day', now() AT TIME ZONE 'UTC') - interval '2 days')
                           + interval '23 hours 50 minutes') AT TIME ZONE 'UTC',
             terminal_at = ((date_trunc('day', now() AT TIME ZONE 'UTC') - interval '1 day')
                            + interval '10 minutes') AT TIME ZONE 'UTC'
         WHERE id = $1",
    )
    .bind(shifted_job_id)
    .execute(&harness.pool)
    .await
    .expect("shift the job across the UTC day boundary");
    sqlx::query(
        "UPDATE ledger.entries
         SET created_at = ((date_trunc('day', now() AT TIME ZONE 'UTC') - interval '2 days')
                           + interval '23 hours 50 minutes') AT TIME ZONE 'UTC'
         WHERE job_id = $1 AND kind = 'capture'",
    )
    .bind(shifted_job_id)
    .execute(&harness.pool)
    .await
    .expect("shift the capture away from the terminal time");

    let adjustment = -12_000_i64;
    sqlx::query(
        "INSERT INTO ledger.entries (id, account_id, job_id, kind, amount_microusd, business_key)
         VALUES ($1,$2,NULL,'adjustment',$3,$4)",
    )
    .bind(Uuid::new_v4())
    .bind(Uuid::parse_str(&account_id).expect("account id"))
    .bind(adjustment)
    .bind(format!("adjustment:{}", Uuid::new_v4()))
    .execute(&harness.pool)
    .await
    .expect("seed a formal adjustment");

    let captures: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(amount_microusd), 0)::bigint FROM ledger.entries
         WHERE account_id = $1 AND kind = 'capture'",
    )
    .bind(Uuid::parse_str(&account_id).expect("account id"))
    .fetch_one(&harness.pool)
    .await
    .expect("capture sum");
    assert!(captures < 0, "夹具应当真的扣了两笔：{captures}");

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

    let usage = client
        .get(format!("{}/v1/customer/usage?limit=100", harness.base_url))
        .bearer_auth(&session)
        .send()
        .await
        .expect("usage request")
        .json::<Value>()
        .await
        .expect("usage body");
    let usage_sum: i64 = usage["usage"]
        .as_array()
        .expect("usage array")
        .iter()
        .map(|row| row["charged_points"].as_i64().expect("charged"))
        .sum();
    assert_eq!(
        usage_sum,
        captures / 1_000,
        "已完成用量逐笔扣费合计必须等于实际扣费流水（积分 = 微单位 / 1000）：usage={usage}"
    );

    // 按**结算日**窗口读那一笔错开时刻的用量：它按终态时刻出现，逐笔扣费必须是这个 Job 的完整
    // capture——哪怕那笔 capture 的入账时刻已经落在窗口之外。按流水入账时刻过滤的实现这里会给 0。
    let today = chrono::Utc::now().date_naive();
    let midnight = |offset: i64| {
        let date = today - chrono::Duration::days(offset);
        let naive = date.and_hms_opt(0, 0, 0).expect("midnight");
        chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(naive, chrono::Utc)
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    };
    let (settled_day, next_day) = (midnight(1), midnight(0));
    let shifted_usage = client
        .get(format!(
            "{}/v1/customer/usage?since={settled_day}&until={next_day}&limit=100",
            harness.base_url
        ))
        .bearer_auth(&session)
        .send()
        .await
        .expect("usage request")
        .json::<Value>()
        .await
        .expect("usage body");
    assert_eq!(
        shifted_usage["count"],
        json!(1),
        "错开时刻的那一笔仍按终态时刻归属到结算日：{shifted_usage}"
    );
    assert_eq!(
        shifted_usage["usage"][0]["charged_points"],
        json!(shifted_capture / 1_000),
        "用量行必须返回该 Job 的完整 capture，不能按流水入账时刻过滤：{shifted_usage}"
    );

    let billing = client
        .get(format!("{}/v1/customer/billing", harness.base_url))
        .bearer_auth(&session)
        .send()
        .await
        .expect("billing request")
        .json::<Value>()
        .await
        .expect("billing body");
    assert_eq!(
        billing["charged_points"],
        json!((captures + adjustment) / 1_000),
        "账单净额 = 实际扣费 + 正式调整：billing={billing}"
    );

    let ledger = client
        .get(format!("{}/v1/customer/ledger?limit=100", harness.base_url))
        .bearer_auth(&session)
        .send()
        .await
        .expect("ledger request")
        .json::<Value>()
        .await
        .expect("ledger body");
    let entries = ledger["entries"].as_array().expect("entries");
    let adjustments: Vec<i64> = entries
        .iter()
        .filter(|entry| entry["kind"].as_str() == Some("adjustment"))
        .map(|entry| entry["amount_points"].as_i64().expect("amount"))
        .collect();
    assert_eq!(
        adjustments,
        vec![adjustment / 1_000],
        "正式调整必须单独列示：{ledger}"
    );
    let ledger_net: i64 = entries
        .iter()
        .filter(|entry| matches!(entry["kind"].as_str(), Some("capture") | Some("adjustment")))
        .map(|entry| entry["amount_points"].as_i64().expect("amount"))
        .sum();
    assert_eq!(
        json!(ledger_net),
        billing["charged_points"],
        "同一区间下扣费与调整之和等于账单净额：ledger={ledger} billing={billing}"
    );

    harness.cleanup().await;
}

/// 读侧遇到非整积分时按**绝对值**向上取整（Spec 0002 §1、A10）。
///
/// 绕过写入口直接改库，造出对客金额取值面（1000 微元的整数倍）之外的两种行：已结算余额差 1 微元（写入方漏
/// 取整留下的行），以及一条没有写入口、也不改余额的正式调整行（账实核对会发现它与余额不一致；读侧对它只
/// 取整，不改账）。把读侧的取整改成截断，两处断言都会失败。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_non_whole_point_amount_rounds_up_on_the_customer_read() {
    let harness = Harness::start_with_draft(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;
    let client = Client::new();

    let (account_id, _api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let account_uuid = Uuid::parse_str(&account_id).expect("account id");
    let email = "non-whole-points@example.com";
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

    // 整积分的账上金额原样给出：读侧的取整对合规行不动一个数。
    let account = client
        .get(format!("{}/v1/customer/account", harness.base_url))
        .bearer_auth(&session)
        .send()
        .await
        .expect("account request")
        .json::<Value>()
        .await
        .expect("account body");
    assert_eq!(account["balance_points"], json!(1_000), "{account}");

    // 差 1 微元不足 1 积分：向上取整到 1001（截断会给 1000）。
    sqlx::query("UPDATE ledger.accounts SET balance_microusd = balance_microusd + 1 WHERE id = $1")
        .bind(account_uuid)
        .execute(&harness.pool)
        .await
        .expect("make the settled balance a non-whole number of points");
    let account = client
        .get(format!("{}/v1/customer/account", harness.base_url))
        .bearer_auth(&session)
        .send()
        .await
        .expect("account request")
        .json::<Value>()
        .await
        .expect("account body");
    assert_eq!(
        account["balance_points"],
        json!(1_001),
        "1000001 微元向上取整到 1001 积分：{account}"
    );
    assert_eq!(account["held_points"], json!(0), "{account}");
    assert_eq!(account["available_points"], json!(1_001), "{account}");

    // 负数按绝对值向上取整：-1500 微元 → -2 积分（截断会给 -1）。
    sqlx::query(
        "INSERT INTO ledger.entries (id, account_id, job_id, kind, amount_microusd, business_key)
         VALUES ($1,$2,NULL,'adjustment',$3,$4)",
    )
    .bind(Uuid::new_v4())
    .bind(account_uuid)
    .bind(-1_500_i64)
    .bind(format!("non-whole-points:{}", Uuid::new_v4()))
    .execute(&harness.pool)
    .await
    .expect("seed a non-whole adjustment");
    let ledger = client
        .get(format!(
            "{}/v1/customer/ledger?kind=adjustment",
            harness.base_url
        ))
        .bearer_auth(&session)
        .send()
        .await
        .expect("ledger request")
        .json::<Value>()
        .await
        .expect("ledger body");
    let entries = ledger["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 1, "只该有那一条非整积分的调整：{ledger}");
    assert_eq!(
        entries[0]["amount_points"],
        json!(-2),
        "-1500 微元按绝对值向上取整到 -2 积分：{ledger}"
    );

    harness.cleanup().await;
}
