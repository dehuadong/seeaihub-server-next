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

    // 签发事件要在审计里可查（V-A7），而且指向的必须是**被重置的那个管理员**：
    // 只查 action 有没有落库的话，"记在了别人头上"照样过。
    let pool = PgPool::connect(&database_url).await.expect("test pool");
    let admin_id: Uuid = sqlx::query_scalar("SELECT id FROM identity.admin_users WHERE email = $1")
        .bind(email)
        .fetch_one(&pool)
        .await
        .expect("admin id");
    let (actor, subject_type, subject_id): (String, String, String) = sqlx::query_as(
        "SELECT actor, subject_type, subject_id FROM operations.audit_events \
         WHERE action = 'admin.password_reset'",
    )
    .fetch_one(&pool)
    .await
    .expect("管理员签发重置令牌必须写审计");
    assert_eq!(actor, "admin-self");
    assert_eq!(subject_type, "admin_user");
    assert_eq!(subject_id, admin_id.to_string());
    pool.close().await;

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

/// 对客面"未认证"的三种来源都回同一个答复（V-C10）：没带凭据、会话已退出、会话已过期。
///
/// 判据点名的是这三种都要回**未认证**，而不是"不存在"或"无权"——三者合成一个答复，调用方才知道该做
/// 的事是重新登录，而不是去查"这个账户是不是没了"。
///
/// 退出与过期都必须**真的**拒绝，所以要各造一次：退出走 `DELETE /v1/customer/sessions`（判据里那句
/// "已退出"就是它），过期不靠等待，直接在真库里把这一行推到过去（与管理员那条 `an_expired_session_is_rejected`
/// 同一套夹具做法）。
///
/// 两处账务读都问一遍：只验一个端点的话，"某一个处理器忘了过鉴权"照样过。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_customer_is_unauthenticated_without_a_credential_or_after_logout_or_expiry() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let email = "customer-unauthenticated@example.com";
    let password = "a-long-enough-password";
    let opened = client
        .post(format!("{base_url}/api/v1/customers"))
        .bearer_auth(&admin_token)
        .json(&json!({"email": email, "password": password}))
        .send()
        .await
        .expect("open customer request");
    assert_eq!(opened.status(), StatusCode::CREATED);

    let login = || {
        let client = client.clone();
        let url = format!("{base_url}/v1/customer/sessions");
        async move {
            client
                .post(url)
                .json(&json!({"email": email, "password": password}))
                .send()
                .await
                .expect("customer login request")
                .json::<Value>()
                .await
                .expect("login body")["token"]
                .as_str()
                .expect("token")
                .to_owned()
        }
    };

    // 两个账务读：判据说的是"客户账务与密钥端点"，所以要各问一遍。
    let reads = ["/v1/customer/ledger?limit=1", "/v1/customer/usage?limit=1"];

    let refused = |token: Option<String>| {
        let client = client.clone();
        let base = base_url.clone();
        async move {
            let mut answers = Vec::new();
            for path in reads {
                let mut request = client.get(format!("{base}{path}"));
                if let Some(token) = &token {
                    request = request.bearer_auth(token);
                }
                let response = request.send().await.expect("customer read");
                let status = response.status();
                let body = response.text().await.expect("refused body");
                answers.push((path, status, body));
            }
            answers
        }
    };

    // 一、没带凭据。
    for (path, status, body) in refused(None).await {
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{path} 不带凭据要回未认证：{body}"
        );
        assert!(
            body.contains("authorization_required"),
            "{path} 的答复要说清是缺凭据：{body}"
        );
    }

    // 二、会话已退出。先确认它能用，否则下面的 401 证明不了"退出把它废了"。
    let after_logout = login().await;
    let usable = client
        .get(format!("{base_url}/v1/customer/ledger?limit=1"))
        .bearer_auth(&after_logout)
        .send()
        .await
        .expect("usable read");
    assert_eq!(usable.status(), StatusCode::OK, "刚登录的会话必须能用");

    let logged_out = client
        .delete(format!("{base_url}/v1/customer/sessions"))
        .bearer_auth(&after_logout)
        .send()
        .await
        .expect("logout request");
    assert_eq!(logged_out.status(), StatusCode::NO_CONTENT);
    for (path, status, body) in refused(Some(after_logout.clone())).await {
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{path} 退出后要回未认证：{body}"
        );
    }
    // 再退一次：这一行已经没了，仍然只能回未认证（不能回 500 或 404）。
    let again = client
        .delete(format!("{base_url}/v1/customer/sessions"))
        .bearer_auth(&after_logout)
        .send()
        .await
        .expect("second logout request");
    assert_eq!(
        again.status(),
        StatusCode::UNAUTHORIZED,
        "重复退出要回未认证"
    );

    // 三、会话已过期：把这一行推到过去，不靠等待。
    let expired = login().await;
    let pool = PgPool::connect(&database_url).await.expect("test pool");
    let updated = sqlx::query(
        "UPDATE identity.customer_sessions SET expires_at = now() - interval '1 minute' \
         WHERE token_hash = $1",
    )
    .bind(seeai_application::session_token_hash(&expired))
    .execute(&pool)
    .await
    .expect("expire the customer session")
    .rows_affected();
    assert_eq!(updated, 1, "夹具必须改到那一行");
    for (path, status, body) in refused(Some(expired)).await {
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{path} 过期后要回未认证：{body}"
        );
    }

    pool.close().await;
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
    // 当前口令不对：被拒，而且**什么都不改**。
    //
    // "不改动"要落在旧口令仍然可用上，不能只看状态码：改口令与吊销会话在实现里是同一件事的两半，
    // 只判 400 的话，"被拒了但会话已经清掉"照样过。
    let wrong = client
        .put(format!("{base_url}/v1/customer/password"))
        .bearer_auth(&session)
        .json(&json!({"current_password": "not-the-password", "new_password": "another-long-password"}))
        .send()
        .await
        .expect("change request");
    assert_eq!(wrong.status(), StatusCode::BAD_REQUEST);
    let still_works = client
        .post(format!("{base_url}/v1/customer/sessions"))
        .json(&json!({"email": email, "password": initial}))
        .send()
        .await
        .expect("login request");
    assert_eq!(
        still_works.status(),
        StatusCode::OK,
        "被拒的改口令不该动任何东西：旧口令必须还在"
    );
    let session_survives = client
        .get(format!("{base_url}/v1/customer/account"))
        .bearer_auth(&session)
        .send()
        .await
        .expect("account request");
    assert_eq!(
        session_survives.status(),
        StatusCode::OK,
        "被拒的改口令不该把这个人的会话也吊销掉"
    );

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

/// 运营签发的重置令牌兑换之后：**重置前发出的会话立刻全部失效**、旧口令登不进来、新口令登得进来、
/// 同一枚令牌第二次使用被拒（V-C9）。
///
/// 与 `a_customer_can_change_or_reset_its_password` 分开：那条用例里的会话在**改口令**那一步就已经
/// 被作废了，拿它验"重置使旧会话失效"是恒真的（它本来就不行了）。这里的会话全部在兑换**之前**签发、
/// 并在兑换前逐一证明过可用，所以兑换后它们被拒才真说明是重置废掉了它们。
///
/// 两条会话而不是一条：判据说的是旧会话**全部**失效，只有一条时"全部"与"这一条"分不开。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn redeeming_a_reset_token_revokes_the_sessions_issued_before_it() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let email = "reset-revokes-sessions@example.com";
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

    let login = || {
        let client = client.clone();
        let url = format!("{base_url}/v1/customer/sessions");
        async move {
            client
                .post(url)
                .json(&json!({"email": email, "password": initial}))
                .send()
                .await
                .expect("customer login request")
                .json::<Value>()
                .await
                .expect("login body")["token"]
                .as_str()
                .expect("token")
                .to_owned()
        }
    };
    let first = login().await;
    let second = login().await;
    assert_ne!(first, second, "两次登录必须是两条不同的会话");

    let account = format!("{base_url}/v1/customer/account");
    for token in [&first, &second] {
        let usable = client
            .get(account.as_str())
            .bearer_auth(token)
            .send()
            .await
            .expect("account request");
        assert_eq!(
            usable.status(),
            StatusCode::OK,
            "兑换之前这两条会话必须可用，否则后面那句'它们被拒'什么也证明不了"
        );
    }

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
        .json(&json!({"reset_token": reset_token, "new_password": "another-long-password"}))
        .send()
        .await
        .expect("redeem request");
    assert_eq!(redeemed.status(), StatusCode::NO_CONTENT);

    for token in [&first, &second] {
        let revoked = client
            .get(account.as_str())
            .bearer_auth(token)
            .send()
            .await
            .expect("account request");
        assert_eq!(
            revoked.status(),
            StatusCode::UNAUTHORIZED,
            "重置前发出的会话必须在兑换之后立刻失效"
        );
    }

    let old = client
        .post(format!("{base_url}/v1/customer/sessions"))
        .json(&json!({"email": email, "password": initial}))
        .send()
        .await
        .expect("old password login");
    assert_eq!(old.status(), StatusCode::BAD_REQUEST, "旧口令必须失效");

    let fresh = client
        .post(format!("{base_url}/v1/customer/sessions"))
        .json(&json!({"email": email, "password": "another-long-password"}))
        .send()
        .await
        .expect("new password login");
    assert_eq!(fresh.status(), StatusCode::OK, "凭令牌设的新口令必须能登录");

    let again = client
        .post(format!("{base_url}/v1/customer/password-resets/redeem"))
        .json(&json!({"reset_token": reset_token, "new_password": "third-long-password"}))
        .send()
        .await
        .expect("second redeem request");
    assert_eq!(again.status(), StatusCode::BAD_REQUEST, "令牌是一次性的");

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

    // 判据要求"注册成功后，用该邮箱与口令能登录"：注册响应里那条会话是**注册顺带发的**，
    // 拿它当"能登录"等于没验登录这条路。
    let login = client
        .post(format!("{base_url}/v1/customer/sessions"))
        .json(&json!({"email": email, "password": "a-long-enough-password"}))
        .send()
        .await
        .expect("customer login request");
    assert_eq!(login.status(), StatusCode::OK, "注册后必须能用邮箱口令登录");
    let login = login.json::<Value>().await.expect("login body");
    assert_eq!(login["account_id"], json!(account_id));
    let fresh_session = login["token"].as_str().expect("session token").to_owned();

    // 账户余额为 0：新开的账户还没被充过值。
    let fresh = client
        .get(format!("{base_url}/v1/customer/account"))
        .bearer_auth(&fresh_session)
        .send()
        .await
        .expect("account request")
        .json::<Value>()
        .await
        .expect("account body");
    assert_eq!(fresh["balance_microusd"], json!(0), "新账户余额必须是 0");

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
    // 判据要求列表里能看到**创建时间**（V-C3）：少了它，"这把密钥是什么时候发的"就查不到。
    let created_at = keys[0]["created_at"].as_str().expect("列表要给出创建时间");
    assert!(
        chrono::DateTime::parse_from_rfc3339(created_at).is_ok(),
        "创建时间要是 RFC 3339：{created_at}"
    );
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

/// V-C11 数的那三个**自助动作**：注册、登录、凭令牌兑换。
const SELF_SERVICE_ROUTES: [(&str, &str); 3] = [
    ("POST", "/v1/customers"),
    ("POST", "/v1/customer/sessions"),
    ("POST", "/v1/customer/password-resets/redeem"),
];

/// 对客目录（`GET /v1/models`）：**既有对客协议**里就公开的那一条，只列发布过的型号身份与合同，
/// 发不出任何凭据、也不改状态。V-C11 数的"三个"是自助动作，不含它；它在这里出现是因为枚举必须
/// 把"未认证可达"的全部列出来，白名单里少写它会让这条用例红，写它则要说明为什么它不是缺口。
const PUBLIC_CATALOGUE_ROUTE: (&str, &str) = ("GET", "/v1/models");

/// 路径参数换成的具体值：占位符原样打过去会落成 404/405，那时验的就不是鉴权了。
const PROBE_ID: &str = "00000000-0000-4000-8000-000000000000";

/// 一次探针带的请求体。
enum ProbeBody {
    None,
    Json(Value),
    Multipart,
}

/// 把路由表里的路径参数换成具体值。
fn concrete_path(path: &str) -> String {
    let mut rendered = String::new();
    let mut rest = path;
    while let Some(open) = rest.find('{') {
        let Some(close) = rest[open..].find('}') else {
            break;
        };
        rendered.push_str(&rest[..open]);
        rendered.push_str(PROBE_ID);
        rest = &rest[open + close + 1..];
    }
    rendered.push_str(rest);
    rendered
}

/// 从**路由表源码**里读出 `/v1/…` 下的每一条 `(方法, 路径)`。
///
/// 手抄一份清单会随路由表腐坏：新增一条忘了挂鉴权的端点时，手抄的清单照样全绿。所以这里直接读
/// `apps/api/src/main.rs`（`include_str!` 在编译期内联），解析只认这个文件当前的写法；写法变了会在
/// 这里炸掉，而不是静默少列几条。
fn customer_routes_in_the_route_table() -> Vec<(String, String)> {
    const SOURCE: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.rs"));
    const METHODS: [&str; 5] = ["get", "post", "put", "patch", "delete"];

    let mut routes = Vec::new();
    let mut rest = SOURCE;
    while let Some(index) = rest.find(".route(") {
        rest = &rest[index..];
        let after_open = &rest[".route(".len()..];
        let literal = after_open
            .find('"')
            .map(|open| &after_open[open + 1..])
            .expect("路由表的路径是字符串字面量");
        let close = literal.find('"').expect("路径字面量要有收尾引号");
        let path = &literal[..close];
        // 处理器表达式：从路径之后配平括号到这条 `.route(` 的收尾。
        let handlers = &literal[close + 1..];
        let mut depth = 1_usize;
        let mut end = handlers.len();
        for (offset, character) in handlers.char_indices() {
            match character {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = offset;
                        break;
                    }
                }
                _ => {}
            }
        }
        let handlers = &handlers[..end];
        let mut found = Vec::new();
        for method in METHODS {
            let needle = format!("{method}(");
            let mut from = 0;
            while let Some(offset) = handlers[from..].find(&needle) {
                let at = from + offset;
                // 方法名前面必须是分隔符：`budget(` 这类处理器名不能算成 `get(`。
                let preceded_by_separator = !handlers[..at]
                    .ends_with(|previous: char| previous.is_alphanumeric() || previous == '_');
                if preceded_by_separator {
                    found.push(method.to_uppercase());
                    break;
                }
                from = at + needle.len();
            }
        }
        assert!(
            !found.is_empty(),
            ".route(\"{path}\", …) 没认出任何 HTTP 方法，解析没跟上写法"
        );
        if path.starts_with("/v1/") {
            for method in found {
                routes.push((method, path.to_owned()));
            }
        }
        rest = &rest[".route(".len()..];
    }
    routes
}

/// 给某条路由配一个**能被解析**的请求体。
///
/// 处理器里的鉴权在提取器**之后**才跑：空体或不合形状的体会先被 422/400 拦在前面，那时"不是 401"
/// 验的是解析器而不是鉴权。所以每条写路由都要有体；新增一条没登记的路由会在这里炸掉，逼着补一个。
fn probe_body(method: &str, path: &str) -> ProbeBody {
    if method == "GET" || method == "DELETE" {
        return ProbeBody::None;
    }
    match path {
        "/v1/images/generations" => ProbeBody::Json(json!({})),
        "/v1/images/edits" => ProbeBody::Multipart,
        "/v1/customers" => ProbeBody::Json(json!({
            "email": "credential-free-probe@example.com",
            "password": "a-long-enough-password",
        })),
        // 邮箱不存在：这条端点本来就该在**没有凭据**的情况下被走通到"登录失败"，而不是"未认证"。
        "/v1/customer/sessions" => ProbeBody::Json(json!({
            "email": "nobody@example.com",
            "password": "a-long-enough-password",
        })),
        "/v1/customer/password" => ProbeBody::Json(json!({
            "current_password": "a-long-enough-password",
            "new_password": "another-long-password",
        })),
        "/v1/customer/password-resets/redeem" => ProbeBody::Json(json!({
            "reset_token": "not-a-real-reset-token",
            "new_password": "another-long-password",
        })),
        "/v1/customer/api-keys" => ProbeBody::Json(json!({"label": "probe"})),
        other => panic!(
            "对客面新增了 {method} {other}：给它配一个能被解析的请求体，否则这里只能证明请求体没过解析，证不了鉴权。"
        ),
    }
}

/// 对客面**不需要凭据就能访问的端点**只有那三个自助动作，外加只读的公开目录（V-C11 的前半）。
///
/// 做法是从路由表源码里把 `/v1/…` 的每一条读出来、逐个**不带任何凭据**打一遍，断言未认证可达的恰好
/// 是注册、登录、凭令牌兑换（外加只读的公开目录，理由见 [`PUBLIC_CATALOGUE_ROUTE`]）。手抄清单会把
/// "新增一条忘了挂鉴权的端点"放过去，而这条用例的失败面正是路由表多出一条。
///
/// 三条自助动作断言的是**具体状态码**而不是"不是 401"：201/400 说明请求真的走进了处理器，若某条改回
/// 401（被误挂上鉴权），这条用例也会红。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_credential_free_customer_surface_is_the_public_catalog_and_the_three_self_service_actions()
 {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let routes = customer_routes_in_the_route_table();
    assert!(
        routes.len() >= 16,
        "路由表里解析出的对客端点太少（{}），多半是解析没跟上写法：{routes:?}",
        routes.len()
    );
    for expected in SELF_SERVICE_ROUTES {
        assert!(
            routes.contains(&(expected.0.to_owned(), expected.1.to_owned())),
            "路由表里找不到 {} {}：这条判据是拿它作基准的",
            expected.0,
            expected.1
        );
    }

    let mut reached = Vec::new();
    for (method, path) in &routes {
        let path = concrete_path(path);
        let request = client.request(
            method.parse().expect("HTTP method"),
            format!("{base_url}{path}"),
        );
        let request = match probe_body(method, &path) {
            ProbeBody::None => request,
            ProbeBody::Json(body) => request.json(&body),
            ProbeBody::Multipart => request
                .multipart(reqwest::multipart::Form::new().text("prompt", "credential-free probe")),
        };
        let response = request.send().await.expect("credential-free probe");
        let status = response.status();
        let body = response.text().await.expect("probe body");
        match (method.as_str(), path.as_str()) {
            ("POST", "/v1/customers") => assert_eq!(
                status,
                StatusCode::CREATED,
                "注册本来就不需要凭据，必须走通到建号：{body}"
            ),
            ("POST", "/v1/customer/sessions") => assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "登录不需要凭据，邮箱不存在该是登录失败而不是未认证：{body}"
            ),
            ("POST", "/v1/customer/password-resets/redeem") => assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "兑换只认令牌本身、不需要会话：令牌不对该是参数错而不是未认证：{body}"
            ),
            ("GET", "/v1/models") => {
                assert_eq!(status, StatusCode::OK, "公开目录不需要凭据：{body}")
            }
            _ => assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "{method} {path} 不带凭据时必须未认证，实际 {status}：{body}"
            ),
        }
        if status != StatusCode::UNAUTHORIZED {
            reached.push(format!("{method} {path}"));
        }
    }

    let mut expected: Vec<String> = SELF_SERVICE_ROUTES
        .iter()
        .copied()
        .chain([PUBLIC_CATALOGUE_ROUTE])
        .map(|(method, path)| format!("{method} {path}"))
        .collect();
    expected.sort();
    reached.sort();
    assert_eq!(
        reached, expected,
        "未认证可达的对客端点变了：多出来的那条要么该挂上鉴权，要么得说明它为什么是公开的"
    );

    drop_isolated_database(&database_name).await;
}

/// 对客面**没有**"提交邮箱就拿到重置令牌"的入口（V-C11 的后半）：那条路等于"知道邮箱就能接管账户"。
///
/// 这不是漏做——平台不发邮件、不做邮箱验证，所以重置只能由运营在管理端签发后转交。所以候选路径要
/// **不存在**（404，不是 2xx、也不是参数错），而且一圈试完之后库里**一条重置令牌都没有**：只看状态码
/// 的话，"某个候选悄悄建了一行、再把答复改成 404"照样过。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn there_is_no_self_service_password_reset_entry() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    // 名字都是"提交邮箱就发令牌"这类入口的常见写法，含对客前缀与误挂到管理前缀下的那种。
    let candidates = [
        ("POST", "/v1/customer/password-resets"),
        ("POST", "/v1/customer/password-reset"),
        ("POST", "/v1/customer/password/forgot"),
        ("POST", "/v1/customer/forgot-password"),
        ("POST", "/v1/customer/password-resets/request"),
        ("POST", "/v1/customers/password-resets"),
        ("POST", "/v1/password-resets"),
        ("POST", "/v1/password-reset"),
        ("GET", "/v1/customer/password-resets"),
        ("POST", "/api/v1/customer/password-resets"),
    ];
    let body = json!({
        "email": "anyone@example.com",
        "new_password": "a-long-enough-password",
    });
    for (method, path) in candidates {
        let request = client.request(
            method.parse().expect("HTTP method"),
            format!("{base_url}{path}"),
        );
        let request = if method == "GET" {
            request
        } else {
            request.json(&body)
        };
        let response = request.send().await.expect("probe request");
        let status = response.status();
        let answer = response.text().await.expect("probe body");
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "{method} {path} 不该存在：{answer}"
        );
        assert!(
            !answer.contains("reset_token"),
            "{method} {path} 回了令牌：{answer}"
        );
    }

    let pool = PgPool::connect(&database_url).await.expect("test pool");
    let issued: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM identity.password_resets")
        .fetch_one(&pool)
        .await
        .expect("reset token count");
    pool.close().await;
    assert_eq!(issued, 0, "没有任何一条提交邮箱的路径该发出重置令牌");

    drop_isolated_database(&database_name).await;
}

/// 运营可以不给初始口令开户，改签一枚重置令牌让客户自己设口令（V-C14 的后半条）。
///
/// 不给口令与"口令那一列是空的"只差一个实现细节，而后者等于谁都能登录。所以先证明不给口令时
/// **谁都进不来**，再证明令牌能把口令换成客户自己选的、且换完就能登录。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn an_account_opened_without_a_password_is_entered_via_a_reset_token() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let email = "no-initial-password@example.com";
    let opened = client
        .post(format!("{base_url}/api/v1/customers"))
        .bearer_auth(&admin_token)
        .json(&json!({"email": email}))
        .send()
        .await
        .expect("open customer request");
    assert_eq!(
        opened.status(),
        StatusCode::CREATED,
        "不给初始口令也要能开户"
    );
    let account_id = opened.json::<Value>().await.expect("open body")["account_id"]
        .as_str()
        .expect("account id")
        .to_owned();

    // 口令还没设：随便拿一个口令都进不来。
    let before = client
        .post(format!("{base_url}/v1/customer/sessions"))
        .json(&json!({"email": email, "password": "a-long-enough-password"}))
        .send()
        .await
        .expect("login request");
    assert_eq!(
        before.status(),
        StatusCode::BAD_REQUEST,
        "没设口令之前不该有人进得来"
    );

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
        .json(&json!({"reset_token": reset_token, "new_password": "a-long-enough-password"}))
        .send()
        .await
        .expect("redeem request");
    assert_eq!(redeemed.status(), StatusCode::NO_CONTENT);

    let login = client
        .post(format!("{base_url}/v1/customer/sessions"))
        .json(&json!({"email": email, "password": "a-long-enough-password"}))
        .send()
        .await
        .expect("login request");
    assert_eq!(login.status(), StatusCode::OK, "凭令牌设完口令必须能登录");
    let login = login.json::<Value>().await.expect("login body");
    assert_eq!(
        login["account_id"],
        json!(account_id),
        "登录上的必须就是刚才开户的那个账户"
    );
    let session = login["token"].as_str().expect("session token").to_owned();
    let account = client
        .get(format!("{base_url}/v1/customer/account"))
        .bearer_auth(&session)
        .send()
        .await
        .expect("account request")
        .json::<Value>()
        .await
        .expect("account body");
    assert_eq!(
        account["balance_microusd"],
        json!(0),
        "这是个新开的空账户：{account}"
    );

    drop_isolated_database(&database_name).await;
}

/// 共享 `ADMIN_TOKEN` 调不通"只认会话"的三条端点，答复与"凭据不对"逐字相同；而本次新增的
/// 管理读端点仍然认它（V-A6）。
///
/// 三条各自验一次，不能只验认身份：共享令牌不指向任何一个人，用它改口令或退出，做出来的是
/// "改了某个不存在的人"和"退了一个不存在的登录"——两种都会让运维以为动作生效了。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_shared_token_cannot_answer_the_three_session_only_endpoints() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    // 用一把不存在的会话令牌当"凭据不对"的对照。
    let bogus = "not-a-real-session";

    // 认身份。
    let shared = client
        .get(format!("{base_url}/api/v1/admin/session"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("shared identity request");
    let wrong = client
        .get(format!("{base_url}/api/v1/admin/session"))
        .bearer_auth(bogus)
        .send()
        .await
        .expect("bogus identity request");
    assert_eq!(shared.status(), StatusCode::FORBIDDEN);
    assert_eq!(shared.status(), wrong.status());
    assert_eq!(
        shared.json::<Value>().await.expect("identity body"),
        wrong.json::<Value>().await.expect("identity body"),
        "共享令牌与错凭据必须回同一个答复"
    );

    // 改口令。
    let change = json!({
        "current_password": "a-long-enough-password",
        "new_password": "another-long-enough-password",
    });
    let shared = client
        .put(format!("{base_url}/api/v1/admin/password"))
        .bearer_auth(&admin_token)
        .json(&change)
        .send()
        .await
        .expect("shared change request");
    let wrong = client
        .put(format!("{base_url}/api/v1/admin/password"))
        .bearer_auth(bogus)
        .json(&change)
        .send()
        .await
        .expect("bogus change request");
    assert_eq!(shared.status(), StatusCode::FORBIDDEN);
    assert_eq!(shared.status(), wrong.status());
    assert_eq!(
        shared.json::<Value>().await.expect("change body"),
        wrong.json::<Value>().await.expect("change body"),
        "共享令牌与错凭据必须回同一个答复"
    );

    // 退出。
    let shared = client
        .delete(format!("{base_url}/api/v1/admin/sessions"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("shared logout request");
    let wrong = client
        .delete(format!("{base_url}/api/v1/admin/sessions"))
        .bearer_auth(bogus)
        .send()
        .await
        .expect("bogus logout request");
    assert_eq!(shared.status(), StatusCode::FORBIDDEN);
    assert_eq!(shared.status(), wrong.status());
    assert_eq!(
        shared.json::<Value>().await.expect("logout body"),
        wrong.json::<Value>().await.expect("logout body"),
        "共享令牌与错凭据必须回同一个答复"
    );

    // 新增的管理读端点仍然认共享令牌——它是自动化与运维自救的凭据，不能被会话这条改动顺手废掉。
    let rates = client
        .get(format!("{base_url}/api/v1/fx-rates"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("fx rates request");
    assert_eq!(
        rates.status(),
        StatusCode::OK,
        "共享令牌必须能调新增的管理读端点"
    );
    let rates = rates.json::<Value>().await.expect("fx rates body");
    assert!(
        rates["rates"]
            .as_array()
            .is_some_and(|rates| !rates.is_empty()),
        "读端点必须真的取到夹具那几条折算率：{rates}"
    );

    drop_isolated_database(&database_name).await;
}
