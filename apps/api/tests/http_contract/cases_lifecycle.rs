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
        assert_eq!(anonymous.status(), StatusCode::FORBIDDEN);
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

/// 把响应里的 `created_at`（`…+00:00`）改成能进查询串的等价写法（`…Z`）。
///
/// `+` 在查询串里会被读成空格，所以响应的原样值不能直接当查询参数；`Z` 与 `+00:00` 是同一个
/// 时刻，字符也全在安全集里。除了时区写法什么都不动：微秒必须留着，否则分界会落到那条分录之前。
fn wire_time(created_at: &str) -> String {
    let converted = created_at.replace("+00:00", "Z");
    assert!(
        !converted.contains('+'),
        "写入时刻的时区写法变了，`since` 需要跟着改：{created_at}"
    );
    converted
}

/// 一个**字面**的 `since` 偏移量：以某条分录自己的时刻为基准，往前或往后挪若干秒。
///
/// `since` 的契约是 RFC3339 的**时刻字面量**：SQL 表达式（`now() + interval …`）过不了反序列化，
/// 所以要挪只能在这边算好再发过去。秒级偏移足够把分界放到两条分录之间——它们的间隔是分钟。
fn offset_seconds(created_at: &str, seconds: i64) -> String {
    let parsed =
        chrono::DateTime::parse_from_rfc3339(&created_at.replace("+00:00", "Z")).expect("RFC3339");
    let shifted = parsed + chrono::Duration::seconds(seconds);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        chrono::Datelike::year(&shifted),
        chrono::Datelike::month(&shifted),
        chrono::Datelike::day(&shifted),
        chrono::Timelike::hour(&shifted),
        chrono::Timelike::minute(&shifted),
        chrono::Timelike::second(&shifted),
    )
}

/// 一次完整的"受理 → 结算"之后，账目流水上只看到 **充值 + 实收**两条真实收支，且与余额一致。
///
/// 判据不只是"两条都在"：两条的**金额**必须与库里的余额、与这次结算实际扣的钱对得上，符号也要
/// 对——充值入账为正，实收是负的（真的扣掉）。预授权（占用）不进资金流水：它只留在 `ledger.holds`，
/// 结算之后那笔占用是 `captured`。对不上就说明流水这条读视图漏了或错了一支分录，而流水正是拿来
/// 核对账目的东西。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_settled_job_leaves_credit_and_capture_in_the_ledger_view() {
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
        publish_cache_priced(&harness, priced_consumer_rates()).await,
        StatusCode::OK,
        "带定价的发布必须成功"
    );
    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;

    let key = format!("entries-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "a settled job leaves three entries");
    request["size"] = json!("2K");
    request["quality"] = json!("low");
    // Worker 必须**拿在手上**：`WorkerProcess` 一落到 `_` 就被 Drop 掉，Job 会停在"已受理"，
    // 同步入口等到窗口尽头按失败回 504——那时根本没有结算，也就没有流水可看。
    let _worker = harness.spawn_worker();
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    let (job_id, state, _) = harness.job(&key).await;
    assert_eq!(status, StatusCode::OK, "got {body}; state={state}");
    assert_sync_success("受理到结算", &body);
    assert_eq!(state, "succeeded", "结算完才能谈流水");
    let captured = -harness.captured_microusd(job_id).await;
    assert!(captured > 0, "结算必须真的扣了钱，实得 {captured}");

    // 持有额取库里那一行：它是受理时的保底额，不是从流水反推出来的。
    let held: i64 =
        sqlx::query_scalar("SELECT amount_microusd FROM ledger.holds WHERE job_id = $1")
            .bind(job_id)
            .fetch_one(&harness.pool)
            .await
            .expect("hold row");
    let db_balance = database_balance(&harness, &account_id).await;

    let listed: Value = client
        .get(format!(
            "{}/api/v1/accounts/{account_id}/entries",
            harness.base_url
        ))
        .bearer_auth(&harness.admin_token)
        .send()
        .await
        .expect("ledger entries")
        .json()
        .await
        .expect("ledger entries JSON");
    let entries = listed["entries"].as_array().expect("entries array");
    // 资金流水**只记真实收支**（`0002` §3）：这个账户只有建账户那一笔充值 + 这次结算的一条实收。
    // 预授权（占用）不进流水——它在 `ledger.holds` 里，上面已经单独读过。
    assert_eq!(entries.len(), 2, "资金流水只该有充值 + 这次实收：{listed}");

    let find = |kind: &str| {
        entries
            .iter()
            .find(|entry| entry["kind"].as_str() == Some(kind))
            .unwrap_or_else(|| panic!("流水里必须看得到 {kind}：{listed}"))
    };
    let credit = find("credit");
    let capture = find("capture");

    assert_eq!(credit["amount_microusd"].as_i64(), Some(1_000_000));
    assert_eq!(
        capture["amount_microusd"].as_i64(),
        Some(-captured),
        "扣费是实收，金额为负：{listed}"
    );
    assert_eq!(
        capture["job_id"].as_str(),
        Some(job_id.to_string().as_str()),
        "实收要指得出是哪次执行：{listed}"
    );
    assert!(capture["created_at"].is_string(), "每条都要带写入时刻");

    // 与余额**互相印证**：已结算余额 = 初始充值 − 实收；占用结清之后这笔 hold 是 `captured`。
    assert_eq!(
        db_balance,
        1_000_000 - captured,
        "余额只被这次结算动过（充值 1000000 减去实收）"
    );
    let hold_status: String =
        sqlx::query_scalar("SELECT status FROM ledger.holds WHERE job_id = $1")
            .bind(job_id)
            .fetch_one(&harness.pool)
            .await
            .expect("hold status");
    assert_eq!(hold_status, "captured", "结算之后这笔占用已结清：{listed}");
    assert!(
        held > captured,
        "保底额要估得比实收高，这条用例才试得出'先占住、后按实收结清'：持有 {held}、实收 {captured}"
    );
    let times: Vec<&str> = entries
        .iter()
        .filter_map(|entry| entry["created_at"].as_str())
        .collect();
    assert_eq!(times.len(), entries.len(), "每条都要带写入时刻：{listed}");
    let mut sorted = times.clone();
    sorted.sort_unstable_by(|left, right| right.cmp(left));
    assert_eq!(times, sorted, "流水必须按时间倒序：{listed}");

    harness.cleanup().await;
}

/// 流水的分页与增量：`limit` 截断、`since` 只取之后的，两者都能用。
///
/// 用**直接写账本**的方式把四条分录在时间上分开放（各自相差十分钟）：这里要试的是**这条读**的
/// 分页与增量语义，不走一次真实结算——受理与结算那条路上的分录共用同一个事务时间，反而试不出
/// "时间上分得开"这件事。
///
/// 分界一律写成 `now() - interval '…'` 这类**相对**时刻（`%20` 是 URL 里的空格）而不是把分录自己的
/// 时间戳原样回传：`since` 的契约是 RFC3339，而响应里的时间戳带微秒，任何转写都可能丢掉精度，
/// 于是分界会悄悄落到分录之前、断言就测不到"开区间"那件事了。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_ledger_view_pages_by_limit_and_pulls_incrementally_by_since() {
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
    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let account_uuid = Uuid::parse_str(&account_id).expect("account id");

    // 建账户那条 `credit` 落在 now()；下面三条依次往前挪 10 / 20 / 30 分钟。
    for (minutes_ago, business_key) in [(10_i64, "newer"), (20, "middle"), (30, "older")] {
        sqlx::query(
            "INSERT INTO ledger.entries (id, account_id, kind, amount_microusd, business_key, created_at)
             VALUES ($1, $2, 'adjustment', $3, $4, now() - make_interval(secs => $5 * 60.0))",
        )
        .bind(Uuid::new_v4())
        .bind(account_uuid)
        .bind(1_000_i64)
        .bind(format!("paging-{business_key}-{}", Uuid::new_v4()))
        .bind(minutes_ago as f64)
        .execute(&harness.pool)
        .await
        .expect("seeded ledger entry");
    }

    let read = |query: String| {
        let client = Client::new();
        let base_url = harness.base_url.clone();
        let admin_token = harness.admin_token.clone();
        async move {
            let response = client
                .get(format!(
                    "{base_url}/api/v1/accounts/{account_uuid}/entries{query}"
                ))
                .bearer_auth(&admin_token)
                .send()
                .await
                .expect("ledger read");
            let status = response.status();
            let body: Value = response.json().await.expect("ledger JSON");
            assert_eq!(status, StatusCode::OK, "query={query} body={body}");
            body
        }
    };
    let kinds = |body: &Value| -> Vec<String> {
        body["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .map(|entry| entry["kind"].as_str().expect("kind").to_owned())
            .collect()
    };

    // 不截断时四条都在，且**时间倒序**。
    let all = read(String::new()).await;
    assert_eq!(all["count"].as_u64(), Some(4), "四条分录都该在：{all}");
    assert_eq!(all["truncated"], json!(false), "没截断：{all}");
    let all_entries = all["entries"].as_array().expect("entries").clone();
    assert_eq!(
        kinds(&all),
        vec!["credit", "adjustment", "adjustment", "adjustment"],
        "最新的排在最前（充值落在 now()，三条调整依次更早）：{all}"
    );
    let times: Vec<&str> = all_entries
        .iter()
        .map(|entry| entry["created_at"].as_str().expect("created_at"))
        .collect();
    let mut sorted = times.clone();
    sorted.sort_unstable_by(|left, right| right.cmp(left));
    assert_eq!(times, sorted, "时间倒序：{all}");

    // `limit` 生效，截掉的是更旧的那一段，且如实说自己截断了。
    let limited = read("?limit=2".to_owned()).await;
    assert_eq!(limited["count"].as_u64(), Some(2), "limit=2：{limited}");
    assert_eq!(limited["truncated"], json!(true), "limit=2：{limited}");
    assert_eq!(
        limited["entries"].as_array().expect("entries"),
        &all_entries[..2],
        "截断时留下的是最新的两条：{limited}"
    );
    assert_eq!(
        kinds(&limited),
        vec!["credit", "adjustment"],
        "留下的确实是最新那两条：{limited}"
    );

    // `since` 只取之后的：分界落在最旧那条与它上一条**之间**（往最旧那条之后挪 5 秒），更旧的
    // 那条调整不该出现。分界由响应里自己的时刻算出来，所以不受进程时钟与库钟差的影响。
    let oldest = all_entries[3]["created_at"].as_str().expect("oldest time");
    let between = read(format!("?since={}", offset_seconds(oldest, 5))).await;
    assert_eq!(
        kinds(&between),
        vec!["credit", "adjustment", "adjustment"],
        "分界之后只有更新的三条：{between}"
    );

    // `since` 是**开区间**：分界正好取在"最旧那只调整"自己的时刻上，它自己不该被算作"之后"。
    // 分界逐微秒转写自那条分录——少写一位精度，分界就落到它之前，这一段就白测了。
    let boundary = wire_time(oldest);
    let after_oldest = read(format!("?since={boundary}")).await;
    assert_eq!(
        kinds(&after_oldest),
        vec!["credit", "adjustment", "adjustment"],
        "分界上那条自己不算之后：{after_oldest}"
    );

    // 反过来：分界取在**最新那条**自己的时刻上，就没有比它更新的东西了——开区间的另一个端点。
    let newest = all_entries[0]["created_at"].as_str().expect("newest time");
    let empty = read(format!("?since={}", wire_time(newest))).await;
    assert!(
        kinds(&empty).is_empty(),
        "最新那条自己不算之后，所以一条都不剩：{empty}"
    );
    assert_eq!(empty["truncated"], json!(false), "空结果不算截断：{empty}");
    assert_eq!(
        empty["total"].as_u64(),
        Some(0),
        "区间内一条都没有时 total 是 0，不是全表的条数：{empty}"
    );

    // 分界取在**未来**：没有比它更新的东西，给空数组而不是报错。
    let future = read("?since=2100-01-01T00:00:00Z".to_owned()).await;
    assert_eq!(
        &kinds(&future),
        &Vec::<String>::new(),
        "未来时刻之后什么都没有：{future}"
    );
    assert_eq!(
        future["truncated"],
        json!(false),
        "空结果不算截断：{future}"
    );

    // `total` 是**同一套区间条件**下的总条数，与 `count`（本页条数）不同：翻页靠它判断还有没有下一页。
    assert_eq!(
        all["total"].as_u64(),
        Some(4),
        "四条都在时 total 也是 4：{all}"
    );
    assert_eq!(
        limited["total"].as_u64(),
        Some(4),
        "limit=2 时本页 2 条、总数仍是 4——只看 count 会以为翻完了：{limited}"
    );

    // `until` 是**半开**上界：取最旧那条自己的时刻，它**不该**被含进来（与对客账单汇总同一条口径，
    // 见 Spec §4.3——同一区间下明细与汇总必须对得上）。
    let oldest_boundary = wire_time(oldest);
    let up_to_oldest = read(format!("?until={oldest_boundary}")).await;
    assert!(
        kinds(&up_to_oldest).is_empty(),
        "半开上界不含端点，最旧那条也在界外：{up_to_oldest}"
    );

    // 上界放在最旧那条与它上一条**之间**（往最新方向挪 5 秒）：这时它该被含进来，且只剩它。
    let after_oldest_gap = read(format!("?until={}", offset_seconds(oldest, 5))).await;
    assert_eq!(
        kinds(&after_oldest_gap),
        vec!["adjustment"],
        "界内只剩最旧那一条：{after_oldest_gap}"
    );

    // `truncated` 的判别据是"**这个位置之后还有没有更多**"，不是"这一页满没满"——两者只在条数**恰好
    // 整除**时给出不同答案，所以这里专门取 `limit` 正好等于总数的那一格：旧定义（`len == limit`）会说
    // "还有更多"，而实际上一条都不剩了。翻页的调用方靠这个信号决定要不要再请求一次。
    let exact = read("?limit=4".to_owned()).await;
    assert_eq!(
        exact["count"].as_u64(),
        Some(4),
        "limit 正好等于总数：{exact}"
    );
    assert_eq!(
        exact["truncated"],
        json!(false),
        "条数恰好等于 limit 时后面没有更多了——按'页满了'判会说反：{exact}"
    );
    assert_eq!(
        exact["total"].as_u64(),
        Some(4),
        "总数与 limit 无关：{exact}"
    );

    // `offset` 翻页：第 2 页留下第 3、4 条，且与不翻页时的顺序**接得上**（不重不漏）。
    let second_page = read("?limit=2&offset=2".to_owned()).await;
    assert_eq!(
        second_page["count"].as_u64(),
        Some(2),
        "第 2 页：{second_page}"
    );
    assert_eq!(
        second_page["total"].as_u64(),
        Some(4),
        "翻到第 2 页时总数不变：{second_page}"
    );
    assert_eq!(
        second_page["truncated"],
        json!(false),
        "第 2 页已经是最后一页，后面没有更多了：{second_page}"
    );
    assert_eq!(
        second_page["entries"].as_array().expect("entries"),
        &all_entries[2..],
        "offset=2 取到的正是全量里的后两条（不重不漏）：{second_page}"
    );

    // `offset` 超出总数：给空数组而不是报错。`truncated` 是 false——"后面还有更多"问的是**这个位置
    // 之后**还有没有，99 已经越过末尾，没有了。总数不受 `offset` 影响，仍是 4。
    let past_end = read("?offset=99".to_owned()).await;
    assert!(kinds(&past_end).is_empty(), "越过末尾给空数组：{past_end}");
    assert_eq!(
        past_end["truncated"],
        json!(false),
        "越过末尾之后没有更多了：{past_end}"
    );
    assert_eq!(
        past_end["total"].as_u64(),
        Some(4),
        "越过末尾时 total 不受 offset 影响：{past_end}"
    );

    // 边界：没有管理员凭证 403；账户不存在 404（与"没有流水"分开）；消费者的 Key 不是管理员凭证。
    let anonymous = client
        .get(format!(
            "{}/api/v1/accounts/{account_id}/entries",
            harness.base_url
        ))
        .send()
        .await
        .expect("anonymous ledger read");
    assert_eq!(anonymous.status(), StatusCode::FORBIDDEN);

    let unknown = client
        .get(format!(
            "{}/api/v1/accounts/{}/entries",
            harness.base_url,
            Uuid::new_v4()
        ))
        .bearer_auth(&harness.admin_token)
        .send()
        .await
        .expect("unknown account ledger read");
    assert_eq!(
        unknown.status(),
        StatusCode::NOT_FOUND,
        "账户不存在与'没有流水'要分开"
    );

    let consumer = client
        .get(format!(
            "{}/api/v1/accounts/{account_id}/entries",
            harness.base_url
        ))
        .bearer_auth(&api_key)
        .send()
        .await
        .expect("consumer key on the admin ledger route");
    assert_eq!(
        consumer.status(),
        StatusCode::FORBIDDEN,
        "对客的 Key 不是管理员凭证"
    );

    harness.cleanup().await;
}

/// 对客账户面：只给**自己的**余额、持有中与可用额，三者**分开给**，且与库里一致。
///
/// 换一把 Key 必须看不到别人的账户；三个数不合成一个数——合成"总资产"会让"这笔钱到底扣没扣"
/// 说不清。这里用一个**停在持有中**的 Job 把三个数拉开：已结算余额没有被预授权动过，持有中
/// 正好是那个保底额，可用额是两者相减，而它还没有被结算。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_consumer_account_view_shows_only_its_own_balance_and_hold() {
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
    let other = funded_account(&client, &harness.base_url, &harness.admin_token, 500_000).await;
    let (other_id, other_key) = other;
    let own_id = harness.account_id.clone();
    let own_key = harness.api_key.clone();

    // 一次受理（不起 Worker）：只增加持有中、留下一条 active 预授权，不动已结算余额。
    let key = format!("statement-hold-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &own_key,
        "/v1/images/generations",
        &key,
        &route_request(harness.model, "a job left holding"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::GATEWAY_TIMEOUT,
        "没有 Worker 时同步入口超时"
    );
    let (job_id, state, _) = harness.job(&key).await;
    assert_eq!(state, "accepted", "受理完成、还没执行：{body}");
    let held: i64 =
        sqlx::query_scalar("SELECT amount_microusd FROM ledger.holds WHERE job_id = $1")
            .bind(job_id)
            .fetch_one(&harness.pool)
            .await
            .expect("hold row");

    let own: Value = client
        .get(format!("{}/v1/account", harness.base_url))
        .bearer_auth(&own_key)
        .send()
        .await
        .expect("own account read")
        .json()
        .await
        .expect("own account JSON");
    assert_eq!(
        own["balance_microusd"].as_i64(),
        Some(database_balance(&harness, &own_id).await),
        "余额与库一致：{own}"
    );
    assert_eq!(
        own["held_microusd"].as_i64(),
        Some(held),
        "持有中是已预授权未结算的那一笔：{own}"
    );
    assert!(
        own["balance_microusd"].as_i64().unwrap_or_default()
            < database_balance(&harness, &own_id).await + held,
        "持有中没有被算进余额：{own}"
    );
    assert!(
        own["updated_at"].is_string(),
        "一起给出写入时刻，才看得出这个数是什么时候的：{own}"
    );
    assert_public_only("对客账户面", &own);

    // 另一把 Key：只看到它自己的账户，看不到上面那个持有。
    let stranger: Value = client
        .get(format!("{}/v1/account", harness.base_url))
        .bearer_auth(&other_key)
        .send()
        .await
        .expect("stranger account read")
        .json()
        .await
        .expect("stranger account JSON");
    assert_eq!(
        stranger["balance_microusd"].as_i64(),
        Some(database_balance(&harness, &other_id).await),
        "看到的是自己的余额：{stranger}"
    );
    assert_eq!(
        stranger["held_microusd"].as_i64(),
        Some(0),
        "别人的持有中不该露出来，自己也没有持有：{stranger}"
    );
    assert_ne!(
        own["balance_microusd"], stranger["balance_microusd"],
        "两个账户的余额不同，才说明这条读真的按调用者分账户"
    );

    // 没有凭证 401；无效凭证也 401（不是 403：调用方要换的是 Key）。
    let anonymous = client
        .get(format!("{}/v1/account", harness.base_url))
        .send()
        .await
        .expect("anonymous account read");
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    let bogus = client
        .get(format!("{}/v1/account", harness.base_url))
        .bearer_auth("sk_seeai_not-a-real-key")
        .send()
        .await
        .expect("bogus key read");
    assert_eq!(bogus.status(), StatusCode::UNAUTHORIZED);

    harness.cleanup().await;
}

// ── 平台故障告警出口 ──
//
// 出口是**配置项**：`PROVIDER_ALERT_WEBHOOK` 指到本地接收器，触发条件是平台侧事件，发送失败是
// 旁路。四条用例共用"一次平台欠费失败"这个场景：它的类别（`platform_funding`）正是第一个触发条件。

/// 上游拒绝提交、判为平台欠费的那一次失败（对客是平台侧故障，预授权释放）。
fn funding_rejection() -> UpstreamBehaviour {
    UpstreamBehaviour {
        submit: SubmitBehaviour::Rejected {
            status: 402,
            body: json!({"error": {"code": 402, "message": "payment_required: account balance is insufficient"}}),
        },
        ..UpstreamBehaviour::apimart()
    }
}

/// 一次平台欠费失败跑完之后，**对客看到的那一份**与**内部留下的那一份**。
///
/// "逐位相同"要的是同一组事实，所以这里把两边的读数装进一个结构再整体比：响应状态与正文、
/// Job 的终态与对客错误码、预授权处置、结果信封、余额与账本分录。
#[derive(Debug, PartialEq)]
struct FailureOutcome {
    http: StatusCode,
    body: Value,
    state: String,
    error_code: Option<String>,
    error_message: Option<String>,
    hold: String,
    images: Option<Value>,
    balance: i64,
    entries: Vec<(String, i64)>,
}

/// 跑一次平台欠费失败，取这次执行的结局。`alerts` 给 `None` 就是**没配出口**的那条路径。
async fn funding_failure_outcome(alerts: Option<&WorkerAlerts>) -> FailureOutcome {
    let harness = Harness::start_with(
        "APIMart",
        "apimart-image-v1",
        &["prompt_only"],
        None,
        funding_rejection(),
        64,
    )
    .await;
    let worker = match alerts {
        Some(alerts) => harness.spawn_worker_with_alerts(alerts),
        None => harness.spawn_worker(),
    };
    let key = format!("alert-outcome-{}", Uuid::new_v4());
    let (http, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &key,
        &route_request(harness.model, "alert side path"),
    )
    .await;
    drop(worker);

    let (job_id, state, images) = harness.job(&key).await;
    let row = sqlx::query("SELECT error_code, error_message FROM generation.jobs WHERE id = $1")
        .bind(job_id)
        .fetch_one(&harness.pool)
        .await
        .expect("job row");
    let hold: String = sqlx::query_scalar("SELECT status FROM ledger.holds WHERE job_id = $1")
        .bind(job_id)
        .fetch_one(&harness.pool)
        .await
        .expect("hold row");
    let balance = database_balance(&harness, &harness.account_id).await;
    let rows = sqlx::query(
        "SELECT kind, amount_microusd FROM ledger.entries WHERE account_id = $1
         ORDER BY kind, amount_microusd",
    )
    .bind(Uuid::parse_str(&harness.account_id).expect("account id"))
    .fetch_all(&harness.pool)
    .await
    .expect("ledger entries");
    let entries = rows
        .iter()
        .map(|row| {
            (
                row.try_get("kind").expect("entry kind"),
                row.try_get("amount_microusd").expect("entry amount"),
            )
        })
        .collect();
    let outcome = FailureOutcome {
        http,
        body,
        state,
        error_code: row.try_get("error_code").expect("error code"),
        error_message: row.try_get("error_message").expect("error message"),
        hold,
        images,
        balance,
        entries,
    };
    harness.cleanup().await;
    outcome
}

/// 把 webhook 指向本地接收器，一次平台欠费失败就该收到一条 JSON：job、渠道、失败类别、时间。
///
/// 载荷同时钉住两件事：它就是那四个字段（多一个键都算多带一份仓库里的东西出门），且不含调用方的
/// 提示词与渠道凭证的值——告警外发到仓库之外，对客内容与密钥都不出门。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_platform_funding_failure_reaches_the_configured_alert_webhook() {
    let receiver = AlertReceiver::start(200).await;
    let harness = Harness::start_with(
        "APIMart",
        "apimart-image-v1",
        &["prompt_only"],
        None,
        funding_rejection(),
        64,
    )
    .await;
    // 阈值调高：这条用例只看"平台欠费或凭证类失败"那一个触发条件。
    let worker = harness.spawn_worker_with_alerts(&WorkerAlerts {
        webhook: receiver.url.clone(),
        consecutive_failures: 100,
    });
    let prompt = format!("prompt-secret-{}", Uuid::new_v4());
    let key = format!("alert-payload-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &key,
        &route_request(harness.model, &prompt),
    )
    .await;
    drop(worker);
    assert_eq!(
        status,
        StatusCode::BAD_GATEWAY,
        "平台欠费对客是平台侧故障：{body}"
    );
    let (job_id, state, _) = harness.job(&key).await;
    assert_eq!(state, "failed", "平台欠费失败落在失败终态");

    let alerts = receiver.wait_for(1).await;
    assert_eq!(alerts.len(), 1, "一次平台欠费失败就是一条告警：{alerts:?}");
    let alert = &alerts[0];
    assert_eq!(
        alert_keys(alert),
        ["failure_kind", "job_id", "occurred_at", "provider_kind"]
            .map(str::to_owned)
            .to_vec(),
        "载荷就是定位所需的最小集：{alert}"
    );
    assert_eq!(alert["job_id"].as_str(), Some(job_id.to_string().as_str()));
    assert_eq!(alert["provider_kind"].as_str(), Some("APIMart"));
    assert_eq!(alert["failure_kind"].as_str(), Some("platform_funding"));
    let occurred_at = alert["occurred_at"].as_str().expect("时间是个字符串");
    chrono::DateTime::parse_from_rfc3339(occurred_at).expect("时间是个可解析的时刻");
    let rendered = alert.to_string();
    assert!(!rendered.contains(&prompt), "提示词不得外发：{rendered}");
    assert!(
        !rendered.contains("contract-test-key"),
        "渠道凭证不得外发：{rendered}"
    );

    harness.cleanup().await;
}

/// 对账案例新增也是平台侧事件：这次失败把 Job 推成 `reconciliation_required`（建案），告警跟着
/// 这条**已提交的事实**走。
///
/// 这次失败的类别是渠道限流——它自己不是平台侧事件、也不在"欠费/凭证"那一类里，所以这条用例证明
/// 的是第二个触发条件独立成立：进对账就告警，不管失败是哪一类。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_new_reconciliation_case_reaches_the_configured_alert_webhook() {
    let receiver = AlertReceiver::start(200).await;
    let harness = Harness::start_with(
        "AIHubMix",
        "aihubmix-image-v1",
        &["prompt_only"],
        None,
        UpstreamBehaviour {
            // 渠道侧限流：受理状态不确定，按既有口径保留预授权并进对账。
            submit: SubmitBehaviour::Rejected {
                status: 429,
                body: json!({"error": {"code": "upstream_rate_limited", "message": "slow down"}}),
            },
            ..UpstreamBehaviour::aihubmix(SyncImageShape::Url)
        },
        64,
    )
    .await;
    // 阈值调高：这条用例只看"对账案例新增"那一个触发条件。
    let worker = harness.spawn_worker_with_alerts(&WorkerAlerts {
        webhook: receiver.url.clone(),
        consecutive_failures: 100,
    });
    let key = format!("alert-reconciliation-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &key,
        &route_request(harness.model, "reconciliation alert"),
    )
    .await;
    drop(worker);
    assert_eq!(
        status,
        StatusCode::BAD_GATEWAY,
        "结果不明对客仍是平台侧故障：{body}"
    );
    let (job_id, state, _) = harness.job(&key).await;
    assert_eq!(state, "reconciliation_required", "这次失败必须进对账");

    let alerts = receiver.wait_for(1).await;
    assert_eq!(alerts.len(), 1, "新增一条对账案例就是一条告警：{alerts:?}");
    assert_eq!(
        alerts[0]["job_id"].as_str(),
        Some(job_id.to_string().as_str())
    );
    assert_eq!(alerts[0]["provider_kind"].as_str(), Some("AIHubMix"));
    assert_eq!(
        alerts[0]["failure_kind"].as_str(),
        Some("upstream_rate_limited"),
        "类别是那次失败自己的类别：进对账与类别无关"
    );

    harness.cleanup().await;
}

/// 某候选连续失败 N 次才告警，N 是**配置项**：阈值之前一条都不发，到阈值那一次才发。
///
/// 这里的失败是渠道限流——它自己不是平台侧事件、也不进对账，所以这条用例证明的是第三个触发条件
/// 独立成立：一条候选连着失败到阈值就告警。收到的那一条必须属于**达到阈值的那次**执行。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_candidate_alerts_only_when_its_failure_streak_reaches_the_configured_threshold() {
    let receiver = AlertReceiver::start(200).await;
    let harness = Harness::start_with(
        "APIMart",
        "apimart-image-v1",
        &["prompt_only"],
        None,
        UpstreamBehaviour {
            submit: SubmitBehaviour::Rejected {
                status: 429,
                body: json!({"error": {"code": 429, "message": "rate_limit_error"}}),
            },
            ..UpstreamBehaviour::apimart()
        },
        64,
    )
    .await;
    let _worker = harness.spawn_worker_with_alerts(&WorkerAlerts {
        webhook: receiver.url.clone(),
        consecutive_failures: 2,
    });

    let first = format!("alert-streak-1-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &first,
        &route_request(harness.model, "streak one"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    let (_, first_state, _) = harness.job(&first).await;
    assert_eq!(first_state, "failed");
    assert!(
        receiver.bodies().is_empty(),
        "第一次失败还没到阈值，一条都不该外发：{:?}",
        receiver.bodies()
    );

    let second = format!("alert-streak-2-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &second,
        &route_request(harness.model, "streak two"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    let (second_job, second_state, _) = harness.job(&second).await;
    assert_eq!(second_state, "failed");

    let alerts = receiver.wait_for(1).await;
    assert_eq!(alerts.len(), 1, "阈值上的那一次失败外发一条：{alerts:?}");
    assert_eq!(
        alerts[0]["job_id"].as_str(),
        Some(second_job.to_string().as_str()),
        "告警属于把连续失败数推到阈值的那次执行"
    );
    assert_eq!(alerts[0]["provider_kind"].as_str(), Some("APIMart"));
    assert_eq!(
        alerts[0]["failure_kind"].as_str(),
        Some("upstream_rate_limited")
    );

    harness.cleanup().await;
}

/// 送不出去**不改**任何东西：接收器回 500（发送会重试到有界次数后认输）时，对客响应、Job 的终态与
/// 对客错误码、预授权处置、结果信封、余额与账本，与**没配出口**时逐位相同。
///
/// 断言里还要看到"接收器真的被调用过"：否则这条对比证明不了旁路，只证明了两边都没发。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_webhook_that_refuses_delivery_leaves_the_job_and_the_consumer_answer_bit_identical() {
    let without = funding_failure_outcome(None).await;
    let receiver = AlertReceiver::start(500).await;
    let refusing = funding_failure_outcome(Some(&WorkerAlerts {
        webhook: receiver.url.clone(),
        consecutive_failures: 100,
    }))
    .await;

    assert_eq!(
        without, refusing,
        "告警是旁路：发不出去也不许改 Job 的处置、结算与对客结果"
    );
    assert!(
        !receiver.bodies().is_empty(),
        "接收器必须真的被调用过，否则这条对比什么也没证明"
    );
}

/// 没配 `PROVIDER_ALERT_WEBHOOK` 时**一条都不外发**：接收器在跑、失败照样发生，它什么都收不到。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn no_configured_webhook_means_nothing_leaves_the_worker() {
    let receiver = AlertReceiver::start(200).await;
    let outcome = funding_failure_outcome(None).await;
    assert_eq!(outcome.state, "failed", "失败本身照旧");
    // 失败处置落库之后外发才可能发生：给那条路径一点时间，然后确认它什么也没发。
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        receiver.bodies().is_empty(),
        "没配出口就不外发：{:?}",
        receiver.bodies()
    );
}

/// 账实核对：把余额**直接改错**（只改这一边、账本条目一条不动）⇒ 管理员按账户触发核查之后，
/// 后台任务必须报出不符，留下一条**账户级**的对账案例，并外发一条账实不符的告警。
///
/// 它与上面那条缓存对账不是一回事：那个以库为准覆盖缓存里的副本，这条比的是库里两组事实——余额
/// 与它自己那本账、占用合计与 active 预授权。案例与告警都要指得出**哪个账户、两组数各是多少**，
/// 收到的人不必先猜是哪一笔执行出了问题；而它**不是**某次执行，所以两个 Job 字段都是空的。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_ledger_audit_reports_a_balance_that_disagrees_with_its_entries() {
    let receiver = AlertReceiver::start(200).await;
    let harness = Harness::start_with_ledger_audit(
        Some(receiver.url.clone()),
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;
    let account_id = harness.account_id.clone();
    let ledger_total = ledger_total_microusd(&harness, &account_id).await;
    assert_eq!(ledger_total, 1_000_000, "夹具账户的账本就是那笔充值");
    let forged = ledger_total - 1;
    forge_account_balance(&harness, &account_id, forged).await;

    trigger_ledger_audit(&harness, &account_id).await;
    let (case_id, reason) = wait_for_open_ledger_case(&harness, &account_id).await;
    assert!(
        reason.contains(&ledger_total.to_string()) && reason.contains(&forged.to_string()),
        "案例要写清两个数：{reason}"
    );

    // 案例要在管理员清单里看得见，而且**没有 Job / Attempt**：账户级案例不该从清单里消失。
    let client = Client::new();
    let cases: Value = client
        .get(format!("{}/api/v1/reconciliation-cases", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .send()
        .await
        .expect("case list")
        .json()
        .await
        .expect("case list JSON");
    let listed = cases
        .as_array()
        .expect("case array")
        .iter()
        .find(|case| case["id"].as_str() == Some(case_id.to_string().as_str()))
        .expect("账户级案例必须在清单里");
    assert!(listed["job_id"].is_null(), "账户级案例没有 Job");
    assert!(listed["attempt_id"].is_null(), "账户级案例没有 Attempt");
    assert_eq!(listed["account_id"].as_str(), Some(account_id.as_str()));

    let alerts = receiver.wait_for(1).await;
    assert_eq!(alerts.len(), 1, "一条账实不符就是一条告警：{alerts:?}");
    assert_eq!(alerts[0]["account_id"].as_str(), Some(account_id.as_str()));
    assert_eq!(alerts[0]["ledger_total_microusd"], json!(ledger_total));
    assert_eq!(alerts[0]["balance_microusd"], json!(forged));
    assert_eq!(alerts[0]["holds_total_microusd"], json!(0));
    assert_eq!(alerts[0]["held_microusd"], json!(0));
    assert!(
        alerts[0]["job_id"].is_null(),
        "账实不符不属于任何一次执行：{alerts:?}"
    );

    harness.cleanup().await;
}

/// 占用那条等式：把 `held_microusd` 直接改错（active 预授权一行不动）⇒ 核查同样报出不符，
/// 案例写清 active holds 与 held 两个数。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_ledger_audit_reports_a_held_total_that_disagrees_with_active_holds() {
    let harness = Harness::start_with_ledger_audit(
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;
    let account_id = harness.account_id.clone();
    let holds_total = holds_total_microusd(&harness, &account_id).await;
    assert_eq!(holds_total, 0, "夹具账户还没有任何预授权");
    let forged = 12_345;
    forge_account_held(&harness, &account_id, forged).await;

    trigger_ledger_audit(&harness, &account_id).await;
    let (_case_id, reason) = wait_for_open_ledger_case(&harness, &account_id).await;
    assert!(
        reason.contains(&holds_total.to_string()) && reason.contains(&forged.to_string()),
        "占用对不上要写清 active holds 与 held：{reason}"
    );

    harness.cleanup().await;
}

/// 账实**一致**时核查**什么都不做**：不建案（连一条历史都不留）、不告警、一个数都不改。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_consistent_ledger_leaves_no_case_and_no_alert() {
    let receiver = AlertReceiver::start(200).await;
    let harness = Harness::start_with_ledger_audit(
        Some(receiver.url.clone()),
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;
    let account_id = harness.account_id.clone();
    let balance = database_balance(&harness, &account_id).await;
    let held = database_held(&harness, &account_id).await;
    let ledger_total = ledger_total_microusd(&harness, &account_id).await;
    let holds_total = holds_total_microusd(&harness, &account_id).await;
    assert_eq!(balance, ledger_total, "起点必须是一致的");
    assert_eq!(held, holds_total, "起点占用也是一致的");
    let cases_before = reconciliation_case_count(&harness).await;

    trigger_ledger_audit(&harness, &account_id).await;
    // 后台任务跑完才有结论；一致时它什么痕迹都不留，只能等一个足够它跑完的窗口。
    tokio::time::sleep(Duration::from_millis(1_000)).await;

    assert_eq!(
        reconciliation_case_count(&harness).await,
        cases_before,
        "一致时案例条数不变（一条也不新增）"
    );
    assert!(
        receiver.bodies().is_empty(),
        "一致时一条告警都不发：{:?}",
        receiver.bodies()
    );
    assert_eq!(
        database_balance(&harness, &account_id).await,
        balance,
        "一致时余额一个数都不动"
    );
    assert_eq!(
        ledger_total_microusd(&harness, &account_id).await,
        ledger_total,
        "一致时账本一条都不写"
    );

    // 证明触发的那条路是活的：改错余额，再触发一次必须报出来。
    forge_account_balance(&harness, &account_id, balance - 1).await;
    trigger_ledger_audit(&harness, &account_id).await;
    wait_for_open_ledger_case(&harness, &account_id).await;

    harness.cleanup().await;
}

/// 核查**只发现、不改账**：报出不符之后再触发几次，那条被改错的余额与账本条目仍是测试留下的样子，
/// 未结案案例仍只有一条。
///
/// 这一条还**没配接收器**——发现与建案不依赖有没有出口。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_ledger_audit_never_repairs_the_books_itself() {
    let harness = Harness::start_with_ledger_audit(
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;
    let account_id = harness.account_id.clone();
    let ledger_total = ledger_total_microusd(&harness, &account_id).await;
    let entries = ledger_entry_count(&harness, &account_id).await;
    let forged = ledger_total - 7;
    forge_account_balance(&harness, &account_id, forged).await;

    trigger_ledger_audit(&harness, &account_id).await;
    wait_for_open_ledger_case(&harness, &account_id).await;
    // 再触发几次：持续不符既不该把账平上，也不该多出案例。
    for _ in 0..3 {
        trigger_ledger_audit(&harness, &account_id).await;
    }
    tokio::time::sleep(Duration::from_millis(800)).await;

    assert_eq!(
        database_balance(&harness, &account_id).await,
        forged,
        "核对不许把余额改回账本的和"
    );
    assert_eq!(
        ledger_entry_count(&harness, &account_id).await,
        entries,
        "核对不许往账本里补条目（补一条调整也不许）"
    );
    assert_eq!(
        open_ledger_cases(&harness, &account_id).await,
        1,
        "一直对不上也只留一条未结案案例，不每次多一条"
    );

    harness.cleanup().await;
}

/// **不默认全库重算**：正常启动的 API 进程没有账实核对的定时任务；把余额改错、再读一次账户、
/// 等够一段时间，库里一条案例都不该出现。随后显式触发才出现——证明"没出现"是没跑，不是坏掉了。
///
/// 这一条就是 A9 的"核对不在请求路径运行"：余额读取与生成请求都不触发它，只有管理员按账户
/// 触发才跑。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn no_ledger_audit_runs_without_an_explicit_trigger() {
    let harness = Harness::start_with(
        "AIHubMix",
        "aihubmix-image-v1",
        &["prompt_only"],
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;
    let account_id = harness.account_id.clone();
    let ledger_total = ledger_total_microusd(&harness, &account_id).await;
    forge_account_balance(&harness, &account_id, ledger_total - 1).await;

    // 读一次账户（管理面与资金读都在这条路上），再等一个够任何定时任务跑几轮的窗口。
    let client = Client::new();
    client
        .get(format!("{}/api/v1/accounts/{account_id}", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .send()
        .await
        .expect("read the account");
    tokio::time::sleep(Duration::from_secs(2)).await;

    assert_eq!(
        open_ledger_cases(&harness, &account_id).await,
        0,
        "没有触发就不该有核查在跑"
    );

    trigger_ledger_audit(&harness, &account_id).await;
    wait_for_open_ledger_case(&harness, &account_id).await;

    harness.cleanup().await;
}
