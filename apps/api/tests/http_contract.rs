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
use seeai_application::HubRepository;
use seeai_domain::AccountId;
use seeai_persistence::PgHubRepository;
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use std::{
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
        let payload = serde_json::to_vec(&json!({
            "code": 200,
            "data": {
                "id": "task-contract-1",
                "status": status,
                "progress": 100,
                "cost": 0.00476,
                "credits_cost": 0.0476,
                "result": {"images": [{"url": [format!("http://127.0.0.1:{port}/result.png")], "expires_at": 4_000_000_000u64}]},
                "usage": {
                    "input_tokens": 14,
                    "input_tokens_details": {"cached_tokens": 0, "image_tokens": 0, "text_tokens": 14},
                    "output_tokens": 196,
                    "output_tokens_details": {"image_tokens": 196, "text_tokens": 0},
                    "total_tokens": 210
                }
            }
        }))
        .expect("task body");
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

/// 起一个平台 API 进程。
///
/// `sync_wait_seconds` 决定同步入口等多久：驱动测试给足（任务要跑完），
/// 只验受理与路由的测试给小值（不必真等）。
fn start_api(
    database_url: &str,
    sync_wait_seconds: u64,
    max_concurrent_jobs: u64,
) -> (String, String, ApiProcess) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("test port should bind");
    let port = listener.local_addr().expect("test address").port();
    drop(listener);
    let base_url = format!("http://127.0.0.1:{port}");
    let admin_token = format!("contract-admin-{}", Uuid::new_v4());
    let child = Command::new(env!("CARGO_BIN_EXE_seeai-api"))
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
        .stderr(Stdio::null())
        .spawn()
        .expect("API process should start");
    (base_url, admin_token, ApiProcess { child })
}

/// 起一个真实 Worker 进程（丢弃返回值即结束它）。
///
/// 环境变量只有一份，两个调用点（[`Harness`] 与直接起进程的用例）共用：两家渠道的凭证都写在
/// 测试进程的环境里，取值只在进程内假上游上用过，不写入配置、日志或响应。
fn spawn_worker_process(database_url: &str) -> WorkerProcess {
    let child = Command::new(worker_binary())
        .env("DATABASE_URL", database_url)
        .env("WORKER_ID", "driver-contract-worker")
        .env("WORKER_POLL_INTERVAL_MS", "200")
        .env("WORKER_LEASE_SECONDS", "300")
        .env("PROVIDER_TIMEOUT_SECONDS", "60")
        .env("APIMART_API_KEY", "contract-test-key")
        .env("AIHUBMIX_API_KEY", "contract-test-key")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("worker process should start");
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
            behaviour,
            64,
        )
        .await
    }

    /// 同 `start`，但指定分支、命名与并发上限。
    async fn start_with(
        provider_kind: &str,
        adapter_key: &str,
        branches: &[&str],
        behaviour: UpstreamBehaviour,
        max_concurrent_jobs: u64,
    ) -> Self {
        let credential_env = match provider_kind {
            "AIHubMix" => "AIHUBMIX_API_KEY",
            _ => "APIMART_API_KEY",
        };
        let mut draft = candidate(provider_kind, adapter_key, branches);
        draft["credential_env"] = Value::String(credential_env.to_owned());
        Self::start_with_draft(draft, behaviour, max_concurrent_jobs).await
    }

    /// 同 `start`，但用**素材里的真实声明面**发布（而不是测试手写的最小 Profile）。
    ///
    /// 参数面过滤按候选声明的字段名走，所以"哪些参数会被留下"只有在真实声明面上才验得准：
    /// 手写的最小 Profile 里除了 `prompt` 什么都没有，一过滤就把所有参数都丢了。
    async fn start_with_bootstrap(behaviour: UpstreamBehaviour, max_concurrent_jobs: u64) -> Self {
        let material: Value = match behaviour.provider {
            ProviderShape::Aihubmix => serde_json::from_str(include_str!(
                "../../../config/bootstrap/aihubmix-gpt-image-2.5-flare.json"
            )),
            ProviderShape::Apimart => serde_json::from_str(include_str!(
                "../../../config/bootstrap/apimart-gpt-image-2.5-flare.json"
            )),
        }
        .expect("bootstrap material parses");
        let mut draft = material["offerings"][0].clone();
        // 上游地址换成这个用例的假上游；凭证仍从环境变量读，值只写在测试进程环境里。
        draft["base_url"] = Value::String("http://127.0.0.1:1".to_owned());
        Self::start_with_draft(draft, behaviour, max_concurrent_jobs).await
    }

    async fn start_with_draft(
        mut draft: Value,
        behaviour: UpstreamBehaviour,
        max_concurrent_jobs: u64,
    ) -> Self {
        let (database_url, database_name) = isolated_database_url().await;
        let calls: UpstreamCalls = Arc::new(Mutex::new(Vec::new()));
        let upstream = start_fake_upstream_with(calls.clone(), behaviour).await;
        let (base_url, admin_token, process) = start_api(&database_url, 30, max_concurrent_jobs);
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
        let published =
            publish_candidates(&client, &base_url, &admin_token, Self::MODEL, vec![draft]).await;
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
        }
    }

    /// 起一个真实 Worker 进程（丢弃返回值即结束它）。
    fn spawn_worker(&self) -> WorkerProcess {
        spawn_worker_process(&self.database_url)
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
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64);
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
        let harness =
            Harness::start_with(provider_kind, adapter_key, &["prompt_only"], behaviour, 64).await;
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
    let (base_url, admin_token, _process) = start_api(&database_url, 1, 64);
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

/// 第二阶段的**发布素材**要真的能用：同一个 Vendor Model 只落**一份合同**，
/// 而每个候选各带**自己的承载面**——缺一不可：素材发不出去、或候选没带上自己的承载面，
/// 都算没覆盖。
///
/// 用 `config/bootstrap/` 里已备好的 2.5 素材发布：AIHubMix 与 APIMart 供同一型号。
/// 两家能承载的字段面不同（AIHubMix 收 `image` / `mask`，APIMart 收 `image_urls` / `mask_url`），
/// 正是"合同一份、承载面各一份"要覆盖的情形。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn stage_two_bootstrap_material_publishes_one_contract_with_per_candidate_carriers() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64);
    let client = Client::new();
    wait_until_ready(&client, &base_url).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    // 两份素材分别是各自的完整发布命令（一个候选）。
    let aihubmix: Value = serde_json::from_str(include_str!(
        "../../../config/bootstrap/aihubmix-gpt-image-2.5-flare.json"
    ))
    .expect("AIHubMix 2.5 material parses");
    let apimart: Value = serde_json::from_str(include_str!(
        "../../../config/bootstrap/apimart-gpt-image-2.5-flare.json"
    ))
    .expect("APIMart 2.5 material parses");
    let model = aihubmix["native_model_id"]
        .as_str()
        .expect("native model id")
        .to_owned();
    assert_eq!(
        apimart["native_model_id"], aihubmix["native_model_id"],
        "both materials must supply the same vendor model"
    );
    // 两家能承载的面必须真的不同——否则这个用例覆盖不到"承载面各自一份"。
    let aihubmix_carrier = aihubmix["offerings"][0]["capability_schema"].clone();
    let apimart_carrier = apimart["offerings"][0]["capability_schema"].clone();
    assert_ne!(
        aihubmix_carrier, apimart_carrier,
        "this test only covers the split surface if the two carriers actually differ"
    );

    // 合并成一次发布：**顶层一份合同** + 两个候选，顺序即优先级。
    // 合同取两家承载面的**名字并集**：名字收得住即可，同一个名字的取值形态怎么统一
    // 是尺寸语义那一步的事，本步只把"字段面"这条边界立住。
    let contract = contract_over(&[&aihubmix_carrier, &apimart_carrier]);
    let mut aihubmix_offering = aihubmix["offerings"][0].clone();
    let mut apimart_offering = apimart["offerings"][0].clone();
    for (offering, carrier) in [
        (&mut aihubmix_offering, &aihubmix_carrier),
        (&mut apimart_offering, &apimart_carrier),
    ] {
        // 承载面改用新名字，并去掉旧字段：这个用例走的必须是新形状。
        offering["carrier_schema"] = carrier.clone();
        offering
            .as_object_mut()
            .expect("offering object")
            .remove("capability_schema");
    }
    let merged = json!({
        "vendor_id": aihubmix["vendor_id"],
        "native_model_id": aihubmix["native_model_id"],
        "native_revision": "stage-two-material-1",
        "actor": "contract-test",
        "capability_schema": contract,
        "offerings": [aihubmix_offering, apimart_offering]
    });
    let published = client
        .post(format!("{base_url}/api/v1/runtime-revisions"))
        .bearer_auth(&admin_token)
        .json(&merged)
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
         WHERE vendor_id = $1 AND native_model_id = $2 AND native_revision = 'stage-two-material-1'",
    )
    .bind(aihubmix["vendor_id"].as_str().expect("vendor id"))
    .bind(&model)
    .fetch_all(&pool)
    .await
    .expect("contract rows");
    assert_eq!(contracts.len(), 1, "one contract per vendor model revision");
    let stored_contract: Value = contracts[0].try_get("capability_schema").expect("contract");
    assert_eq!(
        stored_contract, merged["capability_schema"],
        "the stored contract must be the one given at the top level"
    );

    // 每个候选携带**它自己**的承载面，而合同只读那一份。
    let rows = sqlx::query(
        "SELECT o.provider_model_id, o.adapter_key, o.carrier_schema, c.provider_kind, vm.capability_schema
         FROM publication.runtime_entries re
         JOIN supply.offerings o ON o.id = re.offering_id
         JOIN supply.channels c ON c.id = o.channel_id
         JOIN catalog.vendor_models vm ON vm.id = re.vendor_model_id
         WHERE re.active AND re.gateway_model = $1
         ORDER BY re.routing_priority",
    )
    .bind(&model)
    .fetch_all(&pool)
    .await
    .expect("candidate rows");
    assert_eq!(rows.len(), 2, "both providers must be active candidates");

    let carriers: Vec<Value> = rows
        .iter()
        .map(|row| row.try_get("carrier_schema").expect("carrier"))
        .collect();
    assert_ne!(
        carriers[0], carriers[1],
        "each candidate must carry its own surface, not a shared one"
    );
    for (index, expected) in [(0_usize, &aihubmix), (1, &apimart)].iter() {
        let row = &rows[*index];
        let offering = &expected["offerings"][0];
        let provider_model_id: String = row.try_get("provider_model_id").expect("provider model");
        let adapter_key: String = row.try_get("adapter_key").expect("adapter key");
        assert_eq!(provider_model_id, offering["provider_model_id"]);
        assert_eq!(adapter_key, offering["adapter_key"]);
        assert_eq!(
            carriers[*index], offering["capability_schema"],
            "candidate {index} must carry its own surface"
        );
        // 每个候选读到的合同都是同一份，且它的 `model.const` 就是该型号。
        let contract: Value = row.try_get("capability_schema").expect("contract");
        assert_eq!(contract, stored_contract);
        assert_eq!(contract["properties"]["model"]["const"], model);
        // 承载面的每个字段名都要在合同里（R1 的判据，这里独立复核一遍）。
        for name in carriers[*index]["properties"]
            .as_object()
            .expect("carrier properties")
            .keys()
        {
            assert!(
                contract["properties"]
                    .as_object()
                    .expect("contract properties")
                    .contains_key(name),
                "carrier field {name} must be declared by the contract"
            );
        }
    }

    // 快照指纹跟着"合同 + 承载面"走：两个候选承载面不同，指纹就该不同。
    let snapshot: Value = sqlx::query_scalar(
        "SELECT snapshot FROM publication.runtime_revisions rr
         JOIN publication.runtime_entries re ON re.runtime_revision_id = rr.id
         WHERE re.active AND re.gateway_model = $1 LIMIT 1",
    )
    .bind(&model)
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

/// 两份承载面的**字段名并集**，做成一份封闭的模型级合同。
fn contract_over(carriers: &[&Value]) -> Value {
    let mut properties = serde_json::Map::new();
    for carrier in carriers {
        for (name, definition) in carrier["properties"]
            .as_object()
            .expect("carrier declares properties")
        {
            properties
                .entry(name.clone())
                .or_insert_with(|| definition.clone());
        }
    }
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["model", "prompt"],
        "properties": properties
    })
}

/// 现有三份 AIHubMix 素材（**旧形状**：offering 级 `capability_schema`）照常能发布：
/// 过渡期里合同与承载面都回退到那一份声明面，不必等素材改写。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn legacy_aihubmix_materials_still_publish() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64);
    let client = Client::new();
    wait_until_ready(&client, &base_url).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    let materials = [
        include_str!("../../../config/bootstrap/aihubmix-gpt-image-2.json"),
        include_str!("../../../config/bootstrap/aihubmix-gpt-image-2.5-flare.json"),
        include_str!("../../../config/bootstrap/aihubmix-gpt-image-2.5-sunburst.json"),
    ];
    for material in materials {
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
    }

    pool.close().await;
    drop_isolated_database(&database_name).await;
}

/// 承载面 ⊆ 合同（R1）：供给不能凭空多出调用方可提交的字段。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn carrier_field_outside_the_contract_is_rejected() {
    let (database_url, database_name) = isolated_database_url().await;
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64);
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
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64);
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
    let (base_url, admin_token, _process) = start_api(&database_url, 2, 64);
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
    let (base_url, admin_token, _process) = start_api(&database_url, 1, 64);
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
    let (base_url, admin_token, _process) = start_api(&database_url, 1, 64);
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
    let harness =
        Harness::start_with_draft(draft, UpstreamBehaviour::aihubmix(SyncImageShape::Url), 64)
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
    let harness =
        Harness::start_with_draft(draft, UpstreamBehaviour::aihubmix(SyncImageShape::Url), 64)
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
    let (base_url, admin_token, _process) = start_api(&database_url, 1, 64);
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
    let (base_url, admin_token, _process) = start_api(&database_url, 1, 64);
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
    let harness =
        Harness::start_with_draft(draft, UpstreamBehaviour::aihubmix(SyncImageShape::Url), 64)
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
    let harness = Harness::start_with_draft(draft, UpstreamBehaviour::apimart(), 64).await;

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
    let (base_url, admin_token, _process) = start_api(&database_url, 1, 64);
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
    let (base_url, admin_token, _process) = start_api(&database_url, 1, 64);
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
    let (base_url, admin_token, _process) = start_api(&database_url, 1, 64);
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

/// 新形状的 2.5 素材（**一个 Vendor Model 一份文件**）端到端跑一遍：同一个型号只落**一份合同**，
/// 两条供给各带自己的承载面与参数映射，选路按承载面走。
///
/// 三条请求各钉一件事：
/// - 只带 `prompt`：首选的 AIHubMix 承载得了，请求就该落在它身上；
/// - 带 `background`：AIHubMix 的承载面没声明这个字段（它的字段面按 /v1 端点的机器 Schema 声明，
///   而渠道文档比机器 Schema 宽）→ 该候选**不合格**（判定记录写明原因）、改道 APIMart，且这个字段
///   要原样出现在发给 APIMart 的报文里；
/// - 带参考图：合同字段叫 `image`，APIMart 线上叫 `image_urls`，靠改名落到渠道字段名上
///   （报文里不许出现 `image`），内联图先经上传接口换成公网 URL。
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
    let (base_url, admin_token, _process) = start_api(&database_url, 30, 64);
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
        let mut material: Value = serde_json::from_str(material).expect("material parses");
        let model = material["native_model_id"]
            .as_str()
            .expect("native model id")
            .to_owned();
        let offerings = material["offerings"]
            .as_array_mut()
            .expect("offerings must be an array");
        assert_eq!(
            offerings.len(),
            2,
            "一份素材两条供给：AIHubMix 首选、APIMart 次之"
        );
        // 上游地址换成这个用例的两个假上游，各按渠道给：凭证仍只从环境变量读。
        for offering in offerings.iter_mut() {
            offering["base_url"] = Value::String(match offering["provider_kind"].as_str() {
                Some("AIHubMix") => aihubmix_upstream.base_url.clone(),
                Some("APIMart") => apimart_upstream.base_url.clone(),
                other => panic!("unexpected provider kind {other:?}"),
            });
        }
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
            "{model} 素材必须能发布：{:?}",
            published.text().await
        );

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
        .bind(&model)
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
        // 这个用例的差集就在这里：AIHubMix 的承载面没声明 `background`（它的字段面按 /v1 端点的
        // 机器 Schema 声明），APIMart 声明了。**承载面没声明不等于渠道收不了**：渠道文档的请求
        // 参数表把 background 写上了，只是这一版没按文档把它补进承载面。
        assert!(
            carriers[0]["properties"].get("background").is_none(),
            "AIHubMix 的承载面没声明 background"
        );
        assert!(
            carriers[1]["properties"].get("background").is_some(),
            "APIMart 声明了 background"
        );
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
    }

    // 三条请求共用一个真实 Worker：它只领 Job，不知道这次用例在验什么。
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

    // ── 用例 2：带 background → AIHubMix 承载不了，改道 APIMart，字段原样上行 ──
    let key = format!("contract-background-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &json!({
            "model": "gpt-image-2.5-flare",
            "prompt": "白色运动鞋，透明背景",
            "background": "transparent"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "改道之后要真的跑通：{body}");
    assert_sync_success("带 background 的请求", &body);
    let (chosen, considered) = routing_of(&pool, &key).await;
    assert_eq!(considered.len(), 2, "两个候选都要进判定记录");
    assert_eq!(
        considered[0]["eligible"], false,
        "AIHubMix 承载不了请求用到的 background：{considered:?}"
    );
    assert!(
        considered[0]["skip_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("background")),
        "落选原因必须写明承载不了哪个字段：{considered:?}"
    );
    assert_eq!(considered[1]["eligible"], true);
    assert_eq!(
        chosen_provider_kind(&pool, chosen).await,
        "APIMart",
        "第一条承载不了就该落到下一条：{considered:?}"
    );
    let submit = last_submit_body(&apimart_calls, "/v1/images/generations");
    assert_eq!(
        submit["background"], "transparent",
        "承载得了的字段必须原样上行：{submit}"
    );

    // ── 用例 3：带参考图 → 合同字段名不上线，渠道字段名上 ──
    let key = format!("contract-image-{}", Uuid::new_v4());
    let (status, body) = post_json(
        &base_url,
        &api_key,
        "/v1/images/generations",
        &key,
        &json!({
            "model": "gpt-image-2.5-sunburst",
            "prompt": "保留商品主体，把背景换成米白色摄影棚",
            // `background` 在这里只是把请求逼到 APIMart：AIHubMix 的承载面没声明它。
            "background": "opaque",
            "image": [png_data_url()]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "带图请求要真的跑通：{body}");
    assert_sync_success("带参考图的请求", &body);
    let (chosen, _) = routing_of(&pool, &key).await;
    assert_eq!(chosen_provider_kind(&pool, chosen).await, "APIMart");
    // 内联图先换成渠道要的公网 URL，再按**渠道字段名**装进生成请求。
    assert_eq!(
        count_calls(&apimart_calls, "POST", "/v1/uploads/images"),
        1,
        "内联参考图必须先上传换成公网 URL"
    );
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
        .post(format!("{base_url}/api/v1/runtime-revisions"))
        .bearer_auth(admin_token)
        .json(&body)
        .send()
        .await
        .expect("runtime publication")
        .status()
}

/// 测试构造体：模型 + 提示词（幂等键另走请求头）。
fn route_request(model: &str, prompt: &str) -> Value {
    json!({"model": model, "prompt": prompt})
}

/// 合同里的文生图请求体（bootstrap 素材的模型名）。
fn generation_request_body(prompt: &str) -> Value {
    json!({"model": "gpt-image-2", "prompt": prompt, "n": 1, "quality": "low"})
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

async fn publish_bootstrap(client: &Client, base_url: &str, admin_token: &str) {
    let config: Value = serde_json::from_str(include_str!(
        "../../../config/bootstrap/aihubmix-gpt-image-2.json"
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

async fn reject_mismatched_model_identity(client: &Client, base_url: &str, admin_token: &str) {
    let mut config: Value = serde_json::from_str(include_str!(
        "../../../config/bootstrap/aihubmix-gpt-image-2.json"
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
async fn get_catalog(client: &Client, base_url: &str, api_key: &str) -> (StatusCode, Value) {
    let response = client
        .get(format!("{base_url}/v1/models"))
        .bearer_auth(api_key)
        .send()
        .await
        .expect("catalog request");
    let status = response.status();
    let raw = response.text().await.expect("catalog body");
    (
        status,
        serde_json::from_str(&raw).unwrap_or(Value::String(raw)),
    )
}

/// 对客目录：`GET /v1/models` 只列**当前真的能调**的型号，合同就是发布的那一份。
///
/// 判据与受理期选路**同一条**（生效的发布条目 + 启用的供给 + 启用的渠道）：目录里列出的型号
/// 必须真的提交得起来。列着却提交不了比不列更糟——调用方会照它建表单，然后在提交时落空。
/// 本用例只读发布物与目录，不起 Worker、不连上游：零外部调用。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_model_catalog_lists_only_callable_models_with_their_published_contract() {
    let (database_url, database_name) = isolated_database_url().await;
    // 同步入口在这个用例里只用来验"停用之后真的调不了"；那一步在受理前就失败，不会等超时。
    let (base_url, admin_token, _process) = start_api(&database_url, 1, 64);
    let client = Client::new();
    wait_until_ready(&client, &base_url).await;
    let account = create_account(&client, &base_url, &admin_token).await;
    let api_key = issue_key(&client, &base_url, &admin_token, &account).await;
    let pool = PgPool::connect(&database_url)
        .await
        .expect("contract database");

    // ── 没有 Key：401，且走现有的对客错误信封；乱给的 Key 同样是 401 ──
    let unauthorized = client
        .get(format!("{base_url}/v1/models"))
        .send()
        .await
        .expect("catalog request");
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    let envelope: Value = unauthorized.json().await.expect("error envelope");
    assert_eq!(
        envelope["error"]["code"].as_str(),
        Some("authorization_required")
    );
    assert_public_only("无 Key 的目录请求", &envelope);
    let rejected = client
        .get(format!("{base_url}/v1/models"))
        .bearer_auth("sk_seeai_not_a_real_key")
        .send()
        .await
        .expect("catalog request with an unknown key");
    assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);

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

    // ── 带 Key：两个型号都在，形状是 `{name, vendor, revision, contract}` ──
    let (status, catalog) = get_catalog(&client, &base_url, &api_key).await;
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
        assert_eq!(
            entry.as_object().map(serde_json::Map::len),
            Some(4),
            "目录条目只有 name / vendor / revision / contract：{entry}"
        );
        assert_eq!(entry["vendor"].as_str(), Some("OpenAI"));
        assert_eq!(entry["revision"].as_str(), Some(revision));
        assert_eq!(
            &entry["contract"], schema,
            "目录里的合同必须与发布的那一份逐字一致"
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
    let (_, catalog) = get_catalog(&client, &base_url, &api_key).await;
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
    let (status, catalog) = get_catalog(&client, &base_url, &api_key).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        catalog,
        json!({"data": []}),
        "一个可调型号都没有时，目录是空列表而不是错误：{catalog}"
    );

    pool.close().await;
    drop_isolated_database(&database_name).await;
}
