use super::*;
use serde_json::json;

/// 管理面的**每一条**端点都要求凭据，而且"没带"与"带错了"回**逐字相同**的答复（Spec §4.1）。
///
/// 为什么要全矩阵而不是抽一条：这些端点以前各自在处理器里认证，收成一处中间件之后"漏掉一条"就变成
/// 了路由表少挂一层——抽验一条看不出来。表里的路径必须与 `main.rs` 里那个 `admin` Router 逐条对应。
///
/// 鉴权在**取数之前**：所以这里不需要造任何业务数据，随便给个标识就行；若某条端点回了 200/404，
/// 说明它没挂上认证层（那正是要抓的）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn every_admin_endpoint_requires_credentials() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    // 占位标识：鉴权先于取数，所以这些值不会被用到。用一个固定 UUID 让失败时的输出可读。
    const ID: &str = "00000000-0000-4000-8000-000000000000";

    // (方法, 路径, 是否带 JSON body)——PATCH/PUT/POST 都带上 body，免得被 body 解析拦在前面。
    let matrix: [(&str, String, bool); 27] = [
        ("POST", "/api/v1/accounts".to_owned(), true),
        ("GET", "/api/v1/accounts".to_owned(), false),
        ("GET", format!("/api/v1/accounts/{ID}"), false),
        ("GET", format!("/api/v1/accounts/{ID}/summary"), false),
        ("GET", format!("/api/v1/accounts/{ID}/entries"), false),
        ("PUT", format!("/api/v1/accounts/{ID}/tag"), true),
        ("PUT", format!("/api/v1/accounts/{ID}/name"), true),
        ("POST", format!("/api/v1/accounts/{ID}/credits"), true),
        ("POST", format!("/api/v1/accounts/{ID}/api-keys"), true),
        (
            "POST",
            format!("/api/v1/accounts/{ID}/password-reset"),
            false,
        ),
        ("GET", "/api/v1/customers".to_owned(), false),
        ("POST", "/api/v1/customers".to_owned(), true),
        ("GET", format!("/api/v1/customers/{ID}"), false),
        ("GET", "/api/v1/admin/session".to_owned(), false),
        ("PUT", "/api/v1/admin/password".to_owned(), true),
        ("POST", "/api/v1/admin/password-resets".to_owned(), true),
        ("PUT", "/api/v1/fx-rates".to_owned(), true),
        ("GET", "/api/v1/fx-rates".to_owned(), false),
        ("POST", "/api/v1/runtime-revisions".to_owned(), true),
        ("GET", "/api/v1/gateway-models".to_owned(), false),
        (
            "PATCH",
            "/api/v1/gateway-models/some-model".to_owned(),
            true,
        ),
        ("PATCH", format!("/api/v1/offerings/{ID}"), true),
        ("PATCH", format!("/api/v1/channels/{ID}"), true),
        ("GET", "/api/v1/route-policies".to_owned(), false),
        ("PUT", "/api/v1/route-policies".to_owned(), true),
        ("GET", "/api/v1/reconciliation-cases".to_owned(), false),
        (
            "POST",
            format!("/api/v1/reconciliation-cases/{ID}/refund"),
            true,
        ),
    ];

    // 三种"不被接受"的凭据：没带、带错、格式不对。三者必须回同一个答复。
    let mut reference: Option<(StatusCode, Value)> = None;
    for (method, path, with_body) in matrix {
        for (label, headers) in credential_variants() {
            let mut request =
                client.request(method.parse().expect("method"), format!("{base_url}{path}"));
            if let Some((name, value)) = headers {
                request = request.header(name, value);
            }
            if with_body {
                request = request.json(&json!({}));
            }
            let response = request.send().await.expect("admin request");
            let status = response.status();
            let body = response.json::<Value>().await.unwrap_or(Value::Null);
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "{method} {path}（{label}）必须被拒，实际 {status}：{body}"
            );
            match &reference {
                None => reference = Some((status, body)),
                Some((expected_status, expected_body)) => {
                    assert_eq!(
                        &status, expected_status,
                        "{method} {path}（{label}）状态码不一致"
                    );
                    assert_eq!(
                        &body, expected_body,
                        "{method} {path}（{label}）答复体与其他端点不一致"
                    );
                }
            }
        }
    }

    drop_isolated_database(&database_name).await;
}

/// 三种"这个请求不被接受"的凭据形态。返回 `(标签, Option<(头名, 头值)>)`。
///
/// 用 `String` 而不是 `&str`：`reqwest` 的 `header` 收的是能转成 `HeaderName`/`HeaderValue` 的类型，
/// `&'static str` 不在其中，`String` 在。
fn credential_variants() -> Vec<(&'static str, Option<(String, String)>)> {
    vec![
        ("没带凭据", None),
        (
            "凭据是错的",
            Some((
                "authorization".to_owned(),
                "Bearer not-a-real-token".to_owned(),
            )),
        ),
        (
            "格式不对",
            Some(("authorization".to_owned(), "not-even-bearer".to_owned())),
        ),
    ]
}

/// 账户列表：运营能**先找到再操作**（按邮箱或标签），而不是必须先知道账户标识。
///
/// 这是 Spec V-D10 的接口基础——"充值不需要先知道账户标识"必须有一条读能把邮箱或标签换成账户。
/// 没有登录身份的账户（运营直接建的）也必须出现在不带筛选的列表里，否则运营建完账户就再也找不回它。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn accounts_can_be_found_by_email_or_tag_without_knowing_the_identifier() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    // 一个只带标签、**没有登录身份**的账户：运营直接建的账户就是这种，它也必须能被找回来。
    let tagged = client
        .post(format!("{base_url}/api/v1/accounts"))
        .bearer_auth(&admin_token)
        .json(&json!({ "initial_credit_microusd": 1_000_000 }))
        .send()
        .await
        .expect("create tagged account");
    assert!(tagged.status().is_success(), "建账户：{}", tagged.status());
    let tagged_id = tagged.json::<Value>().await.expect("account id")["account_id"]
        .as_str()
        .expect("account_id")
        .to_owned();

    let put_tag = client
        .put(format!("{base_url}/api/v1/accounts/{tagged_id}/tag"))
        .bearer_auth(&admin_token)
        .json(&json!({ "tag": "vip-e2e" }))
        .send()
        .await
        .expect("set tag");
    assert!(
        put_tag.status().is_success(),
        "设标签：{}",
        put_tag.status()
    );

    // 另一个账户带上邮箱登录身份：用来验"按邮箱找"。
    let email = format!("find-{}@example.com", Uuid::new_v4());
    let bound = client
        .post(format!("{base_url}/api/v1/customers"))
        .bearer_auth(&admin_token)
        .json(&json!({ "email": email, "password": "a-long-enough-password" }))
        .send()
        .await
        .expect("open customer");
    assert!(bound.status().is_success(), "开户：{}", bound.status());
    let bound_id = bound.json::<Value>().await.expect("customer view")["account_id"]
        .as_str()
        .expect("account_id")
        .to_owned();

    // 按标签找：只回那一个，且带着余额与标签——列表要能直接支撑"挑出目标账户"。
    let by_tag = client
        .get(format!("{base_url}/api/v1/accounts?tag=vip-e2e"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("list by tag");
    assert_eq!(by_tag.status(), StatusCode::OK);
    let by_tag: Value = by_tag.json().await.expect("accounts");
    let rows = by_tag["accounts"].as_array().expect("accounts array");
    assert_eq!(rows.len(), 1, "按标签只该命中一个：{by_tag}");
    assert_eq!(rows[0]["account_id"].as_str(), Some(tagged_id.as_str()));
    assert_eq!(rows[0]["tag"].as_str(), Some("vip-e2e"));
    assert_eq!(rows[0]["balance_microusd"].as_i64(), Some(1_000_000));
    // 这个账户没有登录身份，所以邮箱是 `null`——它是"运营直接建的账户"这个事实，不是"没取到"。
    assert_eq!(
        rows[0]["email"],
        Value::Null,
        "没有登录身份的账户，邮箱应当是 null：{by_tag}"
    );

    // 按邮箱找：邮箱比对大小写不敏感（登录用的那个邮箱也不敏感，两处必须一致）。
    let by_email = client
        .get(format!(
            "{base_url}/api/v1/accounts?email={}",
            email.to_uppercase()
        ))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("list by email");
    assert_eq!(by_email.status(), StatusCode::OK);
    let by_email: Value = by_email.json().await.expect("accounts");
    let rows = by_email["accounts"].as_array().expect("accounts array");
    assert_eq!(rows.len(), 1, "按邮箱只该命中一个：{by_email}");
    assert_eq!(rows[0]["account_id"].as_str(), Some(bound_id.as_str()));
    // 列表项要带上邮箱：运营是**按邮箱**找账户的，搜出来的行必须能确认是谁。
    assert_eq!(
        rows[0]["email"].as_str(),
        Some(email.as_str()),
        "绑定了登录身份的账户，列表项要回传邮箱：{by_email}"
    );

    // 邮箱与标签同时给：**与**的关系，两条各命中一个不同的账户，所以结果为空。
    let both = client
        .get(format!(
            "{base_url}/api/v1/accounts?email={email}&tag=vip-e2e"
        ))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("list by both");
    let both: Value = both.json().await.expect("accounts");
    assert!(
        both["accounts"].as_array().expect("accounts").is_empty(),
        "两个条件是与的关系，不该命中任何账户：{both}"
    );

    // 不带任何筛选：**所有**账户都在，包括那个没有登录身份的——运营建完账户必须还能找回它。
    let all = client
        .get(format!("{base_url}/api/v1/accounts"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("list all");
    let all: Value = all.json().await.expect("accounts");
    let ids: Vec<&str> = all["accounts"]
        .as_array()
        .expect("accounts")
        .iter()
        .map(|row| row["account_id"].as_str().expect("account_id"))
        .collect();
    assert!(
        ids.contains(&tagged_id.as_str()),
        "没有登录身份的账户也要在：{all}"
    );
    assert!(
        ids.contains(&bound_id.as_str()),
        "带登录身份的账户要在：{all}"
    );

    // 空串按"没给"处理：`?email=` 不该被当成"找一个邮箱为空字符串的账户"（那会永远空）。
    let blank = client
        .get(format!("{base_url}/api/v1/accounts?email=&tag="))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("list with blank filters");
    let blank: Value = blank.json().await.expect("accounts");
    assert_eq!(
        blank["accounts"].as_array().expect("accounts").len(),
        ids.len(),
        "空串等于不筛，结果应与不带筛选一致：{blank}"
    );

    // 不存在的标签：给空数组而不是报错。
    let missing = client
        .get(format!(
            "{base_url}/api/v1/accounts?tag=no-such-tag-{}",
            Uuid::new_v4()
        ))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("list by unknown tag");
    assert_eq!(missing.status(), StatusCode::OK);
    let missing: Value = missing.json().await.expect("accounts");
    assert!(missing["accounts"].as_array().expect("accounts").is_empty());

    // 未知的查询参数被拒：静默忽略会让调用方以为筛选生效了。
    let unknown = client
        .get(format!("{base_url}/api/v1/accounts?nope=1"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("list with unknown query");
    assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);

    drop_isolated_database(&database_name).await;
}

/// 账户与客户的详情能**按标识**读回来：这两条是详情页直达与刷新的读（Spec V-D15、设计 `0011` §6.3）。
///
/// 列表读不算答案：它有筛选与条数上限，刷新一个较旧对象的详情时可能根本不在结果里。所以详情页只
/// 拿地址里的标识取数，这两条端点必须按标识稳定命中原对象，且响应与列表项同形、不含任何凭据。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn account_and_customer_details_are_readable_by_identifier() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url, &admin_token).await;

    // 一个带标签与余额、**还没有登录身份**的账户；摘要的 `email` 这时必须是 `null`。
    let created = client
        .post(format!("{base_url}/api/v1/accounts"))
        .bearer_auth(&admin_token)
        .json(&json!({ "initial_credit_microusd": 3_210_000 }))
        .send()
        .await
        .expect("create account");
    assert!(
        created.status().is_success(),
        "建账户：{}",
        created.status()
    );
    let account_id = created.json::<Value>().await.expect("account id")["account_id"]
        .as_str()
        .expect("account_id")
        .to_owned();
    assert!(
        client
            .put(format!("{base_url}/api/v1/accounts/{account_id}/tag"))
            .bearer_auth(&admin_token)
            .json(&json!({ "tag": "detail-e2e" }))
            .send()
            .await
            .expect("set tag")
            .status()
            .is_success()
    );

    let bare: Value = client
        .get(format!("{base_url}/api/v1/accounts/{account_id}/summary"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("account summary")
        .json()
        .await
        .expect("account summary body");
    assert_eq!(bare["account_id"].as_str(), Some(account_id.as_str()));
    assert_eq!(bare["balance_microusd"].as_i64(), Some(3_210_000));
    assert_eq!(bare["tag"].as_str(), Some("detail-e2e"));
    assert_eq!(bare["email"], Value::Null, "还没有登录身份：{bare}");
    assert!(bare["created_at"].is_string(), "摘要要带创建时刻：{bare}");
    assert!(bare["updated_at"].is_string(), "摘要要带更新时刻：{bare}");

    // 同一个账户从列表里读一次：两条读的字段与取值必须一致，页面才不会"刷新前后不一样"。
    let listed: Value = client
        .get(format!("{base_url}/api/v1/accounts?tag=detail-e2e"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("list by tag")
        .json()
        .await
        .expect("accounts body");
    let rows = listed["accounts"].as_array().expect("accounts array");
    assert_eq!(rows.len(), 1, "按标签只该命中一个：{listed}");
    assert_eq!(
        rows[0], bare,
        "按标识的摘要与列表项必须同形同值：{listed} vs {bare}"
    );

    // 配一个登录身份之后，摘要要带上邮箱；客户视图能按 `customer_id` 读回来。
    let email = format!("detail-{}@example.com", Uuid::new_v4());
    let opened = client
        .post(format!("{base_url}/api/v1/customers"))
        .bearer_auth(&admin_token)
        .json(&json!({
            "email": email,
            "password": "a-long-enough-password",
            "account_id": account_id,
        }))
        .send()
        .await
        .expect("open customer");
    assert_eq!(opened.status(), StatusCode::CREATED);
    let customer: Value = opened.json().await.expect("customer view");
    let customer_id = customer["customer_id"]
        .as_str()
        .expect("customer_id")
        .to_owned();

    let bound: Value = client
        .get(format!("{base_url}/api/v1/accounts/{account_id}/summary"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("account summary with identity")
        .json()
        .await
        .expect("account summary body");
    assert_eq!(bound["email"].as_str(), Some(email.as_str()));

    let read: Value = client
        .get(format!("{base_url}/api/v1/customers/{customer_id}"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("customer view by id")
        .json()
        .await
        .expect("customer view body");
    assert_eq!(read["customer_id"].as_str(), Some(customer_id.as_str()));
    assert_eq!(read["email"].as_str(), Some(email.as_str()));
    assert_eq!(read["account_id"].as_str(), Some(account_id.as_str()));
    assert!(
        read["created_at"].is_string(),
        "客户视图要带创建时刻：{read}"
    );
    assert_eq!(read["last_login_at"], Value::Null, "还没登录过：{read}");
    // 与开户那次的响应同形同值：开户后进入的详情页就是这条读的样子。
    assert_eq!(read, customer, "按标识的客户视图要与开户响应一致：{read}");

    // **不返回任何凭据**：口令、会话、API Key 明文、重置令牌都不该出现在这两个响应里。
    let mut keys = vec![];
    for value in [&bare, &read] {
        keys.extend(
            value
                .as_object()
                .expect("object")
                .keys()
                .map(String::as_str),
        );
    }
    for forbidden in [
        "password",
        "password_hash",
        "token",
        "reset_token",
        "api_key",
        "session",
    ] {
        assert!(
            !keys.contains(&forbidden),
            "响应里不该出现 {forbidden}：{keys:?}"
        );
    }

    // 不存在的标识：404，而不是空对象或 500——详情页据此显示找不到并清掉上一个对象。
    let unknown = Uuid::new_v4();
    for path in [
        format!("{base_url}/api/v1/accounts/{unknown}/summary"),
        format!("{base_url}/api/v1/customers/{unknown}"),
    ] {
        let response = client
            .get(&path)
            .bearer_auth(&admin_token)
            .send()
            .await
            .expect("read unknown detail");
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
    // 不是 UUID 的标识：400（参数不成立），与"这个对象不存在"分开。
    let malformed = client
        .get(format!("{base_url}/api/v1/customers/not-a-uuid"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("read malformed id");
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);

    drop_isolated_database(&database_name).await;
}
