//! 身份邮箱互斥（控制台 Spec v21 A6/C2/C13/§6）：同一个邮箱不能同时是管理员与客户的登录身份。

use super::*;
use seeai_application::{ApplicationError, HubRepository as _};
use serde_json::json;

/// 用管理员邮箱注册客户：回与普通重复注册**同一句话**的冲突，不透露它是管理员账号，且不产生写入。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_customer_cannot_register_with_an_admin_email() {
    let (database_url, database_name) = isolated_database_url().await;
    let admin_email = "ops@example.com";
    let password = "a-long-enough-password";
    let (base_url, admin_token, _process) =
        start_api_with_admin(&database_url, admin_email, password).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    // 先拿"普通重复注册"的那句话做基准。
    let plain_email = "plain-duplicate@example.com";
    let plain_body = json!({ "email": plain_email, "password": password });
    let first = client
        .post(format!("{base_url}/v1/customers"))
        .json(&plain_body)
        .send()
        .await
        .expect("plain register");
    assert_eq!(first.status(), StatusCode::CREATED);
    let duplicate = client
        .post(format!("{base_url}/v1/customers"))
        .json(&plain_body)
        .send()
        .await
        .expect("plain duplicate");
    assert_eq!(duplicate.status(), StatusCode::CONFLICT);
    let duplicate_message = duplicate
        .json::<Value>()
        .await
        .expect("duplicate body is JSON")["error"]["message"]
        .as_str()
        .expect("message is a string")
        .to_owned();

    // "不产生写入"要直接数行，不能只靠"登录回 400"：注册路径先建账户再建身份，
    // 交叉检查若被挪到建账户之后，会留下孤儿账户而登录照样回 400。
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");
    let accounts_before: i64 = sqlx::query_scalar("SELECT count(*) FROM ledger.accounts")
        .fetch_one(&pool)
        .await
        .expect("account count");

    let response = client
        .post(format!("{base_url}/v1/customers"))
        .json(&json!({ "email": admin_email, "password": password }))
        .send()
        .await
        .expect("register request");
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body: Value = response.json().await.expect("conflict body is JSON");
    assert_eq!(body["error"]["code"], json!("conflict"));
    let expected = duplicate_message.replace(plain_email, admin_email);
    assert_eq!(
        body["error"]["message"].as_str(),
        Some(expected.as_str()),
        "管理员邮箱必须与普通重复注册回同一句话"
    );

    let accounts_after: i64 = sqlx::query_scalar("SELECT count(*) FROM ledger.accounts")
        .fetch_one(&pool)
        .await
        .expect("account count");
    assert_eq!(accounts_after, accounts_before, "被拒的注册不能留下账户");
    let customers: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM identity.customers WHERE lower(email) = lower($1)",
    )
    .bind(admin_email)
    .fetch_one(&pool)
    .await
    .expect("customer count");
    assert_eq!(customers, 0, "被拒的注册不能留下客户身份");
    let audits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM operations.audit_events WHERE action = 'customer.register' AND payload->>'email' = $1",
    )
    .bind(admin_email)
    .fetch_one(&pool)
    .await
    .expect("audit count");
    assert_eq!(audits, 0, "被拒的注册不能留下审计");
    pool.close().await;

    // 客户面用这个邮箱登录回参数错误（"邮箱不存在"与"口令不对"同一句），不是 200。
    let login = client
        .post(format!("{base_url}/v1/customer/sessions"))
        .json(&json!({ "email": admin_email, "password": password }))
        .send()
        .await
        .expect("customer login");
    assert_eq!(login.status(), StatusCode::BAD_REQUEST);

    drop_isolated_database(&database_name).await;
}

/// 运营用管理员邮箱替客户开户：开新账户与给已有账户配身份两条路径都回同一句话的冲突。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_operator_cannot_open_a_customer_with_an_admin_email() {
    let (database_url, database_name) = isolated_database_url().await;
    let admin_email = "ops@example.com";
    let password = "a-long-enough-password";
    let (base_url, admin_token, _process) =
        start_api_with_admin(&database_url, admin_email, password).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let plain_email = "plain-open@example.com";
    let plain_body = json!({ "email": plain_email, "password": password });
    let first = client
        .post(format!("{base_url}/api/v1/customers"))
        .bearer_auth(&admin_token)
        .json(&plain_body)
        .send()
        .await
        .expect("plain open");
    assert_eq!(first.status(), StatusCode::CREATED);
    let opened: Value = first.json().await.expect("open body is JSON");
    let account_id = opened["account_id"]
        .as_str()
        .expect("account id is a string")
        .to_owned();

    let duplicate = client
        .post(format!("{base_url}/api/v1/customers"))
        .bearer_auth(&admin_token)
        .json(&plain_body)
        .send()
        .await
        .expect("plain duplicate");
    assert_eq!(duplicate.status(), StatusCode::CONFLICT);
    let duplicate_message = duplicate
        .json::<Value>()
        .await
        .expect("duplicate body is JSON")["error"]["message"]
        .as_str()
        .expect("message is a string")
        .to_owned();
    let expected = duplicate_message.replace(plain_email, admin_email);

    // 开新账户那条路径。
    let response = client
        .post(format!("{base_url}/api/v1/customers"))
        .bearer_auth(&admin_token)
        .json(&json!({ "email": admin_email, "password": password }))
        .send()
        .await
        .expect("open customer request");
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body: Value = response.json().await.expect("conflict body is JSON");
    assert_eq!(body["error"]["code"], json!("conflict"));
    assert_eq!(body["error"]["message"].as_str(), Some(expected.as_str()));

    // 给已有账户配身份那条路径。
    let bind = client
        .post(format!("{base_url}/api/v1/customers"))
        .bearer_auth(&admin_token)
        .json(&json!({ "email": admin_email, "password": password, "account_id": account_id }))
        .send()
        .await
        .expect("bind admin email");
    assert_eq!(bind.status(), StatusCode::CONFLICT);
    let bind_body: Value = bind.json().await.expect("conflict body is JSON");
    assert_eq!(bind_body["error"]["code"], json!("conflict"));
    assert_eq!(
        bind_body["error"]["message"].as_str(),
        Some(expected.as_str())
    );

    drop_isolated_database(&database_name).await;
}

/// 引导用的邮箱已被客户占用：进程启动失败，并在 stderr 里点名该邮箱。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn seeding_an_admin_with_a_customer_email_fails_and_names_it() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let email = "seed-conflict@example.com";
    let password = "a-long-enough-password";
    let created = client
        .post(format!("{base_url}/v1/customers"))
        .json(&json!({ "email": email, "password": password }))
        .send()
        .await
        .expect("register request");
    assert_eq!(created.status(), StatusCode::CREATED);

    let (running, stderr) =
        probe_api_startup_with_seed_stderr(&database_url, email, password).await;
    assert!(!running, "引导撞上客户邮箱时进程必须退出");
    assert!(
        stderr.contains(email),
        "启动报错要点名该邮箱；实际 stderr：{stderr}"
    );

    drop_isolated_database(&database_name).await;
}

/// 跨域的"注册 vs 管理员建号"不能都成功：另开一条连接先持住身份邮箱锁，注册请求会等在锁上；
/// 期间插入管理员行，释放后注册必须看到它并回冲突。锁被拿掉时请求会在插入前通过检查，这里就会红。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_registration_cannot_slip_past_a_concurrent_admin_claim() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let email = "cross-domain-race@example.com";
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");
    let mut holder = pool.begin().await.expect("holder transaction");
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('identity-email:' || lower($1), 0))",
    )
    .bind(email)
    .execute(&mut *holder)
    .await
    .expect("hold identity email lock");

    let request_client = client.clone();
    let url = format!("{base_url}/v1/customers");
    let race_email = email.to_owned();
    let register = tokio::spawn(async move {
        request_client
            .post(url)
            .json(&json!({ "email": race_email, "password": "a-long-enough-password" }))
            .send()
            .await
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !register.is_finished(),
        "注册请求应当等在身份邮箱锁上，而不是已经通过交叉检查"
    );

    sqlx::query("INSERT INTO identity.admin_users (id, email, password_hash) VALUES ($1, $2, $3)")
        .bind(Uuid::new_v4())
        .bind(email)
        .bind("hash")
        .execute(&pool)
        .await
        .expect("admin fixture");
    holder.commit().await.expect("release identity email lock");

    let response = register
        .await
        .expect("register task joins")
        .expect("register request");
    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "注册必须等在锁上并看到并发插入的管理员"
    );

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// 同一邮箱的两个并发注册只成功一个：咨询锁让"检查 + 插入"不交错，另一个回冲突而不是 500。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn concurrent_registrations_with_the_same_email_yield_one_winner() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let body = json!({ "email": "concurrent@example.com", "password": "a-long-enough-password" });
    let (first, second) = tokio::join!(
        client
            .post(format!("{base_url}/v1/customers"))
            .json(&body)
            .send(),
        client
            .post(format!("{base_url}/v1/customers"))
            .json(&body)
            .send(),
    );
    let mut statuses = [
        first.expect("first request").status(),
        second.expect("second request").status(),
    ];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::CREATED, StatusCode::CONFLICT]);

    drop_isolated_database(&database_name).await;
}

/// 仓储的按邮箱 upsert 也可能新建管理员：客户身份占用的邮箱必须被拒。这条路径没有生产调用点，
/// 直接调仓储把守卫钉住。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_repository_refuses_to_upsert_an_admin_on_a_customer_email() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let email = "upsert-conflict@example.com";
    let created = client
        .post(format!("{base_url}/v1/customers"))
        .json(&json!({ "email": email, "password": "a-long-enough-password" }))
        .send()
        .await
        .expect("register request");
    assert_eq!(created.status(), StatusCode::CREATED);

    let repository = seeai_persistence::PgHubRepository::connect(&database_url, 2)
        .await
        .expect("repository");
    let error = repository
        .upsert_admin_password(email, "hash")
        .await
        .expect_err("客户身份占用的邮箱不能建管理员");
    assert!(
        matches!(error, ApplicationError::Configuration(_)),
        "客户邮箱占用的管理员 upsert 应当是配置错误：{error}"
    );
    assert!(
        format!("{error}").contains(email),
        "报错要点名冲突邮箱；实际：{error}"
    );

    drop_isolated_database(&database_name).await;
}

/// 反向时序：客户注册先占住邮箱、管理员引导后进来，同样不能两边都成功。
///
/// 直接调仓储的 ensure_admin_account，并用另一条连接持住同一把锁把并发固定下来，不靠 HTTP 时序。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn an_admin_claim_cannot_slip_past_a_concurrent_customer_registration() {
    let (database_url, database_name) = isolated_database_url().await;
    let repository = seeai_persistence::PgHubRepository::connect(&database_url, 2)
        .await
        .expect("repository");
    repository.migrate().await.expect("migrations apply");

    let email = "admin-race@example.com";
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");
    let mut holder = pool.begin().await.expect("holder transaction");
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('identity-email:' || lower($1), 0))",
    )
    .bind(email)
    .execute(&mut *holder)
    .await
    .expect("hold identity email lock");

    let claim_email = email.to_owned();
    let claim =
        tokio::spawn(async move { repository.ensure_admin_account(&claim_email, "hash").await });
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !claim.is_finished(),
        "引导应当等在身份邮箱锁上，而不是已经通过交叉检查"
    );

    // 期间客户注册把邮箱占了（直接写行，等价于注册事务提交后的状态）。
    let account = Uuid::new_v4();
    sqlx::query("INSERT INTO ledger.accounts (id, name, balance_microusd) VALUES ($1, $2, 0)")
        .bind(account)
        .bind("admin_race_account")
        .execute(&pool)
        .await
        .expect("account fixture");
    sqlx::query(
        "INSERT INTO identity.customers (id, email, password_hash, account_id) VALUES ($1, $2, $3, $4)",
    )
    .bind(Uuid::new_v4())
    .bind(email)
    .bind("hash")
    .bind(account)
    .execute(&pool)
    .await
    .expect("customer fixture");
    holder.commit().await.expect("release identity email lock");

    let error = claim
        .await
        .expect("claim task joins")
        .expect_err("引导必须看到并发占用的客户邮箱");
    assert!(
        matches!(error, ApplicationError::Configuration(_)),
        "引导撞客户邮箱应当是配置错误：{error}"
    );
    assert!(
        format!("{error}").contains(email),
        "报错要点名冲突邮箱；实际：{error}"
    );

    pool.close().await;
    drop_isolated_database(&database_name).await;
}
