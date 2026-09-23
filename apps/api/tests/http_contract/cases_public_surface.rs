use super::*;

#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn public_surface_has_no_async_task_protocol() {
    let (database_url, database_name) = isolated_database_url().await;
    // 这个用例不起 Worker：同步入口必然等到超时，正好用来验"等不到时对客怎么说"。
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url).await;

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

    let account = create_account(&client, &base_url, &admin_token).await;
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
