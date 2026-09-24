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
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    // ── 对客没有异步入口：受理与查询两条路径都**不存在**（不是 202、也不是 401）──
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

/// 吊销一把 API Key 之后它**立刻**不能再用来受理：同一条请求、同一个 API 进程，吊销前成功、
/// 吊销后被拒。
///
/// "立刻"是这种动作的全部意义——管理员按下去，就是要从这一刻起停止受理，所以用例中间不重启进程、
/// 不等任何窗口、不换请求形状，紧接着用同一把明文密钥再打一次。凭据值本身没有变，变的是库里的
/// `revoked_at`，因此这条用例同时钉住"认证每次按密钥标识读库判吊销状态"：一旦有人在认证链路上加了
/// 按密钥的缓存，这里就会在缓存寿命内放行一把已吊销的密钥，用例立刻变红。
///
/// 吊销不删行（创建与吊销都是历史事实），所以还要验重复吊销仍然成功、且不改第一次的吊销时刻。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn revoked_api_key_stops_working_immediately_and_revoke_is_idempotent() {
    let harness =
        Harness::start_with_bootstrap(UpstreamBehaviour::aihubmix(SyncImageShape::Url), 64).await;
    let client = Client::new();
    // 被吊销的密钥属于夹具之外的账户：吊销改的是那把密钥自己，别顺手把夹具的密钥也停掉。
    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;

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

    // ── 吊销：204，与既有管理员写接口一致；痕迹落在 `revoked_at` 上而不是删行 ──
    let key_id: Uuid = sqlx::query_scalar("SELECT id FROM identity.api_keys WHERE account_id = $1")
        .bind(Uuid::parse_str(&account_id).expect("account id"))
        .fetch_one(&harness.pool)
        .await
        .expect("the issued key must be in the database");
    let revoked = client
        .delete(format!("{}/api/v1/api-keys/{key_id}", harness.base_url))
        .bearer_auth(&harness.admin_token)
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
        .delete(format!("{}/api/v1/api-keys/{key_id}", harness.base_url))
        .bearer_auth(&harness.admin_token)
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
    let revoke_audits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM operations.audit_events WHERE action = 'api_key.revoke' AND subject_id = $1",
    )
    .bind(key_id.to_string())
    .fetch_one(&harness.pool)
    .await
    .expect("revocation audit");
    assert_eq!(revoke_audits, 1, "吊销必须留下一条审计，且只留一条");

    // ── 不存在的键：404（这里只给已经发出来的行盖章，不创建任何东西）──
    let missing = client
        .delete(format!(
            "{}/api/v1/api-keys/{}",
            harness.base_url,
            Uuid::new_v4()
        ))
        .bearer_auth(&harness.admin_token)
        .send()
        .await
        .expect("missing key revocation");
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    // ── 无管理员凭证：401（与既有管理员写接口同一条鉴权路径）──
    let unauthorized = client
        .delete(format!("{}/api/v1/api-keys/{key_id}", harness.base_url))
        .send()
        .await
        .expect("unauthorized revocation");
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    harness.cleanup().await;
}
