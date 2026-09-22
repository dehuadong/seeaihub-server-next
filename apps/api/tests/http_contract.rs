//! 对客面与内部流水线的端到端合同测试。
//!
//! 两条对客路径（`/v1/images/generations` 与 `/v1/images/edits`）都是**同步**的：
//! 受理之后等内部 Job 跑到终态，再把渠道给的图片原样交回去。内部 Job / Attempt / Worker /
//! 计量证据 / 对账仍是执行单位，但**不投射成对客的异步任务协议**：对客没有 job_id、
//! 没有 202、没有可查询的任务接口。
//!
//! 用**进程内假上游**替代真实 Provider，**不产生任何外部调用**。

use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::{Client, StatusCode};
use seeai_application::{
    AttemptFailure, HoldDisposition, HubRepository, ProviderFailureKind, PublicErrorCode,
};
use seeai_domain::{
    AccountId, AttemptId, JobId, ProviderCostFact, ProviderCostSource,
    replace_contract_model_identity,
};
use seeai_persistence::PgHubRepository;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use std::{
    collections::BTreeMap,
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;

/// 一个最小合法 PNG（1×1），用作假上游返回的结果图，也用作调用方传的参考图。
const PNG_FIXTURE: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0xDA, 0x63, 0x64, 0xF8, 0xCF, 0xF0,
    0x1F, 0x00, 0x05, 0xFE, 0x02, 0xFE, 0x5D, 0xC6, 0x38, 0x59, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
];

/// 平台上没有对象存储也没有资产接口，图片只以 data URL 的形态在请求里出现。
fn png_data_url() -> String {
    format!("data:image/png;base64,{}", STANDARD.encode(PNG_FIXTURE))
}

/// 假上游记录下来的请求（方法、路径、原始请求体），用于断言 Driver 的线上请求。
#[derive(Debug, Clone)]
struct UpstreamCall {
    method: String,
    path: String,
    body: Vec<u8>,
}

type UpstreamCalls = Arc<Mutex<Vec<UpstreamCall>>>;

/// 假上游扮演哪个渠道：两家的线上形状不同（一个任务式、一个同步返回图片）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderShape {
    Apimart,
    Aihubmix,
}

/// 同步渠道（AIHubMix）成功响应里图片的形态：两种都要能造。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyncImageShape {
    Url,
    Base64,
}

/// 提交生成请求时上游的应答方式。
#[derive(Clone)]
enum SubmitBehaviour {
    /// 受理成功并返回任务 id。
    Accepted,
    /// 直接以这个状态码与错误体拒绝：欠费、凭证、参数错误、限流等。
    Rejected { status: u16, body: Value },
}

/// 假上游的可配置行为：覆盖重试、未知状态、在飞、上传失败与提交被拒。
#[derive(Clone)]
struct UpstreamBehaviour {
    provider: ProviderShape,
    /// 任务查询先失败这么多次（返回 500），之后才给正常响应 —— 覆盖"查询可重试"。
    query_failures: usize,
    /// 任务查询先返回这么多次未在文档中出现的状态，之后才 completed。
    unknown_status_times: usize,
    /// 任务查询先返回这么多次"仍在跑"，用来把一次请求留在在飞状态。
    pending_times: usize,
    /// 非 0 时，上传接口固定返回这个错误状态码 —— 覆盖"上传失败即确定未受理"。
    upload_failure_status: u16,
    submit: SubmitBehaviour,
    sync_image: SyncImageShape,
    /// 任务终态里声明的成本：`None` 表示响应里**根本没有这个字段**（渠道没给），
    /// 负数与非数字则覆盖"声明了却拿不到"的形态。取值是实测样例。
    declared_cost: Option<Value>,
}

impl UpstreamBehaviour {
    fn apimart() -> Self {
        Self {
            provider: ProviderShape::Apimart,
            query_failures: 0,
            unknown_status_times: 0,
            pending_times: 0,
            upload_failure_status: 0,
            submit: SubmitBehaviour::Accepted,
            sync_image: SyncImageShape::Url,
            declared_cost: Some(json!(0.011354)),
        }
    }

    fn aihubmix(sync_image: SyncImageShape) -> Self {
        Self {
            provider: ProviderShape::Aihubmix,
            sync_image,
            ..Self::apimart()
        }
    }
}

struct FakeUpstream {
    base_url: String,
    _handle: tokio::task::JoinHandle<()>,
}

async fn start_fake_upstream_with(
    calls: UpstreamCalls,
    behaviour: UpstreamBehaviour,
) -> FakeUpstream {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fake upstream binds");
    let port = listener.local_addr().expect("addr").port();
    // 查询与上传的行为按调用次数推进。
    let query_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let upload_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let calls = calls.clone();
            let behaviour = behaviour.clone();
            let query_count = query_count.clone();
            let upload_count = upload_count.clone();
            tokio::spawn(async move {
                let _ =
                    serve_fake_upstream(&mut socket, calls, behaviour, query_count, upload_count)
                        .await;
            });
        }
    });
    FakeUpstream {
        base_url: format!("http://127.0.0.1:{port}"),
        _handle: handle,
    }
}

async fn serve_fake_upstream(
    socket: &mut tokio::net::TcpStream,
    calls: UpstreamCalls,
    behaviour: UpstreamBehaviour,
    query_count: Arc<std::sync::atomic::AtomicUsize>,
    upload_count: Arc<std::sync::atomic::AtomicUsize>,
) -> std::io::Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

    // 正经读完一个请求：请求行 + 头 + 按 Content-Length 读满请求体。
    let (method, path, body) = {
        let mut reader = BufReader::new(&mut *socket);
        let mut request_line = String::new();
        reader.read_line(&mut request_line).await?;
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or_default().to_owned();
        let path = parts.next().unwrap_or_default().to_owned();

        let mut content_length = 0_usize;
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).await? == 0 {
                break;
            }
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some((name, value)) = header.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0_u8; content_length];
        if content_length > 0 {
            reader.read_exact(&mut body).await?;
        }
        (method, path, body)
    };
    if let Ok(mut calls) = calls.lock() {
        calls.push(UpstreamCall {
            method: method.clone(),
            path: path.clone(),
            body,
        });
    }
    let port = port_of(socket);

    // 上传：内联图片换公网 URL。这个分支必须在生成分支之前判断，
    // 而且它的失败**不**代表"生成可能已发生"——生成任务此时还没提交。
    if method == "POST" && path == "/v1/uploads/images" {
        let index = upload_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        if behaviour.upload_failure_status != 0 {
            // 上游上传失败的错误体只有 type 与 message，**没有** error.code。
            let payload = serde_json::to_vec(&json!({
                "error": {"type": "invalid_request_error", "message": "unsupported image type"}
            }))
            .expect("upload failure body");
            return write_response(
                socket,
                behaviour.upload_failure_status,
                "Error",
                "application/json",
                &payload,
            )
            .await;
        }
        let payload = serde_json::to_vec(&json!({
            "url": format!("http://127.0.0.1:{port}/uploaded-{index}.png"),
            "filename": format!("image-{index}.png"),
            "content_type": "image/png",
            "bytes": PNG_FIXTURE.len(),
            "created_at": 1_790_000_000u64
        }))
        .expect("upload body");
        return write_response(socket, 200, "OK", "application/json", &payload).await;
    }

    // 提交生成请求被上游直接拒：欠费、凭证、参数错误、限流都从这里进。
    if method == "POST"
        && (path.ends_with("/images/generations") || path.ends_with("/images/edits"))
        && let SubmitBehaviour::Rejected { status, body } = &behaviour.submit
    {
        let payload = serde_json::to_vec(body).expect("rejection body");
        return write_response(socket, *status, "Error", "application/json", &payload).await;
    }

    // 同步渠道（AIHubMix）：`/v1/images/generations` 与 `/v1/images/edits` 都在同一个响应里
    // 直接给图片与计量。
    if behaviour.provider == ProviderShape::Aihubmix
        && method == "POST"
        && (path.ends_with("/images/generations") || path.ends_with("/images/edits"))
    {
        let payload =
            serde_json::to_vec(&sync_image_payload(behaviour.sync_image, port)).expect("sync body");
        return write_response(socket, 200, "OK", "application/json", &payload).await;
    }

    // 任务式渠道（APIMart）：提交拿 task_id，轮询到终态。
    if method == "POST" && path.ends_with("/images/generations") {
        let payload = serde_json::to_vec(&json!({
            "code": 200,
            "data": [{"status": "submitted", "task_id": "task-contract-1"}]
        }))
        .expect("submit body");
        return write_response(socket, 200, "OK", "application/json", &payload).await;
    }
    if method == "GET" && path.starts_with("/v1/tasks/") {
        let attempt = query_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        if attempt <= behaviour.query_failures {
            let payload = serde_json::to_vec(&json!({
                "error": {"code": 500, "message": "temporary upstream failure"}
            }))
            .expect("failure body");
            return write_response(
                socket,
                500,
                "Internal Server Error",
                "application/json",
                &payload,
            )
            .await;
        }
        let status = if attempt <= behaviour.query_failures + behaviour.unknown_status_times {
            // 未在文档中出现的状态值：Driver 必须继续轮询，不得当失败。
            "queued_somewhere_new"
        } else if attempt
            <= behaviour.query_failures + behaviour.unknown_status_times + behaviour.pending_times
        {
            "processing"
        } else {
            "completed"
        };
        let mut task = json!({
            "code": 200,
            "data": {
                "id": "task-contract-1",
                "status": status,
                "progress": 100,
                "result": {"images": [{"url": [format!("http://127.0.0.1:{port}/result.png")], "expires_at": 4_000_000_000u64}]},
                "usage": {
                    "input_tokens": 14,
                    "input_tokens_details": {"cached_tokens": 0, "image_tokens": 0, "text_tokens": 14},
                    "output_tokens": 196,
                    "output_tokens_details": {"image_tokens": 196, "text_tokens": 0},
                    "total_tokens": 210
                }
            }
        });
        // 成本字段按用例配置给：`None` 就是响应里**没有它**。
        if let Some(cost) = &behaviour.declared_cost {
            task["data"]["cost"] = cost.clone();
            task["data"]["credits_cost"] = json!(0.0476);
        }
        let payload = serde_json::to_vec(&task).expect("task body");
        return write_response(socket, 200, "OK", "application/json", &payload).await;
    }

    // 其余 GET：把 PNG 交出去。参考图走公网 URL 时由 Adapter 自己来取；
    // 结果 URL 则**不该**被平台来取（平台不下载结果）。
    if method == "GET" {
        return write_response(socket, 200, "OK", "image/png", PNG_FIXTURE).await;
    }

    write_response(
        socket,
        404,
        "Not Found",
        "application/json",
        b"{\"error\":{\"message\":\"not found\"}}",
    )
    .await
}

/// 同步渠道的成功响应：`data[0]` 是要么 `url`、要么 `b64_json`，外加计量证据。
fn sync_image_payload(shape: SyncImageShape, port: u16) -> Value {
    let image = match shape {
        SyncImageShape::Url => json!({"url": format!("http://127.0.0.1:{port}/result.png")}),
        SyncImageShape::Base64 => json!({"b64_json": STANDARD.encode(PNG_FIXTURE)}),
    };
    json!({
        "created": 1_790_000_000u64,
        "data": [image],
        "usage": {
            "input_tokens": 14,
            "input_tokens_details": {"cached_tokens": 0, "image_tokens": 0, "text_tokens": 14},
            "output_tokens": 196,
            "output_tokens_details": {"image_tokens": 196, "text_tokens": 0},
            "total_tokens": 210
        }
    })
}

async fn write_response(
    socket: &mut tokio::net::TcpStream,
    status: u16,
    reason: &str,
    content_type: &str,
    payload: &[u8],
) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        payload.len()
    );
    socket.write_all(head.as_bytes()).await?;
    socket.write_all(payload).await?;
    socket.flush().await
}

fn port_of(socket: &tokio::net::TcpStream) -> u16 {
    socket.local_addr().map(|addr| addr.port()).unwrap_or(0)
}

fn body_contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

/// 表单字节里某个**完整部件名**出现的次数。
///
/// 名字连引号一起比：`name="image"` 要求 `image` 后面紧跟着引号，所以列表形态的
/// `name="image[]"` 不会被算成单值 `image`——这正是"多张时不许退回单值"要判的事。
fn part_name_count(rendered: &str, name: &str) -> usize {
    rendered.matches(&format!("name=\"{name}\"")).count()
}

/// 定位 `seeai-worker` 二进制，**并保证它是当前源码构建的**。
///
/// 它是**独立包**，因此有两件事需要注意：
/// 1. Cargo 不为它提供 `CARGO_BIN_EXE_*`，路径只能从当前测试可执行文件推导；
/// 2. `cargo test -p seeai-api` 只会重建 `seeai-api` 与测试本身，**不会重建 `seeai-worker`**。
///    于是测试可能在验证一个陈旧二进制——这曾真实导致一次误判。所以在返回路径前**先构建一次**。
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

struct ApiProcess {
    child: Child,
}

impl Drop for ApiProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// 起一个平台 API 进程，并落下**测试夹具**要用的折算率。
///
/// 汇率是**外部事实**、由管理员在后台录入：发布一个候选时，它声明的成本币种必须已有一行
/// 生效的折算率，否则发布期就拒（这正是设计要的行为——受理时取不到汇率就算不出成本，而那时
/// 拒的是消费者的请求）。所以每个用例在发布之前先有这两行。
///
/// 这两行是**夹具**，不是生产默认值：`USD` 的数值只在本用例里成立，`CNY` 那一行是 1:1
/// （同币种折算按定义就是 1）。要验"没有折算率的币种发布被拒"的用例用一个**没被种下**的币种。
async fn start_api(
    database_url: &str,
    sync_wait_seconds: u64,
    max_concurrent_jobs: u64,
) -> (String, String, ApiProcess) {
    start_api_with(database_url, sync_wait_seconds, max_concurrent_jobs, None).await
}

/// 同 [`start_api`]，但可以给这个进程配上**加速层**（缓存）。
///
/// 不配就是今天的路径：加速层根本不构造，受理不额外读库、不预检、不写缓存。
async fn start_api_with(
    database_url: &str,
    sync_wait_seconds: u64,
    max_concurrent_jobs: u64,
    cache: Option<&CacheFixture>,
) -> (String, String, ApiProcess) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("test port should bind");
    let port = listener.local_addr().expect("test address").port();
    drop(listener);
    let base_url = format!("http://127.0.0.1:{port}");
    let admin_token = format!("contract-admin-{}", Uuid::new_v4());
    let mut command = Command::new(env!("CARGO_BIN_EXE_seeai-api"));
    command
        .env("DATABASE_URL", database_url)
        .env("API_BIND", format!("127.0.0.1:{port}"))
        .env("ADMIN_TOKEN", &admin_token)
        .env(
            "GENERATION_MAX_CONCURRENT_JOBS",
            max_concurrent_jobs.to_string(),
        )
        .env(
            "GENERATION_SYNC_WAIT_SECONDS",
            sync_wait_seconds.to_string(),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    apply_cache_env(&mut command, cache);
    let child = command.spawn().expect("API process should start");
    let client = Client::new();
    wait_until_ready(&client, &base_url).await;
    for (currency, rate_micros) in [("USD", 7_100_000_u64), ("CNY", 1_000_000_u64)] {
        let response = client
            .put(format!("{base_url}/api/v1/fx-rates"))
            .bearer_auth(&admin_token)
            .json(&json!({"currency": currency, "rate_micros": rate_micros}))
            .send()
            .await
            .expect("fx rate fixture request");
        assert_eq!(
            response.status(),
            StatusCode::NO_CONTENT,
            "fixture fx rate for {currency} must be recorded"
        );
    }
    (base_url, admin_token, ApiProcess { child })
}

/// 起一个真实 Worker 进程（丢弃返回值即结束它）。
///
/// 环境变量只有一份，两个调用点（[`Harness`] 与直接起进程的用例）共用：两家渠道的凭证都写在
/// 测试进程的环境里，取值只在进程内假上游上用过，不写入配置、日志或响应。
fn spawn_worker_process(database_url: &str) -> WorkerProcess {
    spawn_worker_process_with(database_url, None)
}

/// 同 [`spawn_worker_process`]，但可以给 Worker 也配上加速层：结算与失败收尾都改余额，
/// 提交后要把新余额写穿缓存，所以两个进程必须看同一个缓存服务。
fn spawn_worker_process_with(database_url: &str, cache: Option<&CacheFixture>) -> WorkerProcess {
    let mut command = Command::new(worker_binary());
    command
        .env("DATABASE_URL", database_url)
        .env("WORKER_ID", "driver-contract-worker")
        .env("WORKER_POLL_INTERVAL_MS", "200")
        .env("WORKER_LEASE_SECONDS", "300")
        .env("PROVIDER_TIMEOUT_SECONDS", "60")
        .env("APIMART_API_KEY", "contract-test-key")
        .env("AIHUBMIX_API_KEY", "contract-test-key")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    apply_cache_env(&mut command, cache);
    let child = command.spawn().expect("worker process should start");
    WorkerProcess { child }
}

/// 一个真实 Worker 进程。
///
/// `Drop` 时结束它：断言失败也不会留下孤儿 Worker 把二进制锁住（那会让下一次
/// `cargo build -p seeai-worker` 失败，看起来像代码错）。
struct WorkerProcess {
    child: Child,
}

impl Drop for WorkerProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// 驱动端到端验证的公共装置：假上游 + API 进程 + 一条发布的供给。
struct Harness {
    model: &'static str,
    database_url: String,
    database_name: String,
    base_url: String,
    admin_token: String,
    api_key: String,
    pool: PgPool,
    calls: UpstreamCalls,
    upstream_base_url: String,
    /// 这次用例**实际发布**的那份**线上声明面**（承载面 `properties` 的顶层名字；老形状的素材
    /// 只有一份 `capability_schema`，那时它就是承载面）。
    ///
    /// 判定"上游请求体里该有哪些名字"只能按它来：两家渠道的声明面并不相同
    /// （APIMart 是 `image_urls` / `mask_url`，AIHubMix 是 `image` / `mask`），
    /// 写死任何一份素材，换到另一家的用例上就会判错——该报的不报、该放的不放。
    /// 合同里声明、承载面没声明的字段（改名或换算之前的样子）不属于它：那些名字永远不上线。
    declared_surface: serde_json::Map<String, Value>,
    /// 保持 API 进程存活；丢弃即结束它。
    _api: ApiProcess,
    /// 保持假上游的监听任务存活。
    _upstream: FakeUpstream,
    /// 这次用例给 API 与 Worker 配的加速层（假 Redis）；没配就是"没有缓存"的那条路径。
    cache: Option<CacheFixture>,
}

impl Harness {
    const MODEL: &'static str = "driver-model";

    async fn start(behaviour: UpstreamBehaviour) -> Self {
        let (provider_kind, adapter_key) = match behaviour.provider {
            ProviderShape::Apimart => ("APIMart", "apimart-image-v1"),
            ProviderShape::Aihubmix => ("AIHubMix", "aihubmix-image-v1"),
        };
        Self::start_with(
            provider_kind,
            adapter_key,
            &["prompt_only", "image_conditioned", "masked"],
            None,
            behaviour,
            64,
        )
        .await
    }

    /// 同 `start`，但指定分支、命名与并发上限；`currency` 给 `Some` 时让这条供给**自己声明**
    /// 一个成本币种（不再假定 USD），不给就用素材里的那份声明。
    async fn start_with(
        provider_kind: &str,
        adapter_key: &str,
        branches: &[&str],
        currency: Option<&str>,
        behaviour: UpstreamBehaviour,
        max_concurrent_jobs: u64,
    ) -> Self {
        let credential_env = match provider_kind {
            "AIHubMix" => "AIHUBMIX_API_KEY",
            _ => "APIMART_API_KEY",
        };
        let mut draft = candidate(provider_kind, adapter_key, branches);
        draft["credential_env"] = Value::String(credential_env.to_owned());
        if let Some(currency) = currency {
            draft["price_plan"]["currency"] = Value::String(currency.to_owned());
        }
        Self::start_with_draft(draft, None, behaviour, max_concurrent_jobs).await
    }

    /// 同 `start`，但用**素材里的真实声明面**发布（而不是测试手写的最小 Profile）。
    ///
    /// 参数面过滤按候选声明的字段名走，所以"哪些参数会被留下"只有在真实声明面上才验得准：
    /// 手写的最小 Profile 里除了 `prompt` 什么都没有，一过滤就把所有参数都丢了。
    async fn start_with_bootstrap(behaviour: UpstreamBehaviour, max_concurrent_jobs: u64) -> Self {
        // 一份素材里有两条供给：下标 0 是 AIHubMix、下标 1 是 APIMart。按用例要起的那家假上游取一条；
        // 合同仍取素材**顶层那一份**——改名的源名（`image` / `mask`）只在合同里，承载面里是线上名。
        let material: Value = serde_json::from_str(include_str!(
            "../../../config/bootstrap/gpt-image-2.5-flare.json"
        ))
        .expect("bootstrap material parses");
        let index = match behaviour.provider {
            ProviderShape::Aihubmix => 0,
            ProviderShape::Apimart => 1,
        };
        let mut draft = material["offerings"][index].clone();
        // 上游地址换成这个用例的假上游；凭证仍从环境变量读，值只写在测试进程环境里。
        draft["base_url"] = Value::String("http://127.0.0.1:1".to_owned());
        Self::start_with_draft(
            draft,
            Some(material["capability_schema"].clone()),
            behaviour,
            max_concurrent_jobs,
        )
        .await
    }

    async fn start_with_draft(
        draft: Value,
        contract: Option<Value>,
        behaviour: UpstreamBehaviour,
        max_concurrent_jobs: u64,
    ) -> Self {
        Self::build(draft, contract, behaviour, max_concurrent_jobs, 30, None).await
    }

    /// 同 `start_with_draft`，但给 API 与 Worker 配上**加速层**（假 Redis）。
    ///
    /// `sync_wait_seconds` 也在这里给：验收里有的用例故意不跑 Worker，让同步入口在很短的窗口后
    /// 超时——那时 Job 已经受理、预授权也扣了，正好用来观察写穿。
    async fn start_with_cache(
        draft: Value,
        contract: Option<Value>,
        behaviour: UpstreamBehaviour,
        max_concurrent_jobs: u64,
        sync_wait_seconds: u64,
        cache: CacheFixture,
    ) -> Self {
        Self::build(
            draft,
            contract,
            behaviour,
            max_concurrent_jobs,
            sync_wait_seconds,
            Some(cache),
        )
        .await
    }

    async fn build(
        mut draft: Value,
        contract: Option<Value>,
        behaviour: UpstreamBehaviour,
        max_concurrent_jobs: u64,
        sync_wait_seconds: u64,
        cache: Option<CacheFixture>,
    ) -> Self {
        let (database_url, database_name) = isolated_database_url().await;
        let calls: UpstreamCalls = Arc::new(Mutex::new(Vec::new()));
        let upstream = start_fake_upstream_with(calls.clone(), behaviour).await;
        let (base_url, admin_token, process) = start_api_with(
            &database_url,
            sync_wait_seconds,
            max_concurrent_jobs,
            cache.as_ref(),
        )
        .await;
        let client = Client::new();
        wait_until_ready(&client, &base_url).await;
        let account = create_account(&client, &base_url, &admin_token).await;
        let api_key = issue_key(&client, &base_url, &admin_token, &account).await;
        let pool = PgPool::connect(&database_url)
            .await
            .expect("contract database");

        draft["base_url"] = Value::String(upstream.base_url.clone());
        // 判据在发布之前从同一份候选里取出来：它就是这次用例真正交给平台的**线上声明面**——
        // 候选带了承载面就用承载面（承载面声明的才是线上字段名），老形状的素材只有一份
        // `capability_schema`，那时它就是承载面。
        let wire_schema = draft
            .get("carrier_schema")
            .filter(|value| !value.is_null())
            .unwrap_or(&draft["capability_schema"]);
        let declared_surface = wire_schema["properties"]
            .as_object()
            .cloned()
            .expect("published candidate must declare a wire surface");
        let published = publish_candidates(
            &client,
            &base_url,
            &admin_token,
            Self::MODEL,
            contract,
            vec![draft],
        )
        .await;
        assert_eq!(published, StatusCode::OK, "publication must succeed");

        Self {
            model: Self::MODEL,
            database_url,
            database_name,
            base_url,
            admin_token,
            api_key,
            pool,
            calls,
            upstream_base_url: upstream.base_url.clone(),
            declared_surface,
            _api: process,
            _upstream: upstream,
            cache,
        }
    }

    /// 起一个真实 Worker 进程（丢弃返回值即结束它）。
    ///
    /// Worker 与 API 共用同一个缓存服务：结算改余额之后要把新余额写穿，否则缓存会留着一个
    /// 刚写过、但偏高的余额。
    fn spawn_worker(&self) -> WorkerProcess {
        spawn_worker_process_with(&self.database_url, self.cache.as_ref())
    }

    /// 这次用例的假 Redis；没配缓存的用例调用它会直接失败（那是用例写错了）。
    fn cache(&self) -> &CacheFixture {
        self.cache
            .as_ref()
            .expect("this test must run with the cache fixture")
    }

    /// 走同步入口发一次 JSON 请求，并起真实 Worker 把它跑到终态。
    async fn sync_json(&self, path: &str, key: &str, body: Value) -> (StatusCode, Value) {
        let _worker = self.spawn_worker();
        post_json(&self.base_url, &self.api_key, path, key, &body).await
    }

    /// 走同步入口发一次 multipart 请求（edits 路径），并起真实 Worker 跑完。
    async fn sync_multipart(
        &self,
        key: &str,
        form: reqwest::multipart::Form,
    ) -> (StatusCode, Value) {
        let _worker = self.spawn_worker();
        let response = Client::new()
            .post(format!("{}/v1/images/edits", self.base_url))
            .bearer_auth(&self.api_key)
            .header("idempotency-key", key)
            .multipart(form)
            .send()
            .await
            .expect("multipart request");
        let status = response.status();
        let body = response.text().await.expect("multipart response body");
        (
            status,
            serde_json::from_str(&body).unwrap_or(Value::String(body)),
            // 上面这行在解析失败时把原文留在 Value 里，断言失败时看得见响应体。
        )
    }

    /// 按幂等键取回这次请求内部的执行记录：`(job_id, state, result_images)`。
    ///
    /// 这是**内部**事实，对客响应里没有它——同步入口不返回 job_id。
    async fn job(&self, key: &str) -> (Uuid, String, Option<Value>) {
        let row = sqlx::query(
            "SELECT id, state, result_images FROM generation.jobs WHERE idempotency_key = $1",
        )
        .bind(key)
        .fetch_one(&self.pool)
        .await
        .expect("the request must have created a job record");
        (
            row.try_get("id").expect("job id"),
            row.try_get("state").expect("job state"),
            row.try_get("result_images").expect("result images"),
        )
    }

    /// 假上游记录下来的请求。
    fn recorded(&self) -> Vec<UpstreamCall> {
        self.calls.lock().expect("calls lock").clone()
    }

    /// 这次执行留下的**成本事实**四列：原币种金额、币种、来源、折算后 CNY。
    async fn attempt_cost(
        &self,
        job_id: Uuid,
    ) -> (Option<i64>, Option<String>, Option<String>, Option<i64>) {
        let row = sqlx::query(
            "SELECT provider_cost_microusd, provider_cost_currency, provider_cost_source, \
             provider_cost_cny_microusd FROM generation.attempts WHERE job_id = $1",
        )
        .bind(job_id)
        .fetch_one(&self.pool)
        .await
        .expect("the request must have created an attempt record");
        (
            row.try_get("provider_cost_microusd").expect("cost amount"),
            row.try_get("provider_cost_currency")
                .expect("cost currency"),
            row.try_get("provider_cost_source").expect("cost source"),
            row.try_get("provider_cost_cny_microusd")
                .expect("cost in CNY"),
        )
    }

    /// 这次结算**实际扣了对客多少钱**（账本是权威，取 capture 分录的金额）。
    async fn captured_microusd(&self, job_id: Uuid) -> i64 {
        sqlx::query_scalar(
            "SELECT amount_microusd FROM ledger.entries WHERE job_id = $1 AND kind = 'capture'",
        )
        .bind(job_id)
        .fetch_one(&self.pool)
        .await
        .expect("a settled job must have a capture entry")
    }

    /// 某个路径上**最近一次**记录到的生成请求体（JSON 形态）。
    fn submit_body(&self, path: &str) -> Value {
        let raw = self
            .recorded()
            .into_iter()
            .rfind(|call| call.method == "POST" && call.path == path)
            .map(|call| call.body)
            .expect("the driver must submit a generation request");
        serde_json::from_slice(&raw).expect("submit body is JSON")
    }

    /// 某个路径上**最近一次**记录到的原始请求体（multipart 形态）。
    fn submit_bytes(&self, path: &str) -> Vec<u8> {
        self.recorded()
            .into_iter()
            .rfind(|call| call.method == "POST" && call.path == path)
            .map(|call| call.body)
            .expect("the driver must submit a generation request")
    }

    fn count(&self, method: &str, path_prefix: &str) -> usize {
        self.recorded()
            .iter()
            .filter(|call| call.method == method && call.path.starts_with(path_prefix))
            .count()
    }

    /// 线上请求体里的每一个字段都必须来自**这次用例实际发布的**候选声明面。
    ///
    /// 平台会往请求体里写 `model`、`prompt`，并把调用方给的图落到**候选自己声明的**图片参数名上
    /// （`image_urls`、`mask_url`……）：这些字段全部来自 Profile 的声明面，平台不许在旁边另加
    /// 一层自己的包装（例如曾经的 `extra`）。
    ///
    /// 判据是 `self.declared_surface`，也就是本用例发布出去的那份**承载面**（线上声明面；
    /// 老形状的素材只有一份 `capability_schema`，那时它就是承载面），不是某一份写死的素材：
    /// 两家的声明面不同（APIMart 收 `image_urls` / `mask_url`，AIHubMix 收 `image` / `mask` /
    /// `quality` 等），钉死一份就会在另一家的用例上判错——该报的不报、该放的不放。因为判据跟着
    /// 用例自己的发布走，调用点不用各自声明用哪份声明面，换成带图的用例也一样成立。
    ///
    /// 平台**只**发声明过的参数名：调用方额外带来的名字（例如 `image_with_roles`）在受理期就按
    /// 声明面丢掉了，既不许出现在上游请求体里，也不许被平台改名后带上去。所以这里同时钉两件事：
    /// 线上没有一个名字超出声明面，且调用方带的未声明参数一个都没上行。
    fn assert_only_declared_fields(&self, caller_body: &Value) {
        let declared = &self.declared_surface;
        let submit_body = self.submit_body("/v1/images/generations");
        let sent = submit_body.as_object().expect("sent body object");
        let supplied = caller_body.as_object().expect("caller body object");
        for name in sent.keys() {
            assert!(
                declared.contains_key(name),
                "`{name}` 不在这次发布的候选声明面里：平台把上游没声明过的字段发了出去"
            );
        }
        for name in ["model", "prompt"] {
            assert!(sent.contains_key(name), "平台必须逐字发出 `{name}`");
            assert!(
                declared.contains_key(name),
                "`{name}` 是平台自己产生的字段，必须在这份候选的声明里"
            );
        }
        // 调用方发了、候选没声明的名字：受理期丢掉，不许出现在上游请求体里。
        for name in supplied.keys() {
            if !declared.contains_key(name) {
                assert!(
                    !sent.contains_key(name),
                    "未声明的参数 `{name}` 必须在上游请求体里完全不出现"
                );
            }
        }
    }

    async fn cleanup(&self) {
        // 先放掉自己的连接，再去删库：否则 DROP 只能靠 `WITH (FORCE)` 强踢，
        // 偶尔会留下一次性库。
        self.pool.close().await;
        drop_isolated_database(&self.database_name).await;
    }
}

/// 对客响应里**不许**出现内部的执行记录，也不许指路任何查询接口。
///
/// 内部它就是一个执行与审计记录：没有 job id、没有任务号、没有"去查任务"的指引。
/// 这里同时钉住"渠道/供给侧的词汇不进对客响应"：供给、渠道、驱动与厂商原生型号都是平台内部
/// 的组织方式，调用方拿到的只有型号身份与它公开的能力面（目录里的合同就是后者）。
fn assert_public_only(what: &str, body: &Value) {
    let rendered = body.to_string();
    for needle in [
        "job",
        "Job",
        "image-generations",
        "task_id",
        "task-contract",
        "attempt",
        "assets",
        "offering",
        "Offering",
        "channel",
        "Channel",
        "provider_kind",
        "provider_model_id",
        "adapter_key",
    ] {
        assert!(
            !rendered.contains(needle),
            "{what} 的对客响应出现了内部字样 `{needle}`：{rendered}"
        );
    }
    for needle in ["poll", "轮询", "查询", "去查"] {
        assert!(
            !rendered.contains(needle),
            "{what} 的对客响应指路了查询接口：{rendered}"
        );
    }
}

/// 发一次 JSON 请求到同步入口，返回状态与响应体。
async fn post_json(
    base_url: &str,
    api_key: &str,
    path: &str,
    key: &str,
    body: &Value,
) -> (StatusCode, Value) {
    let response = Client::new()
        .post(format!("{base_url}{path}"))
        .bearer_auth(api_key)
        .header("idempotency-key", key)
        .json(body)
        .send()
        .await
        .expect("generation request");
    let status = response.status();
    let raw = response.text().await.expect("generation body");
    (
        status,
        serde_json::from_str(&raw).unwrap_or(Value::String(raw)),
    )
}

/// OpenAI 形状的成功响应：`{created, data:[…]}`，且没有内部字样。
fn assert_sync_success(what: &str, body: &Value) {
    assert_public_only(what, body);
    assert!(
        body["created"].as_i64().is_some(),
        "{what}: created is required, got {body}"
    );
    let data = body["data"]
        .as_array()
        .unwrap_or_else(|| panic!("{what}: data must be an array, got {body}"));
    assert!(!data.is_empty(), "{what}: data must not be empty");
    for item in data {
        let has_url = item["url"].as_str().is_some();
        let has_base64 = item["b64_json"].as_str().is_some();
        assert!(
            has_url ^ has_base64,
            "{what}: every item keeps exactly one of url / b64_json, got {item}"
        );
        assert_eq!(
            item.as_object().map(serde_json::Map::len),
            Some(1),
            "{what}: no other fields may appear on an image item, got {item}"
        );
    }
}

/// 只要走通一次真实执行，内部就必须留下一条跑完的记录：内部有记录，对客看不见。
async fn assert_job_succeeded(harness: &Harness, key: &str) -> Value {
    let (_, state, images) = harness.job(key).await;
    assert_eq!(state, "succeeded", "内部执行记录必须跑到终态");
    images.expect("a successful job keeps the result envelope")
}

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

/// AIHubMix：两条同步路径都能跑通，且**渠道给什么就返回什么**。
///
/// 覆盖 data URL 输入、公网 URL 输入与 `url` / `b64_json` 两种上游形态。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn aihubmix_sync_entries_accept_images_and_return_the_provider_envelope() {
    // 上游给 base64：平台的响应里就必须是 b64_json。
    let harness = Harness::start(UpstreamBehaviour::aihubmix(SyncImageShape::Base64)).await;
    let client = Client::new();

    // 1) 文生图：JSON 入口，没有图片。
    let key = format!("sync-gen-{}", Uuid::new_v4());
    let (status, body) = harness
        .sync_json(
            "/v1/images/generations",
            &key,
            route_request(harness.model, "sync prompt"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("文生图", &body);
    assert_eq!(
        body["data"][0]["b64_json"].as_str(),
        Some(STANDARD.encode(PNG_FIXTURE).as_str()),
        "上游给 base64，平台必须原样交回"
    );
    let stored = assert_job_succeeded(&harness, &key).await;
    assert_eq!(
        stored,
        json!([{"b64_json": STANDARD.encode(PNG_FIXTURE)}]),
        "内部记录里存的也是渠道给的那份信封"
    );

    // 2) 参考图走**公网 URL**：这个渠道要字节，所以由 Adapter 自己去取。
    let reference_url = format!("{}/inputs/ref.png", harness.upstream_base_url);
    let key = format!("sync-url-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "edit with a public url");
    request["image_urls"] = json!([reference_url]);
    let (status, body) = harness
        .sync_json("/v1/images/generations", &key, request.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("公网 URL 参考图", &body);
    assert_eq!(
        harness.count("GET", "/inputs/ref.png"),
        1,
        "公网 URL 由 Adapter 自己取一次"
    );
    let edits = harness.submit_bytes("/v1/images/edits");
    let rendered = String::from_utf8_lossy(&edits);
    assert!(
        rendered.contains("name=\"image\""),
        "参考图必须走 image 文件部件"
    );
    assert!(
        body_contains_bytes(&edits, PNG_FIXTURE),
        "公网 URL 取回的字节必须原样进文件部件"
    );
    // 平台不落盘：没有上传接口调用，也没有资产接口可用。
    assert_eq!(harness.count("POST", "/v1/uploads/images"), 0);

    // 3) 参考图走**data URL**：就地解码成字节，仍然不落盘。
    let key = format!("sync-inline-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "edit with an inline image");
    request["image"] = json!(png_data_url());
    let (status, body) = harness
        .sync_json("/v1/images/generations", &key, request.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("data URL 参考图", &body);
    let edits = harness.submit_bytes("/v1/images/edits");
    assert!(
        body_contains_bytes(&edits, PNG_FIXTURE),
        "data URL 必须就地解码进文件部件"
    );
    let (_, state, _) = harness.job(&key).await;
    assert_eq!(state, "succeeded");

    // 4) edits 入口（multipart 文件部件）：与 generations 是**同一个能力**。
    let key = format!("sync-edit-{}", Uuid::new_v4());
    let form = reqwest::multipart::Form::new()
        .part(
            "image",
            reqwest::multipart::Part::bytes(PNG_FIXTURE.to_vec())
                .file_name("input.png")
                .mime_str("image/png")
                .expect("mime"),
        )
        .part(
            "mask",
            reqwest::multipart::Part::bytes(PNG_FIXTURE.to_vec())
                .file_name("mask.png")
                .mime_str("image/png")
                .expect("mime"),
        )
        .text("model", harness.model.to_owned())
        .text("prompt", "edit through the multipart entry");
    let (status, body) = harness.sync_multipart(&key, form).await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("edits 入口", &body);
    let edits = harness.submit_bytes("/v1/images/edits");
    let rendered = String::from_utf8_lossy(&edits);
    assert!(rendered.contains("name=\"image\"") && rendered.contains("name=\"mask\""));
    assert!(
        body_contains_bytes(&edits, PNG_FIXTURE),
        "两个文件部件的字节都必须到上游"
    );
    // 文件部件在受理期被转成 data URL 语义，落在该候选自己的参数名上。
    let stored: Value = sqlx::query_scalar(
        "SELECT native_parameters FROM generation.jobs WHERE idempotency_key = $1",
    )
    .bind(&key)
    .fetch_one(&harness.pool)
    .await
    .expect("native parameters");
    assert!(
        stored["image"]
            .as_str()
            .is_some_and(|v| v.starts_with("data:image/")),
        "文件部件必须以 data URL 语义留在内部参数里，got {stored}"
    );
    assert!(
        stored["mask"]
            .as_str()
            .is_some_and(|v| v.starts_with("data:image/"))
    );

    // 5) 同义字段只能给一个；只给遮罩是结构性错误。
    let key = format!("sync-conflict-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "both synonyms");
    request["image"] = json!(png_data_url());
    request["image_urls"] = json!(["https://example.invalid/a.png"]);
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert_eq!(body["error"]["code"].as_str(), Some("invalid_parameter"));
    assert_public_only("同义字段冲突", &body);

    let key = format!("sync-mask-only-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "mask without an image");
    request["mask"] = json!(png_data_url());
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert_public_only("只有遮罩", &body);

    let _ = client;
    harness.cleanup().await;
}

/// AIHubMix 的参考图按**张数**换线上部件名：多张是重复的 `image[]`，单张才是单值 `image`。
///
/// 为什么要有这条端到端：这条规则在 Driver 单测里已经钉住，但真正要保证的是**发布出去的那份
/// 声明面**——参考图按厂商契约声明成字符串数组（≤16）——能一路走到线上：受理时两张都留得下、
/// 选路时这条候选表达得了、装图时两张都落到它声明的名字上、最后按张数编码。上面任何一处只留下
/// 第一张，对客响应照样是 200，只有看发给上游的报文才暴露出来。
///
/// 两张都用内联 data URL：图片字节就地解码，这条链路不必让假上游提供图片文件。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn aihubmix_encodes_several_reference_images_as_repeated_list_parts() {
    let harness =
        Harness::start_with_bootstrap(UpstreamBehaviour::aihubmix(SyncImageShape::Url), 64).await;

    // 1) 两张参考图：线上是两个 `image[]` 部件，不出现单值 `image`，也没有遮罩部件。
    let key = format!("sync-two-refs-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "edit with two reference images");
    request["image"] = json!([png_data_url(), png_data_url()]);
    let (status, body) = harness
        .sync_json("/v1/images/generations", &key, request)
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("两张参考图", &body);
    // 带图的请求走的是编辑端点：这条渠道的参考图只在 /v1/images/edits 上收。
    assert_eq!(harness.count("POST", "/v1/images/edits"), 1);
    assert_eq!(harness.count("POST", "/v1/images/generations"), 0);

    let edits = harness.submit_bytes("/v1/images/edits");
    let rendered = String::from_utf8_lossy(&edits);
    assert_eq!(
        part_name_count(&rendered, "image[]"),
        2,
        "两张参考图就是两个 `image[]` 部件：{rendered}"
    );
    assert_eq!(
        part_name_count(&rendered, "image"),
        0,
        "多张时不许退回单值 `image`（渠道会 400）：{rendered}"
    );
    assert_eq!(
        part_name_count(&rendered, "mask"),
        0,
        "这次请求没有遮罩，线上就不该有 `mask` 部件：{rendered}"
    );
    assert!(
        body_contains_bytes(&edits, PNG_FIXTURE),
        "参考图的字节必须原样进文件部件"
    );

    // 两张都留在了这次请求的参数面里，且都落在候选声明的名字上：不是只留下第一张。
    let stored: Value = sqlx::query_scalar(
        "SELECT native_parameters FROM generation.jobs WHERE idempotency_key = $1",
    )
    .bind(&key)
    .fetch_one(&harness.pool)
    .await
    .expect("native parameters");
    assert_eq!(
        stored["image"].as_array().map(Vec::len),
        Some(2),
        "两张参考图都要留在内部参数里，got {stored}"
    );

    // 2) 一张参考图：同一份声明面下仍是单值 `image`——列表形态只属于多张。
    let key = format!("sync-one-ref-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "edit with one reference image");
    request["image"] = json!([png_data_url()]);
    let (status, body) = harness
        .sync_json("/v1/images/generations", &key, request)
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("一张参考图", &body);
    let edits = harness.submit_bytes("/v1/images/edits");
    let rendered = String::from_utf8_lossy(&edits);
    assert_eq!(
        part_name_count(&rendered, "image"),
        1,
        "一张参考图就是单值 `image`：{rendered}"
    );
    assert_eq!(
        part_name_count(&rendered, "image[]"),
        0,
        "单张不许用列表形态：{rendered}"
    );

    assert_job_succeeded(&harness, &key).await;
    harness.cleanup().await;
}

/// 上游给 `url` 时，平台把那个地址**原样**交回，绝不下载、不转存。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn aihubmix_returns_the_url_shape_verbatim() {
    let harness = Harness::start(UpstreamBehaviour::aihubmix(SyncImageShape::Url)).await;
    let key = format!("sync-url-shape-{}", Uuid::new_v4());
    let (status, body) = harness
        .sync_json(
            "/v1/images/generations",
            &key,
            route_request(harness.model, "url shape"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("url 形态", &body);
    let expected = format!("{}/result.png", harness.upstream_base_url);
    assert_eq!(body["data"][0]["url"].as_str(), Some(expected.as_str()));
    assert!(
        body["data"][0].get("b64_json").is_none(),
        "上游只给了 url，平台不许自己补一个 base64"
    );
    // 结果地址是**给调用方**的：平台自己不去取它。
    assert_eq!(harness.count("GET", "/result.png"), 0);
    assert_job_succeeded(&harness, &key).await;
    harness.cleanup().await;
}

/// APIMart 的完整驱动流程：提交 → 轮询 → 终态；结果地址原样交回，
/// 计量证据与对账标识留在内部。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn apimart_driver_executes_the_task_flow_against_a_local_upstream() {
    let harness = Harness::start(UpstreamBehaviour::apimart()).await;
    let key = format!("driver-{}", Uuid::new_v4());
    let request = route_request(harness.model, "driver prompt");
    let (status, body) = harness
        .sync_json("/v1/images/generations", &key, request.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("任务式流程", &body);

    let (job_id, state, images) = harness.job(&key).await;
    assert_eq!(state, "succeeded", "the driver flow must settle the job");
    assert_eq!(
        images,
        Some(json!([{"url": format!("{}/result.png", harness.upstream_base_url)}])),
        "结果信封里只有上游给的那个地址"
    );
    assert_eq!(harness.count("GET", "/result.png"), 0, "平台不许下载结果图");

    // 计量证据：四分项 usage 落到 attempts.metering_evidence。
    let evidence: Value =
        sqlx::query_scalar("SELECT metering_evidence FROM generation.attempts WHERE job_id = $1")
            .bind(job_id)
            .fetch_one(&harness.pool)
            .await
            .expect("metering evidence");
    assert_eq!(evidence["usage"]["input_text_tokens"], 14);
    assert_eq!(evidence["usage"]["input_image_tokens"], 0);
    assert_eq!(evidence["usage"]["output_image_tokens"], 196);
    assert_eq!(evidence["usage"]["total_tokens"], 210);

    // 对账标识落到**已存在**的 attempts.provider_trace_id 列：
    // 该列此前只有 fail_job 在写，成功路径不写。
    let trace_id: Option<String> =
        sqlx::query_scalar("SELECT provider_trace_id FROM generation.attempts WHERE job_id = $1")
            .bind(job_id)
            .fetch_one(&harness.pool)
            .await
            .expect("attempt trace id");
    assert_eq!(
        trace_id.as_deref(),
        Some("task-contract-1"),
        "the upstream task id must be persisted for manual reconciliation"
    );

    // 成本事实：上游终态**直接声明了金额**，所以直接取它（含渠道侧折扣，比自算权威）；
    // 币种是**该供给声明的**那个，不假定 USD。折算值这一片不写——汇率还没有落点。
    let (amount, currency, source, cny) = harness.attempt_cost(job_id).await;
    assert_eq!(source.as_deref(), Some("declared"));
    assert_eq!(amount, Some(11_354), "实测样例 cost = 0.011354");
    assert_eq!(currency.as_deref(), Some("USD"));
    assert_eq!(cny, None, "折算要用受理时冻结的汇率，这一片还没有它");

    // 采集成本**不改对客金额**：实收仍是该渠道费率 × 实际分项 token
    // （14 文本输入 × 5 + 196 图像输出 × 30 = 5950 微单位），与上游声明的 11354 是两个量。
    assert_eq!(
        harness.captured_microusd(job_id).await,
        -5_950,
        "上游声明的金额只进成本口径，不许动对客实收"
    );

    // Driver 的线上请求：只提交一次，且参数在顶层（无 extra 包装）。
    assert_eq!(
        harness.count("POST", "/v1/images/generations"),
        1,
        "the create request must never be resent"
    );
    let submit_body = harness.submit_body("/v1/images/generations");
    assert_eq!(submit_body["model"], harness.model);
    assert_eq!(submit_body["prompt"], "driver prompt");
    assert!(
        submit_body.get("extra").is_none(),
        "APIMart takes parameters at the top level"
    );
    harness.assert_only_declared_fields(&request);
    assert!(
        harness.count("GET", "/v1/tasks/") >= 1,
        "the driver must poll the task at least once"
    );
    harness.cleanup().await;
}

/// 渠道声明了会给金额，这次却**拿不到**（终态里没有这个字段）⇒ 不猜：
/// 金额与币种留空、来源记 `unavailable`，缺口查得出来；对客结算照常完成。
///
/// 这是"成本缺口"与"执行失败"的分界：缺口是平台侧的账务问题，不该把消费者的钱扣在对账里。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_declared_cost_that_never_arrives_is_recorded_as_a_gap_not_guessed() {
    let mut behaviour = UpstreamBehaviour::apimart();
    behaviour.declared_cost = None;
    let harness = Harness::start(behaviour).await;
    let key = format!("cost-gap-{}", Uuid::new_v4());
    let (status, body) = harness
        .sync_json(
            "/v1/images/generations",
            &key,
            route_request(harness.model, "cost never arrives"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("成本缺口", &body);

    let (job_id, state, _) = harness.job(&key).await;
    assert_eq!(state, "succeeded", "成本缺口不是执行失败：对客结算照常完成");
    let (amount, currency, source, cny) = harness.attempt_cost(job_id).await;
    assert_eq!(source.as_deref(), Some("unavailable"));
    assert_eq!(amount, None, "拿不到金额就留空：不写 0、也不用费率顶替");
    assert_eq!(currency, None);
    assert_eq!(cny, None);
    // 缺口可发现：按来源筛得出来，不用去翻上游账单才知道有这么一笔。
    let gaps: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM generation.attempts \
         WHERE job_id = $1 AND provider_cost_source = 'unavailable'",
    )
    .bind(job_id)
    .fetch_one(&harness.pool)
    .await
    .expect("gap query");
    assert_eq!(gaps, 1);
    // 对客实收不受成本缺口影响。
    assert_eq!(harness.captured_microusd(job_id).await, -5_950);
    harness.cleanup().await;
}

/// 渠道**不给任何金额字段**（AIHubMix）⇒ 成本按本次实际用量与该渠道四档费率自算，
/// 币种按该渠道声明。它只进成本口径：对客实收另有出处（账本），两者不是同一个量。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_cost_the_channel_never_reports_is_computed_from_the_actual_usage() {
    let harness = Harness::start(UpstreamBehaviour::aihubmix(SyncImageShape::Url)).await;
    let key = format!("cost-computed-{}", Uuid::new_v4());
    let (status, body) = harness
        .sync_json(
            "/v1/images/generations",
            &key,
            route_request(harness.model, "computed cost"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("自算成本", &body);

    let (job_id, state, _) = harness.job(&key).await;
    assert_eq!(state, "succeeded");
    let (amount, currency, source, cny) = harness.attempt_cost(job_id).await;
    assert_eq!(source.as_deref(), Some("computed"));
    assert_eq!(currency.as_deref(), Some("USD"));
    // 实际用量：14 文本输入 × 5 + 196 图像输出 × 30（每 1M） = 5950 微单位。
    assert_eq!(amount, Some(5_950));
    assert_eq!(cny, None);
    assert_eq!(harness.captured_microusd(job_id).await, -5_950);
    harness.cleanup().await;
}

/// 币种**按渠道声明接受**：声明 `CNY` 的供给不再被发布期硬拒，落库的币种就是声明值。
///
/// 能发布出来本身就证明那条"必须是 USD"的硬校验已经不在了；而成本列里的币种证明它不是
/// 被平台替换成某个默认币种，而是**照声明的原值**记下来的。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_channel_declared_currency_other_than_usd_is_accepted_and_recorded() {
    let harness = Harness::start_with(
        "APIMart",
        "apimart-image-v1",
        &["prompt_only"],
        Some("CNY"),
        UpstreamBehaviour::apimart(),
        64,
    )
    .await;
    let key = format!("cost-currency-{}", Uuid::new_v4());
    let (status, body) = harness
        .sync_json(
            "/v1/images/generations",
            &key,
            route_request(harness.model, "declared currency"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("按声明币种", &body);

    let (job_id, _, _) = harness.job(&key).await;
    let (amount, currency, source, _) = harness.attempt_cost(job_id).await;
    assert_eq!(source.as_deref(), Some("declared"));
    assert_eq!(amount, Some(11_354));
    assert_eq!(
        currency.as_deref(),
        Some("CNY"),
        "币种权威是该供给声明的那个值，平台不替换成 USD"
    );
    harness.cleanup().await;
}

/// 公网 URL 原样透传（不下载、不上传），内联 data URL 才需要上传换 URL。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn apimart_passes_public_urls_through_and_uploads_inline_images() {
    let harness = Harness::start(UpstreamBehaviour::apimart()).await;

    // 1) 公网 URL：原样写进 `image_urls`，一次上传都没有。
    let reference_url = format!("{}/inputs/ref.png", harness.upstream_base_url);
    let key = format!("driver-public-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "edit this image");
    request["image"] = json!(reference_url);
    let (status, body) = harness
        .sync_json("/v1/images/generations", &key, request.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let submit_body = harness.submit_body("/v1/images/generations");
    assert_eq!(
        submit_body["image_urls"],
        json!([reference_url]),
        "公网 URL 必须逐字透传"
    );
    assert_eq!(
        harness.count("POST", "/v1/uploads/images"),
        0,
        "公网 URL 不需要上传"
    );
    assert_eq!(
        harness.count("GET", "/inputs/ref.png"),
        0,
        "平台不下载调用方给的公网参考图"
    );
    // 参考图落在该候选自己的参数名上（受理期完成映射）。
    let stored: Value = sqlx::query_scalar(
        "SELECT native_parameters FROM generation.jobs WHERE idempotency_key = $1",
    )
    .bind(&key)
    .fetch_one(&harness.pool)
    .await
    .expect("native parameters");
    assert_eq!(stored["image_urls"], json!([reference_url]));
    harness.assert_only_declared_fields(&request);

    // 2) 内联 data URL + 遮罩：各自上传一次换 URL，再填进 `image_urls` / `mask_url`。
    let key = format!("driver-inline-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "masked edit");
    request["image_urls"] = json!([png_data_url()]);
    request["mask"] = json!(png_data_url());
    let (status, body) = harness
        .sync_json("/v1/images/generations", &key, request.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        harness.count("POST", "/v1/uploads/images"),
        2,
        "参考图与遮罩各上传一次"
    );
    let submit_body = harness.submit_body("/v1/images/generations");
    let image_urls = submit_body["image_urls"]
        .as_array()
        .expect("image_urls must be an array");
    assert_eq!(image_urls.len(), 1);
    let uploaded_image = image_urls[0].as_str().expect("image url");
    let uploaded_mask = submit_body["mask_url"].as_str().expect("mask url");
    for url in [uploaded_image, uploaded_mask] {
        assert!(
            url.contains("/uploaded-"),
            "data URL 必须先换成公网 URL，got {url}"
        );
    }
    assert_ne!(uploaded_image, uploaded_mask, "两张图是两次上传");
    let rendered = submit_body.to_string();
    assert!(
        !rendered.contains("data:image"),
        "内联图片绝不能以 data URL 形态发给上游：{rendered}"
    );
    harness.assert_only_declared_fields(&request);

    // 3) 上游不发结果以外的任何东西：平台也不去取结果。
    assert_eq!(harness.count("GET", "/result.png"), 0);
    harness.cleanup().await;
}

/// 参数面以**选中候选的声明面**为准：声明过的参数原样上行，没声明的一律在上游请求体里消失。
///
/// 三条一起钉：
/// - 候选声明了 `quality`：调用方给的 `"high"` 逐字出现在发给假上游的请求体里；
/// - 候选没声明 `image_with_roles`（渠道文档里的一手参数）、`seed`、`foo`：请求照常 200、Job 照常
///   跑到成功，但这三个名字在发给假上游的报文里**一个字都没有**；
/// - 必填项在场照旧：同一份候选下发一个缺 `prompt` 的请求仍然是 400。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn declared_parameters_go_upstream_and_undeclared_ones_never_leave_the_platform() {
    // 用真实素材的声明面（AIHubMix 声明了 `quality`），只把上游地址换成假上游。
    let harness =
        Harness::start_with_bootstrap(UpstreamBehaviour::aihubmix(SyncImageShape::Url), 64).await;
    let key = format!("driver-declared-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "declared parameters only");
    request["quality"] = json!("high");
    // 三个都没被这份候选声明：一手图片参数、平台没声明的普通参数、纯属多余的字段。
    request["image_with_roles"] = json!([{
        "role": "reference",
        "url": format!("{}/inputs/roles.png", harness.upstream_base_url)
    }]);
    request["seed"] = json!(7);
    request["foo"] = json!({"a": 1});
    let (status, body) = harness
        .sync_json("/v1/images/generations", &key, request.clone())
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "多带一个没声明的参数不该让整次请求失败：{body}"
    );
    assert_sync_success("声明面过滤", &body);
    let submit_body = harness.submit_body("/v1/images/generations");
    assert_eq!(
        submit_body["quality"], "high",
        "声明过的参数原样上行：{submit_body}"
    );
    for dropped in ["image_with_roles", "seed", "foo"] {
        assert!(
            submit_body.get(dropped).is_none(),
            "候选没声明的 `{dropped}` 绝不能出现在上游请求体里：{submit_body}"
        );
    }
    // 它不是平台装载的参考图：平台不去取它。
    assert_eq!(
        harness.count("GET", "/inputs/roles.png"),
        0,
        "平台不把没声明的参数当参考图去取"
    );
    // 平台自己产生的字段仍在声明面内，且调用方带的未声明名字一个都没上行。
    harness.assert_only_declared_fields(&request);
    assert_job_succeeded(&harness, &key).await;

    // 必填项在场照旧：`prompt` 缺了就是 400（丢参数不等于不要必填）。
    let key = format!("driver-declared-missing-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &harness.api_key,
        "/v1/images/generations",
        &key,
        &json!({"model": harness.model, "quality": "high", "seed": 7}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert_eq!(body["error"]["code"].as_str(), Some("validation_error"));
    assert_public_only("缺必填项", &body);
    harness.cleanup().await;
}

/// 真素材的声明面同时钉住另一件容易漏的事：**平台装载的图片参数不会被过滤掉**。
///
/// APIMart 的 Profile 把参考图声明成 `image_urls`（数组），受理期先按声明面过滤、再把调用方给的图
/// 落到这个名字上。过滤若按"调用方原来带了什么"来做，这次请求就没有 `image_urls` 可用——图会
/// 静默丢掉。这里断言它照旧出现在发给假上游的请求体里，且没声明的名字一个都不留。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn filtering_never_drops_the_images_the_platform_places() {
    let harness = Harness::start_with_bootstrap(UpstreamBehaviour::apimart(), 64).await;
    let key = format!("driver-placed-images-{}", Uuid::new_v4());
    let reference_url = format!("{}/inputs/ref.png", harness.upstream_base_url);
    let mut request = route_request(harness.model, "the placed image survives the filter");
    request["image_urls"] = json!([reference_url.clone()]);
    request["image_with_roles"] =
        json!([{"role": "reference", "url": "https://example.invalid/other.png"}]);
    request["seed"] = json!(7);
    let (status, body) = harness
        .sync_json("/v1/images/generations", &key, request.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("平台装载的图不被过滤", &body);
    let submit_body = harness.submit_body("/v1/images/generations");
    assert_eq!(
        submit_body["image_urls"],
        json!([reference_url]),
        "装载进来的参考图必须落在候选声明的名字上：{submit_body}"
    );
    for dropped in ["image_with_roles", "seed"] {
        assert!(
            submit_body.get(dropped).is_none(),
            "候选没声明的 `{dropped}` 绝不能出现在上游请求体里：{submit_body}"
        );
    }
    harness.assert_only_declared_fields(&request);
    assert_job_succeeded(&harness, &key).await;
    harness.cleanup().await;
}

/// 编辑路径（multipart 入口）走同一套：声明过的参数原样到上游，没声明的到此为止。
///
/// AIHubMix 的编辑端点收的是表单部件：标量参数进文本部件、参考图进文件部件。这里断言两个方向——
/// 声明过的 `quality` 确实进了发给假上游的表单（文本部件里能读到它的名字），而 `seed` 与
/// `image_with_roles` 在整份表单字节里都不出现。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_multipart_edit_path_keeps_declared_parameters_and_drops_undeclared_ones() {
    let harness =
        Harness::start_with_bootstrap(UpstreamBehaviour::aihubmix(SyncImageShape::Base64), 64)
            .await;
    let key = format!("edits-declared-{}", Uuid::new_v4());
    let form = reqwest::multipart::Form::new()
        .text("model", harness.model.to_owned())
        .text("prompt", "an edit with a declared parameter")
        .text("quality", "high")
        .text("seed", "7")
        .text(
            "image_with_roles",
            json!([{"role": "reference", "url": "https://example.invalid/a.png"}]).to_string(),
        )
        .part(
            "image",
            reqwest::multipart::Part::bytes(PNG_FIXTURE.to_vec())
                .file_name("input.png")
                .mime_str("image/png")
                .expect("mime"),
        );
    let (status, body) = harness.sync_multipart(&key, form).await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("编辑路径的声明面过滤", &body);
    let edits = harness.submit_bytes("/v1/images/edits");
    let rendered = String::from_utf8_lossy(&edits);
    assert!(
        rendered.contains("name=\"quality\""),
        "声明过的 `quality` 必须进表单部件：{rendered}"
    );
    for dropped in ["seed", "image_with_roles"] {
        assert!(
            !rendered.contains(&format!("name=\"{dropped}\"")),
            "候选没声明的 `{dropped}` 绝不能出现在发给上游的表单里：{rendered}"
        );
    }
    assert!(
        rendered.contains("name=\"image\""),
        "平台装载的参考图照旧走文件部件：{rendered}"
    );
    assert!(
        body_contains_bytes(&edits, PNG_FIXTURE),
        "参考图字节必须原样进文件部件"
    );
    assert_job_succeeded(&harness, &key).await;
    harness.cleanup().await;
}

/// multipart 入口的图片也可以走**文本部件**（不带文件名）：与 JSON 入口同一套语义，
/// 值就是公网 URL 或 data URL，平台认的字段名照样只有 `image` / `image_urls` / `mask`。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn multipart_text_image_fields_follow_the_same_contract() {
    let harness = Harness::start(UpstreamBehaviour::aihubmix(SyncImageShape::Base64)).await;

    // 1) 不带文件名的 `image_urls` 文本件：正常出图。
    let key = format!("edits-text-{}", Uuid::new_v4());
    let form = reqwest::multipart::Form::new()
        .text("model", harness.model.to_owned())
        .text("prompt", "edit through a text field")
        .text("image_urls", png_data_url());
    let (status, body) = harness.sync_multipart(&key, form).await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("文本部件参考图", &body);
    let edits = harness.submit_bytes("/v1/images/edits");
    assert!(
        body_contains_bytes(&edits, PNG_FIXTURE),
        "文本部件里的 data URL 必须就地解码进文件部件"
    );

    // 2) `image` 与 `image_urls` 同时给非空值：同义字段含糊，受理前 400。
    let key = format!("edits-text-conflict-{}", Uuid::new_v4());
    let form = reqwest::multipart::Form::new()
        .text("model", harness.model.to_owned())
        .text("prompt", "both synonyms as text fields")
        .text("image", png_data_url())
        .text("image_urls", png_data_url());
    let (status, body) = harness.sync_multipart(&key, form).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert_eq!(body["error"]["code"].as_str(), Some("invalid_parameter"));
    assert_public_only("文本部件的同义字段冲突", &body);
    harness.cleanup().await;
}

/// 上传失败 = 生成任务**可证明未受理**：Job 走失败、预授权释放，不进对账。
///
/// 这与"提交之后出错进对账"是两条路径，不能混为一谈。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn upload_failure_fails_the_job_before_the_create_request() {
    let behaviour = UpstreamBehaviour {
        upload_failure_status: 400,
        ..UpstreamBehaviour::apimart()
    };
    let harness = Harness::start(behaviour).await;
    let key = format!("driver-upload-failure-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "edit this image");
    request["image"] = json!(png_data_url());
    let (status, body) = harness
        .sync_json("/v1/images/generations", &key, request.clone())
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_GATEWAY,
        "an upload failure is a platform-side failure: {body}"
    );
    assert_eq!(body["error"]["code"].as_str(), Some("platform_unavailable"));
    assert_public_only("上传失败", &body);

    let (job_id, state, _) = harness.job(&key).await;
    assert_eq!(
        state, "failed",
        "an upload failure happens before the create request, so the job is simply failed"
    );
    // 生成请求根本没发出去。
    assert_eq!(
        harness.count("POST", "/v1/images/generations"),
        0,
        "the create request must not be sent when the reference image could not be uploaded"
    );
    // 预授权释放：这台 Job 的 hold 不再是 active。
    let hold_status: String =
        sqlx::query_scalar("SELECT status FROM ledger.holds WHERE job_id = $1")
            .bind(job_id)
            .fetch_one(&harness.pool)
            .await
            .expect("the job must have a hold");
    assert_eq!(
        hold_status, "released",
        "a pre-acceptance failure must release the hold"
    );
    harness.cleanup().await;
}

/// 任务**查询**的瞬时失败可以重试，Job 最终仍成功。
///
/// 查询是幂等读，重试它不会造成重复副作用；这与"创建请求绝不重发"并不冲突。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn transient_query_failure_is_retried_and_the_job_still_succeeds() {
    let behaviour = UpstreamBehaviour {
        query_failures: 1,
        ..UpstreamBehaviour::apimart()
    };
    let outcome = run_driver_attempt(behaviour).await;
    assert_eq!(
        outcome.job_state, "succeeded",
        "a transient query failure must be retried, not turned into a job failure"
    );
    assert_eq!(
        outcome.submits, 1,
        "the create request must never be resent"
    );
    assert!(
        outcome.polls >= 2,
        "expected at least two queries (one failure + one success), got {}",
        outcome.polls
    );
    outcome.harness.cleanup().await;
}

/// 未在文档中出现的状态值必须**继续轮询**，不得当失败。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn unknown_task_status_keeps_polling_instead_of_failing() {
    let behaviour = UpstreamBehaviour {
        unknown_status_times: 1,
        ..UpstreamBehaviour::apimart()
    };
    let outcome = run_driver_attempt(behaviour).await;
    assert_eq!(
        outcome.job_state, "succeeded",
        "an undocumented status must not be treated as failure"
    );
    assert!(
        outcome.polls >= 2,
        "the driver must keep polling after an unknown status, got {} queries",
        outcome.polls
    );
    outcome.harness.cleanup().await;
}

/// 提交之后失败（轮询始终不通）必须进对账，**并且留下 task id**。
///
/// 对账的人能做的唯一一件事就是拿这个 id 去上游查；没有它，对账就是盲的。
/// 但这个 id 只留在内部：对客一个字的提示都没有。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn post_acceptance_failure_keeps_the_task_id_for_reconciliation() {
    // 查询永远 500：有界重试耗尽后仍失败 ⇒ 已确认生成、但取不到结果 ⇒ 对账。
    let behaviour = UpstreamBehaviour {
        query_failures: 99,
        ..UpstreamBehaviour::apimart()
    };
    let harness = Harness::start(behaviour).await;
    let key = format!("driver-reconcile-{}", Uuid::new_v4());
    let (status, body) = harness
        .sync_json(
            "/v1/images/generations",
            &key,
            route_request(harness.model, "driver prompt"),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "got {body}");
    assert_eq!(body["error"]["code"].as_str(), Some("outcome_unknown"));
    assert_public_only("受理状态不明", &body);

    let (job_id, state, images) = harness.job(&key).await;
    assert_eq!(
        state, "reconciliation_required",
        "a post-acceptance failure must go to reconciliation"
    );
    assert!(images.is_none(), "对账中的记录没有结果信封");
    assert_eq!(
        harness.count("POST", "/v1/images/generations"),
        1,
        "the create request must never be resent, not even for reconciliation"
    );

    let trace_id: Option<String> =
        sqlx::query_scalar("SELECT provider_trace_id FROM generation.attempts WHERE job_id = $1")
            .bind(job_id)
            .fetch_one(&harness.pool)
            .await
            .expect("attempt row");
    assert_eq!(
        trace_id.as_deref(),
        Some("task-contract-1"),
        "the upstream task id must survive into the attempt so a human can look it up"
    );

    // 而且它必须能从**对账列表接口**看到，而不是只能翻数据库（那是内部运营面）。
    let cases: Value = Client::new()
        .get(format!("{}/api/v1/reconciliation-cases", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .send()
        .await
        .expect("reconciliation cases")
        .json()
        .await
        .expect("cases JSON");
    let case = cases
        .as_array()
        .and_then(|list| list.first())
        .expect("one open reconciliation case");
    assert_eq!(
        case["provider_trace_id"].as_str(),
        Some("task-contract-1"),
        "the case list must expose the trace id, got {case}"
    );
    harness.cleanup().await;
}

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

struct DriverOutcome {
    job_state: String,
    submits: usize,
    polls: usize,
    harness: Harness,
}

/// 起 API + 假上游 + 真实 Worker，让一个文生图 Job 走完整个驱动流程，返回它的结局。
async fn run_driver_attempt(behaviour: UpstreamBehaviour) -> DriverOutcome {
    let harness = Harness::start(behaviour).await;
    let key = format!("driver-{}", Uuid::new_v4());
    let (_, _) = harness
        .sync_json(
            "/v1/images/generations",
            &key,
            route_request(harness.model, "driver prompt"),
        )
        .await;
    let (_, job_state, _) = harness.job(&key).await;
    DriverOutcome {
        job_state,
        submits: harness.count("POST", "/v1/images/generations"),
        polls: harness.count("GET", "/v1/tasks/"),
        harness,
    }
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

/// 多 Offering 路由的端到端验证。
///
/// 需要独立空库（会发布自己的候选集合）。**不启动 Worker**：路由选择发生在
/// `create_job` 之前的 API 进程内，而 `create_job` 不调用上游——因此本测试
/// **不产生任何外部调用**，同时仍能验证选中顺序与"无合格候选时零上游调用"。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn multiple_active_offerings_route_by_priority() {
    let (database_url, database_name) = isolated_database_url().await;
    // 同步入口会等到超时（没有 Worker）：给小值，别让用例白等。
    let (base_url, admin_token, _process) = start_api(&database_url, 1, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url).await;
    let account = create_account(&client, &base_url, &admin_token).await;
    let api_key = issue_key(&client, &base_url, &admin_token, &account).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    // ── 用例 1：两个都合格的候选 → 选中优先级最小的那个 ──
    let model = "route-model-a";
    let published = publish_candidates(
        &client,
        &base_url,
        &admin_token,
        model,
        None,
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
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &route_request(model, "route prompt"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::GATEWAY_TIMEOUT,
        "no worker runs, so the sync entry times out: {body}"
    );
    let (job_id, _, _) = {
        let row = sqlx::query("SELECT id FROM generation.jobs WHERE idempotency_key = $1")
            .bind(&key)
            .fetch_one(&pool)
            .await
            .expect("the request must have created a job");
        let id: Uuid = row.try_get("id").expect("job id");
        (id, (), ())
    };

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
         WHERE re.active AND re.gateway_model = $1 AND re.routing_priority = 0",
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

    // ── 任务创建失败时，判定记录与 Job 两边都不留下 ──
    // 余额不足在**受理前**拒绝：换一个余额低于服务端预授权额的账户来验（预授权额由服务端定，
    // 调用方自报不了，所以这里用"钱不够"而不是"自报一个很大的上限"）。
    let decisions_before: i64 =
        sqlx::query_scalar("SELECT count(*) FROM generation.routing_decisions")
            .fetch_one(&pool)
            .await
            .expect("decision count");
    let jobs_before: i64 = sqlx::query_scalar("SELECT count(*) FROM generation.jobs")
        .fetch_one(&pool)
        .await
        .expect("job count");
    let poor_account = create_account_with_credit(&client, &base_url, &admin_token, 1_000).await;
    let poor_key = issue_key(&client, &base_url, &admin_token, &poor_account).await;
    let (doomed, doomed_body) = post_json(
        &base_url,
        &poor_key,
        "/v1/images/generations",
        "doomed-request-0001",
        &route_request(model, "over budget"),
    )
    .await;
    assert_eq!(
        doomed,
        StatusCode::PAYMENT_REQUIRED,
        "an unaffordable request must be rejected before acceptance: {doomed_body}"
    );
    let decisions_after: i64 =
        sqlx::query_scalar("SELECT count(*) FROM generation.routing_decisions")
            .fetch_one(&pool)
            .await
            .expect("decision count");
    let jobs_after: i64 = sqlx::query_scalar("SELECT count(*) FROM generation.jobs")
        .fetch_one(&pool)
        .await
        .expect("job count");
    assert_eq!(
        (decisions_after, jobs_after),
        (decisions_before, jobs_before),
        "a rejected creation must leave neither a job nor a routing decision"
    );

    // ── 补充：幂等重放不重复写判定记录 ──
    // 同一个幂等键重放会返回同一条记录，判定记录也应只有一条（它反映"受理时"的判定）。
    let replay_key = format!("replay-{}", Uuid::new_v4());
    let replay_request = route_request(model, "replayed");
    for _ in 0..2 {
        let (status, body) = post_json(
            &base_url,
            &api_key,
            "/v1/images/generations",
            &replay_key,
            &replay_request,
        )
        .await;
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "got {body}");
    }
    let replay_job: Uuid =
        sqlx::query_scalar("SELECT id FROM generation.jobs WHERE idempotency_key = $1")
            .bind(&replay_key)
            .fetch_one(&pool)
            .await
            .expect("replayed job");
    let replay_decisions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM generation.routing_decisions WHERE job_id = $1")
            .bind(replay_job)
            .fetch_one(&pool)
            .await
            .expect("replay decision count");
    assert_eq!(
        replay_decisions, 1,
        "an idempotent replay must not write a second routing decision"
    );

    // ── 用例 3：再发布一次即原子替换该型号的全部 active 候选 ──
    let published = publish_candidates(
        &client,
        &base_url,
        &admin_token,
        model,
        None,
        vec![candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"])],
    )
    .await;
    assert_eq!(published, StatusCode::OK);
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM publication.runtime_entries WHERE active AND gateway_model = $1",
    )
    .bind(model)
    .fetch_one(&pool)
    .await
    .expect("active entries");
    assert_eq!(
        active, 1,
        "republishing must atomically replace the model's active candidates"
    );
    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// 权重与路由日志：**同一档内**按权重确定性分流，判定记录足以重建结论。
///
/// 需要独立空库（会发布自己的候选集合）。**不启动 Worker**：选路发生在 `create_job` 之前，
/// `create_job` 不调用上游——因此本用例**不产生任何外部调用**（同步入口等不到终态，按超时返回，
/// 而 Job 与判定记录都已经落库，正是这里要看的东西）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn routing_weight_splits_within_a_tier_and_is_replayable() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 1, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    // 账户用**固定 id**：分摊的输入里有账户，固定下来这批幂等键的分流结果就是确定的，
    // 用例因此可以逐条断言"落点等于按 (账户, 幂等键) 重算的结果"，而不是只能断言一个大概比例。
    let account_id = Uuid::from_u128(0x5eea_0000_0000_0000_0000_0000_0000_0002);
    sqlx::query("INSERT INTO ledger.accounts (id, balance_microusd) VALUES ($1, 100000000)")
        .bind(account_id)
        .execute(&pool)
        .await
        .expect("fixed account");
    let api_key = issue_key(&client, &base_url, &admin_token, &account_id.to_string()).await;

    // ── 用例 1：同一档两条候选，权重 1:3 ──
    let model = "weight-model-a";
    let mut light = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    light["routing_priority"] = json!(0);
    light["weight"] = json!(1);
    let mut heavy = candidate("APIMart", "apimart-image-v1", &["prompt_only"]);
    heavy["routing_priority"] = json!(0);
    heavy["weight"] = json!(3);
    assert_eq!(
        publish_candidates(
            &client,
            &base_url,
            &admin_token,
            model,
            None,
            vec![light, heavy]
        )
        .await,
        StatusCode::OK,
        "两条候选必须能落在同一档（旧唯一索引不允许，本片换掉了它）"
    );

    let tier = active_tier(&pool, model, 0).await;
    assert_eq!(tier.len(), 2, "同一档两条候选");
    let total: u64 = tier.iter().map(|(_, weight)| u64::from(*weight)).sum();
    assert_eq!(total, 4, "权重 1:3，合计 4");

    let mut chosen_by_key = Vec::new();
    for index in 0..16 {
        let key = format!("weight-split-{index}");
        let (status, body) = post_json(
            &base_url,
            &api_key,
            "/v1/images/generations",
            &key,
            &route_request(model, "weighted prompt"),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::GATEWAY_TIMEOUT,
            "没有 Worker，受理后只会等到超时：{body}"
        );
        let (chosen, considered) = routing_of(&pool, &key).await;
        assert_eq!(
            chosen,
            expected_weight_split(account_id, &key, &tier),
            "落点必须等于按 (账户, 幂等键) 与权重重算的结果：{considered:?}"
        );
        assert_eq!(considered.len(), 2, "两个候选都要进判定记录");
        let draw = considered[0]["weight_draw"]
            .as_u64()
            .expect("分流落点必须记下来");
        assert_eq!(
            draw,
            weight_draw_of(account_id, &key) % total,
            "分流落点必须等于哈希映射到该档权重之和以内的位置"
        );
        assert!(
            considered
                .iter()
                .all(|item| item["weight_draw"].as_u64() == Some(draw)),
            "分流落点是本次判定一个数，逐项同值：{considered:?}"
        );
        // 判定记录自己就能重建结论：档位、权重、落点都在里面，不需要再算一遍哈希。
        assert_eq!(
            rebuild_split_from_decision(&considered, draw),
            chosen,
            "判定记录必须足以重建选中项：{considered:?}"
        );
        for item in &considered {
            assert_eq!(item["routing_priority"], 0, "{item}");
            assert_eq!(item["eligible"], true, "{item}");
            assert!(
                matches!(item["weight"].as_u64(), Some(1) | Some(3)),
                "每条候选自己的权重也要进判定记录：{item}"
            );
        }
        chosen_by_key.push((key, chosen));
    }
    // 固定账户 + 固定幂等键 ⇒ 这是确定的结果，不是概率断言：权重 1:3 下两条候选都该被分到过。
    let distinct: std::collections::BTreeSet<Uuid> =
        chosen_by_key.iter().map(|(_, chosen)| *chosen).collect();
    assert_eq!(distinct.len(), 2, "权重 1:3 下两条候选都该被分到过");

    // ── 用例 2：同一幂等键重放 → 分到同一条候选，且去重成原 Job ──
    let (replay_key, chosen) = chosen_by_key[0].clone();
    let jobs_before: i64 = sqlx::query_scalar("SELECT count(*) FROM generation.jobs")
        .fetch_one(&pool)
        .await
        .expect("job count");
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &replay_key,
        &route_request(model, "weighted prompt"),
    )
    .await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "got {body}");
    let (replayed, considered) = routing_of(&pool, &replay_key).await;
    assert_eq!(replayed, chosen, "同一幂等键重放必须落同一条候选");
    assert_eq!(considered.len(), 2, "重放不新写判定记录");
    let jobs_after: i64 = sqlx::query_scalar("SELECT count(*) FROM generation.jobs")
        .fetch_one(&pool)
        .await
        .expect("job count");
    assert_eq!(
        jobs_after, jobs_before,
        "同一幂等键重放必须去重成原 Job，不新建、不重复计费"
    );

    // ── 用例 3：跨档时权重**不改变**档位顺序 ──
    let model = "weight-model-b";
    let mut preferred = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    preferred["routing_priority"] = json!(0);
    preferred["weight"] = json!(1);
    let mut fallback = candidate("APIMart", "apimart-image-v1", &["prompt_only"]);
    fallback["routing_priority"] = json!(1);
    fallback["weight"] = json!(1000);
    assert_eq!(
        publish_candidates(
            &client,
            &base_url,
            &admin_token,
            model,
            None,
            vec![preferred, fallback]
        )
        .await,
        StatusCode::OK
    );
    let first_tier = active_tier(&pool, model, 0).await;
    assert_eq!(first_tier.len(), 1, "档 0 只有一条候选");
    for index in 0..4 {
        let key = format!("weight-tier-{index}");
        let (status, body) = post_json(
            &base_url,
            &api_key,
            "/v1/images/generations",
            &key,
            &route_request(model, "tier prompt"),
        )
        .await;
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "got {body}");
        let (chosen, _) = routing_of(&pool, &key).await;
        assert_eq!(
            chosen, first_tier[0].0,
            "档 0 有合格候选时，档 1 的权重再大也轮不到"
        );
    }

    // ── 用例 4：全部档都不合格 → 503 platform_unavailable（不是参数错），且不留下 Job 与判定记录 ──
    //
    // 落选用**真实的能力差异**制造：两条候选的承载面都声明了参考图（合同因此允许这次请求），
    // 但各自的 `restrictions` 被收窄成只允许文生图——带图请求于是两条都不合格。
    // 不用"合同里没有 image"来制造落选：那会让请求在合同校验这一步就 400，验不到选路。
    let model = "weight-model-c";
    let mut narrow_first = candidate(
        "AIHubMix",
        "aihubmix-image-v1",
        &["prompt_only", "image_conditioned"],
    );
    narrow_first["routing_priority"] = json!(0);
    narrow_first["restrictions"] = json!({"allowed_branches": ["prompt_only"], "max_images": 0});
    let mut narrow_second = candidate(
        "AIHubMix",
        "aihubmix-image-v1",
        &["prompt_only", "image_conditioned"],
    );
    narrow_second["routing_priority"] = json!(1);
    narrow_second["restrictions"] = json!({"allowed_branches": ["prompt_only"], "max_images": 0});
    assert_eq!(
        publish_candidates(
            &client,
            &base_url,
            &admin_token,
            model,
            None,
            vec![narrow_first, narrow_second]
        )
        .await,
        StatusCode::OK
    );
    let mut request = route_request(model, "an edit neither candidate can carry");
    request["image"] = json!(png_data_url());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        "weight-ineligible-0001",
        &request,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "所有档都不合格是平台侧故障，不是参数错：{body}"
    );
    assert_eq!(
        body["error"]["code"].as_str(),
        Some("platform_unavailable"),
        "{body}"
    );
    let jobs: i64 =
        sqlx::query_scalar("SELECT count(*) FROM generation.jobs WHERE gateway_model = $1")
            .bind(model)
            .fetch_one(&pool)
            .await
            .expect("job count");
    let decisions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM generation.routing_decisions rd
         JOIN generation.jobs j ON j.id = rd.job_id WHERE j.gateway_model = $1",
    )
    .bind(model)
    .fetch_one(&pool)
    .await
    .expect("decision count");
    assert_eq!(
        (jobs, decisions),
        (0, 0),
        "全不合格必须在受理前失败：不建 Job、不写判定记录"
    );

    // ── 用例 5：同一档里不合格的那条**不进分摊**——权重写得再大也换不来一次选中 ──
    //
    // 与用例 4 的区别：这里只让**一条**候选不合格，另一条合格。若不合格的候选也参与分摊，
    // 权重 1000 会让它拿到几乎全部分流；断言"每次都落在合格那条"就是这条硬约束的证据。
    let model = "weight-model-d";
    let mut heavy_but_ineligible = candidate(
        "AIHubMix",
        "aihubmix-image-v1",
        &["prompt_only", "image_conditioned"],
    );
    heavy_but_ineligible["routing_priority"] = json!(0);
    heavy_but_ineligible["weight"] = json!(1000);
    heavy_but_ineligible["restrictions"] =
        json!({"allowed_branches": ["prompt_only"], "max_images": 0});
    let mut light_but_eligible = candidate(
        "AIHubMix",
        "aihubmix-image-v1",
        &["prompt_only", "image_conditioned"],
    );
    light_but_eligible["routing_priority"] = json!(0);
    light_but_eligible["weight"] = json!(1);
    assert_eq!(
        publish_candidates(
            &client,
            &base_url,
            &admin_token,
            model,
            None,
            vec![heavy_but_ineligible, light_but_eligible]
        )
        .await,
        StatusCode::OK
    );
    let mut request = route_request(model, "an edit only the light candidate can carry");
    request["image"] = json!(png_data_url());
    for index in 0..4 {
        let key = format!("weight-eligibility-{index}");
        let (status, body) = post_json(
            &base_url,
            &api_key,
            "/v1/images/generations",
            &key,
            &request,
        )
        .await;
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "got {body}");
        let (chosen, considered) = routing_of(&pool, &key).await;
        let eligible: Vec<Uuid> = considered
            .iter()
            .filter(|item| item["eligible"] == true)
            .map(|item| {
                Uuid::parse_str(item["offering_id"].as_str().expect("offering id")).expect("uuid")
            })
            .collect();
        assert_eq!(eligible.len(), 1, "只有一条候选合格：{considered:?}");
        assert_eq!(
            chosen, eligible[0],
            "不合格的候选不得因为权重大而被选中：{considered:?}"
        );
        let skipped = considered
            .iter()
            .find(|item| item["eligible"] == false)
            .expect("the heavy candidate must be recorded as ineligible");
        assert_eq!(skipped["weight"], 1000, "{skipped}");
        assert!(
            skipped["skip_reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("branch") || reason.contains("image")),
            "落选原因要写清楚：{skipped}"
        );
    }

    // ── 用例 6：权重 0 与负档位在发布期就被拒（不是库层约束错，也不是"分不到"） ──
    let mut zero_weight = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    zero_weight["weight"] = json!(0);
    let status = publish_candidates(
        &client,
        &base_url,
        &admin_token,
        "weight-model-invalid",
        None,
        vec![zero_weight],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "权重 0 必须在发布期被拒");

    let mut negative_priority = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    negative_priority["routing_priority"] = json!(-1);
    let status = publish_candidates(
        &client,
        &base_url,
        &admin_token,
        "weight-model-invalid",
        None,
        vec![negative_priority],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "负档位必须在发布期被拒");

    // ── 用例 7：管理员读接口列出权重 ──
    let response = client
        .get(format!("{base_url}/api/v1/gateway-models"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("gateway model list");
    assert_eq!(response.status(), StatusCode::OK);
    let listed: Value = response.json().await.expect("gateway model JSON");
    let entry = listed["gateway_models"]
        .as_array()
        .expect("gateway_models")
        .iter()
        .find(|entry| entry["gateway_model"].as_str() == Some("weight-model-a"))
        .expect("weight-model-a must be listed");
    let mut weights: Vec<(String, u64)> = entry["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .map(|candidate| {
            (
                candidate["provider_kind"]
                    .as_str()
                    .expect("provider kind")
                    .to_owned(),
                candidate["weight"].as_u64().expect("weight"),
            )
        })
        .collect();
    // 档内按 `offering_id` 定序，而 offering_id 每次发布都是新的：这里比的是**集合**，
    // 顺序由上面那条选路用例负责（它按同一个定序重算落点）。
    weights.sort();
    assert_eq!(
        weights,
        vec![("AIHubMix".to_owned(), 1), ("APIMart".to_owned(), 3)],
        "管理员读接口要列出每个候选的权重：{entry}"
    );

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// 同一个网关模型的**并发发布**：替换必须真的是一次替换——两份修订的 active 条目不得并存。
///
/// 为什么单独验这一条：唯一索引从"每型号每档一条"换成"每型号每条供给一行"之后，它不再能拦住
/// "两份修订同时生效"。而"active 候选跨修订并存"是**读时**才会暴露的问题：表现是这个型号的
/// 所有请求一起失败（平台侧故障），直到有人重新发布一次。发布事务按名字取事务级咨询锁就是为了
/// 让这件事不可能发生——没有那把锁时，两条并发发布的"先失效、后插入"会交错。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn concurrent_publications_of_one_gateway_model_leave_a_single_active_revision() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 1, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");
    let account = create_account(&client, &base_url, &admin_token).await;
    let api_key = issue_key(&client, &base_url, &admin_token, &account).await;

    let model = "concurrent-publish-model";
    let mut tasks = Vec::new();
    for index in 0..6_u64 {
        let mut offering = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
        offering["routing_priority"] = json!(index % 2);
        offering["weight"] = json!(index + 1);
        let body = publication_body(model, "route-test-1", None, vec![offering], None);
        let client = client.clone();
        let url = format!("{base_url}/api/v1/runtime-revisions");
        let token = admin_token.clone();
        tasks.push(tokio::spawn(async move {
            client
                .post(url)
                .bearer_auth(token)
                .json(&body)
                .send()
                .await
                .expect("concurrent publication")
                .status()
        }));
    }
    for task in tasks {
        let status = task.await.expect("publication task must not panic");
        assert_eq!(status, StatusCode::OK, "并发发布各自都该成功");
    }

    let revisions: i64 = sqlx::query_scalar(
        "SELECT count(DISTINCT runtime_revision_id) FROM publication.runtime_entries
         WHERE active AND gateway_model = $1",
    )
    .bind(model)
    .fetch_one(&pool)
    .await
    .expect("active revisions");
    assert_eq!(
        revisions, 1,
        "同一名字的 active 条目必须来自同一次发布（并发发布不得交错）"
    );

    // 受理照常：跨修订并存会让这个型号的所有请求一起失败。
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        "concurrent-publish-0001",
        &route_request(model, "concurrent publish"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::GATEWAY_TIMEOUT,
        "并发发布之后这个型号仍要能受理（没有 Worker，只会等到超时）：{body}"
    );

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// 某一档上的 active 候选：`(offering_id, weight)`，按 `offering_id` 定序。
///
/// 定序与受理侧一致——档内分摊的区间划分就按这个顺序，用例重算落点时必须用同一个顺序。
async fn active_tier(pool: &PgPool, model: &str, routing_priority: i32) -> Vec<(Uuid, u32)> {
    let rows = sqlx::query(
        "SELECT offering_id, weight FROM publication.runtime_entries
         WHERE active AND gateway_model = $1 AND routing_priority = $2
         ORDER BY offering_id ASC",
    )
    .bind(model)
    .bind(routing_priority)
    .fetch_all(pool)
    .await
    .expect("active candidates of the tier");
    rows.iter()
        .map(|row| {
            (
                row.try_get("offering_id").expect("offering id"),
                u32::try_from(row.try_get::<i32, _>("weight").expect("weight"))
                    .expect("positive weight"),
            )
        })
        .collect()
}

/// 按 `(账户, 幂等键)` 与权重**重算**落点。
///
/// 故意在用例里独立写一遍（不调用服务端的实现）：要证明的是"分流由账户、幂等键与权重共同决定"，
/// 用被验对象自己算期望就什么也证明不了。
fn weight_draw_of(account_id: Uuid, key: &str) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(account_id.as_bytes());
    hasher.update(key.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(bytes)
}

/// 重算期望的选中候选：落点对**该档**权重之和取模，再按 `offering_id` 升序走区间。
fn expected_weight_split(account_id: Uuid, key: &str, tier: &[(Uuid, u32)]) -> Uuid {
    let total: u64 = tier.iter().map(|(_, weight)| u64::from(*weight)).sum();
    let draw = weight_draw_of(account_id, key) % total;
    let mut cursor = 0_u64;
    for (offering_id, weight) in tier {
        cursor += u64::from(*weight);
        if draw < cursor {
            return *offering_id;
        }
    }
    unreachable!("落点必然落在某条候选的区间里")
}

/// 只用**判定记录**重建选中项：合格候选、档位、权重与落点都在里面。
fn rebuild_split_from_decision(considered: &[Value], draw: u64) -> Uuid {
    let tier = considered
        .iter()
        .filter(|item| item["eligible"] == true)
        .map(|item| item["routing_priority"].as_i64().expect("priority"))
        .min()
        .expect("a decision always has an eligible candidate");
    let mut ordered: Vec<(Uuid, u64)> = considered
        .iter()
        .filter(|item| item["eligible"] == true && item["routing_priority"].as_i64() == Some(tier))
        .map(|item| {
            (
                Uuid::parse_str(item["offering_id"].as_str().expect("offering id"))
                    .expect("offering id is a uuid"),
                item["weight"].as_u64().expect("weight"),
            )
        })
        .collect();
    ordered.sort_by_key(|(offering_id, _)| *offering_id);
    let mut cursor = 0_u64;
    for (offering_id, weight) in ordered {
        cursor += weight;
        if draw < cursor {
            return offering_id;
        }
    }
    unreachable!("落点必然落在某条候选的区间里")
}

/// 第二阶段的**发布素材**要真的能用：一个 Vendor Model 一份文件、只落**一份合同**，
/// 而每个候选各带**自己的承载面**——缺一不可：素材发不出去、或候选没带上自己的承载面，
/// 都算没覆盖。
///
/// 素材本身就是完整的发布命令（顶层一份合同 + 两条供给），所以直接按它发布：
/// AIHubMix 下标 0（收 `image` / `mask`），APIMart 下标 1（收 `image_urls` / `mask_url`），
/// 两家能承载的字段面不同，正是"合同一份、承载面各一份"要覆盖的情形。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn stage_two_bootstrap_material_publishes_one_contract_with_per_candidate_carriers() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    let material: Value = serde_json::from_str(include_str!(
        "../../../config/bootstrap/gpt-image-2.5-flare.json"
    ))
    .expect("2.5 material parses");
    let model = material["native_model_id"]
        .as_str()
        .expect("native model id")
        .to_owned();
    // 路由条目挂的是**平台对客名**：合同按厂商原生名落行，候选集按对客名生效。种子素材不写
    // 这个字段，按发布期的回退规则取厂商原生名；平台自命名的例子见命名层那条用例。
    let gateway = material["gateway_model"]
        .as_str()
        .unwrap_or(&model)
        .to_owned();
    let revision = material["native_revision"]
        .as_str()
        .expect("native revision")
        .to_owned();
    let offerings = material["offerings"]
        .as_array()
        .expect("offerings must be an array")
        .clone();
    assert_eq!(offerings.len(), 2, "一份素材两条供给");
    assert_eq!(
        offerings[0]["provider_kind"], "AIHubMix",
        "下标 0 是首选：AIHubMix"
    );
    assert_eq!(
        offerings[1]["provider_kind"], "APIMart",
        "下标 1 是次选：APIMart"
    );
    // 两家能承载的面必须真的不同——否则这个用例覆盖不到"承载面各自一份"。
    let carriers = [
        offerings[0]["carrier_schema"].clone(),
        offerings[1]["carrier_schema"].clone(),
    ];
    assert_ne!(
        carriers[0], carriers[1],
        "this test only covers the split surface if the two carriers actually differ"
    );

    let published = client
        .post(format!("{base_url}/api/v1/runtime-revisions"))
        .bearer_auth(&admin_token)
        .json(&material)
        .send()
        .await
        .expect("publication request");
    assert_eq!(
        published.status(),
        StatusCode::OK,
        "the prepared 2.5 material must be publishable: {:?}",
        published.text().await
    );

    // 合同是**模型级唯一一份**：同一个型号的这一版只落一行，内容就是顶层那一份。
    let contracts = sqlx::query(
        "SELECT id, capability_schema FROM catalog.vendor_models
         WHERE vendor_id = $1 AND native_model_id = $2 AND native_revision = $3",
    )
    .bind(material["vendor_id"].as_str().expect("vendor id"))
    .bind(&model)
    .bind(&revision)
    .fetch_all(&pool)
    .await
    .expect("contract rows");
    assert_eq!(contracts.len(), 1, "one contract per vendor model revision");
    let stored_contract: Value = contracts[0].try_get("capability_schema").expect("contract");
    assert_eq!(
        stored_contract, material["capability_schema"],
        "the stored contract must be the one given at the top level"
    );

    // 每个候选携带**它自己**的承载面，而合同只读那一份。
    let rows = sqlx::query(
        "SELECT o.provider_model_id, o.adapter_key, o.carrier_schema, o.parameter_mapping,
                c.provider_kind, vm.capability_schema
         FROM publication.runtime_entries re
         JOIN supply.offerings o ON o.id = re.offering_id
         JOIN supply.channels c ON c.id = o.channel_id
         JOIN catalog.vendor_models vm ON vm.id = re.vendor_model_id
         WHERE re.active AND re.gateway_model = $1
         ORDER BY re.routing_priority",
    )
    .bind(&gateway)
    .fetch_all(&pool)
    .await
    .expect("candidate rows");
    assert_eq!(rows.len(), 2, "both providers must be active candidates");

    let stored_carriers: Vec<Value> = rows
        .iter()
        .map(|row| row.try_get("carrier_schema").expect("carrier"))
        .collect();
    assert_ne!(
        stored_carriers[0], stored_carriers[1],
        "each candidate must carry its own surface, not a shared one"
    );
    for (index, offering) in offerings.iter().enumerate() {
        let row = &rows[index];
        let provider_model_id: String = row.try_get("provider_model_id").expect("provider model");
        let adapter_key: String = row.try_get("adapter_key").expect("adapter key");
        assert_eq!(provider_model_id, offering["provider_model_id"]);
        assert_eq!(adapter_key, offering["adapter_key"]);
        assert_eq!(
            stored_carriers[index], offering["carrier_schema"],
            "candidate {index} must carry its own surface"
        );
        assert_eq!(
            stored_carriers[index], carriers[index],
            "候选带上线的承载面必须逐字就是素材里那一份"
        );
        // 每个候选读到的合同都是同一份，且它的 `model.const` 就是该型号。
        let contract: Value = row.try_get("capability_schema").expect("contract");
        assert_eq!(contract, stored_contract);
        assert_eq!(contract["properties"]["model"]["const"], model);
        // 承载面的每个字段名都要**从合同可达**（R1 的判据，这里独立复核一遍）：要么合同直接声明，
        // 要么被 `rename` 接过去——线上名不必等于合同名（APIMart 的 `image_urls` 就是这么来的）。
        let mapping: Value = row.try_get("parameter_mapping").expect("mapping");
        let wires: Vec<Value> = mapping["rename"]
            .as_object()
            .map(|renames| renames.values().cloned().collect())
            .unwrap_or_default();
        for name in stored_carriers[index]["properties"]
            .as_object()
            .expect("carrier properties")
            .keys()
        {
            let declared = contract["properties"]
                .as_object()
                .expect("contract properties")
                .contains_key(name);
            let renamed = wires
                .iter()
                .any(|wire| wire.as_str() == Some(name.as_str()));
            assert!(
                declared || renamed,
                "carrier field {name} must be reachable from the contract"
            );
        }
    }

    // 快照指纹跟着"合同 + 承载面"走：两个候选承载面不同，指纹就该不同。
    let snapshot: Value = sqlx::query_scalar(
        "SELECT snapshot FROM publication.runtime_revisions rr
         JOIN publication.runtime_entries re ON re.runtime_revision_id = rr.id
         WHERE re.active AND re.gateway_model = $1 LIMIT 1",
    )
    .bind(&gateway)
    .fetch_one(&pool)
    .await
    .expect("runtime revision snapshot");
    let candidates = snapshot["candidates"]
        .as_array()
        .expect("snapshot candidates");
    assert_eq!(candidates.len(), 2);
    for candidate in candidates {
        assert!(
            candidate.get("schema_hash").is_none(),
            "快照指纹必须覆盖合同与承载面，不再是只有一份 schema 的哈希"
        );
        assert!(candidate["contract_carrier_hash"].is_string());
    }
    assert_ne!(
        candidates[0]["contract_carrier_hash"], candidates[1]["contract_carrier_hash"],
        "different carriers must produce different snapshot fingerprints"
    );

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// 仍留在 `config/bootstrap/` 的那一份**旧形状**素材（offering 级 `capability_schema`，没有顶层
/// 合同、也没有 `carrier_schema`）照常能发布：过渡期里合同与承载面都回退到那一份声明面。
///
/// 这是**唯一**还按旧形状读的素材，留着就是为了这条语义——新形状的素材走的是上面那些用例，
/// 旧形状的可发布性没有别的证据可依。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn legacy_aihubmix_material_still_publishes() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    // 只留这一份：它是仓库里唯一还带 offering 级 `capability_schema` 的素材，
    // 也是这条"旧形状照常可发布"语义的唯一夹具。
    let material = include_str!("../../../config/bootstrap/aihubmix-gpt-image-2.json");
    let command: Value = serde_json::from_str(material).expect("material parses");
    let model = command["native_model_id"]
        .as_str()
        .expect("native model id")
        .to_owned();
    let response = client
        .post(format!("{base_url}/api/v1/runtime-revisions"))
        .bearer_auth(&admin_token)
        .json(&command)
        .send()
        .await
        .expect("publication request");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "旧形状素材必须照常可发布：{model}"
    );
    // 回退的结果：合同就是那份声明面，承载面与它同值，同一个型号只落一行。
    let rows = sqlx::query(
        "SELECT vm.capability_schema, o.carrier_schema
         FROM catalog.vendor_models vm
         JOIN supply.offerings o ON o.vendor_model_id = vm.id
         WHERE vm.native_model_id = $1",
    )
    .bind(&model)
    .fetch_all(&pool)
    .await
    .expect("contract rows");
    assert_eq!(rows.len(), 1, "{model} 只能落一行合同");
    let contract: Value = rows[0].try_get("capability_schema").expect("contract");
    let carrier: Value = rows[0].try_get("carrier_schema").expect("carrier");
    assert_eq!(contract, command["offerings"][0]["capability_schema"]);
    assert_eq!(carrier, contract);

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// 承载面 ⊆ 合同（R1）：供给不能凭空多出调用方可提交的字段。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn carrier_field_outside_the_contract_is_rejected() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url).await;

    let model = "contract-boundary-model";
    let contract = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1}
    }));
    // 承载面多声明了 `quality`：合同里没有它，客户端按合同提交永远不会发这个名字。
    let carrier = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "quality": {"type": "string", "enum": ["low", "high"]}
    }));
    let status = publish_with_surfaces(
        &client,
        &base_url,
        &admin_token,
        model,
        "contract-boundary-1",
        contract,
        vec![("aihubmix-image-v1", carrier)],
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a carrier field the contract does not declare must be rejected"
    );

    // 把 `quality` 补进合同后同一份承载面就能发布：拒绝的是那条边界，不是 `quality` 本身。
    let contract = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "quality": {"type": "string", "enum": ["low", "high"]}
    }));
    let carrier = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "quality": {"type": "string", "enum": ["low", "high"]}
    }));
    let status = publish_with_surfaces(
        &client,
        &base_url,
        &admin_token,
        model,
        "contract-boundary-2",
        contract,
        vec![("aihubmix-image-v1", carrier)],
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    drop_isolated_database(&database_name).await;
}

/// 承载面 ⊆ Driver 能写上线文的字段名（R2）：声明了发不出去的字段就拒绝。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn carrier_field_the_driver_cannot_write_is_rejected() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url).await;

    let model = "driver-boundary-model";
    // `resolution` 是 APIMart 那一侧的渠道字段名，AIHubMix 的 Driver 写不出去。
    let contract = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "resolution": {"type": "string", "enum": ["1k", "2k"]}
    }));
    let status = publish_with_surfaces(
        &client,
        &base_url,
        &admin_token,
        model,
        "driver-boundary-1",
        contract.clone(),
        vec![("aihubmix-image-v1", contract.clone())],
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a carrier field the driver cannot write must be rejected"
    );

    // 同一份声明面挂到能写 `resolution` 的 Driver 上就能发布：判的是"发得出去吗"。
    let status = publish_with_surfaces(
        &client,
        &base_url,
        &admin_token,
        model,
        "driver-boundary-2",
        contract.clone(),
        vec![("apimart-image-v1", contract)],
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    drop_isolated_database(&database_name).await;
}

/// 合同行不可变：同一 (vendor, model, revision) 重发幂等、不就地改写；
/// 内容不同的重发要拒绝；新修订才落新行。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn contract_rows_are_immutable_and_republishing_the_same_revision_is_idempotent() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    let model = "immutable-contract-model";
    let contract = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1}
    }));
    let publish = |revision: &'static str, contract: Value| {
        let client = client.clone();
        let base_url = base_url.clone();
        let admin_token = admin_token.clone();
        async move {
            publish_with_surfaces(
                &client,
                &base_url,
                &admin_token,
                model,
                revision,
                contract.clone(),
                vec![("aihubmix-image-v1", contract)],
            )
            .await
        }
    };

    assert_eq!(
        publish("immutable-1", contract.clone()).await,
        StatusCode::OK
    );
    let first = contract_row(&pool, model, "immutable-1").await;
    // 同一修订重发（内容相同）：幂等——还是那一行，且**没有**被改写（时间戳与内容都不变）。
    assert_eq!(
        publish("immutable-1", contract.clone()).await,
        StatusCode::OK
    );
    let again = contract_row(&pool, model, "immutable-1").await;
    assert_eq!(again.0, first.0, "a republish must not create a second row");
    assert_eq!(
        again.1, first.1,
        "a republish must not rewrite the contract"
    );
    assert_eq!(
        again.2, first.2,
        "a republish must not touch the row at all"
    );

    // 同一修订换个合同：拒绝。合同落库后不可改，改合同要发新修订。
    let changed = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "quality": {"type": "string"}
    }));
    assert_eq!(
        publish("immutable-1", changed).await,
        StatusCode::BAD_REQUEST,
        "the same revision must not accept a different contract"
    );
    let after = contract_row(&pool, model, "immutable-1").await;
    assert_eq!(
        after, first,
        "a rejected republish must leave the row untouched"
    );

    // 新修订落新行：同一个模型可以有多版合同，但每一版只有一份。
    assert_eq!(
        publish("immutable-2", contract.clone()).await,
        StatusCode::OK
    );
    let revisions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM catalog.vendor_models WHERE native_model_id = $1")
            .bind(model)
            .fetch_one(&pool)
            .await
            .expect("contract revision count");
    assert_eq!(revisions, 2, "a new revision is a new contract row");

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// 承载面随 Job 冻结：发布换了承载面之后，旧 Job 读到的仍是它受理时那一份。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn carrier_surface_is_frozen_into_the_job() {
    let (database_url, database_name) = isolated_database_url().await;
    // 同步入口会等到超时（没有 Worker）：给小值，别让用例白等。
    let (base_url, admin_token, _process) = start_api(&database_url, 1, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url).await;
    let account = create_account(&client, &base_url, &admin_token).await;
    let api_key = issue_key(&client, &base_url, &admin_token, &account).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    let model = "frozen-carrier-model";
    let contract = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "quality": {"type": "string", "enum": ["low", "high"]}
    }));
    // 受理时这条供给能承载 `quality`。
    let accepted_carrier = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "quality": {"type": "string", "enum": ["low", "high"]}
    }));
    let status = publish_with_surfaces(
        &client,
        &base_url,
        &admin_token,
        model,
        "frozen-1",
        contract.clone(),
        vec![("aihubmix-image-v1", accepted_carrier.clone())],
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let key = format!("frozen-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &route_request(model, "frozen carrier"),
    )
    .await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "got {body}");
    let job_id: Uuid =
        sqlx::query_scalar("SELECT id FROM generation.jobs WHERE idempotency_key = $1")
            .bind(&key)
            .fetch_one(&pool)
            .await
            .expect("the request must have created a job");

    // 换一版发布：这条供给**不再**承载 `quality`（收窄了承载面）。
    let narrowed_carrier = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1}
    }));
    let status = publish_with_surfaces(
        &client,
        &base_url,
        &admin_token,
        model,
        "frozen-2",
        contract.clone(),
        vec![("aihubmix-image-v1", narrowed_carrier.clone())],
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // 走真实的读取路径取这台 Job：它读到的承载面仍是受理时那一份。
    let repository = PgHubRepository::connect(&database_url, 2)
        .await
        .expect("repository");
    let claimed = repository
        .claim_next_job("frozen-carrier-worker", chrono::Duration::seconds(30))
        .await
        .expect("claim")
        .expect("the accepted job must be claimable");
    assert_eq!(claimed.job.id.0, job_id);
    assert_eq!(
        claimed.job.offering.carrier_schema, accepted_carrier,
        "the job must keep the carrier surface it was accepted with"
    );
    assert_eq!(claimed.job.offering.capability_schema, contract);

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// 请求**用到的**字段落在合同里、但某条候选的承载面承载不了：该候选落选、换下一条。
///
/// 一条都承载不了时是**平台侧供给问题**：对客必须是平台侧故障（503），不是消费者的参数错（400）。
/// 请求本身违反合同（缺必填）仍然是 400——两者不能混成同一个码。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_field_a_carrier_cannot_carry_skips_it_and_fails_platform_side_when_none_can() {
    let (database_url, database_name) = isolated_database_url().await;
    // 没有 Worker：同步入口只会等到超时，正好用来只看"受理与选路"。
    let (base_url, admin_token, _process) = start_api(&database_url, 1, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url).await;
    let account = create_account(&client, &base_url, &admin_token).await;
    let api_key = issue_key(&client, &base_url, &admin_token, &account).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    let model = "carry-boundary-model";
    // 合同声明了 `quality`（调用方能提交它），但只有一条供给承载得了。
    let contract = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "quality": {"type": "string", "enum": ["low", "high"]}
    }));
    let narrow = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1}
    }));
    let wide = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "quality": {"type": "string", "enum": ["low", "high"]}
    }));

    // ── 用例 1：优先级 0 的候选承载不了 → 落到优先级 1 的候选，判定记录写明原因 ──
    let status = publish_with_surfaces(
        &client,
        &base_url,
        &admin_token,
        model,
        "carry-1",
        contract.clone(),
        vec![
            ("aihubmix-image-v1", narrow.clone()),
            ("apimart-image-v1", wide.clone()),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "两条候选各带自己的承载面");

    let mut request = route_request(model, "carry this");
    request["quality"] = json!("high");
    let key = format!("carry-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::GATEWAY_TIMEOUT,
        "没有 Worker，受理后只会等到超时：{body}"
    );
    let job_id: Uuid =
        sqlx::query_scalar("SELECT id FROM generation.jobs WHERE idempotency_key = $1")
            .bind(&key)
            .fetch_one(&pool)
            .await
            .expect("the request must have been accepted");
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
    let considered = considered.as_array().expect("considered is an array");
    assert_eq!(considered.len(), 2, "两个候选都要进判定记录");
    assert_eq!(considered[0]["eligible"], false);
    assert!(
        considered[0]["skip_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("quality")),
        "落选原因必须写明承载不了哪个字段：{considered:?}"
    );
    assert_eq!(considered[1]["eligible"], true);
    let expected: Uuid = sqlx::query_scalar(
        "SELECT re.offering_id FROM publication.runtime_entries re
         WHERE re.active AND re.gateway_model = $1 AND re.routing_priority = 1",
    )
    .bind(model)
    .fetch_one(&pool)
    .await
    .expect("priority 1 offering");
    assert_eq!(
        chosen, expected,
        "第一条承载不了请求用到的字段，就该落到下一条"
    );

    // ── 用例 2：同一个型号只留承载面窄的那条 → 一条候选都不合格 ──
    let status = publish_with_surfaces(
        &client,
        &base_url,
        &admin_token,
        model,
        "carry-2",
        contract.clone(),
        vec![("aihubmix-image-v1", narrow.clone())],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let jobs_before: i64 = sqlx::query_scalar("SELECT count(*) FROM generation.jobs")
        .fetch_one(&pool)
        .await
        .expect("job count");
    let key = format!("carry-none-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_ne!(
        status,
        StatusCode::BAD_REQUEST,
        "平台承载不了不是消费者的参数错：{body}"
    );
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "平台侧供给问题必须说成平台侧故障：{body}"
    );
    assert_eq!(body["error"]["code"].as_str(), Some("platform_unavailable"));
    assert_public_only("无可用供给", &body);
    let jobs_after: i64 = sqlx::query_scalar("SELECT count(*) FROM generation.jobs")
        .fetch_one(&pool)
        .await
        .expect("job count");
    assert_eq!(
        jobs_after, jobs_before,
        "承载不了是在受理前失败的，不该留下执行记录"
    );

    // ── 用例 3：同一条供给、调用方**没用到** `quality` → 照常受理 ──
    let key = format!("carry-unused-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &route_request(model, "no quality given"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::GATEWAY_TIMEOUT,
        "没用到那个字段就该照常受理：{body}"
    );

    // ── 用例 3b：**合同里没有**的字段照旧丢掉、请求照常受理 ──
    // 与上面"合同有、承载面没有"的处置必须分开：前者丢掉不报错，后者是平台侧故障。
    // 所以判据面真的是合同，而不是这条窄承载面——合同外的字段连承载校验都进不去。
    let key = format!("carry-unknown-{}", Uuid::new_v4());
    let mut request = route_request(model, "a field the contract never declared");
    request["seed"] = json!(7);
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::GATEWAY_TIMEOUT,
        "合同外的字段该丢掉、请求照常受理：{body}"
    );
    let stored: Value = sqlx::query_scalar(
        "SELECT native_parameters FROM generation.jobs WHERE idempotency_key = $1",
    )
    .bind(&key)
    .fetch_one(&pool)
    .await
    .expect("the accepted job must keep its parameters");
    assert!(
        stored.get("seed").is_none(),
        "合同外的字段不许跟着 Job 走去上游：{stored}"
    );

    // ── 用例 4：请求本身违反合同（缺必填）仍然是 400 ──
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        "carry-missing-prompt-0001",
        &json!({"model": model, "quality": "high"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert_eq!(body["error"]["code"].as_str(), Some("validation_error"));
    assert_public_only("缺必填项", &body);

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// 映射声明的**显式默认值**必须出现在发给上游的报文里：调用方没给该字段时由平台补上，
/// 调用方给了就一个字都不改——渠道自己那套默认值（例如上游把水印默认打开）因此再也用不上。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn explicit_defaults_reach_the_upstream_request_body() {
    // 素材形状照旧（AIHubMix 声明得了 `quality`），只是这条供给挂了一份显式默认值：
    // 调用方不给 `quality` 时，平台自己发一个 `low`，而不是让上游按它的默认值走。
    let mut draft = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    draft["capability_schema"]["properties"]["quality"] =
        json!({"type": "string", "enum": ["low", "high"]});
    draft["parameter_mapping"] = json!({"defaults": {"quality": "low"}});
    let harness = Harness::start_with_draft(
        draft,
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;

    // 1) 调用方没给 `quality`：默认值跟着报文上行，也留在内部参数面里。
    let key = format!("defaults-{}", Uuid::new_v4());
    let request = route_request(harness.model, "no quality given");
    let (status, body) = harness
        .sync_json("/v1/images/generations", &key, request.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("显式默认值", &body);
    let submit_body = harness.submit_body("/v1/images/generations");
    assert_eq!(
        submit_body["quality"], "low",
        "默认值必须出现在上游报文里：{submit_body}"
    );
    harness.assert_only_declared_fields(&request);
    let stored: Value = sqlx::query_scalar(
        "SELECT native_parameters FROM generation.jobs WHERE idempotency_key = $1",
    )
    .bind(&key)
    .fetch_one(&harness.pool)
    .await
    .expect("native parameters");
    assert_eq!(
        stored["quality"], "low",
        "Job 里存的就是这次真正发出去的东西：{stored}"
    );
    assert_job_succeeded(&harness, &key).await;

    // 2) 调用方给了：用调用方的值，不被默认值覆盖。
    let key = format!("defaults-given-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "quality given");
    request["quality"] = json!("high");
    let (status, body) = harness
        .sync_json("/v1/images/generations", &key, request.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let submit_body = harness.submit_body("/v1/images/generations");
    assert_eq!(
        submit_body["quality"], "high",
        "调用方给了就用调用方的值：{submit_body}"
    );
    harness.assert_only_declared_fields(&request);
    assert_job_succeeded(&harness, &key).await;
    harness.cleanup().await;
}

/// Seedream 5.0 lite 的 2K 档映射表：厂商文档里"分辨率档位 × 宽高比 → 宽高像素值"的那张表。
///
/// 它出现在用例里只是**发布数据**：平台代码里没有任何厂商的档位表，档案随发布携带，接新厂商
/// 只发一份新档案。
fn lite_2k_profile() -> Value {
    json!({
        "2K": {
            "1:1": "2048x2048",
            "4:3": "2304x1728",
            "16:9": "2848x1600",
            "3:2": "2496x1664",
            "2:3": "1664x2496",
            "21:9": "3136x1344"
        }
    })
}

/// 合同给"比例 + 档位"、供给要像素：平台在**组装期**查档案换算，线上那个字段是换算后的像素值。
///
/// 这条供给是像素面渠道（线上根本没有 `resolution` 这个名字），它承载得了这次请求全靠映射里
/// 那份尺寸声明：`resolution` 是换算的输入，不是要原样上行的字段。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_size_conversion_reaches_the_upstream_request_body() {
    let mut draft = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    // 合同（模型级）：调用方可以给比例与档位两个字段。
    draft["capability_schema"] = surface_schema(json!({
        "model": {"const": Harness::MODEL},
        "prompt": {"type": "string", "minLength": 1},
        "size": {"type": "string"},
        "resolution": {"type": "string"}
    }));
    // 承载面：这条供给只往线上写 `size` 一个尺寸字段。
    draft["carrier_schema"] = surface_schema(json!({
        "model": {"const": Harness::MODEL},
        "prompt": {"type": "string", "minLength": 1},
        "size": {"type": "string"}
    }));
    draft["parameter_mapping"] = json!({
        "size": {
            "source": ["size", "resolution"],
            "target": "size",
            "form": "pixels",
            "profile": lite_2k_profile()
        }
    });
    let harness = Harness::start_with_draft(
        draft,
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;

    let key = format!("size-converted-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "a 2:3 poster at 2K");
    request["size"] = json!("2:3");
    request["resolution"] = json!("2K");
    let (status, body) = harness
        .sync_json("/v1/images/generations", &key, request.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("尺寸换算", &body);

    let submit_body = harness.submit_body("/v1/images/generations");
    assert_eq!(
        submit_body["size"], "1664x2496",
        "线上那个字段必须是换算后的像素值：{submit_body}"
    );
    assert!(
        submit_body.get("resolution").is_none(),
        "换算的输入字段不再原样上行：{submit_body}"
    );
    harness.assert_only_declared_fields(&request);
    // 内部 Job 里存的就是这次真正发出去的东西：Driver 只看到渠道要的形态。
    let stored: Value = sqlx::query_scalar(
        "SELECT native_parameters FROM generation.jobs WHERE idempotency_key = $1",
    )
    .bind(&key)
    .fetch_one(&harness.pool)
    .await
    .expect("native parameters");
    assert_eq!(stored["size"], "1664x2496", "{stored}");
    assert!(stored.get("resolution").is_none(), "{stored}");
    assert_job_succeeded(&harness, &key).await;
    harness.cleanup().await;
}

/// 档案里缺那一格：这条候选**不合格**（原因写进判定记录），有别的候选就落到它，没有就 503。
///
/// 换算不出不是"参数错"：请求本身完全符合合同（比例与档位都给了），是这条供给的档案里没有
/// 3K 那一格。因此对客只能是平台侧故障，绝不退回调用方给的原值、也绝不猜一个近似值。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_size_combination_the_profile_lacks_makes_the_candidate_ineligible() {
    let (database_url, database_name) = isolated_database_url().await;
    // 没有 Worker：同步入口只会等到超时，正好用来只看"受理与选路"。
    let (base_url, admin_token, _process) = start_api(&database_url, 1, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url).await;
    let account = create_account(&client, &base_url, &admin_token).await;
    let api_key = issue_key(&client, &base_url, &admin_token, &account).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    let model = "size-profile-model";
    let contract = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "size": {"type": "string"},
        "resolution": {"type": "string"}
    }));
    // 像素面供给：档案里只有 2K 那一档。
    let pixel_carrier = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "size": {"type": "string"}
    }));
    let pixel_mapping = json!({
        "size": {
            "source": ["size", "resolution"],
            "target": "size",
            "form": "pixels",
            "profile": lite_2k_profile()
        }
    });
    // 比例 + 档位面供给：两个字段原样承载，不做换算。
    let ratio_carrier = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "size": {"type": "string"},
        "resolution": {"type": "string"}
    }));

    // ── 用例 1：优先级 0 的候选档案里没有 3K → 落到优先级 1 的候选，判定记录写明原因 ──
    let status = publish_with_mappings(
        &client,
        &base_url,
        &admin_token,
        model,
        "size-1",
        contract.clone(),
        vec![
            (
                "aihubmix-image-v1",
                pixel_carrier.clone(),
                pixel_mapping.clone(),
            ),
            ("apimart-image-v1", ratio_carrier.clone(), json!({})),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "两条候选各带自己的尺寸声明");

    let mut request = route_request(model, "a 2:3 poster at 3K");
    request["size"] = json!("2:3");
    request["resolution"] = json!("3K");
    let key = format!("size-skip-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::GATEWAY_TIMEOUT,
        "换算不出只是这条候选不合格，下一条照常受理：{body}"
    );
    let job_id: Uuid =
        sqlx::query_scalar("SELECT id FROM generation.jobs WHERE idempotency_key = $1")
            .bind(&key)
            .fetch_one(&pool)
            .await
            .expect("the request must have been accepted");
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
    let considered = considered.as_array().expect("considered is an array");
    assert_eq!(considered.len(), 2, "两个候选都要进判定记录");
    assert_eq!(considered[0]["eligible"], false);
    assert!(
        considered[0]["skip_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("2:3") && reason.contains("3K")),
        "落选原因必须写明档案缺哪一格：{considered:?}"
    );
    assert_eq!(considered[1]["eligible"], true);
    let expected: Uuid = sqlx::query_scalar(
        "SELECT re.offering_id FROM publication.runtime_entries re
         WHERE re.active AND re.gateway_model = $1 AND re.routing_priority = 1",
    )
    .bind(model)
    .fetch_one(&pool)
    .await
    .expect("priority 1 offering");
    assert_eq!(chosen, expected, "第一条换算不出，就该落到下一条");

    // ── 用例 2：同一个型号只留像素面那条 → 一条候选都不合格 → 平台侧故障 ──
    let status = publish_with_mappings(
        &client,
        &base_url,
        &admin_token,
        model,
        "size-2",
        contract.clone(),
        vec![(
            "aihubmix-image-v1",
            pixel_carrier.clone(),
            pixel_mapping.clone(),
        )],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let jobs_before: i64 = sqlx::query_scalar("SELECT count(*) FROM generation.jobs")
        .fetch_one(&pool)
        .await
        .expect("job count");
    let key = format!("size-none-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_ne!(
        status,
        StatusCode::BAD_REQUEST,
        "档案缺那一格不是消费者的参数错：{body}"
    );
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "平台侧供给问题必须说成平台侧故障：{body}"
    );
    assert_eq!(body["error"]["code"].as_str(), Some("platform_unavailable"));
    assert_public_only("尺寸换算不出", &body);
    let jobs_after: i64 = sqlx::query_scalar("SELECT count(*) FROM generation.jobs")
        .fetch_one(&pool)
        .await
        .expect("job count");
    assert_eq!(
        jobs_after, jobs_before,
        "换算不出是在受理前失败的，不该留下执行记录"
    );

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// `auto` 的语义是"由模型按提示词自己决定最佳比例"：它只原样透传、永不换算。
///
/// 声明了尺寸换算的供给收不了它——那条候选落选（原因写进判定记录），有别的候选就落过去；纯透传的
/// 供给把 `auto` 原样写进 Job。一条候选都收不了时是平台侧故障，不是参数错。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_auto_size_is_passed_through_and_never_converted() {
    let (database_url, database_name) = isolated_database_url().await;
    // 没有 Worker：同步入口只会等到超时，正好用来只看"受理与选路"。
    let (base_url, admin_token, _process) = start_api(&database_url, 1, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url).await;
    let account = create_account(&client, &base_url, &admin_token).await;
    let api_key = issue_key(&client, &base_url, &admin_token, &account).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    let model = "auto-size-model";
    let contract = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "size": {"type": "string"},
        "resolution": {"type": "string"}
    }));
    // 像素面供给：声明了尺寸换算，只收得了具体尺寸。
    let pixel_carrier = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "size": {"type": "string"}
    }));
    let pixel_mapping = json!({
        "size": {
            "source": ["size", "resolution"],
            "target": "size",
            "form": "pixels",
            "profile": lite_2k_profile()
        }
    });
    // 比例 + 档位面供给：尺寸原样承载、不做换算——`auto` 就落在这条上。
    let plain_carrier = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "size": {"type": "string"},
        "resolution": {"type": "string"}
    }));

    // ── 用例 1：换算供给收不了 `auto` → 落到纯透传的候选，`auto` 原样进 Job ──
    let status = publish_with_mappings(
        &client,
        &base_url,
        &admin_token,
        model,
        "auto-1",
        contract.clone(),
        vec![
            (
                "aihubmix-image-v1",
                pixel_carrier.clone(),
                pixel_mapping.clone(),
            ),
            ("apimart-image-v1", plain_carrier.clone(), json!({})),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "两条候选各带自己的尺寸声明");

    let mut request = route_request(model, "let the model pick the size");
    request["size"] = json!("auto");
    let key = format!("auto-skip-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::GATEWAY_TIMEOUT,
        "收不了 `auto` 只是这条候选不合格，下一条照常受理：{body}"
    );
    let job_id: Uuid =
        sqlx::query_scalar("SELECT id FROM generation.jobs WHERE idempotency_key = $1")
            .bind(&key)
            .fetch_one(&pool)
            .await
            .expect("the request must have been accepted");
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
    let considered = considered.as_array().expect("considered is an array");
    assert_eq!(considered.len(), 2, "两个候选都要进判定记录");
    assert_eq!(considered[0]["eligible"], false);
    assert!(
        considered[0]["skip_reason"].as_str().is_some_and(|reason| {
            reason.contains("auto") && reason.contains("cannot be converted")
        }),
        "落选原因必须写明 `auto` 只能原样透传、不能换算：{considered:?}"
    );
    assert_eq!(considered[1]["eligible"], true);
    let expected: Uuid = sqlx::query_scalar(
        "SELECT re.offering_id FROM publication.runtime_entries re
         WHERE re.active AND re.gateway_model = $1 AND re.routing_priority = 1",
    )
    .bind(model)
    .fetch_one(&pool)
    .await
    .expect("priority 1 offering");
    assert_eq!(
        chosen, expected,
        "换算供给收不了 `auto`，就该落到纯透传的那条"
    );
    // Job 里存的就是这次真正要发出去的东西：`auto` 原样，没有被算成一个比例。
    let stored: Value = sqlx::query_scalar(
        "SELECT native_parameters FROM generation.jobs WHERE idempotency_key = $1",
    )
    .bind(&key)
    .fetch_one(&pool)
    .await
    .expect("native parameters");
    assert_eq!(stored["size"], "auto", "`auto` 必须原样上行：{stored}");

    // ── 用例 2：只留换算供给 → 一条候选都收不了 `auto` → 平台侧故障 ──
    let status = publish_with_mappings(
        &client,
        &base_url,
        &admin_token,
        model,
        "auto-2",
        contract.clone(),
        vec![(
            "aihubmix-image-v1",
            pixel_carrier.clone(),
            pixel_mapping.clone(),
        )],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let jobs_before: i64 = sqlx::query_scalar("SELECT count(*) FROM generation.jobs")
        .fetch_one(&pool)
        .await
        .expect("job count");
    let key = format!("auto-none-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_ne!(
        status,
        StatusCode::BAD_REQUEST,
        "收不了 `auto` 不是消费者的参数错：{body}"
    );
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "平台侧供给问题必须说成平台侧故障：{body}"
    );
    assert_eq!(body["error"]["code"].as_str(), Some("platform_unavailable"));
    assert_public_only("收不了 auto", &body);
    let jobs_after: i64 = sqlx::query_scalar("SELECT count(*) FROM generation.jobs")
        .fetch_one(&pool)
        .await
        .expect("job count");
    assert_eq!(
        jobs_after, jobs_before,
        "选不出候选是在受理前失败的，不该留下执行记录"
    );

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// 没声明尺寸换算的供给（纯透传）不过换算函数：`auto` 原样出现在发给上游的报文里。
///
/// 这正是"渠道收 `auto`"的样子（承载面自己声明了那个字段）：平台原样发出去，让模型自己决定最佳
/// 比例，不替它算一个。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_auto_size_reaches_the_upstream_request_body_untouched() {
    let mut draft = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    // 合同与承载面都声明 `size`，但**没有**尺寸换算声明：尺寸原样上行。
    draft["capability_schema"] = surface_schema(json!({
        "model": {"const": Harness::MODEL},
        "prompt": {"type": "string", "minLength": 1},
        "size": {"type": "string"}
    }));
    draft["carrier_schema"] = surface_schema(json!({
        "model": {"const": Harness::MODEL},
        "prompt": {"type": "string", "minLength": 1},
        "size": {"type": "string"}
    }));
    draft["parameter_mapping"] = json!({});
    let harness = Harness::start_with_draft(
        draft,
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;

    let key = format!("auto-pass-through-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "let the model pick the size");
    request["size"] = json!("auto");
    let (status, body) = harness
        .sync_json("/v1/images/generations", &key, request.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("auto 透传", &body);

    let submit_body = harness.submit_body("/v1/images/generations");
    assert_eq!(
        submit_body["size"], "auto",
        "`auto` 必须原样发给上游：{submit_body}"
    );
    harness.assert_only_declared_fields(&request);
    let stored: Value = sqlx::query_scalar(
        "SELECT native_parameters FROM generation.jobs WHERE idempotency_key = $1",
    )
    .bind(&key)
    .fetch_one(&harness.pool)
    .await
    .expect("native parameters");
    assert_eq!(stored["size"], "auto", "{stored}");
    assert_job_succeeded(&harness, &key).await;
    harness.cleanup().await;
}

/// 改名：合同字段承载面承载不了、但改名把它落到承载面声明的名字上时，这条供给照样跑得通。
///
/// 断言两处都是**线上形态**：发给假上游的报文里是线上字段名，Job 里存的也是它——合同字段名
/// 一个都不许上行。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_renamed_field_reaches_the_upstream_under_the_wire_name() {
    // 合同（模型级）：调用方提交的是 `size`（比例）。
    // 承载面：这条供给线上叫 `resolution`（APIMart 那一侧的渠道字段名）。
    let mut draft = candidate("APIMart", "apimart-image-v1", &["prompt_only"]);
    draft["credential_env"] = json!("APIMART_API_KEY");
    draft["capability_schema"] = surface_schema(json!({
        "model": {"const": Harness::MODEL},
        "prompt": {"type": "string", "minLength": 1},
        "size": {"type": "string"}
    }));
    draft["carrier_schema"] = surface_schema(json!({
        "model": {"const": Harness::MODEL},
        "prompt": {"type": "string", "minLength": 1},
        "resolution": {"type": "string"}
    }));
    draft["parameter_mapping"] = json!({"rename": {"size": "resolution"}});
    let harness = Harness::start_with_draft(draft, None, UpstreamBehaviour::apimart(), 64).await;

    let key = format!("renamed-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "a 1:1 poster");
    request["size"] = json!("1:1");
    let (status, body) = harness
        .sync_json("/v1/images/generations", &key, request.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("改名", &body);

    let submit_body = harness.submit_body("/v1/images/generations");
    assert_eq!(
        submit_body["resolution"], "1:1",
        "线上那个字段名才是要发的东西：{submit_body}"
    );
    assert!(
        submit_body.get("size").is_none(),
        "合同字段名不许出现在报文里：{submit_body}"
    );
    harness.assert_only_declared_fields(&request);
    // Job 里存的就是这次真正发出去的东西：合同字段名在受理期就换成了线上名字。
    let stored: Value = sqlx::query_scalar(
        "SELECT native_parameters FROM generation.jobs WHERE idempotency_key = $1",
    )
    .bind(&key)
    .fetch_one(&harness.pool)
    .await
    .expect("native parameters");
    assert_eq!(stored["resolution"], "1:1", "{stored}");
    assert!(stored.get("size").is_none(), "{stored}");
    assert_job_succeeded(&harness, &key).await;
    harness.cleanup().await;
}

/// 取值映射：组装期把合同取值换成线上取值；映射表里没有这个取值 → 该候选**不合格**，原因进
/// `routing_decisions`，有别的候选就落过去、没有就 503（不是 400 参数错）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn an_enum_map_value_reaches_the_upstream_and_an_unmapped_one_skips_the_candidate() {
    let (database_url, database_name) = isolated_database_url().await;
    // 没有 Worker：同步入口只会等到超时，正好用来只看"受理与选路"。
    let (base_url, admin_token, _process) = start_api(&database_url, 1, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url).await;
    let account = create_account(&client, &base_url, &admin_token).await;
    let api_key = issue_key(&client, &base_url, &admin_token, &account).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    let model = "enum-map-model";
    let contract = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "quality": {"type": "string", "enum": ["low", "high"]}
    }));
    let carrier = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "quality": {"type": "string"}
    }));
    // 优先级 0 的候选只把 `high` 映射成线上取值；优先级 1 的候选原样承载取值。
    let mapped = json!({"enum_map": {"quality": {"high": "xhigh"}}});

    let status = publish_with_mappings(
        &client,
        &base_url,
        &admin_token,
        model,
        "enum-1",
        contract.clone(),
        vec![
            ("aihubmix-image-v1", carrier.clone(), mapped.clone()),
            ("aihubmix-image-v1", carrier.clone(), json!({})),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "两条候选各带自己的映射");

    let stored_parameters = |key: &str| {
        let pool = pool.clone();
        let key = key.to_owned();
        async move {
            sqlx::query_scalar::<_, Value>(
                "SELECT native_parameters FROM generation.jobs WHERE idempotency_key = $1",
            )
            .bind(&key)
            .fetch_one(&pool)
            .await
            .expect("native parameters")
        }
    };

    // ── 用例 1：映射表里有这个取值 → 报文与 Job 里都是映射后的线上取值 ──
    let key = format!("enum-mapped-{}", Uuid::new_v4());
    let mut request = route_request(model, "a high quality poster");
    request["quality"] = json!("high");
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::GATEWAY_TIMEOUT,
        "没有 Worker，受理后只会等到超时：{body}"
    );
    let stored = stored_parameters(&key).await;
    assert_eq!(
        stored["quality"], "xhigh",
        "Job 里存的必须是映射后的线上取值：{stored}"
    );

    // ── 用例 2：映射表里没有这个取值 → 优先级 0 落选，落到优先级 1，原因写进判定记录 ──
    let key = format!("enum-skip-{}", Uuid::new_v4());
    let mut request = route_request(model, "a low quality poster");
    request["quality"] = json!("low");
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::GATEWAY_TIMEOUT,
        "映射不了只是这条候选不合格，下一条照常受理：{body}"
    );
    let job_id: Uuid =
        sqlx::query_scalar("SELECT id FROM generation.jobs WHERE idempotency_key = $1")
            .bind(&key)
            .fetch_one(&pool)
            .await
            .expect("the request must have been accepted");
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
    let considered = considered.as_array().expect("considered is an array");
    assert_eq!(considered.len(), 2, "两个候选都要进判定记录");
    assert_eq!(considered[0]["eligible"], false);
    assert!(
        considered[0]["skip_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("quality")),
        "落选原因必须写明是哪个字段的取值映射不了：{considered:?}"
    );
    assert_eq!(considered[1]["eligible"], true);
    let expected: Uuid = sqlx::query_scalar(
        "SELECT re.offering_id FROM publication.runtime_entries re
         WHERE re.active AND re.gateway_model = $1 AND re.routing_priority = 1",
    )
    .bind(model)
    .fetch_one(&pool)
    .await
    .expect("priority 1 offering");
    assert_eq!(chosen, expected, "映射不了就该落到下一条");
    let stored = stored_parameters(&key).await;
    assert_eq!(
        stored["quality"], "low",
        "落到的那条候选原样承载这个取值：{stored}"
    );

    // ── 用例 3：同一个型号只留映射表窄的那条 → 一条候选都不合格 → 平台侧故障 ──
    let status = publish_with_mappings(
        &client,
        &base_url,
        &admin_token,
        model,
        "enum-2",
        contract.clone(),
        vec![("aihubmix-image-v1", carrier.clone(), mapped.clone())],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let jobs_before: i64 = sqlx::query_scalar("SELECT count(*) FROM generation.jobs")
        .fetch_one(&pool)
        .await
        .expect("job count");
    let key = format!("enum-none-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_ne!(
        status,
        StatusCode::BAD_REQUEST,
        "映射不了不是消费者的参数错：{body}"
    );
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "平台侧供给问题必须说成平台侧故障：{body}"
    );
    assert_eq!(body["error"]["code"].as_str(), Some("platform_unavailable"));
    assert_public_only("取值映射不出", &body);
    let jobs_after: i64 = sqlx::query_scalar("SELECT count(*) FROM generation.jobs")
        .fetch_one(&pool)
        .await
        .expect("job count");
    assert_eq!(
        jobs_after, jobs_before,
        "映射不出是在受理前失败的，不该留下执行记录"
    );

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// 显式默认值的每个键必须被这条供给承载：声明了一个发不出去的默认值，发布被拒。
///
/// 与"承载面 ⊆ 合同 ⊆ Driver 能写上线文的名字"同一条道理——声明了却做不到，就是声明与行为分了家。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn defaults_the_carrier_cannot_carry_are_rejected_at_publication() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 1, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url).await;

    let model = "defaults-boundary-model";
    let contract = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "quality": {"type": "string", "enum": ["low", "high"]}
    }));
    // 承载面承载不了 `quality`：这条默认值永远不会生效。
    let narrow = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1}
    }));
    // 同一份承载面、不带默认值时照常发布：拒绝的是那条默认值，不是承载面本身。
    let status = publish_with_mappings(
        &client,
        &base_url,
        &admin_token,
        model,
        "defaults-0",
        contract.clone(),
        vec![("aihubmix-image-v1", narrow.clone(), json!({}))],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let status = publish_with_mappings(
        &client,
        &base_url,
        &admin_token,
        model,
        "defaults-1",
        contract.clone(),
        vec![(
            "aihubmix-image-v1",
            narrow.clone(),
            json!({"defaults": {"quality": "low"}}),
        )],
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a default the carrier cannot carry must be rejected"
    );

    // 承载面声明了它：同一份默认值照常发布。
    let wide = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "quality": {"type": "string", "enum": ["low", "high"]}
    }));
    let status = publish_with_mappings(
        &client,
        &base_url,
        &admin_token,
        model,
        "defaults-2",
        contract.clone(),
        vec![(
            "aihubmix-image-v1",
            wide,
            json!({"defaults": {"quality": "low"}}),
        )],
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    drop_isolated_database(&database_name).await;
}

/// 合同外的**图片字段**不走"合同外字段丢弃"那条：合同没为图留位置时一律 400 `invalid_parameter`。
///
/// 丢图等于悄悄生成一张没有参考图的图（还照样计费），所以既不许丢弃、也不许说成平台侧故障
/// （供给面没问题，是这个模型不接图）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn an_image_the_contract_never_declared_is_rejected_as_an_invalid_parameter() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 1, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url).await;
    let account = create_account(&client, &base_url, &admin_token).await;
    let api_key = issue_key(&client, &base_url, &admin_token, &account).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    let model = "image-boundary-model";
    // 纯文生图：合同与承载面里都没有任何图片字段。
    let contract = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1}
    }));
    let status = publish_with_surfaces(
        &client,
        &base_url,
        &admin_token,
        model,
        "image-boundary-1",
        contract.clone(),
        vec![("aihubmix-image-v1", contract.clone())],
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let jobs_before: i64 = sqlx::query_scalar("SELECT count(*) FROM generation.jobs")
        .fetch_one(&pool)
        .await
        .expect("job count");
    let key = format!("image-boundary-{}", Uuid::new_v4());
    let mut request = route_request(model, "an image the contract never declared");
    request["image"] = json!(png_data_url());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {body}");
    assert_eq!(
        body["error"]["code"].as_str(),
        Some("invalid_parameter"),
        "合同没声明的图片字段是参数错：{body}"
    );
    assert_public_only("合同外的图片字段", &body);
    let jobs_after: i64 = sqlx::query_scalar("SELECT count(*) FROM generation.jobs")
        .fetch_one(&pool)
        .await
        .expect("job count");
    assert_eq!(
        jobs_after, jobs_before,
        "带图请求在受理前就被拒了，不该留下执行记录"
    );

    // 同一份请求去掉图：照常受理（没有 Worker，等到超时）。
    let key = format!("image-boundary-none-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &route_request(model, "no image at all"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::GATEWAY_TIMEOUT,
        "没有图就是普通的文生图：{body}"
    );

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// 假上游记录里某条路径上**最近一次**提交报文（JSON 形态）。
fn last_submit_body(calls: &UpstreamCalls, path: &str) -> Value {
    let raw = calls
        .lock()
        .expect("calls lock")
        .iter()
        .rfind(|call| call.method == "POST" && call.path == path)
        .map(|call| call.body.clone())
        .expect("the driver must submit a generation request");
    serde_json::from_slice(&raw).expect("submit body is JSON")
}

/// 假上游记录里某条路径上的调用次数。
fn count_calls(calls: &UpstreamCalls, method: &str, path: &str) -> usize {
    calls
        .lock()
        .expect("calls lock")
        .iter()
        .filter(|call| call.method == method && call.path == path)
        .count()
}

/// 这次请求的**选路判定**：`(选中的候选, 完整取舍画面)`。内部事实，对客看不见。
async fn routing_of(pool: &PgPool, key: &str) -> (Uuid, Vec<Value>) {
    let job_id: Uuid =
        sqlx::query_scalar("SELECT id FROM generation.jobs WHERE idempotency_key = $1")
            .bind(key)
            .fetch_one(pool)
            .await
            .expect("the request must have created a job");
    let row = sqlx::query(
        "SELECT chosen_offering_id, considered FROM generation.routing_decisions WHERE job_id = $1",
    )
    .bind(job_id)
    .fetch_one(pool)
    .await
    .expect("routing decision row must exist");
    (
        row.try_get("chosen_offering_id").expect("chosen"),
        row.try_get::<Value, _>("considered")
            .expect("considered")
            .as_array()
            .cloned()
            .unwrap_or_default(),
    )
}

/// 被选中的候选属于哪个渠道。
async fn chosen_provider_kind(pool: &PgPool, offering_id: Uuid) -> String {
    sqlx::query_scalar(
        "SELECT c.provider_kind FROM supply.offerings o
         JOIN supply.channels c ON c.id = o.channel_id
         WHERE o.id = $1",
    )
    .bind(offering_id)
    .fetch_one(pool)
    .await
    .expect("the chosen offering must have a channel")
}

/// 把一份 2.5 素材发布到这次用例的两个进程内假上游上：上游地址按渠道替换，其余一字不改。
///
/// 返回**换过地址的**素材、发布状态与响应正文——用例后半段还要按同一份声明做断言，所以替换必须
/// 发生在返回的那一份上。凭证仍只从环境变量读，这里不碰。
async fn publish_2_5_material(
    client: &Client,
    base_url: &str,
    admin_token: &str,
    mut material: Value,
    aihubmix_upstream: &str,
    apimart_upstream: &str,
) -> (Value, StatusCode, String) {
    for offering in material["offerings"]
        .as_array_mut()
        .expect("offerings must be an array")
    {
        offering["base_url"] = Value::String(match offering["provider_kind"].as_str() {
            Some("AIHubMix") => aihubmix_upstream.to_owned(),
            Some("APIMart") => apimart_upstream.to_owned(),
            other => panic!("unexpected provider kind {other:?}"),
        });
    }
    let response = client
        .post(format!("{base_url}/api/v1/runtime-revisions"))
        .bearer_auth(admin_token)
        .json(&material)
        .send()
        .await
        .expect("publication request");
    let status = response.status();
    let body = response.text().await.expect("publication body");
    (material, status, body)
}

/// 新形状的 2.5 素材（**一个 Vendor Model 一份文件**）端到端跑一遍：同一个型号只落**一份合同**，
/// 两条供给各带自己的承载面与参数映射，选路按承载面走。
///
/// 四条请求各钉一件事：
/// - 只带 `prompt`：首选的 AIHubMix 承载得了，请求就该落在它身上；
/// - 带 `background` / `output_compression` / `moderation`：承载面按**厂商契约**声明，两家都承载
///   得了这三项（聚合渠道转售上游能力，"渠道没写"不构成"渠道不能"），因此照旧落在首选上，三个
///   字段逐字上行；
/// - 带参考图：先把 AIHubMix 这条供给收窄成只允许文生图（**限制差异由测试自己构造**，不拿承载面
///   字段的有无制造差异），于是它因**分支限制**不合格（判定记录写明原因）、改道 APIMart；合同
///   字段叫 `image`，APIMart 线上叫 `image_urls`，靠改名落到渠道字段名上（报文里不许出现
///   `image`），内联图先经上传接口换成公网 URL；
/// - 带参考图 + 遮罩：同样因分支限制落到 APIMart，`image_urls` 与 `mask_url` 两个渠道名都得上线。
///
/// 两家渠道各起一个进程内假上游：线上形状不同（一家同步回图、一家任务式），所以"报文里到底是
/// 哪个字段名"只能按真正收到请求的那一方来判。全程零外部调用。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_2_5_materials_route_by_carrier_surface_and_wire_names() {
    let (database_url, database_name) = isolated_database_url().await;
    let client = Client::new();
    let aihubmix_calls: UpstreamCalls = Arc::new(Mutex::new(Vec::new()));
    let apimart_calls: UpstreamCalls = Arc::new(Mutex::new(Vec::new()));
    let aihubmix_upstream = start_fake_upstream_with(
        aihubmix_calls.clone(),
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
    )
    .await;
    let apimart_upstream =
        start_fake_upstream_with(apimart_calls.clone(), UpstreamBehaviour::apimart()).await;
    let (base_url, admin_token, _process) = start_api(&database_url, 30, 64).await;
    wait_until_ready(&client, &base_url).await;
    let account = create_account(&client, &base_url, &admin_token).await;
    let api_key = issue_key(&client, &base_url, &admin_token, &account).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    // ── 两份素材各自发布：一个 Vendor Model 一份文件，顶层一份合同 + 两条候选 ──
    for material in [
        include_str!("../../../config/bootstrap/gpt-image-2.5-flare.json"),
        include_str!("../../../config/bootstrap/gpt-image-2.5-sunburst.json"),
    ] {
        let material: Value = serde_json::from_str(material).expect("material parses");
        assert_eq!(
            material["offerings"]
                .as_array()
                .expect("offerings must be an array")
                .len(),
            2,
            "一份素材两条供给：AIHubMix 首选、APIMart 次之"
        );
        // 上游地址换成这个用例的两个假上游，各按渠道给：凭证仍只从环境变量读。
        let (material, status, body) = publish_2_5_material(
            &client,
            &base_url,
            &admin_token,
            material,
            &aihubmix_upstream.base_url,
            &apimart_upstream.base_url,
        )
        .await;
        let model = material["native_model_id"]
            .as_str()
            .expect("native model id")
            .to_owned();
        // 对客名与厂商原生名是两个角色：目录与受理用前者，合同与合同行用后者。种子素材不写
        // `gateway_model`，按发布期的回退规则取厂商原生名；自命名的例子见命名层那条用例。
        let gateway = material["gateway_model"]
            .as_str()
            .unwrap_or(&model)
            .to_owned();
        assert_eq!(status, StatusCode::OK, "{gateway} 素材必须能发布：{body}");

        // 合同是**模型级唯一一份**：这个型号只落一行，两条候选都挂在它下面。
        let contracts: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM catalog.vendor_models
             WHERE vendor_id = 'OpenAI' AND native_model_id = $1",
        )
        .bind(&model)
        .fetch_one(&pool)
        .await
        .expect("contract count");
        assert_eq!(contracts, 1, "一个 Vendor Model 只能有一份合同");
        let rows = sqlx::query(
            "SELECT re.vendor_model_id, c.provider_kind, o.carrier_schema, o.parameter_mapping,
                    vm.capability_schema
             FROM publication.runtime_entries re
             JOIN supply.offerings o ON o.id = re.offering_id
             JOIN supply.channels c ON c.id = o.channel_id
             JOIN catalog.vendor_models vm ON vm.id = re.vendor_model_id
             WHERE re.active AND re.gateway_model = $1
             ORDER BY re.routing_priority",
        )
        .bind(&gateway)
        .fetch_all(&pool)
        .await
        .expect("candidate rows");
        assert_eq!(rows.len(), 2, "两个渠道都要成为可用候选");
        let vendor_model_ids: Vec<Uuid> = rows
            .iter()
            .map(|row| row.try_get("vendor_model_id").expect("vendor model"))
            .collect();
        assert_eq!(
            vendor_model_ids[0], vendor_model_ids[1],
            "两条候选必须挂在同一份合同（同一个 Vendor Model）上"
        );
        let kinds: Vec<String> = rows
            .iter()
            .map(|row| row.try_get("provider_kind").expect("provider kind"))
            .collect();
        assert_eq!(
            kinds,
            vec!["AIHubMix".to_owned(), "APIMart".to_owned()],
            "下标 0 是首选：AIHubMix"
        );

        let carriers: Vec<Value> = rows
            .iter()
            .map(|row| row.try_get("carrier_schema").expect("carrier"))
            .collect();
        assert_ne!(
            carriers[0], carriers[1],
            "两条供给各带自己的承载面，不是共用一份"
        );
        // 承载面按**厂商契约**声明：聚合渠道转售的就是上游模型的能力，因此 AIHubMix 与 APIMart
        // 一样声明 `background` / `output_compression` / `moderation`，枚举与默认值照厂商契约。
        // 反过来说，承载面声明了就意味着平台会把字段发出去——渠道不接受是渠道报错，不是平台静默
        // 把字段吞掉。
        for name in ["background", "output_compression", "moderation"] {
            assert!(
                carriers[0]["properties"].get(name).is_some(),
                "AIHubMix 的承载面要按厂商契约声明 {name}"
            );
            assert!(
                carriers[1]["properties"].get(name).is_some(),
                "APIMart 的承载面声明了 {name}"
            );
        }
        assert_eq!(
            carriers[0]["properties"]["background"]["enum"],
            json!(["auto", "opaque", "transparent"]),
            "background 的枚举照厂商契约"
        );
        assert_eq!(carriers[0]["properties"]["background"]["default"], "auto");
        assert_eq!(
            carriers[0]["properties"]["output_compression"]["default"],
            100
        );
        assert_eq!(carriers[0]["properties"]["moderation"]["default"], "auto");
        // 参考图两边都声明成数组：AIHubMix 按厂商契约的 edit 面（`file[]`，≤16），APIMart 按
        // 自己的文档（`image_urls`，≤16）——收图上限与这个形态是同一件事，写歪了发布期就拒。
        assert_eq!(
            carriers[0]["properties"]["image"]["type"], "array",
            "AIHubMix 的参考图是数组形态"
        );
        assert_eq!(carriers[0]["properties"]["image"]["maxItems"], 16);
        assert_eq!(carriers[1]["properties"]["image_urls"]["maxItems"], 16);
        // 承载面的每个字段名都要能从合同到达：合同直接声明，或被改名接过去（供给不能凭空多出参数）。
        for (index, row) in rows.iter().enumerate() {
            let carrier: Value = row.try_get("carrier_schema").expect("carrier");
            let contract: Value = row.try_get("capability_schema").expect("contract");
            let mapping: Value = row.try_get("parameter_mapping").expect("mapping");
            let wires: Vec<Value> = mapping["rename"]
                .as_object()
                .map(|renames| renames.values().cloned().collect())
                .unwrap_or_default();
            assert_eq!(
                contract["properties"]["model"]["const"], model,
                "两条候选读到的都是这个型号的合同"
            );
            for name in carrier["properties"]
                .as_object()
                .expect("carrier properties")
                .keys()
            {
                let declared = contract["properties"]
                    .as_object()
                    .expect("contract properties")
                    .contains_key(name);
                let renamed = wires
                    .iter()
                    .any(|wire| wire.as_str() == Some(name.as_str()));
                assert!(declared || renamed, "候选 {index} 的 {name} 必须从合同可达");
            }
        }
        // APIMart 的图片字段靠**改名**接到合同字段上（合同叫 image/mask，线上叫 image_urls/mask_url）。
        let aihubmix_mapping: Value = rows[0].try_get("parameter_mapping").expect("mapping");
        assert_eq!(
            aihubmix_mapping,
            json!({}),
            "AIHubMix 与合同同型（size 都是像素型），不需要映射"
        );
        let apimart_mapping: Value = rows[1].try_get("parameter_mapping").expect("mapping");
        assert_eq!(apimart_mapping["rename"]["image"], "image_urls");
        assert_eq!(apimart_mapping["rename"]["mask"], "mask_url");

        // 对客目录：这个型号必须查得到，`contract` 逐字就是发布的那一份（只有 `model.const`
        // 按对客名替换过）——"库里发布成了"与"调用方按目录建表单建得对"是两件事，这里把后一件
        // 也钉住。目录公开，所以这里照调用方最常见的取法来：不带任何鉴权头。
        let (status, catalog) = get_catalog(&client, &base_url, None).await;
        assert_eq!(status, StatusCode::OK, "{catalog}");
        let entry = catalog["data"]
            .as_array()
            .expect("catalog data")
            .iter()
            .find(|entry| entry["name"].as_str() == Some(gateway.as_str()))
            .unwrap_or_else(|| panic!("{gateway} 必须在目录里：{catalog}"));
        assert_eq!(entry["vendor_id"].as_str(), Some("OpenAI"));
        assert_eq!(entry["revision"], material["native_revision"]);
        assert_eq!(
            entry["contract"],
            consumer_contract(material["capability_schema"].clone(), &gateway),
            "目录里的合同必须是发布的那一份，只有 `model.const` 换成对客名"
        );
        assert!(
            entry["contract"]["properties"]["model"]["const"] == gateway,
            "对客合同里的 model.const 就是调用方要提交的名字：{entry}"
        );
    }

    // 四条请求共用一个真实 Worker：它只领 Job，不知道这次用例在验什么。
    let _worker = spawn_worker_process(&database_url);

    // ── 用例 1：只带 prompt → 首选（AIHubMix）承载得了，就落在它身上 ──
    let key = format!("contract-aihubmix-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &json!({"model": "gpt-image-2.5-flare", "prompt": "雨天窗边的阅读角"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "首选供给必须跑通：{body}");
    assert_sync_success("只带 prompt 的请求", &body);
    let (chosen, considered) = routing_of(&pool, &key).await;
    assert_eq!(
        chosen_provider_kind(&pool, chosen).await,
        "AIHubMix",
        "两条候选都合格时按优先级选第一个：{considered:?}"
    );
    assert!(
        considered
            .iter()
            .all(|candidate| candidate["eligible"] == true),
        "两条候选都该合格：{considered:?}"
    );
    assert_eq!(
        count_calls(&aihubmix_calls, "POST", "/v1/images/generations"),
        1,
        "请求落在 AIHubMix，报文就该发到它的上游"
    );
    assert_eq!(
        count_calls(&apimart_calls, "POST", "/v1/images/generations"),
        0,
        "没落到 APIMart 就不该有它的生成请求"
    );

    // ── 用例 2：带 background / output_compression / moderation → 两家都承载得了，首选照旧 ──
    //
    // 承载面按**厂商契约**声明，这三项不是"APIMart 特有的差异"：AIHubMix 这条供给同样声明了
    // 它们，所以请求落在首选上，三个字段逐字上行。渠道不接受某个取值时表现为渠道报错——平台
    // 不静默丢字段、也不替调用方改值。
    let key = format!("contract-optional-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &json!({
            "model": "gpt-image-2.5-flare",
            "prompt": "白色运动鞋，透明背景",
            "background": "transparent",
            "output_compression": 80,
            "moderation": "low"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "首选供给必须跑通：{body}");
    assert_sync_success("带三个可选参数的请求", &body);
    let (chosen, considered) = routing_of(&pool, &key).await;
    assert_eq!(
        chosen_provider_kind(&pool, chosen).await,
        "AIHubMix",
        "两家都承载得了这三项，首选照旧：{considered:?}"
    );
    assert!(
        considered
            .iter()
            .all(|candidate| candidate["eligible"] == true),
        "两条候选都该合格：{considered:?}"
    );
    let submit = last_submit_body(&aihubmix_calls, "/v1/images/generations");
    assert_eq!(
        submit["background"], "transparent",
        "承载得了的字段必须原样上行：{submit}"
    );
    assert_eq!(submit["output_compression"], 80, "{submit}");
    assert_eq!(submit["moderation"], "low", "{submit}");

    // ── 收窄素材：把 AIHubMix 这条供给的 `allowed_branches` 收成只允许 `prompt_only` ──
    //
    // 落选触发用**测试自己构造的真实限制差异**，不用承载面字段的有无：两家现在按厂商契约声明
    // 同一批字段，靠字段差制造落选会把"渠道转售上游能力"验成相反的样子。收窄 `restrictions`
    // 是它的正当用法（限制只收窄、不放宽），带图请求因此真的落不到这条供给上。合同一字不改，
    // 同一个型号仍是**同一行**合同，替换的是 active 候选集。
    for material in [
        include_str!("../../../config/bootstrap/gpt-image-2.5-flare.json"),
        include_str!("../../../config/bootstrap/gpt-image-2.5-sunburst.json"),
    ] {
        let mut variant: Value = serde_json::from_str(material).expect("material parses");
        let model = variant["native_model_id"]
            .as_str()
            .expect("native model id")
            .to_owned();
        // 种子素材不写对客名，按发布期的回退规则取厂商原生名（见命名层那条用例的自命名例子）。
        let gateway = variant["gateway_model"]
            .as_str()
            .unwrap_or(&model)
            .to_owned();
        let mut narrowed = false;
        for offering in variant["offerings"]
            .as_array_mut()
            .expect("offerings must be an array")
        {
            if offering["provider_kind"] == "AIHubMix" {
                // 只走文生图：带图与带遮罩的请求都不该落在它身上；既然不承诺收图，上限就是 0。
                offering["restrictions"] = json!({
                    "allowed_branches": ["prompt_only"],
                    "max_images": 0
                });
                narrowed = true;
            }
        }
        assert!(narrowed, "{model} 的变体必须收窄 AIHubMix 这条供给");
        let (_, status, body) = publish_2_5_material(
            &client,
            &base_url,
            &admin_token,
            variant,
            &aihubmix_upstream.base_url,
            &apimart_upstream.base_url,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{gateway} 的变体素材必须能发布：{body}"
        );
        // 发布即原子替换该模型的 active 候选：现在生效的就是这份收窄过的声明。
        // 替换的对象是**对客名**，所以这里按对客名查。
        let restrictions: Value = sqlx::query_scalar(
            "SELECT o.restrictions FROM publication.runtime_entries re
             JOIN supply.offerings o ON o.id = re.offering_id
             JOIN supply.channels c ON c.id = o.channel_id
             WHERE re.active AND re.gateway_model = $1 AND c.provider_kind = 'AIHubMix'",
        )
        .bind(&gateway)
        .fetch_one(&pool)
        .await
        .expect("the narrowed AIHubMix entry must be active");
        assert_eq!(
            restrictions,
            json!({"allowed_branches": ["prompt_only"], "max_images": 0}),
            "{gateway} 的变体发布后，AIHubMix 这条供给只允许文生图"
        );
    }

    // ── 用例 3：带参考图 → 收窄过的 AIHubMix 因**分支限制**不合格，改道 APIMart，字段按渠道名上行 ──
    let uploads_before = count_calls(&apimart_calls, "POST", "/v1/uploads/images");
    let key = format!("contract-branch-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &json!({
            "model": "gpt-image-2.5-flare",
            "prompt": "保留商品主体，把背景换成米白色摄影棚",
            "image": [png_data_url()]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "改道之后要真的跑通：{body}");
    assert_sync_success("带参考图的请求", &body);
    let (chosen, considered) = routing_of(&pool, &key).await;
    assert_eq!(considered.len(), 2, "两个候选都要进判定记录");
    assert_eq!(
        considered[0]["eligible"], false,
        "AIHubMix 这条供给收窄成只允许文生图，带图请求不该合格：{considered:?}"
    );
    assert!(
        considered[0]["skip_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("image_conditioned")),
        "落选原因必须写明是哪条限制拦下的：{considered:?}"
    );
    assert_eq!(considered[1]["eligible"], true);
    assert_eq!(
        chosen_provider_kind(&pool, chosen).await,
        "APIMart",
        "第一条不合格就该落到下一条：{considered:?}"
    );
    // 内联图先换成渠道要的公网 URL，再按**渠道字段名**装进生成请求。
    let submit = last_submit_body(&apimart_calls, "/v1/images/generations");
    assert!(
        submit.get("image").is_none(),
        "合同字段名 `image` 不许出现在 APIMart 的报文里：{submit}"
    );
    let urls = submit["image_urls"]
        .as_array()
        .unwrap_or_else(|| panic!("APIMart 线上字段名是 image_urls 数组：{submit}"));
    assert_eq!(urls.len(), 1, "{submit}");
    assert!(
        urls[0]
            .as_str()
            .is_some_and(|url| url.starts_with("http://127.0.0.1:")),
        "上传换回来的公网 URL 才该上行：{submit}"
    );
    assert_eq!(
        count_calls(&apimart_calls, "POST", "/v1/uploads/images") - uploads_before,
        1,
        "内联参考图必须先上传换成公网 URL"
    );

    // ── 用例 4：带参考图 + 遮罩 → 遮罩分支同样被收窄掉，两个渠道字段名都上线 ──
    let key = format!("contract-masked-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &json!({
            "model": "gpt-image-2.5-sunburst",
            "prompt": "只改遮罩圈出的背景",
            "image": [png_data_url()],
            "mask": png_data_url()
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "带遮罩的请求要真的跑通：{body}");
    assert_sync_success("带参考图与遮罩的请求", &body);
    let (chosen, considered) = routing_of(&pool, &key).await;
    assert_eq!(
        chosen_provider_kind(&pool, chosen).await,
        "APIMart",
        "遮罩分支同样被收窄掉：{considered:?}"
    );
    assert!(
        considered[0]["skip_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("masked")),
        "落选原因必须写明是哪条限制拦下的：{considered:?}"
    );
    let submit = last_submit_body(&apimart_calls, "/v1/images/generations");
    assert!(
        submit.get("image").is_none() && submit.get("mask").is_none(),
        "合同字段名 `image` / `mask` 都不许出现在 APIMart 的报文里：{submit}"
    );
    assert_eq!(
        submit["image_urls"].as_array().map(Vec::len),
        Some(1),
        "{submit}"
    );
    assert!(
        submit["mask_url"].as_str().is_some(),
        "遮罩要按渠道字段名 `mask_url` 上线：{submit}"
    );
    assert_eq!(
        count_calls(&apimart_calls, "POST", "/v1/uploads/images") - uploads_before,
        3,
        "用例 3 的参考图与用例 4 的参考图、遮罩各上传一次"
    );

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// 一个只声明给定顶层字段的封闭对象 schema（合同与承载面都用它）。
fn surface_schema(properties: Value) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["model", "prompt"],
        "properties": properties
    })
}

/// 发布一份"顶层合同 + 若干候选（各自承载面）"的命令，返回状态码。
async fn publish_with_surfaces(
    client: &Client,
    base_url: &str,
    admin_token: &str,
    model: &str,
    revision: &str,
    contract: Value,
    carriers: Vec<(&str, Value)>,
) -> StatusCode {
    publish_with_mappings(
        client,
        base_url,
        admin_token,
        model,
        revision,
        contract,
        carriers
            .into_iter()
            .map(|(adapter_key, carrier)| (adapter_key, carrier, json!({})))
            .collect(),
    )
    .await
}

/// 同 `publish_with_surfaces`，但每个候选自带一份**参数映射**（目前只有显式默认值一块）。
async fn publish_with_mappings(
    client: &Client,
    base_url: &str,
    admin_token: &str,
    model: &str,
    revision: &str,
    contract: Value,
    carriers: Vec<(&str, Value, Value)>,
) -> StatusCode {
    let offerings = carriers
        .into_iter()
        .map(|(adapter_key, carrier, parameter_mapping)| {
            json!({
                "provider_kind": if adapter_key == "apimart-image-v1" { "APIMart" } else { "AIHubMix" },
                "adapter_key": adapter_key,
                "provider_model_id": model,
                "base_url": "http://127.0.0.1:1",
                "credential_env": "AIHUBMIX_API_KEY",
                "restrictions": {"allowed_branches": ["prompt_only"], "max_images": 0},
                "carrier_schema": carrier,
                "parameter_mapping": parameter_mapping,
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
        })
        .collect::<Vec<_>>();
    let body = json!({
        "vendor_id": "OpenAI",
        "native_model_id": model,
        "native_revision": revision,
        "actor": "contract-test",
        "capability_schema": contract,
        "offerings": offerings
    });
    client
        .post(format!("{base_url}/api/v1/runtime-revisions"))
        .bearer_auth(admin_token)
        .json(&body)
        .send()
        .await
        .expect("runtime publication")
        .status()
}

/// 某一份合同的落库事实：`(id, 合同内容, created_at)`。
async fn contract_row(pool: &PgPool, model: &str, revision: &str) -> (Uuid, Value, String) {
    let row = sqlx::query(
        "SELECT id, capability_schema, created_at::text AS created_at
         FROM catalog.vendor_models WHERE native_model_id = $1 AND native_revision = $2",
    )
    .bind(model)
    .bind(revision)
    .fetch_one(pool)
    .await
    .expect("contract row");
    (
        row.try_get("id").expect("contract id"),
        row.try_get("capability_schema").expect("contract"),
        row.try_get("created_at").expect("created at"),
    )
}

/// 增量迁移必须在**已经建过库**的环境里跑得通。
///
/// 早期迁移是以"建表"方式被应用的，改它们不会更新已建好的库；本次改动按新迁移增量修改，
/// 因此这里先在只应用了早期迁移的库上建表，再补上整批迁移，确认它能升上来。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn images_pass_through_migration_applies_on_an_existing_database() {
    let (database_url, database_name) = isolated_database_url().await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");
    let migrations = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../migrations");

    // 1) 只应用"这次改动之前"的迁移：把早期 `.sql` 拷到一个临时目录。
    let staged = std::env::temp_dir().join(format!("seeai-early-migrations-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&staged).expect("staging directory");
    for entry in std::fs::read_dir(&migrations).expect("migrations directory") {
        let entry = entry.expect("migration entry");
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".sql") && name.as_str() < "0005" {
            std::fs::copy(entry.path(), staged.join(&name)).expect("copy early migration");
        }
    }
    let early = sqlx::migrate::Migrator::new(staged.clone())
        .await
        .expect("early migrator");
    early.run(&pool).await.expect("early migrations apply");

    // 2) 再应用完整迁移集（含本次的增量）：已应用过的按版本跳过。
    let all = sqlx::migrate::Migrator::new(migrations)
        .await
        .expect("migrator");
    all.run(&pool)
        .await
        .expect("the new migration must apply on an already-built database");

    // 3) 新契约的形状在场，旧资产形状不在。
    let column_exists = |table: &'static str, column: &'static str| {
        let pool = pool.clone();
        async move {
            let count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM information_schema.columns WHERE table_schema = 'generation' AND table_name = $1 AND column_name = $2",
            )
            .bind(table)
            .bind(column)
            .fetch_one(&pool)
            .await
            .expect("column probe");
            count == 1
        }
    };
    assert!(column_exists("jobs", "result_images").await);
    assert!(!column_exists("jobs", "result_asset_ids").await);
    assert!(!column_exists("jobs", "asset_bindings").await);
    let assets_table: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.tables WHERE table_schema = 'generation' AND table_name = 'assets'",
    )
    .fetch_one(&pool)
    .await
    .expect("table probe");
    assert_eq!(assets_table, 0, "资产表必须被删掉");

    pool.close().await;
    let _ = std::fs::remove_dir_all(&staged);
    drop_isolated_database(&database_name).await;
}

/// 增量迁移还要能处理**已经存在的老数据**：同一 (vendor, model, revision) 可能已有多行
/// （老形状按内容分叉），迁移必须自己合并，而不是直接失败。
///
/// 这里先在只应用了早期迁移的库上造出这种数据（两行同一个型号、一个供给与一台 Job 指向
/// 较早那一行），再补上整批迁移，确认合并结果与承载面回填都对。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn vendor_model_contract_migration_merges_existing_duplicate_rows() {
    let (database_url, database_name) = isolated_database_url().await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");
    let migrations = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../migrations");

    // 1) 只应用这次改动之前的迁移（`0006` 之前，含上一轮的 `0005`）。
    let staged = std::env::temp_dir().join(format!("seeai-early-migrations-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&staged).expect("staging directory");
    for entry in std::fs::read_dir(&migrations).expect("migrations directory") {
        let entry = entry.expect("migration entry");
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".sql") && name.as_str() < "0006" {
            std::fs::copy(entry.path(), staged.join(&name)).expect("copy early migration");
        }
    }
    sqlx::migrate::Migrator::new(staged.clone())
        .await
        .expect("early migrator")
        .run(&pool)
        .await
        .expect("early migrations apply");

    // 2) 老形状的数据：同一个型号的两行合同（内容不同，老唯一键含内容哈希所以能并存），
    //    供给与 Job 都指向**较早**的那一行。
    let older = Uuid::new_v4();
    let newer = Uuid::new_v4();
    let channel = Uuid::new_v4();
    let offering = Uuid::new_v4();
    let price_plan = Uuid::new_v4();
    let revision = Uuid::new_v4();
    let account = Uuid::new_v4();
    let job = Uuid::new_v4();
    let older_surface = json!({"surface": "older"});
    let newer_surface = json!({"surface": "newer"});
    sqlx::query(
        "INSERT INTO catalog.vendor_models
             (id, vendor_id, native_model_id, native_revision, capability_schema, schema_hash, created_at)
         VALUES ($1,'OpenAI','legacy-model','legacy-revision',$2,'hash-older', now() - interval '1 hour')",
    )
    .bind(older)
    .bind(&older_surface)
    .execute(&pool)
    .await
    .expect("legacy vendor model row");
    sqlx::query(
        "INSERT INTO catalog.vendor_models
             (id, vendor_id, native_model_id, native_revision, capability_schema, schema_hash, created_at)
         VALUES ($1,'OpenAI','legacy-model','legacy-revision',$2,'hash-newer', now())",
    )
    .bind(newer)
    .bind(&newer_surface)
    .execute(&pool)
    .await
    .expect("newer vendor model row");
    sqlx::query(
        "INSERT INTO supply.channels (id, provider_kind, base_url, credential_env)
         VALUES ($1,'AIHubMix','https://api.inferera.com','AIHUBMIX_API_KEY')",
    )
    .bind(channel)
    .execute(&pool)
    .await
    .expect("channel fixture");
    sqlx::query(
        "INSERT INTO supply.offerings
             (id, vendor_model_id, channel_id, adapter_key, provider_model_id, restrictions)
         VALUES ($1,$2,$3,'aihubmix-image-v1','legacy-model','{}'::jsonb)",
    )
    .bind(offering)
    .bind(older)
    .bind(channel)
    .execute(&pool)
    .await
    .expect("offering fixture");
    sqlx::query(
        "INSERT INTO pricing.price_plans
             (id, offering_id, currency, text_input_microusd_per_million, image_input_microusd_per_million,
              text_output_microusd_per_million, image_output_microusd_per_million, source_url, approved_by)
         VALUES ($1,$2,'USD',0,0,0,0,'https://example.invalid/price','migration-test')",
    )
    .bind(price_plan)
    .bind(offering)
    .execute(&pool)
    .await
    .expect("price plan fixture");
    sqlx::query(
        "INSERT INTO publication.runtime_revisions (id, snapshot, published_by)
         VALUES ($1,'{}'::jsonb,'migration-test')",
    )
    .bind(revision)
    .execute(&pool)
    .await
    .expect("runtime revision fixture");
    sqlx::query(
        "INSERT INTO publication.runtime_entries
             (runtime_revision_id, vendor_model_id, offering_id, price_plan_id, gateway_model, active)
         VALUES ($1,$2,$3,$4,'legacy-model',true)",
    )
    .bind(revision)
    .bind(older)
    .bind(offering)
    .bind(price_plan)
    .execute(&pool)
    .await
    .expect("runtime entry fixture");
    sqlx::query("INSERT INTO ledger.accounts (id, balance_microusd) VALUES ($1, 0)")
        .bind(account)
        .execute(&pool)
        .await
        .expect("account fixture");
    sqlx::query(
        "INSERT INTO generation.jobs
             (id, account_id, idempotency_key, request_hash, state, branch, gateway_model,
              native_parameters, runtime_revision_id, vendor_model_id, offering_id, channel_id,
              price_snapshot, max_cost_microusd)
         VALUES ($1,$2,'legacy-job','hash','accepted','prompt_only','legacy-model',
                 '{}'::jsonb,$3,$4,$5,$6,'{}'::jsonb,1)",
    )
    .bind(job)
    .bind(account)
    .bind(revision)
    .bind(older)
    .bind(offering)
    .bind(channel)
    .execute(&pool)
    .await
    .expect("legacy job fixture");

    // 3) 再应用完整迁移集：合并必须自己跑通，不能因为已有重复行就失败。
    sqlx::migrate::Migrator::new(migrations)
        .await
        .expect("migrator")
        .run(&pool)
        .await
        .expect("the merge migration must apply on an already-built database");

    // 4) 合并结果：只留最新那一行；指向被删行的供给 / 条目 / Job 改挂到它。
    let remaining: Vec<(Uuid, Value)> = sqlx::query(
        "SELECT id, capability_schema FROM catalog.vendor_models WHERE native_model_id = 'legacy-model'",
    )
    .fetch_all(&pool)
    .await
    .expect("merged contract rows")
    .iter()
    .map(|row| {
        (
            row.try_get("id").expect("id"),
            row.try_get("capability_schema").expect("contract"),
        )
    })
    .collect();
    assert_eq!(remaining.len(), 1, "duplicate contract rows must be merged");
    assert_eq!(remaining[0].0, newer, "the newest row must survive");
    assert_eq!(remaining[0].1, newer_surface);

    // 承载面按**它当时指向的那一行**补好：老供给与老 Job 读到的仍是它们当时那份面。
    let offering_row =
        sqlx::query("SELECT vendor_model_id, carrier_schema FROM supply.offerings WHERE id = $1")
            .bind(offering)
            .fetch_one(&pool)
            .await
            .expect("offering after merge");
    assert_eq!(
        offering_row
            .try_get::<Uuid, _>("vendor_model_id")
            .expect("vendor model"),
        newer,
        "the offering must be re-pointed at the surviving contract row"
    );
    assert_eq!(
        offering_row
            .try_get::<Value, _>("carrier_schema")
            .expect("carrier"),
        older_surface
    );
    let job_row = sqlx::query(
        "SELECT vendor_model_id, carrier_schema, parameter_mapping FROM generation.jobs WHERE id = $1",
    )
    .bind(job)
    .fetch_one(&pool)
    .await
    .expect("job after merge");
    assert_eq!(
        job_row
            .try_get::<Uuid, _>("vendor_model_id")
            .expect("vendor model"),
        newer
    );
    assert_eq!(
        job_row
            .try_get::<Value, _>("carrier_schema")
            .expect("carrier"),
        older_surface,
        "the job must keep the carrier surface it was accepted with"
    );
    assert_eq!(
        job_row
            .try_get::<Value, _>("parameter_mapping")
            .expect("mapping"),
        json!({})
    );
    let entry_model: Uuid = sqlx::query_scalar(
        "SELECT vendor_model_id FROM publication.runtime_entries WHERE offering_id = $1",
    )
    .bind(offering)
    .fetch_one(&pool)
    .await
    .expect("runtime entry after merge");
    assert_eq!(entry_model, newer);

    // 唯一键与列的形状：内容哈希不再是身份的一部分，它本身也不在了。
    let schema_hash_columns: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.columns
         WHERE table_schema = 'catalog' AND table_name = 'vendor_models' AND column_name = 'schema_hash'",
    )
    .fetch_one(&pool)
    .await
    .expect("schema_hash probe");
    assert_eq!(schema_hash_columns, 0, "内容哈希不再参与身份");
    let duplicate_insert = sqlx::query(
        "INSERT INTO catalog.vendor_models
             (id, vendor_id, native_model_id, native_revision, capability_schema)
         VALUES ($1,'OpenAI','legacy-model','legacy-revision','{}'::jsonb)",
    )
    .bind(Uuid::new_v4())
    .execute(&pool)
    .await;
    assert!(
        duplicate_insert.is_err(),
        "the unique key must be (vendor, model, revision) now"
    );

    pool.close().await;
    let _ = std::fs::remove_dir_all(&staged);
    drop_isolated_database(&database_name).await;
}

fn candidate(provider_kind: &str, adapter_key: &str, branches: &[&str]) -> Value {
    // Profile 必须与声明的分支自洽：声明 `image_conditioned` 就要有参考图字段，
    // 声明 `masked` 就要有遮罩字段。发布期会拒绝不自洽的声明（限制只能收窄）。
    //
    // 字段名还必须落在该 Adapter 声明的参数面里，而两家用的原生名不同：
    // AIHubMix 收 `image` / `mask`，APIMart 收 `image_urls` / `mask_url`。
    let mut properties = json!({
        "model": {"const": "placeholder"},
        "prompt": {"type": "string", "minLength": 1}
    });
    let declares_image = branches
        .iter()
        .any(|branch| matches!(*branch, "image_conditioned" | "masked"));
    let vendor_names = adapter_key == "apimart-image-v1";
    let image_parameter = if vendor_names { "image_urls" } else { "image" };
    let mask_parameter = if vendor_names { "mask_url" } else { "mask" };
    if declares_image {
        properties[image_parameter] = if vendor_names {
            json!({
                "type": "array",
                "items": {"type": "string"},
                "minItems": 1,
                "maxItems": 1
            })
        } else {
            json!({"type": "string"})
        };
    }
    if branches.contains(&"masked") {
        properties[mask_parameter] = json!({"type": "string"});
    }
    // 遮罩不能脱离参考图：Profile 自己也要这么声明，否则它会把"只有遮罩"的请求
    // 判成合法（AIHubMix 的发布期校验会据此拒绝这份 Profile）。
    let mut schema = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["model", "prompt"],
        "properties": properties
    });
    if declares_image && branches.contains(&"masked") {
        schema["allOf"] = json!([{
            "if": {"required": [mask_parameter]},
            "then": {"required": [image_parameter]}
        }]);
    }
    json!({
        "provider_kind": provider_kind,
        "adapter_key": adapter_key,
        "provider_model_id": "route-model",
        "base_url": "http://127.0.0.1:1",
        "credential_env": "AIHUBMIX_API_KEY",
        "restrictions": {
            "allowed_branches": branches,
            // 收图上限也要与 Profile 自洽：纯文生图只声明 prompt，收图数就是 0。
            "max_images": if declares_image { 1 } else { 0 }
        },
        "capability_schema": schema,
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

/// 发布一组候选。`contract` 给 `Some` 时用它当**模型级合同**（新形状素材顶层那一份）；
/// 给 `None` 时沿用旧形状：候选自己那份 `capability_schema` 既是承载面也是合同。
async fn publish_candidates(
    client: &Client,
    base_url: &str,
    admin_token: &str,
    model: &str,
    contract: Option<Value>,
    offerings: Vec<Value>,
) -> StatusCode {
    publish_candidates_with_markup(
        client,
        base_url,
        admin_token,
        model,
        contract,
        offerings,
        None,
    )
    .await
}

/// 同 `publish_candidates`，但可以带上**修订级**的加价系数。
///
/// 加价系数是定价的**参考口径**，可以不给：管理员直接录入对客费率向量时它不参与计算。
async fn publish_candidates_with_markup(
    client: &Client,
    base_url: &str,
    admin_token: &str,
    model: &str,
    contract: Option<Value>,
    offerings: Vec<Value>,
    markup_bps: Option<i32>,
) -> StatusCode {
    let body = publication_body(model, "route-test-1", contract, offerings, markup_bps);
    client
        .post(format!("{base_url}/api/v1/runtime-revisions"))
        .bearer_auth(admin_token)
        .json(&body)
        .send()
        .await
        .expect("runtime publication")
        .status()
}

/// 在**指定修订**上发布一组候选（用夹具那台 API 与它的管理员令牌）。
///
/// 合同行不可变：同一个 (厂商, 型号, 修订) 只落一行，内容不同就拒——所以要发一份**不同的合同**
/// 必须换修订号；同一个修订号重发只允许内容逐字相同。
async fn publish_on_revision(
    harness: &Harness,
    model: &str,
    revision: &str,
    contract: Value,
    offerings: Vec<Value>,
    markup_bps: Option<i32>,
) -> StatusCode {
    let body = publication_body(model, revision, Some(contract), offerings, markup_bps);
    Client::new()
        .post(format!("{}/api/v1/runtime-revisions", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .json(&body)
        .send()
        .await
        .expect("runtime publication")
        .status()
}

/// 拼一份发布命令：线上名以**承载面**为准，`model.const` 与 `provider_model_id` 都跟着本次型号走。
///
/// 旧形状的素材没有承载面，那份 `capability_schema` 就是它，两者落在同一处；带模型级合同时，
/// 合同里的 `model.const` 也必须等于 `native_model_id`（发布期的硬判据）。
fn publication_body(
    model: &str,
    revision: &str,
    contract: Option<Value>,
    mut offerings: Vec<Value>,
    markup_bps: Option<i32>,
) -> Value {
    for offering in &mut offerings {
        let surface = if offering
            .get("carrier_schema")
            .is_some_and(|value| !value.is_null())
        {
            &mut offering["carrier_schema"]
        } else {
            &mut offering["capability_schema"]
        };
        surface["properties"]["model"]["const"] = Value::String(model.to_owned());
        offering["provider_model_id"] = Value::String(model.to_owned());
    }
    let mut body = json!({
        "vendor_id": "OpenAI",
        "native_model_id": model,
        "native_revision": revision,
        "actor": "contract-test",
        "offerings": offerings
    });
    if let Some(mut contract) = contract {
        contract["properties"]["model"]["const"] = Value::String(model.to_owned());
        body["capability_schema"] = contract;
    }
    if let Some(markup_bps) = markup_bps {
        body["markup_bps"] = json!(markup_bps);
    }
    body
}

/// 测试构造体：模型 + 提示词（幂等键另走请求头）。
fn route_request(model: &str, prompt: &str) -> Value {
    json!({"model": model, "prompt": prompt})
}

/// 定价断言用的**对客四档 CNY 费率向量**（每 1M tokens）。
///
/// 取值故意都高于该渠道的成本费率折算成人民币之后的样子（USD 费率 5/8/10/30 折 7.1 之后约
/// 35/57/71/213），这样毛利是正的——用例要验的是"售价 − 成本折算后可逐笔算出"，毛利为负也
/// 能算，但正数更能看出方向。
fn priced_consumer_rates() -> Value {
    json!({
        "text_input_micros_per_million": 40_000_000,
        "image_input_micros_per_million": 64_000_000,
        "text_output_micros_per_million": 80_000_000,
        "image_output_micros_per_million": 220_000_000
    })
}

/// OpenAI 系当前的保底表形态：只按 `size` 填，`quality` 维留空备用。
fn openai_floor_amounts() -> Value {
    json!({
        "amounts": {"1K": 160_000, "2K": 250_000, "4K": 300_000},
        "cap_microusd": 300_000
    })
}

/// 一个**付得起**的账户与 Key。
///
/// 夹具自带的账户余额只有 ¥0.10，比定价候选的保底额（¥0.16 起）还小——那本身是对的
/// （受理闸门就是"余额 ≥ 保底额"），但要验售价与结算，得先有一个余额充足的账户。
async fn funded_account(
    client: &Client,
    base_url: &str,
    admin_token: &str,
    credit_microusd: u64,
) -> (String, String) {
    let account_id =
        create_account_with_credit(client, base_url, admin_token, credit_microusd).await;
    let api_key = issue_key(client, base_url, admin_token, &account_id).await;
    (account_id, api_key)
}

/// 这次受理冻结下来的**定价快照**（`generation.jobs.price_snapshot`）。
async fn frozen_snapshot(pool: &PgPool, key: &str) -> Value {
    sqlx::query_scalar("SELECT price_snapshot FROM generation.jobs WHERE idempotency_key = $1")
        .bind(key)
        .fetch_one(pool)
        .await
        .expect("the request must have created a job with a frozen snapshot")
}

/// 合同里的文生图请求体（bootstrap 素材的模型名）。
fn generation_request_body(prompt: &str) -> Value {
    json!({"model": "gpt-image-2.5-flare", "prompt": prompt, "n": 1, "quality": "low"})
}

/// 带幂等键的测试构造体：受理时把它提到 `Idempotency-Key` 请求头。
fn generation_request(idempotency_key: &str, prompt: &str) -> Value {
    let mut body = generation_request_body(prompt);
    body["idempotency_key"] = Value::String(idempotency_key.to_owned());
    body
}

/// 去掉构造体里的幂等键：它对客不该出现在请求体里。
fn strip_key(body: &Value) -> Value {
    let mut body = body.clone();
    if let Some(object) = body.as_object_mut() {
        object.remove("idempotency_key");
    }
    body
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
    create_account_with_credit(client, base_url, admin_token, 100_000).await
}

/// 指定初始余额的账户（余额不足的用例要一个"钱不够"的账户）。
async fn create_account_with_credit(
    client: &Client,
    base_url: &str,
    admin_token: &str,
    initial_credit_microusd: u64,
) -> String {
    let response = client
        .post(format!("{base_url}/api/v1/accounts"))
        .bearer_auth(admin_token)
        .json(&json!({"initial_credit_microusd": initial_credit_microusd}))
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
        .post(format!("{base_url}/api/v1/accounts/{account_id}/api-keys"))
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

/// 发布一份 2.5 素材：对客面的用例都按它的合同提交（模型名见 `generation_request_body`）。
async fn publish_bootstrap(client: &Client, base_url: &str, admin_token: &str) {
    let config: Value = serde_json::from_str(include_str!(
        "../../../config/bootstrap/gpt-image-2.5-flare.json"
    ))
    .expect("bootstrap config");
    let response = client
        .post(format!("{base_url}/api/v1/runtime-revisions"))
        .bearer_auth(admin_token)
        .json(&config)
        .send()
        .await
        .expect("runtime publication");
    assert_eq!(response.status(), StatusCode::OK);
}

/// 型号身份对不上时必须被发布期拒掉：把合同里的 `model.const` 与 `native_model_id` 拆开。
async fn reject_mismatched_model_identity(client: &Client, base_url: &str, admin_token: &str) {
    let mut config: Value = serde_json::from_str(include_str!(
        "../../../config/bootstrap/gpt-image-2.5-flare.json"
    ))
    .expect("bootstrap config");
    config["native_model_id"] = Value::String("different-model".to_owned());
    let response = client
        .post(format!("{base_url}/api/v1/runtime-revisions"))
        .bearer_auth(admin_token)
        .json(&config)
        .send()
        .await
        .expect("mismatched publication");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

async fn verify_reconciliation_contract(
    client: &Client,
    base_url: &str,
    admin_token: &str,
    api_key: &str,
    database_url: &str,
) {
    let key = format!("reconciliation-{}", Uuid::new_v4());
    // 没有 Worker：同步入口等到超时，但内部记录已经建好了——正是这里的夹具。
    let (status, body) = post_json(
        base_url,
        api_key,
        "/v1/images/generations",
        &key,
        &generation_request_body("reconciliation contract"),
    )
    .await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "got {body}");
    let pool = PgPool::connect(database_url)
        .await
        .expect("contract database");
    let job_id: Uuid =
        sqlx::query_scalar("SELECT id FROM generation.jobs WHERE idempotency_key = $1")
            .bind(&key)
            .fetch_one(&pool)
            .await
            .expect("reconciliation job");
    let attempt_id = Uuid::new_v4();
    let case_id = Uuid::new_v4();
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
        .get(format!("{base_url}/api/v1/reconciliation-cases"))
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
            "{base_url}/api/v1/reconciliation-cases/{job_id}/refund"
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
                "{base_url}/api/v1/reconciliation-cases/{job_id}/refund"
            ))
            .bearer_auth(admin_token)
            .json(&refund)
            .send()
            .await
            .expect("case resolution");
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
    let row = sqlx::query(
        "SELECT j.state, j.error_code, j.failure_kind, h.status, h.amount_microusd, a.balance_microusd FROM generation.jobs j JOIN ledger.holds h ON h.job_id = j.id JOIN ledger.accounts a ON a.id = j.account_id WHERE j.id = $1",
    )
    .bind(job_id)
    .fetch_one(&pool)
    .await
    .expect("resolved state");
    assert_eq!(row.get::<String, _>("state"), "failed");
    // 退款是一次平台侧处置，不是"消费者的错"：对客码必须仍在白名单内，
    // 且退款已结清，不该再让消费者"等对账结论"。
    assert_eq!(row.get::<String, _>("error_code"), "platform_unavailable");
    assert_eq!(row.get::<String, _>("failure_kind"), "platform_internal");
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
    pool.close().await;
}

async fn verify_lease_recovery_contract(
    client: &Client,
    base_url: &str,
    admin_token: &str,
    api_key: &str,
    database_url: &str,
) {
    // 两台任务：一台停在"已领取"，一台停在"已提交"。都没有 Worker。
    let mut jobs = Vec::new();
    for prompt in [
        "lease recovery before submit",
        "lease recovery after submit",
    ] {
        let key = format!("lease-{}-{}", Uuid::new_v4(), jobs.len());
        let (status, body) = post_json(
            base_url,
            api_key,
            "/v1/images/generations",
            &key,
            &generation_request_body(prompt),
        )
        .await;
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "got {body}");
        let pool = PgPool::connect(database_url)
            .await
            .expect("contract database");
        let job_id: Uuid =
            sqlx::query_scalar("SELECT id FROM generation.jobs WHERE idempotency_key = $1")
                .bind(&key)
                .fetch_one(&pool)
                .await
                .expect("recovery job");
        pool.close().await;
        jobs.push(job_id);
    }
    let leased_job_id = jobs[0];
    let submitted_job_id = jobs[1];
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

    // 租约过期是平台自己的事件，不是渠道事件：它同样必须出现在平台侧失败清单里。
    let failure_kind: Option<String> =
        sqlx::query_scalar("SELECT failure_kind FROM generation.jobs WHERE id = $1")
            .bind(submitted_job_id)
            .fetch_one(&pool)
            .await
            .expect("recovery failure kind");
    assert_eq!(failure_kind.as_deref(), Some("platform_internal"));
    let internal: Value = client
        .get(format!(
            "{base_url}/api/v1/provider-failures?kind=platform_internal"
        ))
        .bearer_auth(admin_token)
        .send()
        .await
        .expect("platform internal failures")
        .json()
        .await
        .expect("platform internal failures JSON");
    let entry = internal
        .get("failures")
        .and_then(Value::as_array)
        .and_then(|list| {
            list.iter()
                .find(|entry| entry["job_id"].as_str() == Some(&submitted_job_id.to_string()))
        })
        .unwrap_or_else(|| panic!("租约过期的 Job 必须能被按类别查到：{internal}"));
    assert_eq!(entry["error_code"].as_str(), Some("outcome_unknown"));

    let repeated = repository
        .recover_expired_leases()
        .await
        .expect("repeated lease recovery");
    assert_eq!(repeated.returned_to_queue, 0);
    assert_eq!(repeated.sent_to_reconciliation, 0);
    pool.close().await;
}

/// 取一次对客目录（`GET /v1/models`）。
///
/// `api_key` 给 `None` 就**一个鉴权头都不带**：目录是公开的，这才是调用方最常见的取法。
async fn get_catalog(
    client: &Client,
    base_url: &str,
    api_key: Option<&str>,
) -> (StatusCode, Value) {
    let request = client.get(format!("{base_url}/v1/models"));
    let request = match api_key {
        Some(key) => request.bearer_auth(key),
        None => request,
    };
    let response = request.send().await.expect("catalog request");
    let status = response.status();
    let raw = response.text().await.expect("catalog body");
    (
        status,
        serde_json::from_str(&raw).unwrap_or(Value::String(raw)),
    )
}

/// 对客目录里那条合同该长什么样：**发布的那一份**，只有型号身份那个常量换成对客名。
///
/// 存的那份合同不动（合同行不可变），因此目录与"库里那份"的差别**只有这一个字段**——
/// 用例按这条判据比对，就不会把"合同被改过"漏过去。
///
/// 复用生产同一个助手，而不是自己按下标赋值：下标赋值在缺键时会**凭空造键**，于是
/// "生产替换了"与"生产没替换"都能与期望值相等，用例就钉不住这条规则了。
fn consumer_contract(mut contract: Value, gateway_model: &str) -> Value {
    replace_contract_model_identity(&mut contract, gateway_model);
    contract
}

/// 目录里列出的模型名，按目录顺序。
fn catalog_names(catalog: &Value) -> Vec<String> {
    catalog["data"]
        .as_array()
        .expect("catalog data is an array")
        .iter()
        .map(|entry| {
            entry["name"]
                .as_str()
                .expect("every catalog entry has a name")
                .to_owned()
        })
        .collect()
}

/// 取一次管理员网关模型清单（`GET /api/v1/gateway-models`）。
///
/// `admin_token` 给 `None` 就一个鉴权头都不带：这条视图是**运营视图**，与公开的对客目录不是
/// 一回事，所以这里必须能看出它要凭证。
async fn get_gateway_models(
    client: &Client,
    base_url: &str,
    admin_token: Option<&str>,
) -> (StatusCode, Value) {
    let request = client.get(format!("{base_url}/api/v1/gateway-models"));
    let request = match admin_token {
        Some(token) => request.bearer_auth(token),
        None => request,
    };
    let response = request.send().await.expect("gateway model request");
    let status = response.status();
    let raw = response.text().await.expect("gateway model body");
    (
        status,
        serde_json::from_str(&raw).unwrap_or(Value::String(raw)),
    )
}

/// 改一次运维开关（`PATCH /api/v1/gateway-models/{name}`）。
async fn patch_gateway_model(
    client: &Client,
    base_url: &str,
    admin_token: &str,
    gateway_model: &str,
    enabled: bool,
) -> StatusCode {
    client
        .patch(format!("{base_url}/api/v1/gateway-models/{gateway_model}"))
        .bearer_auth(admin_token)
        .json(&json!({"enabled": enabled}))
        .send()
        .await
        .expect("gateway model patch")
        .status()
}

/// 一份**对客名与厂商原生名不同**的素材：厂商原生名取 sunburst，对客名取 plus。
///
/// 只留 AIHubMix 那一条候选：这条用例只跑一家渠道，另一条留在这里会多一个用不到的假上游。
fn renamed_material(aihubmix_upstream: &str) -> Value {
    let mut material: Value = serde_json::from_str(include_str!(
        "../../../config/bootstrap/gpt-image-2.5-sunburst.json"
    ))
    .expect("2.5 material parses");
    material["gateway_model"] = Value::String("gpt-image-2.5-plus".to_owned());
    let aihubmix = material["offerings"][0].clone();
    material["offerings"] = json!([aihubmix]);
    material["offerings"][0]["base_url"] = Value::String(aihubmix_upstream.to_owned());
    material
}

/// 对客目录：`GET /v1/models` 只列**当前真的能调**的型号，合同就是发布的那一份。
///
/// 目录**公开**：不带任何鉴权头就能取，乱给的 Key 也不会把它变成 401——调用方要先知道有哪些
/// 型号、各自的参数面，才建得出表单。
/// 判据与受理期选路**同一条**（生效的发布条目 + 启用的供给 + 启用的渠道）：目录里列出的型号
/// 必须真的提交得起来。列着却提交不了比不列更糟——调用方会照它建表单，然后在提交时落空。
/// 本用例只读发布物与目录，不起 Worker、不连上游：零外部调用。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_model_catalog_lists_only_callable_models_with_their_published_contract() {
    let (database_url, database_name) = isolated_database_url().await;
    // 同步入口在这个用例里只用来验"停用之后真的调不了"；那一步在受理前就失败，不会等超时。
    let (base_url, admin_token, _process) = start_api(&database_url, 1, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url).await;
    let account = create_account(&client, &base_url, &admin_token).await;
    let api_key = issue_key(&client, &base_url, &admin_token, &account).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    // ── 目录公开：不带任何鉴权头也 200；乱给的 Key 同样不影响它 ──
    let (status, catalog) = get_catalog(&client, &base_url, None).await;
    assert_eq!(status, StatusCode::OK, "公开目录不该要 Key：{catalog}");
    assert_public_only("无鉴权头的目录请求", &catalog);
    assert_eq!(
        catalog,
        json!({"data": []}),
        "还没发布任何型号时，目录是空列表而不是错误：{catalog}"
    );
    let (status, catalog) = get_catalog(&client, &base_url, Some("sk_seeai_not_a_real_key")).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "目录根本不读 Authorization，乱给的 Key 也不该被拒：{catalog}"
    );

    // 两个型号、两份不同的合同：目录里每一条都必须带**它自己**那份，且逐字一致。
    let model = "catalog-model-a";
    let other = "catalog-model-b";
    let contract = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1}
    }));
    let other_contract = surface_schema(json!({
        "model": {"const": other},
        "prompt": {"type": "string", "minLength": 1},
        "quality": {"type": "string", "enum": ["low", "high"]}
    }));
    let published = [
        (model, "catalog-a-1", &contract),
        (other, "catalog-b-1", &other_contract),
    ];
    for (name, revision, schema) in published {
        let status = publish_with_surfaces(
            &client,
            &base_url,
            &admin_token,
            name,
            revision,
            schema.clone(),
            vec![("aihubmix-image-v1", schema.clone())],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{name} 必须发布成功");
    }

    // ── 两个型号都在，形状是 `{name, vendor_id, revision, contract}`；照旧不带鉴权头 ──
    let (status, catalog) = get_catalog(&client, &base_url, None).await;
    assert_eq!(status, StatusCode::OK, "got {catalog}");
    assert_public_only("目录", &catalog);
    assert_eq!(
        catalog.as_object().map(serde_json::Map::len),
        Some(1),
        "目录顶层只有 data：{catalog}"
    );
    let entries = catalog["data"].as_array().expect("data is an array");
    assert_eq!(entries.len(), 2, "两个在售型号都要在目录里：{catalog}");
    for (name, revision, schema) in published {
        let entry = entries
            .iter()
            .find(|entry| entry["name"].as_str() == Some(name))
            .unwrap_or_else(|| panic!("`{name}` 必须在目录里：{catalog}"));
        let mut keys: Vec<&str> = entry
            .as_object()
            .expect("entry is an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec!["contract", "name", "revision", "vendor_id"],
            "目录条目只有 name / vendor_id / revision / contract 四个字段：{entry}"
        );
        assert_eq!(entry["vendor_id"].as_str(), Some("OpenAI"));
        assert_eq!(entry["revision"].as_str(), Some(revision));
        // 厂商原生名不进对客面：这两个型号的对客名恰好等于原生名，因此这里只钉住"响应里
        // 没有 native_model_id 这个**字段**"；名字不同时"正文不含原生名"由专门的用例钉。
        assert!(
            entry.get("native_model_id").is_none(),
            "对客目录不许出现 native_model_id：{entry}"
        );
        assert_eq!(
            &entry["contract"], schema,
            "目录里的合同必须与发布的那一份逐字一致（对客名与原生名同值时逐字相同）"
        );
    }

    // ── 停用供给：该型号从目录里消失，提交也确实取不到候选 ──
    sqlx::query(
        "UPDATE supply.offerings SET enabled = false
         WHERE vendor_model_id = (SELECT id FROM catalog.vendor_models WHERE native_model_id = $1)",
    )
    .bind(model)
    .execute(&pool)
    .await
    .expect("disable the offering");
    let (_, catalog) = get_catalog(&client, &base_url, None).await;
    let names: Vec<&str> = catalog["data"]
        .as_array()
        .expect("data is an array")
        .iter()
        .filter_map(|entry| entry["name"].as_str())
        .collect();
    assert_eq!(names, vec![other], "停用的型号必须从目录里消失：{catalog}");
    let key = format!("catalog-disabled-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &route_request(model, "disabled model"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "目录不列的型号，受理期同样取不到候选：{body}"
    );

    // ── 渠道停用与供给停用是**同一条**判据：它同样让型号从目录里消失 ──
    sqlx::query(
        "UPDATE supply.channels SET enabled = false
         WHERE id = (SELECT o.channel_id FROM supply.offerings o
                     JOIN catalog.vendor_models vm ON vm.id = o.vendor_model_id
                     WHERE vm.native_model_id = $1)",
    )
    .bind(other)
    .execute(&pool)
    .await
    .expect("disable the channel");
    let (status, catalog) = get_catalog(&client, &base_url, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        catalog,
        json!({"data": []}),
        "供给与渠道全停用后，目录是空列表而不是错误：{catalog}"
    );

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// 命名层：对客面只出现**平台网关模型名**，厂商原生名留在管理端。
///
/// 素材刻意让两个名字不同（厂商 `gpt-image-2.5-sunburst`、对客 `gpt-image-2.5-plus`），
/// 一次把命名层的几条硬约束都钉住：
/// - 素材把**对客名**写进合同正文会被发布期拒掉（合同正文那个常量是厂商模型的身份）；
/// - 目录的 `name` 是对客名、带 `vendor_id`、**正文全文不含**厂商原生名（含合同正文）；
/// - 用对客名能真的受理（假上游跑通），用厂商原生名是"模型不存在"；
/// - 存的那份合同不动：库里 `model.const` 仍是厂商原生名，只有投射给调用方时替换；
/// - 运维开关一关，目录与受理**同时**消失，管理端照样列得出来（否则关了就没法打开）。
///
/// 零外部调用：假上游在进程内，凭证只从环境变量读。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn gateway_model_naming_keeps_the_vendor_name_off_the_consumer_surface() {
    const NATIVE: &str = "gpt-image-2.5-sunburst";
    const GATEWAY: &str = "gpt-image-2.5-plus";

    let (database_url, database_name) = isolated_database_url().await;
    let client = Client::new();
    let calls: UpstreamCalls = Arc::new(Mutex::new(Vec::new()));
    let upstream = start_fake_upstream_with(
        calls.clone(),
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
    )
    .await;
    let (base_url, admin_token, _process) = start_api(&database_url, 60, 64).await;
    wait_until_ready(&client, &base_url).await;
    let account = create_account(&client, &base_url, &admin_token).await;
    let api_key = issue_key(&client, &base_url, &admin_token, &account).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");
    let material = renamed_material(&upstream.base_url);

    // ── 素材把**对客名**写进合同正文 → 发布期拒掉 ──
    //
    // 这条同时是"响应全文不含原生名"可判定的前提：合同正文里没有第二个模型名来源。
    let mut misnamed = material.clone();
    misnamed["capability_schema"]["properties"]["model"]["const"] =
        Value::String(GATEWAY.to_owned());
    let rejected = client
        .post(format!("{base_url}/api/v1/runtime-revisions"))
        .bearer_auth(&admin_token)
        .json(&misnamed)
        .send()
        .await
        .expect("misnamed publication");
    assert_eq!(
        rejected.status(),
        StatusCode::BAD_REQUEST,
        "合同正文写对客名必须被拒：{:?}",
        rejected.text().await
    );

    // ── 正常发布：厂商原生名写进合同正文，对客名写在顶层 ──
    let published = client
        .post(format!("{base_url}/api/v1/runtime-revisions"))
        .bearer_auth(&admin_token)
        .json(&material)
        .send()
        .await
        .expect("publication request");
    let status = published.status();
    let body = published.text().await.expect("publication body");
    assert_eq!(status, StatusCode::OK, "素材必须能发布：{body}");

    // ── 对客目录：name 是对客名，带 vendor_id，正文全文不含厂商原生名 ──
    let raw = client
        .get(format!("{base_url}/v1/models"))
        .send()
        .await
        .expect("catalog request")
        .text()
        .await
        .expect("catalog text");
    assert!(
        !raw.contains(NATIVE),
        "对客目录正文不许出现厂商原生名（含合同正文）：{raw}"
    );
    let catalog: Value = serde_json::from_str(&raw).expect("catalog JSON");
    assert_eq!(
        catalog_names(&catalog),
        vec![GATEWAY.to_owned()],
        "{catalog}"
    );
    let entry = &catalog["data"][0];
    assert_eq!(entry["vendor_id"].as_str(), Some("OpenAI"));
    assert_eq!(entry["revision"], material["native_revision"]);
    assert!(
        entry.get("native_model_id").is_none(),
        "对客目录不许出现 native_model_id：{entry}"
    );
    assert_eq!(
        entry["contract"],
        consumer_contract(material["capability_schema"].clone(), GATEWAY),
        "目录里的合同是发布的那一份，只有 model.const 换成对客名"
    );
    assert_eq!(entry["contract"]["properties"]["model"]["const"], GATEWAY);

    // ── 存的那份合同**不动**：库里那个常量仍是厂商原生名 ──
    let stored_contract: Value = sqlx::query_scalar(
        "SELECT capability_schema FROM catalog.vendor_models WHERE native_model_id = $1",
    )
    .bind(NATIVE)
    .fetch_one(&pool)
    .await
    .expect("stored contract");
    assert_eq!(
        stored_contract["properties"]["model"]["const"], NATIVE,
        "合同行不可变：替换只发生在投射那一步"
    );
    let vendor_model_id: Uuid =
        sqlx::query_scalar("SELECT id FROM catalog.vendor_models WHERE native_model_id = $1")
            .bind(NATIVE)
            .fetch_one(&pool)
            .await
            .expect("vendor model id");

    // ── 管理员读：一条网关模型一项，带候选清单与运维开关 ──
    let (unauthorized, _) = get_gateway_models(&client, &base_url, None).await;
    assert_eq!(
        unauthorized,
        StatusCode::UNAUTHORIZED,
        "运营视图要管理员凭证，与公开的对客目录不是一回事"
    );
    let (status, admin) = get_gateway_models(&client, &base_url, Some(&admin_token)).await;
    assert_eq!(status, StatusCode::OK, "{admin}");
    assert_eq!(
        admin.as_object().map(serde_json::Map::len),
        Some(1),
        "管理端清单只有 gateway_models 一个顶层字段：{admin}"
    );
    let listed = admin["gateway_models"]
        .as_array()
        .expect("gateway_models is an array");
    assert_eq!(listed.len(), 1, "{admin}");
    let view = &listed[0];
    assert_eq!(view["gateway_model"], GATEWAY);
    assert_eq!(view["enabled"], true);
    assert_eq!(view["vendor_id"], "OpenAI");
    assert_eq!(view["native_model_id"], NATIVE, "厂商原生名只在管理端出现");
    assert_eq!(view["native_revision"], material["native_revision"]);
    assert!(
        view["runtime_revision_id"].as_str().is_some(),
        "要能看出这是哪一次发布：{view}"
    );
    assert!(
        view["published_at"].as_str().is_some(),
        "要能看出这次发布是什么时候发的：{view}"
    );
    let candidates = view["candidates"].as_array().expect("candidates");
    assert_eq!(candidates.len(), 1, "一条候选：{view}");
    assert_eq!(candidates[0]["provider_kind"], "AIHubMix");
    assert_eq!(candidates[0]["provider_model_id"], NATIVE);
    assert_eq!(candidates[0]["adapter_key"], "aihubmix-image-v1");
    assert_eq!(candidates[0]["routing_priority"], 0);
    assert_eq!(candidates[0]["enabled"], true);
    assert!(
        candidates[0]["offering_id"].as_str().is_some(),
        "候选要能被指认：{view}"
    );
    assert!(
        candidates[0]["carrier_schema"].is_object()
            && candidates[0]["parameter_mapping"].is_object(),
        "候选自带承载面与映射，不必直查库：{view}"
    );
    assert!(
        candidates[0].get("credential_env").is_none(),
        "不回显渠道凭证：{view}"
    );

    // ── 用**对客名**受理：假上游真跑通；上行给渠道的仍是厂商原生名 ──
    let _worker = spawn_worker_process(&database_url);
    let key = format!("naming-gateway-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &route_request(GATEWAY, "命名层：按对客名受理"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "对客名必须真的能受理：{body}");
    assert_sync_success("按对客名受理", &body);
    assert_eq!(
        count_calls(&calls, "POST", "/v1/images/generations"),
        1,
        "请求要真的发到假上游"
    );
    let submit = last_submit_body(&calls, "/v1/images/generations");
    assert_eq!(
        submit["model"], NATIVE,
        "上行给渠道的是厂商原生名，不是对客名：{submit}"
    );
    let stored_model: String =
        sqlx::query_scalar("SELECT gateway_model FROM generation.jobs WHERE idempotency_key = $1")
            .bind(&key)
            .fetch_one(&pool)
            .await
            .expect("job gateway model");
    assert_eq!(stored_model, GATEWAY, "Job 固化的是对客名");
    let (revision_gateway, revision_vendor_model): (String, Uuid) = {
        let row = sqlx::query(
            "SELECT gateway_model, vendor_model_id FROM publication.runtime_revisions
             WHERE id = (SELECT runtime_revision_id FROM publication.runtime_entries
                         WHERE active AND gateway_model = $1 LIMIT 1)",
        )
        .bind(GATEWAY)
        .fetch_one(&pool)
        .await
        .expect("runtime revision naming columns");
        (
            row.try_get("gateway_model").expect("gateway model"),
            row.try_get("vendor_model_id").expect("vendor model id"),
        )
    };
    assert_eq!(revision_gateway, GATEWAY, "修订上记的是对客名");
    assert_eq!(
        revision_vendor_model, vendor_model_id,
        "修订指向它挂的那行合同"
    );

    // ── 用**厂商原生名**受理：模型不存在 ──
    let native_key = format!("naming-native-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &native_key,
        &route_request(NATIVE, "命名层：按厂商原生名受理"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "厂商原生名不是对客身份，受理期取不到候选：{body}"
    );
    assert_eq!(
        count_calls(&calls, "POST", "/v1/images/generations"),
        1,
        "被拒的请求不该发到上游"
    );

    // ── 运维开关：关掉之后目录与受理同时消失，管理端照样列得出来 ──
    assert_eq!(
        patch_gateway_model(&client, &base_url, &admin_token, GATEWAY, false).await,
        StatusCode::NO_CONTENT
    );
    let (_, catalog) = get_catalog(&client, &base_url, None).await;
    assert_eq!(
        catalog,
        json!({"data": []}),
        "关掉的模型从目录里消失：{catalog}"
    );
    let off_key = format!("naming-off-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &off_key,
        &route_request(GATEWAY, "命名层：关掉之后受理"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "关掉的模型受理得到'模型不存在'：{body}"
    );
    let (_, admin) = get_gateway_models(&client, &base_url, Some(&admin_token)).await;
    assert_eq!(
        admin["gateway_models"][0]["gateway_model"], GATEWAY,
        "关掉的模型照样列得出来，否则关了就没法打开：{admin}"
    );
    assert_eq!(admin["gateway_models"][0]["enabled"], false);
    let audits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM operations.audit_events
         WHERE action = 'gateway_model.set_enabled' AND subject_id = $1",
    )
    .bind(GATEWAY)
    .fetch_one(&pool)
    .await
    .expect("audit events");
    assert_eq!(audits, 1, "PATCH 要写出一条审计事件");

    // ── 重新启用：目录与受理都恢复 ──
    assert_eq!(
        patch_gateway_model(&client, &base_url, &admin_token, GATEWAY, true).await,
        StatusCode::NO_CONTENT
    );
    let (_, catalog) = get_catalog(&client, &base_url, None).await;
    assert_eq!(
        catalog_names(&catalog),
        vec![GATEWAY.to_owned()],
        "重新启用后回到目录：{catalog}"
    );
    let on_key = format!("naming-on-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &on_key,
        &route_request(GATEWAY, "命名层：重新启用之后受理"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "重新启用后必须能受理：{body}");
    assert_sync_success("重新启用后受理", &body);

    // ── 没发布过的名字：404，而且不留下任何审计事件 ──
    let unknown = format!("never-published-{}", Uuid::new_v4());
    assert_eq!(
        patch_gateway_model(&client, &base_url, &admin_token, &unknown, false).await,
        StatusCode::NOT_FOUND,
        "没发布过的名字是'不存在'，不是'待创建'"
    );
    let unknown_audits: i64 =
        sqlx::query_scalar("SELECT count(*) FROM operations.audit_events WHERE subject_id = $1")
            .bind(&unknown)
            .fetch_one(&pool)
            .await
            .expect("audit events");
    assert_eq!(unknown_audits, 0, "被拒的 PATCH 不该留下审计事件");

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// **旧素材**（不带对客名）照常可发布：对客名回退取厂商原生名，行为与今天逐位一致。
///
/// 这是命名层"缺省回退"那条兼容承诺的证据：仓库里唯一不带 `gateway_model` 的素材发布之后，
/// 目录的 `name` 就是厂商原生名，整条目录响应与今天逐字相同，受理也照旧跑得通。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn legacy_material_without_a_gateway_name_falls_back_to_the_vendor_name() {
    let (database_url, database_name) = isolated_database_url().await;
    let client = Client::new();
    let calls: UpstreamCalls = Arc::new(Mutex::new(Vec::new()));
    let upstream = start_fake_upstream_with(
        calls.clone(),
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
    )
    .await;
    let (base_url, admin_token, _process) = start_api(&database_url, 60, 64).await;
    wait_until_ready(&client, &base_url).await;
    let account = create_account(&client, &base_url, &admin_token).await;
    let api_key = issue_key(&client, &base_url, &admin_token, &account).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    let material: Value = serde_json::from_str(include_str!(
        "../../../config/bootstrap/aihubmix-gpt-image-2.json"
    ))
    .expect("legacy material parses");
    assert!(
        material.get("gateway_model").is_none(),
        "这份夹具刻意不带对客名，缺省回退才有的可验"
    );
    let mut command = material.clone();
    command["offerings"][0]["base_url"] = Value::String(upstream.base_url.clone());
    let response = client
        .post(format!("{base_url}/api/v1/runtime-revisions"))
        .bearer_auth(&admin_token)
        .json(&command)
        .send()
        .await
        .expect("publication request");
    let status = response.status();
    let body = response.text().await.expect("publication body");
    assert_eq!(status, StatusCode::OK, "旧素材必须照常可发布：{body}");

    // ── 目录逐位一致：name 是厂商原生名，形状就是新的四字段形状 ──
    let (status, catalog) = get_catalog(&client, &base_url, None).await;
    assert_eq!(status, StatusCode::OK, "{catalog}");
    assert_eq!(
        catalog,
        json!({"data": [{
            "name": "gpt-image-2",
            "vendor_id": "OpenAI",
            "revision": "2026-09-18-validated-1.3",
            "contract": material["offerings"][0]["capability_schema"],
        }]}),
        "缺省回退之后目录与今天逐位一致：{catalog}"
    );

    // ── 受理行为同样照旧：按回退出来的名字跑通 ──
    let _worker = spawn_worker_process(&database_url);
    let key = format!("legacy-naming-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &route_request("gpt-image-2", "旧素材：缺省回退"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "旧素材必须照常受理：{body}");
    assert_sync_success("旧素材受理", &body);

    // ── 落库事实：修订上的对客名就是回退出来的厂商原生名，开关行也在（默认启用）──
    let revision_gateway: String = sqlx::query_scalar(
        "SELECT rr.gateway_model FROM publication.runtime_revisions rr
         JOIN publication.runtime_entries re ON re.runtime_revision_id = rr.id
         WHERE re.active AND re.gateway_model = 'gpt-image-2' LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .expect("runtime revision naming column");
    assert_eq!(revision_gateway, "gpt-image-2");
    let enabled: bool = sqlx::query_scalar(
        "SELECT enabled FROM publication.gateway_models WHERE gateway_model = $1",
    )
    .bind("gpt-image-2")
    .fetch_one(&pool)
    .await
    .expect("gateway model switch");
    assert!(enabled, "首次发布成功时落一行开关，默认开着");

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// 增量迁移：命名两列与运维开关表要在**已经建过库、已经有发布数据**的环境里落下来。
///
/// 先在只应用了早期迁移的库上造出"已经发布过一个型号"的数据（修订 + 一条生效条目），
/// 再补上整批迁移，确认：
/// - 修订上回填出对客名与它挂的那行合同，两列非空；
/// - 运维开关按既有生效名字回填出一行（`enabled = true`）；
/// - 迁移后**立刻可读、可停用**：管理端列得出来，也关得掉——不用重新发布一次。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn gateway_model_naming_migration_backfills_existing_publications() {
    let (database_url, database_name) = isolated_database_url().await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");
    let migrations = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../migrations");

    // 1) 只应用这次改动之前的迁移（`0007` 之前，含上一轮的合同/承载面拆分）。
    let staged = std::env::temp_dir().join(format!("seeai-early-migrations-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&staged).expect("staging directory");
    for entry in std::fs::read_dir(&migrations).expect("migrations directory") {
        let entry = entry.expect("migration entry");
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".sql") && name.as_str() < "0007" {
            std::fs::copy(entry.path(), staged.join(&name)).expect("copy early migration");
        }
    }
    sqlx::migrate::Migrator::new(staged.clone())
        .await
        .expect("early migrator")
        .run(&pool)
        .await
        .expect("early migrations apply");

    // 2) 老数据：一个已经发布过的型号，按**这次改动之前**的形状落库
    //    （合同 + 渠道 + 供给 + 计价 + 修订 + 一条生效条目）。
    let vendor_model = Uuid::new_v4();
    let channel = Uuid::new_v4();
    let offering = Uuid::new_v4();
    let price_plan = Uuid::new_v4();
    let revision = Uuid::new_v4();
    let contract = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["model", "prompt"],
        "properties": {
            "model": {"const": "legacy-name"},
            "prompt": {"type": "string"}
        }
    });
    sqlx::query(
        "INSERT INTO catalog.vendor_models
             (id, vendor_id, native_model_id, native_revision, capability_schema)
         VALUES ($1,'OpenAI','legacy-name','legacy-revision',$2)",
    )
    .bind(vendor_model)
    .bind(&contract)
    .execute(&pool)
    .await
    .expect("legacy contract row");
    sqlx::query(
        "INSERT INTO supply.channels (id, provider_kind, base_url, credential_env)
         VALUES ($1,'AIHubMix','https://api.inferera.com','AIHUBMIX_API_KEY')",
    )
    .bind(channel)
    .execute(&pool)
    .await
    .expect("channel fixture");
    sqlx::query(
        "INSERT INTO supply.offerings
             (id, vendor_model_id, channel_id, adapter_key, provider_model_id, restrictions,
              carrier_schema, parameter_mapping)
         VALUES ($1,$2,$3,'aihubmix-image-v1','legacy-name','{}'::jsonb,$4,'{}'::jsonb)",
    )
    .bind(offering)
    .bind(vendor_model)
    .bind(channel)
    .bind(&contract)
    .execute(&pool)
    .await
    .expect("offering fixture");
    sqlx::query(
        "INSERT INTO pricing.price_plans
             (id, offering_id, currency, text_input_microusd_per_million, image_input_microusd_per_million,
              text_output_microusd_per_million, image_output_microusd_per_million, source_url, approved_by)
         VALUES ($1,$2,'USD',0,0,0,0,'https://example.invalid/price','migration-test')",
    )
    .bind(price_plan)
    .bind(offering)
    .execute(&pool)
    .await
    .expect("price plan fixture");
    sqlx::query(
        "INSERT INTO publication.runtime_revisions (id, snapshot, published_by)
         VALUES ($1,'{}'::jsonb,'migration-test')",
    )
    .bind(revision)
    .execute(&pool)
    .await
    .expect("runtime revision fixture");
    sqlx::query(
        "INSERT INTO publication.runtime_entries
             (runtime_revision_id, vendor_model_id, offering_id, price_plan_id, gateway_model, active)
         VALUES ($1,$2,$3,$4,'legacy-name',true)",
    )
    .bind(revision)
    .bind(vendor_model)
    .bind(offering)
    .bind(price_plan)
    .execute(&pool)
    .await
    .expect("runtime entry fixture");

    // 3) 补上整批迁移：命名两列与开关表必须自己跑通，不能因为已有数据就失败。
    sqlx::migrate::Migrator::new(migrations)
        .await
        .expect("migrator")
        .run(&pool)
        .await
        .expect("the naming migration must apply on an already-built database");

    // 4) 回填结果：修订上的对客名与它挂的那行合同，两列都非空。
    let row = sqlx::query(
        "SELECT gateway_model, vendor_model_id FROM publication.runtime_revisions WHERE id = $1",
    )
    .bind(revision)
    .fetch_one(&pool)
    .await
    .expect("runtime revision after migration");
    let gateway_model: String = row.try_get("gateway_model").expect("gateway model");
    let vendor_model_id: Uuid = row.try_get("vendor_model_id").expect("vendor model id");
    assert_eq!(gateway_model, "legacy-name");
    assert_eq!(vendor_model_id, vendor_model, "修订要指向它挂的那行合同");
    for column in ["gateway_model", "vendor_model_id"] {
        let nullable: String = sqlx::query_scalar(
            "SELECT is_nullable FROM information_schema.columns
             WHERE table_schema = 'publication' AND table_name = 'runtime_revisions'
               AND column_name = $1",
        )
        .bind(column)
        .fetch_one(&pool)
        .await
        .expect("column probe");
        assert_eq!(nullable, "NO", "{column} 在既有行上必须非空");
    }
    let (switch, enabled): (String, bool) = {
        let row = sqlx::query(
            "SELECT gateway_model, enabled FROM publication.gateway_models
             WHERE gateway_model = 'legacy-name'",
        )
        .fetch_one(&pool)
        .await
        .expect("backfilled switch row");
        (
            row.try_get("gateway_model").expect("gateway model"),
            row.try_get("enabled").expect("enabled"),
        )
    };
    assert_eq!(switch, "legacy-name");
    assert!(enabled, "既有生效名字回填成启用");

    // 5) 迁移后立刻可读、可停用：走管理端接口（不起 Worker，也不连上游）。
    let (base_url, admin_token, _process) = start_api(&database_url, 1, 64).await;
    let client = Client::new();
    wait_until_ready(&client, &base_url).await;
    let (status, admin) = get_gateway_models(&client, &base_url, Some(&admin_token)).await;
    assert_eq!(status, StatusCode::OK, "{admin}");
    let view = &admin["gateway_models"][0];
    assert_eq!(view["gateway_model"], "legacy-name");
    assert_eq!(view["enabled"], true);
    assert_eq!(view["vendor_id"], "OpenAI");
    assert_eq!(view["native_model_id"], "legacy-name");
    assert_eq!(view["candidates"][0]["provider_kind"], "AIHubMix");
    assert_eq!(view["candidates"][0]["routing_priority"], 0);
    assert_eq!(
        patch_gateway_model(&client, &base_url, &admin_token, "legacy-name", false).await,
        StatusCode::NO_CONTENT,
        "迁移回填出来的名字必须停得掉，不用重新发布一次"
    );
    let (_, admin) = get_gateway_models(&client, &base_url, Some(&admin_token)).await;
    assert_eq!(admin["gateway_models"][0]["enabled"], false);

    pool.close().await;
    let _ = std::fs::remove_dir_all(&staged);
    drop_isolated_database(&database_name).await;
}

// ───────────────────────── 定价、保底与结算 ─────────────────────────

/// 起一个夹具并把它那条候选**重新发布成带定价的**。
///
/// 上游地址取夹具里那个假上游：重新发布不能把地址写回素材里那个占位地址（那样 Worker 会去连
/// 一个不存在的上游）。重新发布本身就是"发布即原子替换"——它顺带证明**已受理的 Job 不受
/// 后来的修订影响**（旧 Job 固定的是受理时那一版）。
async fn republish_priced(
    harness: &Harness,
    client: &Client,
    floor_amounts: Value,
    consumer_rates: Value,
    markup_bps: i32,
) -> StatusCode {
    let mut draft = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    draft["base_url"] = Value::String(harness.upstream_base_url.clone());
    draft["reference_cost_microusd"] = json!(11_354);
    draft["cost_basis"] = json!("computed");
    draft["consumer_rates_cny"] = consumer_rates;
    draft["tier_prices"] = json!({"1K": 160_000, "2K": 250_000, "4K": 300_000});
    draft["floor_amounts"] = floor_amounts;
    publish_candidates_with_markup(
        client,
        &harness.base_url,
        &harness.admin_token,
        Harness::MODEL,
        None,
        vec![draft],
        Some(markup_bps),
    )
    .await
}

/// 发一次请求，回读这次受理冻结下来的 `(保底额, 保底额来源)`。
async fn hold_for(harness: &Harness, api_key: &str, parameters: Value) -> (u64, String) {
    let key = format!("hold-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "hold contract");
    for (name, value) in parameters
        .as_object()
        .expect("parameters must be an object")
    {
        request[name] = value.clone();
    }
    let (status, body) = post_json(
        &harness.base_url,
        api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let snapshot = frozen_snapshot(&harness.pool, &key).await;
    (
        snapshot["hold_microusd"].as_u64().expect("a frozen hold"),
        snapshot["hold_source"]
            .as_str()
            .expect("a frozen hold source")
            .to_owned(),
    )
}

/// 该 Job 的账户当前余额（账本是权威）。
async fn account_balance(harness: &Harness, job_id: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT a.balance_microusd FROM ledger.accounts a
         JOIN generation.jobs j ON j.account_id = a.id WHERE j.id = $1",
    )
    .bind(job_id)
    .fetch_one(&harness.pool)
    .await
    .expect("balance")
}

/// **定价随 Job 冻结，结算只读那份快照**。
///
/// 一次请求同时钉住四件事：售价按**命中候选**发布的那份对客 CNY 费率向量算（不是渠道成本
/// 费率）、保底额按请求的 `(size, quality)` 查该供给的保底表（**不由售价派生**）、汇率按该候选
/// 的成本币种取受理时刻生效的那一行并随快照冻结、成本记原币种原值并用冻结的汇率折出人民币。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn pricing_is_frozen_into_the_job_and_settlement_only_reads_that_snapshot() {
    let harness = Harness::start_with_draft(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;
    let client = Client::new();
    assert_eq!(
        republish_priced(
            &harness,
            &client,
            openai_floor_amounts(),
            priced_consumer_rates(),
            2_000
        )
        .await,
        StatusCode::OK,
        "带定价的发布必须成功"
    );

    let (_, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let _worker = harness.spawn_worker();
    let key = format!("pricing-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "pricing contract");
    request["size"] = json!("2K");
    request["quality"] = json!("low");
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_sync_success("定价与结算", &body);
    // 对客面**只有人民币**：响应里不出现成本侧那个币种。
    assert!(
        !body.to_string().contains("USD"),
        "对客响应不该出现外币：{body}"
    );

    let snapshot = frozen_snapshot(&harness.pool, &key).await;
    assert_eq!(
        snapshot["consumer_rates_cny"],
        priced_consumer_rates(),
        "对客费率向量必须与后台设定逐位一致"
    );
    assert_eq!(snapshot["hold_microusd"], json!(250_000), "2K 档的保底额");
    assert_eq!(snapshot["hold_source"], json!("tier"));
    assert_eq!(snapshot["cost_currency"], json!("USD"));
    assert_eq!(snapshot["reference_cost_microusd"], json!(11_354));
    assert_eq!(snapshot["cost_basis"], json!("computed"));
    assert_eq!(snapshot["markup_bps"], json!(2_000));
    assert_eq!(snapshot["fx_rate"]["currency"], json!("USD"));
    assert_eq!(snapshot["fx_rate"]["rate_micros"], json!(7_100_000));

    let (job_id, state, _) = harness.job(&key).await;
    assert_eq!(state, "succeeded");
    let offering_id: Uuid =
        sqlx::query_scalar("SELECT offering_id FROM generation.jobs WHERE id = $1")
            .bind(job_id)
            .fetch_one(&harness.pool)
            .await
            .expect("offering");
    assert_eq!(
        snapshot["hit_candidate"]["offering_id"],
        json!(offering_id.to_string()),
        "快照记的命中候选就是这次真正选中的那一条"
    );
    // 预授权额 = 保底额（不由售价派生）：Job 上的数与账本里的 hold 都是它。
    let authorized: i64 =
        sqlx::query_scalar("SELECT max_cost_microusd FROM generation.jobs WHERE id = $1")
            .bind(job_id)
            .fetch_one(&harness.pool)
            .await
            .expect("authorization");
    assert_eq!(authorized, 250_000);
    let held: i64 =
        sqlx::query_scalar("SELECT amount_microusd FROM ledger.holds WHERE job_id = $1")
            .bind(job_id)
            .fetch_one(&harness.pool)
            .await
            .expect("hold");
    assert_eq!(held, 250_000);
    // 实收 = 对客费率向量 × 实际用量：14 文本输入 × 40 + 196 图像输出 × 220（每 1M）。
    assert_eq!(harness.captured_microusd(job_id).await, -43_680);
    // 成本 = 原币种原值 + 折算后 CNY：5950 微美元 × 7.1 = 42245 微元。
    let (amount, currency, source, cny) = harness.attempt_cost(job_id).await;
    assert_eq!(amount, Some(5_950));
    assert_eq!(currency.as_deref(), Some("USD"));
    assert_eq!(source.as_deref(), Some("computed"));
    assert_eq!(cny, Some(42_245));
    // 毛利 = 售价（CNY）− 成本折算后 CNY，两条线分开留痕、可逐笔算出。
    assert_eq!(43_680 - 42_245, 1_435);
    // 余额 = 初始 − 实收（受理时先按保底额冻，结算按实际结清）。
    let balance_after_first = account_balance(&harness, job_id).await;
    assert_eq!(balance_after_first, 1_000_000 - 43_680);

    // ── 重发修订（换对客费率向量与加价系数）**不影响已受理的 Job** ──
    let mut higher = priced_consumer_rates();
    higher["image_output_micros_per_million"] = json!(440_000_000);
    assert_eq!(
        republish_priced(
            &harness,
            &client,
            openai_floor_amounts(),
            higher.clone(),
            3_000
        )
        .await,
        StatusCode::OK
    );
    let next_key = format!("pricing-next-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &next_key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let next_snapshot = frozen_snapshot(&harness.pool, &next_key).await;
    assert_eq!(
        next_snapshot["consumer_rates_cny"], higher,
        "新受理的 Job 用新价"
    );
    assert_eq!(next_snapshot["markup_bps"], json!(3_000));
    assert_eq!(
        frozen_snapshot(&harness.pool, &key).await,
        snapshot,
        "已受理 Job 的快照逐位不动"
    );
    assert_eq!(harness.captured_microusd(job_id).await, -43_680);
    // 第二笔按新价结算：14 文本输入 × 40 + 196 图像输出 × 440（每 1M） = 86800 微元。
    assert_eq!(
        account_balance(&harness, job_id).await,
        balance_after_first - 86_800,
        "旧 Job 的金额不动，新 Job 按新价扣"
    );

    harness.cleanup().await;
}

/// **售价按命中的那条候选算**：同一个网关模型的两个候选各带一份对客费率向量，实收按**命中**的
/// 那一份算，而且只随它变。
///
/// 两份向量故意差得很远（便宜那份算出来是 210 微元、正常那份是 43680 微元），拿错一份立刻露出来；
/// 用**承载面差异**把请求逼到优先级 1 的那条（优先级 0 的候选承载不了 `quality`）。随后只改
/// `reference_cost_microusd` 重发：参考成本只是定价参考，对客实收逐位不变。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_charge_follows_the_hit_candidate_and_ignores_the_reference_cost() {
    let harness = Harness::start_with_draft(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;
    let client = Client::new();
    let model = Harness::MODEL;
    // 合同声明 `quality`（调用方能提交它），而**优先级 0** 的候选承载面里没有它——请求带上
    // `quality` 就一定落到优先级 1 的那条（选路规则：按优先级取第一个合格者）。
    let contract = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "quality": {"type": "string", "enum": ["low", "high"]}
    }));
    let narrow = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1}
    }));
    let wide = surface_schema(json!({
        "model": {"const": model},
        "prompt": {"type": "string", "minLength": 1},
        "quality": {"type": "string", "enum": ["low", "high"]}
    }));
    // 便宜得离谱的那份：这次的用量按它算只有 14 × 1 + 196 × 1 = 210 微元。
    let cheap = json!({
        "text_input_micros_per_million": 1_000_000,
        "image_input_micros_per_million": 1_000_000,
        "text_output_micros_per_million": 1_000_000,
        "image_output_micros_per_million": 1_000_000
    });

    let mut first = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    // 承载面走新名字：这一份发布带**模型级合同**，候选自带的旧 `capability_schema` 不该再充当
    // 合同（两份不同的旧字段会让归一期拒掉整份发布）。
    first["carrier_schema"] = narrow;
    first
        .as_object_mut()
        .expect("a draft object")
        .remove("capability_schema");
    first["base_url"] = Value::String(harness.upstream_base_url.clone());
    first["reference_cost_microusd"] = json!(11_354);
    first["cost_basis"] = json!("computed");
    first["consumer_rates_cny"] = cheap;
    first["tier_prices"] = json!({});
    first["floor_amounts"] = openai_floor_amounts();

    let mut second = first.clone();
    second["carrier_schema"] = wide;
    second["reference_cost_microusd"] = json!(999_999);
    second["consumer_rates_cny"] = priced_consumer_rates();

    // 加价系数**不给**：对客费率向量是直接录入的，那一步用不上它（发布期不再强制）。
    assert_eq!(
        publish_on_revision(
            &harness,
            model,
            "two-candidates-1",
            contract.clone(),
            vec![first.clone(), second.clone()],
            None,
        )
        .await,
        StatusCode::OK,
        "直接录入对客费率向量、不填加价系数也必须发得出去"
    );

    let (_, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let _worker = harness.spawn_worker();
    let mut request = route_request(harness.model, "hit candidate");
    request["quality"] = json!("low");
    let key = format!("hit-candidate-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let (job_id, state, _) = harness.job(&key).await;
    assert_eq!(state, "succeeded");

    let snapshot = frozen_snapshot(&harness.pool, &key).await;
    assert_eq!(
        snapshot["consumer_rates_cny"],
        priced_consumer_rates(),
        "快照冻的是**命中候选**那一份向量"
    );
    let hit: Uuid = snapshot["hit_candidate"]["offering_id"]
        .as_str()
        .expect("the frozen snapshot names the hit candidate")
        .parse()
        .expect("an offering id");
    let chosen: Uuid = sqlx::query_scalar(
        "SELECT re.offering_id FROM publication.runtime_entries re
         WHERE re.active AND re.gateway_model = $1 AND re.routing_priority = 1",
    )
    .bind(model)
    .fetch_one(&harness.pool)
    .await
    .expect("the priority 1 offering");
    assert_eq!(
        hit, chosen,
        "承载不了 `quality` 的那条落选，这次请求落到下一条"
    );
    // 实收按**命中候选**的向量算：14 文本输入 × 40 + 196 图像输出 × 220（每 1M）。
    assert_eq!(
        harness.captured_microusd(job_id).await,
        -43_680,
        "拿优先级 0 那份便宜向量算就是 -210，两者差得很远"
    );

    // ── 只改参考成本重发：对客实收只随对客费率向量变 ──
    let mut repriced = second.clone();
    repriced["reference_cost_microusd"] = json!(7_777_777);
    assert_eq!(
        publish_on_revision(
            &harness,
            model,
            "two-candidates-2",
            contract,
            vec![first, repriced],
            None,
        )
        .await,
        StatusCode::OK
    );
    let next_key = format!("hit-candidate-next-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &next_key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let (next_job, state, _) = harness.job(&next_key).await;
    assert_eq!(state, "succeeded");
    let next_snapshot = frozen_snapshot(&harness.pool, &next_key).await;
    assert_eq!(
        next_snapshot["reference_cost_microusd"],
        json!(7_777_777),
        "重发确实换掉了参考成本（否则下面那条断言就是空的）"
    );
    assert_eq!(
        harness.captured_microusd(next_job).await,
        -43_680,
        "参考成本只是定价参考：对客实收只随 `consumer_rates_cny` 变"
    );
    assert_eq!(
        harness.captured_microusd(job_id).await,
        -43_680,
        "已受理 Job 的金额不动"
    );

    harness.cleanup().await;
}

/// **保底按供给维度查表**：先把这次请求的 `size` 归到档位，再查表；查不到走该供给封顶保底值。
///
/// 归位规则（设计 §6）：像素型 `size` 先按该供给发布的档位像素表反向查、缺失时按**最长边**
/// 阈值兜底；`auto`（与没给 `size` 同义）取**默认档 2K**；比例型归不出档位。
/// `quality` 维留空即按 `size` 档：表里没为某个质量单列时，带任意质量都查到同一个档位。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_hold_resolves_the_tier_then_walks_the_supply_floor_chain() {
    let harness = Harness::start_with_draft(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;
    let client = Client::new();
    assert_eq!(
        republish_priced(
            &harness,
            &client,
            openai_floor_amounts(),
            priced_consumer_rates(),
            2_000
        )
        .await,
        StatusCode::OK
    );
    let (_, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let _worker = harness.spawn_worker();

    // 档位查表：2K = ¥0.25。
    assert_eq!(
        hold_for(&harness, &api_key, json!({"size": "2K"})).await,
        (250_000, "tier".to_owned())
    );
    // 档位写法的大小写不影响查表：调用方的 `2k` 与管理员的 `2K` 是同一个档。
    assert_eq!(
        hold_for(&harness, &api_key, json!({"size": "2k"})).await,
        (250_000, "tier".to_owned())
    );
    // `quality` 维留空即按 `size` 档：带任意质量都查到同一个档位。
    for quality in ["low", "high", "xhigh", "auto"] {
        assert_eq!(
            hold_for(
                &harness,
                &api_key,
                json!({"size": "2K", "quality": quality})
            )
            .await,
            (250_000, "tier".to_owned()),
            "quality={quality}"
        );
    }
    // **像素型 `size` 先归到档位**（用户口径：按分辨率保底、通过 `size` 判断 1K/2K/4K）：
    // 这条供给没发布尺寸档案，所以按**最长边**阈值兜底。
    for (size, amount) in [
        ("1024x1024", 160_000), // 最长边 1024 ⇒ 1K = ¥0.16
        ("2048x2048", 250_000), // 最长边 2048 ⇒ 2K = ¥0.25
        ("3840x2160", 300_000), // 最长边 3840 ⇒ 4K = ¥0.3
    ] {
        assert_eq!(
            hold_for(&harness, &api_key, json!({"size": size})).await,
            (amount, "tier".to_owned()),
            "size={size} 必须按它归出来的档位查保底"
        );
    }
    // `size = auto`（与没给 `size` 同义）⇒ 默认档 2K（中间档）。
    assert_eq!(
        hold_for(&harness, &api_key, json!({"size": "auto"})).await,
        (250_000, "auto_tier".to_owned())
    );
    assert_eq!(
        hold_for(&harness, &api_key, json!({})).await,
        (250_000, "auto_tier".to_owned())
    );
    // **空串不是"没给"**：`size` 的字面量就是调用方说的那个尺寸，归不出档位就回落封顶保底值
    // ——只有字段缺失或字面 `auto` 才走默认档 2K。
    assert_eq!(
        hold_for(&harness, &api_key, json!({"size": ""})).await,
        (300_000, "supply_cap".to_owned()),
        "空串与'没给这个字段'必须落到不同的保底额上"
    );
    // 比例型只说了形状、没说分辨率 ⇒ 归不出档位 ⇒ 回落该供给的封顶保底值。
    assert_eq!(
        hold_for(&harness, &api_key, json!({"size": "16:9"})).await,
        (300_000, "supply_cap".to_owned())
    );

    harness.cleanup().await;
}

/// **没有定价的旧修订与空保底表都回落到平台兜底数**（`GENERATION_MAX_COST_MICROUSD`）。
///
/// 前者的预授权与结算都走旧口径、与今天逐位相同；后者有定价（售价按对客费率向量），只是连该
/// 供给的封顶保底值都没有——来源记的是平台兜底，事后分得清。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn an_unpriced_revision_and_an_empty_floor_table_fall_back_to_the_platform_default() {
    let harness = Harness::start_with_draft(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;
    let client = Client::new();
    let (_, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let _worker = harness.spawn_worker();

    // 1) 没有定价的修订：快照里没有对客费率向量，预授权回落平台兜底数，结算按已发布费率。
    let key = format!("unpriced-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &route_request(harness.model, "unpriced revision"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let (job_id, state, _) = harness.job(&key).await;
    assert_eq!(state, "succeeded");
    let snapshot = frozen_snapshot(&harness.pool, &key).await;
    assert!(
        snapshot["consumer_rates_cny"].is_null(),
        "旧口径的快照不带对客费率向量：{snapshot}"
    );
    assert!(snapshot["hold_microusd"].is_null());
    let authorized: i64 =
        sqlx::query_scalar("SELECT max_cost_microusd FROM generation.jobs WHERE id = $1")
            .bind(job_id)
            .fetch_one(&harness.pool)
            .await
            .expect("authorization");
    assert_eq!(
        authorized, 20_000,
        "没有定价时预授权回落平台兜底数（今天的行为）"
    );
    assert_eq!(
        harness.captured_microusd(job_id).await,
        -5_950,
        "结算也走旧口径：已发布费率 × 实际用量"
    );

    // 2) 有定价、但保底表里什么都没有：连封顶保底值也没有 ⇒ 平台兜底。
    assert_eq!(
        republish_priced(&harness, &client, json!({}), priced_consumer_rates(), 2_000).await,
        StatusCode::OK
    );
    assert_eq!(
        hold_for(&harness, &api_key, json!({"size": "2K"})).await,
        (20_000, "platform_default".to_owned())
    );

    harness.cleanup().await;
}

/// **透支**：实收超过保底额时余额被扣成负数，随后同一账户再发请求按当时余额判 402。
///
/// 受理闸门是"余额 ≥ 保底额"——保底额估小了由**结算**吸收，估大了结算释放差额；透支不是错误，
/// 也不进对账（对账态是"受理/执行状态不明"，会把消费者的钱扣在对账里）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn an_overdraft_settles_into_a_negative_balance_and_the_next_request_is_refused() {
    let harness = Harness::start_with_draft(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;
    let client = Client::new();
    // 保底额 ¥0.001（1000 微元），而这次生成实际要 ¥0.043680：估小了。
    assert_eq!(
        republish_priced(
            &harness,
            &client,
            json!({"amounts": {"1K": 1_000}, "cap_microusd": 1_000}),
            priced_consumer_rates(),
            2_000
        )
        .await,
        StatusCode::OK
    );
    let (_, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000).await;
    let _worker = harness.spawn_worker();

    let key = format!("overdraft-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "overdraft");
    request["size"] = json!("1K");
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "受理闸门是'余额 ≥ 保底额'：1000 ≥ 1000，照常受理。got {body}"
    );
    let (job_id, state, _) = harness.job(&key).await;
    assert_eq!(state, "succeeded");
    assert_eq!(harness.captured_microusd(job_id).await, -43_680);
    let balance = account_balance(&harness, job_id).await;
    assert_eq!(balance, 1_000 - 43_680, "结算按实际扣：差额把余额扣成负数");
    assert!(balance < 0);
    let cases: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM operations.reconciliation_cases WHERE job_id = $1",
    )
    .bind(job_id)
    .fetch_one(&harness.pool)
    .await
    .expect("cases");
    assert_eq!(cases, 0, "透支不是'状态不明'，不该把消费者的钱扣在对账里");

    // 随后同一个账户再发一次：按当时（负）余额判 ⇒ 402，不产生 Job、不扣款。
    let refused_key = format!("overdraft-refused-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &refused_key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::PAYMENT_REQUIRED, "got {body}");
    assert_eq!(body["error"]["code"], json!("insufficient_balance"));
    let created: i64 =
        sqlx::query_scalar("SELECT count(*) FROM generation.jobs WHERE idempotency_key = $1")
            .bind(&refused_key)
            .fetch_one(&harness.pool)
            .await
            .expect("refused jobs");
    assert_eq!(created, 0, "被拒的受理不产生 Job");
    assert_eq!(
        account_balance(&harness, job_id).await,
        balance,
        "被拒的受理不扣款"
    );

    harness.cleanup().await;
}

/// **汇率按受理时刻生效的那一行取值**，且**没有折算率的币种在发布期被拒**。
///
/// 未来生效的一行是调价预告：受理时该用的仍是受理时刻之前已生效的那一行。受理之后再录一行
/// 也不动已受理 Job 的折算——快照已经把它冻住了。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_rate_effective_at_acceptance_is_frozen_and_a_missing_rate_blocks_publication() {
    let harness = Harness::start_with_draft(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;
    let client = Client::new();

    // 调价预告：未来生效的一行不参与受理时的取值。
    let future = chrono::Utc::now() + chrono::Duration::days(1);
    let response = client
        .put(format!("{}/api/v1/fx-rates", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .json(&json!({
            "currency": "USD",
            "rate_micros": 9_000_000u64,
            "effective_at": future.to_rfc3339(),
        }))
        .send()
        .await
        .expect("future fx rate");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // 没有折算率的币种：发布期拒绝，整份发布不落任何行。
    let mut draft = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    draft["base_url"] = Value::String(harness.upstream_base_url.clone());
    draft["price_plan"]["currency"] = json!("EUR");
    assert_eq!(
        publish_candidates(
            &client,
            &harness.base_url,
            &harness.admin_token,
            "eur-model",
            None,
            vec![draft]
        )
        .await,
        StatusCode::BAD_REQUEST,
        "该币种没有折算率就必须在发布期被拒"
    );
    let revisions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM publication.runtime_revisions WHERE gateway_model = 'eur-model'",
    )
    .fetch_one(&harness.pool)
    .await
    .expect("revisions");
    assert_eq!(revisions, 0, "被拒的发布不落任何行");

    assert_eq!(
        republish_priced(
            &harness,
            &client,
            openai_floor_amounts(),
            priced_consumer_rates(),
            2_000
        )
        .await,
        StatusCode::OK
    );
    let (_, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let _worker = harness.spawn_worker();
    let key = format!("fx-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &route_request(harness.model, "fx rate"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let snapshot = frozen_snapshot(&harness.pool, &key).await;
    assert_eq!(
        snapshot["fx_rate"]["rate_micros"],
        json!(7_100_000),
        "取的是受理时刻生效的那一行，不是未来那一行"
    );
    let (job_id, _, _) = harness.job(&key).await;
    assert_eq!(harness.attempt_cost(job_id).await.3, Some(42_245));

    // 受理之后再录一行（立即生效）：已受理 Job 的折算用的是冻结的那个数。
    let response = client
        .put(format!("{}/api/v1/fx-rates", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .json(&json!({"currency": "USD", "rate_micros": 5_000_000u64}))
        .send()
        .await
        .expect("later fx rate");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        frozen_snapshot(&harness.pool, &key).await["fx_rate"]["rate_micros"],
        json!(7_100_000),
        "快照已经冻住了受理当时那一行"
    );
    assert_eq!(
        harness.attempt_cost(job_id).await.3,
        Some(42_245),
        "已受理 Job 的成本折算不变"
    );

    harness.cleanup().await;
}

/// **未指定 `effective_at` 时，落库的生效时刻由数据库决定**，不由 API 进程的时钟盖章。
///
/// 判据是"同一事务里两个库侧时刻必须逐位相同"：折算率那一行的 `effective_at` 由库的 `now()`
/// 盖章，同一事务里那条审计事件的 `created_at` 也是库的 `now()`。若改回由进程时钟盖章，两者
/// 会差出宿主与容器的时钟漂移——那正是"录完折算率立刻发布"被判成"该币种还没有生效的折算率"
/// 的成因（发布期校验与受理取值比的都是库的 `now()`）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn an_fx_rate_without_an_effective_time_is_stamped_by_the_database_clock() {
    let harness = Harness::start(UpstreamBehaviour::aihubmix(SyncImageShape::Url)).await;
    let client = Client::new();

    let response = client
        .put(format!("{}/api/v1/fx-rates", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .json(&json!({"currency": "GBP", "rate_micros": 8_800_000u64}))
        .send()
        .await
        .expect("fx rate without effective_at");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let same_clock: bool = sqlx::query_scalar(
        r#"
        SELECT f.effective_at = a.created_at
        FROM pricing.fx_rates f
        JOIN operations.audit_events a
          ON a.action = 'fx_rate.upsert' AND a.subject_id = f.currency
        WHERE f.currency = 'GBP'
        "#,
    )
    .fetch_one(&harness.pool)
    .await
    .expect("stamped fx rate row and its audit event");
    assert!(
        same_clock,
        "未指定生效时刻的折算率必须由库盖章：它的生效时刻要与同一事务里那条审计事件的库侧时间戳相同"
    );

    // 同一事实的另一面：库里不该出现一行"还没生效"的折算率——发布期校验看到的就是这些行。
    let pending: i64 =
        sqlx::query_scalar("SELECT count(*) FROM pricing.fx_rates WHERE effective_at > now()")
            .fetch_one(&harness.pool)
            .await
            .expect("pending fx rates");
    assert_eq!(pending, 0, "库盖章的行落库即生效，不会落在库的 now() 之后");

    harness.cleanup().await;
}

/// **上游声明的金额直接取，并用冻结的汇率折出人民币**（`declared` 那一态）。
///
/// 上游声明的是 11354 微美元，而按该渠道成本费率自算是 5950——两个数不同，正好钉住"声明就
/// 直接取、不自己算"。对客金额只由受理时冻结的对客费率向量决定，实际成本只进毛利口径。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_declared_cost_is_taken_as_is_and_converted_with_the_frozen_rate() {
    let harness = Harness::start(UpstreamBehaviour::apimart()).await;
    let client = Client::new();
    // 承载面与首次发布那一份**逐字一致**：合同是模型级唯一一份且不可变，换了面会被发布期拒。
    let mut draft = candidate(
        "APIMart",
        "apimart-image-v1",
        &["prompt_only", "image_conditioned", "masked"],
    );
    draft["base_url"] = Value::String(harness.upstream_base_url.clone());
    draft["reference_cost_microusd"] = json!(11_354);
    draft["cost_basis"] = json!("declared");
    draft["consumer_rates_cny"] = priced_consumer_rates();
    draft["tier_prices"] = json!({});
    draft["floor_amounts"] = openai_floor_amounts();
    assert_eq!(
        publish_candidates_with_markup(
            &client,
            &harness.base_url,
            &harness.admin_token,
            Harness::MODEL,
            None,
            vec![draft],
            Some(2_000),
        )
        .await,
        StatusCode::OK
    );

    let (_, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let _worker = harness.spawn_worker();
    let key = format!("declared-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &route_request(harness.model, "declared cost"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let (job_id, state, _) = harness.job(&key).await;
    assert_eq!(state, "succeeded");
    let (amount, currency, source, cny) = harness.attempt_cost(job_id).await;
    assert_eq!(amount, Some(11_354), "上游给了金额就直接取它，不自己算");
    assert_eq!(currency.as_deref(), Some("USD"));
    assert_eq!(source.as_deref(), Some("declared"));
    assert_eq!(cny, Some(80_614), "11354 微美元 × 7.1 = 80613.4 ⇒ 向上取整");
    assert_eq!(
        harness.captured_microusd(job_id).await,
        -43_680,
        "实际成本不改对客金额：对客金额只由冻结的对客费率向量决定"
    );
    // 毛利 = 售价（CNY）− 成本折算后 CNY：这一笔是负的（参考成本只是发布时的定价参考，
    // 上游实际声明的金额比它高），照样能逐笔算出——不猜、不掩盖。
    assert_eq!(43_680 - 80_614, -36_934);

    harness.cleanup().await;
}

/// 管理员读：`GET /api/v1/gateway-models` 列出每个候选的定价与修订级加价系数，不用直查库。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_admin_view_lists_the_published_pricing() {
    let harness = Harness::start_with_draft(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
    )
    .await;
    let client = Client::new();
    assert_eq!(
        republish_priced(
            &harness,
            &client,
            openai_floor_amounts(),
            priced_consumer_rates(),
            2_000
        )
        .await,
        StatusCode::OK
    );

    let (status, admin) =
        get_gateway_models(&client, &harness.base_url, Some(&harness.admin_token)).await;
    assert_eq!(status, StatusCode::OK, "{admin}");
    let view = &admin["gateway_models"][0];
    assert_eq!(view["markup_bps"], json!(2_000), "加价系数是修订级的");
    let candidate = &view["candidates"][0];
    assert_eq!(candidate["consumer_rates_cny"], priced_consumer_rates());
    assert_eq!(candidate["reference_cost_microusd"], json!(11_354));
    assert_eq!(candidate["cost_currency"], json!("USD"));
    assert_eq!(candidate["cost_basis"], json!("computed"));
    assert_eq!(candidate["tier_prices"]["2K"], json!(250_000));
    assert_eq!(candidate["floor_amounts"]["amounts"]["2K"], json!(250_000));
    assert_eq!(candidate["floor_amounts"]["cap_microusd"], json!(300_000));

    harness.cleanup().await;
}

/// **成本缺口**的处置：不进对账态、对客结算照常完成，运营从缺口清单里看得到它。
///
/// 补录金额归账实核对那条线（另一张工单）；补录完成后这一笔不再出现在清单里，所以清单就是
/// "当前还有哪些缺口"的答案。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_cost_gap_is_listed_for_operations_without_pushing_the_job_into_reconciliation() {
    let mut behaviour = UpstreamBehaviour::apimart();
    // 渠道声明了金额却拿不到（终态没有 `cost` 字段）⇒ 成本缺口。
    behaviour.declared_cost = None;
    let harness = Harness::start(behaviour).await;
    let key = format!("gap-list-{}", Uuid::new_v4());
    let (status, body) = harness
        .sync_json(
            "/v1/images/generations",
            &key,
            route_request(harness.model, "cost gap for operations"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let (job_id, state, _) = harness.job(&key).await;
    assert_eq!(state, "succeeded", "成本缺口不是执行失败");
    let (amount, currency, source, cny) = harness.attempt_cost(job_id).await;
    assert_eq!(source.as_deref(), Some("unavailable"));
    assert_eq!(
        (amount, currency, cny),
        (None, None, None),
        "缺口不猜：三样都留空"
    );

    // 不进对账态、也不开对账案例：消费者的钱该扣的照扣。
    let cases: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM operations.reconciliation_cases WHERE job_id = $1",
    )
    .bind(job_id)
    .fetch_one(&harness.pool)
    .await
    .expect("cases");
    assert_eq!(cases, 0);
    assert_eq!(harness.captured_microusd(job_id).await, -5_950);

    // 运营从缺口清单里看到它，带着去上游核账单要用的对账标识。
    let client = Client::new();
    let gaps: Value = client
        .get(format!("{}/api/v1/provider-cost-gaps", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .send()
        .await
        .expect("cost gap list")
        .json()
        .await
        .expect("cost gap list JSON");
    assert_eq!(gaps["count"], json!(1), "{gaps}");
    assert_eq!(gaps["truncated"], json!(false));
    assert_eq!(gaps["gaps"][0]["job_id"], json!(job_id.to_string()));
    assert_eq!(gaps["gaps"][0]["gateway_model"], json!(harness.model));
    assert!(
        gaps["gaps"][0]["provider_trace_id"].as_str().is_some(),
        "缺口清单必须带上游对账标识，否则核账单的人不知道该查哪个任务：{gaps}"
    );
    // 该接口只对管理员开放。
    let unauthorized = client
        .get(format!("{}/api/v1/provider-cost-gaps", harness.base_url))
        .send()
        .await
        .expect("unauthorized cost gap list");
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    harness.cleanup().await;
}

/// **进对账那条路径也落成本事实**：执行已经发生、上游成本也拿得到，成本必须有去处。
///
/// 直接调仓库端口的 `fail_job`：结果交付失败在端到端里很难构造（假上游总会给图），而这条路径
/// 的写入本来就是库层的事。同时验"没有成本事实时四列留空"——那是"这次没有成本事实可落"，
/// 与"成本是 0"不是一回事。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_reconciliation_path_records_the_cost_fact_it_already_has() {
    let (database_url, database_name) = isolated_database_url().await;
    let repository = PgHubRepository::connect(&database_url, 2)
        .await
        .expect("repository");
    repository.migrate().await.expect("migrations");
    let pool = repository.pool().clone();

    let account = Uuid::new_v4();
    let vendor_model = Uuid::new_v4();
    let channel = Uuid::new_v4();
    let offering = Uuid::new_v4();
    let price_plan = Uuid::new_v4();
    let revision = Uuid::new_v4();
    let contract = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["model", "prompt"],
        "properties": {
            "model": {"const": "cost-path"},
            "prompt": {"type": "string"}
        }
    });
    sqlx::query("INSERT INTO ledger.accounts (id, balance_microusd) VALUES ($1, 100000)")
        .bind(account)
        .execute(&pool)
        .await
        .expect("account fixture");
    sqlx::query(
        "INSERT INTO catalog.vendor_models
             (id, vendor_id, native_model_id, native_revision, capability_schema)
         VALUES ($1,'OpenAI','cost-path','rev-1',$2)",
    )
    .bind(vendor_model)
    .bind(&contract)
    .execute(&pool)
    .await
    .expect("contract fixture");
    sqlx::query(
        "INSERT INTO supply.channels (id, provider_kind, base_url, credential_env)
         VALUES ($1,'AIHubMix','https://api.inferera.com','AIHUBMIX_API_KEY')",
    )
    .bind(channel)
    .execute(&pool)
    .await
    .expect("channel fixture");
    sqlx::query(
        "INSERT INTO supply.offerings
             (id, vendor_model_id, channel_id, adapter_key, provider_model_id, restrictions,
              carrier_schema, parameter_mapping)
         VALUES ($1,$2,$3,'aihubmix-image-v1','cost-path','{}'::jsonb,$4,'{}'::jsonb)",
    )
    .bind(offering)
    .bind(vendor_model)
    .bind(channel)
    .bind(&contract)
    .execute(&pool)
    .await
    .expect("offering fixture");
    sqlx::query(
        "INSERT INTO pricing.price_plans
             (id, offering_id, currency, text_input_microusd_per_million, image_input_microusd_per_million,
              text_output_microusd_per_million, image_output_microusd_per_million, source_url, approved_by)
         VALUES ($1,$2,'USD',5,8,10,30,'https://example.invalid/price','cost-path-test')",
    )
    .bind(price_plan)
    .bind(offering)
    .execute(&pool)
    .await
    .expect("price plan fixture");
    sqlx::query(
        "INSERT INTO publication.runtime_revisions
             (id, snapshot, published_by, gateway_model, vendor_model_id)
         VALUES ($1,'{}'::jsonb,'cost-path-test','cost-path',$2)",
    )
    .bind(revision)
    .bind(vendor_model)
    .execute(&pool)
    .await
    .expect("runtime revision fixture");
    sqlx::query(
        "INSERT INTO publication.runtime_entries
             (runtime_revision_id, vendor_model_id, offering_id, price_plan_id, gateway_model, active)
         VALUES ($1,$2,$3,$4,'cost-path',true)",
    )
    .bind(revision)
    .bind(vendor_model)
    .bind(offering)
    .bind(price_plan)
    .execute(&pool)
    .await
    .expect("runtime entry fixture");

    // 两条停在"正在调上游"、持有租约的 Job：一条带成本事实进对账，一条不带。
    let mut jobs = Vec::new();
    for key in ["with-cost", "without-cost"] {
        let job_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO generation.jobs
                 (id, account_id, idempotency_key, request_hash, state, branch, gateway_model,
                  native_parameters, carrier_schema, parameter_mapping, runtime_revision_id,
                  vendor_model_id, offering_id, channel_id, price_snapshot, max_cost_microusd,
                  lease_owner, lease_expires_at)
             VALUES ($1,$2,$3,'hash','submitting','prompt_only','cost-path',
                     '{}'::jsonb,'{}'::jsonb,'{}'::jsonb,$4,$5,$6,$7,'{}'::jsonb,20000,
                     'worker-x', now() + interval '1 hour')",
        )
        .bind(job_id)
        .bind(account)
        .bind(key)
        .bind(revision)
        .bind(vendor_model)
        .bind(offering)
        .bind(channel)
        .execute(&pool)
        .await
        .expect("job fixture");
        let attempt_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO generation.attempts (id, job_id, state, request_digest)
             VALUES ($1,$2,'submitting','digest')",
        )
        .bind(attempt_id)
        .bind(job_id)
        .execute(&pool)
        .await
        .expect("attempt fixture");
        jobs.push((JobId(job_id), AttemptId(attempt_id)));
    }

    let failure = |provider_cost| AttemptFailure {
        provider_code: "result_delivery_failed".to_owned(),
        public_code: PublicErrorCode::OutcomeUnknown,
        message: "provider returned no image".to_owned(),
        trace_id: Some("task-1".to_owned()),
        kind: ProviderFailureKind::PlatformInternal,
        target_state: seeai_domain::JobState::ReconciliationRequired,
        hold_disposition: HoldDisposition::RetainForReconciliation,
        provider_cost,
    };
    let (with_cost, with_cost_attempt) = jobs[0];
    repository
        .fail_job(
            with_cost,
            "worker-x",
            Some(with_cost_attempt),
            failure(Some(ProviderCostFact {
                source: ProviderCostSource::Computed,
                amount_microusd: Some(5_950),
                currency: Some("USD".to_owned()),
                cny_microusd: Some(42_245),
            })),
        )
        .await
        .expect("the reconciliation path must record the cost it already has");

    let row = sqlx::query(
        "SELECT provider_cost_microusd, provider_cost_currency, provider_cost_source,
                provider_cost_cny_microusd, provider_trace_id
         FROM generation.attempts WHERE id = $1",
    )
    .bind(with_cost_attempt.0)
    .fetch_one(&pool)
    .await
    .expect("attempt after failure");
    assert_eq!(
        row.get::<Option<i64>, _>("provider_cost_microusd"),
        Some(5_950)
    );
    assert_eq!(
        row.get::<Option<String>, _>("provider_cost_currency")
            .as_deref(),
        Some("USD")
    );
    assert_eq!(
        row.get::<Option<String>, _>("provider_cost_source")
            .as_deref(),
        Some("computed")
    );
    assert_eq!(
        row.get::<Option<i64>, _>("provider_cost_cny_microusd"),
        Some(42_245)
    );
    assert_eq!(
        row.get::<Option<String>, _>("provider_trace_id").as_deref(),
        Some("task-1")
    );
    let cases: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM operations.reconciliation_cases WHERE job_id = $1",
    )
    .bind(with_cost.0)
    .fetch_one(&pool)
    .await
    .expect("cases");
    assert_eq!(cases, 1, "结果交付失败仍然进对账（与成本缺口不同）");

    // 没有成本事实（连用量都算不出）：四列留空，不写成 0。
    let (without_cost, without_cost_attempt) = jobs[1];
    repository
        .fail_job(
            without_cost,
            "worker-x",
            Some(without_cost_attempt),
            failure(None),
        )
        .await
        .expect("a failure without a cost fact is still recorded");
    let row = sqlx::query(
        "SELECT provider_cost_microusd, provider_cost_currency, provider_cost_source,
                provider_cost_cny_microusd
         FROM generation.attempts WHERE id = $1",
    )
    .bind(without_cost_attempt.0)
    .fetch_one(&pool)
    .await
    .expect("attempt after failure");
    assert!(
        row.get::<Option<i64>, _>("provider_cost_microusd")
            .is_none()
    );
    assert!(
        row.get::<Option<String>, _>("provider_cost_currency")
            .is_none()
    );
    assert!(
        row.get::<Option<String>, _>("provider_cost_source")
            .is_none()
    );
    assert!(
        row.get::<Option<i64>, _>("provider_cost_cny_microusd")
            .is_none()
    );

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// **迁移 0009 的增量路径**：旧库（只应用 0009 之前的迁移）上的数据在迁移后逐字不变，
/// 定价列留 NULL（旧修订没有定价），三处约束被放宽，汇率表落成空的。
///
/// 三处放宽不是顺手做的：不透支与"保底额可为 0"在库层面直接报错，而它们是这套口径的前提。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_pricing_migration_relaxes_the_balance_checks_on_an_existing_database() {
    let (database_url, database_name) = isolated_database_url().await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");
    let migrations = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../migrations");

    // 1) 只应用这次改动之前的迁移。
    let staged = std::env::temp_dir().join(format!("seeai-pricing-migrations-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&staged).expect("staging directory");
    for entry in std::fs::read_dir(&migrations).expect("migrations directory") {
        let entry = entry.expect("migration entry");
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".sql") && name.as_str() < "0009" {
            std::fs::copy(entry.path(), staged.join(&name)).expect("copy early migration");
        }
    }
    sqlx::migrate::Migrator::new(staged.clone())
        .await
        .expect("early migrator")
        .run(&pool)
        .await
        .expect("early migrations apply");

    // 2) 旧数据：一个已经发布过、**没有定价**的型号，外加一个余额为 0 的账户。
    let vendor_model = Uuid::new_v4();
    let channel = Uuid::new_v4();
    let offering = Uuid::new_v4();
    let price_plan = Uuid::new_v4();
    let revision = Uuid::new_v4();
    let account = Uuid::new_v4();
    let contract = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["model", "prompt"],
        "properties": {
            "model": {"const": "priced-legacy"},
            "prompt": {"type": "string"}
        }
    });
    sqlx::query(
        "INSERT INTO catalog.vendor_models
             (id, vendor_id, native_model_id, native_revision, capability_schema)
         VALUES ($1,'OpenAI','priced-legacy','legacy-revision',$2)",
    )
    .bind(vendor_model)
    .bind(&contract)
    .execute(&pool)
    .await
    .expect("legacy contract row");
    sqlx::query(
        "INSERT INTO supply.channels (id, provider_kind, base_url, credential_env)
         VALUES ($1,'AIHubMix','https://api.inferera.com','AIHUBMIX_API_KEY')",
    )
    .bind(channel)
    .execute(&pool)
    .await
    .expect("channel fixture");
    sqlx::query(
        "INSERT INTO supply.offerings
             (id, vendor_model_id, channel_id, adapter_key, provider_model_id, restrictions,
              carrier_schema, parameter_mapping)
         VALUES ($1,$2,$3,'aihubmix-image-v1','priced-legacy','{}'::jsonb,$4,'{}'::jsonb)",
    )
    .bind(offering)
    .bind(vendor_model)
    .bind(channel)
    .bind(&contract)
    .execute(&pool)
    .await
    .expect("offering fixture");
    sqlx::query(
        "INSERT INTO pricing.price_plans
             (id, offering_id, currency, text_input_microusd_per_million, image_input_microusd_per_million,
              text_output_microusd_per_million, image_output_microusd_per_million, source_url, approved_by)
         VALUES ($1,$2,'USD',5,8,10,30,'https://example.invalid/price','migration-test')",
    )
    .bind(price_plan)
    .bind(offering)
    .execute(&pool)
    .await
    .expect("price plan fixture");
    sqlx::query(
        "INSERT INTO publication.runtime_revisions
             (id, snapshot, published_by, gateway_model, vendor_model_id)
         VALUES ($1,'{}'::jsonb,'migration-test','priced-legacy',$2)",
    )
    .bind(revision)
    .bind(vendor_model)
    .execute(&pool)
    .await
    .expect("runtime revision fixture");
    sqlx::query(
        "INSERT INTO publication.runtime_entries
             (runtime_revision_id, vendor_model_id, offering_id, price_plan_id, gateway_model, active)
         VALUES ($1,$2,$3,$4,'priced-legacy',true)",
    )
    .bind(revision)
    .bind(vendor_model)
    .bind(offering)
    .bind(price_plan)
    .execute(&pool)
    .await
    .expect("runtime entry fixture");
    sqlx::query("INSERT INTO ledger.accounts (id, balance_microusd) VALUES ($1, 0)")
        .bind(account)
        .execute(&pool)
        .await
        .expect("account fixture");

    // 3) 补上整批迁移：定价列、汇率表与三处放宽都必须自己跑通。
    sqlx::migrate::Migrator::new(migrations)
        .await
        .expect("migrator")
        .run(&pool)
        .await
        .expect("the pricing migration must apply on an already-built database");

    // 4) 旧修订的定价列全部为 NULL：它没有定价，受理与结算走旧口径。
    let row = sqlx::query(
        "SELECT markup_bps, reference_cost_microusd, cost_currency, consumer_rates_cny,
                cost_basis, tier_prices, floor_amounts
         FROM publication.runtime_revisions WHERE id = $1",
    )
    .bind(revision)
    .fetch_one(&pool)
    .await
    .expect("runtime revision after migration");
    for column in [
        "markup_bps",
        "reference_cost_microusd",
        "cost_currency",
        "consumer_rates_cny",
        "cost_basis",
        "tier_prices",
        "floor_amounts",
    ] {
        assert!(
            row.try_get::<Option<Value>, _>(column)
                .expect("column probe")
                .is_none(),
            "{column} 在旧修订上必须留 NULL（不回填）"
        );
    }

    // 5) 汇率表落成空的：数值是外部事实，由管理员录入，迁移不预置任何一行。
    let rates: i64 = sqlx::query_scalar("SELECT count(*) FROM pricing.fx_rates")
        .fetch_one(&pool)
        .await
        .expect("fx rates");
    assert_eq!(rates, 0);

    // 6) 三处约束已放宽：余额可为负、保底额与预授权额可为 0。
    sqlx::query("UPDATE ledger.accounts SET balance_microusd = -1 WHERE id = $1")
        .bind(account)
        .execute(&pool)
        .await
        .expect("透支要能把余额扣成负数");
    sqlx::query(
        "INSERT INTO generation.jobs
             (id, account_id, idempotency_key, request_hash, state, branch, gateway_model,
              native_parameters, carrier_schema, parameter_mapping, runtime_revision_id,
              vendor_model_id, offering_id, channel_id, price_snapshot, max_cost_microusd)
         VALUES ($1,$2,'zero-hold','hash','accepted','prompt_only','priced-legacy',
                 '{}'::jsonb,'{}'::jsonb,'{}'::jsonb,$3,$4,$5,$6,'{}'::jsonb,0)",
    )
    .bind(Uuid::new_v4())
    .bind(account)
    .bind(revision)
    .bind(vendor_model)
    .bind(offering)
    .bind(channel)
    .execute(&pool)
    .await
    .expect("保底额可为 0");
    let job_id: Uuid =
        sqlx::query_scalar("SELECT id FROM generation.jobs WHERE idempotency_key = 'zero-hold'")
            .fetch_one(&pool)
            .await
            .expect("job");
    sqlx::query(
        "INSERT INTO ledger.holds (id, account_id, job_id, amount_microusd, status)
         VALUES ($1,$2,$3,0,'active')",
    )
    .bind(Uuid::new_v4())
    .bind(account)
    .bind(job_id)
    .execute(&pool)
    .await
    .expect("零保底额要能落下来");

    pool.close().await;
    let _ = std::fs::remove_dir_all(&staged);
    drop_isolated_database(&database_name).await;
}

/// **迁移 0010 的增量路径**：旧库（只应用 0010 之前的迁移）上的候选条目迁移后逐字不变、
/// 权重取默认 1；唯一索引换成"同一网关模型下同一条供给只能有一行"，**同一档因此可以有两条候选**。
///
/// 这条不是顺手做的：旧索引按 `(native_model_id, routing_priority)` 唯一，等价于"同一档只能有
/// 一条候选"——档内按权重分流要先有第二条候选，旧索引先把这条路堵死了。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_routing_weight_migration_keeps_existing_entries_and_allows_shared_tiers() {
    let (database_url, database_name) = isolated_database_url().await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");
    let migrations = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../migrations");

    // 1) 只应用这次改动之前的迁移。
    let staged = std::env::temp_dir().join(format!("seeai-weight-migrations-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&staged).expect("staging directory");
    for entry in std::fs::read_dir(&migrations).expect("migrations directory") {
        let entry = entry.expect("migration entry");
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".sql") && name.as_str() < "0010" {
            std::fs::copy(entry.path(), staged.join(&name)).expect("copy early migration");
        }
    }
    sqlx::migrate::Migrator::new(staged.clone())
        .await
        .expect("early migrator")
        .run(&pool)
        .await
        .expect("early migrations apply");

    // 2) 旧数据：一个已经发布过、只有一条候选的型号；另备一条供给给"同档第二条候选"用。
    let contract = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["model", "prompt"],
        "properties": {
            "model": {"const": "weighted-legacy"},
            "prompt": {"type": "string"}
        }
    });
    let mut offerings = Vec::new();
    // 合同是**模型级**唯一一份：同一型号的两条候选共用这一行，各自带自己的渠道、供给与价格计划。
    let vendor_model = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO catalog.vendor_models
             (id, vendor_id, native_model_id, native_revision, capability_schema)
         VALUES ($1,'OpenAI','weighted-legacy','legacy-revision',$2)",
    )
    .bind(vendor_model)
    .bind(&contract)
    .execute(&pool)
    .await
    .expect("legacy contract row");
    for _ in 0..2 {
        let channel = Uuid::new_v4();
        let offering = Uuid::new_v4();
        let price_plan = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO supply.channels (id, provider_kind, base_url, credential_env)
             VALUES ($1,'AIHubMix','https://api.inferera.com','AIHUBMIX_API_KEY')",
        )
        .bind(channel)
        .execute(&pool)
        .await
        .expect("channel fixture");
        sqlx::query(
            "INSERT INTO supply.offerings
                 (id, vendor_model_id, channel_id, adapter_key, provider_model_id, restrictions,
                  carrier_schema, parameter_mapping)
             VALUES ($1,$2,$3,'aihubmix-image-v1','weighted-legacy','{}'::jsonb,$4,'{}'::jsonb)",
        )
        .bind(offering)
        .bind(vendor_model)
        .bind(channel)
        .bind(&contract)
        .execute(&pool)
        .await
        .expect("offering fixture");
        sqlx::query(
            "INSERT INTO pricing.price_plans
                 (id, offering_id, currency, text_input_microusd_per_million, image_input_microusd_per_million,
                  text_output_microusd_per_million, image_output_microusd_per_million, source_url, approved_by)
             VALUES ($1,$2,'USD',5,8,10,30,'https://example.invalid/price','migration-test')",
        )
        .bind(price_plan)
        .bind(offering)
        .execute(&pool)
        .await
        .expect("price plan fixture");
        offerings.push((offering, price_plan));
    }
    let (offering, price_plan) = offerings[0];
    let revision = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO publication.runtime_revisions
             (id, snapshot, published_by, gateway_model, vendor_model_id)
         VALUES ($1,'{}'::jsonb,'migration-test','weighted-legacy',$2)",
    )
    .bind(revision)
    .bind(vendor_model)
    .execute(&pool)
    .await
    .expect("runtime revision fixture");
    sqlx::query(
        "INSERT INTO publication.runtime_entries
             (runtime_revision_id, vendor_model_id, offering_id, price_plan_id, gateway_model,
              active, routing_priority)
         VALUES ($1,$2,$3,$4,'weighted-legacy',true,0)",
    )
    .bind(revision)
    .bind(vendor_model)
    .bind(offering)
    .bind(price_plan)
    .execute(&pool)
    .await
    .expect("runtime entry fixture");

    // 3) 补上整批迁移。
    sqlx::migrate::Migrator::new(migrations)
        .await
        .expect("migrator")
        .run(&pool)
        .await
        .expect("the routing weight migration must apply on an already-built database");

    // 4) 旧条目逐字不变，权重取默认 1（不回填、不改写）。
    let row = sqlx::query(
        "SELECT routing_priority, weight FROM publication.runtime_entries
         WHERE runtime_revision_id = $1 AND offering_id = $2",
    )
    .bind(revision)
    .bind(offering)
    .fetch_one(&pool)
    .await
    .expect("legacy entry after migration");
    assert_eq!(
        row.try_get::<i32, _>("routing_priority").expect("priority"),
        0
    );
    assert_eq!(row.try_get::<i32, _>("weight").expect("weight"), 1);

    // 5) 旧索引已换成新的那条。
    let old_index: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_indexes
         WHERE schemaname = 'publication' AND indexname = 'one_active_entry_per_model_and_priority'",
    )
    .fetch_one(&pool)
    .await
    .expect("old index probe");
    assert_eq!(old_index, 0, "旧索引必须消失");
    let new_index: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_indexes
         WHERE schemaname = 'publication' AND indexname = 'one_active_entry_per_model_and_offering'",
    )
    .fetch_one(&pool)
    .await
    .expect("new index probe");
    assert_eq!(new_index, 1, "新索引必须建起来");

    // 6) 同一档现在可以有第二条候选（旧索引下这一条会撞唯一约束）。
    let (second_offering, second_price_plan) = offerings[1];
    sqlx::query(
        "INSERT INTO publication.runtime_entries
             (runtime_revision_id, vendor_model_id, offering_id, price_plan_id, gateway_model,
              active, routing_priority, weight)
         VALUES ($1,$2,$3,$4,'weighted-legacy',true,0,3)",
    )
    .bind(revision)
    .bind(vendor_model)
    .bind(second_offering)
    .bind(second_price_plan)
    .execute(&pool)
    .await
    .expect("同一档的第二条候选必须能落下来");

    // 7) 权重必须是正整数；同一网关模型下同一条供给只允许一行 active。
    let zero_weight = sqlx::query(
        "INSERT INTO publication.runtime_entries
             (runtime_revision_id, vendor_model_id, offering_id, price_plan_id, gateway_model,
              active, routing_priority, weight)
         VALUES ($1,$2,$3,$4,'weighted-legacy',true,1,0)",
    )
    .bind(revision)
    .bind(vendor_model)
    .bind(second_offering)
    .bind(second_price_plan)
    .execute(&pool)
    .await;
    assert!(zero_weight.is_err(), "权重 0 必须在库层被拒");
    let duplicate = sqlx::query(
        "INSERT INTO publication.runtime_entries
             (runtime_revision_id, vendor_model_id, offering_id, price_plan_id, gateway_model,
              active, routing_priority, weight)
         VALUES ($1,$2,$3,$4,'weighted-legacy',true,1,1)",
    )
    .bind(revision)
    .bind(vendor_model)
    .bind(second_offering)
    .bind(second_price_plan)
    .execute(&pool)
    .await;
    assert!(
        duplicate.is_err(),
        "同一网关模型下同一条供给的 active 条目只能有一条"
    );

    pool.close().await;
    let _ = std::fs::remove_dir_all(&staged);
    drop_isolated_database(&database_name).await;
}

// ───────────────────────────── 加速层（缓存）─────────────────────────────
//
// 这一组用例验的是"Redis 只是加速层"：扣减与余额事实只在数据库事务里发生，缓存写的是提交后的
// 值，陈旧一律回源，凭缓存提前拒绝必须留审计，缓存停掉结果逐位不变。
//
// 缓存服务用**进程内假 Redis**（与假上游同一套做法）：验收要能直接改坏缓存里的值、能让写入失败
// （模拟"失效没成功"），还要在没有 Redis 的机器上跑得起来。它只在 127.0.0.1 上监听、不出网。

/// 缓存参数：写进 API / Worker 进程的环境变量。默认值就是设计里给的那一套。
#[derive(Debug, Clone, Copy)]
struct CacheSettings {
    route_ttl_seconds: u64,
    balance_ttl_seconds: u64,
    freshness_window_ms: u64,
    reconcile_interval_ms: u64,
}

impl Default for CacheSettings {
    fn default() -> Self {
        Self {
            route_ttl_seconds: 60,
            balance_ttl_seconds: 360,
            freshness_window_ms: 5_000,
            reconcile_interval_ms: 180_000,
        }
    }
}

impl CacheSettings {
    /// 换掉新鲜窗口与对账周期。两者必须满足生产实现的那条校验（窗口至少小 4 倍），否则 API 进程
    /// 会因为配置不合法直接退出。
    fn with_windows(self, freshness_window_ms: u64, reconcile_interval_ms: u64) -> Self {
        Self {
            freshness_window_ms,
            reconcile_interval_ms,
            ..self
        }
    }
}

/// 假 Redis 里的一条：值 + 过期时刻（`None` 表示不过期）。
struct CacheEntry {
    value: String,
    expires_at: Option<tokio::time::Instant>,
}

/// 进程内假 Redis：只实现加速层用到的那几条命令。
///
/// 它不是"另一个实现"，而是**测试用的可观测替身**：用例可以读它、改它、让它拒绝写入，从而构造
/// "缓存被改错""失效没成功""缓存服务停掉"这三种现实里会发生、但没法靠真实 Redis 稳定复现的情形。
struct CacheFixture {
    url: String,
    settings: CacheSettings,
    state: Arc<Mutex<BTreeMap<String, CacheEntry>>>,
    fail_writes: Arc<std::sync::atomic::AtomicBool>,
    connections: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    listener: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl CacheFixture {
    async fn start(settings: CacheSettings) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake redis binds");
        let port = listener.local_addr().expect("addr").port();
        let state: Arc<Mutex<BTreeMap<String, CacheEntry>>> = Arc::new(Mutex::new(BTreeMap::new()));
        let fail_writes = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let connections: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>> =
            Arc::new(Mutex::new(Vec::new()));
        let handle = {
            let state = state.clone();
            let fail_writes = fail_writes.clone();
            let connections = connections.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((socket, _)) = listener.accept().await else {
                        break;
                    };
                    let state = state.clone();
                    let fail_writes = fail_writes.clone();
                    let served = tokio::spawn(async move {
                        let _ = serve_fake_redis(socket, state, fail_writes).await;
                    });
                    if let Ok(mut connections) = connections.lock() {
                        connections.push(served);
                    }
                }
            })
        };
        Self {
            url: format!("redis://127.0.0.1:{port}"),
            settings,
            state,
            fail_writes,
            connections,
            listener: Mutex::new(Some(handle)),
        }
    }

    fn url(&self) -> &str {
        &self.url
    }

    fn settings(&self) -> CacheSettings {
        self.settings
    }

    /// 缓存里的原文（不看 TTL）。
    fn raw(&self, key: &str) -> Option<String> {
        self.state
            .lock()
            .expect("cache state lock")
            .get(key)
            .map(|entry| entry.value.clone())
    }

    fn json(&self, key: &str) -> Option<Value> {
        self.raw(key)
            .map(|raw| serde_json::from_str(&raw).expect("cache values are JSON"))
    }

    /// 直接写一条（绕过服务）：用例用它构造"缓存被人为改错"。
    fn put(&self, key: &str, value: &Value) {
        self.state.lock().expect("cache state lock").insert(
            key.to_owned(),
            CacheEntry {
                value: value.to_string(),
                expires_at: None,
            },
        );
    }

    /// 让后续的 `SET` / `DEL` 全部失败：模拟"发布之后的失效没成功"。
    fn set_fail_writes(&self, fail: bool) {
        self.fail_writes
            .store(fail, std::sync::atomic::Ordering::SeqCst);
    }

    /// 关掉这个缓存服务：监听与已建立的连接一起断，客户端会看到连接被重置。
    fn stop(&self) {
        if let Ok(mut listener) = self.listener.lock()
            && let Some(handle) = listener.take()
        {
            handle.abort();
        }
        if let Ok(mut connections) = self.connections.lock() {
            for handle in connections.drain(..) {
                handle.abort();
            }
        }
    }

    fn balance(&self, account_id: &str) -> Option<Value> {
        self.json(&format!("user_balance:{account_id}"))
    }

    fn route(&self, gateway_model: &str) -> Option<Value> {
        self.json(&format!("route:{gateway_model}"))
    }

    /// 把缓存里的余额改成一个错值（写入时间与来源由用例指定）。
    fn corrupt_balance(
        &self,
        account_id: &str,
        balance_microusd: i64,
        source: &str,
        written_at: Value,
    ) {
        self.put(
            &format!("user_balance:{account_id}"),
            &json!({
                "balance_microusd": balance_microusd,
                "written_at": written_at,
                "source": source,
            }),
        );
    }

    /// 等到缓存里的余额变成这个值（对账是定时的，只能等）。
    async fn wait_for_balance(&self, account_id: &str, expected: i64) -> Option<Value> {
        for _ in 0..200 {
            if let Some(cached) = self.balance(account_id)
                && cached["balance_microusd"] == json!(expected)
            {
                return Some(cached);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        None
    }
}

/// 给子进程装上加速层的那几个环境变量。
fn apply_cache_env(command: &mut Command, cache: Option<&CacheFixture>) {
    let Some(cache) = cache else {
        // 不设 `REDIS_URL` 就是"没有缓存"：加速层不构造，路径与没有这一层时逐位相同。
        return;
    };
    let settings = cache.settings();
    command
        .env("REDIS_URL", cache.url())
        .env(
            "CACHE_ROUTE_TTL_SECONDS",
            settings.route_ttl_seconds.to_string(),
        )
        .env(
            "CACHE_BALANCE_TTL_SECONDS",
            settings.balance_ttl_seconds.to_string(),
        )
        .env(
            "CACHE_FRESHNESS_WINDOW_MS",
            settings.freshness_window_ms.to_string(),
        )
        .env(
            "CACHE_RECONCILE_INTERVAL_MS",
            settings.reconcile_interval_ms.to_string(),
        )
        .env("CACHE_OPERATION_TIMEOUT_MS", "200");
}

/// 假 Redis 的服务循环：读一条 RESP 命令、回一条应答。
async fn serve_fake_redis(
    socket: tokio::net::TcpStream,
    state: Arc<Mutex<BTreeMap<String, CacheEntry>>>,
    fail_writes: Arc<std::sync::atomic::AtomicBool>,
) -> std::io::Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    let (read_half, mut writer) = socket.into_split();
    let mut reader = BufReader::new(read_half);
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).await? == 0 {
            return Ok(());
        }
        let header = header.trim_end();
        let Some(count) = header.strip_prefix('*') else {
            // 客户端只用数组形式；真收到别的就当这条连接没法用了。
            return Ok(());
        };
        let count: usize = count.parse().unwrap_or(0);
        let mut args = Vec::with_capacity(count);
        for _ in 0..count {
            let mut bulk_header = String::new();
            if reader.read_line(&mut bulk_header).await? == 0 {
                return Ok(());
            }
            let length: usize = bulk_header
                .trim_end()
                .trim_start_matches('$')
                .parse()
                .unwrap_or(0);
            let mut buffer = vec![0_u8; length + 2];
            reader.read_exact(&mut buffer).await?;
            args.push(String::from_utf8_lossy(&buffer[..length]).into_owned());
        }
        let reply = fake_redis_command(&args, &state, &fail_writes);
        writer.write_all(reply.as_bytes()).await?;
    }
}

/// 一条命令的应答。只认加速层真正会发的那几条：**认不出来的一律报错**——假服务宽容地回 `+OK`
/// 会让"命令名写错了"这种错误在用例里悄悄通过，而真实 Redis 会直接拒绝它。
///
/// `CLIENT` 要放行：客户端建连接时会发两条 `CLIENT SETINFO`，它们的应答内容没人看。
fn fake_redis_command(
    args: &[String],
    state: &Arc<Mutex<BTreeMap<String, CacheEntry>>>,
    fail_writes: &Arc<std::sync::atomic::AtomicBool>,
) -> String {
    let name = args
        .first()
        .map(|value| value.to_ascii_uppercase())
        .unwrap_or_default();
    match name.as_str() {
        "GET" => {
            let Some(key) = args.get(1) else {
                return "-ERR wrong number of arguments\r\n".to_owned();
            };
            let mut state = state.lock().expect("cache state lock");
            let expired = state
                .get(key)
                .and_then(|entry| entry.expires_at)
                .is_some_and(|deadline| deadline <= tokio::time::Instant::now());
            if expired {
                state.remove(key);
            }
            match state.get(key) {
                Some(entry) => bulk_string(&entry.value),
                None => "$-1\r\n".to_owned(),
            }
        }
        "SET" => {
            if fail_writes.load(std::sync::atomic::Ordering::SeqCst) {
                return "-ERR writes are disabled in this test\r\n".to_owned();
            }
            let (Some(key), Some(value)) = (args.get(1), args.get(2)) else {
                return "-ERR wrong number of arguments\r\n".to_owned();
            };
            let expires_at = match (
                args.get(3).map(|option| option.to_ascii_uppercase()),
                args.get(4).and_then(|amount| amount.parse::<u64>().ok()),
            ) {
                (Some(option), Some(amount)) if option == "PX" => {
                    Some(tokio::time::Instant::now() + Duration::from_millis(amount))
                }
                (Some(option), Some(amount)) if option == "EX" => {
                    Some(tokio::time::Instant::now() + Duration::from_secs(amount))
                }
                _ => None,
            };
            state.lock().expect("cache state lock").insert(
                key.clone(),
                CacheEntry {
                    value: value.clone(),
                    expires_at,
                },
            );
            "+OK\r\n".to_owned()
        }
        "DEL" => {
            if fail_writes.load(std::sync::atomic::Ordering::SeqCst) {
                return "-ERR writes are disabled in this test\r\n".to_owned();
            }
            let mut state = state.lock().expect("cache state lock");
            let mut removed = 0_i64;
            for key in args.iter().skip(1) {
                if state.remove(key).is_some() {
                    removed += 1;
                }
            }
            format!(":{removed}\r\n")
        }
        "PING" => "+PONG\r\n".to_owned(),
        "CLIENT" => "+OK\r\n".to_owned(),
        other => format!("-ERR unknown command '{other}'\r\n"),
    }
}

fn bulk_string(value: &str) -> String {
    format!("${}\r\n{value}\r\n", value.len())
}

/// 这次用例发布的定价（与 P2b 的用例同一份口径），让"保底额"与"实收"都是确定的数。
async fn publish_cache_priced(harness: &Harness, consumer_rates: Value) -> StatusCode {
    let client = Client::new();
    republish_priced(
        harness,
        &client,
        openai_floor_amounts(),
        consumer_rates,
        2_000,
    )
    .await
}

/// 该账户在**数据库**里的余额（账本是权威）。
async fn database_balance(harness: &Harness, account_id: &str) -> i64 {
    sqlx::query_scalar("SELECT balance_microusd FROM ledger.accounts WHERE id = $1")
        .bind(Uuid::parse_str(account_id).expect("account id"))
        .fetch_one(&harness.pool)
        .await
        .expect("balance")
}

/// 某一类审计事件的载荷（运营要能发现平台侧事件）。
async fn audit_events(harness: &Harness, action: &str) -> Vec<Value> {
    sqlx::query_scalar(
        "SELECT payload FROM operations.audit_events WHERE action = $1 ORDER BY created_at ASC",
    )
    .bind(action)
    .fetch_all(&harness.pool)
    .await
    .expect("audit events")
}

/// 等内部执行记录跑到某个状态：起了 Worker 之后，Job 是被异步领走的，读一次不够。
async fn wait_for_job_state(harness: &Harness, key: &str, expected: &str) {
    for _ in 0..300 {
        if harness.job(key).await.1 == expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        harness.job(key).await.1,
        expected,
        "内部执行记录必须跑到这个状态"
    );
}

/// 对客响应里**可比对**的那部分：图片项各有哪些字段、是不是本机假上游给的 `url`。
///
/// `created` 是时间戳，`url` 里带着假上游每次随机的端口——两者逐位比不了。比的是响应结构：
/// 缓存开着与关掉，对客拿到的形状必须逐位相同。
fn comparable_response(body: &Value) -> Vec<(Vec<String>, bool)> {
    body["data"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    let mut keys: Vec<String> = item
                        .as_object()
                        .map(|object| object.keys().cloned().collect())
                        .unwrap_or_default();
                    keys.sort();
                    let is_url = item["url"]
                        .as_str()
                        .is_some_and(|url| url.starts_with("http://127.0.0.1:"));
                    (keys, is_url)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// **写穿**：充值、受理预授权扣减、结算三条路径都在数据库提交之后把余额写进缓存。
///
/// 一次用例把三条路径都走一遍：充值后缓存立刻是充值后的值；不跑 Worker 发一次请求（同步入口
/// 超时，但 Job 已经受理、预授权已经扣），缓存跟着变成"初始 − 保底额"；再起 Worker 把同一个 Job
/// 跑完，缓存变成结算后的余额。每一步都与数据库逐位比对。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn cache_write_through_makes_the_balance_visible_after_every_write() {
    let harness = Harness::start_with_cache(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
        1,
        CacheFixture::start(CacheSettings::default()).await,
    )
    .await;
    let client = Client::new();
    assert_eq!(
        publish_cache_priced(&harness, priced_consumer_rates()).await,
        StatusCode::OK,
        "带定价的发布必须成功"
    );

    // ① 充值：提交后立刻可见。
    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let cached = harness
        .cache()
        .balance(&account_id)
        .expect("充值之后缓存里必须立刻有余额");
    assert_eq!(cached["balance_microusd"], json!(1_000_000));
    assert_eq!(cached["source"], json!("db_commit"));
    assert_eq!(
        cached["balance_microusd"],
        json!(database_balance(&harness, &account_id).await),
        "缓存里的值与数据库逐位一致"
    );

    // ② 受理（预授权扣减）：不跑 Worker，同步入口 1 秒后超时；Job 已受理、保底额已扣。
    let key = format!("cache-hold-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "cache contract");
    request["size"] = json!("2K");
    request["quality"] = json!("low");
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "got {body}");
    let (job_id, state, _) = harness.job(&key).await;
    assert_eq!(state, "accepted");
    let cached = harness.cache().balance(&account_id).expect("受理之后缓存");
    assert_eq!(
        cached["balance_microusd"],
        json!(1_000_000 - 250_000),
        "缓存跟着变成扣掉保底额之后的值"
    );
    assert_eq!(
        cached["balance_microusd"],
        json!(database_balance(&harness, &account_id).await)
    );

    // route 缓存也建起来了，且带着**当前生效修订**的标识。
    let cached_route = harness
        .cache()
        .route(harness.model)
        .expect("受理之后 route 缓存必须建起来");
    let effective: Uuid =
        sqlx::query_scalar("SELECT runtime_revision_id FROM generation.jobs WHERE id = $1")
            .bind(job_id)
            .fetch_one(&harness.pool)
            .await
            .expect("job revision");
    assert_eq!(
        cached_route["runtime_revision_id"],
        json!(effective.to_string()),
        "route 缓存带的是写它那次发布的修订标识"
    );

    // ③ 结算：起 Worker 把同一个 Job 跑完，缓存变成实收之后的余额。
    let _worker = harness.spawn_worker();
    wait_for_job_state(&harness, &key, "succeeded").await;
    let settled = database_balance(&harness, &account_id).await;
    assert_eq!(settled, 1_000_000 - 43_680, "实收按对客费率向量算");
    let cached = harness.cache().balance(&account_id).expect("结算之后缓存");
    assert_eq!(cached["balance_microusd"], json!(settled));
    assert_eq!(cached["source"], json!("db_commit"));

    harness.cleanup().await;
}

/// **停掉缓存服务，结果逐位相同**：同一场景跑两遍（配了缓存但把服务关掉 / 完全不配缓存），
/// 实收、最终余额、Job 终态与对客响应体都逐位相同——降级是"全部回源数据库"，不是"另一条路径"。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn stopping_the_cache_leaves_acceptance_and_settlement_bit_identical() {
    /// 跑一遍完整场景，回读可比对的四个数。
    async fn run(cache: Option<CacheFixture>) -> (i64, i64, String, Vec<(Vec<String>, bool)>) {
        let draft = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
        let harness = match cache {
            Some(cache) => {
                Harness::start_with_cache(
                    draft,
                    None,
                    UpstreamBehaviour::aihubmix(SyncImageShape::Url),
                    64,
                    30,
                    cache,
                )
                .await
            }
            None => {
                Harness::start_with_draft(
                    draft,
                    None,
                    UpstreamBehaviour::aihubmix(SyncImageShape::Url),
                    64,
                )
                .await
            }
        };
        let client = Client::new();
        assert_eq!(
            publish_cache_priced(&harness, priced_consumer_rates()).await,
            StatusCode::OK
        );
        let (account_id, api_key) =
            funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
        // 充值把缓存写起来之后再把缓存服务关掉：这时"缓存里有一条值、服务却不可用"。
        if let Some(cache) = harness.cache.as_ref() {
            assert!(cache.balance(&account_id).is_some());
            cache.stop();
        }
        let _worker = harness.spawn_worker();
        let key = format!("cache-down-{}", Uuid::new_v4());
        let mut request = route_request(harness.model, "cache down contract");
        request["size"] = json!("2K");
        request["quality"] = json!("low");
        let (status, body) = post_json(
            &harness.base_url,
            &api_key,
            "/v1/images/generations",
            &key,
            &request,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "got {body}");
        let (job_id, state, _) = harness.job(&key).await;
        let captured = harness.captured_microusd(job_id).await;
        let balance = database_balance(&harness, &account_id).await;
        let shape = comparable_response(&body);
        harness.cleanup().await;
        (captured, balance, state, shape)
    }

    let with_cache = run(Some(CacheFixture::start(CacheSettings::default()).await)).await;
    let without_cache = run(None).await;
    assert_eq!(
        with_cache, without_cache,
        "缓存不可用与完全没有缓存必须逐位相同（实收、余额、终态、响应体）"
    );
    assert_eq!(with_cache.0, -43_680, "实收按对客费率向量算");
    assert_eq!(with_cache.1, 1_000_000 - 43_680);
}

/// **route 缓存陈旧不可用**：发布新修订之后让失效失败（或手工把值里的修订标识改旧）→ 受理
/// **回源数据库**读到新候选集，选路与定价都用新修订那一份。
///
/// 这条验的是"陈旧可检"：正确性不依赖"发布后的失效一定成功"。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_stale_route_cache_falls_back_to_the_database() {
    let harness = Harness::start_with_cache(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
        30,
        CacheFixture::start(CacheSettings::default()).await,
    )
    .await;
    let client = Client::new();
    assert_eq!(
        publish_cache_priced(&harness, priced_consumer_rates()).await,
        StatusCode::OK
    );
    let (_, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;

    // 先受理一次，把 route 缓存写起来（带着第一版修订的标识）。
    let first_key = format!("cache-route-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "route cache contract");
    request["size"] = json!("2K");
    request["quality"] = json!("low");
    let _worker = harness.spawn_worker();
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &first_key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let stale = harness.cache().route(harness.model).expect("route 缓存");
    assert_eq!(
        frozen_snapshot(&harness.pool, &first_key).await["consumer_rates_cny"],
        priced_consumer_rates()
    );

    // 换一份对客费率重发修订，并让**失效失败**：缓存里留着的还是第一版那份候选集。
    harness.cache().set_fail_writes(true);
    let mut higher = priced_consumer_rates();
    higher["image_output_micros_per_million"] = json!(440_000_000);
    assert_eq!(
        publish_cache_priced(&harness, higher.clone()).await,
        StatusCode::OK
    );
    harness.cache().set_fail_writes(false);
    assert_eq!(
        harness.cache().route(harness.model).expect("旧值还在"),
        stale,
        "失效失败了，缓存里留着的还是旧值（这正是要检出的情形）"
    );

    // 再受理一次：修订标识对不上 ⇒ 回源数据库，用新修订的定价。
    let second_key = format!("cache-route-next-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &second_key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        frozen_snapshot(&harness.pool, &second_key).await["consumer_rates_cny"],
        higher,
        "陈旧缓存必须回源，用新修订那一份定价"
    );
    assert_ne!(
        harness.cache().route(harness.model).expect("重建后的缓存"),
        stale,
        "回源之后缓存被重建成新修订那一份"
    );

    // 第二种陈旧形态：手工把值里的修订标识改旧，结果同样回源。
    let mut forged = harness.cache().route(harness.model).expect("缓存");
    forged["runtime_revision_id"] = json!(Uuid::new_v4().to_string());
    harness
        .cache()
        .put(&format!("route:{}", harness.model), &forged);
    let third_key = format!("cache-route-forged-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &third_key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        frozen_snapshot(&harness.pool, &third_key).await["consumer_rates_cny"],
        higher
    );

    harness.cleanup().await;
}

/// **陈旧缓存不得拒绝**：缓存里的余额偏低，但超出新鲜窗口（或来源是对账写回）→ 不提前拒绝，
/// 判定交给数据库，请求照常成功。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_stale_balance_entry_never_rejects() {
    let settings = CacheSettings::default().with_windows(30_000, 300_000);
    let harness = Harness::start_with_cache(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
        30,
        CacheFixture::start(settings).await,
    )
    .await;
    let client = Client::new();
    assert_eq!(
        publish_cache_priced(&harness, priced_consumer_rates()).await,
        StatusCode::OK
    );
    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let fresh = harness.cache().balance(&account_id).expect("充值之后缓存");
    let written_at = fresh["written_at"].clone();

    // ① 来源是对账写回：它只保证"与数据库一致"，不构成"刚有一笔钱变动过"的证据。
    harness
        .cache()
        .corrupt_balance(&account_id, 1, "reconciler", written_at);
    let _worker = harness.spawn_worker();
    let mut request = route_request(harness.model, "stale cache contract");
    request["size"] = json!("2K");
    request["quality"] = json!("low");
    let first_key = format!("cache-stale-reconciler-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &first_key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "对账写回的值不得用于拒绝：{body}");

    // ② 来源是写穿路径，但写入时间在窗口之外（一小时前）。
    let long_ago = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
    harness
        .cache()
        .corrupt_balance(&account_id, 1, "db_commit", json!(long_ago));
    let second_key = format!("cache-stale-old-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &second_key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "陈旧缓存不得拒绝：{body}");

    // 两次都真的扣了钱（判定交给了数据库），而且没有留下任何"凭缓存拒绝"的审计。
    assert_eq!(
        database_balance(&harness, &account_id).await,
        1_000_000 - 2 * 43_680
    );
    assert!(
        audit_events(&harness, "balance.precheck_rejected")
            .await
            .is_empty(),
        "没有发生凭缓存的拒绝"
    );

    harness.cleanup().await;
}

/// **误拒有审计**：缓存**新鲜**（来源写穿、写入时间在窗口内）且余额低于保底额 → 提前返回
/// 402，不建 Job、不扣款，同时留下一条审计（缓存余额、写入时间、来源与本次保底额）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_fresh_cache_rejection_is_audited() {
    let settings = CacheSettings::default().with_windows(30_000, 300_000);
    let harness = Harness::start_with_cache(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
        30,
        CacheFixture::start(settings).await,
    )
    .await;
    let client = Client::new();
    assert_eq!(
        publish_cache_priced(&harness, priced_consumer_rates()).await,
        StatusCode::OK
    );
    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;
    let fresh = harness.cache().balance(&account_id).expect("充值之后缓存");
    let written_at = fresh["written_at"].clone();
    // 缓存说"不够"（比 2K 档的保底额 ¥0.25 还少），数据库说"够"——这正是要能解释清楚的那一次。
    harness
        .cache()
        .corrupt_balance(&account_id, 1, "db_commit", written_at.clone());

    let key = format!("cache-reject-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "fresh cache rejection");
    request["size"] = json!("2K");
    request["quality"] = json!("low");
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::PAYMENT_REQUIRED, "got {body}");
    assert_eq!(body["error"]["code"], json!("insufficient_balance"));

    // 拒绝没有副作用：不建 Job、不扣款。
    let jobs: i64 =
        sqlx::query_scalar("SELECT count(*) FROM generation.jobs WHERE idempotency_key = $1")
            .bind(&key)
            .fetch_one(&harness.pool)
            .await
            .expect("job count");
    assert_eq!(jobs, 0, "凭缓存拒绝不建 Job");
    assert_eq!(database_balance(&harness, &account_id).await, 1_000_000);

    // 但必须留下一条能解释"为什么拒了这个客户"的审计。
    let events = audit_events(&harness, "balance.precheck_rejected").await;
    assert_eq!(events.len(), 1, "凭缓存拒绝必须留审计");
    assert_eq!(events[0]["cached_balance_microusd"], json!(1));
    assert_eq!(events[0]["cached_source"], json!("db_commit"));
    assert_eq!(events[0]["cached_written_at"], written_at);
    assert_eq!(events[0]["hold_microusd"], json!(250_000));
    assert_eq!(events[0]["gateway_model"], json!(harness.model));

    harness.cleanup().await;
}

/// **重放不受余额预检管辖**：同一个幂等键重发会去重成原来那个 Job，不新建、不扣款，所以哪怕
/// 缓存新鲜且余额已经低于保底额，也不能凭它回 402——否则"重发同一个键"就变成看余额脸色的行为。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_replayed_request_is_never_refused_by_the_balance_precheck() {
    let settings = CacheSettings::default().with_windows(30_000, 300_000);
    let harness = Harness::start_with_cache(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
        1,
        CacheFixture::start(settings).await,
    )
    .await;
    let client = Client::new();
    assert_eq!(
        publish_cache_priced(&harness, priced_consumer_rates()).await,
        StatusCode::OK
    );
    // 余额刚好够扣一次保底额（¥0.30 ≥ ¥0.25）：受理之后余额就低于保底额了。
    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 300_000).await;

    let key = format!("cache-replay-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "replay contract");
    request["size"] = json!("2K");
    request["quality"] = json!("low");
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "got {body}");
    let cached = harness.cache().balance(&account_id).expect("受理之后缓存");
    assert_eq!(
        cached["balance_microusd"],
        json!(50_000),
        "缓存新鲜，且已经低于 2K 档的保底额"
    );

    // 同一个键立刻重发：去重成原来那个 Job，不因为缓存说"不够"而被拒。
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::GATEWAY_TIMEOUT,
        "重放不得被预检拒：{body}"
    );
    let jobs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM generation.jobs WHERE account_id = $1 AND idempotency_key = $2",
    )
    .bind(Uuid::parse_str(&account_id).expect("account id"))
    .bind(&key)
    .fetch_one(&harness.pool)
    .await
    .expect("job count");
    assert_eq!(jobs, 1, "重放去重成原来那个 Job");
    assert_eq!(
        database_balance(&harness, &account_id).await,
        50_000,
        "重放不扣款"
    );
    assert!(
        audit_events(&harness, "balance.precheck_rejected")
            .await
            .is_empty(),
        "重放没有发生凭缓存的拒绝"
    );

    harness.cleanup().await;
}

/// **定时对账兜底**：把缓存里的余额与候选集改错 → 对账以数据库为准覆盖，并留下审计。
///
/// 覆盖之后的来源标记是 `reconciler`——它**不再**能用于提前拒绝（见上一条用例的口径）。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_reconciler_overwrites_corrupted_entries_from_the_database() {
    // 对账周期 1 秒、新鲜窗口 200 毫秒：用例等得起，而且满足"窗口显著小于周期"。
    let settings = CacheSettings::default().with_windows(200, 1_000);
    let harness = Harness::start_with_cache(
        candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
        None,
        UpstreamBehaviour::aihubmix(SyncImageShape::Url),
        64,
        30,
        CacheFixture::start(settings).await,
    )
    .await;
    let client = Client::new();
    assert_eq!(
        publish_cache_priced(&harness, priced_consumer_rates()).await,
        StatusCode::OK
    );
    let (account_id, api_key) =
        funded_account(&client, &harness.base_url, &harness.admin_token, 1_000_000).await;

    // 先受理一次把 route 缓存写起来，再把余额与候选集都改错。
    let _worker = harness.spawn_worker();
    let key = format!("cache-reconcile-{}", Uuid::new_v4());
    let mut request = route_request(harness.model, "reconcile contract");
    request["size"] = json!("2K");
    request["quality"] = json!("low");
    let (status, body) = post_json(
        &harness.base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &request,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let settled = database_balance(&harness, &account_id).await;
    let fresh = harness.cache().balance(&account_id).expect("结算之后缓存");
    harness
        .cache()
        .corrupt_balance(&account_id, 1, "db_commit", fresh["written_at"].clone());
    let mut forged_route = harness.cache().route(harness.model).expect("route 缓存");
    forged_route["runtime_revision_id"] = json!(Uuid::new_v4().to_string());
    harness
        .cache()
        .put(&format!("route:{}", harness.model), &forged_route);

    let corrected = harness
        .cache()
        .wait_for_balance(&account_id, settled)
        .await
        .expect("定时对账必须把缓存余额覆盖回数据库的值");
    assert_eq!(
        corrected["source"],
        json!("reconciler"),
        "对账写回的值来源是 reconciler（因此不再能用于提前拒绝）"
    );
    assert_eq!(corrected["balance_microusd"], json!(settled));

    // 候选集那条：对账校正它以当前生效修订为准（这里直接把它拿掉，下一次受理回源重建）。
    for _ in 0..200 {
        if harness.cache().route(harness.model).is_none() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        harness.cache().route(harness.model).is_none(),
        "对账必须把陈旧候选集拿掉"
    );

    let balance_events = audit_events(&harness, "cache.balance_corrected").await;
    assert!(
        !balance_events.is_empty(),
        "覆盖缓存必须留审计（运营要能发现缓存被动过）"
    );
    assert_eq!(balance_events[0]["cached_balance_microusd"], json!(1));
    assert_eq!(
        balance_events[0]["database_balance_microusd"],
        json!(settled)
    );
    assert!(
        !audit_events(&harness, "cache.route_invalidated")
            .await
            .is_empty(),
        "候选集被校正也要留审计"
    );

    harness.cleanup().await;
}
