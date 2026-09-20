use anyhow::{Context, Result};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Multipart, Path, Query, State, multipart::Field},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::{DateTime, Utc};
use seeai_adapter_aihubmix::AihubmixAdapterFactory;
use seeai_adapter_apimart::ApimartAdapterFactory;
use seeai_application::{
    AdapterRegistry, ApplicationError, AssetService, CreateImageGenerationRequest,
    GenerationService, HubRepository, IdentityService, JobView, ProviderFailureKind,
    ProviderFailureQuery, ProviderFailureView, PublishRuntimeCommand, ReconciliationService,
    RefundReconciliationCommand, RuntimeService,
};
use seeai_domain::{AccountId, AssetId, ImageBranch, JobId, JobState};
use seeai_object_storage::ObjectStoreAssetStore;
use seeai_persistence::PgHubRepository;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{env, net::SocketAddr, sync::Arc, time::Duration};
use tower_http::{request_id::MakeRequestUuid, trace::TraceLayer};
use tracing::info;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

#[derive(Clone)]
struct AppState {
    admin_token: Arc<str>,
    repository: Arc<dyn HubRepository>,
    identity: IdentityService,
    runtime: RuntimeService,
    reconciliation: ReconciliationService,
    assets: AssetService,
    generations: GenerationService,
    /// 兼容入口等任务跑完的最长时间。
    sync_wait: Duration,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    init_tracing();
    let database_url = required_env("DATABASE_URL")?;
    let bind: SocketAddr = env::var("API_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8081".to_owned())
        .parse()
        .context("API_BIND must be a socket address")?;
    let admin_token: Arc<str> = Arc::from(required_env("ADMIN_TOKEN")?);
    let repository = Arc::new(PgHubRepository::connect(&database_url, 10).await?);
    repository.migrate().await?;
    let store = Arc::new(ObjectStoreAssetStore::from_env()?);
    let repository_port: Arc<dyn HubRepository> = repository;
    // 组合工厂：按 adapter_key 分派到各渠道自己的 Driver（纯装配）。
    let adapters: Arc<dyn seeai_application::AdapterFactory> =
        Arc::new(AdapterRegistry::new(vec![
            Arc::new(AihubmixAdapterFactory),
            Arc::new(ApimartAdapterFactory),
        ]));
    let state = AppState {
        admin_token,
        repository: repository_port.clone(),
        identity: IdentityService::new(repository_port.clone()),
        runtime: RuntimeService::new(repository_port.clone(), adapters),
        reconciliation: ReconciliationService::new(repository_port.clone()),
        assets: AssetService::new(repository_port.clone(), store),
        sync_wait: generation_sync_wait()?,
        generations: GenerationService::new(
            repository_port,
            generation_max_cost_microusd()?,
            generation_max_concurrent_jobs()?,
        ),
    };
    let app = Router::new()
        .route("/health", get(health))
        .route("/api/v1/accounts", post(create_account))
        .route(
            "/api/v1/accounts/{account_id}/credits",
            post(credit_account),
        )
        .route(
            "/api/v1/accounts/{account_id}/api-keys",
            post(issue_api_key),
        )
        .route("/api/v1/runtime-revisions", post(publish_runtime))
        .route(
            "/api/v1/reconciliation-cases",
            get(list_reconciliation_cases),
        )
        .route(
            "/api/v1/reconciliation-cases/{job_id}/refund",
            post(refund_reconciliation),
        )
        .route("/api/v1/provider-failures", get(list_provider_failures))
        .route("/v1/assets", post(upload_asset))
        .route("/v1/assets/{asset_id}", get(download_asset))
        .route("/v1/image-generations", post(create_generation))
        .route("/v1/image-generations/{job_id}", get(get_generation))
        .route("/v1/images/generations", post(create_generation_compat))
        .route("/v1/images/edits", post(create_image_edit_compat))
        .layer(DefaultBodyLimit::max(16 * 1024 * 1024))
        .layer(tower_http::request_id::SetRequestIdLayer::new(
            header::HeaderName::from_static("x-request-id"),
            MakeRequestUuid,
        ))
        .layer(TraceLayer::new_for_http())
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(bind).await?;
    info!(%bind, "api listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(error = %error, "failed to listen for shutdown signal");
    }
}

async fn health() -> Json<Value> {
    Json(json!({"status": "ok"}))
}

#[derive(Debug, Deserialize)]
struct CreateAccountBody {
    initial_credit_microusd: u64,
}

#[derive(Debug, Serialize)]
struct CreateAccountResponse {
    account_id: AccountId,
}

async fn create_account(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateAccountBody>,
) -> Result<Json<CreateAccountResponse>, ApiError> {
    require_admin(&state, &headers)?;
    let account_id = AccountId::new();
    state
        .repository
        .create_account(account_id, body.initial_credit_microusd, "admin-api")
        .await?;
    Ok(Json(CreateAccountResponse { account_id }))
}

#[derive(Debug, Deserialize)]
struct CreditAccountBody {
    amount_microusd: u64,
    business_key: String,
}

async fn credit_account(
    State(state): State<AppState>,
    Path(account_id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<CreditAccountBody>,
) -> Result<StatusCode, ApiError> {
    require_admin(&state, &headers)?;
    state
        .repository
        .credit_account(
            AccountId(account_id),
            body.amount_microusd,
            &body.business_key,
            "admin-api",
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct IssueApiKeyBody {
    label: String,
}

#[derive(Debug, Serialize)]
struct IssueApiKeyResponse {
    api_key: String,
}

async fn issue_api_key(
    State(state): State<AppState>,
    Path(account_id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<IssueApiKeyBody>,
) -> Result<Json<IssueApiKeyResponse>, ApiError> {
    require_admin(&state, &headers)?;
    let api_key = state
        .identity
        .issue_api_key(AccountId(account_id), &body.label, "admin-api")
        .await?;
    Ok(Json(IssueApiKeyResponse { api_key }))
}

async fn publish_runtime(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut command): Json<PublishRuntimeCommand>,
) -> Result<Json<seeai_domain::PublishedRevision>, ApiError> {
    require_admin(&state, &headers)?;
    command.actor = "admin-api".to_owned();
    Ok(Json(state.runtime.publish(command).await?))
}

async fn list_reconciliation_cases(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<seeai_application::ReconciliationCaseView>>, ApiError> {
    require_admin(&state, &headers)?;
    Ok(Json(state.reconciliation.list_open().await?))
}

/// 平台侧失败清单的查询参数：`kind` 为逗号分隔的类别、`since` 为 RFC3339、`limit` 为条数上限。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderFailuresQuery {
    kind: Option<String>,
    since: Option<DateTime<Utc>>,
    limit: Option<u32>,
}

const DEFAULT_FAILURE_LIMIT: u32 = 100;
const MAX_FAILURE_LIMIT: u32 = 500;

/// 平台侧失败清单的响应：不翻页，因此必须让调用方看得出结果被截断了。
#[derive(Debug, Serialize)]
struct ProviderFailuresResponse {
    failures: Vec<ProviderFailureView>,
    count: usize,
    truncated: bool,
}

/// 平台侧失败清单（仅管理员）：运营用它发现平台在渠道侧欠费、凭证/配置问题与平台自己的 bug。
///
/// 渠道的原始码与原文只在这个管理端视图里出现；其中可能夹带凭证片段，先过滤再返回。
/// `kind` 的取值与落库值同名（见 `crates/adapter-sdk` 的 `ProviderFailureKind`）：它是本平台
/// 管理端的取值契约，改枚举名即改接口。不传 `kind` 时只列平台侧事件。
async fn list_provider_failures(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ProviderFailuresQuery>,
) -> Result<Json<ProviderFailuresResponse>, ApiError> {
    require_admin(&state, &headers)?;
    let kinds = match query.kind.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => raw
            .split(',')
            .map(str::trim)
            .map(|value| {
                ProviderFailureKind::parse(value).ok_or_else(|| {
                    ApiError::bad_request(
                        "invalid_failure_kind",
                        format!("unknown failure kind: {value}"),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => Vec::new(),
    };
    let limit = query
        .limit
        .unwrap_or(DEFAULT_FAILURE_LIMIT)
        .clamp(1, MAX_FAILURE_LIMIT);
    let failures = state
        .reconciliation
        .provider_failures(ProviderFailureQuery {
            kinds,
            since: query.since,
            limit,
        })
        .await?
        .into_iter()
        .map(|mut view| {
            view.provider_error_message = view
                .provider_error_message
                .as_deref()
                .map(sanitize_provider_text);
            view
        })
        .collect::<Vec<_>>();
    Ok(Json(ProviderFailuresResponse {
        count: failures.len(),
        truncated: failures.len() as u32 == limit,
        failures,
    }))
}

/// 过滤渠道原文里的凭证片段：有的渠道错误消息会回显密钥后几位（形如 `key(abcdef)`）。
///
/// 这是**兜底**，不是凭证保护的主力——凭证永远不从环境变量以外的渠道进响应；它只处理
/// 上游把片段写进错误文本这一种情况。括号没闭合时宁可丢掉后半句，也不把可疑片段放出去。
fn sanitize_provider_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find("key(") {
        out.push_str(rest.get(..index).unwrap_or_default());
        match rest.get(index..).and_then(|tail| tail.find(')')) {
            Some(end) => rest = rest.get(index + end + 1..).unwrap_or_default(),
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RefundReconciliationBody {
    note: String,
    business_key: String,
}

async fn refund_reconciliation(
    State(state): State<AppState>,
    Path(job_id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<RefundReconciliationBody>,
) -> Result<StatusCode, ApiError> {
    require_admin(&state, &headers)?;
    state
        .reconciliation
        .refund(RefundReconciliationCommand {
            job_id: JobId(job_id),
            note: body.note,
            business_key: body.business_key,
            actor: "admin-api".to_owned(),
        })
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn upload_asset(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<seeai_application::AssetRecord>), ApiError> {
    let account_id = authenticate(&state, &headers).await?;
    let media_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::bad_request("missing_content_type", "Content-Type is required"))?;
    let role = headers
        .get("x-asset-role")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("image");
    let asset = state
        .assets
        .upload(account_id, role, media_type, body)
        .await?;
    Ok((StatusCode::CREATED, Json(asset)))
}

async fn download_asset(
    State(state): State<AppState>,
    Path(asset_id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let account_id = authenticate(&state, &headers).await?;
    let (record, bytes) = state.assets.download(account_id, AssetId(asset_id)).await?;
    let content_type = HeaderValue::from_str(&record.media_type).map_err(|_| {
        ApiError::bad_request("invalid_asset_media_type", "Asset media type is invalid")
    })?;
    Ok(([(header::CONTENT_TYPE, content_type)], bytes).into_response())
}

/// 受理请求：**平铺**的模型参数 + 平台自己的控制字段。
///
/// 调用方按合同把模型参数写在顶层（不再有 `native_parameters` 外壳），图片用 `image` /
/// `mask` 指名平台资产 id；平台按选中候选声明的参数面决定装到哪个字段上。
#[derive(Debug, Deserialize)]
struct CreateGenerationBody {
    #[serde(flatten)]
    parameters: Map<String, Value>,
}

/// `Idempotency-Key` 请求头（可选，OpenAI 的写法）：给了就用它去重，没给就生成一个。
fn idempotency_key(headers: &HeaderMap) -> String {
    headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string())
}

/// 取出 `image`（一个 id 或 id 数组）并从参数里删掉它。
fn take_asset_ids(
    parameters: &mut Map<String, Value>,
    name: &str,
) -> Result<Vec<AssetId>, ApiError> {
    match parameters.remove(name) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(id)) => Ok(vec![parse_asset_id(name, &id)?]),
        Some(Value::Array(items)) => items
            .into_iter()
            .map(|item| match item {
                Value::String(id) => parse_asset_id(name, &id),
                other => Err(invalid_asset_reference(name, &other)),
            })
            .collect(),
        Some(other) => Err(invalid_asset_reference(name, &other)),
    }
}

/// 取出 `mask`（只接受一个 id）并从参数里删掉它。
fn take_asset_id(
    parameters: &mut Map<String, Value>,
    name: &str,
) -> Result<Option<AssetId>, ApiError> {
    match parameters.remove(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(id)) => Ok(Some(parse_asset_id(name, &id)?)),
        Some(other) => Err(invalid_asset_reference(name, &other)),
    }
}

fn parse_asset_id(name: &str, value: &str) -> Result<AssetId, ApiError> {
    Uuid::parse_str(value.trim()).map(AssetId).map_err(|_| {
        ApiError::bad_request(
            "invalid_asset_id",
            format!("{name} is not a valid asset id: {value}"),
        )
    })
}

fn invalid_asset_reference(name: &str, value: &Value) -> ApiError {
    ApiError::bad_request(
        "invalid_asset_reference",
        format!("{name} must be an asset id or an array of asset ids, got {value}"),
    )
}

#[derive(Debug, Serialize)]
struct CreateGenerationResponse {
    job_id: JobId,
    state: JobState,
    branch: ImageBranch,
    created_at: chrono::DateTime<chrono::Utc>,
}

/// 统一入口：平铺的模型参数 + `image` / `mask`（平台资产 id）。
async fn create_generation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateGenerationBody>,
) -> Result<(StatusCode, Json<CreateGenerationResponse>), ApiError> {
    let account_id = authenticate(&state, &headers).await?;
    let mut parameters = body.parameters;
    let image_asset_ids = take_asset_ids(&mut parameters, "image")?;
    let mask_asset_id = take_asset_id(&mut parameters, "mask")?;
    let accepted = accept_generation(
        &state,
        account_id,
        &headers,
        parameters,
        image_asset_ids,
        mask_asset_id,
    )
    .await?;
    Ok((StatusCode::ACCEPTED, Json(accepted)))
}

/// 三个入口共用的受理路径：解码差异只到"参数 + 图片角色"为止，之后完全一样。
///
/// 兼容入口不自己选路、计费或调用 Provider——它们只做请求解码与资产绑定，分支由参数与图片角色决定。
async fn accept_generation(
    state: &AppState,
    account_id: AccountId,
    headers: &HeaderMap,
    mut parameters: Map<String, Value>,
    image_asset_ids: Vec<AssetId>,
    mask_asset_id: Option<AssetId>,
) -> Result<CreateGenerationResponse, ApiError> {
    let model = parameters
        .remove("model")
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| ApiError::bad_request("missing_model", "model is required"))?;
    let job = state
        .generations
        .create(CreateImageGenerationRequest {
            account_id,
            model,
            native_parameters: Value::Object(parameters),
            image_asset_ids,
            mask_asset_id,
            idempotency_key: idempotency_key(headers),
        })
        .await?;
    Ok(CreateGenerationResponse {
        job_id: job.id,
        state: job.state,
        branch: job.branch,
        created_at: job.created_at,
    })
}

/// generations 兼容入口（OpenAI 契约的路径）。
///
/// 它与编辑入口**是同一个能力**：分支只看请求里有没有 `image` / `mask`，不由端点断言——
/// 带图的 generations、不带图的 edits 都是合法请求。
async fn create_generation_compat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateGenerationBody>,
) -> Result<Response, ApiError> {
    let account_id = authenticate(&state, &headers).await?;
    let mut parameters = body.parameters;
    let image_asset_ids = take_asset_ids(&mut parameters, "image")?;
    let mask_asset_id = take_asset_id(&mut parameters, "mask")?;
    accept_compat_generation(
        &state,
        account_id,
        &headers,
        parameters,
        image_asset_ids,
        mask_asset_id,
    )
    .await
}

/// edits 兼容入口（OpenAI 契约的路径）：`multipart/form-data`，`image` 与 `mask` 是**文件
/// 部件**；其余文本部件就是模型参数。
///
/// 没有 `image` 的 edits 同样合法（那就是文生图）——分支由请求内容决定，不由端点断言。
async fn create_image_edit_compat(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Response, ApiError> {
    let account_id = authenticate(&state, &headers).await?;
    let mut parameters = Map::new();
    let mut image_asset_ids = Vec::new();
    let mut mask_asset_id = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| ApiError::bad_request("invalid_multipart", error.to_string()))?
    {
        let name = field.name().unwrap_or_default().to_owned();
        match name.as_str() {
            "image" => {
                image_asset_ids.push(upload_form_asset(&state, account_id, "image", field).await?);
            }
            "mask" => {
                mask_asset_id = Some(upload_form_asset(&state, account_id, "mask", field).await?);
            }
            _ => {
                let text = field.text().await.map_err(|error| {
                    ApiError::bad_request("invalid_multipart", error.to_string())
                })?;
                parameters.insert(name.clone(), form_scalar(&name, &text));
            }
        }
    }
    accept_compat_generation(
        &state,
        account_id,
        &headers,
        parameters,
        image_asset_ids,
        mask_asset_id,
    )
    .await
}

/// 兼容入口的受理与响应：受理之后**等任务跑到终态**，按 OpenAI 的形状把图片交回去。
///
/// OpenAI 的客户端调这两个路径时，期望在同一个响应里直接拿到图片；我们的流水线是异步的
/// （受理 → Worker 执行），所以这里给它一个同步门面：等不到就返回 504，并把 `job_id` 写进
/// 错误信息里，调用方可以改用 `/v1/image-generations/{job_id}` 自己轮询。
async fn accept_compat_generation(
    state: &AppState,
    account_id: AccountId,
    headers: &HeaderMap,
    parameters: Map<String, Value>,
    image_asset_ids: Vec<AssetId>,
    mask_asset_id: Option<AssetId>,
) -> Result<Response, ApiError> {
    let accepted = accept_generation(
        state,
        account_id,
        headers,
        parameters,
        image_asset_ids,
        mask_asset_id,
    )
    .await?;
    let job_id = accepted.job_id;
    let deadline = tokio::time::Instant::now() + state.sync_wait;
    loop {
        let view = state.generations.get(account_id, job_id).await?;
        match view.state.as_str() {
            "succeeded" => return openai_image_response(state, account_id, view).await,
            "failed" | "reconciliation_required" => {
                let code = view.error_code.as_deref().unwrap_or("platform_unavailable");
                return Ok(openai_error_response(code));
            }
            _ if tokio::time::Instant::now() >= deadline => {
                return Ok(openai_error_response_with(
                    StatusCode::GATEWAY_TIMEOUT,
                    "job_still_running",
                    "server_error",
                    &format!(
                        "the job is still running; query /v1/image-generations/{job_id} for the result"
                    ),
                ));
            }
            _ => tokio::time::sleep(Duration::from_millis(250)).await,
        }
    }
}

/// 成功：OpenAI 的形状 `{created, data:[{b64_json}]}`。
async fn openai_image_response(
    state: &AppState,
    account_id: AccountId,
    view: JobView,
) -> Result<Response, ApiError> {
    let mut data = Vec::with_capacity(view.result_asset_ids.len());
    for asset_id in view.result_asset_ids {
        let (_record, bytes) = state.assets.download(account_id, asset_id).await?;
        data.push(json!({ "b64_json": STANDARD.encode(&bytes) }));
    }
    Ok(Json(json!({
        "created": view.updated_at.timestamp(),
        "data": data,
    }))
    .into_response())
}

/// 失败：仍用 OpenAI 的错误信封，对客码沿用平台那三个。
fn openai_error_response(public_code: &str) -> Response {
    match public_code {
        "content_rejected" => openai_error_response_with(
            StatusCode::BAD_REQUEST,
            public_code,
            "invalid_request_error",
            "the submitted content was rejected",
        ),
        "outcome_unknown" => openai_error_response_with(
            StatusCode::BAD_GATEWAY,
            public_code,
            "server_error",
            "the request outcome is unknown; see reconciliation",
        ),
        _ => openai_error_response_with(
            StatusCode::BAD_GATEWAY,
            public_code,
            "server_error",
            "the platform could not complete this request",
        ),
    }
}

fn openai_error_response_with(
    status: StatusCode,
    code: &str,
    error_type: &str,
    message: &str,
) -> Response {
    (
        status,
        Json(json!({ "error": { "message": message, "type": error_type, "code": code } })),
    )
        .into_response()
}

/// 把 multipart 里的文件部件存成平台资产，返回它的 id。
async fn upload_form_asset(
    state: &AppState,
    account_id: AccountId,
    role: &str,
    field: Field<'_>,
) -> Result<AssetId, ApiError> {
    let media_type = field
        .content_type()
        .map(str::to_owned)
        .unwrap_or_else(|| "image/png".to_owned());
    let bytes = field
        .bytes()
        .await
        .map_err(|error| ApiError::bad_request("invalid_multipart", error.to_string()))?;
    let asset = state
        .assets
        .upload(account_id, role, &media_type, bytes)
        .await?;
    Ok(asset.id)
}

/// 表单里除文件外的部件都是字符串；只有整数型参数还原成数字（`n`），其余保持字符串
/// （`prompt` 写成 "1" 也不能变成数字）。
fn form_scalar(name: &str, text: &str) -> Value {
    if name == "n"
        && let Ok(value) = text.trim().parse::<i64>()
    {
        return Value::from(value);
    }
    Value::String(text.to_owned())
}

async fn get_generation(
    State(state): State<AppState>,
    Path(job_id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Json<seeai_application::JobView>, ApiError> {
    let account_id = authenticate(&state, &headers).await?;
    Ok(Json(
        state.generations.get(account_id, JobId(job_id)).await?,
    ))
}

async fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<AccountId, ApiError> {
    let token = bearer_token(headers)?;
    state
        .identity
        .authenticate(token)
        .await
        .map_err(|_| ApiError {
            status: StatusCode::UNAUTHORIZED,
            code: "invalid_api_key",
            message: "API key is invalid or revoked".to_owned(),
        })
}

fn require_admin(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let token = bearer_token(headers)?;
    if token.as_bytes() == state.admin_token.as_bytes() {
        Ok(())
    } else {
        Err(ApiError {
            status: StatusCode::FORBIDDEN,
            code: "admin_forbidden",
            message: "admin authorization failed".to_owned(),
        })
    }
}

fn bearer_token(headers: &HeaderMap) -> Result<&str, ApiError> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError {
            status: StatusCode::UNAUTHORIZED,
            code: "authorization_required",
            message: "Bearer authorization is required".to_owned(),
        })
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code,
            message: message.into(),
        }
    }
}

impl From<ApplicationError> for ApiError {
    fn from(error: ApplicationError) -> Self {
        let (status, code) = match error {
            ApplicationError::Validation(_) => (StatusCode::BAD_REQUEST, "validation_error"),
            ApplicationError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            ApplicationError::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
            ApplicationError::InsufficientBalance => {
                (StatusCode::PAYMENT_REQUIRED, "insufficient_balance")
            }
            ApplicationError::TooManyInFlight => {
                (StatusCode::TOO_MANY_REQUESTS, "too_many_in_flight")
            }
            ApplicationError::Configuration(_)
            | ApplicationError::Persistence(_)
            | ApplicationError::ObjectStorage(_)
            | ApplicationError::Reconciliation(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
            }
        };
        let message = if status.is_server_error() {
            "The server could not complete the request".to_owned()
        } else {
            error.to_string()
        };
        Self {
            status,
            code,
            message,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({
                "error": {
                    "code": self.code,
                    "message": self.message,
                }
            })),
        )
            .into_response()
    }
}

fn required_env(name: &str) -> Result<String> {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("missing environment {name}"))
}

/// 预授权额（microusd）。**服务端定，不由调用方自报**——现状是一个固定数（默认 $0.02），
/// 够跑通也有上限；按 Price Snapshot 算该请求的最坏成本是后续优化。
fn generation_max_cost_microusd() -> Result<u64> {
    match env::var("GENERATION_MAX_COST_MICROUSD") {
        Ok(value) if !value.trim().is_empty() => value
            .trim()
            .parse::<u64>()
            .context("GENERATION_MAX_COST_MICROUSD must be an integer"),
        _ => Ok(20_000),
    }
}

/// 兼容入口等任务跑完的窗口（秒，默认 120）：超了就把 job id 交回调用方自己去查。
fn generation_sync_wait() -> Result<Duration> {
    let seconds = match env::var("GENERATION_SYNC_WAIT_SECONDS") {
        Ok(value) if !value.trim().is_empty() => value
            .trim()
            .parse::<u64>()
            .context("GENERATION_SYNC_WAIT_SECONDS must be an integer")?,
        _ => 120,
    };
    Ok(Duration::from_secs(seconds))
}

/// 一个账户同时能有多少个在跑的生成任务（默认 1）。
fn generation_max_concurrent_jobs() -> Result<u64> {
    match env::var("GENERATION_MAX_CONCURRENT_JOBS") {
        Ok(value) if !value.trim().is_empty() => value
            .trim()
            .parse::<u64>()
            .context("GENERATION_MAX_CONCURRENT_JOBS must be an integer"),
        _ => Ok(1),
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .json()
        .init();
}

#[cfg(test)]
mod tests {
    use super::sanitize_provider_text;

    #[test]
    fn credential_fragments_are_removed_from_provider_text() {
        assert_eq!(
            sanitize_provider_text("Forbidden – key(ab12cd) allowed only from approved IP ranges."),
            "Forbidden –  allowed only from approved IP ranges."
        );
        // 一条消息里出现多次也要全去掉。
        assert_eq!(
            sanitize_provider_text("key(aaa) then key(bbb) done"),
            " then  done"
        );
    }

    #[test]
    fn text_without_credential_fragments_is_untouched() {
        assert_eq!(
            sanitize_provider_text("insufficient_user_quota: quota exhausted"),
            "insufficient_user_quota: quota exhausted"
        );
    }

    #[test]
    fn an_unclosed_fragment_drops_the_rest_of_the_message() {
        // 宁可丢掉后半句，也不能把括号里的内容放出去。
        assert_eq!(
            sanitize_provider_text("Forbidden – key(ab12cd nothing closes this"),
            "Forbidden – "
        );
    }
}
