use anyhow::{Context, Result};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use seeai_adapter_aihubmix::AihubmixAdapterFactory;
use seeai_adapter_apimart::ApimartAdapterFactory;
use seeai_application::{
    AdapterRegistry, ApplicationError, AssetService, GenerationService, HubRepository,
    IdentityService, PublishRuntimeCommand, ReconciliationService, RefundReconciliationCommand,
    RuntimeService,
};
use seeai_domain::{
    AccountId, AssetBinding, AssetId, CreateImageGeneration, ImageBranch, JobId, JobState,
};
use seeai_object_storage::ObjectStoreAssetStore;
use seeai_persistence::PgHubRepository;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{env, net::SocketAddr, sync::Arc};
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
        generations: GenerationService::new(repository_port),
    };
    let app = Router::new()
        .route("/health", get(health))
        .route("/admin/accounts", post(create_account))
        .route("/admin/accounts/{account_id}/credits", post(credit_account))
        .route("/admin/accounts/{account_id}/api-keys", post(issue_api_key))
        .route("/admin/runtime-revisions", post(publish_runtime))
        .route(
            "/admin/reconciliation-cases",
            get(list_reconciliation_cases),
        )
        .route(
            "/admin/reconciliation-cases/{job_id}/refund",
            post(refund_reconciliation),
        )
        .route("/v1/assets", post(upload_asset))
        .route("/v1/assets/{asset_id}", get(download_asset))
        .route("/v1/image-generations", post(create_generation))
        .route("/v1/image-generations/{job_id}", get(get_generation))
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

#[derive(Debug, Deserialize)]
struct CreateGenerationBody {
    native_model_id: String,
    native_parameters: Value,
    #[serde(default)]
    asset_bindings: Vec<AssetBinding>,
    idempotency_key: String,
    max_cost_microusd: u64,
}

#[derive(Debug, Serialize)]
struct CreateGenerationResponse {
    job_id: JobId,
    state: JobState,
    branch: ImageBranch,
    created_at: chrono::DateTime<chrono::Utc>,
}

async fn create_generation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateGenerationBody>,
) -> Result<(StatusCode, Json<CreateGenerationResponse>), ApiError> {
    let account_id = authenticate(&state, &headers).await?;
    let job = state
        .generations
        .create(CreateImageGeneration {
            account_id,
            native_model_id: body.native_model_id,
            native_parameters: body.native_parameters,
            asset_bindings: body.asset_bindings,
            idempotency_key: body.idempotency_key,
            max_cost_microusd: body.max_cost_microusd,
        })
        .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(CreateGenerationResponse {
            job_id: job.id,
            state: job.state,
            branch: job.branch,
            created_at: job.created_at,
        }),
    ))
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

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .json()
        .init();
}
