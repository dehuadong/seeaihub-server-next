use super::*;

/// 第二个网关模型的线上名：名额按模型给之后，两个模型必须各自有名额。
const SECOND_MODEL: &str = "driver-model-second";

/// 发布第二个网关模型，指向夹具那台假上游。
async fn publish_second_model(harness: &Harness) {
    let contract = surface_schema(json!({
        "model": {"const": SECOND_MODEL},
        "prompt": {"type": "string", "minLength": 1}
    }));
    let mut draft = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    // 假上游的地址：不写回夹具里的占位地址，请求会去连一个不存在的端口。
    draft["base_url"] = Value::String(harness.upstream_base_url.clone());
    assert_eq!(
        publish_on_revision(
            harness,
            SECOND_MODEL,
            "route-test-second-1",
            contract,
            vec![draft],
            // 这条候选按上游声明金额计价：发布期要求修订级倍率（Spec 0001 M2）。
            Some(2_000),
        )
        .await,
        StatusCode::OK,
        "第二个网关模型必须发得出去"
    );
}

/// 管理员改运维开关与并发名额（`null`＝清成部署缺省；省略＝这次不改）。
async fn patch_settings(harness: &Harness, model: &str, body: Value) -> (StatusCode, Value) {
    let response = Client::new()
        .patch(format!(
            "{}/api/v1/gateway-models/{model}",
            harness.base_url
        ))
        .bearer_auth(&harness.admin_token)
        .json(&body)
        .send()
        .await
        .expect("the admin patch");
    let status = response.status();
    let body = response.json().await.unwrap_or(Value::Null);
    (status, body)
}

/// 只关心状态码时的简写。
async fn patch_status(harness: &Harness, model: &str, body: Value) -> StatusCode {
    patch_settings(harness, model, body).await.0
}

/// 管理端模型清单里该模型的那一项。
async fn model_view(harness: &Harness, model: &str) -> Value {
    let body: Value = Client::new()
        .get(format!("{}/api/v1/gateway-models", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .send()
        .await
        .expect("the admin model list")
        .json()
        .await
        .expect("the model list body");
    body["gateway_models"]
        .as_array()
        .expect("a model array")
        .iter()
        .find(|view| view["gateway_model"] == json!(model))
        .cloned()
        .unwrap_or_else(|| panic!("model {model} missing from the admin list: {body}"))
}

/// 该动作写过几条审计事件（harness 那份 `audit_events` 取的是全部事件，这里按动作计数）。
async fn audit_count(harness: &Harness, action: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM operations.audit_events WHERE action = $1")
        .bind(action)
        .fetch_one(&harness.pool)
        .await
        .expect("audit event count")
}

/// 等假上游上第 `n` 条 Create 请求到达；等不到就是"请求根本没被受理"。
async fn wait_for_arrival(harness: &Harness, n: usize) {
    let gate = harness.gate().clone();
    tokio::time::timeout(Duration::from_secs(10), gate.wait_for_arrival(n))
        .await
        .unwrap_or_else(|_| {
            panic!(
                "只有 {} 条请求到达假上游，期望至少 {n} 条：请求没有进到上游",
                gate.arrivals()
            )
        });
}

/// #94：并发名额按**网关模型**给——一个模型占住名额，不挡同一账户的另一个模型。
///
/// 两个模型都用部署缺省（1）。模型 A 的请求停在假上游时：
/// - 同一账户在模型 B 上再发 ⇒ **被受理**（`wait_for_arrival(2)` 就是证据：它走到了上游）；
/// - 同一账户在模型 A 上再发 ⇒ 429 `too_many_in_flight`。
///
/// 改之前这两条都按账户合计判定，B 也会被挡住——这条用例就是那个回归的守卫。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn another_model_is_not_blocked_by_an_in_flight_request() {
    let harness = Harness::start_direct_with(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        UpstreamBehaviour::aihubmix(SyncImageShape::Url).holding(HeldRequest::Create),
        1,
        30,
        ApiProcessSettings::default(),
    )
    .await;
    publish_second_model(&harness).await;

    // 模型 A 的第一个请求停在假上游：A 的名额被占住。
    let first = tokio::spawn({
        let base_url = harness.base_url.clone();
        let api_key = harness.api_key.clone();
        let key = format!("model-concurrency-a1-{}", Uuid::new_v4());
        let request = route_request(harness.model, "hold model A open");
        async move {
            post_json(
                &base_url,
                &api_key,
                "/v1/images/generations",
                &key,
                &request,
            )
            .await
        }
    });
    wait_for_arrival(&harness, 1).await;

    // 模型 B：自己的名额是空的，必须被受理。
    let second = tokio::spawn({
        let base_url = harness.base_url.clone();
        let api_key = harness.api_key.clone();
        let key = format!("model-concurrency-b1-{}", Uuid::new_v4());
        let request = route_request(SECOND_MODEL, "another model");
        async move {
            post_json(
                &base_url,
                &api_key,
                "/v1/images/generations",
                &key,
                &request,
            )
            .await
        }
    });
    wait_for_arrival(&harness, 2).await;

    // 模型 A 的第二个请求：A 的名额已满。
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &format!("model-concurrency-a2-{}", Uuid::new_v4()),
        &route_request(harness.model, "same model again"),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "got {body}");
    assert_eq!(body["error"]["code"], json!("too_many_in_flight"));

    harness.gate().release_all();
    let (status, body) = first.await.expect("the first request");
    assert_eq!(status, StatusCode::OK, "模型 A 的第一个请求：{body}");
    let (status, body) = second.await.expect("the second model's request");
    assert_eq!(status, StatusCode::OK, "模型 B 的请求：{body}");
}

/// #94：运营设的名额只管这个模型，而且**改完即时生效**（不重新发布、不重启）。
///
/// 模型 A 设 2：两个 A 请求同时在飞都被受理，第三个回 429。
/// 模型 B 没设：仍用部署缺省 1——一个在飞之后第二个回 429。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_quota_set_by_operations_bounds_only_that_model() {
    let harness = Harness::start_direct_with(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        UpstreamBehaviour::aihubmix(SyncImageShape::Url).holding(HeldRequest::Create),
        1,
        30,
        ApiProcessSettings::default(),
    )
    .await;
    publish_second_model(&harness).await;
    assert_eq!(
        patch_status(&harness, harness.model, json!({"max_concurrent_jobs": 2})).await,
        StatusCode::NO_CONTENT,
        "运营给模型 A 设 2 个名额"
    );
    assert_eq!(
        model_view(&harness, harness.model).await["max_concurrent_jobs"],
        json!(2),
        "读面要回显设过的名额"
    );
    assert_eq!(
        model_view(&harness, SECOND_MODEL).await["max_concurrent_jobs"],
        Value::Null,
        "没设过的模型回 null"
    );

    let spawn_request = |model: &'static str, tag: &str| {
        let base_url = harness.base_url.clone();
        let api_key = harness.api_key.clone();
        let key = format!("model-quota-{tag}-{}", Uuid::new_v4());
        let request = route_request(model, tag);
        tokio::spawn(async move {
            post_json(
                &base_url,
                &api_key,
                "/v1/images/generations",
                &key,
                &request,
            )
            .await
        })
    };
    // 模型 A 的两个请求都在飞：名额 2 的两个位置都被占住。
    let a1 = spawn_request(harness.model, "a1");
    let a2 = spawn_request(harness.model, "a2");
    wait_for_arrival(&harness, 2).await;
    // 第三个 A：超出名额。
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &format!("model-quota-a3-{}", Uuid::new_v4()),
        &route_request(harness.model, "a3"),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "got {body}");
    assert_eq!(body["error"]["code"], json!("too_many_in_flight"));

    // 模型 B 用部署缺省 1：第一个在飞，第二个就回 429。
    let b1 = spawn_request(SECOND_MODEL, "b1");
    wait_for_arrival(&harness, 3).await;
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &format!("model-quota-b2-{}", Uuid::new_v4()),
        &route_request(SECOND_MODEL, "b2"),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "got {body}");
    assert_eq!(body["error"]["code"], json!("too_many_in_flight"));

    harness.gate().release_all();
    for handle in [a1, a2, b1] {
        let (status, body) = handle.await.expect("an in-flight request");
        assert_eq!(status, StatusCode::OK, "放行后必须成功：{body}");
    }
}

/// #94：清成 `null` 回到部署缺省——不是"没有上限"。
///
/// 先设 2（两个 A 请求都能进），再清空；清空后一个在飞、第二个就回 429，说明生效的是缺省 1。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn clearing_the_quota_falls_back_to_the_deployment_default() {
    let harness = Harness::start_direct_with(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        UpstreamBehaviour::aihubmix(SyncImageShape::Url).holding(HeldRequest::Create),
        1,
        30,
        ApiProcessSettings::default(),
    )
    .await;
    assert_eq!(
        patch_status(&harness, harness.model, json!({"max_concurrent_jobs": 2})).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        patch_status(
            &harness,
            harness.model,
            json!({"max_concurrent_jobs": null})
        )
        .await,
        StatusCode::NO_CONTENT,
        "清成部署缺省"
    );
    assert_eq!(
        model_view(&harness, harness.model).await["max_concurrent_jobs"],
        Value::Null
    );

    let first = tokio::spawn({
        let base_url = harness.base_url.clone();
        let api_key = harness.api_key.clone();
        let key = format!("model-clear-a1-{}", Uuid::new_v4());
        let request = route_request(harness.model, "after clearing");
        async move {
            post_json(
                &base_url,
                &api_key,
                "/v1/images/generations",
                &key,
                &request,
            )
            .await
        }
    });
    wait_for_arrival(&harness, 1).await;
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &format!("model-clear-a2-{}", Uuid::new_v4()),
        &route_request(harness.model, "after clearing again"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "清空后生效的是部署缺省 1，所以第二个必须被挡：{body}"
    );
    assert_eq!(body["error"]["code"], json!("too_many_in_flight"));

    harness.gate().release_all();
    let (status, body) = first.await.expect("the first request");
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// #94：请求体的边界，以及"改了哪一项就写哪一条审计"。
///
/// 两个字段都可省略（只改一项），但都省略就是 400；`enabled` 不收 `null`；名额不收 0；
/// 未知字段仍拒（保留 `deny_unknown_fields`）。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn bad_patch_bodies_are_rejected_and_each_change_is_audited() {
    let harness = Harness::start_direct_with(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        1,
        30,
        ApiProcessSettings::default(),
    )
    .await;

    // 结构问题（未知字段）由解码层拒：422 的正文点名了字段与允许的字段。
    for (body, why) in [
        (
            json!({"max_concurrent_jobs": 2, "unknown": true}),
            "未知字段仍拒",
        ),
        (json!({"gateway_model": "renamed"}), "定义不能就地改"),
    ] {
        assert_eq!(
            patch_status(&harness, harness.model, body.clone()).await,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{why}：{body}"
        );
    }
    // 取值问题由处理函数拒成 400，正文要点名出问题的那个字段。
    for (body, why, named) in [
        (
            json!({}),
            "两个字段都不给＝没有要改的东西",
            "max_concurrent_jobs",
        ),
        (json!({"enabled": null}), "enabled 只收布尔", "enabled"),
        (
            json!({"max_concurrent_jobs": 0}),
            "名额至少 1",
            "max_concurrent_jobs",
        ),
        (
            json!({"max_concurrent_jobs": -1}),
            "名额不收负数",
            "max_concurrent_jobs",
        ),
        (
            json!({"max_concurrent_jobs": 3_000_000_000_i64}),
            "超出列宽的合法正整数也要 400，不能变成 500",
            "max_concurrent_jobs",
        ),
    ] {
        let (status, response) = patch_settings(&harness, harness.model, body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{why}：{body}");
        let message = response["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains(named),
            "{why}：正文要点名 {named}，实际是 {response}"
        );
    }

    // 只给名额：只写名额审计，不写开关审计。
    assert_eq!(
        patch_status(&harness, harness.model, json!({"max_concurrent_jobs": 2})).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        audit_count(&harness, "gateway_model.set_max_concurrent_jobs").await,
        1,
        "给了名额就写一条名额审计"
    );
    assert_eq!(
        audit_count(&harness, "gateway_model.set_enabled").await,
        0,
        "没给开关就不写开关审计"
    );
    // 只给开关：写开关审计。空改动也记一笔——"运营点过一次保存"本身要看得到，
    // 与 `set_offering_enabled` / `set_channel_enabled` 的写法一致。
    assert_eq!(
        patch_status(&harness, harness.model, json!({"enabled": false})).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        patch_status(&harness, harness.model, json!({"enabled": false})).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        audit_count(&harness, "gateway_model.set_enabled").await,
        2,
        "给两次开关就写两条（空改动也记）"
    );
    assert_eq!(
        model_view(&harness, harness.model).await["enabled"],
        json!(false)
    );
}
