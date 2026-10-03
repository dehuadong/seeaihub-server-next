//! 账户名称：生成、校验、子串查找、改名（运营与客户各一条路）与既有读的投影。
//!
//! 覆盖 Spec `0003` v4 的 N1–N6、F1–F6、U1–U6 与 §5 的接口增量；界面部分由浏览器用例覆盖
//! （V1–V10 的完整归属见设计 `0015` §6）。

use super::*;
use serde_json::json;

/// 起一个最小的夹具：账户名称不需要任何供给或模型，空契约起 API 就够。
async fn names_harness() -> Harness {
    Harness::start(UpstreamBehaviour::apimart()).await
}

async fn create_account(client: &Client, harness: &Harness, body: Value) -> (StatusCode, Value) {
    let response = client
        .post(format!("{}/api/v1/accounts", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .json(&body)
        .send()
        .await
        .expect("create account request");
    let status = response.status();
    let body = response.json::<Value>().await.unwrap_or(Value::Null);
    (status, body)
}

async fn account_summary(client: &Client, harness: &Harness, account_id: &str) -> Value {
    let response = client
        .get(format!(
            "{}/api/v1/accounts/{account_id}/summary",
            harness.base_url
        ))
        .bearer_auth(&harness.admin_token)
        .send()
        .await
        .expect("account summary request");
    assert_eq!(response.status(), StatusCode::OK);
    response.json::<Value>().await.expect("summary JSON")
}

async fn list_names(client: &Client, harness: &Harness, name: &str) -> Vec<String> {
    let response = client
        .get(format!(
            "{}/api/v1/accounts?name={}&limit=100",
            harness.base_url,
            urlencoding(name)
        ))
        .bearer_auth(&harness.admin_token)
        .send()
        .await
        .expect("account list request");
    assert_eq!(response.status(), StatusCode::OK);
    response.json::<Value>().await.expect("list JSON")["accounts"]
        .as_array()
        .expect("accounts array")
        .iter()
        .map(|account| account["name"].as_str().expect("name").to_owned())
        .collect()
}

/// 最小的百分号／下划线编码：这两个字符在查询串里会被当转义用，测试要发它们的**字面值**。
fn urlencoding(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace('_', "%5F")
        .replace(' ', "%20")
}

/// 留空即生成：没有邮箱可读时退到 `账户_<id 前 8 位>`，并且能按名称找到。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn an_omitted_name_is_generated_and_searchable() {
    let harness = names_harness().await;
    let client = Client::new();

    let (status, created) =
        create_account(&client, &harness, json!({"initial_credit_microusd": 0})).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let account_id = created["account_id"]
        .as_str()
        .expect("account id")
        .to_owned();

    let summary = account_summary(&client, &harness, &account_id).await;
    let generated = summary["name"]
        .as_str()
        .expect("name is never null")
        .to_owned();
    assert_eq!(
        generated,
        format!("账户_{}", &account_id[..8]),
        "没有邮箱时按账户 id 前 8 位生成"
    );

    // 名称能被子串搜到，且账户摘要与列表读的是同一个值。
    assert!(
        list_names(&client, &harness, &generated)
            .await
            .contains(&generated),
        "刚生成的账户必须能按名称找到"
    );

    harness.cleanup().await;
}

/// 给出的名称：去首尾空白、按 Unicode scalar 计数、拒控制字符与格式字符；显式 `null` 不是“省略”。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_provided_name_is_trimmed_and_rejected_when_malformed() {
    let harness = names_harness().await;
    let client = Client::new();

    let (status, created) = create_account(
        &client,
        &harness,
        json!({"initial_credit_microusd": 0, "name": "  星尘  工作室  "}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let account_id = created["account_id"].as_str().expect("account id");
    let summary = account_summary(&client, &harness, account_id).await;
    assert_eq!(
        summary["name"],
        json!("星尘  工作室"),
        "首尾去空白、内部空格保留"
    );

    // 非法输入一律 400：空串、纯空白、显式 null、控制字符、超长。
    let too_long = "a".repeat(101);
    for body in [
        json!({"initial_credit_microusd": 0, "name": ""}),
        json!({"initial_credit_microusd": 0, "name": "   "}),
        json!({"initial_credit_microusd": 0, "name": null}),
        json!({"initial_credit_microusd": 0, "name": "星尘\t工作室"}),
        json!({"initial_credit_microusd": 0, "name": too_long}),
    ] {
        let (status, body) = create_account(&client, &harness, body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "应当被拒：{body}");
    }
    // 100 个字符可以。
    let (status, _) = create_account(
        &client,
        &harness,
        json!({"initial_credit_microusd": 0, "name": "a".repeat(100)}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "正好 100 个字符应当通过");

    harness.cleanup().await;
}

/// 名称查询是**字面子串**：`%` 与 `_` 当普通字符，大小写不敏感。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn names_are_searched_as_literal_substrings() {
    let harness = names_harness().await;
    let client = Client::new();

    for name in ["星尘工作室", "100%_折扣", "别家工作室"] {
        let (status, body) = create_account(
            &client,
            &harness,
            json!({"initial_credit_microusd": 0, "name": name}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    assert_eq!(
        list_names(&client, &harness, "星尘").await,
        vec!["星尘工作室"]
    );
    let mut both = list_names(&client, &harness, "工作室").await;
    both.sort();
    assert_eq!(both, vec!["别家工作室", "星尘工作室"]);
    // `%` 与 `_` 不能当通配符：搜它们只命中名字里**真的有**那个字符的账户。
    // 夹具自己那个账户的名称（`账户_<id 前 8 位>`）含 `_` 不含 `%`，所以两条的期望不同。
    let percent = list_names(&client, &harness, "%").await;
    assert_eq!(percent, vec!["100%_折扣"], "`%` 只作字面匹配");
    let underscore = list_names(&client, &harness, "_").await;
    assert!(
        underscore.contains(&"100%_折扣".to_owned()),
        "`_` 要命中名字里真的有下划线的账户：{underscore:?}"
    );
    assert!(
        !underscore.contains(&"星尘工作室".to_owned()),
        "`_` 不能当通配符去命中没有下划线的名字：{underscore:?}"
    );

    harness.cleanup().await;
}

/// 名称筛选的其余分支：Latin 大小写不敏感、与邮箱／标签按「与」组合。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn name_search_ignores_case_and_combines_with_email_and_tag() {
    let harness = names_harness().await;
    let client = Client::new();

    // 三个名字互不相同的账户（名称唯一，Spec N1），共用一个标签；其中一个再配上登录邮箱。
    let mut created = Vec::new();
    for (name, tag) in [
        ("StarStudio", "vip"),
        ("星尘工作室", "vip"),
        ("OtherStudio", "vip"),
    ] {
        let (status, body) = create_account(
            &client,
            &harness,
            json!({"initial_credit_microusd": 0, "name": name, "tag": tag}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        created.push(body["account_id"].as_str().expect("account id").to_owned());
    }
    let bound = client
        .post(format!("{}/api/v1/customers", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .json(&json!({"email": "star@example.com", "account_id": created[0]}))
        .send()
        .await
        .expect("open customer request");
    assert_eq!(bound.status(), StatusCode::CREATED);

    // 大小写不敏感的子串匹配：小写查询命中的是大写的那个账户。
    assert_eq!(
        list_names(&client, &harness, "starstudio").await,
        vec!["StarStudio"]
    );
    assert_eq!(
        list_names(&client, &harness, "STUDIO").await.len(),
        2,
        "两个 Studio 都命中"
    );

    // 名称 + 标签：命中这两个 vip（换成不存在的标签就是空）。
    let filtered = client
        .get(format!(
            "{}/api/v1/accounts?name=studio&tag=vip&limit=100",
            harness.base_url
        ))
        .bearer_auth(&harness.admin_token)
        .send()
        .await
        .expect("filtered list request")
        .json::<Value>()
        .await
        .expect("filtered list");
    let ids: Vec<String> = filtered["accounts"]
        .as_array()
        .expect("accounts")
        .iter()
        .map(|account| account["account_id"].as_str().expect("id").to_owned())
        .collect();
    assert_eq!(ids.len(), 2, "名称与标签是「与」的关系：{filtered}");
    assert!(ids.contains(&created[0]));

    // 名称 + 邮箱：只有绑了那个邮箱的那一行。
    let by_email = client
        .get(format!(
            "{}/api/v1/accounts?name=studio&email=star@example.com&limit=100",
            harness.base_url
        ))
        .bearer_auth(&harness.admin_token)
        .send()
        .await
        .expect("filtered list request")
        .json::<Value>()
        .await
        .expect("filtered list");
    let rows = by_email["accounts"].as_array().expect("accounts");
    assert_eq!(rows.len(), 1, "名称与邮箱是「与」的关系：{by_email}");
    assert_eq!(rows[0]["account_id"], json!(created[0]));

    harness.cleanup().await;
}

/// 名称唯一（Spec N1/N6，**区分大小写**）：完全相同才冲突，只差大小写是两个名称。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn account_names_are_unique_case_sensitively() {
    let harness = names_harness().await;
    let client = Client::new();

    let (status, first) = create_account(
        &client,
        &harness,
        json!({"initial_credit_microusd": 0, "name": "StarStudio"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let first_id = first["account_id"].as_str().expect("account id").to_owned();

    // 逐字符完全相同（含首尾空白被去掉之后相同）被拒：`409`，且不产生账户。
    let accounts_before = client
        .get(format!("{}/api/v1/accounts?limit=100", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .send()
        .await
        .expect("account list request")
        .json::<Value>()
        .await
        .expect("account list")["accounts"]
        .as_array()
        .expect("accounts")
        .len();
    for duplicate in ["StarStudio", " StarStudio "] {
        let (status, body) = create_account(
            &client,
            &harness,
            json!({"initial_credit_microusd": 0, "name": duplicate}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{duplicate} 应当冲突：{body}");
        assert_eq!(body["error"]["code"], json!("name_taken"), "{body}");
    }
    let accounts_after = client
        .get(format!("{}/api/v1/accounts?limit=100", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .send()
        .await
        .expect("account list request")
        .json::<Value>()
        .await
        .expect("account list")["accounts"]
        .as_array()
        .expect("accounts")
        .len();
    assert_eq!(accounts_after, accounts_before, "撞名的创建一次都没有落库");

    // 只差大小写是**另一个**名称：可以并存，各是一个账户。
    for variant in ["starstudio", "STARSTUDIO"] {
        let (status, body) = create_account(
            &client,
            &harness,
            json!({"initial_credit_microusd": 0, "name": variant}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{variant} 是另一个名称：{body}");
    }
    let mut found = list_names(&client, &harness, "starstudio").await;
    found.sort();
    assert_eq!(found, vec!["STARSTUDIO", "StarStudio", "starstudio"]);
    // 子串查询本身仍是大小写不敏感：换一种大小写查同一批账户。
    let mut caseless = list_names(&client, &harness, "Studio").await;
    caseless.sort();
    assert_eq!(caseless, found, "筛选大小写不敏感，唯一性区分大小写");

    // 改名撞另一个账户的完全相同名称是 `409`；改成只差大小写的名称允许。
    let (status, second) = create_account(
        &client,
        &harness,
        json!({"initial_credit_microusd": 0, "name": "Another Studio"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    let second_id = second["account_id"]
        .as_str()
        .expect("account id")
        .to_owned();

    let taken = client
        .put(format!(
            "{}/api/v1/accounts/{second_id}/name",
            harness.base_url
        ))
        .bearer_auth(&harness.admin_token)
        .json(&json!({"name": "StarStudio"}))
        .send()
        .await
        .expect("rename request");
    assert_eq!(taken.status(), StatusCode::CONFLICT);
    let conflict_body = taken.json::<Value>().await.expect("error body");
    assert_eq!(
        conflict_body["error"]["code"],
        json!("name_taken"),
        "{conflict_body}"
    );
    assert_eq!(
        account_summary(&client, &harness, &second_id).await["name"],
        json!("Another Studio"),
        "冲突的改名不改动任何资料"
    );

    let variant = client
        .put(format!(
            "{}/api/v1/accounts/{second_id}/name",
            harness.base_url
        ))
        .bearer_auth(&harness.admin_token)
        .json(&json!({"name": "ANOTHER STUDIO"}))
        .send()
        .await
        .expect("rename request");
    assert_eq!(
        variant.status(),
        StatusCode::NO_CONTENT,
        "大小写变体是另一个名称"
    );
    assert_eq!(
        account_summary(&client, &harness, &second_id).await["name"],
        json!("ANOTHER STUDIO")
    );

    // 改成自己当前的名称等于没改：不算冲突。
    let unchanged = client
        .put(format!(
            "{}/api/v1/accounts/{first_id}/name",
            harness.base_url
        ))
        .bearer_auth(&harness.admin_token)
        .json(&json!({"name": "StarStudio"}))
        .send()
        .await
        .expect("rename own request");
    assert_eq!(unchanged.status(), StatusCode::NO_CONTENT);

    // 不存在的账户带一个被占用的名称：不存在优先（`404`），不是冲突。
    let missing = client
        .put(format!(
            "{}/api/v1/accounts/00000000-0000-4000-8000-000000000000/name",
            harness.base_url
        ))
        .bearer_auth(&harness.admin_token)
        .json(&json!({"name": "StarStudio"}))
        .send()
        .await
        .expect("rename missing request");
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    // 客户改自己的名称同样受唯一约束：改成别人已用的完全相同名称是冲突，原值不变；
    // 改成只差大小写的名称允许。
    let registered = client
        .post(format!("{}/v1/customers", harness.base_url))
        .json(&json!({"email": "unique@example.com", "password": "a-long-enough-password"}))
        .send()
        .await
        .expect("register request")
        .json::<Value>()
        .await
        .expect("register body");
    let session = registered["token"]
        .as_str()
        .expect("session token")
        .to_owned();
    let customer_account = registered["account_id"]
        .as_str()
        .expect("account id")
        .to_owned();
    assert!(customer_account != first_id && customer_account != second_id);
    let conflict = client
        .put(format!("{}/v1/customer/account/name", harness.base_url))
        .bearer_auth(&session)
        .json(&json!({"name": "StarStudio"}))
        .send()
        .await
        .expect("customer rename request");
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    let account = client
        .get(format!("{}/v1/customer/account", harness.base_url))
        .bearer_auth(&session)
        .send()
        .await
        .expect("customer account request")
        .json::<Value>()
        .await
        .expect("account body");
    assert_eq!(
        account["name"],
        json!(format!("unique_{}", &customer_account[..4])),
        "冲突的改名不改动客户自己的名称"
    );

    // 只差大小写是另一个名称：客户把自己的名称换成大写形式，改得成。
    let upper = format!("UNIQUE_{}", customer_account[..4].to_uppercase());
    let variant = client
        .put(format!("{}/v1/customer/account/name", harness.base_url))
        .bearer_auth(&session)
        .json(&json!({"name": upper}))
        .send()
        .await
        .expect("customer rename request");
    assert_eq!(
        variant.status(),
        StatusCode::NO_CONTENT,
        "大小写变体是另一个名称"
    );
    let account = client
        .get(format!("{}/v1/customer/account", harness.base_url))
        .bearer_auth(&session)
        .send()
        .await
        .expect("customer account request")
        .json::<Value>()
        .await
        .expect("account body");
    assert_eq!(account["name"], json!(upper));

    harness.cleanup().await;
}

/// 生成路径自己解决撞名：同一邮箱本地部分的两个账户都注册成功，名称互不相同（Spec N3）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn generated_names_stay_unique_for_the_same_email_local_part() {
    let harness = names_harness().await;
    let client = Client::new();

    let mut names = Vec::new();
    for domain in ["a.example.com", "b.example.com", "c.example.com"] {
        let registered = client
            .post(format!("{}/v1/customers", harness.base_url))
            .json(&json!({
                "email": format!("zhangsan@{domain}"),
                "password": "a-long-enough-password",
            }))
            .send()
            .await
            .expect("register request");
        assert_eq!(registered.status(), StatusCode::CREATED, "{domain}");
        let registered = registered.json::<Value>().await.expect("register body");
        let session = registered["token"].as_str().expect("session token");
        let account = client
            .get(format!("{}/v1/customer/account", harness.base_url))
            .bearer_auth(session)
            .send()
            .await
            .expect("customer account request")
            .json::<Value>()
            .await
            .expect("account body");
        let name = account["name"].as_str().expect("name").to_owned();
        assert!(name.starts_with("zhangsan_"), "按邮箱本地部分生成：{name}");
        assert!(
            !names.contains(&name),
            "生成名称必须唯一：{names:?} 里已有 {name}"
        );
        names.push(name);
    }

    // 三个账户都在库里。
    assert_eq!(list_names(&client, &harness, "zhangsan_").await.len(), 3);

    // 管理端「新建账户并开通邮箱登录」这条路径同样按候选生成：留空时也得唯一，且同样以本地部分开头。
    let mut opened_names = Vec::new();
    for domain in ["d.example.com", "e.example.com"] {
        let opened = client
            .post(format!("{}/api/v1/customers", harness.base_url))
            .bearer_auth(&harness.admin_token)
            .json(&json!({"email": format!("zhangsan@{domain}")}))
            .send()
            .await
            .expect("open customer request");
        assert_eq!(opened.status(), StatusCode::CREATED, "{domain}");
        let view = opened.json::<Value>().await.expect("customer view");
        let name = view["account_name"]
            .as_str()
            .expect("account name")
            .to_owned();
        assert!(name.starts_with("zhangsan_"), "{name}");
        assert!(
            !names.contains(&name) && !opened_names.contains(&name),
            "两条路径生成的名字都不能重复：{names:?} {opened_names:?} 里已有 {name}"
        );
        opened_names.push(name);
    }

    harness.cleanup().await;
}

/// 改名只动名称：余额、标签、绑定邮箱与密钥都不受影响；不存在的账户 404，非法名称 400。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn renaming_an_account_keeps_money_tags_and_keys() {
    let harness = names_harness().await;
    let client = Client::new();

    let (status, created) = create_account(
        &client,
        &harness,
        json!({"initial_credit_microusd": 1_000_000, "tag": "vip", "name": "旧名字"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let account_id = created["account_id"]
        .as_str()
        .expect("account id")
        .to_owned();
    let api_key = issue_key(
        &client,
        &harness.base_url,
        &harness.admin_token,
        &account_id,
    )
    .await;

    let renamed = client
        .put(format!(
            "{}/api/v1/accounts/{account_id}/name",
            harness.base_url
        ))
        .bearer_auth(&harness.admin_token)
        .json(&json!({"name": "  新名字  "}))
        .send()
        .await
        .expect("rename request");
    assert_eq!(renamed.status(), StatusCode::NO_CONTENT);

    let summary = account_summary(&client, &harness, &account_id).await;
    assert_eq!(summary["name"], json!("新名字"));
    assert_eq!(summary["tag"], json!("vip"), "改名不动标签");
    assert_eq!(
        summary["balance_microusd"],
        json!(1_000_000),
        "改名不动金额"
    );
    assert_eq!(
        list_names(&client, &harness, "新名字").await,
        vec!["新名字"],
        "按新名称能搜到"
    );

    // 密钥仍然可用：改名不碰凭据。
    let own = client
        .get(format!("{}/v1/account", harness.base_url))
        .bearer_auth(&api_key)
        .send()
        .await
        .expect("api key account read");
    assert_eq!(own.status(), StatusCode::OK);

    // 不存在的账户 404；非法名称 400。
    let missing = client
        .put(format!(
            "{}/api/v1/accounts/00000000-0000-4000-8000-000000000000/name",
            harness.base_url
        ))
        .bearer_auth(&harness.admin_token)
        .json(&json!({"name": "谁"}))
        .send()
        .await
        .expect("rename missing request");
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    let invalid = client
        .put(format!(
            "{}/api/v1/accounts/{account_id}/name",
            harness.base_url
        ))
        .bearer_auth(&harness.admin_token)
        .json(&json!({"name": "  "}))
        .send()
        .await
        .expect("rename invalid request");
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

    harness.cleanup().await;
}

/// 自助注册：按邮箱生成名称；客户能改自己的名称，运营侧与客户侧读同一份值，非法名称被拒。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_self_registered_customer_gets_a_generated_name_and_can_rename_it() {
    let harness = names_harness().await;
    let client = Client::new();

    // 自助注册（公开端点）：邮箱 zhangsan@… → 名称 zhangsan_<id 前 4 位>。
    let registered = client
        .post(format!("{}/v1/customers", harness.base_url))
        .json(&json!({"email": "zhangsan@example.com", "password": "a-long-enough-password"}))
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

    let account = client
        .get(format!("{}/v1/customer/account", harness.base_url))
        .bearer_auth(&session)
        .send()
        .await
        .expect("customer account request");
    assert_eq!(account.status(), StatusCode::OK);
    let account = account.json::<Value>().await.expect("account body");
    assert_eq!(
        account["name"],
        json!(format!("zhangsan_{}", &account_id[..4])),
        "自助注册按邮箱本地部分加 id 前 4 位生成：{account}"
    );

    // 客户改自己的名称：客户侧与运营侧读到同一个新值。
    let renamed = client
        .put(format!("{}/v1/customer/account/name", harness.base_url))
        .bearer_auth(&session)
        .json(&json!({"name": "张三的工作室"}))
        .send()
        .await
        .expect("customer rename request");
    let renamed_status = renamed.status();
    let renamed_body = renamed.text().await.unwrap_or_default();
    assert_eq!(
        renamed_status,
        StatusCode::NO_CONTENT,
        "客户改名应当成功：{renamed_body}"
    );
    let account = client
        .get(format!("{}/v1/customer/account", harness.base_url))
        .bearer_auth(&session)
        .send()
        .await
        .expect("customer account request")
        .json::<Value>()
        .await
        .expect("account body");
    assert_eq!(account["name"], json!("张三的工作室"));
    assert_eq!(
        account_summary(&client, &harness, &account_id).await["name"],
        json!("张三的工作室"),
        "运营侧读的是同一个账户行"
    );

    // 清空与控制字符都被拒：客户不能把自己的账户改成空名。
    for value in [json!("   "), json!("张三\t工作室")] {
        let rejected = client
            .put(format!("{}/v1/customer/account/name", harness.base_url))
            .bearer_auth(&session)
            .json(&json!({"name": value}))
            .send()
            .await
            .expect("customer rename request");
        assert_eq!(
            rejected.status(),
            StatusCode::BAD_REQUEST,
            "{value} 应当被拒"
        );
    }

    // 运营改名 → 客户页面读到新值（两个方向都要覆盖）。
    let admin_renamed = client
        .put(format!(
            "{}/api/v1/accounts/{account_id}/name",
            harness.base_url
        ))
        .bearer_auth(&harness.admin_token)
        .json(&json!({"name": "张三工作室（运营改名）"}))
        .send()
        .await
        .expect("admin rename request");
    assert_eq!(admin_renamed.status(), StatusCode::NO_CONTENT);
    let account = client
        .get(format!("{}/v1/customer/account", harness.base_url))
        .bearer_auth(&session)
        .send()
        .await
        .expect("customer account request")
        .json::<Value>()
        .await
        .expect("account body");
    assert_eq!(account["name"], json!("张三工作室（运营改名）"));

    // 客户登录那条读（管理端客户视图）也显示当前名称：列表与按邮箱查都看得到。
    let customers = client
        .get(format!(
            "{}/api/v1/customers?email=zhangsan@example.com",
            harness.base_url
        ))
        .bearer_auth(&harness.admin_token)
        .send()
        .await
        .expect("customers request")
        .json::<Value>()
        .await
        .expect("customers body");
    assert_eq!(
        customers["customers"][0]["account_name"],
        json!("张三工作室（运营改名）"),
        "客户视图读的是关联账户的当前名称：{customers}"
    );

    // 审计：两条改名各留一条 account.name_set，操作者分别是对客自助与管理员，payload 记旧新值。
    let audits: Vec<(String, Value)> = sqlx::query_as(
        "SELECT actor, payload FROM operations.audit_events
         WHERE subject_id = $1 AND action = 'account.name_set'
         ORDER BY created_at ASC",
    )
    // `subject_id` 是 text：账户标识按字符串比，不做 uuid 转换。
    .bind(account_id.as_str())
    .fetch_all(&harness.pool)
    .await
    .expect("audit rows");
    assert_eq!(audits.len(), 2, "两条改名各留一条审计：{audits:?}");
    assert_eq!(audits[0].0, "customer-self-service");
    assert_eq!(
        audits[0].1["previous_name"],
        json!(format!("zhangsan_{}", &account_id[..4]))
    );
    assert_eq!(audits[0].1["name"], json!("张三的工作室"));
    assert_eq!(audits[1].0, "admin-api");
    assert_eq!(audits[1].1["previous_name"], json!("张三的工作室"));
    assert_eq!(audits[1].1["name"], json!("张三工作室（运营改名）"));

    // 另一个客户改自己的名字，不动前面这位。
    let second = client
        .post(format!("{}/v1/customers", harness.base_url))
        .json(&json!({"email": "lisi@example.com", "password": "a-long-enough-password"}))
        .send()
        .await
        .expect("second register request")
        .json::<Value>()
        .await
        .expect("second register body");
    let second_session = second["token"].as_str().expect("session token").to_owned();
    let renamed = client
        .put(format!("{}/v1/customer/account/name", harness.base_url))
        .bearer_auth(&second_session)
        .json(&json!({"name": "李四的工作室"}))
        .send()
        .await
        .expect("second rename request");
    assert_eq!(renamed.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        account_summary(&client, &harness, &account_id).await["name"],
        json!("张三工作室（运营改名）"),
        "改自己账户的名称不影响别人的账户"
    );

    // 没带会话时改名沿既有未认证行为。
    let anonymous = client
        .put(format!("{}/v1/customer/account/name", harness.base_url))
        .json(&json!({"name": "谁"}))
        .send()
        .await
        .expect("anonymous rename request");
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);

    harness.cleanup().await;
}

/// 绑定已有账户不能同时给名称（绑定不改名）；新账户模式省略名称时按邮箱生成。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn binding_an_existing_account_cannot_set_a_name() {
    let harness = names_harness().await;
    let client = Client::new();

    let (status, created) = create_account(
        &client,
        &harness,
        json!({"initial_credit_microusd": 500_000, "name": "已有的账户"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let account_id = created["account_id"]
        .as_str()
        .expect("account id")
        .to_owned();

    // 绑定 + 名称 = 参数错误，且不留下任何身份。
    let both = client
        .post(format!("{}/api/v1/customers", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .json(&json!({
            "email": "bind@example.com",
            "account_id": account_id,
            "account_name": "想改的名",
        }))
        .send()
        .await
        .expect("open customer request");
    assert_eq!(both.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        account_summary(&client, &harness, &account_id).await["name"],
        json!("已有的账户")
    );

    // 不带名称的绑定成功，账户名称不变。
    let bound = client
        .post(format!("{}/api/v1/customers", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .json(&json!({"email": "bind@example.com", "account_id": account_id}))
        .send()
        .await
        .expect("bind request");
    assert_eq!(bound.status(), StatusCode::CREATED);
    assert_eq!(
        bound.json::<Value>().await.expect("customer view")["account_name"],
        json!("已有的账户"),
        "客户视图带上关联账户名称"
    );

    // 新账户模式：给了名称就用它，省略就按邮箱生成。
    let named = client
        .post(format!("{}/api/v1/customers", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .json(&json!({
            "email": "named@example.com",
            "account_name": "指定名称",
        }))
        .send()
        .await
        .expect("open named request");
    assert_eq!(named.status(), StatusCode::CREATED);
    let named = named.json::<Value>().await.expect("customer view");
    assert_eq!(named["account_name"], json!("指定名称"));

    let generated = client
        .post(format!("{}/api/v1/customers", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .json(&json!({"email": "generated@example.com"}))
        .send()
        .await
        .expect("open generated request")
        .json::<Value>()
        .await
        .expect("customer view");
    let generated_account = generated["account_id"].as_str().expect("account id");
    assert_eq!(
        generated["account_name"],
        json!(format!("generated_{}", &generated_account[..4])),
        "省略名称时按登录邮箱生成：{generated}"
    );

    harness.cleanup().await;
}

/// 重复邮箱与名称撞名是两类冲突：错误码不同，界面才能说对要换的是哪个参数。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_duplicate_email_is_a_conflict_not_a_name_taken() {
    let harness = names_harness().await;
    let client = Client::new();

    let first = client
        .post(format!("{}/v1/customers", harness.base_url))
        .json(&json!({"email": "dup@example.com", "password": "a-long-enough-password"}))
        .send()
        .await
        .expect("register request");
    assert_eq!(first.status(), StatusCode::CREATED);

    let second = client
        .post(format!("{}/v1/customers", harness.base_url))
        .json(&json!({"email": "dup@example.com", "password": "a-long-enough-password"}))
        .send()
        .await
        .expect("second register request");
    assert_eq!(second.status(), StatusCode::CONFLICT);
    let body = second.json::<Value>().await.expect("error body");
    assert_eq!(
        body["error"]["code"],
        json!("conflict"),
        "撞邮箱是 conflict；只有名称撞了才是 name_taken：{body}"
    );

    // 管理端开户撞邮箱同理（这条路径也会走新建账户的候选序列）。
    let opened = client
        .post(format!("{}/api/v1/customers", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .json(&json!({"email": "dup@example.com", "account_name": "想给的名字"}))
        .send()
        .await
        .expect("open customer request");
    assert_eq!(opened.status(), StatusCode::CONFLICT);
    let body = opened.json::<Value>().await.expect("error body");
    assert_eq!(body["error"]["code"], json!("conflict"), "{body}");

    harness.cleanup().await;
}

/// 撞名时错误码是 `name_taken`（客户端据此说"换一个名称"）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_taken_name_reports_the_name_taken_code() {
    let harness = names_harness().await;
    let client = Client::new();

    let (status, _) = create_account(
        &client,
        &harness,
        json!({"initial_credit_microusd": 0, "name": "唯一名称"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = create_account(
        &client,
        &harness,
        json!({"initial_credit_microusd": 0, "name": "唯一名称"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], json!("name_taken"), "{body}");

    harness.cleanup().await;
}

/// API Key 面的账户读不因为客户侧多了名称而改变形状。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_api_key_face_does_not_gain_a_name() {
    let harness = names_harness().await;
    let client = Client::new();

    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 100_000).await;
    let own = client
        .get(format!("{}/v1/account", harness.base_url))
        .bearer_auth(&api_key)
        .send()
        .await
        .expect("api key account request")
        .json::<Value>()
        .await
        .expect("account body");
    assert!(
        own.get("name").is_none(),
        "API Key 面的形状是既有合同，不跟着客户侧加字段：{own}"
    );
    assert!(own.get("balance_microusd").is_some());

    // 同时确认运营侧摘要确实带了名称（两处读分开成两个响应类型的理由就在这）。
    assert!(
        account_summary(&client, &harness, &account_id)
            .await
            .get("name")
            .is_some()
    );

    harness.cleanup().await;
}
