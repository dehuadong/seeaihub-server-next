use super::*;

/// 健康检查探的是**事实源**：数据库可达就 `200 {"status":"ok"}`。
///
/// 只有健康这一侧能自动化：装置是"起 API 进程 + 等 `/health` 就绪"，数据库不可达时进程根本起不来，
/// 拿不到一个"正在服务、但库连不上"的实例。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn health_reports_ok_when_the_database_is_reachable() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    // 只判 body 逐字相等：多一个字段就意味着对客多暴露了一份内部状态。
    let response = client
        .get(format!("{base_url}/health"))
        .send()
        .await
        .expect("health request");
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.json::<Value>().await.expect("health body is JSON");
    assert_eq!(body, json!({"status": "ok"}));

    drop_isolated_database(&database_name).await;
}

/// 同一个实例**跑着的时候**事实源没了 ⇒ 探活变红：`503 {"status":"unhealthy","database":"unreachable"}`。
///
/// 做法是把这个用例的**整个空库删掉**（探活连的就是它），而不是去动数据库服务：实例不必重启，
/// 库确实不可达，这正是编排系统该看到的那个状态。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn health_reports_unhealthy_once_the_database_is_gone() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    // 先把库删掉：池里已有的连接大多是空闲的，第一次探活会撞上"库不存在"。
    drop_isolated_database(&database_name).await;

    // 探活是只读幂等的，所以"等它意识到"就是重复探——不等的话这条用例会去赌连接池的实现细节。
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut last_status = StatusCode::OK;
    let mut last_body = Value::Null;
    while std::time::Instant::now() < deadline {
        let response = client
            .get(format!("{base_url}/health"))
            .send()
            .await
            .expect("health request");
        last_status = response.status();
        last_body = response.json::<Value>().await.expect("health body is JSON");
        if last_status == StatusCode::SERVICE_UNAVAILABLE {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    assert_eq!(
        last_status,
        StatusCode::SERVICE_UNAVAILABLE,
        "事实源不可达时探活必须变红，而不是一直说 ok：{last_body}"
    );
    assert_eq!(
        last_body,
        json!({"status": "unhealthy", "database": "unreachable"}),
        "对客只报是哪一层不可达，不多说内部细节"
    );
}

#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn public_surface_has_no_async_task_protocol() {
    let (database_url, database_name) = isolated_database_url().await;
    // 这个用例不起 Worker：同步入口必然等到超时，正好用来验"等不到时对客怎么说"。
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    let unauthorized = client
        .post(format!("{base_url}/api/v1/accounts"))
        .json(&json!({"initial_credit_microusd": 1}))
        .send()
        .await
        .expect("unauthorized request");
    assert_eq!(unauthorized.status(), StatusCode::FORBIDDEN);

    // ── 对客没有异步入口：受理与查询两条路径都**不存在**（不是 202、也不是鉴权失败）──
    let removed_accept = client
        .post(format!("{base_url}/v1/image-generations"))
        .json(&json!({"model": "gpt-image-2", "prompt": "x"}))
        .send()
        .await
        .expect("removed accept route");
    assert_eq!(
        removed_accept.status(),
        StatusCode::NOT_FOUND,
        "受理入口不能再存在"
    );
    let removed_query = client
        .get(format!(
            "{base_url}/v1/image-generations/{}",
            Uuid::new_v4()
        ))
        .send()
        .await
        .expect("removed query route");
    assert_eq!(
        removed_query.status(),
        StatusCode::NOT_FOUND,
        "查询入口不能再存在"
    );
    // 资产接口同样不存在。
    for (method, path) in [
        ("POST", "/v1/assets"),
        ("GET", &format!("/v1/assets/{}", Uuid::new_v4())),
    ] {
        let response = match method {
            "POST" => client.post(format!("{base_url}{path}")).send().await,
            _ => client.get(format!("{base_url}{path}")).send().await,
        }
        .expect("removed asset route");
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "资产接口不能存在：{method} {path}"
        );
    }

    // 素材带保底表，受理闸门是"余额 ≥ 保底额"：这个用例看的是"等不到结果时对客怎么说"，
    // 所以账户要付得起那份保底额，别让 402 抢在超时前面。
    let account = create_account_with_credit(&client, &base_url, &admin_token, 1_000_000).await;
    let api_key = issue_key(&client, &base_url, &admin_token, &account).await;
    publish_bootstrap(&client, &base_url, &admin_token).await;
    reject_mismatched_model_identity(&client, &base_url, &admin_token).await;

    // ── 等不到结果时：普通的超时错误，不提 job、不指路查询接口 ──
    let key = format!("pending-{}", Uuid::new_v4());
    let request = generation_request(&key, "contract prompt");
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &strip_key(&request),
    )
    .await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "got {body}");
    assert_public_only("超时", &body);
    assert_eq!(body["error"]["code"].as_str(), Some("result_pending"));

    // 同一个幂等键重发：仍然只留下**一条**内部记录（重发不是新任务）。
    let (again_status, again) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &strip_key(&request),
    )
    .await;
    assert_eq!(again_status, StatusCode::GATEWAY_TIMEOUT);
    assert_public_only("超时重发", &again);
    let job_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM generation.jobs WHERE idempotency_key = $1")
            .bind(&key)
            .fetch_one(&PgPool::connect(&database_url).await.expect("pool"))
            .await
            .expect("job count");
    assert_eq!(job_count, 1, "幂等键重发必须去重成同一条内部记录");

    // 同一个幂等键、不同的请求体：冲突。
    let conflict = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &generation_request_body("changed prompt"),
    )
    .await;
    assert_eq!(conflict.0, StatusCode::CONFLICT, "got {}", conflict.1);

    // ── 跨账户隔离（内部接口层）：别的账户看不到这条记录 ──
    let pool = PgPool::connect(&database_url).await.expect("pool");
    let job_id: Uuid =
        sqlx::query_scalar("SELECT id FROM generation.jobs WHERE idempotency_key = $1")
            .bind(&key)
            .fetch_one(&pool)
            .await
            .expect("job id");
    let other_account = create_account(&client, &base_url, &admin_token).await;
    let other_id = Uuid::parse_str(&other_account).expect("account id");
    let repository = PgHubRepository::connect(&database_url, 2)
        .await
        .expect("repository");
    assert!(
        repository
            .get_job(AccountId(other_id), seeai_domain::JobId(job_id))
            .await
            .is_err(),
        "别的账户不许看到这条记录"
    );

    verify_reconciliation_contract(&client, &base_url, &admin_token, &api_key, &database_url).await;
    verify_lease_recovery_contract(&client, &base_url, &admin_token, &api_key, &database_url).await;
    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// 客户吊销自己的 API Key 之后它**立刻**不能再用来受理：同一条请求、同一个 API 进程，吊销前成功、
/// 吊销后被拒。
///
/// "立刻"是这种动作的全部意义——客户按下去，就是要从这一刻起停止受理，所以用例中间不重启进程、
/// 不等任何窗口、不换请求形状，紧接着用同一把明文密钥再打一次。凭据值本身没有变，变的是库里的
/// `revoked_at`，因此这条用例同时钉住"认证每次按密钥标识读库判吊销状态"：一旦有人在认证链路上加了
/// 按密钥的缓存，这里就会在缓存寿命内放行一把已吊销的密钥，用例立刻变红。
///
/// 吊销不删行（创建与吊销都是历史事实），所以还要验重复吊销仍然成功、且不改第一次的吊销时刻。
///
/// **吊销只由客户自己做**（Spec `0001` M5、V-D16）：这条用例走客户会话，并顺带确认管理面那条
/// `DELETE /api/v1/api-keys/{key_id}` 已经不存在——运营判断不了客户是否在用，也拿不到要吊销的标识。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn revoked_api_key_stops_working_immediately_and_revoke_is_idempotent() {
    let harness =
        Harness::start_with_bootstrap(UpstreamBehaviour::aihubmix(SyncImageShape::Url), 64).await;
    let client = Client::new();
    // 被吊销的密钥属于夹具之外的账户：吊销改的是那把密钥自己，别顺手把夹具的密钥也停掉。
    let account_id =
        create_account_with_credit(&client, &harness.base_url, &harness.admin_token, 1_000_000)
            .await;
    let issued = issue_key_response(
        &client,
        &harness.base_url,
        &harness.admin_token,
        &account_id,
    )
    .await;
    // 发密钥的响应仍然同时给明文与标识：标识是密钥自己的身份，列表与吊销都用它（界面上不再显示，
    // 接口字段不变）。
    assert!(
        issued.get("key_id").is_some_and(Value::is_string),
        "发密钥的响应必须带密钥标识：{issued}"
    );
    let api_key = issued["api_key"]
        .as_str()
        .expect("发密钥的响应必须带明文密钥")
        .to_owned();
    let key_id =
        Uuid::parse_str(issued["key_id"].as_str().expect("key_id")).expect("密钥标识得是个 UUID");

    // 管理面已经没有吊销这条路：拿管理员令牌打旧接口是"没有这个端点"，不是"没权限"。
    let gone = client
        .delete(format!("{}/api/v1/api-keys/{key_id}", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .send()
        .await
        .expect("admin revocation request");
    assert_eq!(
        gone.status(),
        StatusCode::NOT_FOUND,
        "管理面不该还留着吊销接口"
    );

    // 运营给这个账户配一个登录身份，客户用会话吊销自己的密钥（这是现在唯一的吊销路径）。
    let email = format!("revoke-{}@example.com", Uuid::new_v4());
    let password = "a-long-enough-password";
    let opened = client
        .post(format!("{}/api/v1/customers", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .json(&json!({"email": email, "password": password, "account_id": account_id}))
        .send()
        .await
        .expect("open customer request");
    assert_eq!(opened.status(), StatusCode::CREATED, "运营替客户配登录身份");
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

    // 夹具自检：响应给的标识指向刚发出来的那一行、且属于这个账户。
    let stored_account: Uuid =
        sqlx::query_scalar("SELECT account_id FROM identity.api_keys WHERE id = $1")
            .bind(key_id)
            .fetch_one(&harness.pool)
            .await
            .expect("响应里的标识必须能定位到刚发出来的那一行");
    assert_eq!(
        stored_account.to_string(),
        account_id,
        "标识得指向这个账户的密钥"
    );

    // ── 吊销前：这把密钥能受理并跑完一次生成（进程内假上游，不产生任何外部调用）──
    let worker = harness.spawn_worker();
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &format!("revoke-before-{}", Uuid::new_v4()),
        &route_request(harness.model, "before revocation"),
    )
    .await;
    drop(worker);
    assert_eq!(status, StatusCode::OK, "吊销前这把密钥必须可用：{body}");
    assert_sync_success("吊销前", &body);

    // ── 吊销：204，痕迹落在 `revoked_at` 上而不是删行 ──
    let revoked = client
        .delete(format!(
            "{}/v1/customer/api-keys/{key_id}",
            harness.base_url
        ))
        .bearer_auth(&session)
        .send()
        .await
        .expect("revocation request");
    assert_eq!(revoked.status(), StatusCode::NO_CONTENT);
    let revoked_at: Option<String> =
        sqlx::query_scalar("SELECT revoked_at::text FROM identity.api_keys WHERE id = $1")
            .bind(key_id)
            .fetch_one(&harness.pool)
            .await
            .expect("revoked key row");
    assert!(
        revoked_at.is_some(),
        "吊销必须写 revoked_at，而不是删掉这行"
    );

    // ── 吊销后：同一个进程、紧接着、同一把明文密钥 ⇒ 立刻按既有认证失败语义被拒 ──
    let (denied, denied_body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &format!("revoke-after-{}", Uuid::new_v4()),
        &route_request(harness.model, "after revocation"),
    )
    .await;
    assert_eq!(
        denied,
        StatusCode::UNAUTHORIZED,
        "吊销必须立刻生效，不能等任何窗口：{denied_body}"
    );
    assert_eq!(
        denied_body["error"]["code"].as_str(),
        Some("invalid_api_key"),
        "{denied_body}"
    );

    // ── 重复吊销仍然成功（幂等）：调用方在意的是"它现在不可用"，不是这次调用改变了什么 ──
    let again = client
        .delete(format!(
            "{}/v1/customer/api-keys/{key_id}",
            harness.base_url
        ))
        .bearer_auth(&session)
        .send()
        .await
        .expect("second revocation request");
    assert_eq!(again.status(), StatusCode::NO_CONTENT);
    let revoked_again: Option<String> =
        sqlx::query_scalar("SELECT revoked_at::text FROM identity.api_keys WHERE id = $1")
            .bind(key_id)
            .fetch_one(&harness.pool)
            .await
            .expect("revoked key row after the second call");
    assert_eq!(
        revoked_again, revoked_at,
        "重复吊销不能改写第一次盖章的时刻"
    );
    // 审计是"发生了什么事"的记录：吊销只发生一次，重复调用不该凭空多出一条。
    // 客户这条路把审计挂在**账户**上（subject_type = account），payload 里带密钥标识。
    let revoke_audits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM operations.audit_events
         WHERE action = 'api_key.revoke' AND actor = 'customer-self-service'
           AND subject_id = $1 AND payload->>'key_id' = $2",
    )
    .bind(account_id.clone())
    .bind(key_id.to_string())
    .fetch_one(&harness.pool)
    .await
    .expect("revocation audit");
    assert_eq!(revoke_audits, 1, "吊销必须留下一条审计，且只留一条");

    // ── 别的账户的密钥：客户按自己的账户吊销，指向别人的键就是"不存在"（不区分无权限）──
    let other = issue_key_response(
        &client,
        &harness.base_url,
        &harness.admin_token,
        &create_account_with_credit(&client, &harness.base_url, &harness.admin_token, 0).await,
    )
    .await;
    let other_key_id = Uuid::parse_str(other["key_id"].as_str().expect("key_id")).expect("uuid");
    let not_mine = client
        .delete(format!(
            "{}/v1/customer/api-keys/{other_key_id}",
            harness.base_url
        ))
        .bearer_auth(&session)
        .send()
        .await
        .expect("cross-account revocation request");
    assert_eq!(not_mine.status(), StatusCode::NOT_FOUND);
    let still_live: Option<String> =
        sqlx::query_scalar("SELECT revoked_at::text FROM identity.api_keys WHERE id = $1")
            .bind(other_key_id)
            .fetch_one(&harness.pool)
            .await
            .expect("other account key row");
    assert!(still_live.is_none(), "别家账户的密钥不能被吊销");

    // ── 没有会话：未认证（不是"不存在"）──
    let unauthorized = client
        .delete(format!(
            "{}/v1/customer/api-keys/{key_id}",
            harness.base_url
        ))
        .send()
        .await
        .expect("unauthorized revocation");
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    harness.cleanup().await;
}
