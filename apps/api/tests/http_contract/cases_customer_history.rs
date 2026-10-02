//! 客户历史浏览：调用记录与资金流水的**日期区间、类别、连续翻页与游标**。
//!
//! 覆盖 Spec `0001` v15 的 C8–C10、V-C15 与设计 `0014` §2–§3、§7：游标翻页不重不漏、类别筛选、
//! 错误游标按参数错误拒、区间半开且逐笔与汇总口径一致、旧参数与旧响应字段仍然可用。

use super::*;

/// 一套"已结束历史多于一页"的夹具：定价过的候选、有余额的账户、客户登录身份与一个 Worker。
async fn history_harness() -> Harness {
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
    harness
}

/// 开户 + 登录，返回客户会话令牌。
async fn customer_session(
    client: &Client,
    harness: &Harness,
    email: &str,
    account_id: &str,
) -> String {
    let opened = client
        .post(format!("{}/api/v1/customers", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .json(&json!({
            "email": email,
            "password": "a-long-enough-password",
            "account_id": account_id,
        }))
        .send()
        .await
        .expect("open customer request");
    assert_eq!(opened.status(), StatusCode::CREATED);
    client
        .post(format!("{}/v1/customer/sessions", harness.base_url))
        .json(&json!({"email": email, "password": "a-long-enough-password"}))
        .send()
        .await
        .expect("customer login request")
        .json::<Value>()
        .await
        .expect("login body")["token"]
        .as_str()
        .expect("session token")
        .to_owned()
}

/// 跑一笔真实生成（Worker 在同步窗口内做到终态），返回这次请求的扣费额。
async fn settle_one_generation(harness: &Harness, api_key: &str, prompt: &str) -> i64 {
    let key = format!("history-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        api_key,
        "/v1/images/generations",
        &key,
        &route_request(harness.model, prompt),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let (job_id, state, _) = harness.job(&key).await;
    assert_eq!(state, "succeeded", "这一笔必须跑到终态");
    sqlx::query_scalar(
        "SELECT COALESCE(SUM(amount_microusd), 0)::bigint FROM ledger.entries
         WHERE job_id = $1 AND kind = 'capture'",
    )
    .bind(job_id)
    .fetch_one(&harness.pool)
    .await
    .expect("capture sum")
}

async fn get_json(client: &Client, url: String, session: &str) -> (StatusCode, Value) {
    let response = client
        .get(url)
        .bearer_auth(session)
        .send()
        .await
        .expect("customer history request");
    let status = response.status();
    let body = response.json::<Value>().await.unwrap_or(Value::Null);
    (status, body)
}

/// 已结束历史用游标连续翻页：**无重复、无漏项**，最后一页的 `next_cursor` 为 `null`。
///
/// 处理中与已结束分开：`view=active` 这时是空的，`view=completed` 才看得到这几笔；不带 `view` 的旧
/// 调用仍然给出合并视图。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn completed_usage_pages_by_cursor_without_gaps_or_repeats() {
    let harness = history_harness().await;
    let client = Client::new();
    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let session =
        customer_session(&client, &harness, "history-usage@example.com", &account_id).await;

    let _worker = harness.spawn_worker();
    for prompt in ["first", "second", "third"] {
        settle_one_generation(&harness, &api_key, prompt).await;
    }

    // 处理中：没有。已结束：三笔。
    let (status, active) = get_json(
        &client,
        format!("{}/v1/customer/usage?view=active", harness.base_url),
        &session,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{active}");
    assert_eq!(
        active["usage"].as_array().expect("usage").len(),
        0,
        "三笔都跑到了终态，处理中该是空的：{active}"
    );
    assert_eq!(active["next_cursor"], Value::Null);

    // 第一页：limit=2，多出来那一行只用来判断"还有下一页"，不进响应。
    let (status, first) = get_json(
        &client,
        format!(
            "{}/v1/customer/usage?view=completed&limit=2",
            harness.base_url
        ),
        &session,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let rows = first["usage"].as_array().expect("usage");
    assert_eq!(rows.len(), 2, "一页两行：{first}");
    assert_eq!(first["count"], json!(2));
    assert_eq!(first["truncated"], json!(true), "还有下一页：{first}");
    let cursor = first["next_cursor"]
        .as_str()
        .expect("第一页之后必须给出游标")
        .to_owned();

    // 第二页：只剩一行，游标为 null。
    let (status, second) = get_json(
        &client,
        format!(
            "{}/v1/customer/usage?view=completed&limit=2&cursor={cursor}",
            harness.base_url
        ),
        &session,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    let second_rows = second["usage"].as_array().expect("usage");
    assert_eq!(second_rows.len(), 1, "第二页只剩一行：{second}");
    assert_eq!(second["next_cursor"], Value::Null, "没有下一页了：{second}");
    assert_eq!(second["truncated"], json!(false));

    // 两页合起来**正好是那三笔**：既没有重复，也没有漏。
    let mut seen: Vec<String> = rows
        .iter()
        .chain(second_rows.iter())
        .map(|row| row["created_at"].as_str().expect("created_at").to_owned())
        .collect();
    seen.sort();
    let unique = seen.len();
    seen.dedup();
    assert_eq!(unique, seen.len(), "两页之间不该有重复：{first} / {second}");
    let expected: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM generation.jobs WHERE account_id = $1")
            .bind(Uuid::parse_str(&account_id).expect("account id"))
            .fetch_one(&harness.pool)
            .await
            .expect("job count");
    assert_eq!(
        seen.len() as i64,
        expected,
        "两页合起来要覆盖全部已结束请求"
    );

    // 已结束历史按终态时刻倒序：终态时刻必须逐行给出。
    let times: Vec<&str> = rows
        .iter()
        .chain(second_rows.iter())
        .map(|row| row["terminal_at"].as_str().expect("terminal_at"))
        .collect();
    let mut sorted = times.clone();
    sorted.sort_by(|left, right| right.cmp(left));
    assert_eq!(
        times, sorted,
        "已结束历史按终态时刻倒序：{first} / {second}"
    );

    // 旧调用（不带 `view`）仍然给出合并视图，且响应里多了 `next_cursor` 但值可以为 null。
    let (status, merged) = get_json(
        &client,
        format!("{}/v1/customer/usage?limit=100", harness.base_url),
        &session,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{merged}");
    assert_eq!(merged["usage"].as_array().expect("usage").len(), 3);
    assert_eq!(merged["count"], json!(3));
    assert_eq!(merged["truncated"], json!(false));

    harness.cleanup().await;
}

/// 资金流水按 `(created_at, id)` 游标翻页、可按类别筛选；未知类别与错误游标都是参数错误。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn ledger_pages_by_cursor_and_filters_by_kind() {
    let harness = history_harness().await;
    let client = Client::new();
    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let session =
        customer_session(&client, &harness, "history-ledger@example.com", &account_id).await;

    // 第二笔充值：这样 `credit` 与 `capture` 都有多条，类别筛选才验得出来。
    let credited = client
        .post(format!(
            "{}/api/v1/accounts/{account_id}/credits",
            harness.base_url
        ))
        .bearer_auth(&harness.admin_token)
        .json(&json!({"amount_microusd": 500_000, "business_key": format!("history-{}", Uuid::new_v4())}))
        .send()
        .await
        .expect("admin credit");
    assert!(credited.status().is_success(), "{}", credited.status());

    let _worker = harness.spawn_worker();
    settle_one_generation(&harness, &api_key, "ledger first").await;
    settle_one_generation(&harness, &api_key, "ledger second").await;

    // 逐页翻完，条数必须等于 `total`，且没有重复。
    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    let total: u64 = loop {
        let url = match &cursor {
            Some(token) => format!(
                "{}/v1/customer/ledger?limit=2&cursor={token}",
                harness.base_url
            ),
            None => format!("{}/v1/customer/ledger?limit=2", harness.base_url),
        };
        let (status, page) = get_json(&client, url, &session).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        for entry in page["entries"].as_array().expect("entries") {
            seen.push(format!(
                "{}|{}",
                entry["created_at"].as_str().expect("created_at"),
                entry["amount_microusd"]
            ));
        }
        pages += 1;
        assert!(pages <= 10, "翻页不该停不下来：{page}");
        match page["next_cursor"].as_str() {
            Some(token) => cursor = Some(token.to_owned()),
            None => break page["total"].as_u64().expect("total"),
        }
    };
    assert_eq!(
        seen.len() as u64,
        total,
        "翻完所有页拿到的条数必须等于 total"
    );
    let unique = {
        let mut copy = seen.clone();
        copy.sort();
        copy.dedup();
        copy.len()
    };
    assert_eq!(unique, seen.len(), "翻页之间不该重复");

    // 类别筛选：只看充值。
    let (status, credits) = get_json(
        &client,
        format!("{}/v1/customer/ledger?kind=credit", harness.base_url),
        &session,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{credits}");
    let entries = credits["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 2, "两笔充值：{credits}");
    assert_eq!(credits["total"], json!(2));
    assert!(
        entries.iter().all(|entry| entry["kind"] == json!("credit")),
        "只该有充值：{credits}"
    );

    // 平台成本不是客户的事实；未知类别与错误游标都是参数错误。
    for query in ["kind=cost", "kind=bogus"] {
        let (status, body) = get_json(
            &client,
            format!("{}/v1/customer/ledger?{query}", harness.base_url),
            &session,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}：{body}");
    }
    let (status, body) = get_json(
        &client,
        format!("{}/v1/customer/ledger?cursor=not-a-token", harness.base_url),
        &session,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    harness.cleanup().await;
}

/// 游标只在**同一账户、同一流、同一筛选面**下有效：串用一律参数错误，不静默从首页重查。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_cursor_from_another_query_or_account_is_rejected() {
    let harness = history_harness().await;
    let client = Client::new();
    let (first_account, first_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let (second_account, _) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let first = customer_session(&client, &harness, "history-a@example.com", &first_account).await;
    let second =
        customer_session(&client, &harness, "history-b@example.com", &second_account).await;

    let _worker = harness.spawn_worker();
    for prompt in ["a first", "a second"] {
        settle_one_generation(&harness, &first_key, prompt).await;
    }

    let (status, page) = get_json(
        &client,
        format!(
            "{}/v1/customer/usage?view=completed&limit=1",
            harness.base_url
        ),
        &first,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let cursor = page["next_cursor"]
        .as_str()
        .expect("两笔已结束请求、limit=1，必须给出游标")
        .to_owned();

    // 换一个账户用同一条游标：拒绝。
    let (status, body) = get_json(
        &client,
        format!(
            "{}/v1/customer/usage?view=completed&limit=1&cursor={cursor}",
            harness.base_url
        ),
        &second,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "别的账户不能用这条游标：{body}"
    );

    // 同一个账户、换了筛选条件（这里换 `since`）：拒绝。
    let stamp = chrono::Utc::now() - chrono::Duration::days(1);
    let (status, body) = get_json(
        &client,
        format!(
            "{}/v1/customer/usage?view=completed&limit=1&since={}&cursor={cursor}",
            harness.base_url,
            stamp.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        ),
        &first,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "换了区间就不能复用游标：{body}"
    );

    // 处理中不承担稳定历史：带游标是参数错误。
    let (status, body) = get_json(
        &client,
        format!(
            "{}/v1/customer/usage?view=active&cursor={cursor}",
            harness.base_url
        ),
        &first,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // 篡改过的游标：拒绝。
    let mut tampered = cursor.clone();
    tampered.push('A');
    let (status, body) = get_json(
        &client,
        format!(
            "{}/v1/customer/usage?view=completed&limit=1&cursor={tampered}",
            harness.base_url
        ),
        &first,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    harness.cleanup().await;
}

/// 区间下界那一笔**既进逐笔、也进汇总**：半开区间 `[since, until)` 两条读同一条口径。
///
/// 这是 V-C8「明细与汇总对得上」在边界上的那一笔：一边算进去、另一边不算，逐笔求和就不等于汇总。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_window_lower_bound_counts_in_both_detail_and_summary() {
    let harness = history_harness().await;
    let client = Client::new();
    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let session =
        customer_session(&client, &harness, "history-edge@example.com", &account_id).await;

    let _worker = harness.spawn_worker();
    let charged = settle_one_generation(&harness, &api_key, "edge").await;
    assert!(charged < 0, "扣费是负数：{charged}");

    let capture_at: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
        "SELECT created_at FROM ledger.entries WHERE account_id = $1 AND kind = 'capture'",
    )
    .bind(Uuid::parse_str(&account_id).expect("account id"))
    .fetch_one(&harness.pool)
    .await
    .expect("capture created_at");
    let since = capture_at.to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
    let until = (capture_at + chrono::Duration::hours(1))
        .to_rfc3339_opts(chrono::SecondsFormat::Micros, true);

    // 逐笔：下界那一笔在。
    let (status, detail) = get_json(
        &client,
        format!(
            "{}/v1/customer/ledger?since={since}&until={until}&kind=capture",
            harness.base_url
        ),
        &session,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    let entries = detail["entries"].as_array().expect("entries");
    assert_eq!(
        entries.len(),
        1,
        "下界那一笔必须算进逐笔（半开区间含下界）：{detail}"
    );
    assert_eq!(entries[0]["amount_microusd"], json!(charged));
    assert_eq!(detail["total"], json!(1));

    // 汇总：同一区间把同一笔算进净额。
    let (status, summary) = get_json(
        &client,
        format!(
            "{}/v1/customer/billing?since={since}&until={until}",
            harness.base_url
        ),
        &session,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{summary}");
    assert_eq!(
        summary["charged_microusd"],
        json!(charged),
        "同一区间下逐笔与汇总必须对得上：{summary} vs {detail}"
    );
    assert_eq!(summary["requests"], json!(1));

    harness.cleanup().await;
}

/// 游标密钥是**必填**：缺失或不是 32 字节的 base64 时 API 起不来，而且报错点名那个配置。
///
/// 这条对应设计 `0014` §2 的"密钥缺失或格式无效时启动失败并点名配置"：翻页游标是加密载荷，没有密钥
/// 就既发不出也解不开；静默降级会变成"翻页偶发 400"，比起不来更难查。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_cursor_key_is_required_and_named_when_it_is_wrong() {
    let database_url = std::env::var("HTTP_CONTRACT_DATABASE_URL")
        .expect("HTTP_CONTRACT_DATABASE_URL must point at an empty database");

    for key in [
        None,
        Some("not-base64-at-all"),
        // 合法 base64、但不是 32 字节。
        Some("c2hvcnQ="),
    ] {
        let (running, stderr) = probe_api_startup_with_cursor_key(&database_url, key).await;
        assert!(!running, "密钥取 {key:?} 时 API 不该起来：{stderr}");
        assert!(
            stderr.contains("CUSTOMER_HISTORY_CURSOR_KEY"),
            "报错必须点名配置，否则运维不知道要配什么：{stderr}"
        );
    }

    // 合法密钥必须起得来：否则上面三条断言可能只是因为别的原因起不来。
    let (running, stderr) =
        probe_api_startup_with_cursor_key(&database_url, Some(CONTRACT_CURSOR_KEY)).await;
    assert!(running, "合法密钥时 API 必须起来：{stderr}");
}

/// 已结束历史按**终态时刻**归属日期区间：结算在昨天的那一笔落在昨天那段区间里。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn completed_usage_is_attributed_to_the_terminal_day() {
    let harness = history_harness().await;
    let client = Client::new();
    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let session = customer_session(
        &client,
        &harness,
        "history-crossday@example.com",
        &account_id,
    )
    .await;

    let _worker = harness.spawn_worker();
    settle_one_generation(&harness, &api_key, "cross day").await;

    // 把这一笔的终态时刻挪到**昨天**：受理时刻不动，所以它只可能出现在按终态时刻取的区间里。
    let yesterday = chrono::Utc::now() - chrono::Duration::days(1);
    let moved = sqlx::query("UPDATE generation.jobs SET terminal_at = $2 WHERE account_id = $1")
        .bind(Uuid::parse_str(&account_id).expect("account id"))
        .bind(yesterday)
        .execute(&harness.pool)
        .await
        .expect("move terminal_at");
    assert_eq!(moved.rows_affected(), 1, "必须真的挪动了那一笔");

    let day = |offset_hours: i64| {
        (yesterday + chrono::Duration::hours(offset_hours))
            .to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
    };
    let (status, inside) = get_json(
        &client,
        format!(
            "{}/v1/customer/usage?view=completed&since={}&until={}",
            harness.base_url,
            day(-1),
            day(1)
        ),
        &session,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{inside}");
    assert_eq!(
        inside["usage"].as_array().expect("usage").len(),
        1,
        "结算在昨天的请求要落在昨天那段区间里：{inside}"
    );

    let (status, outside) = get_json(
        &client,
        format!(
            "{}/v1/customer/usage?view=completed&since={}&until={}",
            harness.base_url,
            day(1),
            day(25)
        ),
        &session,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{outside}");
    assert_eq!(
        outside["usage"].as_array().expect("usage").len(),
        0,
        "区间之外不该出现：{outside}"
    );

    harness.cleanup().await;
}

/// 区间参与游标：**同一区间**连续翻页不重不漏，换了区间再用旧游标就是参数错误。
///
/// V-C15 要的"按同一日期区间连续翻页"就是这条：区间钉在游标里，翻页时不重算。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn finished_requests_page_within_a_pinned_window() {
    let harness = history_harness().await;
    let client = Client::new();
    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let session =
        customer_session(&client, &harness, "history-window@example.com", &account_id).await;

    let _worker = harness.spawn_worker();
    for prompt in ["window first", "window second", "window third"] {
        settle_one_generation(&harness, &api_key, prompt).await;
    }

    let since = (chrono::Utc::now() - chrono::Duration::hours(1))
        .to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
    let until = (chrono::Utc::now() + chrono::Duration::hours(1))
        .to_rfc3339_opts(chrono::SecondsFormat::Micros, true);

    let (status, first) = get_json(
        &client,
        format!(
            "{}/v1/customer/usage?view=completed&limit=2&since={since}&until={until}",
            harness.base_url
        ),
        &session,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["usage"].as_array().expect("usage").len(), 2);
    let cursor = first["next_cursor"]
        .as_str()
        .expect("三笔里取两笔，必须给出游标")
        .to_owned();

    let (status, second) = get_json(
        &client,
        format!(
            "{}/v1/customer/usage?view=completed&limit=2&since={since}&until={until}&cursor={cursor}",
            harness.base_url
        ),
        &session,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["usage"].as_array().expect("usage").len(), 1);
    assert_eq!(second["next_cursor"], Value::Null);

    // 同一账户、换了区间却还用这条游标：拒绝，而不是拿它当"从头开始"。
    let (status, body) = get_json(
        &client,
        format!(
            "{}/v1/customer/usage?view=completed&limit=2&since={}&until={until}&cursor={cursor}",
            harness.base_url,
            (chrono::Utc::now() - chrono::Duration::hours(2))
                .to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
        ),
        &session,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    harness.cleanup().await;
}

/// 资金流水只认真实收支：**平台成本 `cost` 在任何筛选下都不出现**，也不进 `total`。
///
/// 账本上的 `cost` 记在平台账户名下（迁移 `0019`）；预授权与释放根本不在 `ledger.entries` 里
/// （迁移 `0024` 已把这两类从该表清掉并收窄了约束）。所以对客这条读要挡的是"平台成本混进来"——
/// 用例**直接往这个客户的账上塞一行 `cost`**，再确认对客看不见、管理员那条读看得见。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_customer_ledger_hides_the_platform_cost_line() {
    let harness = history_harness().await;
    let client = Client::new();
    let (account_id, _) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let account_uuid = Uuid::parse_str(&account_id).expect("account id");
    let session =
        customer_session(&client, &harness, "history-costs@example.com", &account_id).await;

    sqlx::query(
        "INSERT INTO ledger.entries (id, account_id, kind, amount_microusd, business_key)
         VALUES ($1, $2, 'cost', $3, $4)",
    )
    .bind(Uuid::new_v4())
    .bind(account_uuid)
    .bind(-11_354_i64)
    .bind(format!("history-excluded-cost-{}", Uuid::new_v4()))
    .execute(&harness.pool)
    .await
    .expect("insert platform cost entry");

    // 管理员那条流水读看得到它：证明这一行真的在库里，不是"没插进去"。
    let admin = client
        .get(format!(
            "{}/api/v1/accounts/{account_id}/entries?limit=100",
            harness.base_url
        ))
        .bearer_auth(&harness.admin_token)
        .send()
        .await
        .expect("admin entries request")
        .json::<Value>()
        .await
        .expect("admin entries body");
    let admin_kinds = admin["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|entry| entry["kind"].as_str().expect("kind").to_owned())
        .collect::<Vec<_>>();
    assert!(
        admin_kinds.iter().any(|kind| kind == "cost"),
        "夹具必须真的插进了平台成本行：{admin}"
    );

    // 对客：无论怎么筛都只有真实收支，`total` 也不把它算进去。
    for query in ["", "?kind=credit", "?kind=capture", "?kind=adjustment"] {
        let (status, body) = get_json(
            &client,
            format!("{}/v1/customer/ledger{query}", harness.base_url),
            &session,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{query}：{body}");
        for entry in body["entries"].as_array().expect("entries") {
            let kind = entry["kind"].as_str().expect("kind");
            assert_ne!(
                kind, "cost",
                "平台成本不是客户的资金记录（{query}）：{body}"
            );
        }
        assert_eq!(
            body["total"].as_u64().expect("total"),
            body["entries"].as_array().expect("entries").len() as u64,
            "被排除的行不该算进 total（{query}）：{body}"
        );
    }

    // `cost` 连"可筛的类别"都不是：问它就该被拒，而不是回一批空数据让它以为"这个类别没有记录"。
    let (status, body) = get_json(
        &client,
        format!("{}/v1/customer/ledger?kind=cost", harness.base_url),
        &session,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    harness.cleanup().await;
}
