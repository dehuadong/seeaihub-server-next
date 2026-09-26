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

    // **第二条会话**（同一个人的第二次登录，比如另一台机器）：下面验"重置使**全部**会话失效"。
    let second = client
        .post(format!("{base_url}/api/v1/admin/sessions"))
        .json(&json!({"email": email, "password": password}))
        .send()
        .await
        .expect("second login request")
        .json::<Value>()
        .await
        .expect("second login body")["token"]
        .as_str()
        .expect("second token")
        .to_owned();
    assert_ne!(token, second, "两次登录必须是两条不同的会话");

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
    //
    // 兑换之前先证明**第二条会话确实可用**：否则"兑换后它被拒"什么也证明不了（它本来就不行）。
    // 这是 V-A7 那条断言的前置。
    let second_works = client
        .get(format!("{base_url}/api/v1/admin/session"))
        .bearer_auth(&second)
        .send()
        .await
        .expect("second session before redemption");
    assert_eq!(
        second_works.status(),
        StatusCode::OK,
        "兑换之前第二条会话必须是可用的，否则后面的断言恒真"
    );

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

    // **兑换重置令牌使此前的会话全部失效**（V-A7）。
    //
    // 判据用**第二条**会话（`second`）：它在兑换之前**被证明过可用**，所以"兑换后被拒"才是真的失效。
    // 拿 `token` 去验是恒真的——它在上面退出登录时就已经失效了，它被拒什么也说明不了。
    let second_revoked = client
        .get(format!("{base_url}/api/v1/admin/session"))
        .bearer_auth(&second)
        .send()
        .await
        .expect("second identity request after redemption");
    assert_eq!(
        second_revoked.status(),
        StatusCode::FORBIDDEN,
        "重置必须使**全部**旧会话失效，不只是发起兑换的那一条"
    );
    // 而且答复与"凭据不对"**逐字相同**：调用方分不出自己是失效了还是拿错了（V-A6 的那条断言）。
    let second_body = second_revoked.json::<Value>().await.expect("revoked body");
    let bogus = client
        .get(format!("{base_url}/api/v1/admin/session"))
        .bearer_auth("not-a-real-token")
        .send()
        .await
        .expect("bogus request");
    assert_eq!(bogus.status(), StatusCode::FORBIDDEN);
    let bogus_body = bogus.json::<Value>().await.expect("bogus body");
    assert_eq!(second_body, bogus_body, "失效会话与错凭据必须同答复");

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
    let after_status = after.status();
    assert_eq!(after_status, StatusCode::FORBIDDEN, "过期会话必须被拒");
    let after_body = after.json::<Value>().await.expect("expired body");

    // **答复体也要逐字相同**，不只是状态码：这条路径与"库里根本没有这一行"在实现里是两个分支
    // （一个要顺手删掉过期行、一个直接拒），只比状态码会漏掉"过期"这条分支多说的话。
    let absent = client
        .get(format!("{base_url}/api/v1/gateway-models"))
        .bearer_auth("not-a-real-token")
        .send()
        .await
        .expect("absent token request");
    assert_eq!(absent.status(), after_status, "过期与凭据不对不可区分");
    let absent_body = absent.json::<Value>().await.expect("absent body");
    assert_eq!(
        after_body, absent_body,
        "过期与「库里没有这一行」必须逐字同答复"
    );

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// 管理员改口令（V-A4）：旧口令失效、新口令可用，**改之前发出的全部会话都失效**。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn changing_the_admin_password_revokes_every_session() {
    let (database_url, database_name) = isolated_database_url().await;
    let email = "ops@example.com";
    let password = "a-long-enough-password";
    let (base_url, admin_token, _process) =
        start_api_with_admin(&database_url, email, password).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let session = client
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

    // 当前口令不对：被拒，且什么都不改。
    let wrong = client
        .put(format!("{base_url}/api/v1/admin/password"))
        .bearer_auth(&session)
        .json(&json!({"current_password": "not-the-password", "new_password": "another-long-password"}))
        .send()
        .await
        .expect("change request");
    assert_eq!(wrong.status(), StatusCode::BAD_REQUEST);
    let still_works = client
        .post(format!("{base_url}/api/v1/admin/sessions"))
        .json(&json!({"email": email, "password": password}))
        .send()
        .await
        .expect("login request");
    assert_eq!(
        still_works.status(),
        StatusCode::OK,
        "失败的改口令不该动任何东西"
    );

    let changed = client
        .put(format!("{base_url}/api/v1/admin/password"))
        .bearer_auth(&session)
        .json(&json!({"current_password": password, "new_password": "another-long-password"}))
        .send()
        .await
        .expect("change request");
    assert_eq!(changed.status(), StatusCode::NO_CONTENT);

    let after = client
        .get(format!("{base_url}/api/v1/gateway-models"))
        .bearer_auth(&session)
        .send()
        .await
        .expect("gateway models request");
    assert_eq!(after.status(), StatusCode::FORBIDDEN, "旧会话必须失效");

    let old = client
        .post(format!("{base_url}/api/v1/admin/sessions"))
        .json(&json!({"email": email, "password": password}))
        .send()
        .await
        .expect("login request");
    assert_eq!(old.status(), StatusCode::BAD_REQUEST);
    let fresh = client
        .post(format!("{base_url}/api/v1/admin/sessions"))
        .json(&json!({"email": email, "password": "another-long-password"}))
        .send()
        .await
        .expect("login request");
    assert_eq!(fresh.status(), StatusCode::OK);

    drop_isolated_database(&database_name).await;
}

/// 引导幂等，而且**不改已有账号的口令**（V-A5）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_bootstrap_never_overwrites_a_changed_password() {
    let (database_url, database_name) = isolated_database_url().await;
    let email = "ops@example.com";
    let password = "a-long-enough-password";
    let (base_url, admin_token, process) =
        start_api_with_admin(&database_url, email, password).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let session = client
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
    assert_eq!(
        client
            .put(format!("{base_url}/api/v1/admin/password"))
            .bearer_auth(&session)
            .json(&json!({"current_password": password, "new_password": "another-long-password"}))
            .send()
            .await
            .expect("change request")
            .status(),
        StatusCode::NO_CONTENT
    );

    // 用**同一份环境变量**（还是旧口令）再起一个进程。
    drop(process);
    let (base_url, admin_token, _second) =
        start_api_with_admin(&database_url, email, password).await;
    wait_until_ready(&client, &base_url, &admin_token).await;

    let pool = PgPool::connect(&database_url).await.expect("test pool");
    let admins: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM identity.admin_users")
        .fetch_one(&pool)
        .await
        .expect("admin count");
    assert_eq!(admins, 1, "引导不该产生第二个账号");
    pool.close().await;

    let changed = client
        .post(format!("{base_url}/api/v1/admin/sessions"))
        .json(&json!({"email": email, "password": "another-long-password"}))
        .send()
        .await
        .expect("login request");
    assert_eq!(changed.status(), StatusCode::OK, "改过的口令必须还在");
    let restored = client
        .post(format!("{base_url}/api/v1/admin/sessions"))
        .json(&json!({"email": email, "password": password}))
        .send()
        .await
        .expect("login request");
    assert_eq!(
        restored.status(),
        StatusCode::BAD_REQUEST,
        "引导不该把口令打回环境变量里的那个"
    );

    drop_isolated_database(&database_name).await;
}

/// 客户口令出路整条链（V-C9/C12/C14）：运营开户给初始口令 → 客户改口令 → 旧会话失效；
/// 另一条：运营只签重置令牌 → 客户凭它设口令 → 能登录，且**签发留了痕**。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_customer_can_change_or_reset_its_password() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let email = "customer-password@example.com";
    let initial = "a-long-enough-password";
    let opened = client
        .post(format!("{base_url}/api/v1/customers"))
        .bearer_auth(&admin_token)
        .json(&json!({"email": email, "password": initial}))
        .send()
        .await
        .expect("open customer request");
    assert_eq!(opened.status(), StatusCode::CREATED);
    let account_id = opened.json::<Value>().await.expect("open body")["account_id"]
        .as_str()
        .expect("account id")
        .to_owned();

    let session = client
        .post(format!("{base_url}/v1/customer/sessions"))
        .json(&json!({"email": email, "password": initial}))
        .send()
        .await
        .expect("customer login request")
        .json::<Value>()
        .await
        .expect("login body")["token"]
        .as_str()
        .expect("token")
        .to_owned();
    let changed = client
        .put(format!("{base_url}/v1/customer/password"))
        .bearer_auth(&session)
        .json(&json!({"current_password": initial, "new_password": "another-long-password"}))
        .send()
        .await
        .expect("change request");
    assert_eq!(changed.status(), StatusCode::NO_CONTENT);

    let after = client
        .get(format!("{base_url}/v1/customer/account"))
        .bearer_auth(&session)
        .send()
        .await
        .expect("account request");
    assert_eq!(after.status(), StatusCode::UNAUTHORIZED, "旧会话必须失效");
    let old = client
        .post(format!("{base_url}/v1/customer/sessions"))
        .json(&json!({"email": email, "password": initial}))
        .send()
        .await
        .expect("login request");
    assert_eq!(old.status(), StatusCode::BAD_REQUEST);
    let fresh = client
        .post(format!("{base_url}/v1/customer/sessions"))
        .json(&json!({"email": email, "password": "another-long-password"}))
        .send()
        .await
        .expect("login request");
    assert_eq!(fresh.status(), StatusCode::OK);

    // 第二条出路：运营签重置令牌，客户凭它设口令。
    let issued = client
        .post(format!(
            "{base_url}/api/v1/accounts/{account_id}/password-reset"
        ))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("issue reset request");
    assert_eq!(issued.status(), StatusCode::CREATED);
    let reset_token = issued.json::<Value>().await.expect("reset body")["reset_token"]
        .as_str()
        .expect("reset token")
        .to_owned();

    let redeemed = client
        .post(format!("{base_url}/v1/customer/password-resets/redeem"))
        .json(&json!({"reset_token": reset_token, "new_password": "third-long-password"}))
        .send()
        .await
        .expect("redeem request");
    assert_eq!(redeemed.status(), StatusCode::NO_CONTENT);

    let again = client
        .post(format!("{base_url}/v1/customer/password-resets/redeem"))
        .json(&json!({"reset_token": reset_token, "new_password": "fourth-long-password"}))
        .send()
        .await
        .expect("second redeem request");
    assert_eq!(again.status(), StatusCode::BAD_REQUEST, "令牌是一次性的");
    let after_reset = client
        .post(format!("{base_url}/v1/customer/sessions"))
        .json(&json!({"email": email, "password": "third-long-password"}))
        .send()
        .await
        .expect("login request");
    assert_eq!(after_reset.status(), StatusCode::OK);

    // 签发重置令牌留了痕（V-A7 的审计一半）。
    let pool = PgPool::connect(&database_url).await.expect("test pool");
    let audited: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM operations.audit_events WHERE action = 'customer.password_reset'",
    )
    .fetch_one(&pool)
    .await
    .expect("audit count");
    assert_eq!(audited, 1, "签发重置令牌必须写审计");
    pool.close().await;

    drop_isolated_database(&database_name).await;
}

/// 引导变量的三种组合（V-A8）：两个都不给**不建号、进程照起**；只给一个**启动失败**。
///
/// "两个都不给时不建号、且日志里说得出后台登录不可用"另有一条：这里只看进程起没起来。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_bootstrap_variables_are_all_or_nothing() {
    let (database_url, database_name) = isolated_database_url().await;

    // 两个都给：起来了。
    assert!(
        probe_api_startup_with_seed(
            &database_url,
            Some("ops@example.com"),
            Some("a-long-enough-password")
        )
        .await
        .is_ok(),
        "两个都给时进程必须起来"
    );

    // 两个都不给：进程照起（后台登录不可用不该让 API 起不来）。
    assert!(
        probe_api_startup_with_seed(&database_url, None, None)
            .await
            .is_ok(),
        "两个都不给不该让进程起不来"
    );

    // 只给一个：启动失败。
    assert!(
        probe_api_startup_with_seed(&database_url, Some("ops@example.com"), None)
            .await
            .is_err(),
        "只给邮箱必须启动失败"
    );
    assert!(
        probe_api_startup_with_seed(&database_url, None, Some("a-long-enough-password"))
            .await
            .is_err(),
        "只给口令必须启动失败"
    );

    // 上一步"两个都不给"那次不该建出账号。
    let pool = PgPool::connect(&database_url).await.expect("test pool");
    let admins: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM identity.admin_users")
        .fetch_one(&pool)
        .await
        .expect("admin count");
    pool.close().await;
    assert_eq!(
        admins, 1,
        "只有'两个都给'那一次该建号，现在应当只有一个账号"
    );

    drop_isolated_database(&database_name).await;
}

/// 静态托管（V-D1）：两个入口产物的 HTML 能取到；未注册的 API 路径**仍然是 JSON 404**。
///
/// 后者是重点：兜底成一份 HTML 会把"路径写错了"变成"调用成功"。第一版实现就是这样错的，实测才发现。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_api_serves_the_front_end_without_swallowing_api_404s() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    // 两份入口产物：**先看这次运行有没有构建过**。
    //
    // `apps/web/dist/` 在 `.gitignore` 里，所以 CI 的 `rust` job（只跑 cargo，不跑 npm）里没有它，
    // 而 API 在没有产物时**本来就不托管前端**（`static_spa` 回 `None`）。那种情况下断言 200 会必挂，
    // 而"没构建"本身不是缺陷——所以分两支：没产物时验"未注册路径仍回 JSON 404、且不吐 HTML"，
    // 有产物时才验两个入口。产出物那一支的完整覆盖由 CI 的 `web-e2e` job 与 `apps/web/e2e/` 承担。
    let dist = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("web")
        .join("dist");
    if dist.is_dir() {
        // **按文件名直达**：两份产物在同一个目录里，只按主机名分发时，本机（开发出口开着）就只剩
        // 管理端一条路。文件名优先让两个入口在任何主机上都各有一条明确地址。
        for entry in ["/console.html", "/portal.html"] {
            let response = client
                .get(format!("{base_url}{entry}"))
                .send()
                .await
                .expect("entry request");
            assert_eq!(response.status(), StatusCode::OK, "入口产物取不到：{entry}");
            let body = response.text().await.expect("entry body");
            assert!(body.contains("<!doctype html"), "{entry} 应当是一份 HTML");
        }
        // 两个入口是**不同的**两份产物，不是同一个文件：各自的标题不一样。
        let console = client
            .get(format!("{base_url}/console.html"))
            .send()
            .await
            .expect("console request")
            .text()
            .await
            .expect("console body");
        let portal = client
            .get(format!("{base_url}/portal.html"))
            .send()
            .await
            .expect("portal request")
            .text()
            .await
            .expect("portal body");
        assert!(
            console.contains("运营后台") && portal.contains("seeai 控制台") && console != portal,
            "两个入口必须是各自那一份产物"
        );
    } else {
        // 没有产物：入口路径也得是 JSON 404，**不许**回 HTML——否则调用方会把"这次没部署前端"
        // 读成"前端在这儿"。
        for entry in ["/console.html", "/portal.html"] {
            let response = client
                .get(format!("{base_url}{entry}"))
                .send()
                .await
                .expect("entry request");
            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "没有构建产物时 {entry} 必须是 404"
            );
        }
        eprintln!(
            "没有 apps/web/dist：本次只验\"未托管前端\"那一支，产物那一支由 CI 的 web-e2e job 覆盖"
        );
    }

    // 未注册的 API 路径：**JSON 404**，不是 HTML。
    for path in ["/api/v1/nope", "/v1/nope"] {
        let response = client
            .get(format!("{base_url}{path}"))
            .send()
            .await
            .expect("unknown api request");
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "{path} 必须是 404"
        );
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        assert!(
            content_type.starts_with("application/json"),
            "{path} 必须回 JSON，实际是 {content_type}"
        );
    }

    drop_isolated_database(&database_name).await;
}

/// 越权与未认证（V-C5、V-C10）：**没有凭据**与**凭据无效**访问对客账务/密钥端点，一律未认证；
/// 而**有凭据但目标不属于自己**的，一律"不存在"。
///
/// 两者不是一回事：前者是"我还没证明我是谁"，后者是"我证明了，但这不是我的东西"。混在一起会让
/// 调用方分不清该去登录还是该去核对账户。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn customer_endpoints_distinguish_unauthenticated_from_not_yours() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let reads = [
        "/v1/customer/account",
        "/v1/customer/ledger",
        "/v1/customer/usage",
        "/v1/customer/billing",
        "/v1/customer/api-keys",
    ];

    // 没凭据：未认证。
    for path in reads {
        let response = client
            .get(format!("{base_url}{path}"))
            .send()
            .await
            .expect("request");
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{path} 没凭据时必须未认证"
        );
    }

    // 凭据无效：同样未认证（会话不存在与过期不可区分）。
    for path in reads {
        let response = client
            .get(format!("{base_url}{path}"))
            .bearer_auth("not-a-real-session")
            .send()
            .await
            .expect("request");
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{path} 凭据无效时必须未认证"
        );
    }

    // 有凭据但目标不属于自己：按"不存在"回，而不是 403。
    let registered = client
        .post(format!("{base_url}/v1/customers"))
        .json(&json!({"email": "owner@example.com", "password": "a-long-enough-password"}))
        .send()
        .await
        .expect("register request")
        .json::<Value>()
        .await
        .expect("register body");
    let session = registered["token"].as_str().expect("token").to_owned();

    let stolen = client
        .delete(format!(
            "{base_url}/v1/customer/api-keys/{}",
            Uuid::new_v4()
        ))
        .bearer_auth(&session)
        .send()
        .await
        .expect("revoke request");
    assert_eq!(
        stolen.status(),
        StatusCode::NOT_FOUND,
        "不属于自己的密钥标识必须按'不存在'回，而不是 403"
    );

    drop_isolated_database(&database_name).await;
}

/// 管理面"没带凭据"与"凭据不对"**逐字同答复**（Spec §4.1）。
///
/// 两者可分（一个 401 一个 403）就等于告诉调用方"你连格式都没带对"；而这条端点对外只有一个含义：
/// 这次访问不被接受。对客面另有一条一致的口径：未认证一律 401，与"不是你的东西"（404）也分得开。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn admin_endpoints_do_not_reveal_whether_a_credential_was_sent() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let path = format!("{base_url}/api/v1/gateway-models");

    // 没带任何凭据。
    let missing = client.get(&path).send().await.expect("request");
    let missing_status = missing.status();
    let missing_body = missing.json::<Value>().await.expect("body");

    // 带了但是错的。
    let wrong = client
        .get(&path)
        .bearer_auth("not-a-real-token")
        .send()
        .await
        .expect("request");
    let wrong_status = wrong.status();
    let wrong_body = wrong.json::<Value>().await.expect("body");

    // 连"格式都不对"也算在内（没有 Bearer 前缀）。
    let malformed = client
        .get(&path)
        .header("authorization", "not-a-bearer-token")
        .send()
        .await
        .expect("request");

    assert_eq!(missing_status, wrong_status, "状态码必须相同");
    assert_eq!(missing_body, wrong_body, "响应体必须逐字相同");
    assert_eq!(malformed.status(), wrong_status, "格式不对也必须同答复");

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

    // 形状或取值不合法：参数错（V-C2 的另一半）。**未认证**与**参数错**必须分开——
    // 混成一个答复会让调用方不知道是"你还没登录"还是"你填错了"。
    //
    // 两种错法各有各的码，所以分开断言：**字段缺失/类型不对**是请求体解不出来（422），
    // 而**字段在但取值不合规**是业务校验拒绝（400）。把两者合成一个数会让调用方分不清该改结构还是改值。
    for (label, body, expected) in [
        (
            "邮箱不是邮箱",
            json!({"email": "not-an-email", "password": "a-long-enough-password"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "口令太短",
            json!({"email": "short@example.com", "password": "short"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "口令恰好等于下限",
            json!({"email": "floor@example.com", "password": "12345678"}),
            StatusCode::CREATED,
        ),
        (
            "缺口令",
            json!({"email": "missing@example.com"}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "缺邮箱",
            json!({"password": "a-long-enough-password"}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ] {
        let response = client
            .post(format!("{base_url}/v1/customers"))
            .json(&body)
            .send()
            .await
            .expect("register request");
        assert_eq!(response.status(), expected, "{label}");
    }

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

/// 对客的四条账务读**按调用者自己的账户收窄**（V-C5 的账务那一半）。
///
/// 光断言"B 读到空"是不够的——空库也会给空。所以先给 A 造**真实**的余额与流水（由管理端充值），
/// 再断言 B 读到的是空、而 A 读到的就是那些数。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn accounting_reads_are_scoped_to_the_caller() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let mut sessions = Vec::new();
    for email in ["rich@example.com", "poor@example.com"] {
        let registered = client
            .post(format!("{base_url}/v1/customers"))
            .json(&json!({"email": email, "password": "a-long-enough-password"}))
            .send()
            .await
            .expect("register request")
            .json::<Value>()
            .await
            .expect("register body");
        sessions.push((
            registered["token"].as_str().expect("token").to_owned(),
            registered["account_id"]
                .as_str()
                .expect("account id")
                .to_owned(),
        ));
    }

    // 给 A 充 25 元：这样它的余额与流水都**非空**，B 读到空才有意义。
    let credited = client
        .post(format!(
            "{base_url}/api/v1/accounts/{}/credits",
            sessions[0].1
        ))
        .bearer_auth(&admin_token)
        .json(&json!({"amount_microusd": 25_000_000, "business_key": "scoping-test"}))
        .send()
        .await
        .expect("credit request");
    assert!(
        credited.status().is_success(),
        "充值必须成功：{}",
        credited.status()
    );

    // A 读到自己的钱。
    let a_account = client
        .get(format!("{base_url}/v1/customer/account"))
        .bearer_auth(&sessions[0].0)
        .send()
        .await
        .expect("account request")
        .json::<Value>()
        .await
        .expect("account body");
    assert_eq!(a_account["balance_microusd"], json!(25_000_000));
    let a_ledger = client
        .get(format!("{base_url}/v1/customer/ledger"))
        .bearer_auth(&sessions[0].0)
        .send()
        .await
        .expect("ledger request")
        .json::<Value>()
        .await
        .expect("ledger body");
    assert_eq!(
        a_ledger["entries"].as_array().expect("entries").len(),
        1,
        "A 应当看到自己那一条充值：{a_ledger}"
    );

    // B 看同样的四条：余额与流水都**空**，而且看不到 A 的那一条。
    let b_account = client
        .get(format!("{base_url}/v1/customer/account"))
        .bearer_auth(&sessions[1].0)
        .send()
        .await
        .expect("account request")
        .json::<Value>()
        .await
        .expect("account body");
    assert_eq!(
        b_account["balance_microusd"],
        json!(0),
        "B 不该看到 A 的余额：{b_account}"
    );
    let b_ledger = client
        .get(format!("{base_url}/v1/customer/ledger"))
        .bearer_auth(&sessions[1].0)
        .send()
        .await
        .expect("ledger request")
        .json::<Value>()
        .await
        .expect("ledger body");
    assert_eq!(b_ledger["entries"], json!([]), "B 不该看到 A 的流水");
    let b_usage = client
        .get(format!("{base_url}/v1/customer/usage"))
        .bearer_auth(&sessions[1].0)
        .send()
        .await
        .expect("usage request")
        .json::<Value>()
        .await
        .expect("usage body");
    assert_eq!(b_usage["usage"], json!([]), "B 不该看到 A 的用量");
    let b_billing = client
        .get(format!("{base_url}/v1/customer/billing"))
        .bearer_auth(&sessions[1].0)
        .send()
        .await
        .expect("billing request")
        .json::<Value>()
        .await
        .expect("billing body");
    assert_eq!(
        b_billing["charged_microusd"],
        json!(0),
        "B 的账单不该带上 A 的钱：{b_billing}"
    );

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
