//! 端到端合同测试的共享装置：进程内假上游与假 Redis、真实 API 进程的启停、独立空库、
//! `Harness` 驱动、管理员与对客 HTTP 辅助，以及跨用例共用的断言。
//!
//! 图片入口只有**直接执行**这一条：API 进程自己连假上游，没有 Worker、没有作业队列。
//!
//! 各 `cases_*.rs` 用 `#[path]` 声明为本模块的子模块：装置条目因此不必对外可见——这是这批文件
//! 从单文件搬运过来能不动一行可见性的前提。代价是模块拓扑不能只看目录名，新增一个用例文件
//! 要在这里登记。

use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::{Client, StatusCode};
use seeai_application::{
    DEFAULT_SETTLE_RESERVE_SECONDS, RequestFingerprintInput, RequestFingerprintKeys,
    idempotency_key_digest,
};
use seeai_domain::replace_contract_model_identity;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use std::{
    collections::BTreeMap,
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

#[path = "cases_account_names.rs"]
mod cases_account_names;
#[path = "cases_admin_surface.rs"]
mod cases_admin_surface;
#[path = "cases_aihubmix.rs"]
mod cases_aihubmix;
#[path = "cases_apimart.rs"]
mod cases_apimart;
#[path = "cases_auth_attempts.rs"]
mod cases_auth_attempts;
#[path = "cases_billing.rs"]
mod cases_billing;
#[path = "cases_cache.rs"]
mod cases_cache;
#[path = "cases_capacity_measurement.rs"]
mod cases_capacity_measurement;
#[path = "cases_cost_facts.rs"]
mod cases_cost_facts;
#[path = "cases_customer_history.rs"]
mod cases_customer_history;
#[path = "cases_direct_execution.rs"]
mod cases_direct_execution;
#[path = "cases_funds.rs"]
mod cases_funds;
#[path = "cases_identity.rs"]
mod cases_identity;
#[path = "cases_identity_email.rs"]
mod cases_identity_email;
#[path = "cases_kill_matrix.rs"]
mod cases_kill_matrix;
#[path = "cases_lifecycle.rs"]
mod cases_lifecycle;
#[path = "cases_migrations.rs"]
mod cases_migrations;
#[path = "cases_model_concurrency.rs"]
mod cases_model_concurrency;
#[path = "cases_model_document.rs"]
mod cases_model_document;
#[path = "cases_model_type.rs"]
mod cases_model_type;
#[path = "cases_parameter_mapping.rs"]
mod cases_parameter_mapping;
#[path = "cases_parameters.rs"]
mod cases_parameters;
#[path = "cases_performance_baseline.rs"]
mod cases_performance_baseline;
#[path = "cases_pricing.rs"]
mod cases_pricing;
#[path = "cases_public_surface.rs"]
mod cases_public_surface;
#[path = "cases_publication.rs"]
mod cases_publication;
#[path = "cases_retry.rs"]
mod cases_retry;
#[path = "cases_routing.rs"]
mod cases_routing;
#[path = "cases_upload.rs"]
mod cases_upload;

// 夹具自身的检查：不启平台进程、不用数据库，因此不进 `#[ignore]`，由 workspace 单测那一步跑。
#[path = "harness_check.rs"]
mod harness_check;

/// 客户历史游标密钥（32 字节的 base64）：**必须配**，缺了 API 进程起不来，所以夹具给一份固定的。
const CONTRACT_CURSOR_KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

/// 直接执行要的请求指纹密钥（32 字节 base64）。
///
/// 夹具里只是让进程起得来、让摘要可复现；真实部署的密钥来自环境变量，不进仓库。
const CONTRACT_FINGERPRINT_KEY: &str = "Hx4dHBsaGRgXFhUUExIREA8ODQwLCgkIBwYFBAMCAQA=";

/// 夹具用的请求指纹密钥（与子进程环境里那份同值）：重放用例要按同一版本重算摘要。
fn contract_fingerprint_keys() -> RequestFingerprintKeys {
    let key = STANDARD
        .decode(CONTRACT_FINGERPRINT_KEY)
        .expect("the fixture fingerprint key decodes");
    RequestFingerprintKeys::new(BTreeMap::from([(1_i16, key)]), 1)
        .expect("the fixture fingerprint keys are valid")
}
/// 直接执行时渠道凭证的假值：只在本机假上游上用过，不写配置、日志或响应。
const CONTRACT_PROVIDER_KEY: &str = "contract-test-key";
/// 假对象存储的桶名与访问密钥：只在测试进程与子进程环境里用，不写进配置或响应。
const UPLOAD_BUCKET: &str = "contract-upload-bucket";
const UPLOAD_ACCESS_KEY_ID: &str = "contract-upload-id";
const UPLOAD_ACCESS_KEY_SECRET: &str = "contract-upload-secret";

/// 一个最小合法 PNG（1×1），用作假上游返回的结果图，也用作调用方传的参考图。
/// 一段以 JPEG 魔数开头的字节：判型只看魔数，不需要真的能解码。
const JPEG_FIXTURE: &[u8] = &[
    0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00, 0x01,
];

const PNG_FIXTURE: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0xDA, 0x63, 0x64, 0xF8, 0xCF, 0xF0,
    0x1F, 0x00, 0x05, 0xFE, 0x02, 0xFE, 0x5D, 0xC6, 0x38, 0x59, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
];

/// 调用方以内联 data URL 给图片的形态：生成入口只收公网 URL，这个取值在受理前一律被拒
/// （400 public_image_url_required）。夹具只在拒绝用例里用它构造输入。
fn inline_png_data_url() -> String {
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

/// 假上游要在哪一类请求上停住，等用例放行。
///
/// 两类正是强杀矩阵要用屏障钉住的注入点：生成提交（提交中／接受后句柄未写入）与任务查询
/// （轮询中，终态还没回）。生成入口收敛后不再有"内联图片先上传换 URL"这一格。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeldRequest {
    Create,
    Query,
}

impl HeldRequest {
    fn matches(self, method: &str, path: &str) -> bool {
        match self {
            Self::Create => {
                method == "POST"
                    && (path.ends_with("/images/generations") || path.ends_with("/images/edits"))
            }
            Self::Query => method == "GET" && path.starts_with("/v1/tasks/"),
        }
    }
}

/// 假上游上的**闸门**：命中目标请求时记一次到达再停住，直到用例放行。
///
/// 强杀矩阵用它把 API 稳定地钉在"请求已发出、响应还没回"那一格：用例等的是假上游**真的收到了**
/// 这条请求（到达计数），不是 sleep 猜时间；放行之前 API 一直停着，所以杀进程时状态是确定的。
#[derive(Default)]
struct UpstreamGate {
    arrivals: AtomicUsize,
    /// 允许继续的到达序号上限：`release_all` 把它置为 `usize::MAX`。
    released_upto: AtomicUsize,
    /// 已经走出闸门（`arrive_and_hold` 返回）的到达数。
    resumed: AtomicUsize,
    arrival_notify: tokio::sync::Notify,
    resume_notify: tokio::sync::Notify,
}

impl UpstreamGate {
    /// 用例侧：等第 `n` 条目标请求到达假上游。
    async fn wait_for_arrival(&self, n: usize) {
        loop {
            if self.arrivals.load(Ordering::SeqCst) >= n {
                return;
            }
            let notified = self.arrival_notify.notified();
            if self.arrivals.load(Ordering::SeqCst) >= n {
                return;
            }
            notified.await;
        }
    }

    /// 到达过的目标请求条数。
    fn arrivals(&self) -> usize {
        self.arrivals.load(Ordering::SeqCst)
    }

    /// 放行全部已到达与之后才到达的目标请求。
    fn release_all(&self) {
        self.released_upto.store(usize::MAX, Ordering::SeqCst);
        self.resume_notify.notify_waiters();
    }

    /// 用例侧：等第 `n` 条被停住的请求真的走出闸门（紧接着就会写响应）。
    ///
    /// 恢复对账复用同一个假上游时，它要靠这个信号确认"被杀进程手里那次挂起的请求已经放完"，
    /// 否则上游的查询计数可能被两条请求交错推进。
    async fn wait_for_resume(&self, n: usize) {
        loop {
            if self.resumed.load(Ordering::SeqCst) >= n {
                return;
            }
            let notified = self.resume_notify.notified();
            if self.resumed.load(Ordering::SeqCst) >= n {
                return;
            }
            notified.await;
        }
    }

    /// 请求侧：记下这次到达的序号，到放行为止一直等。
    async fn arrive_and_hold(&self, ordinal: usize) {
        loop {
            if self.released_upto.load(Ordering::SeqCst) >= ordinal {
                break;
            }
            let notified = self.resume_notify.notified();
            if self.released_upto.load(Ordering::SeqCst) >= ordinal {
                break;
            }
            notified.await;
        }
        self.resumed.fetch_add(1, Ordering::SeqCst);
        self.resume_notify.notify_waiters();
    }
}

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

/// 假上游的可配置行为：覆盖重试、未知状态、在飞、参考图取用与提交被拒。
#[derive(Clone)]
struct UpstreamBehaviour {
    provider: ProviderShape,
    /// 任务查询先失败这么多次（返回 500），之后才给正常响应 —— 覆盖"查询可重试"。
    query_failures: usize,
    /// 任务查询先返回这么多次未在文档中出现的状态，之后才 completed。
    unknown_status_times: usize,
    /// 任务查询先返回这么多次"仍在跑"，用来把一次请求留在在飞状态。
    pending_times: usize,
    /// 参考图 GET 先失败这么多次（返回 500），之后回图片。
    ///
    /// AIHubMix 的参考图由 Adapter 自己按公网 URL 取，取不到是**可证明未受理**；"失败一次就恢复"
    /// 正好是重投要覆盖的形态。
    reference_get_failures: usize,
    /// 参考图 GET 回的字节数（0 表示用夹具那张小图）。
    ///
    /// 生成入口收敛为只收公网 URL 之后，参考图字节不再随请求体进来；内存观测用例靠这条下载路
    /// 把大图送进进程，压的仍是"取图 + 编码进 multipart"那一段。
    reference_image_bytes: usize,
    /// 生成请求**前几次**直接以这个状态码被拒（之后正常应答）。
    ///
    /// 它用来观察重投：上游明确拒绝受理这类失败是"可证明未受理"，重投不会付两次上游成本。
    /// `0` 就是今天的路径——第一次就正常应答。
    create_rejection_status: u16,
    /// 上面那种拒绝**前几次**——0 表示一直拒（用来观察"到上限仍失败"）。
    create_rejection_times: usize,
    submit: SubmitBehaviour,
    sync_image: SyncImageShape,
    /// base64 结果图的**载荷字节数**（0 表示用夹具那张小图）。
    ///
    /// 内存观测用例用它把上游响应顶到 Driver 声明的上限，验证单次预留真的罩得住最大响应。
    sync_image_b64_bytes: usize,
    /// 任务终态里声明的成本：`None` 表示响应里**根本没有这个字段**（渠道没给），
    /// 负数与非数字则覆盖"声明了却拿不到"的形态。取值是实测样例。
    declared_cost: Option<Value>,
    /// 终态里**没有结果图**（`result.images` 是空数组），但金额照给。
    ///
    /// 这是"上游明明给了金额、这次却没出图"的形态：用来观察那笔成本会不会丢。
    terminal_without_images: bool,
    /// 生成请求应答前的延迟（毫秒）：把一次执行留在在飞状态，观察同键重放与期限。
    delay_ms: u64,
    /// 命中这个目标请求时停住、等用例放行（强杀矩阵的屏障，见 [`UpstreamGate`]）；`None` 是不停。
    hold: Option<(HeldRequest, Arc<UpstreamGate>)>,
}

impl UpstreamBehaviour {
    fn apimart() -> Self {
        Self {
            provider: ProviderShape::Apimart,
            query_failures: 0,
            unknown_status_times: 0,
            pending_times: 0,
            reference_get_failures: 0,
            reference_image_bytes: 0,
            create_rejection_status: 0,
            create_rejection_times: 0,
            submit: SubmitBehaviour::Accepted,
            sync_image: SyncImageShape::Url,
            sync_image_b64_bytes: 0,
            declared_cost: Some(json!(0.011354)),
            terminal_without_images: false,
            delay_ms: 0,
            hold: None,
        }
    }

    /// AIHubMix 的假上游行为：这条渠道的完成态任务对象**只回内联 base64**（`content_url` 要
    /// 平台凭据、平台不取），所以 `SyncImageShape::Url` 对它没有意义——结果与结算的判据都按
    /// base64 走；`sync_image` 参数留着是为了与另一家的构造体同形。
    fn aihubmix(sync_image: SyncImageShape) -> Self {
        Self {
            provider: ProviderShape::Aihubmix,
            sync_image,
            ..Self::apimart()
        }
    }

    /// 让假上游在命中 `target` 的请求上停住，等用例放行；闸门句柄由 [`Harness::gate`] 取回。
    fn holding(mut self, target: HeldRequest) -> Self {
        self.hold = Some((target, Arc::new(UpstreamGate::default())));
        self
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
    // 查询、参考图取用与生成的行为按调用次数推进。
    let query_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let reference_get_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let create_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let calls = calls.clone();
            let behaviour = behaviour.clone();
            let query_count = query_count.clone();
            let reference_get_count = reference_get_count.clone();
            let create_count = create_count.clone();
            tokio::spawn(async move {
                let _ = serve_fake_upstream(
                    &mut socket,
                    calls,
                    behaviour,
                    query_count,
                    reference_get_count,
                    create_count,
                )
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
    reference_get_count: Arc<std::sync::atomic::AtomicUsize>,
    create_count: Arc<std::sync::atomic::AtomicUsize>,
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

    // 查询编号在**读完请求**时就定下来：闸门把响应停住也不会改变这一次查询的序号，用例因此能
    // 确定"被杀进程那一次查询"与"恢复对账那一次查询"各拿到上游的第几个状态。
    let query_ordinal = if method == "GET" && path.starts_with("/v1/tasks/") {
        Some(query_count.fetch_add(1, Ordering::SeqCst) + 1)
    } else {
        None
    };
    // 闸门：命中目标请求时先记一次到达，再停住等用例放行。用例等的正是"API 真的把这条请求发
    // 出来了"这一格，而不是靠 sleep 猜时间。
    if let Some((target, gate)) = &behaviour.hold
        && target.matches(&method, &path)
    {
        let ordinal = gate.arrivals.fetch_add(1, Ordering::SeqCst) + 1;
        gate.arrival_notify.notify_waiters();
        gate.arrive_and_hold(ordinal).await;
    }

    // 提交生成请求被上游直接拒：欠费、凭证、参数错误、限流都从这里进。
    if method == "POST"
        && (path.ends_with("/images/generations") || path.ends_with("/images/edits"))
        && let SubmitBehaviour::Rejected { status, body } = &behaviour.submit
    {
        let payload = serde_json::to_vec(body).expect("rejection body");
        return write_response(socket, *status, "Error", "application/json", &payload).await;
    }

    // 前几次生成请求直接以配置的状态码被拒，之后正常应答 —— 覆盖"重投"。
    //
    // 拒绝体按 APIMart 的形状给 `error.code`：分类要看它，不能只看 HTTP 状态码（同一批状态码在
    // 两家渠道上的含义不同）。
    if behaviour.create_rejection_status != 0
        && method == "POST"
        && (path.ends_with("/images/generations") || path.ends_with("/images/edits"))
    {
        let seen = create_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        if behaviour.create_rejection_times == 0 || seen <= behaviour.create_rejection_times {
            let payload = serde_json::to_vec(&json!({
                "error": {
                    "code": behaviour.create_rejection_status,
                    "message": "rejected before the generation started"
                }
            }))
            .expect("create rejection body");
            return write_response(
                socket,
                behaviour.create_rejection_status,
                "Error",
                "application/json",
                &payload,
            )
            .await;
        }
    }

    // 用例要观察"执行还在飞"时，先把这次生成请求压住。
    if behaviour.delay_ms > 0
        && method == "POST"
        && (path.ends_with("/images/generations") || path.ends_with("/images/edits"))
    {
        tokio::time::sleep(Duration::from_millis(behaviour.delay_ms)).await;
    }

    // 同步渠道（AIHubMix）：`/ai/v1/images/generations` 一步给出完成态任务对象——内联 base64 的
    // 结果与上游声明的金额。平台不再走 `/v1` 的 multipart 编辑端点。
    if behaviour.provider == ProviderShape::Aihubmix
        && method == "POST"
        && path.ends_with("/images/generations")
    {
        let payload = serde_json::to_vec(&aihubmix_task_payload(
            behaviour.sync_image,
            behaviour.sync_image_b64_bytes,
            behaviour.declared_cost.as_ref(),
        ))
        .expect("sync body");
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
        let attempt =
            query_ordinal.expect("a task query carries its ordinal from the request read");
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

    // 其余 GET：把参考图交出去。参考图走公网 URL 时由 Adapter 自己来取；
    // 结果 URL 则**不该**被平台来取（平台不下载结果）。
    if method == "GET" {
        let seen = reference_get_count.fetch_add(1, Ordering::SeqCst) + 1;
        if behaviour.reference_get_failures != 0 && seen <= behaviour.reference_get_failures {
            let payload = serde_json::to_vec(&json!({
                "error": {"code": 500, "message": "reference image unavailable"}
            }))
            .expect("reference failure body");
            return write_response(
                socket,
                500,
                "Internal Server Error",
                "application/json",
                &payload,
            )
            .await;
        }
        if behaviour.reference_image_bytes > 0 {
            let body = vec![0_u8; behaviour.reference_image_bytes];
            return write_response(socket, 200, "OK", "image/png", &body).await;
        }
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

/// AIHubMix（`/ai/v1`）的成功响应：完成态任务对象。`output[]` 给内联 base64，`usage.cost` 是上游
/// 声明的实际扣费——这条通路没有 token 分项（设计 0022 §4）。
///
/// `b64_bytes` 非 0 时把 base64 载荷顶到该字节数（内存观测用例用），否则用夹具那张小图。
fn aihubmix_task_payload(
    shape: SyncImageShape,
    b64_bytes: usize,
    declared_cost: Option<&Value>,
) -> Value {
    let b64 = match shape {
        SyncImageShape::Base64 if b64_bytes > 0 => "A".repeat(b64_bytes),
        _ => STANDARD.encode(PNG_FIXTURE),
    };
    let mut payload = json!({
        "id": "t_contract_task_1",
        "object": "image",
        "model": "gpt-image-2.5-flare",
        "status": "completed",
        "error": null,
        "output": [{"index": 0, "type": "file", "b64_json": b64}],
        "usage": {"cost": 0.011354},
        "created_at": 1_790_000_000u64,
        "completed_at": 1_790_000_060u64,
        "expires_at": 1_790_007_200u64,
    });
    // 金额按用例配置给：`None` 就是响应里**没有它**（成本缺口）。
    match declared_cost {
        Some(cost) => payload["usage"]["cost"] = cost.clone(),
        None => payload["usage"] = json!({}),
    }
    payload
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

/// 假对象存储的行为：脚本化的失败与元数据，用来构造失败分类、重试与核验不一致的形态。
#[derive(Clone, Default)]
struct ObjectStorageBehaviour {
    /// 依次给前几次 PUT 的状态码；用完之后一律成功。`0` 表示成功。
    put_statuses: Vec<u16>,
    /// 依次给前几次 HEAD 的状态码；用完之后一律成功。`0` 表示成功。
    head_statuses: Vec<u16>,
    /// HEAD 报告的字节长度相对写入值的偏移：非零即"写入后元数据与提交不一致"。
    head_byte_length_delta: i64,
    /// HEAD 报告的内容类型覆盖；`None` 表示按对象键扩展名推导。
    head_content_type: Option<String>,
    /// 让假对象存储把 PUT 停住等用例放行：用来把上传名额占住，观察 429 upload_busy。
    hold_put: Option<Arc<UpstreamGate>>,
}

/// 一次假对象存储上的调用。
struct ObjectStoreCall {
    method: String,
    path: String,
    byte_length: usize,
}

/// 一个存下来的对象。
struct StoredObject {
    content_type: String,
    bytes: Vec<u8>,
}

/// 进程内假对象存储：接收平台的 PUT 与 HEAD，并对外提供匿名 GET（用例读公网 URL）。
struct FakeObjectStorage {
    endpoint: String,
    calls: Arc<Mutex<Vec<ObjectStoreCall>>>,
    _task: tokio::task::JoinHandle<()>,
}

impl FakeObjectStorage {
    fn calls(&self) -> Vec<(String, String, usize)> {
        self.calls
            .lock()
            .expect("object store calls lock")
            .iter()
            .map(|call| (call.method.clone(), call.path.clone(), call.byte_length))
            .collect()
    }
}

async fn start_fake_object_storage(behaviour: ObjectStorageBehaviour) -> FakeObjectStorage {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fake object storage binds");
    let port = listener.local_addr().expect("addr").port();
    let objects: Arc<Mutex<std::collections::HashMap<String, StoredObject>>> =
        Arc::new(Mutex::new(std::collections::HashMap::new()));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let behaviour = Arc::new(behaviour);
    let put_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let head_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let task = {
        let objects = objects.clone();
        let calls = calls.clone();
        let behaviour = behaviour.clone();
        let put_count = put_count.clone();
        let head_count = head_count.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let objects = objects.clone();
                let calls = calls.clone();
                let behaviour = behaviour.clone();
                let put_count = put_count.clone();
                let head_count = head_count.clone();
                tokio::spawn(async move {
                    let _ = serve_fake_object_storage(
                        &mut socket,
                        objects,
                        calls,
                        behaviour,
                        put_count,
                        head_count,
                    )
                    .await;
                });
            }
        })
    };
    FakeObjectStorage {
        endpoint: format!("http://127.0.0.1:{port}"),
        calls,
        _task: task,
    }
}

async fn serve_fake_object_storage(
    socket: &mut tokio::net::TcpStream,
    objects: Arc<Mutex<std::collections::HashMap<String, StoredObject>>>,
    calls: Arc<Mutex<Vec<ObjectStoreCall>>>,
    behaviour: Arc<ObjectStorageBehaviour>,
    put_count: Arc<std::sync::atomic::AtomicUsize>,
    head_count: Arc<std::sync::atomic::AtomicUsize>,
) -> std::io::Result<()> {
    use std::sync::atomic::Ordering;
    let (method, path, body) = read_request(socket).await?;
    if let Ok(mut calls) = calls.lock() {
        calls.push(ObjectStoreCall {
            method: method.clone(),
            path: path.clone(),
            byte_length: body.len(),
        });
    }
    match method.as_str() {
        "PUT" => {
            if let Some(gate) = behaviour.hold_put.as_ref() {
                let ordinal = gate.arrivals.fetch_add(1, Ordering::SeqCst) + 1;
                gate.arrival_notify.notify_waiters();
                gate.arrive_and_hold(ordinal).await;
            }
            let index = put_count.fetch_add(1, Ordering::SeqCst);
            let status = behaviour.put_statuses.get(index).copied().unwrap_or(0);
            if status != 0 {
                return write_response(socket, status, "Error", "application/xml", b"<Error/>")
                    .await;
            }
            let content_type = content_type_for_path(&path);
            objects.lock().expect("object store objects lock").insert(
                path,
                StoredObject {
                    content_type,
                    bytes: body,
                },
            );
            write_response(socket, 200, "OK", "application/xml", b"").await
        }
        "HEAD" => {
            let index = head_count.fetch_add(1, Ordering::SeqCst);
            let status = behaviour.head_statuses.get(index).copied().unwrap_or(0);
            if status != 0 {
                return write_response(socket, status, "Error", "application/xml", b"").await;
            }
            let stored = objects
                .lock()
                .expect("object store objects lock")
                .get(&path)
                .map(|object| (object.bytes.len(), object.content_type.clone()));
            match stored {
                Some((length, content_type)) => {
                    let reported = if behaviour.head_byte_length_delta == 0 {
                        length
                    } else {
                        usize::try_from(
                            i64::try_from(length).unwrap_or(i64::MAX)
                                + behaviour.head_byte_length_delta,
                        )
                        .unwrap_or(0)
                    };
                    let content_type = behaviour.head_content_type.clone().unwrap_or(content_type);
                    write_head_response(socket, 200, reported, &content_type).await
                }
                None => write_response(socket, 404, "Not Found", "application/xml", b"").await,
            }
        }
        "GET" => {
            let stored = objects
                .lock()
                .expect("object store objects lock")
                .get(&path)
                .map(|object| (object.bytes.clone(), object.content_type.clone()));
            match stored {
                Some((bytes, content_type)) => {
                    write_response(socket, 200, "OK", &content_type, &bytes).await
                }
                None => write_response(socket, 404, "Not Found", "text/plain", b"").await,
            }
        }
        _ => write_response(socket, 405, "Method Not Allowed", "text/plain", b"").await,
    }
}

/// HEAD 响应：有 content-length 与 content-type，但没有正文。
async fn write_head_response(
    socket: &mut tokio::net::TcpStream,
    status: u16,
    content_length: usize,
    content_type: &str,
) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let head = format!(
        "HTTP/1.1 {status} OK\r\ncontent-type: {content_type}\r\ncontent-length: {content_length}\r\nconnection: close\r\n\r\n"
    );
    socket.write_all(head.as_bytes()).await?;
    socket.flush().await
}

/// 对象键扩展名反推内容类型：平台写入时用的就是同一个规范 MIME。
fn content_type_for_path(path: &str) -> String {
    if path.ends_with(".png") {
        "image/png".to_owned()
    } else if path.ends_with(".webp") {
        "image/webp".to_owned()
    } else {
        "image/jpeg".to_owned()
    }
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

    /// 子进程号：峰值 RSS 用例要读它的 `/proc/<pid>/status`。
    fn pid(&self) -> u32 {
        self.child.id()
    }

    /// 给这个 API 进程发 SIGKILL 并收尸。
    ///
    /// `Child::kill` 在 Unix 上就是 SIGKILL（不给进程任何收尾机会），这正是强杀矩阵要的注入方式；
    /// 这里再核对一次退出状态里记的确实是信号 9——"强杀"是这些用例的判据本身，不能只靠"进程没了"。
    fn sigkill(&mut self) {
        self.child.kill().expect("the API process must be killable");
        let status = self
            .child
            .wait()
            .expect("the killed API process must be reaped");
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            status.signal(),
            Some(9),
            "the API process must die from SIGKILL, got {status}"
        );
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
async fn start_api(database_url: &str, sync_wait_seconds: u64) -> (String, String, ApiProcess) {
    start_api_with(
        database_url,
        sync_wait_seconds,
        &ApiProcessSettings::default(),
    )
    .await
}

/// 同 [`start_api`]，但这个进程**带一个引导出来的管理员账号**。
///
/// 登录那条链必须有账号才能跑：共享令牌验不了"邮箱 + 口令 → 会话"，所以这一组用例单独起一个
/// 配了 `ADMIN_EMAIL`/`ADMIN_PASSWORD` 的进程。返回引导时用的邮箱与口令，供用例登录。
async fn start_api_with_admin(
    database_url: &str,
    email: &str,
    password: &str,
) -> (String, String, ApiProcess) {
    start_api_with(
        database_url,
        2,
        &ApiProcessSettings {
            admin_credentials: Some((email.to_owned(), password.to_owned())),
            ..ApiProcessSettings::default()
        },
    )
    .await
}

/// 起一个 API 进程，配上加速层与压小的**公开鉴权失败上限**（来源维采信 `x-real-ip`）。
///
/// 共享令牌可用，所以签发重置码那条管理端点不必再引导一个管理员账号。
async fn start_api_with_auth_attempts(
    database_url: &str,
    cache: CacheFixture,
    failures: u64,
    window_ms: u64,
) -> (String, String, ApiProcess) {
    start_api_with(
        database_url,
        2,
        &ApiProcessSettings::with_cache_and_auth_attempt_limit(cache, failures, window_ms),
    )
    .await
}

/// 装配一个只差引导变量与 stdio 的 API 探针命令：跑迁移所需的公共环境都在这里。
///
/// 在无 `.env` 的目录起进程，本地检出的 `.env` 才不会替探针补配置（"没配"与"配成空串"在引导里
/// 是两回事，探针要能造出真的没配）。
fn api_probe_command(database_url: &str, port: u16, admin_token: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_seeai-api"));
    remove_proxy_env(&mut command);
    command
        .env("DATABASE_URL", database_url)
        .env("API_BIND", format!("127.0.0.1:{port}"))
        .env("ADMIN_TOKEN", admin_token)
        .env("SEE_BASEURL", "http://api.contract.test")
        .env("CUSTOMER_HISTORY_CURSOR_KEY", CONTRACT_CURSOR_KEY)
        .env("PROVIDER_TIMEOUT_SECONDS", "30")
        .env("PROVIDER_TIMEOUT_BASE_SECONDS", "30")
        .env("PROVIDER_TIMEOUT_INCLUDED_IMAGES", "1")
        .env("PROVIDER_TIMEOUT_PER_IMAGE_SECONDS", "0")
        .env("REQUEST_FINGERPRINT_KEY_V1", CONTRACT_FINGERPRINT_KEY)
        .env("AIHUBMIX_API_KEY", CONTRACT_PROVIDER_KEY)
        .env("APIMART_API_KEY", CONTRACT_PROVIDER_KEY)
        // 公共文档按绝对路径指到仓库里的 `public-docs/`：探针的工作目录是临时目录，相对路径找不到。
        .env(
            "PUBLIC_DOCS_DIR",
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../public-docs"),
        )
        .current_dir(std::env::temp_dir());
    remove_upload_env(&mut command);
    command
}

/// 取一个一次性端口，避免探针与其它进程撞端口。
fn probe_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("test port should bind");
    let port = listener.local_addr().expect("test address").port();
    drop(listener);
    port
}

/// 同 [`probe_api_startup_with_seed`]，但把 stderr 带回来：验“启动失败要点名哪个邮箱”。
async fn probe_api_startup_with_seed_stderr(
    database_url: &str,
    email: &str,
    password: &str,
) -> (bool, String) {
    let mut command = api_probe_command(database_url, probe_port(), "seed-conflict-probe-token");
    command
        .env("ADMIN_EMAIL", email)
        .env("ADMIN_PASSWORD", password)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    probe_running_and_stderr(command).await
}

/// 只配**引导变量**起一个 API 进程，回报它"起来了"还是"退出了"（V-A8 的三分支）。
///
/// 与 [`start_api`] 的区别是它**不**等 `/health` 等到超时：引导只给一个变量时进程本来就该启动失败。
/// 返回 `Ok(())` 表示它还在跑（已收掉），`Err(状态)` 表示已经退出。日志不抓（管道不留心就会把子进程
/// 堵死），"两个都不给时日志里有没有警告"由别的用例读启动日志覆盖。
async fn probe_api_startup_with_seed(
    database_url: &str,
    email: Option<&str>,
    password: Option<&str>,
) -> Result<(), std::process::ExitStatus> {
    let mut command = api_probe_command(database_url, probe_port(), "seed-probe-token");
    command.stdout(Stdio::null()).stderr(Stdio::null());
    // 两个都没给时要**明确不设**这两个变量，而不是设成空串（空串与"没配"在引导里是两回事）。
    if let Some(email) = email {
        command.env("ADMIN_EMAIL", email);
    }
    if let Some(password) = password {
        command.env("ADMIN_PASSWORD", password);
    }
    let mut child = command.spawn().expect("API process should start");
    tokio::time::sleep(Duration::from_millis(1_800)).await;
    match child.try_wait().expect("try_wait") {
        Some(status) => Err(status),
        None => {
            let _ = child.kill();
            let _ = child.wait();
            Ok(())
        }
    }
}

/// 只配**游标密钥**起一个 API 进程，回报"还在跑吗"与它的 stderr。
///
/// 客户历史翻页的密钥是**必填**：缺失或不是 32 字节的 base64 时进程该起不来，而且报错要**点名那个
/// 配置**——否则运维只知道"起不来"，不知道去配什么。`None` 表示**显式不设**这个变量。
async fn probe_api_startup_with_cursor_key(
    database_url: &str,
    key: Option<&str>,
) -> (bool, String) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("test port should bind");
    let port = listener.local_addr().expect("test address").port();
    drop(listener);
    let mut command = Command::new(env!("CARGO_BIN_EXE_seeai-api"));
    remove_proxy_env(&mut command);
    command
        .env("DATABASE_URL", database_url)
        .env("API_BIND", format!("127.0.0.1:{port}"))
        .env("ADMIN_TOKEN", "cursor-key-probe-token")
        .env("SEE_BASEURL", "http://api.contract.test")
        .env("ADMIN_EMAIL", "cursor-key-probe@example.com")
        .env("ADMIN_PASSWORD", "a-long-enough-password")
        .env("PROVIDER_TIMEOUT_SECONDS", "30")
        .env("PROVIDER_TIMEOUT_BASE_SECONDS", "30")
        .env("PROVIDER_TIMEOUT_INCLUDED_IMAGES", "1")
        .env("PROVIDER_TIMEOUT_PER_IMAGE_SECONDS", "0")
        // 直接执行的必填项先配齐："没配"的那个才只剩这次要探的那个变量。
        .env("REQUEST_FINGERPRINT_KEY_V1", CONTRACT_FINGERPRINT_KEY)
        .env("AIHUBMIX_API_KEY", CONTRACT_PROVIDER_KEY)
        .env("APIMART_API_KEY", CONTRACT_PROVIDER_KEY)
        // 与 [`probe_api_startup_with_seed`] 同理：在无 `.env` 的目录起进程，"没配"才是真的没配。
        .current_dir(std::env::temp_dir())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(key) = key {
        command.env("CUSTOMER_HISTORY_CURSOR_KEY", key);
    }
    probe_running_and_stderr(command).await
}

/// 起一个 API 进程，只给**执行容量组合**这两个可选项，回报"还在跑吗"与它的 stderr。
///
/// 直接执行的其他必填项都配齐，因此进程要么带着这份容量组合起来，要么就是被组合校验挡下——
/// 正好用来验证"不自洽时拒绝启动并点名配置"与"刚好自洽时起得来"。
async fn probe_api_startup_with_execution_capacity(
    database_url: &str,
    execution_slots: Option<usize>,
    max_memory_bytes: Option<usize>,
) -> (bool, String) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("test port should bind");
    let port = listener.local_addr().expect("test address").port();
    drop(listener);
    let mut command = Command::new(env!("CARGO_BIN_EXE_seeai-api"));
    remove_proxy_env(&mut command);
    command
        .env("DATABASE_URL", database_url)
        .env("API_BIND", format!("127.0.0.1:{port}"))
        .env("ADMIN_TOKEN", "capacity-probe-token")
        .env("SEE_BASEURL", "http://api.contract.test")
        .env("CUSTOMER_HISTORY_CURSOR_KEY", CONTRACT_CURSOR_KEY)
        // 供给素材的导入与这条判据无关，显式关掉，探针只探容量组合。
        .env("SUPPLY_MATERIAL_DIR", "")
        .env("GENERATION_SYNC_WAIT_SECONDS", "30")
        .env("PROVIDER_TIMEOUT_SECONDS", "30")
        .env("PROVIDER_TIMEOUT_BASE_SECONDS", "30")
        .env("PROVIDER_TIMEOUT_INCLUDED_IMAGES", "1")
        .env("PROVIDER_TIMEOUT_PER_IMAGE_SECONDS", "0")
        .env("REQUEST_FINGERPRINT_KEY_V1", CONTRACT_FINGERPRINT_KEY)
        .env("AIHUBMIX_API_KEY", CONTRACT_PROVIDER_KEY)
        .env("APIMART_API_KEY", CONTRACT_PROVIDER_KEY)
        .current_dir(std::env::temp_dir())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(slots) = execution_slots {
        command.env("GENERATION_EXECUTION_SLOTS", slots.to_string());
    }
    if let Some(bytes) = max_memory_bytes {
        command.env("GENERATION_MAX_MEMORY_BYTES", bytes.to_string());
    }
    probe_running_and_stderr(command).await
}

/// 起一个 API 进程，只额外给上传存储的环境变量，回报"还在跑吗"与 stderr。
///
/// 上传存储的其他变量由 `api_probe_command` 显式移除，所以这里给的每一个都是"探针要探的那一个"。
async fn probe_api_startup_with_upload_env(
    database_url: &str,
    env: &[(&str, &str)],
) -> (bool, String) {
    let mut command = api_probe_command(database_url, probe_port(), "upload-probe-token");
    command.stdout(Stdio::null()).stderr(Stdio::piped());
    for (name, value) in env {
        command.env(name, value);
    }
    probe_running_and_stderr(command).await
}

/// 起一个已经装配好的命令，等它要么退出一场配置错误、要么真的开始服务，并回报 stderr。
///
/// 只用于"启动该失败/该成功"这一类判据：不查 `/health`、不写夹具，也不会把一个还在跑的探针
/// 进程留在后面。
async fn probe_running_and_stderr(mut command: Command) -> (bool, String) {
    let mut child = command.spawn().expect("API process should start");
    tokio::time::sleep(Duration::from_millis(1_800)).await;
    let running = child.try_wait().expect("try_wait").is_none();
    if running {
        let _ = child.kill();
        let _ = child.wait();
    }
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        use std::io::Read;
        let _ = pipe.read_to_string(&mut stderr);
    }
    (running, stderr)
}

/// API 进程的可选配置：**加速层**、**平台告警出口**与其余按用例压小的取值。
///
/// 平台告警出口（`PROVIDER_ALERT_WEBHOOK`）给账实核对用：核对**没有默认周期**，由用例显式触发
/// （见 `trigger_ledger_audit`），出口只决定发现不符时外发到哪里。
#[derive(Default)]
struct ApiProcessSettings {
    cache: Option<CacheFixture>,
    alert_webhook: Option<String>,
    /// 引导管理员账号用的邮箱与口令：给了就等价于运维在部署时配了 `ADMIN_EMAIL`/`ADMIN_PASSWORD`。
    ///
    /// 缺省**不配**——既有用例全都靠共享令牌，配了反而会多出一个账号；只有验登录那条链的用例才给。
    admin_credentials: Option<(String, String)>,
    /// 会话有效期（秒）：用例要验"过期凭据被拒"时把它压到等得起的量级。
    session_ttl_seconds: Option<u64>,
    /// 结算预留 R（秒）：直接执行的总期限 D 减去它才是上游预算。缺省取应用层的
    /// [`DEFAULT_SETTLE_RESERVE_SECONDS`]，不在这里另写一个数。
    settle_reserve_seconds: Option<u64>,
    /// 正文慢读期限（秒）：用例要观察"滴流慢读被 408 收口"时把它压到等得起的量级。
    slow_read_timeout_seconds: Option<u64>,
    /// 本机在飞执行的字节预算（GENERATION_MAX_MEMORY_BYTES）：用例要观察在飞执行占满预算后
    /// 新请求被拒时，把它压到只够一次执行。缺省不配，进程用生产默认值（2GiB）。
    ///
    /// 下限是每次执行的固定预留 32MiB——比它小 Supervisor::new 直接拒绝启动，所以单个请求
    /// 永远在预算内；能压出来的边界只有预算已被另一个在飞执行占满这一种。
    max_memory_bytes: Option<usize>,
    /// 渠道全局未决任务上限（GENERATION_MAX_CHANNEL_IN_FLIGHT）：用例要观察"账户名额还空着、
    /// 但渠道名额已被另一个副本占住"那条 503 时把它压到最小。缺省不配，进程用生产默认值（32）。
    channel_max_in_flight: Option<u64>,
    /// 请求 JSON 结构的四条计数上限（`GENERATION_REQUEST_JSON_*`，RFC 0018 §2.2）：用例要观察
    /// 某一条超限在受理前被拒时把它压到等得起的量级。缺省不配，进程用从 16 MiB 推导的默认值；
    /// 与生产校验一致，只允许收紧。
    request_json_max_depth: Option<usize>,
    request_json_max_nodes: Option<usize>,
    request_json_max_object_fields: Option<usize>,
    request_json_max_string_bytes: Option<usize>,
    /// 请求内安全重投的运维取值（次数与退避基）：直接执行在受理路径上读 `GENERATION_RETRY_*`。
    retry: RetrySettings,
    /// 公开鉴权端点的失败尝试上限（次数、窗口毫秒）。缺省不配，进程用默认值（10 次 / 60 秒）。
    auth_attempt_limit: Option<(u64, u64)>,
    /// 公开鉴权端点来源维采信的受信头。缺省不配，进程退回连接对端地址。
    auth_source_header: Option<String>,
    /// 上传存储夹具：给了就在起进程时配上假对象存储（region/bucket/密钥固定，endpoint 由
    /// `Harness::build` 填）。缺省不配——进程按"上传存储未配置"启动，上传端点回 503。
    upload_storage: Option<UploadStorageFixture>,
    /// PostgreSQL 连接池上限（`DATABASE_MAX_CONNECTIONS`）：容量测量要靠它把池这一轴扫出来。
    /// 缺省不配，进程取缺省。
    database_max_connections: Option<u32>,
}

/// 一次用例的上传存储夹具：假对象存储的行为与进程级上传上限。
#[derive(Clone, Default)]
struct UploadStorageFixture {
    behaviour: ObjectStorageBehaviour,
    /// 假对象存储地址；由 `Harness::build` 在起进程之前填上。
    endpoint: Option<String>,
    max_request_bytes: Option<usize>,
    slots: Option<usize>,
    max_buffer_bytes: Option<usize>,
    slow_read_timeout_seconds: Option<u64>,
    retry_max_attempts: Option<u32>,
    retry_backoff_base_seconds: Option<u64>,
}

impl UploadStorageFixture {
    fn with_behaviour(behaviour: ObjectStorageBehaviour) -> Self {
        Self {
            behaviour,
            ..Self::default()
        }
    }
}

/// 一次用例的全部进程配置：API 进程那一套与发布时的修订级加价系数。
///
/// 两件事装在一起，只是因为它们都是"这次用例怎么起这套服务"的参数：分成两个参数传下去会让
/// `Harness::build` 的参数表长到读不出哪一项管什么。
#[derive(Default)]
struct CaseSettings {
    api: ApiProcessSettings,
    /// 发布时的**修订级**加价系数（按张 / 按次 / 上游给金额的候选要靠它算对客价）。
    markup_bps: Option<i32>,
}

impl ApiProcessSettings {
    /// 配置好加速层。
    fn with_cache(cache: CacheFixture) -> Self {
        Self {
            cache: Some(cache),
            ..Self::default()
        }
    }

    /// 同 [`Self::with_cache`]，但把公开鉴权端点的**失败尝试上限**压到用例等得起的量级，
    /// 并让来源维采信 `x-real-ip`——用例因此可以用不同的头区分"同一来源"与"同一身份"。
    fn with_cache_and_auth_attempt_limit(
        cache: CacheFixture,
        failures: u64,
        window_ms: u64,
    ) -> Self {
        Self {
            cache: Some(cache),
            auth_attempt_limit: Some((failures, window_ms)),
            auth_source_header: Some("x-real-ip".to_owned()),
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
        // 这个进程的**对客同步等待窗口**：用例给多长就是多长，但有一个下限——链上要求
        // "窗口 ≥ 上游超时"，而生产那套按张算超时的**取值**在这里压不到 1s：APIMart 的 Driver 每 3s
        // 轮询一次任务，而"上游超时"同时是这一次执行的**总期限**，压到 1s 会让"提交 + 轮询到终态"
        // 必然超时（实测两条用例因此变成"结果不明"）。所以窗口与上游超时都取 10s 与用例窗口里较大
        // 的那个：1s / 2s 那些用例要观察的"窗口先到期"仍然成立——窗口一到，这一次执行就按
        // `request_timeout` 回 504。生产取值（300s 级）不能搬进用例：每条都要等上几分钟，而且会
        // 逼着校验放宽。
        let test_chain_seconds = sync_wait_seconds.max(10);
        let mut command = Command::new(env!("CARGO_BIN_EXE_seeai-api"));
        remove_proxy_env(&mut command);
        command
            // 工作目录落在临时目录：进程启动会读一次 `.env`（`dotenvy` 从工作目录向上找），默认落在
            // 仓库内就会读到**开发者本机**那份配置——用例的结论于是随各人环境变（本机 `.env` 里配了
            // 上传存储时，"没配存储回 503"那条用例必红）。下面按绝对路径给的变量不依赖工作目录。
            .current_dir(std::env::temp_dir())
            .env("DATABASE_URL", database_url)
            .env("API_BIND", format!("127.0.0.1:{port}"))
            .env("ADMIN_TOKEN", &admin_token)
            .env("SEE_BASEURL", "http://api.contract.test")
            // 客户历史游标密钥是**必须配**的（生产缺了进程起不来），夹具也给一份固定的 32 字节。
            .env("CUSTOMER_HISTORY_CURSOR_KEY", CONTRACT_CURSOR_KEY)
            // **显式不导入供给素材**：每个用例的库是空的、夹具自己造；不设的话默认值
            // （`config/bootstrap`）会让它们先看到仓库那两份素材。
            .env("SUPPLY_MATERIAL_DIR", "")
            // 公共文档按绝对路径指到仓库的 `public-docs/`：进程在临时目录里起，相对路径找不到。
            .env(
                "PUBLIC_DOCS_DIR",
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../public-docs"),
            )
            .env(
                "GENERATION_SYNC_WAIT_SECONDS",
                test_chain_seconds.to_string(),
            )
            .env(
                "PROVIDER_TIMEOUT_BASE_SECONDS",
                test_chain_seconds.to_string(),
            )
            .env("PROVIDER_TIMEOUT_INCLUDED_IMAGES", "4")
            .env("PROVIDER_TIMEOUT_PER_IMAGE_SECONDS", "0")
            .env("PROVIDER_TIMEOUT_SECONDS", test_chain_seconds.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        apply_cache_env(&mut command, settings.cache.as_ref());
        if let Some(limit) = settings.channel_max_in_flight {
            command.env("GENERATION_MAX_CHANNEL_IN_FLIGHT", limit.to_string());
        }
        if let Some((failures, window_ms)) = settings.auth_attempt_limit {
            // 三个端点各自一份上限与窗口：用例把三份都压到同一个量级，端点之间仍互不占用。
            for prefix in [
                "AUTH_ATTEMPT_LIMIT_REGISTER",
                "AUTH_ATTEMPT_LIMIT_LOGIN",
                "AUTH_ATTEMPT_LIMIT_REDEEM",
            ] {
                command
                    .env(
                        format!("{prefix}_FAILURES_PER_WINDOW"),
                        failures.to_string(),
                    )
                    .env(format!("{prefix}_WINDOW_MS"), window_ms.to_string());
            }
        }
        if let Some(name) = &settings.auth_source_header {
            command.env("AUTH_SOURCE_HEADER", name);
        }
        if let Some(webhook) = &settings.alert_webhook {
            command.env("PROVIDER_ALERT_WEBHOOK", webhook);
        }
        if let Some((email, password)) = &settings.admin_credentials {
            command
                .env("ADMIN_EMAIL", email)
                .env("ADMIN_PASSWORD", password);
        }
        if let Some(seconds) = settings.session_ttl_seconds {
            command.env("SESSION_TTL_SECONDS", seconds.to_string());
        }
        // 图片生成只有直接执行这一条路：指纹密钥与渠道凭证是**必须配**的（缺任何一项进程都拒绝
        // 启动），所以每个用例都配上假值；取值只在这个用例的假上游上用过，不写入配置、日志或响应。
        command
            .env(
                "GENERATION_SETTLE_RESERVE_SECONDS",
                settings
                    .settle_reserve_seconds
                    .unwrap_or(DEFAULT_SETTLE_RESERVE_SECONDS)
                    .to_string(),
            )
            .env("REQUEST_FINGERPRINT_KEY_V1", CONTRACT_FINGERPRINT_KEY)
            .env("AIHUBMIX_API_KEY", CONTRACT_PROVIDER_KEY)
            .env("APIMART_API_KEY", CONTRACT_PROVIDER_KEY)
            // 请求内安全重投的运维取值：用例按自己等得起的量级给（生产缺省是 3 次 / 1 秒起步）。
            .env(
                "GENERATION_RETRY_MAX_ATTEMPTS",
                settings.retry.max_attempts.to_string(),
            )
            .env(
                "GENERATION_RETRY_BACKOFF_BASE_MS",
                settings.retry.backoff_base_ms.to_string(),
            );
        if let Some(seconds) = settings.slow_read_timeout_seconds {
            command.env("GENERATION_SLOW_READ_TIMEOUT_SECONDS", seconds.to_string());
        }
        if let Some(bytes) = settings.max_memory_bytes {
            command.env("GENERATION_MAX_MEMORY_BYTES", bytes.to_string());
        }
        // 池上限显式给：不给时置空，进程取缺省——否则本机 shell 里导出的同名变量会漏进每个用例。
        command.env(
            "DATABASE_MAX_CONNECTIONS",
            settings
                .database_max_connections
                .map(|max| max.to_string())
                .unwrap_or_default(),
        );
        // 请求结构上限：只配用例明确要压的那几条，其余留给进程的推导默认值。
        for (name, value) in [
            (
                "GENERATION_REQUEST_JSON_MAX_DEPTH",
                settings.request_json_max_depth,
            ),
            (
                "GENERATION_REQUEST_JSON_MAX_NODES",
                settings.request_json_max_nodes,
            ),
            (
                "GENERATION_REQUEST_JSON_MAX_OBJECT_FIELDS",
                settings.request_json_max_object_fields,
            ),
            (
                "GENERATION_REQUEST_JSON_MAX_STRING_BYTES",
                settings.request_json_max_string_bytes,
            ),
        ] {
            if let Some(value) = value {
                command.env(name, value.to_string());
            }
        }
        apply_upload_env(&mut command, settings.upload_storage.as_ref());
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

/// 一次用例给**请求内安全重投**配的运维取值：一次请求最多调几次上游、第一次重投前等多久。
///
/// 字段直接就是那两个环境变量的值，由 API 进程在受理路径上读。重投只在**可证明未受理**时发生，
/// 而"证明"来自假上游怎么应答（见 [`UpstreamBehaviour::create_rejection_status`]）——所以用例能
/// 精确地构造出"第一次没被受理、第二次成功"与"一直被拒直到用完额度"这两种形态。
///
/// 缺省那套（[`Self::default`]）把退避压到毫秒级、上限给 3：用例跑得快，而"重投发生过"仍然
/// 看得见。把它们当成"测试专用的一套语义"是错的——它们本来就是运维配置，生产缺省是 3 次 /
/// 1 秒起步。
#[derive(Debug, Clone, Copy)]
struct RetrySettings {
    max_attempts: u32,
    backoff_base_ms: u64,
}

impl Default for RetrySettings {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            backoff_base_ms: 20,
        }
    }
}

impl RetrySettings {
    /// 关掉重投：上限 1 就是"一次请求只调一次上游"。
    fn disabled() -> Self {
        Self {
            max_attempts: 1,
            ..Self::default()
        }
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
    /// 连到**基库**（`HTTP_CONTRACT_DATABASE_URL`）的池：读 `pg_stat_database` 时用它，
    /// 这样读本身提交的事务记在基库上，不会算进被观测的一次性库。
    admin_pool: PgPool,
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
    /// 这次用例给 API 与对账配的加速层（假 Redis）；没配就是"没有缓存"的那条路径。
    cache: Option<CacheFixture>,
    /// 这次用例假上游上的闸门（没配 [`UpstreamBehaviour::holding`] 时为 `None`）。
    hold: Option<Arc<UpstreamGate>>,
    /// 这次用例的假对象存储；没配上传夹具时为 `None`。
    upload_storage: Option<FakeObjectStorage>,
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
            CaseSettings {
                markup_bps,
                ..CaseSettings::default()
            },
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
            CaseSettings::default(),
        )
        .await
    }

    /// 同 [`Self::start_with_draft`]，但把同步入口的等待窗口压到用例等得起的量级。
    ///
    /// 不跑 Worker 的用例靠它让"已受理但没结算"的那次请求尽快以 `504 result_pending` 返回：
    /// 受理与占用已经提交，正好在占用还占着的时候观察账户当前值。
    async fn start_with_sync_wait(
        draft: Value,
        behaviour: UpstreamBehaviour,
        max_concurrent_jobs: u64,
        sync_wait_seconds: u64,
    ) -> Self {
        Self::build(
            draft,
            None,
            behaviour,
            max_concurrent_jobs,
            sync_wait_seconds,
            CaseSettings::default(),
        )
        .await
    }

    /// 起一套带上传存储夹具的装置：假上游照旧，上传端点指向进程内假对象存储。
    ///
    /// 上传与生成共用同一个 Supervisor（第二套上传名额与预算）与同一套客户凭证，因此这里照常
    /// 发布一条供给、签发一把密钥，只是用例发的是上传请求。
    async fn start_with_upload_storage(
        upload: UploadStorageFixture,
        behaviour: UpstreamBehaviour,
    ) -> Self {
        Self::build(
            candidate(
                "APIMart",
                "apimart-image-v1",
                &["prompt_only", "image_conditioned", "masked"],
            ),
            None,
            behaviour,
            64,
            30,
            CaseSettings {
                api: ApiProcessSettings {
                    upload_storage: Some(upload),
                    ..ApiProcessSettings::default()
                },
                ..CaseSettings::default()
            },
        )
        .await
    }

    /// 同 [`Self::start_with_upload_storage`]，另外配一个加速层缓存。
    ///
    /// 上传这条路上曾经有一层按密钥的速率计数，它落在缓存里：用例要证明那一层不在了，就得让缓存
    /// 配上——没有缓存时它本来就不生效。
    async fn start_with_upload_storage_and_cache(
        upload: UploadStorageFixture,
        behaviour: UpstreamBehaviour,
        cache: CacheFixture,
    ) -> Self {
        Self::build(
            candidate(
                "APIMart",
                "apimart-image-v1",
                &["prompt_only", "image_conditioned", "masked"],
            ),
            None,
            behaviour,
            64,
            30,
            CaseSettings {
                api: ApiProcessSettings {
                    upload_storage: Some(upload),
                    cache: Some(cache),
                    ..ApiProcessSettings::default()
                },
                ..CaseSettings::default()
            },
        )
        .await
    }

    /// 同 [`Self::start_with_draft`]，但给**请求内安全重投**定下运维取值（上限与退避基）。
    ///
    /// 用例要能观察到"重投了几次"就必须把退避压到毫秒级：生产缺省是 1 秒起步、指数增长，靠它
    /// 跑重投会把每条用例拖成分钟级。这两项本来就是运维配置，用例按自己等得起的量级给。
    async fn start_with_retry(
        draft: Value,
        behaviour: UpstreamBehaviour,
        max_concurrent_jobs: u64,
        retry: RetrySettings,
    ) -> Self {
        Self::build(
            draft,
            None,
            behaviour,
            max_concurrent_jobs,
            30,
            CaseSettings {
                api: ApiProcessSettings {
                    retry,
                    ..ApiProcessSettings::default()
                },
                ..CaseSettings::default()
            },
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
            CaseSettings {
                api: ApiProcessSettings::with_cache(cache),
                ..CaseSettings::default()
            },
        )
        .await
    }

    /// 同 `start_with`，但给 API 进程配上**平台告警出口**（账实核对发现不符时外发到哪里）。
    ///
    /// 它**不**启缓存：核对读的是账本与余额，与加速层无关——顺带也就验了"没有缓存时这条核查
    /// 照样跑"（那条缓存对账在没有缓存时根本不进循环，两条检查的启用条件不同）。
    async fn start_with_ledger_audit(
        webhook: Option<String>,
        behaviour: UpstreamBehaviour,
        max_concurrent_jobs: u64,
    ) -> Self {
        Self::build(
            candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]),
            None,
            behaviour,
            max_concurrent_jobs,
            30,
            CaseSettings {
                api: ApiProcessSettings {
                    alert_webhook: webhook,
                    ..ApiProcessSettings::default()
                },
                ..CaseSettings::default()
            },
        )
        .await
    }

    /// 起一个开着**直接执行**的 API：不跑 Worker，图片入口在进程内直连假上游。
    ///
    /// sync_wait_seconds 是总期限 D，配置里的结算预留 R 由 ApiProcessSettings 给；上游预算因此
    /// 是 D 减 R。用例不启 Worker，走的正是"API 自己执行、不读 Job、不轮询结果"那条路。
    async fn start_direct(
        draft: Value,
        behaviour: UpstreamBehaviour,
        max_concurrent_jobs: u64,
        sync_wait_seconds: u64,
    ) -> Self {
        Self::build(
            draft,
            None,
            behaviour,
            max_concurrent_jobs,
            sync_wait_seconds,
            CaseSettings::default(),
        )
        .await
    }

    /// 同 [`Self::start_direct`]，但允许用例覆盖进程配置（例如把慢读期限压短）。
    async fn start_direct_with(
        draft: Value,
        behaviour: UpstreamBehaviour,
        max_concurrent_jobs: u64,
        sync_wait_seconds: u64,
        api: ApiProcessSettings,
    ) -> Self {
        Self::build(
            draft,
            None,
            behaviour,
            max_concurrent_jobs,
            sync_wait_seconds,
            CaseSettings {
                api,
                ..CaseSettings::default()
            },
        )
        .await
    }

    /// 直接执行 + AIHubMix 同步渠道的默认候选：A1 的最小闭环就用它。
    async fn start_direct_aihubmix(
        behaviour: UpstreamBehaviour,
        max_concurrent_jobs: u64,
        sync_wait_seconds: u64,
    ) -> Self {
        Self::start_direct(
            candidate(
                "AIHubMix",
                "aihubmix-image-v1",
                &["prompt_only", "image_conditioned", "masked"],
            ),
            behaviour,
            max_concurrent_jobs,
            sync_wait_seconds,
        )
        .await
    }

    async fn build(
        mut draft: Value,
        contract: Option<Value>,
        behaviour: UpstreamBehaviour,
        max_concurrent_jobs: u64,
        sync_wait_seconds: u64,
        mut settings: CaseSettings,
    ) -> Self {
        // 倍率由 `publication_body` 一处按候选的对客形态补（声明金额计价的候选必须有它）。
        let (database_url, database_name) = isolated_database_url().await;
        let calls: UpstreamCalls = Arc::new(Mutex::new(Vec::new()));
        // 闸门句柄要在 behaviour 交给假上游之前取出来：用例侧拿它等到达、放行。
        let hold = behaviour.hold.as_ref().map(|(_, gate)| gate.clone());
        let upstream = start_fake_upstream_with(calls.clone(), behaviour).await;
        // 上传夹具：先把假对象存储起起来，再把它的地址填进进程配置，API 子进程才连得上。
        let upload_storage = match settings.api.upload_storage.take() {
            Some(mut fixture) => {
                let storage = start_fake_object_storage(fixture.behaviour.clone()).await;
                fixture.endpoint = Some(storage.endpoint.clone());
                settings.api.upload_storage = Some(fixture);
                Some(storage)
            }
            None => None,
        };
        // 名额已经由本次发布命令给出（`max_concurrent_jobs` 形参走 publish），进程配置里没有它。
        let _ = max_concurrent_jobs;
        let (base_url, admin_token, process) =
            start_api_with(&database_url, sync_wait_seconds, &settings.api).await;
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
        let admin_pool = PgPool::connect(
            &std::env::var("HTTP_CONTRACT_DATABASE_URL")
                .expect("HTTP_CONTRACT_DATABASE_URL is required for the ignored contract tests"),
        )
        .await
        .expect("contract base database");

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
        let published = publish_candidates_with_quota(
            &client,
            &base_url,
            &admin_token,
            Self::MODEL,
            contract,
            vec![draft],
            settings.markup_bps,
            Some(u32::try_from(max_concurrent_jobs).unwrap_or(u32::MAX)),
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
            admin_pool,
            calls,
            upstream_base_url: upstream.base_url.clone(),
            declared_surface,
            _api: process,
            _upstream: upstream,
            cache: settings.api.cache,
            hold,
            upload_storage,
        }
    }

    /// 起**第二个**连同一数据库的 API 副本：多副本容量竞争与"缓存通知丢失"的验收靠它。
    ///
    /// 副本复用本装置已经发布好的库，摘要/指纹密钥与假渠道凭证也由 `start_api_with` 的直连分支
    /// 给同一套（见那里），所以同一把客户密钥在两个进程上都认得出。返回它的 `base_url` 与进程
    /// 句柄，丢弃句柄即结束它。
    async fn start_replica(
        &self,
        sync_wait_seconds: u64,
        settings: &ApiProcessSettings,
    ) -> (String, ApiProcess) {
        let (base_url, _admin_token, process) =
            start_api_with(&self.database_url, sync_wait_seconds, settings).await;
        (base_url, process)
    }

    /// 这次用例的假 Redis；没配缓存的用例调用它会直接失败（那是用例写错了）。
    fn cache(&self) -> &CacheFixture {
        self.cache
            .as_ref()
            .expect("this test must run with the cache fixture")
    }

    /// 假上游上的闸门：配了 [`UpstreamBehaviour::holding`] 的用例用它等到达、等放行。
    fn gate(&self) -> &Arc<UpstreamGate> {
        self.hold
            .as_ref()
            .expect("this case must pin the fake upstream with UpstreamBehaviour::holding")
    }

    /// 一个指向**本用例假上游**的公网参考图 URL。
    ///
    /// 生成入口只收公网 URL：APIMart 逐字透传，AIHubMix 自己来取。假上游对任何 GET 都回
    /// PNG_FIXTURE（见 `serve_fake_upstream`），所以这份地址对两家都能当合法参考图。
    fn png_url(&self) -> String {
        self.input_url("ref.png")
    }

    /// 假上游上一个具名的输入图地址：同一个用例要区分多张参考图时用它（下载计数才分得开）。
    fn input_url(&self, name: &str) -> String {
        format!("{}/inputs/{name}", self.upstream_base_url)
    }

    /// 走同步入口发一次 JSON 请求：这一次执行在本进程内跑完才返回。
    async fn sync_json(&self, path: &str, key: &str, body: Value) -> (StatusCode, Value) {
        post_json(&self.base_url, &self.api_key, path, key, &body).await
    }

    /// 走同步入口发一次 multipart 请求（edits 路径）：这一次执行在本进程内跑完才返回。
    async fn sync_multipart(
        &self,
        key: &str,
        form: reqwest::multipart::Form,
    ) -> (StatusCode, Value) {
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
    /// 走上传端点发一次 multipart 请求（字段名固定 `file`）。
    async fn upload(&self, part: reqwest::multipart::Part) -> (StatusCode, Value) {
        let form = reqwest::multipart::Form::new().part("file", part);
        self.upload_form(form).await
    }

    /// 走上传端点发一次任意 multipart 表单：缺 `file`、多个部件等形态由用例自己构造。
    async fn upload_form(&self, form: reqwest::multipart::Form) -> (StatusCode, Value) {
        let response = Client::new()
            .post(format!("{}/v1/uploads/images", self.base_url))
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .await
            .expect("upload request");
        let status = response.status();
        let body = response.text().await.expect("upload response body");
        (
            status,
            serde_json::from_str(&body).unwrap_or(Value::String(body)),
        )
    }

    /// 这次用例的假对象存储。
    fn object_storage(&self) -> &FakeObjectStorage {
        self.upload_storage
            .as_ref()
            .expect("this case must run with the upload storage fixture")
    }

    /// 用不带凭证的客户端读一个公网 URL，返回状态码与字节。
    async fn read_public(&self, url: &str) -> (StatusCode, Vec<u8>) {
        let response = Client::new().get(url).send().await.expect("public read");
        let status = response.status();
        let bytes = response.bytes().await.expect("public read body").to_vec();
        (status, bytes)
    }

    /// 按幂等键取回这次请求内部的执行记录：`(job_id, state)`。
    ///
    /// 这是**内部**事实，对客响应里没有它——同步入口不返回 job_id。v1 记录只存幂等键的**摘要**，
    /// 明文键不进库，所以这里按摘要查。载荷（结果图片、参数）**不在库里**：要看结果就读这一次的
    /// 对客响应。
    async fn job(&self, key: &str) -> (Uuid, String) {
        let row =
            sqlx::query("SELECT id, state FROM generation.jobs WHERE idempotency_key_digest = $1")
                .bind(idempotency_key_digest(key))
                .fetch_one(&self.pool)
                .await
                .expect("the request must have created a job record");
        (
            row.try_get("id").expect("job id"),
            row.try_get("state").expect("job state"),
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
        let raw = self.submit_bytes(path);
        serde_json::from_slice(&raw).expect("submit body is JSON")
    }

    /// 某个路径上**最近一次**记录到的原始请求体（multipart 形态）。
    /// 假上游收到的**最近一次**该路径的请求体。
    ///
    /// 路径按**后缀**比：两家的生成入口前缀不同（AIHubMix 是 `/ai/v1/images/generations`，
    /// APIMart 是 `/v1/images/generations`），调用点只写自己关心的那一段就够。
    fn submit_bytes(&self, path: &str) -> Vec<u8> {
        self.recorded()
            .into_iter()
            .rfind(|call| call.method == "POST" && call.path.ends_with(path))
            .map(|call| call.body)
            .expect("the driver must submit a generation request")
    }

    fn count(&self, method: &str, path_prefix: &str) -> usize {
        self.recorded()
            .iter()
            .filter(|call| call.method == method && call.path.starts_with(path_prefix))
            .count()
    }

    /// 假上游收到的**生成请求**次数：重投的判据是"上游被调了几次"，不是"内部记了几行"。
    ///
    /// 两家的生成端点路径不同（同步渠道是 `/v1/images/generations` 与 `/v1/images/edits`，
    /// 任务式渠道也是这两条），所以判据只能是"POST 到生成端点"，不能钉死其中一条。
    fn create_calls(&self) -> usize {
        self.recorded()
            .iter()
            .filter(|call| {
                call.method == "POST"
                    && (call.path.ends_with("/images/generations")
                        || call.path.ends_with("/images/edits"))
            })
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

    /// 这次用例启动的 API 进程号：峰值 RSS 用例据此读 `VmHWM`。
    fn api_pid(&self) -> u32 {
        self._api.pid()
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
/// 内部它就是一个执行与审计记录：对客只暴露它的标识（调用标识），没有内部状态、没有任务号、
/// 没有"去查任务"的指引。
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

/// 对客成功响应：`{code, data:{id, status, cost, result:{images[]}}}`，且没有内部字样。
fn assert_sync_success(what: &str, body: &Value) {
    assert_public_only(what, body);
    assert_eq!(
        body["code"].as_u64(),
        Some(200),
        "{what}: code must be 200, got {body}"
    );
    let data = body["data"]
        .as_object()
        .unwrap_or_else(|| panic!("{what}: data must be an object, got {body}"));
    assert!(
        data["id"].as_str().is_some(),
        "{what}: id is required, got {body}"
    );
    assert_eq!(
        data["status"].as_str(),
        Some("completed"),
        "{what}: status must be completed, got {body}"
    );
    assert!(
        data["cost"].as_u64().is_some(),
        "{what}: cost must be an integer number of points, got {body}"
    );
    let images = data["result"]["images"]
        .as_array()
        .unwrap_or_else(|| panic!("{what}: result.images must be an array, got {body}"));
    assert!(
        !images.is_empty(),
        "{what}: result.images must not be empty"
    );
    for item in images {
        let has_url = item["url"].as_array().is_some();
        let has_base64 = item["b64_json"].as_str().is_some();
        assert!(
            has_url ^ has_base64,
            "{what}: every item keeps exactly one of url / b64_json, got {item}"
        );
        if has_url {
            let urls = item["url"].as_array().expect("url array");
            assert!(
                !urls.is_empty(),
                "{what}: a url item carries at least one address, got {item}"
            );
            assert!(
                urls.iter().all(|url| url.as_str().is_some()),
                "{what}: every address is a string, got {item}"
            );
        }
        for key in item
            .as_object()
            .map(|object| object.keys())
            .into_iter()
            .flatten()
        {
            assert!(
                matches!(key.as_str(), "url" | "b64_json" | "expires_at"),
                "{what}: unexpected image field `{key}` in {item}"
            );
        }
    }
}

/// 只要走通一次真实执行，内部就必须留下一条跑完的记录：内部有记录，对客看不见。
async fn assert_job_succeeded(harness: &Harness, key: &str) -> Uuid {
    let (job_id, state) = harness.job(key).await;
    assert_eq!(state, "succeeded", "内部执行记录必须跑到终态");
    job_id
}

struct DriverOutcome {
    job_state: String,
    submits: usize,
    polls: usize,
    harness: Harness,
}

/// 起 API + 假上游，让一次文生图请求走完整个执行流程，返回它的结局。
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
    let (_, job_state) = harness.job(&key).await;
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
/// 假上游记录里**最近一次**该路径的请求体（路径按后缀比，两家的入口前缀不同）。
fn last_submit_body(calls: &UpstreamCalls, path: &str) -> Value {
    let raw = calls
        .lock()
        .expect("calls lock")
        .iter()
        .rfind(|call| call.method == "POST" && call.path.ends_with(path))
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
    let job_id = job_id_of(pool, key).await;
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
            // 对客形态按渠道的能力给：AIHubMix 的成功件没有 token 分项，只能按上游声明的金额加价；
            // APIMart 给得出四分项用量，沿用按 token 四档卖（这批用例只看参数映射，与计价无关）。
            let consumer_formula = if adapter_key == "apimart-image-v1" {
                json!("token_rates")
            } else {
                json!("upstream_declared")
            };
            json!({
                "provider_kind": if adapter_key == "apimart-image-v1" { "APIMart" } else { "AIHubMix" },
                "adapter_key": adapter_key,
                "provider_model_id": model,
                "base_url": "http://127.0.0.1:1",
                "credential_env": "AIHUBMIX_API_KEY",
                "restrictions": {"allowed_branches": ["prompt_only"], "max_reference_images": 0},
                "carrier_schema": carrier,
                "parameter_mapping": parameter_mapping,
                "formula": "token_rates",
                "consumer_formula": consumer_formula,
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
        "type": "image",
        "actor": "contract-test",
        "capability_schema": contract,
        "offerings": offerings,
        // 有候选按上游声明的金额计价，修订级倍率就是它的对客价来源，必须一起发。
        "markup_bps": 2_000,
        // 内联发布必须带文档素材：夹具按同版合同生成一份最小素材（Spec 0008 §4）。
        "documentation": documentation_for(&contract)
    });
    let response = client
        .post(format!("{base_url}/api/v1/runtime-revisions"))
        .bearer_auth(admin_token)
        .json(&body)
        .send()
        .await
        .expect("runtime publication");
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        eprintln!("publication refused: {status} {text}");
    }
    status
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
    // 两条渠道各自的线上名：AIHubMix 的参考图叫 `images`、遮罩叫 `mask`，APIMart 用 `image_urls` / `mask_url`。
    let mut properties = json!({
        "model": {"const": "placeholder"},
        "prompt": {"type": "string", "minLength": 1}
    });
    let declares_image = branches
        .iter()
        .any(|branch| matches!(*branch, "image_conditioned" | "masked"));
    let vendor_names = adapter_key == "apimart-image-v1";
    // AIHubMix 走 `/ai/v1`：参考图是字符串数组（`images`）、遮罩是单值 `mask`；
    // APIMart 收 `image_urls` / `mask_url`。
    let image_parameter = if vendor_names { "image_urls" } else { "images" };
    let mask_parameter = if vendor_names { "mask_url" } else { "mask" };
    if declares_image {
        properties[image_parameter] = json!({
            "type": "array",
            "items": {"type": "string"},
            "minItems": 1,
            "maxItems": 1
        });
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
    let mut offering = json!({
        "provider_kind": provider_kind,
        "adapter_key": adapter_key,
        "provider_model_id": "route-model",
        "base_url": "http://127.0.0.1:1",
        "credential_env": "AIHUBMIX_API_KEY",
        "restrictions": {
            "allowed_branches": branches,
            // 收图上限也要与 Profile 自洽：纯文生图只声明 prompt，收图数就是 0。
            "max_reference_images": if declares_image { 1 } else { 0 }
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
    });
    // 计价形态按渠道的事实：AIHubMix 走 `/ai/v1`，只给上游声明的金额、没有 token 分项，所以
    // 成本与对客价都按声明金额算；APIMart 仍按四分项 token 计量量，带一份可复现的价目表。
    if !vendor_names {
        offering["formula"] = json!("upstream_declared");
        offering["consumer_formula"] = json!("upstream_declared");
        offering["cost_basis"] = json!("declared");
        offering["cost_currency"] = json!("USD");
        if let Some(object) = offering.as_object_mut() {
            object.remove("price_plan");
        }
    }
    offering
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
    publish_candidates_with_quota(
        client,
        base_url,
        admin_token,
        model,
        contract,
        offerings,
        markup_bps,
        None,
    )
    .await
}

/// 同 [`publish_candidates_with_markup`]，另可带并发名额（夹具按用例需要给出）。
#[allow(clippy::too_many_arguments)]
async fn publish_candidates_with_quota(
    client: &Client,
    base_url: &str,
    admin_token: &str,
    model: &str,
    contract: Option<Value>,
    offerings: Vec<Value>,
    markup_bps: Option<i32>,
    max_concurrent_jobs: Option<u32>,
) -> StatusCode {
    // 按上游声明金额计价（`consumer_formula: upstream_declared`）的候选，对客价靠修订级倍率算
    // 出来，缺了发布期就拒；夹具走这条发布路径时补一个默认值。**故意不给倍率**的用例各自直接
    // 组发布体，不受这里影响。
    let markup_bps = markup_bps.or_else(|| {
        offerings
            .iter()
            .any(|offering| offering["consumer_formula"] == json!("upstream_declared"))
            .then_some(2000)
    });
    let body = publication_body_with_quota(
        model,
        "route-test-1",
        contract,
        offerings,
        markup_bps,
        max_concurrent_jobs,
    );
    let response = client
        .post(format!("{base_url}/api/v1/runtime-revisions"))
        .bearer_auth(admin_token)
        .json(&body)
        .send()
        .await
        .expect("runtime publication");
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        eprintln!("publication refused: {status} {text}");
    }
    status
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
    max_concurrent_jobs: Option<u32>,
) -> StatusCode {
    let body = publication_body_with_quota(
        model,
        revision,
        Some(contract),
        offerings,
        markup_bps,
        max_concurrent_jobs,
    );
    let response = Client::new()
        .post(format!("{}/api/v1/runtime-revisions", harness.base_url))
        .bearer_auth(&harness.admin_token)
        .json(&body)
        .send()
        .await
        .expect("runtime publication");
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        eprintln!("publication refused: {status} {text}");
    }
    status
}

/// 拼一份发布命令：线上名以**承载面**为准，`model.const` 与 `provider_model_id` 都跟着本次型号走。
///
/// 旧形状的素材没有承载面，那份 `capability_schema` 就是它，两者落在同一处；带模型级合同时，
/// 合同里的 `model.const` 也必须等于 `native_model_id`（发布期的硬判据）。
fn publication_body(
    model: &str,
    revision: &str,
    contract: Option<Value>,
    offerings: Vec<Value>,
    markup_bps: Option<i32>,
) -> Value {
    publication_body_with_quota(model, revision, contract, offerings, markup_bps, None)
}

/// 同 [`publication_body`]，另可带上**并发名额**（`None`＝发布命令不给它）。
///
/// 并发名额是模型行上的运行状态（列必填）：命令不给时新建的模型按平台固定值 1。夹具用它把
/// "这套用例需要多大并发"写进发布命令——进程配置里已经没有名额来源。
fn publication_body_with_quota(
    model: &str,
    revision: &str,
    contract: Option<Value>,
    mut offerings: Vec<Value>,
    markup_bps: Option<i32>,
    max_concurrent_jobs: Option<u32>,
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
        "type": "image",
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
    if let Some(max_concurrent_jobs) = max_concurrent_jobs {
        body["max_concurrent_jobs"] = json!(max_concurrent_jobs);
    }
    // 内联发布也要带文档素材：没有它发布会被拒（Spec 0008 §4）。夹具按同版合同生成一份最小素材。
    let contract = body
        .get("capability_schema")
        .cloned()
        .or_else(|| body["offerings"][0].get("capability_schema").cloned());
    if let Some(contract) = contract {
        body["documentation"] = documentation_for(&contract);
    }
    body
}

/// 按合同生成一份最小文档素材：字段释义逐项覆盖属性与组合约束，正文只放参数表插入点。
pub(super) fn documentation_for(contract: &Value) -> Value {
    let mut fields = serde_json::Map::new();
    collect_documentation_fields(contract, "", &mut fields);
    if let Some(all_of) = contract.get("allOf").and_then(Value::as_array) {
        for index in 0..all_of.len() {
            fields.insert(format!("/allOf/{index}"), json!("组合约束。"));
        }
    }
    json!({
        "narrative": "# {{platform_name}}\n\n厂商 {{vendor_id}}，类型 {{model_type}}，修订 {{contract_revision}}。\n\n[API Key 鉴权](../../authentication.md)\n\n## 参数\n\n{{parameter_table}}\n",
        "fields": fields
    })
}

fn collect_documentation_fields(
    schema: &Value,
    prefix: &str,
    fields: &mut serde_json::Map<String, Value>,
) {
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (name, property) in properties {
            let pointer = format!("{prefix}/properties/{name}");
            fields.insert(pointer.clone(), json!("字段释义。"));
            collect_documentation_fields(property, &pointer, fields);
        }
    }
}

/// 排序后的样本在 `percentile`（0–1）处的取值：取向上取整那一档。
///
/// 空样本返回零：一档全被拒时没有延迟可报，那不是测量失败——拒本身就是这一档的结论。
/// 性能基线与容量测量共用这一处，分位口径不会各自漂移。
fn percentile(sorted: &[Duration], percentile: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let index = (sorted.len() as f64 * percentile).ceil() as usize - 1;
    sorted[index.min(sorted.len() - 1)]
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
    sqlx::query_scalar(
        "SELECT price_snapshot FROM generation.jobs WHERE idempotency_key_digest = $1",
    )
    .bind(idempotency_key_digest(key))
    .fetch_one(pool)
    .await
    .expect("the request must have created a job with a frozen snapshot")
}

/// 按幂等键（明文）找这次请求内部那条执行记录的标识。
///
/// 库里只存摘要，所以这里先算摘要再查；调用方拿它去读别的内部事实（选路判定、成本四列…）。
async fn job_id_of(pool: &PgPool, key: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM generation.jobs WHERE idempotency_key_digest = $1")
        .bind(idempotency_key_digest(key))
        .fetch_one(pool)
        .await
        .expect("the request must have created a job")
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

/// 等计数窗口的剩余时间够放下 `margin_ms`；不够就先等到下一个窗口开头之后 100 毫秒（正好停在
/// 边界上会又变成"贴着边界"）。
///
/// 计数键里带着窗口号、TTL 只给本窗口剩余时间，窗口按钟点等分（与实现里那套
/// 窗口算法用同一套钟和窗口长度）。序列跨过边界时，边界之后的判定读到的是归零的计数，断言就会时好时坏。
/// 余量够时不等，所以正常跑法不引入等待；对齐之后同一窗口内的判定是确定的，不必为此改窗口取值
/// 或放松断言。
async fn wait_for_window_margin(window_ms: u64, margin_ms: u64) {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the wall clock is after the epoch")
        .as_millis() as u64;
    let remaining = window_ms - now_ms % window_ms;
    if remaining < margin_ms {
        eprintln!("窗口余量不足，对齐到下一个计数窗口：等 {remaining} 毫秒（需要 {margin_ms}）");
        tokio::time::sleep(Duration::from_millis(remaining + 100)).await;
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

/// 走一次**结果不明**的执行，验对账案例的列出与退款处置。
///
/// v1 自己在 `fail_or_reconcile` 里建案、保留占用与渠道槽位，所以这条夹具不再人工造状态：
/// 假上游在受理后拒绝，这次执行就落在 `reconciliation_required`。
async fn verify_reconciliation_contract(harness: &Harness) {
    let client = Client::new();
    let key = format!("reconciliation-{}", Uuid::new_v4());
    let (status, body) = harness
        .sync_json(
            "/v1/images/generations",
            &key,
            route_request(harness.model, "reconciliation contract"),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "got {body}");
    assert_eq!(body["error"]["code"].as_str(), Some("outcome_unknown"));
    let job_id = job_id_of(&harness.pool, &key).await;
    let state: String = sqlx::query_scalar("SELECT state FROM generation.jobs WHERE id = $1")
        .bind(job_id)
        .fetch_one(&harness.pool)
        .await
        .expect("reconciliation job state");
    assert_eq!(state, "reconciliation_required");
    // 占用**保留**：受理的那一刻扣下的预授权一直占着，直到对账给出结论。
    let balance_after_hold: i64 = sqlx::query_scalar(
        "SELECT a.balance_microusd FROM ledger.accounts a JOIN generation.jobs j ON j.account_id = a.id WHERE j.id = $1",
    )
    .bind(job_id)
    .fetch_one(&harness.pool)
    .await
    .expect("balance after hold");
    let cases: Value = client
        .get(format!("{}/api/v1/reconciliation-cases", harness.base_url))
        .bearer_auth(&harness.admin_token)
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
            "{}/api/v1/reconciliation-cases/{job_id}/refund",
            harness.base_url
        ))
        .bearer_auth(&harness.admin_token)
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
                "{}/api/v1/reconciliation-cases/{job_id}/refund",
                harness.base_url
            ))
            .bearer_auth(&harness.admin_token)
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
    .fetch_one(&harness.pool)
    .await
    .expect("resolved state");
    assert_eq!(row.get::<String, _>("state"), "failed");
    // 退款是一次平台侧处置，不是"消费者的错"：对客码必须仍在白名单内，
    // 且退款已结清，不该再让消费者"等对账结论"。
    assert_eq!(row.get::<String, _>("error_code"), "platform_unavailable");
    assert_eq!(row.get::<String, _>("failure_kind"), "platform_internal");
    assert_eq!(row.get::<String, _>("status"), "released");
    // 解除预授权**不改已结算余额**（`0002` §2.4/§3）：余额就是受理之后那个数，不把占用加回来。
    assert_eq!(
        row.get::<i64, _>("balance_microusd"),
        balance_after_hold,
        "解除预授权只减占用，不动已结算余额"
    );
    let capture_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ledger.entries WHERE job_id = $1 AND kind = 'capture'",
    )
    .bind(job_id)
    .fetch_one(&harness.pool)
    .await
    .expect("capture count");
    assert_eq!(capture_count, 0);
    let removed_charge_columns: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.columns WHERE table_schema = 'operations' AND table_name = 'reconciliation_cases' AND column_name IN ('resolution', 'charge_microusd')",
    )
    .fetch_one(&harness.pool)
    .await
    .expect("reconciliation schema");
    assert_eq!(removed_charge_columns, 0);
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

/// 一条**按张 / 按次**计价的候选草案：成本形态、成本单价、成本币种与那组定价参考都带上。
///
/// 承载面沿用夹具那条默认候选（同一份合同、同一份承载面），所以重新发布它不会撞上"合同不可变"。
///
/// 按张 / 按次是**成本**形态；对客形态是另一件事，必须显式给一种。这里给按 token 四档与一份向量，
/// 让这些用例只看成本侧的路径（成本事实）不被对客形态挡住。
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
    // 对客形态只有 `token_rates` 与 `upstream_declared` 两个取值：按张 / 按次只作**成本**形态，
    // 对客价按上游声明的金额加价。这条供给声明了成本币种，客户价因此按同币种直接加价、不做折算。
    draft["consumer_formula"] = json!("upstream_declared");
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
    markup_bps: i32,
) -> StatusCode {
    let mut draft = candidate("AIHubMix", "aihubmix-image-v1", &["prompt_only"]);
    draft["base_url"] = Value::String(harness.upstream_base_url.clone());
    draft["reference_cost_microusd"] = json!(11_354);
    // 这条渠道的响应只有上游声明的金额、没有 token 分项：成本按声明值记，对客也按声明金额加价。
    // 对客 token 价目在声明金额形态下永远不会被读，发布期带着它反而被拒。
    draft["cost_basis"] = json!("declared");
    draft["consumer_formula"] = json!("upstream_declared");
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
// 值，陈旧一律回源，缓存不足只记提示、402 一律由数据库条件更新确认，缓存停掉结果逐位不变。
//
// 缓存服务用**进程内假 Redis**（与假上游同一套做法）：验收要能直接改坏缓存里的值、能让写入失败
// （模拟"失效没成功"），还要在没有 Redis 的机器上跑得起来。它只在 127.0.0.1 上监听、不出网。

/// 缓存参数：写进 API / Worker 进程的环境变量。默认值就是设计里给的那一套。
#[derive(Debug, Clone, Copy)]
struct CacheSettings {
    balance_ttl_seconds: u64,
    reconcile_interval_ms: u64,
    /// 单条缓存命令的等待上限（毫秒），写进 `CACHE_OPERATION_TIMEOUT_MS`。
    ///
    /// 缺省 200ms 就是生产里的快速失败取值；强杀矩阵要"把进程卡在缓存写回上"，得把它调到
    /// 等得起的量级，否则写入会先超时、进程照样把响应交出去。
    operation_timeout_ms: u64,
}

impl Default for CacheSettings {
    fn default() -> Self {
        Self {
            balance_ttl_seconds: 360,
            reconcile_interval_ms: 180_000,
            operation_timeout_ms: 200,
        }
    }
}

impl CacheSettings {
    /// 换掉对账周期：用例要等得起对账，就得把它调短。
    fn with_reconcile_interval(self, reconcile_interval_ms: u64) -> Self {
        Self {
            reconcile_interval_ms,
            ..self
        }
    }
}

/// 余额写回闸门：把 API 钉在"结算已经提交、写回缓存还没回来"那一格（A5 的结算提交后）。
///
/// 只拦 `SET user_balance:…`——限流与路由缓存用别的键前缀，不该被这条闸门拦下。`arm` 之后
/// **下一条**余额写回会被停住并置信号。
///
/// 余额写回走后台队列，所以卡住它**卡不住 HTTP 响应**：用例只能用"提交已经落库"作停点，
/// 不能再声称响应还没交出去。
#[derive(Default)]
struct BalanceWriteGate {
    armed: AtomicBool,
    held: AtomicUsize,
    /// **到达**过的余额写回条数，与有没有武装无关。
    ///
    /// 用例靠它把"写回试过了、但被版本闸门挡下"与"写回根本没发生"分开：只等一段时间再断言
    /// 缓存没变，在后一种情况下也会通过。
    arrived: AtomicUsize,
    notify: tokio::sync::Notify,
}

impl BalanceWriteGate {
    /// 武装：下一条余额写回停住。
    fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }

    /// 到达计数：某次写回已经发到假 Redis 上。
    fn arrivals(&self) -> usize {
        self.arrived.load(Ordering::SeqCst)
    }

    /// 等到达计数越过 `target`。**有上限**：没等到就 panic，不能让用例挂着占住串行测试。
    async fn wait_for_arrivals(&self, target: usize) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if self.arrived.load(Ordering::SeqCst) >= target {
                return;
            }
            let notified = self.notify.notified();
            if self.arrived.load(Ordering::SeqCst) >= target {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "10 秒内没有等到第 {target} 条余额写回到达假 Redis"
            );
            let _ = tokio::time::timeout_at(deadline, notified).await;
        }
    }

    /// 一条落在余额缓存上的命令到了：记一次到达。GET（版本闸门）与 SET（写回）都算——版本被
    /// 挡下时只发生 GET，拿 SET 当到达信号会永远等不到。
    fn arrive(&self) {
        self.arrived.fetch_add(1, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    /// 请求侧：已经武装就原子地卸下并置信号，调用方随后永不应答。
    fn take_hold(&self) -> bool {
        if !self.armed.swap(false, Ordering::SeqCst) {
            return false;
        }
        self.held.fetch_add(1, Ordering::SeqCst);
        self.notify.notify_waiters();
        true
    }

    /// 用例侧：等一条余额写回真的被停住。
    async fn wait_until_held(&self) {
        loop {
            if self.held.load(Ordering::SeqCst) >= 1 {
                return;
            }
            let notified = self.notify.notified();
            if self.held.load(Ordering::SeqCst) >= 1 {
                return;
            }
            notified.await;
        }
    }
}

/// 是不是余额写回（`SET user_balance:…`）。
fn is_balance_write(args: &[String]) -> bool {
    args.first()
        .is_some_and(|name| name.eq_ignore_ascii_case("SET"))
        && args
            .get(1)
            .is_some_and(|key| key.starts_with("user_balance:"))
}

/// 是不是落在余额缓存上的命令：读版本闸门的 `GET` 或写回的 `SET`。
fn is_balance_command(args: &[String]) -> bool {
    args.first()
        .is_some_and(|name| name.eq_ignore_ascii_case("GET") || name.eq_ignore_ascii_case("SET"))
        && args
            .get(1)
            .is_some_and(|key| key.starts_with("user_balance:"))
}

/// 假 Redis 里的一条：值 + 过期时刻（`None` 表示不过期）。
struct CacheEntry {
    value: String,
    expires_at: Option<tokio::time::Instant>,
}

/// 进程内假 Redis：只实现加速层用到的那几条命令。
///
/// 它不是"另一个实现"，而是**测试用的可观测替身**：用例可以读它、改它、关掉它，从而构造
/// "缓存被改错""缓存服务停掉"这两种现实里会发生、但没法靠真实 Redis 稳定复现的情形。
struct CacheFixture {
    url: String,
    settings: CacheSettings,
    state: Arc<Mutex<BTreeMap<String, CacheEntry>>>,
    connections: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    listener: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// 余额写回闸门：强杀矩阵要卡在"结算已提交、缓存写回未回"时用它。
    balance_write_gate: Arc<BalanceWriteGate>,
}

/// 等**余额写穿**落到缓存里：按账户的版本追上数据库那一行为止。
///
/// 写穿走后台队列，返回时它可能还没写完，所以读侧不能假设"接口返回即有缓存"。正常在一毫秒内
/// 到；上限 10 秒，超过说明后台写没跑起来——那是真的坏了，不是慢。
async fn await_write_through(harness: &Harness, account_id: &str) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let database = database_version(harness, account_id).await;
        if let Some(cached) = harness.cache().balance(account_id)
            && cached["version"] == json!(database)
        {
            return cached;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "10 秒内缓存里的版本没有追上数据库那一行"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

impl CacheFixture {
    async fn start(settings: CacheSettings) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake redis binds");
        let port = listener.local_addr().expect("addr").port();
        let state: Arc<Mutex<BTreeMap<String, CacheEntry>>> = Arc::new(Mutex::new(BTreeMap::new()));
        let connections: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>> =
            Arc::new(Mutex::new(Vec::new()));
        let balance_write_gate = Arc::new(BalanceWriteGate::default());
        let handle = {
            let state = state.clone();
            let connections = connections.clone();
            let balance_write_gate = balance_write_gate.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((socket, _)) = listener.accept().await else {
                        break;
                    };
                    let state = state.clone();
                    let gate = balance_write_gate.clone();
                    let served = tokio::spawn(async move {
                        let _ = serve_fake_redis(socket, state, gate).await;
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
            connections,
            listener: Mutex::new(Some(handle)),
            balance_write_gate,
        }
    }

    fn url(&self) -> &str {
        &self.url
    }

    fn settings(&self) -> CacheSettings {
        self.settings
    }

    /// 给**第二个 API 副本**用的缓存句柄：共享同一台假 Redis 的 URL、状态与写入闸门，但不持有
    /// 监听任务——停止与否仍由原夹具决定，第二个副本只借它读写同一台缓存。
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

    /// 把缓存里的余额改成一个错值（写入时间与来源由用例指定）。
    ///
    /// 版本取 0：不高于任何真实账户版本（开户初始充值后就是 0），所以既不会挡住后续写回，也能被对账按数据库校正。要构造
    /// "缓存里的版本比数据库新"那种倒序形态，用 [`Self::put_balance`] 自己给版本。
    fn corrupt_balance(
        &self,
        account_id: &str,
        balance_microusd: i64,
        source: &str,
        written_at: Value,
    ) {
        self.put_balance(
            account_id,
            json!({
                "balance_microusd": balance_microusd,
                "held_microusd": 0,
                "available_microusd": balance_microusd,
                "version": 0,
                "written_at": written_at,
                "source": source,
            }),
        );
    }

    /// 直接写一条完整的余额快照（绕过服务）。
    fn put_balance(&self, account_id: &str, value: Value) {
        self.put(&format!("user_balance:{account_id}"), &value);
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

    /// 武装余额写回闸门：下一条 `SET user_balance:…` 被停住、永不应答。
    ///
    /// 用例要在**受理时的写回已经过去之后**再武装（例如等假上游先收到生成请求），这样被停住的
    /// 就一定是结算之后的那一次写回。
    fn hold_next_balance_write(&self) {
        self.balance_write_gate.arm();
    }

    /// 等那条被停住的余额写回真的到点。到点时结算事务**已经提交**；响应有没有交出去不由它决定。
    async fn wait_for_balance_write_hold(&self) {
        self.balance_write_gate.wait_until_held().await;
    }

    /// 落在余额缓存上的命令条数（`user_balance:` 上的 `GET` 与 `SET` 都算）。
    ///
    /// **它不区分来源**：对账循环的读也算在里面。用它的用例要保证自己的观察窗口远短于对账间隔
    /// （夹具缺省 180 秒），否则对账的读会提前把计数推上去。
    fn balance_write_arrivals(&self) -> usize {
        self.balance_write_gate.arrivals()
    }

    /// 等到余额写回确实到达过（越过 `target` 条）。用它证明"试过了"，而不是"等了一会儿"。
    async fn wait_for_balance_write_arrivals(&self, target: usize) {
        self.balance_write_gate.wait_for_arrivals(target).await;
    }
}

/// 上传存储的全部环境变量：显式移除它们，让"没配"是真的没配（本机 `.env` 或 shell 里可能有）。
const UPLOAD_ENV_NAMES: [&str; 12] = [
    "UPLOAD_STORAGE_REGION",
    "UPLOAD_STORAGE_BUCKET",
    "UPLOAD_STORAGE_ENDPOINT",
    "UPLOAD_STORAGE_ACCESS_KEY_ID",
    "UPLOAD_STORAGE_ACCESS_KEY_SECRET",
    "UPLOAD_MAX_REQUEST_BYTES",
    "UPLOAD_SLOTS",
    "UPLOAD_MAX_BUFFER_BYTES",
    "UPLOAD_REQUEST_TIMEOUT_SECONDS",
    "UPLOAD_SLOW_READ_TIMEOUT_SECONDS",
    "UPLOAD_RETRY_MAX_ATTEMPTS",
    "UPLOAD_RETRY_BACKOFF_BASE_SECONDS",
];

/// 显式清掉代理环境变量：本装置的进程只连回环（假上游、假对象存储），环境里的代理会把
/// "连不上就立刻失败"变成"经代理挂住"，用例因此要等到执行期限才收口。
fn remove_proxy_env(command: &mut Command) {
    for name in [
        "http_proxy",
        "HTTP_PROXY",
        "https_proxy",
        "HTTPS_PROXY",
        "all_proxy",
        "ALL_PROXY",
    ] {
        command.env_remove(name);
    }
}

/// 显式移除上传存储的整组环境变量。
fn remove_upload_env(command: &mut Command) {
    for name in UPLOAD_ENV_NAMES {
        command.env_remove(name);
    }
}

/// 按夹具给上传存储配上环境变量；夹具没有 endpoint 时保持"未配置"。
fn apply_upload_env(command: &mut Command, upload: Option<&UploadStorageFixture>) {
    remove_upload_env(command);
    let Some(upload) = upload else {
        return;
    };
    let Some(endpoint) = upload.endpoint.as_ref() else {
        return;
    };
    command
        .env("UPLOAD_STORAGE_REGION", "cn-hangzhou")
        .env("UPLOAD_STORAGE_BUCKET", UPLOAD_BUCKET)
        .env("UPLOAD_STORAGE_ENDPOINT", endpoint)
        .env("UPLOAD_STORAGE_ACCESS_KEY_ID", UPLOAD_ACCESS_KEY_ID)
        .env("UPLOAD_STORAGE_ACCESS_KEY_SECRET", UPLOAD_ACCESS_KEY_SECRET);
    for (name, value) in [
        (
            "UPLOAD_MAX_REQUEST_BYTES",
            upload.max_request_bytes.map(|value| value.to_string()),
        ),
        ("UPLOAD_SLOTS", upload.slots.map(|value| value.to_string())),
        (
            "UPLOAD_MAX_BUFFER_BYTES",
            upload.max_buffer_bytes.map(|value| value.to_string()),
        ),
        (
            "UPLOAD_SLOW_READ_TIMEOUT_SECONDS",
            upload
                .slow_read_timeout_seconds
                .map(|value| value.to_string()),
        ),
        (
            "UPLOAD_RETRY_MAX_ATTEMPTS",
            upload.retry_max_attempts.map(|value| value.to_string()),
        ),
        (
            "UPLOAD_RETRY_BACKOFF_BASE_SECONDS",
            upload
                .retry_backoff_base_seconds
                .map(|value| value.to_string()),
        ),
    ] {
        if let Some(value) = value {
            command.env(name, value);
        }
    }
}

/// 给子进程装上加速层的那几个环境变量。
fn apply_cache_env(command: &mut Command, cache: Option<&CacheFixture>) {
    let Some(cache) = cache else {
        // **显式**把 `REDIS_URL` 置空来表达"没有缓存"：API 与 Worker 进程用 `dotenvy` 加载仓库
        // `.env`，它不覆盖已设置的变量，而空值在 `RedisCache::from_env` 里判为未配置。
        // 只"不设"的话，本机那份 `.env` 的 `REDIS_URL` 会替无缓存用例补上一个真实 Redis，
        // 跑的不是 CI（无 `.env`）那条路径。
        command.env("REDIS_URL", "");
        return;
    };
    let settings = cache.settings();
    command
        .env("REDIS_URL", cache.url())
        .env(
            "CACHE_BALANCE_TTL_SECONDS",
            settings.balance_ttl_seconds.to_string(),
        )
        .env(
            "CACHE_RECONCILE_INTERVAL_MS",
            settings.reconcile_interval_ms.to_string(),
        )
        .env(
            "CACHE_OPERATION_TIMEOUT_MS",
            settings.operation_timeout_ms.to_string(),
        );
}

/// 假 Redis 的服务循环：读一条 RESP 命令、回一条应答。
async fn serve_fake_redis(
    socket: tokio::net::TcpStream,
    state: Arc<Mutex<BTreeMap<String, CacheEntry>>>,
    balance_write_gate: Arc<BalanceWriteGate>,
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
        if is_balance_command(&args) {
            balance_write_gate.arrive();
        }
        if is_balance_write(&args) && balance_write_gate.take_hold() {
            // 模拟余额写回阻塞：连接保持打开但一条应答都不写。用例把缓存命令上限调长到等得起，
            // 于是后台写回停在这里——此刻结算事务已经提交；响应在另一个任务里，不受它影响。
            std::future::pending::<()>().await;
        }
        let reply = fake_redis_command(&args, &state);
        writer.write_all(reply.as_bytes()).await?;
    }
}

/// 一条命令的应答。只认加速层真正会发的那几条：**认不出来的一律报错**——假服务宽容地回 `+OK`
/// 会让"命令名写错了"这种错误在用例里悄悄通过，而真实 Redis 会直接拒绝它。
///
/// `CLIENT` 要放行：客户端建连接时会发两条 `CLIENT SETINFO`，它们的应答内容没人看。
fn fake_redis_command(args: &[String], state: &Arc<Mutex<BTreeMap<String, CacheEntry>>>) -> String {
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
async fn publish_cache_priced(harness: &Harness) -> StatusCode {
    let client = Client::new();
    republish_priced(harness, &client, openai_floor_amounts(), 2_000).await
}

/// 该账户在**数据库**里的余额（账本是权威）。
async fn database_balance(harness: &Harness, account_id: &str) -> i64 {
    sqlx::query_scalar("SELECT balance_microusd FROM ledger.accounts WHERE id = $1")
        .bind(Uuid::parse_str(account_id).expect("account id"))
        .fetch_one(&harness.pool)
        .await
        .expect("balance")
}

/// 该账户在**数据库**里的金额版本（缓存快照的版本必须与它一致）。
async fn database_version(harness: &Harness, account_id: &str) -> i64 {
    sqlx::query_scalar("SELECT version FROM ledger.accounts WHERE id = $1")
        .bind(Uuid::parse_str(account_id).expect("account id"))
        .fetch_one(&harness.pool)
        .await
        .expect("version")
}

/// 该账户账本条目的**符号和**：账实核对拿它当"账本说是多少"。
async fn ledger_total_microusd(harness: &Harness, account_id: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COALESCE(sum(amount_microusd), 0)::bigint FROM ledger.entries WHERE account_id = $1",
    )
    .bind(Uuid::parse_str(account_id).expect("account id"))
    .fetch_one(&harness.pool)
    .await
    .expect("ledger total")
}

/// 该账户的账本条目数：核对**不许**往账本里补条目，用例据此断言它一条都没写。
async fn ledger_entry_count(harness: &Harness, account_id: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM ledger.entries WHERE account_id = $1")
        .bind(Uuid::parse_str(account_id).expect("account id"))
        .fetch_one(&harness.pool)
        .await
        .expect("ledger entry count")
}

/// 把某个账户的余额**直接改错**：只改这一边，账本条目一条不动——账实不符的那种形态。
async fn forge_account_balance(harness: &Harness, account_id: &str, balance: i64) {
    sqlx::query(
        "UPDATE ledger.accounts SET balance_microusd = $2, updated_at = now() WHERE id = $1",
    )
    .bind(Uuid::parse_str(account_id).expect("account id"))
    .bind(balance)
    .execute(&harness.pool)
    .await
    .expect("forged balance");
}

/// 这个账户当前**未结案**的账户级案例数（`job_id IS NULL` 那一类）。
async fn open_ledger_cases(harness: &Harness, account_id: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM operations.reconciliation_cases
         WHERE account_id = $1 AND job_id IS NULL AND status = 'open'",
    )
    .bind(Uuid::parse_str(account_id).expect("account id"))
    .fetch_one(&harness.pool)
    .await
    .expect("open ledger cases")
}

/// 全部案例的条数（含已结案）：一致时这个数不许动。
async fn reconciliation_case_count(harness: &Harness) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM operations.reconciliation_cases")
        .fetch_one(&harness.pool)
        .await
        .expect("case count")
}

/// 等账实核对在库里留下那条案例（返回案例 id 与它写的理由）。
///
/// 核对在触发之后由后台任务执行，"跑完没有"只能由库里的痕迹回答，所以有上限地等，而不是睡一个
/// 固定时长就断言。
async fn wait_for_open_ledger_case(harness: &Harness, account_id: &str) -> (Uuid, String) {
    for _ in 0..200 {
        let row = sqlx::query(
            "SELECT id, reason FROM operations.reconciliation_cases
             WHERE account_id = $1 AND job_id IS NULL AND status = 'open'",
        )
        .bind(Uuid::parse_str(account_id).expect("account id"))
        .fetch_optional(&harness.pool)
        .await
        .expect("case probe");
        if let Some(row) = row {
            return (
                row.try_get("id").expect("case id"),
                row.try_get("reason").expect("case reason"),
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("按账户触发的账实核对必须为这个账户留下一条案例");
}

/// 触发一次按账户的账实核对（`POST /api/v1/accounts/{account_id}/ledger-audit`）。
///
/// 触发即返回 `202`：核对在后台任务里跑，所以断言要等库里的痕迹，不能紧跟在这条请求后面。
async fn trigger_ledger_audit(harness: &Harness, account_id: &str) {
    let response = Client::new()
        .post(format!(
            "{}/api/v1/accounts/{account_id}/ledger-audit",
            harness.base_url
        ))
        .bearer_auth(&harness.admin_token)
        .send()
        .await
        .expect("trigger the ledger audit");
    assert_eq!(response.status(), 202, "触发只是把核对交给后台");
}

/// 把某个账户的**占用合计**直接改错：active 预授权一行不动——占用那条等式断掉的那种形态。
async fn forge_account_held(harness: &Harness, account_id: &str, held: i64) {
    sqlx::query("UPDATE ledger.accounts SET held_microusd = $2, updated_at = now() WHERE id = $1")
        .bind(Uuid::parse_str(account_id).expect("account id"))
        .bind(held)
        .execute(&harness.pool)
        .await
        .expect("forged held");
}

/// 该账户 active 预授权的金额之和：核对拿它当"明细说是多少"。
async fn holds_total_microusd(harness: &Harness, account_id: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COALESCE(sum(amount_microusd), 0)::bigint FROM ledger.holds
         WHERE account_id = $1 AND status = 'active'",
    )
    .bind(Uuid::parse_str(account_id).expect("account id"))
    .fetch_one(&harness.pool)
    .await
    .expect("holds total")
}

/// 该账户在**数据库**里的占用合计。
async fn database_held(harness: &Harness, account_id: &str) -> i64 {
    sqlx::query_scalar("SELECT held_microusd FROM ledger.accounts WHERE id = $1")
        .bind(Uuid::parse_str(account_id).expect("account id"))
        .fetch_one(&harness.pool)
        .await
        .expect("held")
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

/// 对客响应里**可比对**的那部分：图片项各有哪些字段、是不是本机假上游给的 `url`。
///
/// `url` 里带着假上游每次随机的端口，逐位比不了。比的是响应结构：缓存开着与关掉，
/// 对客拿到的形状必须逐位相同。
fn comparable_response(body: &Value) -> Vec<(Vec<String>, bool)> {
    body["data"]["result"]["images"]
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
                        .as_array()
                        .and_then(|urls| urls.first())
                        .and_then(|url| url.as_str())
                        .is_some_and(|url| url.starts_with("http://127.0.0.1:"));
                    (keys, is_url)
                })
                .collect()
        })
        .unwrap_or_default()
}
