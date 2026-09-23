use super::*;

/// 并发上限：同一账户同时只能有 N 个在跑的生成任务（默认 1），多出来的在**受理前**就被拒。
///
/// 这条是"一次提交一堆把上游额度与平台成本一起打满"的第一道闸；跑完一个才能再提一个。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn concurrent_generations_are_capped() {
    let behaviour = UpstreamBehaviour {
        // 让第一台任务在轮询里多停一会儿：第二个请求必须在它在飞时到达。
        pending_times: 2,
        ..UpstreamBehaviour::apimart()
    };
    let harness = Harness::start_with(
        "APIMart",
        "apimart-image-v1",
        &["prompt_only"],
        None,
        behaviour,
        1,
    )
    .await;
    let base_url = harness.base_url.clone();
    let api_key = harness.api_key.clone();
    let model = harness.model;

    // 第一个请求在后台跑着（同步入口会等它跑完）；此时没有 Worker，它停在"已受理"。
    let first_key = format!("cap-0001-{}", Uuid::new_v4());
    let first_request = route_request(model, "first in flight");
    let first = tokio::spawn({
        let base_url = base_url.clone();
        let api_key = api_key.clone();
        let key = first_key.clone();
        let body = first_request.clone();
        async move { post_json(&base_url, &api_key, "/v1/images/generations", &key, &body).await }
    });

    // 等第一个请求真的受理了，再发第二个。
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let second_key = format!("cap-0002-{}", Uuid::new_v4());
    let (blocked, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &second_key,
        &route_request(model, "second while the first runs"),
    )
    .await;
    assert_eq!(
        blocked,
        StatusCode::TOO_MANY_REQUESTS,
        "a second job while the first is in flight must be rejected: {body}"
    );
    assert_eq!(body["error"]["code"].as_str(), Some("too_many_in_flight"));
    assert_public_only("并发上限", &body);

    let _worker = harness.spawn_worker();
    let (first_status, first_body) = first.await.expect("first request");
    assert_eq!(
        first_status,
        StatusCode::OK,
        "the first job must finish: {first_body}"
    );
    assert_sync_success("第一台任务", &first_body);

    // **同一个幂等键**的重发不算新任务：它去重成原来那条记录。
    let (retried, retried_body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &first_key,
        &first_request,
    )
    .await;
    assert_eq!(
        retried,
        StatusCode::OK,
        "a retry with the same idempotency key must get the same result back: {retried_body}"
    );
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM generation.jobs WHERE idempotency_key = $1")
            .bind(&first_key)
            .fetch_one(&harness.pool)
            .await
            .expect("job count");
    assert_eq!(count, 1, "重发必须去重成同一条内部记录");

    harness.cleanup().await;
}

/// 上游直接拒绝提交时，消费者看到的必须是**平台侧语义**：渠道的状态码、错误码、原文与上游标识一律不外泄。
///
/// 渠道说的"余额不足"指的是平台在渠道侧的账户，原样返回会让消费者去充值。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn channel_rejections_reach_consumers_as_platform_problems() {
    /// 上游逐请求标识（AIHubMix 错误信封里的 `tid`）：只该留在内部。
    const UPSTREAM_TRACE_ID: &str = "upstream-trace-9f3a";

    /// 一条"上游拒绝提交"的场景。字段多，用命名字段而不是位置元组，免得加行时错位。
    struct Rejected {
        provider_kind: &'static str,
        adapter_key: &'static str,
        status: u16,
        body: Value,
        channel_code: &'static str,
        channel_message: &'static str,
        expected_code: &'static str,
        expected_http: u16,
        expected_state: &'static str,
        /// 该终态对应的预授权处置。**单列**而不是从终态派生——派生出来的断言只能证明
        /// "两者一致"，发现不了错判。
        expected_hold: &'static str,
        /// 是否属平台侧事件（决定缺省清单列不列它）。
        platform_side: bool,
    }

    let cases = [
        Rejected {
            provider_kind: "APIMart",
            adapter_key: "apimart-image-v1",
            status: 402,
            body: json!({"error": {"code": 402, "message": "payment_required: account balance is insufficient"}}),
            channel_code: "402",
            channel_message: "payment_required: account balance is insufficient",
            expected_code: "platform_unavailable",
            expected_http: 502,
            expected_state: "failed",
            expected_hold: "released",
            platform_side: true,
        },
        Rejected {
            provider_kind: "APIMart",
            adapter_key: "apimart-image-v1",
            status: 403,
            body: json!({"error": {"code": 403, "message": "permission denied for this key"}}),
            channel_code: "403",
            channel_message: "permission denied for this key",
            expected_code: "platform_unavailable",
            expected_http: 502,
            expected_state: "failed",
            expected_hold: "released",
            platform_side: true,
        },
        Rejected {
            provider_kind: "APIMart",
            adapter_key: "apimart-image-v1",
            status: 500,
            body: json!({"error": {"code": 500, "message": "build_request_failed: invalid size 9999x9999"}}),
            channel_code: "500",
            channel_message: "build_request_failed: invalid size 9999x9999",
            expected_code: "platform_unavailable",
            expected_http: 502,
            expected_state: "failed",
            expected_hold: "released",
            platform_side: true,
        },
        // 以下三行是第一方写明"请求未执行"的三类：判为失败并释放预授权，不进对账。
        Rejected {
            provider_kind: "APIMart",
            adapter_key: "apimart-image-v1",
            status: 503,
            body: json!({"error": {"code": 503, "message": "idempotency_unavailable"}}),
            channel_code: "503",
            channel_message: "idempotency_unavailable",
            expected_code: "platform_unavailable",
            expected_http: 502,
            expected_state: "failed",
            expected_hold: "released",
            platform_side: false,
        },
        Rejected {
            provider_kind: "APIMart",
            adapter_key: "apimart-image-v1",
            status: 429,
            body: json!({"error": {"code": 429, "message": "rate_limit_error"}}),
            channel_code: "429",
            channel_message: "rate_limit_error",
            expected_code: "platform_unavailable",
            expected_http: 502,
            expected_state: "failed",
            expected_hold: "released",
            platform_side: false,
        },
        Rejected {
            provider_kind: "APIMart",
            adapter_key: "apimart-image-v1",
            status: 409,
            body: json!({"error": {"code": "idempotency_in_progress", "message": "the same key is in flight"}}),
            channel_code: "idempotency_in_progress",
            channel_message: "the same key is in flight",
            expected_code: "platform_unavailable",
            expected_http: 502,
            expected_state: "failed",
            expected_hold: "released",
            platform_side: true,
        },
        // 结果不明的那一类：第一方要求停止自动重试、不要换 Key，仍进对账并保留预授权。
        Rejected {
            provider_kind: "APIMart",
            adapter_key: "apimart-image-v1",
            status: 409,
            body: json!({"error": {"code": 409, "message": "idempotency_result_indeterminate"}}),
            channel_code: "409",
            channel_message: "idempotency_result_indeterminate",
            expected_code: "outcome_unknown",
            expected_http: 502,
            expected_state: "reconciliation_required",
            expected_hold: "active",
            platform_side: true,
        },
        Rejected {
            provider_kind: "AIHubMix",
            adapter_key: "aihubmix-image-v1",
            status: 403,
            body: json!({"error": {"code": "insufficient_user_quota", "message": "quota exhausted", "tid": UPSTREAM_TRACE_ID}}),
            channel_code: "insufficient_user_quota",
            channel_message: "quota exhausted",
            expected_code: "platform_unavailable",
            expected_http: 502,
            expected_state: "failed",
            expected_hold: "released",
            platform_side: true,
        },
        // 同一个状态码在另一个渠道没有"未受理"依据：不许跨渠道套用结论。
        Rejected {
            provider_kind: "AIHubMix",
            adapter_key: "aihubmix-image-v1",
            status: 429,
            body: json!({"error": {"code": "upstream_rate_limited", "message": "slow down"}}),
            channel_code: "upstream_rate_limited",
            channel_message: "slow down",
            expected_code: "outcome_unknown",
            expected_http: 502,
            expected_state: "reconciliation_required",
            expected_hold: "active",
            platform_side: false,
        },
    ];

    for Rejected {
        provider_kind,
        adapter_key,
        status,
        body: error_body,
        channel_code,
        channel_message,
        expected_code,
        expected_http,
        expected_state,
        expected_hold,
        platform_side,
    } in cases
    {
        let behaviour = UpstreamBehaviour {
            submit: SubmitBehaviour::Rejected {
                status,
                body: error_body.clone(),
            },
            ..match provider_kind {
                "AIHubMix" => UpstreamBehaviour::aihubmix(SyncImageShape::Base64),
                _ => UpstreamBehaviour::apimart(),
            }
        };
        let harness = Harness::start_with(
            provider_kind,
            adapter_key,
            &["prompt_only"],
            None,
            behaviour,
            64,
        )
        .await;
        let key = format!("rejected-{status}-{}", Uuid::new_v4());
        let (http, body) = harness
            .sync_json(
                "/v1/images/generations",
                &key,
                route_request(harness.model, "rejected prompt"),
            )
            .await;

        // 消费者面：只有平台码，没有任何渠道字样，也没有内部记录标识。
        assert_eq!(
            http,
            StatusCode::from_u16(expected_http).expect("status"),
            "HTTP 状态不符：{body}"
        );
        assert_eq!(
            body["error"]["code"].as_str(),
            Some(expected_code),
            "消费者看到的对客码不对：{body}"
        );
        assert_public_only(&format!("{provider_kind} {status}"), &body);
        let rendered = body.to_string();
        assert!(
            !rendered.contains(channel_message),
            "渠道原文不得出现在消费者面：{rendered}"
        );
        assert!(
            !rendered.contains(UPSTREAM_TRACE_ID),
            "上游逐请求标识不得出现在消费者面：{rendered}"
        );
        let values: Vec<String> = body["error"]
            .as_object()
            .expect("error object")
            .values()
            .map(|value| value.to_string().trim_matches('"').to_owned())
            .collect();
        assert!(
            values.iter().all(|value| value != channel_code),
            "渠道码不得作为字段值出现在消费者面：{rendered}"
        );

        // 内部：终态、预授权与渠道原始记录都留住了。
        let (job_id, state, _) = harness.job(&key).await;
        assert_eq!(
            state, expected_state,
            "{provider_kind} 的 {status} 必须落在 {expected_state}"
        );
        let hold_status: String =
            sqlx::query_scalar("SELECT status FROM ledger.holds WHERE job_id = $1")
                .bind(job_id)
                .fetch_one(&harness.pool)
                .await
                .expect("hold status");
        assert_eq!(
            hold_status, expected_hold,
            "{provider_kind} 的 {status} 预授权处置不符"
        );

        let row = sqlx::query(
            r#"
            SELECT a.provider_error_code, a.provider_error_message, a.provider_trace_id,
                   j.failure_kind
            FROM generation.jobs j
            JOIN generation.attempts a ON a.job_id = j.id
            WHERE j.id = $1
            "#,
        )
        .bind(job_id)
        .fetch_one(&harness.pool)
        .await
        .expect("attempt row");
        let stored_code: Option<String> = row.try_get("provider_error_code").expect("code");
        assert_eq!(
            stored_code.as_deref(),
            Some(channel_code),
            "渠道原始码必须留在内部记录里"
        );
        let stored_message: Option<String> =
            row.try_get("provider_error_message").expect("message");
        assert_eq!(stored_message.as_deref(), Some(channel_message));
        let stored_trace: Option<String> = row.try_get("provider_trace_id").expect("trace");
        // 上游给了逐请求标识就必须留住；没给就不该凭空造一个。
        let expected_trace = error_body
            .to_string()
            .contains(UPSTREAM_TRACE_ID)
            .then_some(UPSTREAM_TRACE_ID);
        assert_eq!(
            stored_trace.as_deref(),
            expected_trace,
            "上游给的逐请求标识必须留在内部记录里"
        );
        let stored_kind: Option<String> = row.try_get("failure_kind").expect("kind");
        assert!(
            stored_kind.is_some(),
            "每个失败路径都必须记录平台侧失败类别"
        );

        // 运营面（管理员）能看到渠道原始码；这正是不把它放上消费者面的补偿。
        // 缺省（不传 kind）只列**平台侧事件**：渠道不可用这类可观测事件要显式按类别才查得到。
        let stored_kind = stored_kind.expect("kind");
        let listed = |body: &Value| {
            body["failures"].as_array().is_some_and(|list| {
                list.iter()
                    .any(|entry| entry["job_id"].as_str() == Some(&job_id.to_string()))
            })
        };
        let default_list: Value = Client::new()
            .get(format!("{}/api/v1/provider-failures", harness.base_url))
            .bearer_auth(&harness.admin_token)
            .send()
            .await
            .expect("provider failures")
            .json()
            .await
            .expect("provider failures JSON");
        assert_eq!(
            listed(&default_list),
            platform_side,
            "缺省清单只该列平台侧事件：{default_list}"
        );

        let filtered: Value = Client::new()
            .get(format!(
                "{}/api/v1/provider-failures?kind={stored_kind}",
                harness.base_url
            ))
            .bearer_auth(&harness.admin_token)
            .send()
            .await
            .expect("filtered failures")
            .json()
            .await
            .expect("filtered failures JSON");
        assert!(
            listed(&filtered),
            "按类别筛选必须能查到这条失败：{filtered}"
        );
        let entry = filtered["failures"]
            .as_array()
            .and_then(|list| {
                list.iter()
                    .find(|entry| entry["job_id"].as_str() == Some(&job_id.to_string()))
            })
            .expect("entry");
        assert_eq!(entry["provider_error_code"].as_str(), Some(channel_code));
        assert_eq!(entry["error_code"].as_str(), Some(expected_code));
        assert_eq!(entry["kind"].as_str(), Some(stored_kind.as_str()));
        assert!(
            entry["offering_id"].as_str().is_some(),
            "运营要知道是哪条供给出的问题：{entry}"
        );
        assert_eq!(
            filtered["count"].as_u64(),
            filtered["failures"]
                .as_array()
                .map(|list| list.len() as u64),
            "响应必须如实给出条数"
        );
        assert_eq!(filtered["truncated"], json!(false));

        let unknown_kind = Client::new()
            .get(format!(
                "{}/api/v1/provider-failures?kind=nonsense",
                harness.base_url
            ))
            .bearer_auth(&harness.admin_token)
            .send()
            .await
            .expect("unknown kind");
        assert_eq!(unknown_kind.status(), StatusCode::BAD_REQUEST);
        let anonymous = Client::new()
            .get(format!("{}/api/v1/provider-failures", harness.base_url))
            .send()
            .await
            .expect("anonymous failures");
        assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
        let consumer_key = Client::new()
            .get(format!("{}/api/v1/provider-failures", harness.base_url))
            .bearer_auth(&harness.api_key)
            .send()
            .await
            .expect("consumer key on admin route");
        assert_eq!(consumer_key.status(), StatusCode::FORBIDDEN);

        harness.cleanup().await;
    }
}
