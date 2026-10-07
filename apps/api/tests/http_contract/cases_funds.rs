use super::*;

/// 占用是**同一次请求内**的中间态：上游压这么久，用例才有窗口观察到 `active` 的预授权。
const SLOW_UPSTREAM_MS: u64 = 3_000;

/// 对客同步窗口：要盖住上面那段上游延迟与结算预留，否则请求会先被总期限收成 504。
const SLOW_SYNC_WAIT_SECONDS: u64 = 30;

/// 账户读必须同时给出三项，且 `available = balance − held`（账户资金 Spec `0002` §4、A7）。
///
/// 期望值按 **CNY 微单位**给：管理端响应就是微单位；对客响应是积分（1积分 = 1000 微单位），
/// 这里按响应自带的字段名判定单位、换算后再比，调用方不必为两种单位各写一遍期望。
fn assert_account_triplet(what: &str, body: &Value, balance_microusd: i64, held_microusd: i64) {
    let points = body.get("balance_points").is_some();
    let (balance_field, held_field, available_field) = if points {
        ("balance_points", "held_points", "available_points")
    } else {
        ("balance_microusd", "held_microusd", "available_microusd")
    };
    let unit = |microusd: i64| if points { microusd / 1_000 } else { microusd };
    let balance = unit(balance_microusd);
    let held = unit(held_microusd);
    assert_eq!(
        body[balance_field].as_i64(),
        Some(balance),
        "{what}：{body}"
    );
    assert_eq!(body[held_field].as_i64(), Some(held), "{what}：{body}");
    let available = body[available_field]
        .as_i64()
        .unwrap_or_else(|| panic!("{what} 必须给 {available_field}：{body}"));
    assert_eq!(
        available,
        balance - held,
        "{what}：available = balance − held；{body}"
    );
    assert!(
        body["updated_at"].is_string(),
        "{what} 必须给 updated_at：{body}"
    );
}

/// 保底表：任何档位都是 60 微元，受理一次的预授权额因此是 60。
fn sixty_microusd_floor() -> Value {
    json!({"amounts": {"1K": 60, "2K": 60, "4K": 60}, "cap_microusd": 60})
}

/// 等到这个幂等键的 Job 落库、并且占用是 `active`；返回 Job 标识。
async fn wait_for_active_hold(harness: &Harness, key: &str) -> Uuid {
    for _ in 0..200 {
        let job: Option<Uuid> = sqlx::query_scalar(
            "SELECT j.id FROM generation.jobs j
             JOIN ledger.holds h ON h.job_id = j.id AND h.status = 'active'
             WHERE j.idempotency_key_digest = $1",
        )
        .bind(idempotency_key_digest(key))
        .fetch_optional(&harness.pool)
        .await
        .expect("active hold lookup");
        if let Some(job) = job {
            return job;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("受理后 10 秒内没有看到 {key} 的 active 占用");
}

/// 账户那一行里的占用合计（`ledger.accounts.held_microusd`）。
async fn held_in_db(harness: &Harness, account_id: &str) -> i64 {
    sqlx::query_scalar("SELECT held_microusd FROM ledger.accounts WHERE id = $1")
        .bind(Uuid::parse_str(account_id).expect("account id"))
        .fetch_one(&harness.pool)
        .await
        .expect("held")
}

/// 发一次请求并**等它把占用写进库**，返回那次同步请求的句柄；不跑 Worker。
///
/// 等的是库里的 `active` 占用而不是固定睡眠：这样"第二笔到达时第一笔已经占着"是观察到的事实，
/// 不是靠时间赌出来的。不跑 Worker 时句柄会等到同步窗口尽头回 `504`，用例可以 abort 掉。
async fn accept_without_worker(
    harness: &Harness,
    api_key: &str,
    key: &str,
    prompt: &str,
) -> tokio::task::JoinHandle<(StatusCode, Value)> {
    let request = tokio::spawn({
        let base_url = harness.base_url.clone();
        let api_key = api_key.to_owned();
        let key = key.to_owned();
        let body = route_request(harness.model, prompt);
        async move { post_json(&base_url, &api_key, "/v1/images/generations", &key, &body).await }
    });
    wait_for_active_hold(harness, key).await;
    request
}

/// 账户读接口在同一时点给出已结算余额、持有中与可用额，且 `available = balance − held`。
///
/// 管理员那条（`/api/v1/accounts/{id}`）与对客两条（`/v1/account`、`/v1/customer/account`）都用
/// 账户行的同一次读：三个数必须齐全、自洽，不能只回余额再让调用方自己算（`0002` §4、`0001` §6）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn account_reads_return_settled_balance_held_and_available_together() {
    // 直接执行里预授权是**同一次请求内**的中间态：上游慢下来，占用才有可观察的窗口。
    let harness = Harness::start_with_sync_wait(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        UpstreamBehaviour {
            delay_ms: SLOW_UPSTREAM_MS,
            ..UpstreamBehaviour::aihubmix(SyncImageShape::Url)
        },
        64,
        SLOW_SYNC_WAIT_SECONDS,
    )
    .await;
    let client = Client::new();
    assert_eq!(
        republish_priced(&harness, &client, sixty_microusd_floor(), 2_000).await,
        StatusCode::OK,
        "带定价的发布必须成功"
    );
    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000).await;

    // 一次受理（不跑 Worker）：占用落库、已结算余额不动。
    let key = format!("funds-read-{}", Uuid::new_v4());
    let request = accept_without_worker(&harness, &api_key, &key, "account read").await;

    let admin: Value = client
        .get(format!("{}/api/v1/accounts/{account_id}", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .send()
        .await
        .expect("admin account read")
        .json()
        .await
        .expect("admin account body");
    assert_account_triplet("管理员账户读", &admin, 1_000, 1_000);

    let own: Value = client
        .get(format!("{}/v1/account", harness.base_url))
        .bearer_auth(&api_key)
        .send()
        .await
        .expect("own account read")
        .json()
        .await
        .expect("own account body");
    assert_account_triplet("对客 Key 账户读", &own, 1_000, 1_000);

    // 客户会话读：先给这个账户配上登录身份，再登录拿会话。
    let email = format!("funds-{}@example.com", Uuid::new_v4());
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
    let customer: Value = client
        .get(format!("{}/v1/customer/account", harness.base_url))
        .bearer_auth(&session)
        .send()
        .await
        .expect("customer account read")
        .json()
        .await
        .expect("customer account body");
    assert_account_triplet("客户会话账户读", &customer, 1_000, 1_000);

    request.abort();
    harness.cleanup().await;
}

/// 同一账户的并发受理共同遵守同一可用额：余额 1000（1积分）、两笔各占 1000，最多一笔成立（A2）。
///
/// 保底额 60 落账前向上取整到 1000（1积分），所以两笔各占 1000。第二笔必须由数据库的条件更新
/// 拒绝——若受理只比余额、不看已有占用，两笔会同时成立，库里就会留下 2000 的占用而余额只有 1000。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn concurrent_reservations_cannot_both_occupy_the_same_available_amount() {
    // 直接执行里预授权是**同一次请求内**的中间态：上游慢下来，占用才有可观察的窗口。
    let harness = Harness::start_with_sync_wait(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        UpstreamBehaviour {
            delay_ms: SLOW_UPSTREAM_MS,
            ..UpstreamBehaviour::aihubmix(SyncImageShape::Url)
        },
        64,
        SLOW_SYNC_WAIT_SECONDS,
    )
    .await;
    let client = Client::new();
    assert_eq!(
        republish_priced(&harness, &client, sixty_microusd_floor(), 2_000).await,
        StatusCode::OK
    );
    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000).await;

    let first_key = format!("funds-concurrent-a-{}", Uuid::new_v4());
    let first = accept_without_worker(&harness, &api_key, &first_key, "first reservation").await;

    // 第二笔：不同幂等键、同样 60；受理瞬间可用额只剩 40。
    let second_key = format!("funds-concurrent-b-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &second_key,
        &route_request(harness.model, "second reservation"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::PAYMENT_REQUIRED,
        "第二笔必须因可用额不足被拒：{body}"
    );
    assert_eq!(
        body["error"]["code"].as_str(),
        Some("insufficient_balance"),
        "{body}"
    );

    // 库里只有一笔占用；已结算余额没有被预授权动过。
    assert_eq!(
        database_balance(&harness, &account_id).await,
        1_000,
        "预授权不改已结算余额"
    );
    assert_eq!(
        held_in_db(&harness, &account_id).await,
        1_000,
        "只有一笔 1000（整积分）的占用成立"
    );
    let jobs: i64 =
        sqlx::query_scalar("SELECT count(*) FROM generation.jobs WHERE account_id = $1")
            .bind(Uuid::parse_str(&account_id).expect("account id"))
            .fetch_one(&harness.pool)
            .await
            .expect("job count");
    assert_eq!(jobs, 1, "第二笔没有建 Job");

    let (first_status, first_body) = first.await.expect("first request");
    assert_eq!(first_status, StatusCode::OK, "第一笔自己跑完：{first_body}");
    assert_sync_success("慢上游上的第一笔", &first_body);
    harness.cleanup().await;
}

/// 同一幂等键的重发返回原请求，不再次占用（A2）。
///
/// 余额故意给足（2000）：若重发被当成新请求，它会**成功**再占 1000，库里因此留下两条 Job 与
/// 2000 的占用；只有真正去重成原来那条 Job，才会仍然是一条、1000。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_replayed_idempotency_key_does_not_reserve_again() {
    // 直接执行里预授权是**同一次请求内**的中间态：上游慢下来，占用才有可观察的窗口。
    let harness = Harness::start_with_sync_wait(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        UpstreamBehaviour {
            delay_ms: SLOW_UPSTREAM_MS,
            ..UpstreamBehaviour::aihubmix(SyncImageShape::Url)
        },
        64,
        SLOW_SYNC_WAIT_SECONDS,
    )
    .await;
    let client = Client::new();
    assert_eq!(
        republish_priced(&harness, &client, sixty_microusd_floor(), 2_000).await,
        StatusCode::OK
    );
    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 2_000).await;

    let key = format!("funds-replay-{}", Uuid::new_v4());
    let first = accept_without_worker(&harness, &api_key, &key, "replay reservation").await;

    // 重发同一幂等键：去重成原来那条 Job，再等同一个同步窗口。
    let (replayed, replayed_body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &route_request(harness.model, "replay reservation"),
    )
    .await;
    assert_ne!(
        replayed,
        StatusCode::PAYMENT_REQUIRED,
        "重发不是新请求，不该撞余额闸门：{replayed_body}"
    );
    assert_eq!(
        held_in_db(&harness, &account_id).await,
        1_000,
        "重发不重复占用"
    );
    let jobs: i64 =
        sqlx::query_scalar("SELECT count(*) FROM generation.jobs WHERE account_id = $1")
            .bind(Uuid::parse_str(&account_id).expect("account id"))
            .fetch_one(&harness.pool)
            .await
            .expect("job count");
    assert_eq!(jobs, 1, "重发不建第二条 Job");
    let holds: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ledger.holds WHERE account_id = $1 AND status = 'active'",
    )
    .bind(Uuid::parse_str(&account_id).expect("account id"))
    .fetch_one(&harness.pool)
    .await
    .expect("hold count");
    assert_eq!(holds, 1, "重发不新增占用行");

    first.abort();
    harness.cleanup().await;
}

/// 零实收不写 `capture` 流水：结算成功但一分钱没扣时，账上不出现零金额收支（A5）。
///
/// 对客费率全 0，实收因此是 0。若结算仍按 0 写一条 `capture`，这条用例就会看到流水里多出
/// 一条零金额记录；合同要的是"没有资金变动就没有流水"。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_settlement_with_zero_charge_writes_no_capture_entry() {
    // 上游这次声明 0：这条供给按声明金额加价，实收因此是 0，结算不该留下任何流水。
    let mut behaviour = UpstreamBehaviour::aihubmix(SyncImageShape::Url);
    behaviour.declared_cost = Some(json!(0));
    let harness = Harness::start_with(
        "AIHubMix",
        "aihubmix-image-v1",
        &["prompt_only"],
        None,
        behaviour,
        64,
    )
    .await;
    let client = Client::new();
    assert_eq!(
        republish_priced(&harness, &client, sixty_microusd_floor(), 2_000).await,
        StatusCode::OK,
        "声明金额计价也是合法发布"
    );
    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000).await;
    let key = format!("funds-zero-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &route_request(harness.model, "zero charge"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "零实收不影响这次请求成功：{body}");
    assert_sync_success("零实收结算", &body);
    let (job_id, state) = harness.job(&key).await;
    assert_eq!(state, "succeeded", "结算要走到成功终态");

    // 零实收不得留下**扣费**流水；受理时的预授权是另一回事，由持仓用例管。
    let captures: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ledger.entries WHERE job_id = $1 AND kind = 'capture'",
    )
    .bind(job_id)
    .fetch_one(&harness.pool)
    .await
    .expect("job captures");
    assert_eq!(captures, 0, "零实收不得留下任何扣费流水");
    assert_eq!(
        database_balance(&harness, &account_id).await,
        1_000,
        "零实收不改已结算余额"
    );
    assert_eq!(
        held_in_db(&harness, &account_id).await,
        0,
        "结算把占用全部解除"
    );
    let credits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ledger.entries WHERE account_id = $1 AND kind = 'credit'",
    )
    .bind(Uuid::parse_str(&account_id).expect("account id"))
    .fetch_one(&harness.pool)
    .await
    .expect("credit count");
    assert_eq!(credits, 1, "账上只有开户那笔充值");
    harness.cleanup().await;
}

/// 实收低于预授权额时，未花的部分只恢复可用额、不产生退款流水（`0002` §2.3、A1）。
///
/// 2K 的预授权额是 250000，实收 43680 取整到 44000：结算后占用清零、已结算余额只减实收；
/// 账上只有充值与实收两条，没有把差额退回来的调整分录。可用额从 `余额 − 250000` 回到 `余额 − 44000`。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_charge_below_the_authorized_hold_only_restores_available_without_a_refund() {
    let harness = Harness::start_with(
        "AIHubMix",
        "aihubmix-image-v1",
        &["prompt_only"],
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
    let key = format!("funds-under-hold-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "charge below hold");
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
    assert_eq!(state, "succeeded");

    let hold: i64 =
        sqlx::query_scalar("SELECT amount_microusd FROM ledger.holds WHERE job_id = $1")
            .bind(job_id)
            .fetch_one(&harness.pool)
            .await
            .expect("hold");
    let charge = -harness.captured_microusd(job_id).await;
    assert!(
        charge > 0 && charge < hold,
        "这条用例要的是实收低于预授权额：hold={hold}, charge={charge}"
    );

    // 结算之后：占用清零、余额只减实收。
    assert_eq!(held_in_db(&harness, &account_id).await, 0);
    assert_eq!(
        database_balance(&harness, &account_id).await,
        1_000_000 - charge
    );
    let kinds: Vec<String> =
        sqlx::query_scalar("SELECT kind FROM ledger.entries WHERE account_id = $1 ORDER BY kind")
            .bind(Uuid::parse_str(&account_id).expect("account id"))
            .fetch_all(&harness.pool)
            .await
            .expect("entry kinds");
    assert_eq!(
        kinds,
        vec!["capture".to_owned(), "credit".to_owned()],
        "只该有充值 + 实收，差额不退成调整分录"
    );

    // 对客读把可用额给出来：占用清掉之后 available 就等于余额。
    let own: Value = client
        .get(format!("{}/v1/account", harness.base_url))
        .bearer_auth(&api_key)
        .send()
        .await
        .expect("own account read")
        .json()
        .await
        .expect("own account body");
    assert_eq!(
        own["available_points"].as_i64(),
        Some((1_000_000 - charge) / 1_000),
        "{own}"
    );
    harness.cleanup().await;
}
