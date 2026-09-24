//! 端到端合同测试的共享装置：进程内假上游与假 Redis、真实 API / Worker 进程的启停、独立空库、
//! `Harness` 驱动、管理员与对客 HTTP 辅助，以及跨用例共用的断言。
//!
//! 各 `cases_*.rs` 用 `#[path]` 声明为本模块的子模块：装置条目因此不必对外可见——这是这批文件
//! 从单文件搬运过来能不动一行可见性的前提。代价是模块拓扑不能只看目录名，新增一个用例文件
//! 要在这里登记。

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

#[path = "cases_aihubmix.rs"]
mod cases_aihubmix;
#[path = "cases_apimart.rs"]
mod cases_apimart;
#[path = "cases_cache.rs"]
mod cases_cache;
#[path = "cases_cost_facts.rs"]
mod cases_cost_facts;
#[path = "cases_lifecycle.rs"]
mod cases_lifecycle;
#[path = "cases_migrations.rs"]
mod cases_migrations;
#[path = "cases_parameter_mapping.rs"]
mod cases_parameter_mapping;
#[path = "cases_parameters.rs"]
mod cases_parameters;
#[path = "cases_pricing.rs"]
mod cases_pricing;
#[path = "cases_public_surface.rs"]
mod cases_public_surface;
#[path = "cases_publication.rs"]
mod cases_publication;
#[path = "cases_routing.rs"]
mod cases_routing;

// 夹具自身的检查：不启平台进程、不用数据库，因此不进 `#[ignore]`，由 workspace 单测那一步跑。
#[path = "harness_check.rs"]
mod harness_check;

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
    /// 终态里**没有结果图**（`result.images` 是空数组），但金额照给。
    ///
    /// 这是"上游明明给了金额、这次却没出图"的形态：用来观察那笔成本会不会丢。
    terminal_without_images: bool,
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
            terminal_without_images: false,
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
    // 正经读完一个请求：请求行 + 头 + 按 Content-Length 读满请求体。
    let (method, path, body) = read_request(socket).await?;
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
        let images = if behaviour.terminal_without_images {
            json!([])
        } else {
            json!([{
                "url": [format!("http://127.0.0.1:{port}/result.png")],
                "expires_at": 4_000_000_000u64
            }])
        };
        let mut task = json!({
            "code": 200,
            "data": {
                "id": "task-contract-1",
                "status": status,
                "progress": 100,
                "result": {"images": images},
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

/// 读完一个 HTTP 请求：请求行、头，以及按 `Content-Length` 读满的请求体。
///
/// 两个进程内接收器（假上游与告警接收器）共用它：要验的正是"线上到底发了什么"，
/// 所以两边都按同一套办法把原始报文读出来。
async fn read_request(
    socket: &mut tokio::net::TcpStream,
) -> std::io::Result<(String, String, Vec<u8>)> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

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
    Ok((method, path, body))
}

/// 一个只收告警的本地接收器：记下每条请求的正文（告警就是 JSON），按给定状态码应答。
///
/// 告警是**旁路**，所以接收器可以正常应答、回错、或者根本不存在：三种都只该影响"送出去了没有"，
/// 不影响 Job 的处置与对客结果。
struct AlertReceiver {
    url: String,
    bodies: Arc<Mutex<Vec<Value>>>,
    _task: tokio::task::JoinHandle<()>,
}

impl AlertReceiver {
    async fn start(status: u16) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("the alert receiver binds a local port");
        let port = listener
            .local_addr()
            .expect("the alert receiver address")
            .port();
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let recorded = bodies.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let recorded = recorded.clone();
                tokio::spawn(async move {
                    let _ = serve_alert(&mut socket, recorded, status).await;
                });
            }
        });
        Self {
            url: format!("http://127.0.0.1:{port}/alerts"),
            bodies,
            _task: task,
        }
    }

    fn bodies(&self) -> Vec<Value> {
        self.bodies.lock().expect("alert bodies lock").clone()
    }

    /// 等到收到 `count` 条为止（有上限地等）：外发发生在另一个进程里，断言要等它到。
    async fn wait_for(&self, count: usize) -> Vec<Value> {
        for _ in 0..200 {
            let bodies = self.bodies();
            if bodies.len() >= count {
                return bodies;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        self.bodies()
    }
}

async fn serve_alert(
    socket: &mut tokio::net::TcpStream,
    bodies: Arc<Mutex<Vec<Value>>>,
    status: u16,
) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;

    let (_, _, body) = read_request(socket).await?;
    if let Ok(mut bodies) = bodies.lock() {
        bodies.push(serde_json::from_slice(&body).unwrap_or(Value::Null));
    }
    let reason = if status < 400 { "OK" } else { "Error" };
    let head =
        format!("HTTP/1.1 {status} {reason}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
    socket.write_all(head.as_bytes()).await?;
    socket.flush().await
}

/// 一条告警载荷的**键**（排序后）：用例据此钉住"外发的就是那四个定位字段"。
fn alert_keys(alert: &Value) -> Vec<String> {
    let mut keys: Vec<String> = alert
        .as_object()
        .expect("an alert payload is a JSON object")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
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

impl ApiProcess {
    /// 子进程已经退出的话，返回它的退出状态。
    ///
    /// 它 bind 失败时会**静默退出**（`stderr` 是 null），所以这是"端口上坐着的不是我们"最直接的
    /// 信号，也是启动失败时唯一还拿得到的线索。
    fn exit_status(&mut self) -> Option<String> {
        self.child.try_wait().ok().flatten().map(|s| s.to_string())
    }
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
    start_api_with(
        database_url,
        sync_wait_seconds,
        max_concurrent_jobs,
        &ApiProcessSettings::default(),
    )
    .await
}

/// 一次用例要给 API 进程配的**速率上限**（每把 API Key）。
///
/// 默认那一套是每分钟 60 次，用例要观察"超限被拒"就得把它调到 1 次——按默认值跑，光是把上限
/// 撞到就要先发 61 次请求。字段直接就是那两个环境变量的值。
#[derive(Debug, Clone, Copy)]
struct ApiRateLimit {
    requests_per_window: u64,
    window_ms: u64,
}

impl ApiRateLimit {
    /// 一个窗口只放一次：第二次请求必然越限，用例不必等到 60 次。
    fn once_per(window_ms: u64) -> Self {
        Self {
            requests_per_window: 1,
            window_ms,
        }
    }
}

/// API 进程的三项可选配置：**加速层**、**每把密钥的速率上限**与**每账户每日扣费上限**。
///
/// 前两项绑在一起，是因为速率计数就落在加速层的缓存端口上：没有缓存时计数无处可落，限流那一层
/// 也就无从谈起（见 `AccelerationService::consume_request_slot`）。
///
/// 第三项**不**依赖缓存：每日扣费上限问的是账本上的事实，有没有加速层都从账本聚合，所以它可以
/// 单独配。这也正是它与速率上限分开成两个字段的原因——它们只是碰巧都读环境变量。
#[derive(Default)]
struct ApiProcessSettings {
    cache: Option<CacheFixture>,
    rate_limit: Option<ApiRateLimit>,
    daily_spend_limit_microusd: Option<u64>,
}

impl ApiProcessSettings {
    /// 配置好加速层，并用默认的速率上限（每分钟 60 次）。
    fn with_cache(cache: CacheFixture) -> Self {
        Self {
            cache: Some(cache),
            ..Self::default()
        }
    }

    /// 同 [`Self::with_cache`]，但把速率上限也调小。
    fn with_cache_and_rate_limit(cache: CacheFixture, rate_limit: ApiRateLimit) -> Self {
        Self {
            cache: Some(cache),
            rate_limit: Some(rate_limit),
            ..Self::default()
        }
    }

    /// 同 [`Self::with_cache`]，但把**每日扣费上限**调小。
    fn with_cache_and_daily_spend_limit(cache: CacheFixture, limit_microusd: u64) -> Self {
        Self {
            cache: Some(cache),
            daily_spend_limit_microusd: Some(limit_microusd),
            ..Self::default()
        }
    }
}

/// 同 [`start_api`]，但可以给这个进程配上**加速层**（缓存）。
///
/// 不配就是今天的路径：加速层根本不构造，受理不额外读库、不预检、不写缓存。
///
/// 端口是**先占后放**的：`bind(:0)` 读到端口就释放，子进程要到连库与迁移之后才真正 bind。
/// 中间那段空窗里端口可能被别人拿走，那时子进程 bind 失败会静默退出。所以这里换端口重试。
async fn start_api_with(
    database_url: &str,
    sync_wait_seconds: u64,
    max_concurrent_jobs: u64,
    settings: &ApiProcessSettings,
) -> (String, String, ApiProcess) {
    const ATTEMPTS: usize = 5;

    let client = Client::new();
    let admin_token = format!("contract-admin-{}", Uuid::new_v4());
    let mut last_failure = String::new();
    for attempt in 1..=ATTEMPTS {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test port should bind");
        let port = listener.local_addr().expect("test address").port();
        drop(listener);
        let base_url = format!("http://127.0.0.1:{port}");
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
        apply_cache_env(&mut command, settings.cache.as_ref());
        if let Some(rate_limit) = settings.rate_limit {
            command
                .env(
                    "GENERATION_RATE_LIMIT_REQUESTS_PER_WINDOW",
                    rate_limit.requests_per_window.to_string(),
                )
                .env(
                    "GENERATION_RATE_LIMIT_WINDOW_MS",
                    rate_limit.window_ms.to_string(),
                );
        }
        if let Some(limit_microusd) = settings.daily_spend_limit_microusd {
            command.env(
                "GENERATION_MAX_DAILY_SPEND_MICROUSD",
                limit_microusd.to_string(),
            );
        }
        let child = command.spawn().expect("API process should start");
        // 拿住这个进程：重试时要先杀掉它，端口才真的回到空闲池。
        let mut process = ApiProcess { child };
        match await_api_ready(&client, &base_url, &admin_token).await {
            Ok(()) => {
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
                return (base_url, admin_token, process);
            }
            Err(reason) => {
                last_failure = match process.exit_status() {
                    Some(status) => {
                        format!("attempt {attempt}: {reason}; the API process exited ({status})")
                    }
                    None => format!("attempt {attempt}: {reason}"),
                };
            }
        }
    }
    panic!("API did not become ready after {ATTEMPTS} attempts; last: {last_failure}");
}

/// 起一个真实 Worker 进程（丢弃返回值即结束它）。
///
/// 环境变量只有一份，两个调用点（[`Harness`] 与直接起进程的用例）共用：两家渠道的凭证都写在
/// 测试进程的环境里，取值只在进程内假上游上用过，不写入配置、日志或响应。
fn spawn_worker_process(database_url: &str) -> WorkerProcess {
    spawn_worker_process_with(database_url, None, None)
}

/// 一次用例给 Worker 配的**平台故障告警出口**。
///
/// 字段直接就是那两个环境变量的值；`PROVIDER_ALERT_WEBHOOK` 没配（`None`）就是今天的路径——
/// 一条都不外发。超时与重试用生产缺省：它们是配置项，用例不该为了跑得快把它们改成另一套语义。
struct WorkerAlerts {
    webhook: String,
    /// 某候选连续失败几次才外发。
    consecutive_failures: u64,
}

/// 同 [`spawn_worker_process`]，但可以给 Worker 也配上加速层：结算与失败收尾都改余额，
/// 提交后要把新余额写穿缓存，所以两个进程必须看同一个缓存服务。
fn spawn_worker_process_with(
    database_url: &str,
    cache: Option<&CacheFixture>,
    alerts: Option<&WorkerAlerts>,
) -> WorkerProcess {
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
    if let Some(alerts) = alerts {
        command.env("PROVIDER_ALERT_WEBHOOK", &alerts.webhook).env(
            "PROVIDER_ALERT_CONSECUTIVE_FAILURES",
            alerts.consecutive_failures.to_string(),
        );
    }
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
    /// 这个用例那个夹具账户的标识。用例要往账本里造"今天已经花掉"的事实时按它落地——
    /// 配额判的是**账户**维度的钱，不是某一把密钥的。
    account_id: String,
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
            "../../../../config/bootstrap/gpt-image-2.5-flare.json"
        ))
        .expect("bootstrap material parses");
        let index = match behaviour.provider {
            ProviderShape::Aihubmix => 0,
            ProviderShape::Apimart => 1,
        };
        let mut draft = material["offerings"][index].clone();
        // 上游地址换成这个用例的假上游；凭证仍从环境变量读，值只写在测试进程环境里。
        draft["base_url"] = Value::String("http://127.0.0.1:1".to_owned());
        // 素材的**修订级**加价系数要一起发：按张 / 按次 / 上游给金额的候选的对客价全靠它算出来，
        // 缺了发布期就拒（夹具那条默认候选是按 token 计量量的，不带它照发）。
        let markup_bps = material["markup_bps"]
            .as_i64()
            .and_then(|value| i32::try_from(value).ok());
        Self::build(
            draft,
            Some(material["capability_schema"].clone()),
            behaviour,
            max_concurrent_jobs,
            30,
            ApiProcessSettings::default(),
            markup_bps,
        )
        .await
    }

    async fn start_with_draft(
        draft: Value,
        contract: Option<Value>,
        behaviour: UpstreamBehaviour,
        max_concurrent_jobs: u64,
    ) -> Self {
        Self::build(
            draft,
            contract,
            behaviour,
            max_concurrent_jobs,
            30,
            ApiProcessSettings::default(),
            None,
        )
        .await
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
            ApiProcessSettings::with_cache(cache),
            None,
        )
        .await
    }

    /// 同 [`Self::start_with_cache`]，但把**每把密钥的速率上限**也调小：用例因此不必发满默认的
    /// 每分钟 60 次，第二次请求就能看到越限那条路。
    async fn start_with_cache_and_rate_limit(
        draft: Value,
        contract: Option<Value>,
        behaviour: UpstreamBehaviour,
        max_concurrent_jobs: u64,
        sync_wait_seconds: u64,
        cache: CacheFixture,
        rate_limit: ApiRateLimit,
    ) -> Self {
        Self::build(
            draft,
            contract,
            behaviour,
            max_concurrent_jobs,
            sync_wait_seconds,
            ApiProcessSettings::with_cache_and_rate_limit(cache, rate_limit),
            None,
        )
        .await
    }

    /// 同 [`Self::start_with_cache`]，但把**每账户每日扣费上限**调小。
    ///
    /// 上限是**进程启动时**读的环境变量，所以要在起进程之前就定下来：想按"这笔实际花了多少"
    /// 来定额度，就得先让那笔跑完（见用例里那次充值——余额与额度是两回事，用例把前者抬开，
    /// 好让被拒的唯一理由就是"今天到头了"）。
    async fn start_with_cache_and_daily_spend_limit(
        draft: Value,
        contract: Option<Value>,
        behaviour: UpstreamBehaviour,
        max_concurrent_jobs: u64,
        sync_wait_seconds: u64,
        cache: CacheFixture,
        limit_microusd: u64,
    ) -> Self {
        Self::build(
            draft,
            contract,
            behaviour,
            max_concurrent_jobs,
            sync_wait_seconds,
            ApiProcessSettings::with_cache_and_daily_spend_limit(cache, limit_microusd),
            None,
        )
        .await
    }

    async fn build(
        mut draft: Value,
        contract: Option<Value>,
        behaviour: UpstreamBehaviour,
        max_concurrent_jobs: u64,
        sync_wait_seconds: u64,
        settings: ApiProcessSettings,
        markup_bps: Option<i32>,
    ) -> Self {
        let (database_url, database_name) = isolated_database_url().await;
        let calls: UpstreamCalls = Arc::new(Mutex::new(Vec::new()));
        let upstream = start_fake_upstream_with(calls.clone(), behaviour).await;
        let (base_url, admin_token, process) = start_api_with(
            &database_url,
            sync_wait_seconds,
            max_concurrent_jobs,
            &settings,
        )
        .await;
        let client = Client::new();
        wait_until_ready(&client, &base_url, &admin_token).await;
        // 夹具账户要**付得起这次发布带的那份保底额**：素材带定价之后，受理闸门（余额 ≥ 保底额）
        // 会拿它去比，余额不够时连"参数面过滤""结果原形"这类与钱无关的用例也会 402。
        // "钱不够就拒"那条路不靠这个数——需要低余额账户的用例自己建一个。
        let account = create_account_with_credit(&client, &base_url, &admin_token, 1_000_000).await;
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
        let published = publish_candidates_with_markup(
            &client,
            &base_url,
            &admin_token,
            Self::MODEL,
            contract,
            vec![draft],
            markup_bps,
        )
        .await;
        assert_eq!(published, StatusCode::OK, "publication must succeed");

        Self {
            model: Self::MODEL,
            database_url,
            database_name,
            base_url,
            admin_token,
            account_id: account,
            api_key,
            pool,
            calls,
            upstream_base_url: upstream.base_url.clone(),
            declared_surface,
            _api: process,
            _upstream: upstream,
            cache: settings.cache,
        }
    }

    /// 起一个真实 Worker 进程（丢弃返回值即结束它）。
    ///
    /// Worker 与 API 共用同一个缓存服务：结算改余额之后要把新余额写穿，否则缓存会留着一个
    /// 刚写过、但偏高的余额。
    fn spawn_worker(&self) -> WorkerProcess {
        spawn_worker_process_with(&self.database_url, self.cache.as_ref(), None)
    }

    /// 同 [`Self::spawn_worker`]，但给这个 Worker 配上告警出口。
    fn spawn_worker_with_alerts(&self, alerts: &WorkerAlerts) -> WorkerProcess {
        spawn_worker_process_with(&self.database_url, self.cache.as_ref(), Some(alerts))
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
                "formula": "token_rates",
                "price_plan": {
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
        // 这份测试构造体按**四分项 token 计量量**计价（用例要的是可复现的费率），所以它带一份
        // 价目表；真实素材里各渠道按自己的计价形态登记，见 `config/bootstrap/`。
        "formula": "token_rates",
        "price_plan": {
            "currency": "USD",
            "text_input_microusd_per_million": 5_000_000,
            "image_input_microusd_per_million": 8_000_000,
            "text_output_microusd_per_million": 10_000_000,
            "image_output_microusd_per_million": 30_000_000,
            "source_url": "https://example.invalid/price"
        }
    })
}

/// 一次发布里的两条候选必须来自**两个渠道**：供给身份是"模型 + 渠道"唯一，同一渠道发两条会
/// 塌成一条（第二条要么被复用、要么被发布期拒）。要两条候选就换一个入口（地址或凭证身份不同）。
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

/// 一个**按给定余额建出来的**账户与 Key：用例要另一个账户，或要一个指定余额的账户时用它。
///
/// 夹具自己那个账户的余额够跑通带定价的发布（见 [`Harness::build`]）；"余额不够就拒"那条路
/// 一律走这里，把余额显式写成不够的数——它不该依赖夹具账户恰好很穷。
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

/// 单次探活的超时。
///
/// 占住端口的也可能是**只完成握手、从不作答**的监听者：没有超时，一次探活会永远等下去，
/// 而 `Client` 默认不设超时。
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// 等 API 就绪的整体上限。
const READY_TIMEOUT: Duration = Duration::from_secs(10);

/// 探一次：`base_url` 上坐着的到底是不是**我们起的那个** API 进程。
///
/// 三层判据缺一不可：`/health` 回 2xx 只说明有个 HTTP 服务在；响应体是那一份 `/health` 才说明
/// 它是个平台 API（假上游对任何 GET 都回 200 + PNG）；**这份管理员令牌在它上面有效**才说明它是
/// 我们自己起的那一个（另一条用例的 API 进程回的也是同一份 `/health`，但它不认我们的令牌）。
///
/// 失败时返回**实际**看到的原因，供调用方原样报出来。
async fn probe_api(client: &Client, base_url: &str, admin_token: &str) -> Result<(), &'static str> {
    let response = client
        .get(format!("{base_url}/health"))
        .timeout(PROBE_TIMEOUT)
        .send()
        .await
        .map_err(|_| "no answer on /health")?;
    if !response.status().is_success() {
        return Err("/health did not answer with a success status");
    }
    let healthy = response
        .json::<Value>()
        .await
        .ok()
        .is_some_and(|body| body.get("status").and_then(Value::as_str) == Some("ok"));
    if !healthy {
        return Err("/health did not answer with the platform payload");
    }
    let ours = client
        .get(format!("{base_url}/api/v1/route-policies"))
        .bearer_auth(admin_token)
        .timeout(PROBE_TIMEOUT)
        .send()
        .await
        .is_ok_and(|response| response.status().is_success());
    if !ours {
        return Err("the responder does not accept this test's admin token");
    }
    Ok(())
}

/// 轮询到 API 就绪；到点返回最后一次探测**实际**看到的原因。
async fn await_api_ready(
    client: &Client,
    base_url: &str,
    admin_token: &str,
) -> Result<(), &'static str> {
    let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
    loop {
        let last = match probe_api(client, base_url, admin_token).await {
            Ok(()) => return Ok(()),
            Err(reason) => reason,
        };
        if tokio::time::Instant::now() >= deadline {
            return Err(last);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// 等 API 就绪；等不到就 panic，panic 里带最后一次探测看到的原因。给**自己起进程**的调用点用。
async fn wait_until_ready(client: &Client, base_url: &str, admin_token: &str) {
    if let Err(reason) = await_api_ready(client, base_url, admin_token).await {
        panic!("API at {base_url} did not become ready: {reason}");
    }
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

/// 发一把密钥，把**响应原样**交回去：明文与密钥标识（`api_key` / `key_id`）都在里面。
///
/// 要标识的用例（吊销那条路）用它；只要明文的用例走 [`issue_key`]，不必各自解一遍 JSON。
async fn issue_key_response(
    client: &Client,
    base_url: &str,
    admin_token: &str,
    account_id: &str,
) -> Value {
    let response = client
        .post(format!("{base_url}/api/v1/accounts/{account_id}/api-keys"))
        .bearer_auth(admin_token)
        .json(&json!({"label": "contract"}))
        .send()
        .await
        .expect("key creation");
    assert_eq!(response.status(), StatusCode::OK, "key issuance");
    response.json::<Value>().await.expect("key JSON")
}

async fn issue_key(client: &Client, base_url: &str, admin_token: &str, account_id: &str) -> String {
    issue_key_response(client, base_url, admin_token, account_id)
        .await
        .get("api_key")
        .and_then(Value::as_str)
        .expect("API key")
        .to_owned()
}

/// 发布一份 2.5 素材：对客面的用例都按它的合同提交（模型名见 `generation_request_body`）。
async fn publish_bootstrap(client: &Client, base_url: &str, admin_token: &str) {
    let config: Value = serde_json::from_str(include_str!(
        "../../../../config/bootstrap/gpt-image-2.5-flare.json"
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
        "../../../../config/bootstrap/gpt-image-2.5-flare.json"
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

/// 改一次**供给 / 渠道**的启用开关（`PATCH /api/v1/offerings/{id}`、`/api/v1/channels/{id}`）。
///
/// 请求体原样发出、响应原样取回：错误形状那几条用例要自己构造"多给一个字段""少给 `enabled`"
/// 这类不合形状的请求体，夹具先过一遍结构体就把它们磨平了。`admin_token` 给 `None` 就一个
/// 鉴权头都不带——这条接口是运营面，用例要能看出它要凭证。
async fn patch_supply(
    client: &Client,
    base_url: &str,
    path: &str,
    admin_token: Option<&str>,
    body: &Value,
) -> (StatusCode, Value) {
    let request = client.patch(format!("{base_url}{path}")).json(body);
    let request = match admin_token {
        Some(token) => request.bearer_auth(token),
        None => request,
    };
    let response = request.send().await.expect("supply patch");
    let status = response.status();
    let raw = response.text().await.expect("supply patch body");
    (
        status,
        serde_json::from_str(&raw).unwrap_or(Value::String(raw)),
    )
}

/// 启停一条供给（成功路径）。
async fn patch_offering(
    client: &Client,
    base_url: &str,
    admin_token: &str,
    offering_id: Uuid,
    enabled: bool,
) -> StatusCode {
    patch_supply(
        client,
        base_url,
        &format!("/api/v1/offerings/{offering_id}"),
        Some(admin_token),
        &json!({"enabled": enabled}),
    )
    .await
    .0
}

/// 启停一条渠道（成功路径）。
async fn patch_channel(
    client: &Client,
    base_url: &str,
    admin_token: &str,
    channel_id: Uuid,
    enabled: bool,
) -> StatusCode {
    patch_supply(
        client,
        base_url,
        &format!("/api/v1/channels/{channel_id}"),
        Some(admin_token),
        &json!({"enabled": enabled}),
    )
    .await
    .0
}

/// 当前生效的那条候选供给与它所在的渠道：启停用例要按 id 指认它们。
async fn active_supply(harness: &Harness) -> (Uuid, Uuid) {
    let row = sqlx::query(
        "SELECT o.id AS offering_id, o.channel_id FROM supply.offerings o
         JOIN publication.runtime_entries re ON re.offering_id = o.id
         WHERE re.active AND re.gateway_model = $1",
    )
    .bind(harness.model)
    .fetch_one(&harness.pool)
    .await
    .expect("the active candidate supply");
    (
        row.try_get("offering_id").expect("offering id"),
        row.try_get("channel_id").expect("channel id"),
    )
}

/// 一份**对客名与厂商原生名不同**的素材：厂商原生名取 sunburst，对客名取 plus。
/// 只留 AIHubMix 那一条候选：这条用例只跑一家渠道，另一条留在这里会多一个用不到的假上游。
fn renamed_material(aihubmix_upstream: &str) -> Value {
    let mut material: Value = serde_json::from_str(include_str!(
        "../../../../config/bootstrap/gpt-image-2.5-sunburst.json"
    ))
    .expect("2.5 material parses");
    material["gateway_model"] = Value::String("gpt-image-2.5-plus".to_owned());
    let aihubmix = material["offerings"][0].clone();
    material["offerings"] = json!([aihubmix]);
    material["offerings"][0]["base_url"] = Value::String(aihubmix_upstream.to_owned());
    material
}

// ───────────────────────── 定价、保底与结算 ─────────────────────────

/// 把一条候选**重新发布成带定价的**：上游地址取夹具里那个假上游，加价系数由用例给。
///
/// 按张 / 按次 / 上游给金额的候选没有对客价载体，它们的对客价由成本单价乘倍率算出来——所以
/// 这些用例必须自己给倍率（夹具那条默认候选是按 token 计量量的，不带倍率也发得出去）。
async fn republish_candidate(
    harness: &Harness,
    client: &Client,
    mut draft: Value,
    markup_bps: i32,
) -> StatusCode {
    draft["base_url"] = Value::String(harness.upstream_base_url.clone());
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

/// 一条**按张 / 按次**计价的候选草案：形态、单价、成本币种与那组定价参考都带上。
///
/// 承载面沿用夹具那条默认候选（同一份合同、同一份承载面），所以重新发布它不会撞上"合同不可变"。
fn unit_candidate(formula: &str, unit_price_microusd: u64, currency: &str) -> Value {
    let mut draft = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    draft["formula"] = Value::String(formula.to_owned());
    draft["price_plan"] = Value::Null;
    draft["cost_unit_price_microusd"] = json!(unit_price_microusd);
    draft["cost_currency"] = json!(currency);
    draft["reference_cost_microusd"] = json!(unit_price_microusd);
    draft["cost_basis"] = json!("computed");
    draft["tier_prices"] = json!({});
    draft["floor_amounts"] = openai_floor_amounts();
    draft
}

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

    /// 直接删一条（绕过服务）：用例用它构造"这条值不在了"（例如到了别的限流窗口）。
    fn delete(&self, key: &str) {
        self.state.lock().expect("cache state lock").remove(key);
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
