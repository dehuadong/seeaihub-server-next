use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::{Client, StatusCode};
use seeai_application::HubRepository;
use seeai_persistence::PgHubRepository;
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use std::{
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::Duration,
};
use uuid::Uuid;

struct ApiProcess {
    child: Child,
    asset_root: PathBuf,
}

impl Drop for ApiProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.asset_root);
    }
}

/// 一个最小合法 PNG（1×1），用作假上游返回的结果图。
const PNG_FIXTURE: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0xDA, 0x63, 0x64, 0xF8, 0xCF, 0xF0,
    0x1F, 0x00, 0x05, 0xFE, 0x02, 0xFE, 0x5D, 0xC6, 0x38, 0x59, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
];

/// 假上游记录下来的请求（方法、路径、解码后的请求体），用于断言 Driver 的线上请求。
type UpstreamCalls = std::sync::Arc<std::sync::Mutex<Vec<(String, String, String)>>>;

/// APIMart Driver 的真实执行验证（规划 §6 第 13 条等）。
///
/// 用**进程内假上游**替代真实 Provider：发布一个 `base_url` 指向 `127.0.0.1` 的
/// APIMart Offering，让真实 Worker 跑一次完整流程（提交 → 轮询 → 取图 → 归档 → 结算）。
/// **不产生任何外部调用。**
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn apimart_driver_executes_task_flow_against_local_upstream() {
    let (database_url, database_name) = isolated_database_url().await;
    let calls: UpstreamCalls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let upstream = start_fake_upstream(calls.clone()).await;
    let upstream_base = format!("http://127.0.0.1:{}", upstream.port);

    let listener = TcpListener::bind("127.0.0.1:0").expect("test port should bind");
    let port = listener.local_addr().expect("test address").port();
    drop(listener);
    let base_url = format!("http://127.0.0.1:{port}");
    let admin_token = format!("driver-admin-{}", Uuid::new_v4());
    let asset_root = std::env::temp_dir().join(format!("seeai-driver-{}", Uuid::new_v4()));
    let child = Command::new(env!("CARGO_BIN_EXE_seeai-api"))
        .env("DATABASE_URL", &database_url)
        .env("API_BIND", format!("127.0.0.1:{port}"))
        .env("ADMIN_TOKEN", &admin_token)
        .env("ASSET_STORE", "local")
        .env("ASSET_LOCAL_ROOT", &asset_root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("API process should start");
    let _process = ApiProcess {
        child,
        asset_root: asset_root.clone(),
    };
    let client = Client::new();
    wait_until_ready(&client, &base_url).await;
    let account = create_account(&client, &base_url, &admin_token).await;
    let api_key = issue_key(&client, &base_url, &admin_token, &account).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    // 发布一个指向假上游的 APIMart 供给。
    let model = "driver-model";
    let mut draft = candidate("APIMart", "apimart-image-v1", &["prompt_only"]);
    draft["base_url"] = Value::String(upstream_base.clone());
    draft["credential_env"] = Value::String("APIMART_API_KEY".to_owned());
    let published = publish_candidates(&client, &base_url, &admin_token, model, vec![draft]).await;
    assert_eq!(published, StatusCode::OK, "publication must succeed");

    // 受理一个 Job。
    let key = format!("driver-{}", Uuid::new_v4());
    let created = client
        .post(format!("{base_url}/v1/image-generations"))
        .bearer_auth(&api_key)
        .json(&route_request(model, &key, "driver prompt"))
        .send()
        .await
        .expect("generation accepted");
    assert_eq!(created.status(), StatusCode::ACCEPTED);
    let created: Value = created.json().await.expect("job JSON");
    let job_id = Uuid::parse_str(created["job_id"].as_str().expect("job id")).expect("job UUID");

    // 启动真实 Worker（独立包的二进制，按当前 profile 定位）跑一次。
    let mut worker = Command::new(worker_binary())
        .env("DATABASE_URL", &database_url)
        .env("WORKER_ID", "driver-contract-worker")
        .env("WORKER_POLL_INTERVAL_MS", "200")
        .env("WORKER_LEASE_SECONDS", "300")
        .env("PROVIDER_TIMEOUT_SECONDS", "60")
        .env("ASSET_STORE", "local")
        .env("ASSET_LOCAL_ROOT", &asset_root)
        .env("APIMART_API_KEY", "contract-test-key")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("worker process should start");

    // 等 Job 进入终态。
    let mut state = String::new();
    for _ in 0..600 {
        tokio::time::sleep(Duration::from_millis(200)).await;
        state = sqlx::query_scalar("SELECT state FROM generation.jobs WHERE id = $1")
            .bind(job_id)
            .fetch_one(&pool)
            .await
            .expect("job state");
        if matches!(
            state.as_str(),
            "succeeded" | "failed" | "reconciliation_required"
        ) {
            break;
        }
    }
    let _ = worker.kill();
    let _ = worker.wait();
    assert_eq!(state, "succeeded", "the driver flow must settle the job");

    // 计量证据：四分项 usage 落到 attempts.metering_evidence。
    let evidence: Value =
        sqlx::query_scalar("SELECT metering_evidence FROM generation.attempts WHERE job_id = $1")
            .bind(job_id)
            .fetch_one(&pool)
            .await
            .expect("metering evidence");
    assert_eq!(evidence["usage"]["input_text_tokens"], 14);
    assert_eq!(evidence["usage"]["input_image_tokens"], 0);
    assert_eq!(evidence["usage"]["output_image_tokens"], 196);
    assert_eq!(evidence["usage"]["total_tokens"], 210);

    // 结果先归档到自有对象存储，Job 只留资产引用。
    let result_assets: Vec<Uuid> =
        sqlx::query_scalar("SELECT result_asset_ids FROM generation.jobs WHERE id = $1")
            .bind(job_id)
            .fetch_one(&pool)
            .await
            .expect("result assets");
    assert_eq!(result_assets.len(), 1, "one generated image must be stored");
    let (media_type, byte_count): (String, i64) = {
        let row = sqlx::query("SELECT media_type, byte_count FROM generation.assets WHERE id = $1")
            .bind(result_assets[0])
            .fetch_one(&pool)
            .await
            .expect("asset row");
        (
            row.try_get("media_type").expect("media type"),
            row.try_get("byte_count").expect("byte count"),
        )
    };
    assert_eq!(media_type, "image/png");
    assert_eq!(byte_count, PNG_FIXTURE.len() as i64);

    // 对账标识落到**已存在**的 attempts.provider_trace_id 列（规划 §4/§6-13）：
    // 该列此前只有 fail_job 在写，成功路径不写。
    let trace_id: Option<String> =
        sqlx::query_scalar("SELECT provider_trace_id FROM generation.attempts WHERE job_id = $1")
            .bind(job_id)
            .fetch_one(&pool)
            .await
            .expect("attempt trace id");
    assert_eq!(
        trace_id.as_deref(),
        Some("task-contract-1"),
        "the upstream task id must be persisted for manual reconciliation"
    );

    // Driver 的线上请求：只提交一次，且参数在顶层（无 extra 包装）。
    let calls = calls.lock().expect("calls lock").clone();
    let submits = calls
        .iter()
        .filter(|(method, path, _)| method == "POST" && path == "/v1/images/generations")
        .count();
    assert_eq!(submits, 1, "the create request must never be resent");
    let submit_body = calls
        .iter()
        .find(|(method, path, _)| method == "POST" && path == "/v1/images/generations")
        .map(|(_, _, body)| body.clone())
        .expect("submit body");
    let submit_body: Value = serde_json::from_str(&submit_body).expect("submit body is JSON");
    assert_eq!(submit_body["model"], model);
    assert_eq!(submit_body["prompt"], "driver prompt");
    assert!(
        submit_body.get("extra").is_none(),
        "APIMart takes parameters at the top level"
    );
    let polls = calls
        .iter()
        .filter(|(method, path, _)| method == "GET" && path.starts_with("/v1/tasks/"))
        .count();
    assert!(polls >= 1, "the driver must poll the task at least once");
    drop_isolated_database(&database_name).await;
}

struct FakeUpstream {
    port: u16,
    _handle: tokio::task::JoinHandle<()>,
}

async fn start_fake_upstream(calls: UpstreamCalls) -> FakeUpstream {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fake upstream binds");
    let port = listener.local_addr().expect("addr").port();
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let calls = calls.clone();
            tokio::spawn(async move {
                let _ = serve_fake_upstream(&mut socket, calls).await;
            });
        }
    });
    FakeUpstream {
        port,
        _handle: handle,
    }
}

async fn serve_fake_upstream(
    socket: &mut tokio::net::TcpStream,
    calls: UpstreamCalls,
) -> std::io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut buffer = vec![0_u8; 8192];
    let read = socket.read(&mut buffer).await?;
    let request = String::from_utf8_lossy(&buffer[..read]).to_string();
    let request_line = request.lines().next().unwrap_or_default().to_owned();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    let body = request
        .split("\r\n\r\n")
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    if let Ok(mut calls) = calls.lock() {
        calls.push((method.clone(), path.clone(), body));
    }

    let (content_type, payload) = if method == "POST" && path.ends_with("/images/generations") {
        (
            "application/json",
            serde_json::to_vec(&json!({
                "code": 200,
                "data": [{"status": "submitted", "task_id": "task-contract-1"}]
            }))
            .expect("submit body"),
        )
    } else if method == "GET" && path.starts_with("/v1/tasks/") {
        (
            "application/json",
            serde_json::to_vec(&json!({
                "code": 200,
                "data": {
                    "id": "task-contract-1",
                    "status": "completed",
                    "progress": 100,
                    "cost": 0.00476,
                    "credits_cost": 0.0476,
                    "result": {"images": [{"url": [format!("http://127.0.0.1:{}/result.png", port_of(socket))], "expires_at": 4_000_000_000u64}]},
                    "usage": {
                        "input_tokens": 14,
                        "input_tokens_details": {"cached_tokens": 0, "image_tokens": 0, "text_tokens": 14},
                        "output_tokens": 196,
                        "output_tokens_details": {"image_tokens": 196, "text_tokens": 0},
                        "total_tokens": 210
                    }
                }
            }))
            .expect("task body"),
        )
    } else {
        ("image/png", PNG_FIXTURE.to_vec())
    };

    let head = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        payload.len()
    );
    socket.write_all(head.as_bytes()).await?;
    socket.write_all(&payload).await?;
    socket.flush().await?;
    Ok(())
}

fn port_of(socket: &tokio::net::TcpStream) -> u16 {
    socket.local_addr().map(|addr| addr.port()).unwrap_or(0)
}

/// 定位 `seeai-worker` 二进制，**并保证它是当前源码构建的**。
///
/// 它是**独立包**，因此有两件事需要注意：
/// 1. Cargo 不为它提供 `CARGO_BIN_EXE_*`，路径只能从当前测试可执行文件推导；
/// 2. `cargo test -p seeai-api` 只会重建 `seeai-api` 与测试本身，**不会重建 `seeai-worker`**。
///    于是测试可能在验证一个陈旧二进制——这曾真实导致一次误判（新增的
///    `provider_trace_id` 断言失败，原因只是 worker 没重建）。所以在返回路径前**先构建一次**。
fn worker_binary() -> PathBuf {
    // 注意 profile 名的坑：`debug` 是保留名，构建 profile 叫 `dev`，
    // 但它的产物目录是 `target/debug`。
    let profile = if cfg!(debug_assertions) {
        "dev"
    } else {
        "release"
    };
    let status = Command::new(env!("CARGO"))
        .args(["build", "-p", "seeai-worker", "--profile", profile])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("cargo must be runnable from the test");
    assert!(
        status.success(),
        "failed to build seeai-worker for the driver contract test"
    );

    let test_binary = std::env::current_exe().expect("current test executable");
    let profile_dir = test_binary
        .parent()
        .and_then(|deps| deps.parent())
        .expect("target profile directory");
    let candidate = profile_dir.join(if cfg!(windows) {
        "seeai-worker.exe"
    } else {
        "seeai-worker"
    });
    assert!(
        candidate.is_file(),
        "worker binary not found at {}",
        candidate.display()
    );
    candidate
}

/// 为本次测试创建**独立的空库**。
///
/// `HTTP_CONTRACT_DATABASE_URL` 指向一个可连接的空库；多个端到端测试并行时，
/// 它们必须各自有库——驱动测试会启动真实 Worker，而 Worker 会领取数据库里**任何**
/// 可领取的 Job，从而破坏其他测试的人工夹具。因此每个测试从该 URL 派生一个
/// 一次性数据库，结束后自动删除。
async fn isolated_database_url() -> (String, String) {
    let base = std::env::var("HTTP_CONTRACT_DATABASE_URL")
        .expect("HTTP_CONTRACT_DATABASE_URL is required for the ignored contract test");
    let admin = PgPool::connect(&base)
        .await
        .expect("connect to the provided contract database");
    let name = format!("seeai_contract_{}", Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE \"{name}\"")))
        .execute(&admin)
        .await
        .expect("create an isolated contract database");
    admin.close().await;
    // 保留原有查询串的形状，只替换库名。
    let url = match base.rfind('/') {
        Some(index) => format!("{}/{}", &base[..index], name),
        None => panic!("HTTP_CONTRACT_DATABASE_URL must include a database name"),
    };
    (url, name)
}

async fn drop_isolated_database(name: &str) {
    let base = std::env::var("HTTP_CONTRACT_DATABASE_URL").unwrap_or_default();
    let Ok(admin) = PgPool::connect(&base).await else {
        return;
    };
    let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)"
    )))
    .execute(&admin)
    .await;
    admin.close().await;
}

#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn image_generation_http_contract() {
    let (database_url, database_name) = isolated_database_url().await;
    let listener = TcpListener::bind("127.0.0.1:0").expect("test port should bind");
    let port = listener.local_addr().expect("test address").port();
    drop(listener);
    let base_url = format!("http://127.0.0.1:{port}");
    let admin_token = format!("contract-admin-{}", Uuid::new_v4());
    let asset_root = std::env::temp_dir().join(format!("seeai-contract-{}", Uuid::new_v4()));
    let child = Command::new(env!("CARGO_BIN_EXE_seeai-api"))
        .env("DATABASE_URL", &database_url)
        .env("API_BIND", format!("127.0.0.1:{port}"))
        .env("ADMIN_TOKEN", &admin_token)
        .env("ASSET_STORE", "local")
        .env("ASSET_LOCAL_ROOT", &asset_root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("API process should start");
    let _process = ApiProcess { child, asset_root };
    let client = Client::new();
    wait_until_ready(&client, &base_url).await;

    let unauthorized = client
        .post(format!("{base_url}/admin/accounts"))
        .json(&json!({"initial_credit_microusd": 1}))
        .send()
        .await
        .expect("unauthorized request");
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let account = create_account(&client, &base_url, &admin_token).await;
    let api_key = issue_key(&client, &base_url, &admin_token, &account).await;
    publish_bootstrap(&client, &base_url, &admin_token).await;
    reject_mismatched_model_identity(&client, &base_url, &admin_token).await;

    let png = STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M/wHwAF/gL+XcY4WQAAAABJRU5ErkJggg==")
        .expect("PNG fixture");
    let asset_response = client
        .post(format!("{base_url}/v1/assets"))
        .bearer_auth(&api_key)
        .header("content-type", "image/png")
        .header("x-asset-role", "image")
        .body(png)
        .send()
        .await
        .expect("asset upload");
    assert_eq!(asset_response.status(), StatusCode::CREATED);
    let asset: Value = asset_response.json().await.expect("asset JSON");
    assert_eq!(asset["width"], 1);
    assert_eq!(asset["height"], 1);

    let request_key = format!("contract-{}", Uuid::new_v4());
    let request = generation_request(&request_key, "contract prompt");
    let first = client
        .post(format!("{base_url}/v1/image-generations"))
        .bearer_auth(&api_key)
        .json(&request)
        .send()
        .await
        .expect("first generation");
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    let first: Value = first.json().await.expect("first job JSON");
    let repeated: Value = client
        .post(format!("{base_url}/v1/image-generations"))
        .bearer_auth(&api_key)
        .json(&request)
        .send()
        .await
        .expect("repeated generation")
        .json()
        .await
        .expect("repeated job JSON");
    assert_eq!(first["job_id"], repeated["job_id"]);
    assert_eq!(
        first.as_object().expect("response object").len(),
        4,
        "creation response must not leak runtime/provider fields"
    );

    let conflict = client
        .post(format!("{base_url}/v1/image-generations"))
        .bearer_auth(&api_key)
        .json(&generation_request(&request_key, "changed prompt"))
        .send()
        .await
        .expect("conflicting generation");
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    let job_id = first["job_id"].as_str().expect("job id");
    let job = client
        .get(format!("{base_url}/v1/image-generations/{job_id}"))
        .bearer_auth(&api_key)
        .send()
        .await
        .expect("job query");
    assert_eq!(job.status(), StatusCode::OK);

    let other_account = create_account(&client, &base_url, &admin_token).await;
    let other_key = issue_key(&client, &base_url, &admin_token, &other_account).await;
    let foreign_asset = client
        .get(format!(
            "{base_url}/v1/assets/{}",
            asset["id"].as_str().expect("asset id")
        ))
        .bearer_auth(other_key)
        .send()
        .await
        .expect("foreign asset query");
    assert_eq!(foreign_asset.status(), StatusCode::NOT_FOUND);

    verify_reconciliation_contract(&client, &base_url, &admin_token, &api_key, &database_url).await;
    verify_lease_recovery_contract(&client, &base_url, &api_key, &database_url).await;
    drop_isolated_database(&database_name).await;
}

/// 多 Offering 路由的端到端验证（规划 §6 第 1–4、7 条）。
///
/// 需要独立空库（会发布自己的候选集合）。**不启动 Worker**：路由选择发生在
/// `create_job` 之前的 API 进程内，而 `create_job` 不调用上游——因此本测试
/// **不产生任何外部调用**，同时仍能验证选中顺序与"无合格候选时零上游调用"。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn multiple_active_offerings_route_by_priority() {
    let (database_url, database_name) = isolated_database_url().await;
    let listener = TcpListener::bind("127.0.0.1:0").expect("test port should bind");
    let port = listener.local_addr().expect("test address").port();
    drop(listener);
    let base_url = format!("http://127.0.0.1:{port}");
    let admin_token = format!("route-admin-{}", Uuid::new_v4());
    let asset_root = std::env::temp_dir().join(format!("seeai-route-{}", Uuid::new_v4()));
    let child = Command::new(env!("CARGO_BIN_EXE_seeai-api"))
        .env("DATABASE_URL", &database_url)
        .env("API_BIND", format!("127.0.0.1:{port}"))
        .env("ADMIN_TOKEN", &admin_token)
        .env("ASSET_STORE", "local")
        .env("ASSET_LOCAL_ROOT", &asset_root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("API process should start");
    let _process = ApiProcess { child, asset_root };
    let client = Client::new();
    wait_until_ready(&client, &base_url).await;
    let account = create_account(&client, &base_url, &admin_token).await;
    let api_key = issue_key(&client, &base_url, &admin_token, &account).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    // ── 用例 1：两个都合格的候选 → 选中优先级最小的那个（第 1、2、3 条）──
    let model = "route-model-a";
    let published = publish_candidates(
        &client,
        &base_url,
        &admin_token,
        model,
        vec![
            candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
            candidate("APIMart", "apimart-image-v1", &["prompt_only"]),
        ],
    )
    .await;
    assert_eq!(
        published,
        StatusCode::OK,
        "multi-offering publish must succeed"
    );

    let key = format!("route-{}", Uuid::new_v4());
    let created = client
        .post(format!("{base_url}/v1/image-generations"))
        .bearer_auth(&api_key)
        .json(&route_request(model, &key, "route prompt"))
        .send()
        .await
        .expect("routed generation");
    assert_eq!(created.status(), StatusCode::ACCEPTED);
    let created: Value = created.json().await.expect("job JSON");
    let job_id = Uuid::parse_str(created["job_id"].as_str().expect("job id")).expect("job UUID");

    // 判定记录与 Job 同事务写入。
    let (chosen, considered): (Uuid, Value) = {
        let row = sqlx::query(
            "SELECT chosen_offering_id, considered FROM generation.routing_decisions WHERE job_id = $1",
        )
        .bind(job_id)
        .fetch_one(&pool)
        .await
        .expect("routing decision row must exist");
        (
            row.try_get("chosen_offering_id").expect("chosen"),
            row.try_get("considered").expect("considered"),
        )
    };
    // `considered` 记录了两个候选各自的 priority 与 eligible。
    let considered = considered.as_array().expect("considered is an array");
    assert_eq!(considered.len(), 2, "both candidates must be considered");
    assert_eq!(considered[0]["routing_priority"], 0);
    assert_eq!(considered[0]["provider_kind"], "AIHubMix");
    assert_eq!(considered[0]["eligible"], true);
    // 选中项就是优先级 0 的那个候选。
    let expected: Uuid = sqlx::query_scalar(
        "SELECT re.offering_id FROM publication.runtime_entries re
         WHERE re.active AND re.native_model_id = $1 AND re.routing_priority = 0",
    )
    .bind(model)
    .fetch_one(&pool)
    .await
    .expect("priority 0 offering");
    assert_eq!(
        chosen, expected,
        "the first eligible candidate must be chosen"
    );
    // Job 固化了被选中的 Offering 与 Channel。
    let (job_offering, job_channel): (Uuid, Uuid) = {
        let row = sqlx::query("SELECT offering_id, channel_id FROM generation.jobs WHERE id = $1")
            .bind(job_id)
            .fetch_one(&pool)
            .await
            .expect("job row");
        (
            row.try_get("offering_id").expect("offering"),
            row.try_get("channel_id").expect("channel"),
        )
    };
    assert_eq!(job_offering, chosen);
    let chosen_channel: Uuid =
        sqlx::query_scalar("SELECT channel_id FROM supply.offerings WHERE id = $1")
            .bind(chosen)
            .fetch_one(&pool)
            .await
            .expect("chosen offering channel");
    assert_eq!(job_channel, chosen_channel);

    // ── 用例 3：再发布一次即原子替换该型号的全部 active 候选（第 1 条）──
    let published = publish_candidates(
        &client,
        &base_url,
        &admin_token,
        model,
        vec![candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"])],
    )
    .await;
    assert_eq!(published, StatusCode::OK);
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM publication.runtime_entries WHERE active AND native_model_id = $1",
    )
    .bind(model)
    .fetch_one(&pool)
    .await
    .expect("active entries");
    assert_eq!(
        active, 1,
        "republishing must atomically replace the model's active candidates"
    );
    drop_isolated_database(&database_name).await;
}

fn candidate(provider_kind: &str, adapter_key: &str, branches: &[&str]) -> Value {
    json!({
        "provider_kind": provider_kind,
        "adapter_key": adapter_key,
        "provider_model_id": "route-model",
        "base_url": "http://127.0.0.1:1",
        "credential_env": "AIHUBMIX_API_KEY",
        "restrictions": {
            "allowed_branches": branches,
            "max_images": 1
        },
        "capability_schema": {
            "type": "object",
            "additionalProperties": false,
            "required": ["model", "prompt"],
            "properties": {
                "model": {"const": "placeholder"},
                "prompt": {"type": "string", "minLength": 1}
            }
        },
        "price_plan": {
            "formula": "token_rates",
            "currency": "USD",
            "text_input_microusd_per_million": 5_000_000,
            "image_input_microusd_per_million": 8_000_000,
            "text_output_microusd_per_million": 10_000_000,
            "image_output_microusd_per_million": 30_000_000,
            "source_url": "https://example.invalid/price"
        }
    })
}

async fn publish_candidates(
    client: &Client,
    base_url: &str,
    admin_token: &str,
    model: &str,
    mut offerings: Vec<Value>,
) -> StatusCode {
    for offering in &mut offerings {
        offering["capability_schema"]["properties"]["model"]["const"] =
            Value::String(model.to_owned());
        offering["provider_model_id"] = Value::String(model.to_owned());
    }
    let body = json!({
        "vendor_id": "OpenAI",
        "native_model_id": model,
        "native_revision": "route-test-1",
        "actor": "contract-test",
        "offerings": offerings
    });
    client
        .post(format!("{base_url}/admin/runtime-revisions"))
        .bearer_auth(admin_token)
        .json(&body)
        .send()
        .await
        .expect("runtime publication")
        .status()
}

fn route_request(native_model_id: &str, idempotency_key: &str, prompt: &str) -> Value {
    json!({
        "native_model_id": native_model_id,
        "native_parameters": {"prompt": prompt},
        "asset_bindings": [],
        "idempotency_key": idempotency_key,
        "max_cost_microusd": 20_000
    })
}

async fn wait_until_ready(client: &Client, base_url: &str) {
    for _ in 0..100 {
        if client
            .get(format!("{base_url}/health"))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("API did not become ready");
}

async fn create_account(client: &Client, base_url: &str, admin_token: &str) -> String {
    let response = client
        .post(format!("{base_url}/admin/accounts"))
        .bearer_auth(admin_token)
        .json(&json!({"initial_credit_microusd": 100_000}))
        .send()
        .await
        .expect("account creation");
    assert_eq!(response.status(), StatusCode::OK);
    response.json::<Value>().await.expect("account JSON")["account_id"]
        .as_str()
        .expect("account id")
        .to_owned()
}

async fn issue_key(client: &Client, base_url: &str, admin_token: &str, account_id: &str) -> String {
    client
        .post(format!("{base_url}/admin/accounts/{account_id}/api-keys"))
        .bearer_auth(admin_token)
        .json(&json!({"label": "contract"}))
        .send()
        .await
        .expect("key creation")
        .json::<Value>()
        .await
        .expect("key JSON")["api_key"]
        .as_str()
        .expect("API key")
        .to_owned()
}

async fn publish_bootstrap(client: &Client, base_url: &str, admin_token: &str) {
    let config: Value = serde_json::from_str(include_str!(
        "../../../config/bootstrap/aihubmix-gpt-image-2.json"
    ))
    .expect("bootstrap config");
    let response = client
        .post(format!("{base_url}/admin/runtime-revisions"))
        .bearer_auth(admin_token)
        .json(&config)
        .send()
        .await
        .expect("runtime publication");
    assert_eq!(response.status(), StatusCode::OK);
}

async fn reject_mismatched_model_identity(client: &Client, base_url: &str, admin_token: &str) {
    let mut config: Value = serde_json::from_str(include_str!(
        "../../../config/bootstrap/aihubmix-gpt-image-2.json"
    ))
    .expect("bootstrap config");
    config["native_model_id"] = Value::String("different-model".to_owned());
    let response = client
        .post(format!("{base_url}/admin/runtime-revisions"))
        .bearer_auth(admin_token)
        .json(&config)
        .send()
        .await
        .expect("mismatched publication");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

fn generation_request(idempotency_key: &str, prompt: &str) -> Value {
    json!({
        "native_model_id": "gpt-image-2",
        "native_parameters": {"prompt": prompt, "n": 1, "extra": {"quality": "low"}},
        "asset_bindings": [],
        "idempotency_key": idempotency_key,
        "max_cost_microusd": 20_000
    })
}

async fn verify_reconciliation_contract(
    client: &Client,
    base_url: &str,
    admin_token: &str,
    api_key: &str,
    database_url: &str,
) {
    let key = format!("reconciliation-{}", Uuid::new_v4());
    let job: Value = client
        .post(format!("{base_url}/v1/image-generations"))
        .bearer_auth(api_key)
        .json(&generation_request(&key, "reconciliation contract"))
        .send()
        .await
        .expect("reconciliation job")
        .json()
        .await
        .expect("reconciliation job JSON");
    let job_id = Uuid::parse_str(job["job_id"].as_str().expect("job id")).expect("job UUID");
    let attempt_id = Uuid::new_v4();
    let case_id = Uuid::new_v4();
    let pool = PgPool::connect(database_url)
        .await
        .expect("contract database");
    let balance_after_hold: i64 = sqlx::query_scalar(
        "SELECT a.balance_microusd FROM ledger.accounts a JOIN generation.jobs j ON j.account_id = a.id WHERE j.id = $1",
    )
    .bind(job_id)
    .fetch_one(&pool)
    .await
    .expect("balance after hold");
    sqlx::query("UPDATE generation.jobs SET state = 'reconciliation_required' WHERE id = $1")
        .bind(job_id)
        .execute(&pool)
        .await
        .expect("job reconciliation state");
    sqlx::query(
        "INSERT INTO generation.attempts (id, job_id, state, request_digest) VALUES ($1,$2,'reconciliation_required','contract')",
    )
    .bind(attempt_id)
    .bind(job_id)
    .execute(&pool)
    .await
    .expect("attempt fixture");
    sqlx::query(
        "INSERT INTO operations.reconciliation_cases (id, job_id, attempt_id, reason) VALUES ($1,$2,$3,'contract')",
    )
    .bind(case_id)
    .bind(job_id)
    .bind(attempt_id)
    .execute(&pool)
    .await
    .expect("case fixture");

    let cases: Value = client
        .get(format!("{base_url}/admin/reconciliation-cases"))
        .bearer_auth(admin_token)
        .send()
        .await
        .expect("case list")
        .json()
        .await
        .expect("case list JSON");
    assert!(
        cases
            .as_array()
            .expect("case array")
            .iter()
            .any(|case| { case["job_id"].as_str() == Some(job_id.to_string().as_str()) })
    );
    let legacy_charge = json!({
        "resolution": "charge",
        "charge_microusd": 1,
        "note": "must not settle without evidence",
        "business_key": format!("contract-charge-{job_id}")
    });
    let response = client
        .post(format!(
            "{base_url}/admin/reconciliation-cases/{job_id}/refund"
        ))
        .bearer_auth(admin_token)
        .json(&legacy_charge)
        .send()
        .await
        .expect("legacy charge rejection");
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let refund = json!({
        "note": "contract refund",
        "business_key": format!("contract-refund-{job_id}")
    });
    for _ in 0..2 {
        let response = client
            .post(format!(
                "{base_url}/admin/reconciliation-cases/{job_id}/refund"
            ))
            .bearer_auth(admin_token)
            .json(&refund)
            .send()
            .await
            .expect("case resolution");
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
    let row = sqlx::query(
        "SELECT j.state, h.status, h.amount_microusd, a.balance_microusd FROM generation.jobs j JOIN ledger.holds h ON h.job_id = j.id JOIN ledger.accounts a ON a.id = j.account_id WHERE j.id = $1",
    )
    .bind(job_id)
    .fetch_one(&pool)
    .await
    .expect("resolved state");
    assert_eq!(row.get::<String, _>("state"), "failed");
    assert_eq!(row.get::<String, _>("status"), "released");
    assert_eq!(
        row.get::<i64, _>("balance_microusd"),
        balance_after_hold + row.get::<i64, _>("amount_microusd")
    );
    let capture_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ledger.entries WHERE job_id = $1 AND kind = 'capture'",
    )
    .bind(job_id)
    .fetch_one(&pool)
    .await
    .expect("capture count");
    assert_eq!(capture_count, 0);
    let removed_charge_columns: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.columns WHERE table_schema = 'operations' AND table_name = 'reconciliation_cases' AND column_name IN ('resolution', 'charge_microusd')",
    )
    .fetch_one(&pool)
    .await
    .expect("reconciliation schema");
    assert_eq!(removed_charge_columns, 0);
}

async fn verify_lease_recovery_contract(
    client: &Client,
    base_url: &str,
    api_key: &str,
    database_url: &str,
) {
    let leased_job: Value = client
        .post(format!("{base_url}/v1/image-generations"))
        .bearer_auth(api_key)
        .json(&generation_request(
            &format!("lease-before-submit-{}", Uuid::new_v4()),
            "lease recovery before submit",
        ))
        .send()
        .await
        .expect("leased recovery job")
        .json()
        .await
        .expect("leased recovery job JSON");
    let leased_job_id = Uuid::parse_str(leased_job["job_id"].as_str().expect("leased job id"))
        .expect("leased job UUID");
    let submitted_job: Value = client
        .post(format!("{base_url}/v1/image-generations"))
        .bearer_auth(api_key)
        .json(&generation_request(
            &format!("lease-after-submit-{}", Uuid::new_v4()),
            "lease recovery after submit",
        ))
        .send()
        .await
        .expect("submitted recovery job")
        .json()
        .await
        .expect("submitted recovery job JSON");
    let submitted_job_id =
        Uuid::parse_str(submitted_job["job_id"].as_str().expect("submitted job id"))
            .expect("submitted job UUID");
    let submitted_attempt_id = Uuid::new_v4();
    let pool = PgPool::connect(database_url)
        .await
        .expect("contract database");
    sqlx::query(
        "UPDATE generation.jobs SET state = 'leased', lease_owner = 'expired-worker', lease_expires_at = now() - interval '1 second' WHERE id = $1",
    )
    .bind(leased_job_id)
    .execute(&pool)
    .await
    .expect("expired leased fixture");
    sqlx::query(
        "UPDATE generation.jobs SET state = 'submitting', lease_owner = 'expired-worker', lease_expires_at = now() - interval '1 second' WHERE id = $1",
    )
    .bind(submitted_job_id)
    .execute(&pool)
    .await
    .expect("expired submission fixture");
    sqlx::query(
        "INSERT INTO generation.attempts (id, job_id, state, request_digest) VALUES ($1,$2,'submitting','lease-contract')",
    )
    .bind(submitted_attempt_id)
    .bind(submitted_job_id)
    .execute(&pool)
    .await
    .expect("submitted attempt fixture");

    let repository = PgHubRepository::connect(database_url, 2)
        .await
        .expect("recovery repository");
    let recovered = repository
        .recover_expired_leases()
        .await
        .expect("lease recovery");
    assert_eq!(recovered.returned_to_queue, 1);
    assert_eq!(recovered.sent_to_reconciliation, 1);
    let leased_state: String =
        sqlx::query_scalar("SELECT state FROM generation.jobs WHERE id = $1")
            .bind(leased_job_id)
            .fetch_one(&pool)
            .await
            .expect("leased recovery state");
    assert_eq!(leased_state, "accepted");
    let submitted_state: String =
        sqlx::query_scalar("SELECT state FROM generation.jobs WHERE id = $1")
            .bind(submitted_job_id)
            .fetch_one(&pool)
            .await
            .expect("submitted recovery state");
    assert_eq!(submitted_state, "reconciliation_required");
    let case_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM operations.reconciliation_cases WHERE job_id = $1",
    )
    .bind(submitted_job_id)
    .fetch_one(&pool)
    .await
    .expect("recovery case count");
    assert_eq!(case_count, 1);

    let repeated = repository
        .recover_expired_leases()
        .await
        .expect("repeated lease recovery");
    assert_eq!(repeated.returned_to_queue, 0);
    assert_eq!(repeated.sent_to_reconciliation, 0);
}
