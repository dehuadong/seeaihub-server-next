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
    let matrix: [(&str, String, bool); 24] = [
        ("POST", "/api/v1/accounts".to_owned(), true),
        ("GET", format!("/api/v1/accounts/{ID}"), false),
        ("GET", format!("/api/v1/accounts/{ID}/entries"), false),
        ("PUT", format!("/api/v1/accounts/{ID}/tag"), true),
        ("POST", format!("/api/v1/accounts/{ID}/credits"), true),
        ("POST", format!("/api/v1/accounts/{ID}/api-keys"), true),
        (
            "POST",
            format!("/api/v1/accounts/{ID}/password-reset"),
            false,
        ),
        ("DELETE", format!("/api/v1/api-keys/{ID}"), false),
        ("GET", "/api/v1/customers".to_owned(), false),
        ("POST", "/api/v1/customers".to_owned(), true),
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
