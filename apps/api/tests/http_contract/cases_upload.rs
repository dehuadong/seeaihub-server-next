//! 上传端点（POST /v1/uploads/images）：媒体与上限、对象键、写入与核验、失败分类与对客码。
//!
//! 全部用例只打**进程内假对象存储**与假上游，不访问真实 OSS；上传不计费、不计量、不建执行记录。

use super::*;

/// 20 MiB 单文件上限（严格小于）。
const MAX_SINGLE_FILE_BYTES: usize = 20 * 1024 * 1024;
/// 上传路由的请求体上限缺省值（21 MiB）。
const UPLOAD_REQUEST_BYTES: usize = 21 * 1024 * 1024;

/// 一个文件部件；`content_type` 为 `None` 时不给该部件声明 `Content-Type`。
fn file_part(
    bytes: &[u8],
    file_name: &str,
    content_type: Option<&str>,
) -> reqwest::multipart::Part {
    let part = reqwest::multipart::Part::bytes(bytes.to_vec()).file_name(file_name.to_owned());
    match content_type {
        Some(content_type) => part.mime_str(content_type).expect("a valid media type"),
        None => part,
    }
}

/// 一段以 PNG 魔数开头的字节：内容判型只看魔数。
fn png_of_length(length: usize) -> Vec<u8> {
    let mut bytes = vec![0_u8; length];
    bytes[0..8].copy_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    bytes
}

/// 在独立任务里发一次上传请求（A6 的 upload_busy 要一条在飞请求占住名额）。
async fn upload_request(base_url: String, api_key: String, bytes: Vec<u8>) -> (StatusCode, Value) {
    let form = reqwest::multipart::Form::new()
        .part("file", file_part(&bytes, "gated.png", Some("image/png")));
    let response = reqwest::Client::new()
        .post(format!("{base_url}/v1/uploads/images"))
        .bearer_auth(api_key)
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

/// 解析一段原始 HTTP 响应为状态码与 JSON 体。
fn parse_http_response(response: &[u8]) -> (StatusCode, Value) {
    let text = String::from_utf8_lossy(response);
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text.as_ref(), ""));
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(0);
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        serde_json::from_str(body).unwrap_or(Value::String(body.to_owned())),
    )
}

/// 发一条只声明了超大 Content-Length、不发正文的原始请求：声明的上限必须在零正文读取时被拒。
async fn raw_upload_with_declared_length(
    base_url: &str,
    api_key: &str,
    content_length: usize,
) -> (StatusCode, Value) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let addr = base_url.trim_start_matches("http://").to_owned();
    let mut socket = tokio::net::TcpStream::connect(&addr)
        .await
        .expect("connect to the API");
    let head = format!(
        "POST /v1/uploads/images HTTP/1.1\r\nhost: {addr}\r\nauthorization: Bearer {api_key}\r\ncontent-type: multipart/form-data; boundary=probe\r\ncontent-length: {content_length}\r\nconnection: close\r\n\r\n"
    );
    socket.write_all(head.as_bytes()).await.expect("write head");
    let mut response = Vec::new();
    socket
        .read_to_end(&mut response)
        .await
        .expect("read response");
    parse_http_response(&response)
}

/// 发一条滴流正文的原始请求：正文慢读超时在受理前按 408 收口，且不写对象。
async fn slow_upload_request(base_url: &str, api_key: &str) -> (StatusCode, Value) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let addr = base_url.trim_start_matches("http://").to_owned();
    let mut socket = tokio::net::TcpStream::connect(&addr)
        .await
        .expect("connect to the API");
    let head = format!(
        "POST /v1/uploads/images HTTP/1.1\r\nhost: {addr}\r\nauthorization: Bearer {api_key}\r\ncontent-type: multipart/form-data; boundary=probe\r\ncontent-length: 100\r\nconnection: close\r\n\r\n"
    );
    socket.write_all(head.as_bytes()).await.expect("write head");
    socket
        .write_all(b"--probe\r\n")
        .await
        .expect("write partial body");
    socket.flush().await.expect("flush");
    // 拖过 1 秒的慢读上限：服务端在受理前终止，回 408。
    tokio::time::sleep(std::time::Duration::from_millis(2_000)).await;
    let mut response = Vec::new();
    socket
        .read_to_end(&mut response)
        .await
        .expect("read response");
    parse_http_response(&response)
}

/// 分块编码的一个 HTTP 块。
async fn write_chunk(socket: &mut tokio::net::TcpStream, bytes: &[u8]) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    socket
        .write_all(format!("{:x}\r\n", bytes.len()).as_bytes())
        .await?;
    socket.write_all(bytes).await?;
    socket.write_all(b"\r\n").await
}

/// 发一条分块编码、总正文超过上传路由上限的原始请求：流式读到上限时同样回平台信封 413。
async fn chunked_over_limit_upload(base_url: &str, api_key: &str) -> (StatusCode, Value) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let addr = base_url.trim_start_matches("http://").to_owned();
    let mut socket = tokio::net::TcpStream::connect(&addr)
        .await
        .expect("connect to the API");
    let head = format!(
        "POST /v1/uploads/images HTTP/1.1\r\nhost: {addr}\r\nauthorization: Bearer {api_key}\r\ncontent-type: multipart/form-data; boundary=probe\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n"
    );
    socket.write_all(head.as_bytes()).await.expect("write head");
    // 合法的 multipart 前导加一个超上限的 file 部件；分块发送，服务端读到路由上限即终止。
    let preamble = b"--probe\r\nContent-Disposition: form-data; name=\"file\"; filename=\"big.png\"\r\nContent-Type: image/png\r\n\r\n";
    let mut written = 0_usize;
    if write_chunk(&mut socket, preamble).await.is_ok() {
        written += preamble.len();
    }
    let chunk = vec![0x89_u8; 64 * 1024];
    while written < UPLOAD_REQUEST_BYTES + 1024 * 1024 {
        if write_chunk(&mut socket, &chunk).await.is_err() {
            // 服务端在上限处终止了连接：不再写，直接去读响应。
            break;
        }
        written += chunk.len();
    }
    let _ = write_chunk(&mut socket, b"").await;
    let _ = socket.flush().await;
    let mut response = Vec::new();
    let _ = socket.read_to_end(&mut response).await;
    parse_http_response(&response)
}

/// 读一次账户余额与占用。
async fn balance_snapshot(harness: &Harness) -> (i64, i64) {
    let body = reqwest::Client::new()
        .get(format!(
            "{}/api/v1/accounts/{}",
            harness.base_url, harness.account_id
        ))
        .bearer_auth(&harness.admin_token)
        .send()
        .await
        .expect("balance request")
        .json::<Value>()
        .await
        .expect("balance body");
    (
        body["balance_microusd"].as_i64().expect("balance"),
        body["held_microusd"].as_i64().expect("held"),
    )
}

async fn account_uuid(harness: &Harness) -> Uuid {
    Uuid::parse_str(&harness.account_id).expect("account id")
}

async fn ledger_entry_count(harness: &Harness) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM ledger.entries WHERE account_id = $1")
        .bind(account_uuid(harness).await)
        .fetch_one(&harness.pool)
        .await
        .expect("ledger entry count")
}

async fn job_count(harness: &Harness) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM generation.jobs WHERE account_id = $1")
        .bind(account_uuid(harness).await)
        .fetch_one(&harness.pool)
        .await
        .expect("job count")
}

fn upload_storage() -> UploadStorageFixture {
    UploadStorageFixture::with_behaviour(ObjectStorageBehaviour::default())
}

fn upload_error_code(body: &Value) -> String {
    body["error"]["code"]
        .as_str()
        .unwrap_or_else(|| panic!("an error envelope with a code is expected: {body}"))
        .to_owned()
}

/// A1：整组上传存储变量都不给时上传回 503 upload_storage_unavailable，进程照常启动。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn upload_storage_absent_returns_503_and_the_process_still_starts() {
    let harness = Harness::start(UpstreamBehaviour::apimart()).await;
    let (status, body) = harness
        .upload(file_part(PNG_FIXTURE, "probe.png", Some("image/png")))
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(upload_error_code(&body), "upload_storage_unavailable");
    harness.cleanup().await;
}

/// A1：只给一部分或形状不合法时进程在启动期被拒并点名变量。
#[tokio::test]
#[ignore = "requires a PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn an_incomplete_or_malformed_upload_storage_configuration_refuses_to_start_by_name() {
    let database_url = std::env::var("HTTP_CONTRACT_DATABASE_URL")
        .expect("HTTP_CONTRACT_DATABASE_URL is required for the ignored contract test");

    // 只给 region：缺 bucket 与访问密钥。
    let (running, stderr) = probe_api_startup_with_upload_env(
        &database_url,
        &[("UPLOAD_STORAGE_REGION", "cn-hangzhou")],
    )
    .await;
    assert!(!running, "只给一部分配置必须拒绝启动；stderr: {stderr}");
    assert!(
        stderr.contains("UPLOAD_STORAGE_BUCKET"),
        "报错要点名缺的那个变量；stderr: {stderr}"
    );

    // bucket 含点号：形状不合法。
    let (running, stderr) = probe_api_startup_with_upload_env(
        &database_url,
        &[
            ("UPLOAD_STORAGE_REGION", "cn-hangzhou"),
            ("UPLOAD_STORAGE_BUCKET", "bad.bucket"),
            ("UPLOAD_STORAGE_ACCESS_KEY_ID", "probe-id"),
            ("UPLOAD_STORAGE_ACCESS_KEY_SECRET", "probe-secret"),
        ],
    )
    .await;
    assert!(!running, "含点号的桶名必须拒绝启动；stderr: {stderr}");
    assert!(
        stderr.contains("UPLOAD_STORAGE_BUCKET"),
        "报错要点名桶名；stderr: {stderr}"
    );

    // 非 loopback 的 http endpoint：形状不合法。
    let (running, stderr) = probe_api_startup_with_upload_env(
        &database_url,
        &[
            ("UPLOAD_STORAGE_REGION", "cn-hangzhou"),
            ("UPLOAD_STORAGE_BUCKET", "probe-bucket"),
            (
                "UPLOAD_STORAGE_ENDPOINT",
                "http://oss-cn-hangzhou.aliyuncs.com",
            ),
            ("UPLOAD_STORAGE_ACCESS_KEY_ID", "probe-id"),
            ("UPLOAD_STORAGE_ACCESS_KEY_SECRET", "probe-secret"),
        ],
    )
    .await;
    assert!(
        !running,
        "非 loopback 的 http 必须拒绝启动；stderr: {stderr}"
    );
    assert!(
        stderr.contains("UPLOAD_STORAGE_ENDPOINT"),
        "报错要点名 endpoint；stderr: {stderr}"
    );
}

/// A2：合法 PNG 上传成功，media_type 取服务端判定值，返回的 URL 可被无凭证客户端读到同一份字节。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_png_upload_writes_one_object_and_the_public_url_reads_back() {
    let harness =
        Harness::start_with_upload_storage(upload_storage(), UpstreamBehaviour::apimart()).await;
    let (status, body) = harness
        .upload(file_part(PNG_FIXTURE, "caller-name.png", Some("image/png")))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["media_type"], json!("image/png"));
    assert_eq!(body["byte_length"], json!(PNG_FIXTURE.len()));
    let url = body["url"].as_str().expect("url").to_owned();
    assert!(
        url.starts_with(&format!(
            "{}/{}/reference-media/",
            harness.object_storage().endpoint,
            UPLOAD_BUCKET
        )),
        "path-style 假对象存储的公网 URL：{url}"
    );
    assert!(url.ends_with(".png"));
    assert!(
        !url.contains("caller-name"),
        "调用方文件名不进对象键：{url}"
    );

    // 平台的上传路径只发 PUT 与 HEAD，不发 GET。
    let calls = harness.object_storage().calls();
    assert_eq!(
        calls.iter().filter(|(method, ..)| method == "PUT").count(),
        1
    );
    assert_eq!(
        calls.iter().filter(|(method, ..)| method == "HEAD").count(),
        1
    );
    assert_eq!(
        calls.iter().filter(|(method, ..)| method == "GET").count(),
        0,
        "平台不发 GET；下面的匿名读是测试客户端自己发的"
    );

    // A8：对客响应只给稳定字段，不含访问密钥或签名材料。
    let rendered = body.to_string();
    for leaked in [
        "contract-upload-id",
        "contract-upload-secret",
        "OSS4-HMAC-SHA256",
    ] {
        assert!(
            !rendered.contains(leaked),
            "响应不得含 {leaked}：{rendered}"
        );
    }

    let (read_status, bytes) = harness.read_public(&url).await;
    assert_eq!(read_status, StatusCode::OK);
    assert_eq!(bytes, PNG_FIXTURE);
    harness.cleanup().await;
}

/// A3a：单文件等于 20 MiB 即 413 image_too_large，且不写对象。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_file_at_the_single_file_limit_is_rejected_before_writing() {
    let harness =
        Harness::start_with_upload_storage(upload_storage(), UpstreamBehaviour::apimart()).await;
    let bytes = png_of_length(MAX_SINGLE_FILE_BYTES);
    let (status, body) = harness
        .upload(file_part(&bytes, "big.png", Some("image/png")))
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    assert_eq!(upload_error_code(&body), "image_too_large");
    assert!(
        harness.object_storage().calls().is_empty(),
        "不合规的文件不写对象"
    );
    harness.cleanup().await;
}

/// A3a：声明的 Content-Length 超过上传路由上限时在零正文读取的情况下回平台信封 413 request_too_large。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_declared_body_over_the_route_limit_is_rejected_without_reading_it() {
    let harness =
        Harness::start_with_upload_storage(upload_storage(), UpstreamBehaviour::apimart()).await;
    let (status, body) = raw_upload_with_declared_length(
        &harness.base_url,
        &harness.api_key,
        UPLOAD_REQUEST_BYTES + 1,
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    assert_eq!(upload_error_code(&body), "request_too_large");
    assert!(harness.object_storage().calls().is_empty());
    harness.cleanup().await;
}

/// A3b：单文件合法、整体请求体超过全局 16 MiB 且不超过上传路由上限的请求必须被受理。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_body_over_sixteen_mib_is_accepted_by_the_upload_route_limit() {
    let harness =
        Harness::start_with_upload_storage(upload_storage(), UpstreamBehaviour::apimart()).await;
    // 17 MiB 单文件：整体请求体超过全局 16 MiB，单文件仍在 20 MiB 上限内。
    let size = 17 * 1024 * 1024;
    let bytes = png_of_length(size);
    let (status, body) = harness
        .upload(file_part(&bytes, "large.png", Some("image/png")))
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "整体超过 16 MiB 的合法 multipart 必须被路由级上限受理：{body}"
    );
    assert_eq!(body["byte_length"], json!(size));
    harness.cleanup().await;
}

/// A4：白名单外的类型、0 字节、声明与内容不一致都在写入前被拒；不声明 Content-Type 时不做比较。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn unsupported_and_mismatched_media_are_rejected_before_writing() {
    let harness =
        Harness::start_with_upload_storage(upload_storage(), UpstreamBehaviour::apimart()).await;

    let (status, body) = harness
        .upload(file_part(&[], "empty.png", Some("image/png")))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(upload_error_code(&body), "unsupported_media_type");

    let (status, body) = harness
        .upload(file_part(b"not an image", "text.png", Some("image/png")))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(upload_error_code(&body), "unsupported_media_type");

    let (status, body) = harness
        .upload(file_part(PNG_FIXTURE, "wrong.jpg", Some("image/jpeg")))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(upload_error_code(&body), "media_type_mismatch");

    // 不声明 Content-Type：不做一致性比较，media_type 仍取魔数判定值。
    let (status, body) = harness
        .upload(file_part(PNG_FIXTURE, "undeclared.png", None))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["media_type"], json!("image/png"));

    assert!(
        harness
            .object_storage()
            .calls()
            .iter()
            .filter(|(method, ..)| method == "PUT")
            .count()
            == 1,
        "只有最后那次合法上传写了对象"
    );
    harness.cleanup().await;
}

/// A5 / A10：对象键形如 reference-media/{UTC 日期}/{uuid}.{ext}，不含调用方文件名，两次上传键不同；
/// 文件名扩展名不影响受理与对象键。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn object_keys_are_random_and_ignore_the_caller_file_name() {
    let harness =
        Harness::start_with_upload_storage(upload_storage(), UpstreamBehaviour::apimart()).await;

    let (status, first) = harness
        .upload(file_part(PNG_FIXTURE, "first.png", Some("image/png")))
        .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    // PNG 字节、文件名叫 a.gif：仍按 image/png 受理，对象键扩展名是 png。
    let (status, second) = harness
        .upload(file_part(PNG_FIXTURE, "a.gif", Some("image/png")))
        .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["media_type"], json!("image/png"));

    for body in [&first, &second] {
        let url = body["url"].as_str().expect("url");
        let key = url
            .rsplit_once("/reference-media/")
            .map(|(_, key)| format!("reference-media/{key}"))
            .expect("the object key prefix");
        let (date, rest) = key
            .trim_start_matches("reference-media/")
            .split_once('/')
            .expect("a date segment");
        assert_eq!(date.len(), 10, "日期段是 UTC 日期：{key}");
        assert!(date.as_bytes()[4] == b'-' && date.as_bytes()[7] == b'-');
        let (uuid, extension) = rest.split_once('.').expect("a uuid and an extension");
        assert_eq!(extension, "png");
        assert_eq!(uuid.len(), 36, "随机 v4 标识：{key}");
        assert!(!key.contains("first") && !key.contains("a.gif"));
    }
    assert_ne!(first["url"], second["url"], "两次上传产生不同的键");
    harness.cleanup().await;
}

/// A6：无效 API Key 回 401 invalid_api_key，且不写对象。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn an_invalid_api_key_is_rejected_before_the_body() {
    let harness =
        Harness::start_with_upload_storage(upload_storage(), UpstreamBehaviour::apimart()).await;
    let form = reqwest::multipart::Form::new().part(
        "file",
        file_part(PNG_FIXTURE, "probe.png", Some("image/png")),
    );
    let response = reqwest::Client::new()
        .post(format!("{}/v1/uploads/images", harness.base_url))
        .bearer_auth("sk_seeai_not-a-real-key")
        .multipart(form)
        .send()
        .await
        .expect("upload request");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = response.json::<Value>().await.expect("error body");
    assert_eq!(upload_error_code(&body), "invalid_api_key");
    assert!(harness.object_storage().calls().is_empty());

    // 认证先于正文长度判定与正文读取：声明超大 Content-Length、零正文、无效密钥，仍是 401。
    let (status, body) = raw_upload_with_declared_length(
        &harness.base_url,
        "sk_seeai_not-a-real-key",
        UPLOAD_REQUEST_BYTES + 1,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(upload_error_code(&body), "invalid_api_key");
    assert!(harness.object_storage().calls().is_empty());
    harness.cleanup().await;
}

/// A6：本机上传名额被占满时第二次上传回 429 upload_busy，不排队。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn an_exhausted_upload_slot_returns_429_upload_busy() {
    let gate = Arc::new(UpstreamGate::default());
    let behaviour = ObjectStorageBehaviour {
        hold_put: Some(gate.clone()),
        ..ObjectStorageBehaviour::default()
    };
    let mut upload = UploadStorageFixture::with_behaviour(behaviour);
    upload.slots = Some(1);
    upload.max_buffer_bytes = Some(UPLOAD_REQUEST_BYTES);
    let harness = Harness::start_with_upload_storage(upload, UpstreamBehaviour::apimart()).await;

    let first = tokio::spawn(upload_request(
        harness.base_url.clone(),
        harness.api_key.clone(),
        PNG_FIXTURE.to_vec(),
    ));
    // 等第一条上传真的开始写对象：此时它正占着唯一的上传名额。
    gate.wait_for_arrival(1).await;
    let (status, body) = harness
        .upload(file_part(PNG_FIXTURE, "second.png", Some("image/png")))
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(upload_error_code(&body), "upload_busy");
    assert!(body.get("retry_after").is_none() || body["retry_after"].is_number());

    gate.release_all();
    let (first_status, first_body) = first.await.expect("the first upload task");
    assert_eq!(first_status, StatusCode::OK, "{first_body}");
    harness.cleanup().await;
}

/// A6：上传速率用独立命名空间，不挤占生成的每 API Key 配额。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn the_upload_rate_limit_does_not_spend_the_generation_quota() {
    let cache = CacheFixture::start(CacheSettings::default()).await;
    let mut upload = upload_storage();
    upload.rate_limit = Some((1, 60_000));
    let harness =
        Harness::start_with_upload_storage_and_cache(upload, UpstreamBehaviour::apimart(), cache)
            .await;

    let (status, body) = harness
        .upload(file_part(PNG_FIXTURE, "one.png", Some("image/png")))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 生成配额仍是空的：上传没有占用它。
    let key = format!("upload-namespace-{}", Uuid::new_v4());
    let (status, body) = harness
        .sync_json(
            "/v1/images/generations",
            &key,
            route_request(harness.model, "namespace probe"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "上传不挤占生成的配额：{body}");
    assert_sync_success("上传之后的生成", &body);

    // 上传命名空间的第二个请求越限。
    let (status, body) = harness
        .upload(file_part(PNG_FIXTURE, "two.png", Some("image/png")))
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(upload_error_code(&body), "rate_limit_exceeded");
    harness.cleanup().await;
}

/// A7：上传前后账户余额、资金占用、账本条目与执行记录都不变。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn an_upload_changes_no_accounting_facts() {
    let harness =
        Harness::start_with_upload_storage(upload_storage(), UpstreamBehaviour::apimart()).await;
    let (balance_before, held_before) = balance_snapshot(&harness).await;
    let entries_before = ledger_entry_count(&harness).await;
    let jobs_before = job_count(&harness).await;

    let (status, body) = harness
        .upload(file_part(PNG_FIXTURE, "probe.png", Some("image/png")))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    assert_eq!(
        balance_snapshot(&harness).await,
        (balance_before, held_before)
    );
    assert_eq!(ledger_entry_count(&harness).await, entries_before);
    assert_eq!(job_count(&harness).await, jobs_before);
    harness.cleanup().await;
}

/// A8：对象存储返回可重试错误时按同一对象键重试，成功后返回 URL。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_retryable_object_storage_failure_is_retried_with_the_same_key() {
    let behaviour = ObjectStorageBehaviour {
        put_statuses: vec![503, 0],
        ..ObjectStorageBehaviour::default()
    };
    let mut upload = UploadStorageFixture::with_behaviour(behaviour);
    upload.retry_max_attempts = Some(3);
    upload.retry_backoff_base_seconds = Some(1);
    let harness = Harness::start_with_upload_storage(upload, UpstreamBehaviour::apimart()).await;

    let (status, body) = harness
        .upload(file_part(PNG_FIXTURE, "probe.png", Some("image/png")))
        .await;
    assert_eq!(status, StatusCode::OK, "重试之后成功：{body}");
    let puts: Vec<String> = harness
        .object_storage()
        .calls()
        .into_iter()
        .filter(|(method, ..)| method == "PUT")
        .map(|(_, path, _)| path)
        .collect();
    assert_eq!(puts.len(), 2, "一次失败一次成功");
    assert_eq!(puts[0], puts[1], "重试用同一个对象键");
    harness.cleanup().await;
}

/// A8：重试耗尽、终态 4xx 与元数据核验不一致都回 503 object_store_unavailable 且不返回 URL。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn terminal_and_exhausted_object_storage_failures_return_503() {
    // 重试耗尽：三次 503。
    let behaviour = ObjectStorageBehaviour {
        put_statuses: vec![503, 503, 503],
        ..ObjectStorageBehaviour::default()
    };
    let mut upload = UploadStorageFixture::with_behaviour(behaviour);
    upload.retry_max_attempts = Some(3);
    upload.retry_backoff_base_seconds = Some(1);
    let harness = Harness::start_with_upload_storage(upload, UpstreamBehaviour::apimart()).await;
    let (status, body) = harness
        .upload(file_part(PNG_FIXTURE, "probe.png", Some("image/png")))
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(upload_error_code(&body), "object_store_unavailable");
    assert_eq!(
        harness
            .object_storage()
            .calls()
            .iter()
            .filter(|(method, ..)| method == "PUT")
            .count(),
        3
    );
    harness.cleanup().await;

    // 终态 403：不重试。
    let behaviour = ObjectStorageBehaviour {
        put_statuses: vec![403, 0],
        ..ObjectStorageBehaviour::default()
    };
    let mut upload = UploadStorageFixture::with_behaviour(behaviour);
    upload.retry_max_attempts = Some(3);
    let harness = Harness::start_with_upload_storage(upload, UpstreamBehaviour::apimart()).await;
    let (status, body) = harness
        .upload(file_part(PNG_FIXTURE, "probe.png", Some("image/png")))
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(upload_error_code(&body), "object_store_unavailable");
    assert_eq!(
        harness
            .object_storage()
            .calls()
            .iter()
            .filter(|(method, ..)| method == "PUT")
            .count(),
        1,
        "终态失败不重试"
    );
    harness.cleanup().await;

    // 写入后元数据核验不一致：失败关闭、不返回 URL。
    let behaviour = ObjectStorageBehaviour {
        head_byte_length_delta: 1,
        ..ObjectStorageBehaviour::default()
    };
    let harness = Harness::start_with_upload_storage(
        UploadStorageFixture::with_behaviour(behaviour),
        UpstreamBehaviour::apimart(),
    )
    .await;
    let (status, body) = harness
        .upload(file_part(PNG_FIXTURE, "probe.png", Some("image/png")))
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(upload_error_code(&body), "object_store_unavailable");
    assert!(body.get("url").is_none(), "核验不一致不返回 URL");
    harness.cleanup().await;
}

/// A9：正文慢读超时在受理前回 408 request_timeout，且没有对象被写入。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_slow_upload_body_times_out_before_acceptance() {
    let mut upload = upload_storage();
    upload.slow_read_timeout_seconds = Some(1);
    let harness = Harness::start_with_upload_storage(upload, UpstreamBehaviour::apimart()).await;
    let (status, body) = slow_upload_request(&harness.base_url, &harness.api_key).await;
    assert_eq!(status, StatusCode::REQUEST_TIMEOUT, "{body}");
    assert_eq!(upload_error_code(&body), "request_timeout");
    assert!(
        harness.object_storage().calls().is_empty(),
        "慢读超时不写对象"
    );
    harness.cleanup().await;
}

/// A11：非 multipart、缺 file、多个部件、两个 file、file 没有文件名都回 400 invalid_multipart。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn malformed_multipart_requests_return_invalid_multipart() {
    let harness =
        Harness::start_with_upload_storage(upload_storage(), UpstreamBehaviour::apimart()).await;

    // 非 multipart 正文。
    let response = reqwest::Client::new()
        .post(format!("{}/v1/uploads/images", harness.base_url))
        .bearer_auth(&harness.api_key)
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await
        .expect("non-multipart request");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response.json::<Value>().await.expect("error body");
    assert_eq!(upload_error_code(&body), "invalid_multipart");

    // 缺 file 部件。
    let form = reqwest::multipart::Form::new().text("prompt", "hello");
    let (status, body) = harness.upload_form(form).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(upload_error_code(&body), "invalid_multipart");

    // 除 file 以外的部件。
    let form = reqwest::multipart::Form::new().text("other", "hello").part(
        "file",
        file_part(PNG_FIXTURE, "probe.png", Some("image/png")),
    );
    let (status, body) = harness.upload_form(form).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(upload_error_code(&body), "invalid_multipart");

    // 两个 file 部件。
    let form = reqwest::multipart::Form::new()
        .part("file", file_part(PNG_FIXTURE, "one.png", Some("image/png")))
        .part("file", file_part(PNG_FIXTURE, "two.png", Some("image/png")));
    let (status, body) = harness.upload_form(form).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(upload_error_code(&body), "invalid_multipart");

    // file 没有文件名。
    let form = reqwest::multipart::Form::new().part(
        "file",
        reqwest::multipart::Part::bytes(PNG_FIXTURE.to_vec()),
    );
    let (status, body) = harness.upload_form(form).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(upload_error_code(&body), "invalid_multipart");

    assert!(harness.object_storage().calls().is_empty());
    harness.cleanup().await;
}

/// A3a：分块编码、流式读到上传路由上限时同样回平台信封 413 request_too_large，不是框架裸 413。
#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn a_streamed_body_over_the_route_limit_is_rejected_with_the_platform_envelope() {
    let harness =
        Harness::start_with_upload_storage(upload_storage(), UpstreamBehaviour::apimart()).await;
    let (status, body) = chunked_over_limit_upload(&harness.base_url, &harness.api_key).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    assert_eq!(upload_error_code(&body), "request_too_large");
    assert!(harness.object_storage().calls().is_empty());
    harness.cleanup().await;
}
