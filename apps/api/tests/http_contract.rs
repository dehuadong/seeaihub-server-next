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

/// APIMart Driver 的真实执行验证。
///
/// 用**进程内假上游**替代真实 Provider：发布一个 `base_url` 指向 `127.0.0.1` 的
/// APIMart Offering，让真实 Worker 跑一次完整流程（提交 → 轮询 → 取图 → 归档 → 结算）。
/// **不产生任何外部调用。**
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn apimart_driver_executes_task_flow_against_local_upstream() {
    let harness = DriverHarness::start(&["prompt_only"], UpstreamBehaviour::default()).await;
    let model = harness.model;
    let key = format!("driver-{}", Uuid::new_v4());
    let (job_id, state) = harness
        .run_job(route_request(model, &key, "driver prompt"))
        .await;
    assert_eq!(state, "succeeded", "the driver flow must settle the job");

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

    // 结果先归档到自有对象存储，Job 只留资产引用。
    let result_assets: Vec<Uuid> =
        sqlx::query_scalar("SELECT result_asset_ids FROM generation.jobs WHERE id = $1")
            .bind(job_id)
            .fetch_one(&harness.pool)
            .await
            .expect("result assets");
    assert_eq!(result_assets.len(), 1, "one generated image must be stored");
    let (media_type, byte_count): (String, i64) = {
        let row = sqlx::query("SELECT media_type, byte_count FROM generation.assets WHERE id = $1")
            .bind(result_assets[0])
            .fetch_one(&harness.pool)
            .await
            .expect("asset row");
        (
            row.try_get("media_type").expect("media type"),
            row.try_get("byte_count").expect("byte count"),
        )
    };
    assert_eq!(media_type, "image/png");
    assert_eq!(byte_count, PNG_FIXTURE.len() as i64);

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
    let submit_body = harness.submit_body();
    assert_eq!(submit_body["model"], model);
    assert_eq!(submit_body["prompt"], "driver prompt");
    assert!(
        submit_body.get("extra").is_none(),
        "APIMart takes parameters at the top level"
    );
    // 不改写原生字段：线上请求体的键，名字与 Profile 声明**逐字相同**，
    // 且不出现 Profile 未声明的字段（含内部包装字段）。
    harness.assert_only_declared_fields();
    assert!(
        harness.count("GET", "/v1/tasks/") >= 1,
        "the driver must poll the task at least once"
    );
    harness.cleanup().await;
}

/// 参考图路径：Driver 必须先把平台资产上传换取公网 URL，再用 URL 组装生成请求。
///
/// 本地资产引用（`asset://…`）是平台内部标识，**绝不能**出现在上行请求里。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn apimart_driver_uploads_reference_images_before_submitting() {
    let harness = DriverHarness::start(
        &["prompt_only", "image_conditioned"],
        UpstreamBehaviour::default(),
    )
    .await;
    let model = harness.model;

    // 先经平台接口上传一张参考图（真实路径，不是直接写库）。
    let asset_id = harness
        .upload_input_asset("image", PNG_FIXTURE.to_vec(), "image/png")
        .await;

    let key = format!("driver-reference-{}", Uuid::new_v4());
    let mut request = route_request(model, &key, "edit this image");
    // 绑定路径用的是该渠道的原生字段名（APIMart 收 `image_urls`）。
    request["asset_bindings"] = json!([
        {"native_parameter_path": "/image_urls/0", "asset_id": asset_id, "position": 0}
    ]);
    let (_, state) = harness.run_job(request).await;
    assert_eq!(state, "succeeded", "the reference-image flow must succeed");

    // 每张参考图上传一次（不是零次、也不是每张多次）。
    assert_eq!(
        harness.count("POST", "/v1/uploads/images"),
        1,
        "each reference image must be uploaded exactly once"
    );
    let submit_body = harness.submit_body();
    let image_urls = submit_body["image_urls"]
        .as_array()
        .expect("image_urls must be an array");
    assert_eq!(image_urls.len(), 1, "one reference image was bound");
    let uploaded_url = image_urls[0]
        .as_str()
        .expect("image_urls entries are strings");
    assert!(
        uploaded_url.starts_with("http://127.0.0.1:"),
        "the generation request must carry the uploaded public URL, got {uploaded_url}"
    );
    // 带图请求同理：上线字段必须全是 Profile 声明过的。
    harness.assert_only_declared_fields();
    for (_, _, body) in harness.recorded() {
        assert!(
            !body.contains("asset://"),
            "a local asset reference leaked onto the wire: {body}"
        );
    }
    harness.cleanup().await;
}

/// 遮罩路径（`mask_url`）与参考图一样要先上传，且两者必须各就各位。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn apimart_driver_uploads_reference_image_and_mask_together() {
    let harness = DriverHarness::start(
        &["prompt_only", "image_conditioned", "masked"],
        UpstreamBehaviour::default(),
    )
    .await;
    let model = harness.model;

    // 尺寸必须一致，否则平台在受理期就会拒绝（遮罩尺寸须与输入图相同）。
    let image_id = harness
        .upload_input_asset("image", PNG_FIXTURE.to_vec(), "image/png")
        .await;
    let mask_id = harness
        .upload_input_asset("mask", PNG_FIXTURE.to_vec(), "image/png")
        .await;

    let key = format!("driver-mask-{}", Uuid::new_v4());
    let mut request = route_request(model, &key, "masked edit");
    request["asset_bindings"] = json!([
        {"native_parameter_path": "/image_urls/0", "asset_id": image_id, "position": 0},
        {"native_parameter_path": "/mask_url", "asset_id": mask_id, "position": 0}
    ]);
    let (_, state) = harness.run_job(request).await;
    assert_eq!(state, "succeeded", "the masked edit must succeed");

    // 两张图各上传一次。
    assert_eq!(
        harness.count("POST", "/v1/uploads/images"),
        2,
        "the reference image and the mask must each be uploaded once"
    );
    let submit_body = harness.submit_body();
    let image_urls = submit_body["image_urls"]
        .as_array()
        .expect("image_urls must be an array");
    assert_eq!(image_urls.len(), 1, "one reference image was bound");
    let mask_url = submit_body["mask_url"]
        .as_str()
        .expect("mask_url must be a string");
    assert!(
        mask_url.starts_with("http://127.0.0.1:"),
        "the mask must travel as an uploaded public URL, got {mask_url}"
    );
    assert_ne!(
        mask_url,
        image_urls[0].as_str().unwrap_or_default(),
        "the mask and the reference image are different uploads"
    );
    harness.assert_only_declared_fields();
    harness.cleanup().await;
}

/// 上传失败 = 生成任务**可证明未受理**：Job 走失败、预授权释放，不进对账。
///
/// 这与"提交之后出错进对账"是两条路径，不能混为一谈。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn upload_failure_fails_the_job_instead_of_asking_for_reconciliation() {
    let behaviour = UpstreamBehaviour {
        upload_failure_status: 400,
        ..UpstreamBehaviour::default()
    };
    let harness = DriverHarness::start(&["prompt_only", "image_conditioned"], behaviour).await;
    let model = harness.model;
    let asset_id = harness
        .upload_input_asset("image", PNG_FIXTURE.to_vec(), "image/png")
        .await;

    let key = format!("driver-upload-failure-{}", Uuid::new_v4());
    let mut request = route_request(model, &key, "edit this image");
    request["asset_bindings"] = json!([
        {"native_parameter_path": "/image_urls/0", "asset_id": asset_id, "position": 0}
    ]);
    let (job_id, state) = harness.run_job(request).await;
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

struct FakeUpstream {
    base_url: String,
    _handle: tokio::task::JoinHandle<()>,
}

/// 假上游的可配置行为，用于构造"查询瞬时失败重试"与"未知状态继续轮询"两类场景。
#[derive(Default, Clone)]
struct UpstreamBehaviour {
    /// 任务查询先失败这么多次（返回 500），之后才给正常响应 —— 覆盖"查询可重试"。
    query_failures: usize,
    /// 任务查询先返回这么多次未在文档中出现的状态，之后才 completed —— 覆盖"未知状态继续轮询"。
    unknown_status_times: usize,
    /// 非 0 时，资产上传接口固定返回这个错误状态码 —— 覆盖"上传失败即确定未受理"。
    upload_failure_status: u16,
    /// 提交生成请求时上游的应答方式。
    submit: SubmitBehaviour,
}

/// 提交生成请求时上游的应答方式。
#[derive(Clone, Default)]
enum SubmitBehaviour {
    /// 受理成功并返回任务 id（异步上游）。
    #[default]
    Accepted,
    /// 直接以这个状态码与错误体拒绝：欠费、凭证、参数错误、限流等。
    Rejected { status: u16, body: Value },
}

async fn start_fake_upstream_with(
    calls: UpstreamCalls,
    behaviour: UpstreamBehaviour,
) -> FakeUpstream {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fake upstream binds");
    let port = listener.local_addr().expect("addr").port();
    // 查询行为按调用次数推进：第 n 次查询按 behaviour 决定失败/未知状态/正常。
    let query_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let upload_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
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
    query_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    upload_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
) -> std::io::Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    // 正经读完一个请求：请求行 + 头 + 按 Content-Length 读满请求体。
    // 不能只 read 一次就假设整条请求都到了——网络会把请求拆成几段，
    // 那样断言请求体的测试会偶发失败（看起来像代码错，其实是测试不稳）。
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
        (method, path, String::from_utf8_lossy(&body).to_string())
    };
    if let Ok(mut calls) = calls.lock() {
        calls.push((method.clone(), path.clone(), body));
    }

    // 资产上传：参考图/遮罩先换公网 URL。这个分支必须在生成分支之前判断，
    // 而且它的失败**不**代表"生成可能已发生"——生成任务此时还没提交。
    if method == "POST" && path == "/v1/uploads/images" {
        let index = upload_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        if behaviour.upload_failure_status != 0 {
            // 上游上传失败的错误体只有 type 与 message，**没有** error.code。
            let payload = serde_json::to_vec(&json!({
                "error": {"type": "invalid_request_error", "message": "unsupported image type"}
            }))
            .expect("upload failure body");
            let head = format!(
                "HTTP/1.1 {} Error\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                behaviour.upload_failure_status,
                payload.len()
            );
            socket.write_all(head.as_bytes()).await?;
            socket.write_all(&payload).await?;
            socket.flush().await?;
            return Ok(());
        }
        let payload = serde_json::to_vec(&json!({
            "url": format!("http://127.0.0.1:{}/uploaded-{index}.png", port_of(socket)),
            "filename": format!("asset-{index}.png"),
            "content_type": "image/png",
            "bytes": PNG_FIXTURE.len(),
            "created_at": 1_790_000_000u64
        }))
        .expect("upload body");
        let head = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            payload.len()
        );
        socket.write_all(head.as_bytes()).await?;
        socket.write_all(&payload).await?;
        socket.flush().await?;
        return Ok(());
    }

    // 提交生成请求被上游直接拒：欠费、凭证、参数错误、限流都从这里进。
    if method == "POST"
        && path.ends_with("/images/generations")
        && let SubmitBehaviour::Rejected { status, body } = &behaviour.submit
    {
        let payload = serde_json::to_vec(body).expect("rejection body");
        let head = format!(
            "HTTP/1.1 {status} Error\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            payload.len()
        );
        socket.write_all(head.as_bytes()).await?;
        socket.write_all(&payload).await?;
        socket.flush().await?;
        return Ok(());
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
        // 第 n 次查询（1-based），用于构造瞬时失败与未知状态。
        let attempt = query_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        if attempt <= behaviour.query_failures {
            // 瞬时失败：上游 500。查询是幂等读，Driver 应重试而不是让 Job 失败。
            let payload = serde_json::to_vec(&json!({
                "error": {"code": 500, "message": "temporary upstream failure"}
            }))
            .expect("failure body");
            let head = format!(
                "HTTP/1.1 500 Internal Server Error\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                payload.len()
            );
            socket.write_all(head.as_bytes()).await?;
            socket.write_all(&payload).await?;
            socket.flush().await?;
            return Ok(());
        }
        let status = if attempt <= behaviour.query_failures + behaviour.unknown_status_times {
            // 未在文档中出现的状态值：Driver 必须继续轮询，不得当失败。
            "queued_somewhere_new"
        } else {
            "completed"
        };
        (
            "application/json",
            serde_json::to_vec(&json!({
                "code": 200,
                "data": {
                    "id": "task-contract-1",
                    "status": status,
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

/// 一个可用的平台 API 进程：`Drop` 时结束它并清掉资产目录。
fn start_api(database_url: &str, asset_root: &std::path::Path) -> (String, String, ApiProcess) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("test port should bind");
    let port = listener.local_addr().expect("test address").port();
    drop(listener);
    let base_url = format!("http://127.0.0.1:{port}");
    let admin_token = format!("driver-admin-{}", Uuid::new_v4());
    let child = Command::new(env!("CARGO_BIN_EXE_seeai-api"))
        .env("DATABASE_URL", database_url)
        .env("API_BIND", format!("127.0.0.1:{port}"))
        .env("ADMIN_TOKEN", &admin_token)
        .env("ASSET_STORE", "local")
        .env("ASSET_LOCAL_ROOT", asset_root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("API process should start");
    let process = ApiProcess {
        child,
        asset_root: asset_root.to_path_buf(),
    };
    (base_url, admin_token, process)
}

/// 驱动端到端验证的公共装置。
///
/// 起一个**进程内假上游**与一个指向它的 API 进程，发布一条 APIMart 供给，
/// 并在需要时用真实 Worker 把 Job 跑到终态。全程不产生任何外部调用。
struct DriverHarness {
    model: &'static str,
    database_url: String,
    database_name: String,
    base_url: String,
    admin_token: String,
    api_key: String,
    pool: PgPool,
    calls: UpstreamCalls,
    asset_root: PathBuf,
    /// 保持 API 进程存活；丢弃即结束它。
    _api: ApiProcess,
    /// 保持假上游的监听任务存活。
    _upstream: FakeUpstream,
}

impl DriverHarness {
    const MODEL: &'static str = "driver-model";

    /// 起装置并发布一条供给。`branches` 决定 Profile 声明哪些入口。
    async fn start(branches: &[&str], behaviour: UpstreamBehaviour) -> Self {
        Self::start_with_provider("APIMart", "apimart-image-v1", branches, behaviour).await
    }

    /// 同 `start`，但指定渠道方与适配器 —— 两个渠道的错误码与应答形状不同，需要分别覆盖。
    async fn start_with_provider(
        provider_kind: &str,
        adapter_key: &str,
        branches: &[&str],
        behaviour: UpstreamBehaviour,
    ) -> Self {
        let (database_url, database_name) = isolated_database_url().await;
        let calls: UpstreamCalls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let upstream = start_fake_upstream_with(calls.clone(), behaviour).await;
        let asset_root = std::env::temp_dir().join(format!("seeai-driver-{}", Uuid::new_v4()));
        let (base_url, admin_token, process) = start_api(&database_url, &asset_root);
        let client = Client::new();
        wait_until_ready(&client, &base_url).await;
        let account = create_account(&client, &base_url, &admin_token).await;
        let api_key = issue_key(&client, &base_url, &admin_token, &account).await;
        let pool = PgPool::connect(&database_url)
            .await
            .expect("contract database");

        let mut draft = candidate(provider_kind, adapter_key, branches);
        draft["base_url"] = Value::String(upstream.base_url.clone());
        let credential_env = match provider_kind {
            "AIHubMix" => "AIHUBMIX_API_KEY",
            _ => "APIMART_API_KEY",
        };
        draft["credential_env"] = Value::String(credential_env.to_owned());
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
            asset_root,
            _api: process,
            _upstream: upstream,
        }
    }

    /// 通过平台接口上传一个输入资产，返回它的 id。
    ///
    /// 走真实 HTTP 路径而不是直接写库：输入资产的解析结果（尺寸、角色）
    /// 正是创建 Job 时被校验的东西。
    async fn upload_input_asset(&self, role: &str, bytes: Vec<u8>, media_type: &str) -> Uuid {
        let response = Client::new()
            .post(format!("{}/v1/assets", self.base_url))
            .bearer_auth(&self.api_key)
            .header("content-type", media_type)
            .header("x-asset-role", role)
            .body(bytes)
            .send()
            .await
            .expect("input asset upload");
        assert_eq!(
            response.status(),
            StatusCode::CREATED,
            "input asset upload must succeed"
        );
        let asset: Value = response.json().await.expect("asset JSON");
        Uuid::parse_str(asset["id"].as_str().expect("asset id")).expect("asset UUID")
    }

    /// 受理一个 Job（尚未启动 Worker），返回它的 id。
    async fn accept(&self, request: Value) -> Uuid {
        let created = Client::new()
            .post(format!("{}/v1/image-generations", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&request)
            .send()
            .await
            .expect("generation accepted");
        let status = created.status();
        let body = created.text().await.expect("job JSON");
        assert_eq!(status, StatusCode::ACCEPTED, "generation rejected: {body}");
        let created: Value = serde_json::from_str(&body).expect("job JSON");
        Uuid::parse_str(created["job_id"].as_str().expect("job id")).expect("job UUID")
    }

    /// 起真实 Worker 并把 Job 跑到终态，返回 (job_id, 终态)。
    async fn run_job(&self, request: Value) -> (Uuid, String) {
        let job_id = self.accept(request).await;
        // Worker 是**独立包**的二进制，按当前 profile 定位（见 `worker_binary`）。
        let mut worker = Command::new(worker_binary())
            .env("DATABASE_URL", &self.database_url)
            .env("WORKER_ID", "driver-contract-worker")
            .env("WORKER_POLL_INTERVAL_MS", "200")
            .env("WORKER_LEASE_SECONDS", "300")
            .env("PROVIDER_TIMEOUT_SECONDS", "60")
            .env("ASSET_STORE", "local")
            .env("ASSET_LOCAL_ROOT", &self.asset_root)
            .env("APIMART_API_KEY", "contract-test-key")
            .env("AIHUBMIX_API_KEY", "contract-test-key")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("worker process should start");

        let mut state = String::new();
        for _ in 0..600 {
            tokio::time::sleep(Duration::from_millis(200)).await;
            state = sqlx::query_scalar("SELECT state FROM generation.jobs WHERE id = $1")
                .bind(job_id)
                .fetch_one(&self.pool)
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
        (job_id, state)
    }

    /// 假上游记录下来的请求（方法、路径、请求体）。
    fn recorded(&self) -> Vec<(String, String, String)> {
        self.calls.lock().expect("calls lock").clone()
    }

    /// 记录下来的生成请求体。
    fn submit_body(&self) -> Value {
        let raw = self
            .recorded()
            .into_iter()
            .find(|(method, path, _)| method == "POST" && path == "/v1/images/generations")
            .map(|(_, _, body)| body)
            .expect("the driver must submit a generation request");
        serde_json::from_str(&raw).expect("submit body is JSON")
    }

    fn count(&self, method: &str, path_prefix: &str) -> usize {
        self.recorded()
            .iter()
            .filter(|(call_method, path, _)| call_method == method && path.starts_with(path_prefix))
            .count()
    }

    /// 线上请求体里不允许出现 Profile 未声明的字段（含内部包装字段）。
    ///
    /// 校验对象是 `config/bootstrap/apimart-gpt-image-2.5-flare.json` 里那份**真实** Profile：
    /// 契约要求请求体的键名与 Profile 声明逐字相同。
    fn assert_only_declared_fields(&self) {
        let material: Value = serde_json::from_str(include_str!(
            "../../../config/bootstrap/apimart-gpt-image-2.5-flare.json"
        ))
        .expect("APIMart material parses");
        let declared = material["offerings"][0]["capability_schema"]["properties"]
            .as_object()
            .expect("declared properties");
        let submit_body = self.submit_body();
        let sent = submit_body.as_object().expect("sent body object");
        for name in sent.keys() {
            assert!(
                declared.contains_key(name),
                "the driver sent `{name}`, which the profile does not declare"
            );
        }
        for name in ["model", "prompt"] {
            assert!(
                sent.contains_key(name),
                "the driver must send `{name}` verbatim"
            );
        }
    }

    async fn cleanup(&self) {
        // 先放掉自己的连接，再去删库：否则 DROP 只能靠 `WITH (FORCE)` 强踢，
        // 偶尔会留下一次性库。
        self.pool.close().await;
        drop_isolated_database(&self.database_name).await;
    }
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
        .post(format!("{base_url}/api/v1/accounts"))
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
    verify_lease_recovery_contract(&client, &base_url, &admin_token, &api_key, &database_url).await;
    drop_isolated_database(&database_name).await;
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

    // ── 任务创建失败时，判定记录与 Job 两边都不留下 ──
    // 预授权上限超过账户余额会让 create_job 在写入前失败（扣不动预授权），
    // 此时不该留下判定记录，也不该留下 Job。
    let decisions_before: i64 =
        sqlx::query_scalar("SELECT count(*) FROM generation.routing_decisions")
            .fetch_one(&pool)
            .await
            .expect("decision count");
    let jobs_before: i64 = sqlx::query_scalar("SELECT count(*) FROM generation.jobs")
        .fetch_one(&pool)
        .await
        .expect("job count");
    let mut over_budget =
        route_request(model, &format!("doomed-{}", Uuid::new_v4()), "over budget");
    over_budget["max_cost_microusd"] = json!(10_000_000);
    let doomed = client
        .post(format!("{base_url}/v1/image-generations"))
        .bearer_auth(&api_key)
        .json(&over_budget)
        .send()
        .await
        .expect("over-budget request");
    assert!(
        doomed.status().is_client_error(),
        "an unaffordable request must be rejected, got {}",
        doomed.status()
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
    // 同一个幂等键重放会返回同一个 Job，判定记录也应只有一条（它反映"受理时"的判定）。
    let replay_key = format!("replay-{}", Uuid::new_v4());
    let replay_request = route_request(model, &replay_key, "replayed");
    let first = client
        .post(format!("{base_url}/v1/image-generations"))
        .bearer_auth(&api_key)
        .json(&replay_request)
        .send()
        .await
        .expect("first attempt");
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    let replay = client
        .post(format!("{base_url}/v1/image-generations"))
        .bearer_auth(&api_key)
        .json(&replay_request)
        .send()
        .await
        .expect("replay");
    assert_eq!(replay.status(), StatusCode::ACCEPTED);
    let replay_job = Uuid::parse_str(
        replay.json::<Value>().await.expect("replay JSON")["job_id"]
            .as_str()
            .expect("job id"),
    )
    .expect("job UUID");
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

/// 第二阶段的**发布素材**要真的能用，并且每个候选要带自己的那份 Profile
/// ——缺一不可：素材发不出去、或候选没带上自己的 Profile，都算没覆盖。
///
/// 用 `config/bootstrap/` 里已备好的 2.5 素材发布：AIHubMix 与 APIMart 供同一型号。
/// 两家 Profile 内容不同（AIHubMix 有 `extra`，APIMart 参数在顶层），正是"候选各自携带
/// Profile"要覆盖的情形。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn stage_two_bootstrap_material_publishes_with_per_candidate_profiles() {
    let (database_url, database_name) = isolated_database_url().await;
    let listener = TcpListener::bind("127.0.0.1:0").expect("test port should bind");
    let port = listener.local_addr().expect("test address").port();
    drop(listener);
    let base_url = format!("http://127.0.0.1:{port}");
    let admin_token = format!("material-admin-{}", Uuid::new_v4());
    let asset_root = std::env::temp_dir().join(format!("seeai-material-{}", Uuid::new_v4()));
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
    // 两家的 Profile 内容必须真的不同——否则这个用例覆盖不到"各自携带 Profile"。
    let aihubmix_schema = &aihubmix["offerings"][0]["capability_schema"];
    let apimart_schema = &apimart["offerings"][0]["capability_schema"];
    assert_ne!(
        aihubmix_schema, apimart_schema,
        "this test only covers condition 22 if the two profiles actually differ"
    );

    // 合并成一次发布：两个候选，顺序即优先级。
    let merged = json!({
        "vendor_id": aihubmix["vendor_id"],
        "native_model_id": aihubmix["native_model_id"],
        "native_revision": "stage-two-material-1",
        "actor": "contract-test",
        "offerings": [aihubmix["offerings"][0], apimart["offerings"][0]]
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

    // 每个候选携带**它自己**的那份 Profile——读回来的 schema 要分别等于
    // 发布时给各自的那一份，而不是共享同一份。
    let rows = sqlx::query(
        "SELECT o.provider_model_id, o.adapter_key, c.provider_kind, vm.capability_schema
         FROM publication.runtime_entries re
         JOIN supply.offerings o ON o.id = re.offering_id
         JOIN supply.channels c ON c.id = o.channel_id
         JOIN catalog.vendor_models vm ON vm.id = re.vendor_model_id
         WHERE re.active AND re.native_model_id = $1
         ORDER BY re.routing_priority",
    )
    .bind(&model)
    .fetch_all(&pool)
    .await
    .expect("candidate rows");
    assert_eq!(rows.len(), 2, "both providers must be active candidates");

    let schemas: Vec<Value> = rows
        .iter()
        .map(|row| row.try_get("capability_schema").expect("schema"))
        .collect();
    assert_ne!(
        schemas[0], schemas[1],
        "each candidate must carry its own profile, not a shared one"
    );
    // 每个候选的 provider_model_id / adapter_key 与它自己 Profile 的 model.const 一致。
    for (index, expected) in [(0_usize, &aihubmix), (1, &apimart)].iter() {
        let row = &rows[*index];
        let offering = &expected["offerings"][0];
        let provider_model_id: String = row.try_get("provider_model_id").expect("provider model");
        let adapter_key: String = row.try_get("adapter_key").expect("adapter key");
        assert_eq!(provider_model_id, offering["provider_model_id"]);
        assert_eq!(adapter_key, offering["adapter_key"]);
        assert_eq!(
            schemas[*index], offering["capability_schema"],
            "candidate {index} must carry its own profile"
        );
        assert_eq!(
            schemas[*index]["properties"]["model"]["const"], model,
            "each profile's model.const must equal the vendor model identity"
        );
    }

    drop_isolated_database(&database_name).await;
}

/// 任务**查询**的瞬时失败可以重试，Job 最终仍成功。
///
/// 查询是幂等读，重试它不会造成重复副作用；这与"创建请求绝不重发"并不冲突。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn transient_query_failure_is_retried_and_the_job_still_succeeds() {
    let behaviour = UpstreamBehaviour {
        query_failures: 1,
        unknown_status_times: 0,
        ..UpstreamBehaviour::default()
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
        query_failures: 0,
        unknown_status_times: 1,
        ..UpstreamBehaviour::default()
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
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn post_acceptance_failure_keeps_the_task_id_for_reconciliation() {
    // 查询永远 500：有界重试耗尽后仍失败 ⇒ 已确认生成、但取不到结果 ⇒ 对账。
    let behaviour = UpstreamBehaviour {
        query_failures: 99,
        ..UpstreamBehaviour::default()
    };
    let outcome = run_driver_attempt(behaviour).await;
    assert_eq!(
        outcome.job_state, "reconciliation_required",
        "a post-acceptance failure must go to reconciliation"
    );
    assert_eq!(
        outcome.submits, 1,
        "the create request must never be resent, not even for reconciliation"
    );

    let trace_id: Option<String> =
        sqlx::query_scalar("SELECT provider_trace_id FROM generation.attempts WHERE job_id = $1")
            .bind(outcome.job_id)
            .fetch_one(&outcome.harness.pool)
            .await
            .expect("attempt row");
    assert_eq!(
        trace_id.as_deref(),
        Some("task-contract-1"),
        "the upstream task id must survive into the attempt so a human can look it up"
    );

    // 而且它必须能从**对账列表接口**看到，而不是只能翻数据库。
    let cases: Value = Client::new()
        .get(format!(
            "{}/api/v1/reconciliation-cases",
            outcome.harness.base_url
        ))
        .bearer_auth(&outcome.harness.admin_token)
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
    outcome.harness.cleanup().await;
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
            ..UpstreamBehaviour::default()
        };
        let harness = DriverHarness::start_with_provider(
            provider_kind,
            adapter_key,
            &["prompt_only"],
            behaviour,
        )
        .await;
        let key = format!("rejected-{status}-{}", Uuid::new_v4());
        let (job_id, state) = harness
            .run_job(route_request(harness.model, &key, "rejected prompt"))
            .await;
        assert_eq!(
            state, expected_state,
            "{provider_kind} 的 {status} 必须落在 {expected_state}"
        );

        // 消费者面：只有平台码，没有任何渠道字样。
        let view: Value = Client::new()
            .get(format!(
                "{}/v1/image-generations/{job_id}",
                harness.base_url
            ))
            .bearer_auth(&harness.api_key)
            .send()
            .await
            .expect("job view")
            .json()
            .await
            .expect("job view JSON");
        assert_eq!(
            view["error_code"].as_str(),
            Some(expected_code),
            "消费者看到的对客码不对：{view}"
        );
        let rendered = view.to_string();
        assert!(
            !rendered.contains(channel_message),
            "渠道原文不得出现在消费者面：{rendered}"
        );
        assert!(
            !rendered.contains(channel_code),
            "渠道码不得出现在消费者面：{rendered}"
        );
        assert!(
            !rendered.contains(UPSTREAM_TRACE_ID),
            "上游逐请求标识不得出现在消费者面：{rendered}"
        );

        // 终态与预授权必须一致：判成确定失败就释放预授权，进对账就继续握着。
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

        // 内部记录保留渠道原始码、原文与上游标识：出问题时人要能拿去上游核对。
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

struct DriverOutcome {
    job_state: String,
    job_id: Uuid,
    submits: usize,
    polls: usize,
    harness: DriverHarness,
}

/// 起 API + 假上游 + 真实 Worker，让一个文生图 Job 走完整个驱动流程，返回它的结局。
async fn run_driver_attempt(behaviour: UpstreamBehaviour) -> DriverOutcome {
    let harness = DriverHarness::start(&["prompt_only"], behaviour).await;
    let key = format!("driver-{}", Uuid::new_v4());
    let (job_id, job_state) = harness
        .run_job(route_request(harness.model, &key, "driver prompt"))
        .await;
    DriverOutcome {
        job_state,
        job_id,
        submits: harness.count("POST", "/v1/images/generations"),
        polls: harness.count("GET", "/v1/tasks/"),
        harness,
    }
}

fn candidate(provider_kind: &str, adapter_key: &str, branches: &[&str]) -> Value {
    // Profile 必须与声明的分支自洽：声明 `image_conditioned` 就要有参考图字段，
    // 声明 `masked` 就要有遮罩字段。发布期会拒绝不自洽的声明（限制只能收窄）。
    let mut properties = json!({
        "model": {"const": "placeholder"},
        "prompt": {"type": "string", "minLength": 1}
    });
    let declares_image = branches
        .iter()
        .any(|branch| matches!(*branch, "image_conditioned" | "masked"));
    if declares_image {
        // 字段名必须落在 Adapter 声明的参数面里（APIMart 收的是 `image_urls`，
        // 没有 `image` 这个顶层参数），否则发布期会以"不支持该原生参数"拒绝。
        properties["image_urls"] = json!({
            "type": "array",
            "items": {"type": "string"},
            "minItems": 1,
            "maxItems": 1
        });
    }
    if branches.contains(&"masked") {
        properties["mask_url"] = json!({"type": "string"});
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
        "capability_schema": {
            "type": "object",
            "additionalProperties": false,
            "required": ["model", "prompt"],
            "properties": properties
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
        .post(format!("{base_url}/api/v1/runtime-revisions"))
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
        .post(format!("{base_url}/api/v1/accounts"))
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
}

async fn verify_lease_recovery_contract(
    client: &Client,
    base_url: &str,
    admin_token: &str,
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
}
