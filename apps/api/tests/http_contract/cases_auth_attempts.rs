//! 公开鉴权端点（注册、登录、重置码兑换）的失败尝试限制（Spec 0004 §1 S5、§5 A11）。
//!
//! 只有把上限压到个位数、并让来源维采信 `x-real-ip`，用例才能在几步之内分别构造出"同一身份
//! 换来源"与"同一来源换身份"。假 Redis 提供计数落点；无缓存放行那条单起一个不配缓存的进程。

use super::*;
use serde_json::json;

/// 公开鉴权尝试的计数窗口与"整段序列要跑完"的余量。
///
/// 窗口是运维取值（60 秒就是缺省值）；用例压小的是失败上限，几步就能触限。余量要盖住整条序列：
/// CI 运行 37553143246 的日志里，用这个窗口的几条用例最长约 3.5 秒（含起进程），余量取 10 秒；
/// 不够时 [`wait_for_window_margin`] 先等到下一个窗口开头。
const WINDOW_MS: u64 = 60_000;
const WINDOW_MARGIN_MS: u64 = 10_000;

/// 起一个带假 Redis、失败上限压小的 API：来源维采信 `x-real-ip`。
async fn bounded_api(
    database_url: &str,
    failures: u64,
    window_ms: u64,
) -> (String, String, ApiProcess) {
    start_api_with_auth_attempts(
        database_url,
        CacheFixture::start(CacheSettings::default()).await,
        failures,
        window_ms,
    )
    .await
}

async fn register(
    client: &Client,
    base_url: &str,
    email: &str,
    password: &str,
    source: &str,
) -> reqwest::Response {
    client
        .post(format!("{base_url}/v1/customers"))
        .header("x-real-ip", source)
        .json(&json!({ "email": email, "password": password }))
        .send()
        .await
        .expect("register request")
}

async fn login(
    client: &Client,
    base_url: &str,
    email: &str,
    password: &str,
    source: &str,
) -> reqwest::Response {
    client
        .post(format!("{base_url}/v1/customer/sessions"))
        .header("x-real-ip", source)
        .json(&json!({ "email": email, "password": password }))
        .send()
        .await
        .expect("login request")
}

/// 同一身份、不同来源的连续失败各自只记来源一次，但身份维记满——等待期内正确的凭据也被拒。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn public_auth_attempts_are_bounded_by_identity() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = bounded_api(&database_url, 3, WINDOW_MS).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;
    wait_for_window_margin(WINDOW_MS, WINDOW_MARGIN_MS).await;

    let email = "bounded-identity@example.com";
    let password = "a-long-enough-password";
    let created = register(&client, &base_url, email, password, "10.0.0.1").await;
    assert_eq!(created.status(), StatusCode::CREATED);

    // 三次失败来自三个不同来源：来源维各自只记 1 次，身份维记满 3 次。
    for source in ["10.0.0.1", "10.0.0.2", "10.0.0.3"] {
        let response = login(&client, &base_url, email, "not-the-password", source).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    // 全新来源、正确凭据仍被拒：身份维已到上限；429 必须带可等待时长。
    let limited = login(&client, &base_url, email, password, "10.0.0.9").await;
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(
        limited.headers().contains_key("retry-after"),
        "429 必须带 Retry-After"
    );
    let body: Value = limited.json().await.expect("429 body is JSON");
    assert_eq!(body["error"]["code"], json!("rate_limit_exceeded"));

    drop_isolated_database(&database_name).await;
}

/// 同一来源、不同身份的连续失败把来源维记满——全新身份、全新凭据也被拒。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn public_auth_attempts_are_bounded_by_source() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = bounded_api(&database_url, 3, WINDOW_MS).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;
    wait_for_window_margin(WINDOW_MS, WINDOW_MARGIN_MS).await;

    for index in 0..3 {
        let email = format!("bounded-source-{index}@example.com");
        let response = login(&client, &base_url, &email, "not-the-password", "10.1.0.1").await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    let limited = login(
        &client,
        &base_url,
        "fresh-source@example.com",
        "not-the-password",
        "10.1.0.1",
    )
    .await;
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(limited.headers().contains_key("retry-after"));

    drop_isolated_database(&database_name).await;
}

/// 成功的尝试不累计失败次数：上限为 1 时反复成功登录都不被限，一次失败之后才被限。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_successful_attempt_does_not_count_toward_the_limit() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = bounded_api(&database_url, 1, WINDOW_MS).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;
    wait_for_window_margin(WINDOW_MS, WINDOW_MARGIN_MS).await;

    let email = "bounded-success@example.com";
    let password = "a-long-enough-password";
    assert_eq!(
        register(&client, &base_url, email, password, "10.2.0.1")
            .await
            .status(),
        StatusCode::CREATED
    );
    for _ in 0..3 {
        assert_eq!(
            login(&client, &base_url, email, password, "10.2.0.1")
                .await
                .status(),
            StatusCode::OK
        );
    }

    assert_eq!(
        login(&client, &base_url, email, "not-the-password", "10.2.0.1")
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        login(&client, &base_url, email, password, "10.2.0.1")
            .await
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );

    drop_isolated_database(&database_name).await;
}

/// 等待期内正确凭据同样被拒，等过窗口后恢复。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_auth_waiting_period_rejects_correct_credentials_then_recovers() {
    let (database_url, database_name) = isolated_database_url().await;
    // 这条用例自取窗口与余量（遮蔽文件头那对同名常量）：窗口要盖住 register + 一次失败登录 +
    // 一次被拒登录的耗时（含 argon2），这几步约 2.5 秒（CI 运行 37553143246 的日志），余量取 3.5 秒。
    const WINDOW_MS: u64 = 5_000;
    const WINDOW_MARGIN_MS: u64 = 3_500;
    let (base_url, admin_token, _process) = bounded_api(&database_url, 1, WINDOW_MS).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;
    wait_for_window_margin(WINDOW_MS, WINDOW_MARGIN_MS).await;

    let email = "bounded-window@example.com";
    let password = "a-long-enough-password";
    assert_eq!(
        register(&client, &base_url, email, password, "10.3.0.1")
            .await
            .status(),
        StatusCode::CREATED
    );
    assert_eq!(
        login(&client, &base_url, email, "not-the-password", "10.3.0.1")
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );

    let limited = login(&client, &base_url, email, password, "10.3.0.1").await;
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    let retry_after = limited
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .expect("Retry-After is whole seconds");
    assert!(retry_after >= 1);

    tokio::time::sleep(Duration::from_millis(WINDOW_MS + 500)).await;
    assert_eq!(
        login(&client, &base_url, email, password, "10.3.0.1")
            .await
            .status(),
        StatusCode::OK
    );

    drop_isolated_database(&database_name).await;
}

/// 无缓存部署按既有无缓存行为放行：连续失败不产生 429（Spec 明确接受这不满足 A11）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn without_a_cache_the_auth_attempt_limit_does_not_apply() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let email = "no-cache-bounded@example.com";
    let password = "a-long-enough-password";
    assert_eq!(
        register(&client, &base_url, email, password, "10.4.0.1")
            .await
            .status(),
        StatusCode::CREATED
    );
    for _ in 0..5 {
        assert_eq!(
            login(&client, &base_url, email, "not-the-password", "10.4.0.1")
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        login(&client, &base_url, email, password, "10.4.0.1")
            .await
            .status(),
        StatusCode::OK
    );

    drop_isolated_database(&database_name).await;
}

/// 注册与重置码兑换同样受限：注册冲突计入失败；重置码有效但口令过短记一次身份失败，随后正确兑换被拒。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn register_and_reset_redemption_are_bounded_too() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = bounded_api(&database_url, 1, WINDOW_MS).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;
    wait_for_window_margin(WINDOW_MS, WINDOW_MARGIN_MS).await;

    let email = "bounded-register@example.com";
    let password = "a-long-enough-password";
    let created: Value = register(&client, &base_url, email, password, "10.5.0.1")
        .await
        .json()
        .await
        .expect("register body is JSON");
    let account_id = created["account_id"]
        .as_str()
        .expect("account id is returned")
        .to_owned();

    // 注册冲突是一次失败的尝试：上限为 1，再注册同一邮箱即 429。
    assert_eq!(
        register(&client, &base_url, email, password, "10.5.0.1")
            .await
            .status(),
        StatusCode::CONFLICT
    );
    let limited_register = register(&client, &base_url, email, password, "10.5.0.1").await;
    assert_eq!(limited_register.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(limited_register.headers().contains_key("retry-after"));

    let issued = client
        .post(format!(
            "{base_url}/api/v1/accounts/{account_id}/password-reset"
        ))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("issue reset token");
    assert_eq!(issued.status(), StatusCode::CREATED);
    let reset_token = issued.json::<Value>().await.expect("reset body is JSON")["reset_token"]
        .as_str()
        .expect("reset token is returned")
        .to_owned();

    // 有效码 + 过短口令：口令校验先失败、码没被消费，这次失败计入身份维（重置码所属客户）。
    let short = client
        .post(format!("{base_url}/v1/customer/password-resets/redeem"))
        .header("x-real-ip", "10.5.0.2")
        .json(&json!({ "reset_token": reset_token, "new_password": "short" }))
        .send()
        .await
        .expect("short redeem");
    assert_eq!(short.status(), StatusCode::BAD_REQUEST);

    // 身份维已到上限：换来源、用有效新口令也被拒。
    let limited = client
        .post(format!("{base_url}/v1/customer/password-resets/redeem"))
        .header("x-real-ip", "10.5.0.3")
        .json(&json!({ "reset_token": reset_token, "new_password": "a-fresh-enough-password" }))
        .send()
        .await
        .expect("limited redeem");
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(limited.headers().contains_key("retry-after"));

    drop_isolated_database(&database_name).await;
}

/// 到上限后的拒绝不区分身份是否存在：已注册邮箱与未知邮箱、有效码与无效码回同一个 429。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_attempt_limit_does_not_reveal_whether_the_identity_exists() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = bounded_api(&database_url, 1, WINDOW_MS).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;
    wait_for_window_margin(WINDOW_MS, WINDOW_MARGIN_MS).await;

    let known = "bounded-known@example.com";
    let password = "a-long-enough-password";
    let created: Value = register(&client, &base_url, known, password, "10.6.0.1")
        .await
        .json()
        .await
        .expect("register body is JSON");
    let account_id = created["account_id"]
        .as_str()
        .expect("account id is returned")
        .to_owned();

    // 未知邮箱失败一次即把来源 10.6.0.1 记满。
    assert_eq!(
        login(
            &client,
            &base_url,
            "unknown@example.com",
            "not-the-password",
            "10.6.0.1"
        )
        .await
        .status(),
        StatusCode::BAD_REQUEST
    );

    // 已注册邮箱（凭据正确）与另一个未知邮箱回同一个 429：拒绝不透露身份是否存在。
    let existing = login(&client, &base_url, known, password, "10.6.0.1").await;
    let unknown = login(
        &client,
        &base_url,
        "other-unknown@example.com",
        "x",
        "10.6.0.1",
    )
    .await;
    assert_eq!(existing.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(unknown.status(), StatusCode::TOO_MANY_REQUESTS);
    let existing_body: Value = existing.json().await.expect("existing 429 body");
    let unknown_body: Value = unknown.json().await.expect("unknown 429 body");
    // 只比错误码：等待时长按当前窗口剩余时间给，两次请求差几毫秒是正常的，不构成可区分的信号。
    assert_eq!(
        existing_body["error"]["code"],
        unknown_body["error"]["code"]
    );

    // 重置码同理：先兑换掉一张（变成"已用"），再在受限来源上比较未用 / 已用 / 不存在三种码。
    let issue = |account_id: String| {
        let client = client.clone();
        let base_url = base_url.clone();
        let admin_token = admin_token.clone();
        async move {
            let response = client
                .post(format!(
                    "{base_url}/api/v1/accounts/{account_id}/password-reset"
                ))
                .bearer_auth(&admin_token)
                .send()
                .await
                .expect("issue reset token");
            assert_eq!(response.status(), StatusCode::CREATED);
            response.json::<Value>().await.expect("reset body is JSON")["reset_token"]
                .as_str()
                .expect("reset token is returned")
                .to_owned()
        }
    };
    let redeem = |source: &'static str, code: String| {
        let client = client.clone();
        let base_url = base_url.clone();
        async move {
            client
                .post(format!("{base_url}/v1/customer/password-resets/redeem"))
                .header("x-real-ip", source)
                .json(&json!({ "reset_token": code, "new_password": "a-fresh-enough-password" }))
                .send()
                .await
                .expect("redeem request")
        }
    };

    // 同一身份只保留一张未用码（签新的会作废旧码），所以"已用"与"未用"分属两个客户。
    let used_code = issue(account_id.clone()).await;
    assert_eq!(
        redeem("10.6.0.8", used_code.clone()).await.status(),
        StatusCode::NO_CONTENT
    );
    let holder: Value = register(
        &client,
        &base_url,
        "valid-holder@example.com",
        password,
        "10.6.0.7",
    )
    .await
    .json()
    .await
    .expect("holder register body");
    let holder_account = holder["account_id"]
        .as_str()
        .expect("holder account id is returned")
        .to_owned();
    let valid_code = issue(holder_account).await;

    // 受限来源 10.6.0.9：先用不存在的码失败一次，把来源记满。
    assert_eq!(
        redeem("10.6.0.9", "not-a-real-token".to_owned())
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    // 未用、已用、不存在三种码回同一个 429：拒绝不透露码的状态。
    let valid = redeem("10.6.0.9", valid_code).await;
    let used = redeem("10.6.0.9", used_code).await;
    let missing = redeem("10.6.0.9", "another-not-real-token".to_owned()).await;
    assert_eq!(valid.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(used.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(missing.status(), StatusCode::TOO_MANY_REQUESTS);
    let valid_body: Value = valid.json().await.expect("valid 429 body");
    let used_body: Value = used.json().await.expect("used 429 body");
    let missing_body: Value = missing.json().await.expect("missing 429 body");
    assert_eq!(valid_body["error"]["code"], used_body["error"]["code"]);
    assert_eq!(valid_body["error"]["code"], missing_body["error"]["code"]);

    drop_isolated_database(&database_name).await;
}

/// 三个端点各自计数：一个端点的失败不占用另一个端点的额度。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_three_endpoints_count_failures_separately() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = bounded_api(&database_url, 1, WINDOW_MS).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;
    wait_for_window_margin(WINDOW_MS, WINDOW_MARGIN_MS).await;

    let email = "endpoint-separation@example.com";
    let password = "a-long-enough-password";
    assert_eq!(
        register(&client, &base_url, email, password, "10.7.0.1")
            .await
            .status(),
        StatusCode::CREATED
    );

    // 登录失败一次把登录桶记满；同一来源的注册是另一份计数，照常成功。
    assert_eq!(
        login(&client, &base_url, email, "not-the-password", "10.7.0.1")
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        login(&client, &base_url, email, password, "10.7.0.1")
            .await
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        register(
            &client,
            &base_url,
            "another-endpoint@example.com",
            password,
            "10.7.0.1"
        )
        .await
        .status(),
        StatusCode::CREATED
    );

    // 兑换桶同样独立：登录桶已满，但同一来源的第一次无效兑换仍回参数错误，不是 429。
    let redeem = |code: &'static str| {
        let client = client.clone();
        let base_url = base_url.clone();
        async move {
            client
                .post(format!("{base_url}/v1/customer/password-resets/redeem"))
                .header("x-real-ip", "10.7.0.1")
                .json(&json!({ "reset_token": code, "new_password": "a-fresh-enough-password" }))
                .send()
                .await
                .expect("redeem request")
        }
    };
    assert_eq!(
        redeem("not-a-real-token").await.status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        redeem("still-not-a-real-token").await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );

    drop_isolated_database(&database_name).await;
}
