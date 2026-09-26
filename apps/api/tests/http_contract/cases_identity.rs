use super::*;
use serde_json::json;

/// 邮箱 + 口令能换到会话凭据，并且用这条凭据能调管理 API。
///
/// 这是 V-A1：登录的目的是"之后的管理 API 调用用它认证"，所以这条用例必须**真的去调一个既有
/// 管理端点**，只断言登录响应不够。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn an_admin_can_log_in_and_use_the_session() {
    let (database_url, database_name) = isolated_database_url().await;
    let email = "ops@example.com";
    let password = "a-long-enough-password";
    let (base_url, admin_token, _process) =
        start_api_with_admin(&database_url, email, password).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let response = client
        .post(format!("{base_url}/api/v1/admin/sessions"))
        .json(&json!({"email": email, "password": password}))
        .send()
        .await
        .expect("login request");
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.json::<Value>().await.expect("login body is JSON");
    let token = body["token"]
        .as_str()
        .expect("token is returned")
        .to_owned();
    assert_eq!(body["email"], json!(email));

    // 用会话凭据调既有管理端点：形状与用共享令牌时逐字一致（这里只判它能读到清单）。
    let models = client
        .get(format!("{base_url}/api/v1/gateway-models"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("gateway models request");
    assert_eq!(models.status(), StatusCode::OK);

    // 认身份：**仅会话**那条端点回的是这个人的身份。
    let me = client
        .get(format!("{base_url}/api/v1/admin/session"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("session identity request");
    assert_eq!(me.status(), StatusCode::OK);
    let me = me.json::<Value>().await.expect("identity body is JSON");
    assert_eq!(me["email"], json!(email));

    drop_isolated_database(&database_name).await;
}

/// 邮箱不存在与口令不对**回同一个答复**：文案、状态码与错误码都不区分。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_wrong_password_and_an_unknown_email_are_indistinguishable() {
    let (database_url, database_name) = isolated_database_url().await;
    let email = "ops@example.com";
    let password = "a-long-enough-password";
    let (base_url, admin_token, _process) =
        start_api_with_admin(&database_url, email, password).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let wrong_password = client
        .post(format!("{base_url}/api/v1/admin/sessions"))
        .json(&json!({"email": email, "password": "not-the-password"}))
        .send()
        .await
        .expect("login request");
    let unknown_email = client
        .post(format!("{base_url}/api/v1/admin/sessions"))
        .json(&json!({"email": "nobody@example.com", "password": password}))
        .send()
        .await
        .expect("login request");

    assert_eq!(wrong_password.status(), unknown_email.status());
    let left = wrong_password.json::<Value>().await.expect("error body");
    let right = unknown_email.json::<Value>().await.expect("error body");
    assert_eq!(left, right, "两种失败必须回同一个响应体");

    drop_isolated_database(&database_name).await;
}

/// 退出之后那条会话立刻不能用；共享令牌**不适用于**"关于我自己"的三条端点。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_session_is_revoked_on_logout_and_shared_token_cannot_act_as_a_person() {
    let (database_url, database_name) = isolated_database_url().await;
    let email = "ops@example.com";
    let password = "a-long-enough-password";
    let (base_url, admin_token, _process) =
        start_api_with_admin(&database_url, email, password).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let token = client
        .post(format!("{base_url}/api/v1/admin/sessions"))
        .json(&json!({"email": email, "password": password}))
        .send()
        .await
        .expect("login request")
        .json::<Value>()
        .await
        .expect("login body")["token"]
        .as_str()
        .expect("token")
        .to_owned();

    let logout = client
        .delete(format!("{base_url}/api/v1/admin/sessions"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("logout request");
    assert_eq!(logout.status(), StatusCode::NO_CONTENT);

    let after = client
        .get(format!("{base_url}/api/v1/gateway-models"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("gateway models request");
    assert_eq!(
        after.status(),
        StatusCode::FORBIDDEN,
        "退出后这条会话必须立刻失效"
    );

    // 共享令牌能调既有管理端点……
    let shared = client
        .get(format!("{base_url}/api/v1/gateway-models"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("shared token request");
    assert_eq!(shared.status(), StatusCode::OK);

    // ……但回答不了"我是谁"。
    let me = client
        .get(format!("{base_url}/api/v1/admin/session"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("identity request");
    assert_eq!(me.status(), StatusCode::FORBIDDEN);

    // 重置令牌的兑换入口**不需要任何凭据**（口令重置的全部意义就是"进不去了"）。
    let issued = client
        .post(format!("{base_url}/api/v1/admin/password-resets"))
        .bearer_auth(&admin_token)
        .json(&json!({"email": email}))
        .send()
        .await
        .expect("issue reset request");
    assert_eq!(issued.status(), StatusCode::CREATED);
    let issued = issued.json::<Value>().await.expect("reset body");
    let reset_token = issued["reset_token"].as_str().expect("reset token");

    let redeemed = client
        .post(format!("{base_url}/api/v1/admin/password-resets/redeem"))
        .json(&json!({"reset_token": reset_token, "new_password": "another-long-password"}))
        .send()
        .await
        .expect("redeem request");
    assert_eq!(redeemed.status(), StatusCode::NO_CONTENT);

    // 新口令能登录、旧口令不能；同一枚令牌第二次兑换被拒。
    let again = client
        .post(format!("{base_url}/api/v1/admin/password-resets/redeem"))
        .json(&json!({"reset_token": reset_token, "new_password": "third-long-password"}))
        .send()
        .await
        .expect("second redeem request");
    assert_eq!(again.status(), StatusCode::BAD_REQUEST, "令牌是一次性的");

    let old = client
        .post(format!("{base_url}/api/v1/admin/sessions"))
        .json(&json!({"email": email, "password": password}))
        .send()
        .await
        .expect("old password login");
    assert_eq!(old.status(), StatusCode::BAD_REQUEST);
    let fresh = client
        .post(format!("{base_url}/api/v1/admin/sessions"))
        .json(&json!({"email": email, "password": "another-long-password"}))
        .send()
        .await
        .expect("new password login");
    assert_eq!(fresh.status(), StatusCode::OK);

    drop_isolated_database(&database_name).await;
}

/// 过期的会话被拒（V-A3 的端到端那一半）。
///
/// 不靠 `sleep` 等 TTL：直接在真库里把这一行的 `expires_at` 推到过去，再用它调用。判据是"过期凭据
/// 真的被拒"，与"过期判定与清理"（应用层用例）各验一半。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn an_expired_session_is_rejected() {
    let (database_url, database_name) = isolated_database_url().await;
    let email = "ops@example.com";
    let password = "a-long-enough-password";
    let (base_url, admin_token, _process) =
        start_api_with_admin(&database_url, email, password).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let token = client
        .post(format!("{base_url}/api/v1/admin/sessions"))
        .json(&json!({"email": email, "password": password}))
        .send()
        .await
        .expect("login request")
        .json::<Value>()
        .await
        .expect("login body")["token"]
        .as_str()
        .expect("token")
        .to_owned();

    let before = client
        .get(format!("{base_url}/api/v1/gateway-models"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("gateway models request");
    assert_eq!(before.status(), StatusCode::OK, "刚登录的会话必须能用");

    // 把这一行推到过去（按摘要定位，与认证路径同一条读）。
    let pool = PgPool::connect(&database_url).await.expect("test pool");
    let updated = sqlx::query(
        "UPDATE identity.admin_sessions SET expires_at = now() - interval '1 minute' \
         WHERE token_hash = $1",
    )
    .bind(seeai_application::session_token_hash(&token))
    .execute(&pool)
    .await
    .expect("expire the session")
    .rows_affected();
    assert_eq!(updated, 1, "夹具必须改到那一行");

    let after = client
        .get(format!("{base_url}/api/v1/gateway-models"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("gateway models request");
    assert_eq!(after.status(), StatusCode::FORBIDDEN, "过期会话必须被拒");

    let wrong = client
        .get(format!("{base_url}/api/v1/gateway-models"))
        .bearer_auth("not-a-real-token")
        .send()
        .await
        .expect("gateway models request");
    assert_eq!(after.status(), wrong.status(), "过期与凭据不对不可区分");

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// 客户自助：注册 → 发密钥 → 列密钥（**没有明文**）→ 吊销 → 该密钥不能再调对客接口。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_customer_registers_manages_its_own_keys() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let email = "customer@example.com";
    let registered = client
        .post(format!("{base_url}/v1/customers"))
        .json(&json!({"email": email, "password": "a-long-enough-password"}))
        .send()
        .await
        .expect("register request");
    assert_eq!(registered.status(), StatusCode::CREATED);
    let registered = registered.json::<Value>().await.expect("register body");
    let session = registered["token"]
        .as_str()
        .expect("session token")
        .to_owned();
    let account_id = registered["account_id"]
        .as_str()
        .expect("account id")
        .to_owned();

    // 同一邮箱再注册一次是冲突。
    let duplicate = client
        .post(format!("{base_url}/v1/customers"))
        .json(&json!({"email": email, "password": "a-long-enough-password"}))
        .send()
        .await
        .expect("duplicate register request");
    assert_eq!(duplicate.status(), StatusCode::CONFLICT);

    // 发密钥：明文只这一次。
    let issued = client
        .post(format!("{base_url}/v1/customer/api-keys"))
        .bearer_auth(&session)
        .json(&json!({"label": "my-first-key"}))
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

    // 列表里**没有任何明文**：只有标签、创建时间与吊销状态。
    let listed = client
        .get(format!("{base_url}/v1/customer/api-keys"))
        .bearer_auth(&session)
        .send()
        .await
        .expect("list keys request")
        .json::<Value>()
        .await
        .expect("list body");
    let keys = listed["keys"].as_array().expect("keys array");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0]["label"], json!("my-first-key"));
    assert_eq!(keys[0]["revoked_at"], json!(null));
    let rendered = listed.to_string();
    assert!(!rendered.contains(&api_key), "密钥明文绝不能在列表里出现");

    // 这把密钥现在能用（对客只读端点）。
    let usable = client
        .get(format!("{base_url}/v1/account"))
        .bearer_auth(&api_key)
        .send()
        .await
        .expect("own account request");
    assert_eq!(usable.status(), StatusCode::OK);

    // 吊销之后立刻不能用。
    let revoked = client
        .delete(format!("{base_url}/v1/customer/api-keys/{key_id}"))
        .bearer_auth(&session)
        .send()
        .await
        .expect("revoke request");
    assert_eq!(revoked.status(), StatusCode::NO_CONTENT);
    let after = client
        .get(format!("{base_url}/v1/account"))
        .bearer_auth(&api_key)
        .send()
        .await
        .expect("own account request");
    assert_eq!(after.status(), StatusCode::UNAUTHORIZED);

    drop_isolated_database(&database_name).await;
    // `account_id` 只用来说明这条链拿到了一个真账户；它的形状由注册响应保证。
    assert!(!account_id.is_empty());
}

/// 运营替客户开户（Spec C13）：新账户一个；**给已有账户配上登录身份**一个——配好之后客户登录看
/// 到的就是那个账户原有的余额。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn operations_open_customer_accounts_including_for_existing_ones() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    // 先按老路子建一个账户并充值：它今天只有账户、没有登录身份。
    let created = client
        .post(format!("{base_url}/api/v1/accounts"))
        .bearer_auth(&admin_token)
        .json(&json!({"initial_credit_microusd": 5_000_000}))
        .send()
        .await
        .expect("create account request")
        .json::<Value>()
        .await
        .expect("create account body");
    let account_id = created["account_id"]
        .as_str()
        .expect("account id")
        .to_owned();

    // 运营给这个**已有账户**配登录身份。
    let opened = client
        .post(format!("{base_url}/api/v1/customers"))
        .bearer_auth(&admin_token)
        .json(&json!({
            "email": "existing@example.com",
            "password": "a-long-enough-password",
            "account_id": account_id,
        }))
        .send()
        .await
        .expect("open customer request");
    assert_eq!(opened.status(), StatusCode::CREATED);
    let opened = opened.json::<Value>().await.expect("open body");
    assert_eq!(opened["account_id"], json!(account_id));

    // 运营配好身份之后，客户用那个邮箱口令能登录。
    let login = client
        .post(format!("{base_url}/v1/customer/sessions"))
        .json(&json!({"email": "existing@example.com", "password": "a-long-enough-password"}))
        .send()
        .await
        .expect("customer login request");
    assert_eq!(login.status(), StatusCode::OK, "开户之后客户必须能登录");
    let login = login.json::<Value>().await.expect("customer login body");
    let session = login["token"].as_str().expect("session token").to_owned();

    // 客户登录后，用**自己的密钥**读余额：读到的必须是那个已有账户原有的 5 元
    // （`/v1/account` 认的是 API Key，不是客户会话——会话走的是 `/v1/customer/*`）。
    let issued = client
        .post(format!("{base_url}/v1/customer/api-keys"))
        .bearer_auth(&session)
        .json(&json!({"label": "existing-account-key"}))
        .send()
        .await
        .expect("issue key request");
    assert_eq!(issued.status(), StatusCode::CREATED);
    let issued = issued.json::<Value>().await.expect("issue body");
    let api_key = issued["api_key"]
        .as_str()
        .expect("plaintext key")
        .to_owned();

    let account = client
        .get(format!("{base_url}/v1/account"))
        .bearer_auth(&api_key)
        .send()
        .await
        .expect("own account request");
    assert_eq!(account.status(), StatusCode::OK);
    let account = account.json::<Value>().await.expect("own account body");
    assert_eq!(account["balance_microusd"], json!(5_000_000));

    // 同一个账户再绑一个邮箱是冲突。
    let twice = client
        .post(format!("{base_url}/api/v1/customers"))
        .bearer_auth(&admin_token)
        .json(&json!({
            "email": "someone-else@example.com",
            "password": "a-long-enough-password",
            "account_id": account_id,
        }))
        .send()
        .await
        .expect("second binding request");
    assert_eq!(twice.status(), StatusCode::CONFLICT);

    // 按邮箱能找到它（运营要靠这个标识给客户充值、签重置令牌）。
    let found = client
        .get(format!(
            "{base_url}/api/v1/customers?email=existing@example.com"
        ))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("find customer request")
        .json::<Value>()
        .await
        .expect("find customer body");
    assert_eq!(found["customers"][0]["account_id"], json!(account_id));

    drop_isolated_database(&database_name).await;
}

/// 客户只能碰自己的东西：A 的会话查不到 B 的密钥，吊销 B 的密钥也拿不到（**不存在**），
/// 而 B 的密钥仍然可用。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_customer_cannot_touch_another_customers_keys() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let mut sessions = Vec::new();
    for email in ["a@example.com", "b@example.com"] {
        let registered = client
            .post(format!("{base_url}/v1/customers"))
            .json(&json!({"email": email, "password": "a-long-enough-password"}))
            .send()
            .await
            .expect("register request")
            .json::<Value>()
            .await
            .expect("register body");
        sessions.push(registered["token"].as_str().expect("token").to_owned());
    }

    // B 发一把密钥。
    let b_key = client
        .post(format!("{base_url}/v1/customer/api-keys"))
        .bearer_auth(&sessions[1])
        .json(&json!({"label": "b-key"}))
        .send()
        .await
        .expect("issue key request")
        .json::<Value>()
        .await
        .expect("issue body");
    let b_key_id = b_key["key_id"].as_str().expect("key id").to_owned();
    let b_plaintext = b_key["api_key"].as_str().expect("plaintext").to_owned();

    // A 看不到 B 的密钥。
    let a_keys = client
        .get(format!("{base_url}/v1/customer/api-keys"))
        .bearer_auth(&sessions[0])
        .send()
        .await
        .expect("list request")
        .json::<Value>()
        .await
        .expect("list body");
    assert_eq!(a_keys["keys"], json!([]));

    // A 吊销 B 的密钥：**不存在**（不回 403，也不说明它存在）。
    let stolen = client
        .delete(format!("{base_url}/v1/customer/api-keys/{b_key_id}"))
        .bearer_auth(&sessions[0])
        .send()
        .await
        .expect("steal request");
    assert_eq!(stolen.status(), StatusCode::NOT_FOUND);

    // B 的密钥仍然可用。
    let still_usable = client
        .get(format!("{base_url}/v1/account"))
        .bearer_auth(&b_plaintext)
        .send()
        .await
        .expect("own account request");
    assert_eq!(still_usable.status(), StatusCode::OK);

    drop_isolated_database(&database_name).await;
}

/// 对客面**没有**"提交邮箱就拿到重置令牌"的入口：那条路等于"知道邮箱就能接管账户"。
///
/// 这不是漏做——平台不发邮件、不做邮箱验证，所以重置只能由运营在管理端签发后转交。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn there_is_no_self_service_password_reset_entry() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let response = client
        .post(format!("{base_url}/v1/customer/password-resets"))
        .json(&json!({"email": "anyone@example.com"}))
        .send()
        .await
        .expect("probe request");
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "对客面不该存在这个端点"
    );

    drop_isolated_database(&database_name).await;
}
