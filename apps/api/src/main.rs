use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Multipart, Path, Query, State, multipart::Field},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, patch, post, put},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::{DateTime, Utc};
use seeai_adapter_aihubmix::AihubmixAdapterFactory;
use seeai_adapter_apimart::ApimartAdapterFactory;
use seeai_application::{
    AccelerationService, AccountsService, AdapterRegistry, ApplicationError, CachePolicy,
    CreateImageGenerationRequest, GatewayModelView, GeneratedImage, GenerationService,
    HubRepository, IdentityService, JobView, MAX_OPERATIONAL_LIMIT, NewFxRate, PricingService,
    ProviderCostGapView, ProviderFailureKind, ProviderFailureQuery, ProviderFailureView,
    PublishRuntimeCommand, ReconciliationService, RefundReconciliationCommand, RuntimeService,
};
use seeai_cache_redis::RedisCache;
use seeai_domain::{
    AccountId, ImageInputs, ImageParameterKind, JobId, PublishedModel,
    contract_image_parameter_kind, replace_contract_model_identity,
};
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
    identity: IdentityService,
    runtime: RuntimeService,
    reconciliation: ReconciliationService,
    /// 定价侧的管理员面：折算率的录入与取值（汇率不进不可变修订）。
    pricing: PricingService,
    /// 账户面的管理员用例：建账户与充值——两件事都要在提交成功后把余额写进缓存。
    accounts: AccountsService,
    generations: GenerationService,
    /// 同步入口等任务跑完的最长时间。
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
    let repository_port: Arc<dyn HubRepository> = repository;
    // 组合工厂：按 adapter_key 分派到各渠道自己的 Driver（纯装配）。
    let adapters: Arc<dyn seeai_application::AdapterFactory> =
        Arc::new(AdapterRegistry::new(vec![
            Arc::new(AihubmixAdapterFactory),
            Arc::new(ApimartAdapterFactory),
        ]));
    // 加速层：`REDIS_URL` 没配就是没有缓存——那时这一层是空操作，受理路径连那次轻量读都不做，
    // 行为与没有它时逐位相同。配了但连不上也只是"每次都未命中"，回源数据库。
    let acceleration = match RedisCache::from_env()? {
        Some(cache) => {
            info!("cache acceleration layer enabled");
            Arc::new(AccelerationService::new(
                repository_port.clone(),
                Arc::new(cache),
                CachePolicy::from_env()?,
            ))
        }
        None => Arc::new(AccelerationService::disabled(repository_port.clone())),
    };
    // 定时对账兜底：以数据库为准把缓存覆盖回去。它挂在这里而不是 Worker 上——对客请求由本进程
    // 服务，本进程在，兜底就在。
    if acceleration.is_enabled() {
        tokio::spawn(acceleration.clone().run_reconciler());
    }
    let state = AppState {
        admin_token,
        identity: IdentityService::new(repository_port.clone()),
        runtime: RuntimeService::new(repository_port.clone(), adapters)
            .with_acceleration(acceleration.clone()),
        reconciliation: ReconciliationService::new(repository_port.clone())
            .with_acceleration(acceleration.clone()),
        pricing: PricingService::new(repository_port.clone()),
        accounts: AccountsService::new(repository_port.clone())
            .with_acceleration(acceleration.clone()),
        sync_wait: generation_sync_wait()?,
        generations: GenerationService::new(
            repository_port,
            generation_max_cost_microusd()?,
            generation_max_concurrent_jobs()?,
        )
        .with_acceleration(acceleration),
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
        .route("/api/v1/gateway-models", get(list_gateway_models))
        .route(
            "/api/v1/gateway-models/{gateway_model}",
            patch(set_gateway_model_enabled),
        )
        .route(
            "/api/v1/reconciliation-cases",
            get(list_reconciliation_cases),
        )
        .route(
            "/api/v1/reconciliation-cases/{job_id}/refund",
            post(refund_reconciliation),
        )
        .route("/api/v1/provider-failures", get(list_provider_failures))
        .route("/api/v1/fx-rates", put(upsert_fx_rate))
        .route("/api/v1/provider-cost-gaps", get(list_provider_cost_gaps))
        .route("/v1/images/generations", post(generate_image))
        .route("/v1/images/edits", post(edit_image))
        .route("/v1/models", get(list_models))
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
        .accounts
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
        .accounts
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
        .clamp(1, MAX_OPERATIONAL_LIMIT);
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

/// 折算率的请求体：按币种给"1 单位该币种 = 多少人民币"的定点比值。
///
/// `rate_micros` 是定点整数（分母 1_000_000）：钱与汇率都不走浮点，差一个微单位就是对不上账。
/// `effective_at` 不给就是"立即生效"；给了未来时刻就是调价预告——受理时取的仍是受理时刻
/// 之前已生效的那一行。不给时这个时刻由**数据库**盖章：发布期校验与受理取值用的都是库的
/// `now()`，换成 API 进程的时钟就会因两个时钟的漂移把"刚录完就发布"误判成"还没有生效"。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpsertFxRateBody {
    currency: String,
    rate_micros: u64,
    effective_at: Option<DateTime<Utc>>,
}

/// 管理员写：录入一行折算率（渠道币种 → CNY）。
///
/// 汇率是**外部事实**，按币种维护，不属于任何一份发布：同一时刻同一币种全平台必须是同一个数
/// 才对账得起来，放进每份发布里改一次汇率就要重发所有型号。没有折算率的币种在**发布期**被拒
/// ——受理时取不到汇率就等于算不出成本，而那时拒的是消费者的请求。
async fn upsert_fx_rate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<UpsertFxRateBody>,
) -> Result<StatusCode, ApiError> {
    require_admin(&state, &headers)?;
    state
        .pricing
        .upsert_fx_rate(
            // 生效时刻原样交给端口：`None` 表示"立即生效"，由数据库盖章，API 不替它读时钟。
            NewFxRate {
                currency: body.currency,
                rate_micros: body.rate_micros,
                effective_at: body.effective_at,
            },
            "admin-api",
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 成本缺口清单的查询参数：`limit` 为条数上限。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderCostGapsQuery {
    limit: Option<u32>,
}

/// 成本缺口清单的响应：与平台侧失败清单同形——不翻页，所以必须让调用方看得出结果被截断了。
#[derive(Debug, Serialize)]
struct ProviderCostGapsResponse {
    gaps: Vec<ProviderCostGapView>,
    count: usize,
    truncated: bool,
}

/// 成本缺口清单（仅管理员）：执行发生了、成本本该有金额却拿不到（`unavailable`）的那些执行。
///
/// 运营从这里看到缺口：拿 `provider_trace_id` 去上游核账单，人工补录金额归账实核对那条线，
/// 补录完成后这一笔不再出现在清单里。这些 Job **不进对账态**——对客结算已经按费率快照正常
/// 完成，消费者的钱该扣的照扣；缺口是平台侧的账务缺口。
async fn list_provider_cost_gaps(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ProviderCostGapsQuery>,
) -> Result<Json<ProviderCostGapsResponse>, ApiError> {
    require_admin(&state, &headers)?;
    let limit = query
        .limit
        .unwrap_or(DEFAULT_FAILURE_LIMIT)
        .clamp(1, MAX_OPERATIONAL_LIMIT);
    let gaps = state.pricing.provider_cost_gaps(limit).await?;
    Ok(Json(ProviderCostGapsResponse {
        count: gaps.len(),
        truncated: gaps.len() as u32 == limit,
        gaps,
    }))
}

/// 对客目录的响应：与生成入口一样用 `data` 承载列表。
#[derive(Debug, Serialize)]
struct ModelCatalogResponse {
    data: Vec<ModelCatalogEntry>,
}

/// 目录里的一条：型号的公开身份 + 它那份发布的合同。
///
/// 字段名是对客协议的取值，与内部的 [`PublishedModel`] 分开：内部字段改名不该动对客协议。
#[derive(Debug, Serialize)]
struct ModelCatalogEntry {
    /// 客户端提交 `model` 时用的名字——**平台对客名**（网关模型名）。
    name: String,
    /// 厂商标识：目录属性，同一个厂商模型可以由多条渠道供给。
    ///
    /// 叫 `vendor_id` 而不是 `vendor`：它是厂商的**标识**，与 `catalog.vendor_models.vendor_id`
    /// 同义；对客协议里换名字比内部换名字代价大，因此这里一次说清。
    vendor_id: String,
    /// 合同修订。
    revision: String,
    /// 该模型的调用方合同（发布的 JSON Schema），客户端据此建表单。
    contract: Value,
}

impl From<PublishedModel> for ModelCatalogEntry {
    fn from(model: PublishedModel) -> Self {
        let PublishedModel {
            gateway_model,
            vendor_id,
            native_revision,
            capability_schema,
        } = model;
        Self {
            name: gateway_model.clone(),
            vendor_id,
            revision: native_revision,
            contract: consumer_contract(capability_schema, &gateway_model),
        }
    }
}

/// 对客投射：把合同正文里的 `model.const` 换成**平台对客名**。
///
/// 存的那份合同**不动**（合同行不可变：旧 Job 事后读到的必须与它受理时逐字相同），只在交给
/// 调用方时替换这一个字段。为什么可以替换：`model` 一直是**平台字段**——受理期平台本来就用
/// 调用方给的 `model` 覆盖它（见 `contract_parameter_face`）；而合同里那个常量写的是**厂商
/// 原生名**，原生名不进对客面。
///
/// 替换的位置、以及"合同没声明这个常量时不动它"，由 [`replace_contract_model_identity`] 一处
/// 决定：发布期校验与这里必须指向合同里同一个字段，各写一遍迟早对不上。
fn consumer_contract(mut contract: Value, gateway_model: &str) -> Value {
    replace_contract_model_identity(&mut contract, gateway_model);
    contract
}

/// 对客目录：当前**真的能调**的模型，以及每个模型那份发布的合同。
///
/// **公开，不校验 Key**：客户端得先知道有哪些模型、各自的参数面长什么样，才建得出表单；
/// 表单都还没建起来的时候先要凭证，等于逼调用方为了看一眼目录去开户。目录本身只有型号身份
/// 与合同，不含渠道、供给、驱动或任何执行记录，公开它不泄露内部信息。
/// 取数与判据在仓库层（与受理期选路同一条），这里只做投射。
async fn list_models(
    State(state): State<AppState>,
) -> Result<Json<ModelCatalogResponse>, ApiError> {
    let data = state
        .runtime
        .published_models()
        .await?
        .into_iter()
        .map(ModelCatalogEntry::from)
        .collect();
    Ok(Json(ModelCatalogResponse { data }))
}

/// 管理员读：网关模型清单的响应。
///
/// 用 `gateway_models` 包一层而不是直接回数组：这条视图将来只增字段（候选、定价），
/// 有外层对象才不会每加一样就改一次响应的顶层形状。
#[derive(Debug, Serialize)]
struct GatewayModelsResponse {
    gateway_models: Vec<GatewayModelView>,
}

/// 管理员读：当前有生效定义的网关模型，一条一项，带候选清单与运维开关。
///
/// **需要管理员凭证**：它是运营视图，与对客目录不是一回事——对客目录只有型号身份与合同，
/// 这里带厂商原生名、候选、承载面与映射。**不回显渠道凭证**（`credential_env` 只是变量名）。
/// 全部数据来自数据库（生效修订 + 运维开关），不读缓存、不需要直查库。
async fn list_gateway_models(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<GatewayModelsResponse>, ApiError> {
    require_admin(&state, &headers)?;
    Ok(Json(GatewayModelsResponse {
        gateway_models: state.runtime.gateway_models().await?,
    }))
}

/// 运维开关的请求体：**唯一可变位**就是它。
///
/// `deny_unknown_fields`：把"想顺手改候选/改合同"的请求直接拒掉，而不是静默忽略——
/// 忽略会让调用方以为改成功了，而定义只能由发布产生。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetGatewayModelEnabledBody {
    enabled: bool,
}

/// 管理员写：启停一个网关模型。
///
/// 关闭的语义：该名字从对客目录消失、受理得到"模型不存在"；**已受理的 Job 不受影响**
/// （它们固定的是受理时那一版）。没有发布过的名字是 404——这里不创建任何东西。
async fn set_gateway_model_enabled(
    State(state): State<AppState>,
    Path(gateway_model): Path<String>,
    headers: HeaderMap,
    Json(body): Json<SetGatewayModelEnabledBody>,
) -> Result<StatusCode, ApiError> {
    require_admin(&state, &headers)?;
    state
        .runtime
        .set_gateway_model_enabled(&gateway_model, body.enabled, "admin-api")
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 受理请求：**平铺**的模型参数 + 图片字段（`image` 与 `image_urls` 同义二选一，`mask` 是遮罩）。
///
/// 图片直接是公网 URL 或 `data:image/…;base64,…`——平台不换 id、不上传、不落盘。
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

/// 取参考图与遮罩：认哪些字段、空值怎么算，全在 `seeai_domain` 里；这里只把领域的话翻成对客的
/// `invalid_parameter`。
///
/// 受理侧只认契约字段名（`image`、`image_urls`、`mask`），**不按名字像不像图片去判**：渠道
/// 文档里的一手参数（例如带角色的图片列表）因此不会被平台误截；它们留在请求参数里，
/// 由选路后的声明面过滤决定留不留。
fn take_contract_image_inputs(
    parameters: &mut Map<String, Value>,
) -> Result<ImageInputs, ApiError> {
    seeai_domain::take_contract_image_inputs(parameters)
        .map_err(|message| ApiError::bad_request("invalid_parameter", message))
}

/// 对客的成功响应：只有 `created` 与 `data`。
///
/// 这里**结构上**就没有 job、没有任务的字段——内部的执行记录不投射成对客协议；
/// `data` 每项只有渠道给的 `url` 或 `b64_json`。
#[derive(Debug, Serialize)]
struct SyncImageResponse {
    created: i64,
    data: Vec<GeneratedImage>,
}

/// 对客的错误信封（沿用 OpenAI 的形状），对客码仍是平台那三个。
#[derive(Debug, Serialize)]
struct SyncErrorResponse {
    error: SyncErrorBody,
}

#[derive(Debug, Serialize)]
struct SyncErrorBody {
    message: &'static str,
    #[serde(rename = "type")]
    error_type: &'static str,
    code: &'static str,
}

/// generations 入口（JSON）：与 edits 入口**是同一个能力**，只是请求编码不同。
///
/// 分支只看请求里有没有参考图 / 遮罩，不由端点断言——带图的 generations、不带图的 edits
/// 都是合法请求。
async fn generate_image(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateGenerationBody>,
) -> Result<Response, ApiError> {
    let account_id = authenticate(&state, &headers).await?;
    let mut parameters = body.parameters;
    let inputs = take_contract_image_inputs(&mut parameters)?;
    run_sync_generation(
        &state,
        account_id,
        &headers,
        parameters,
        inputs.reference_images,
        inputs.mask,
    )
    .await
}

/// edits 入口（`multipart/form-data`）：`image` 与 `mask` 是**文件部件**。
///
/// 文件只留在内存里，转成 `data:` URL 语义交给 Driver——**不落盘、不上传**。
/// 没有 `image` 的 edits 同样合法（那就是文生图）。
/// 图片字段也可以走文本部件（与 JSON 入口同一套语义），但同一个字段不能既当文件又当文本。
async fn edit_image(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Response, ApiError> {
    let account_id = authenticate(&state, &headers).await?;
    let mut parameters = Map::new();
    let mut file_inputs = ImageInputs::default();
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| ApiError::bad_request("invalid_multipart", error.to_string()))?
    {
        let name = field.name().unwrap_or_default().to_owned();
        // 只有契约字段名才是平台自己管的图片部件，带文件名的就当图片字节；其余名字按文本参数
        // 留在请求里，等选路后按候选的声明面处置（渠道文档里的一手参数不会被"像图片"就截走，
        // 没被候选声明的名字也不会跟着请求走去上游）。
        match contract_image_parameter_kind(&name) {
            Some(ImageParameterKind::Reference) if field.file_name().is_some() => {
                file_inputs
                    .reference_images
                    .push(form_image_data_url(field).await?);
            }
            Some(ImageParameterKind::Mask) if field.file_name().is_some() => {
                file_inputs.mask = Some(form_image_data_url(field).await?);
            }
            _ => {
                let text = field.text().await.map_err(|error| {
                    ApiError::bad_request("invalid_multipart", error.to_string())
                })?;
                parameters.insert(name.clone(), form_scalar(&name, &text));
            }
        }
    }
    let text_inputs = take_contract_image_inputs(&mut parameters)?;
    if !text_inputs.reference_images.is_empty() && !file_inputs.reference_images.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_parameter",
            "image was given both as a file part and as a text field",
        ));
    }
    if text_inputs.mask.is_some() && file_inputs.mask.is_some() {
        return Err(ApiError::bad_request(
            "invalid_parameter",
            "mask was given both as a file part and as a text field",
        ));
    }
    let reference_images = if file_inputs.reference_images.is_empty() {
        text_inputs.reference_images
    } else {
        file_inputs.reference_images
    };
    run_sync_generation(
        &state,
        account_id,
        &headers,
        parameters,
        reference_images,
        file_inputs.mask.or(text_inputs.mask),
    )
    .await
}

/// 把 multipart 的图片部件转成 `data:` URL：字节只在内存里过一手，平台不保存。
async fn form_image_data_url(field: Field<'_>) -> Result<String, ApiError> {
    let media_type = field
        .content_type()
        .map(str::to_owned)
        .unwrap_or_else(|| "image/png".to_owned());
    let bytes = field
        .bytes()
        .await
        .map_err(|error| ApiError::bad_request("invalid_multipart", error.to_string()))?;
    Ok(format!(
        "data:{media_type};base64,{}",
        STANDARD.encode(&bytes)
    ))
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

/// 两个入口共用的受理与响应：内部照旧走 Job 流水线，对外**等它跑到终态**再回图片。
///
/// 同步门面：等不到就按失败回 504——对客只说"这次没在时限内拿到结果"，不提内部的执行记录、
/// 也不指路任何查询接口（对客没有这样的接口）。
async fn run_sync_generation(
    state: &AppState,
    account_id: AccountId,
    headers: &HeaderMap,
    mut parameters: Map<String, Value>,
    reference_images: Vec<String>,
    mask: Option<String>,
) -> Result<Response, ApiError> {
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
            reference_images,
            mask,
            idempotency_key: idempotency_key(headers),
        })
        .await?;
    let job_id = job.id;
    let deadline = tokio::time::Instant::now() + state.sync_wait;
    loop {
        let view = state.generations.get(account_id, job_id).await?;
        match view.state.as_str() {
            "succeeded" => return Ok(sync_image_response(view)),
            "failed" | "reconciliation_required" => {
                let code = view.error_code.as_deref().unwrap_or("platform_unavailable");
                return Ok(sync_error_response(code));
            }
            _ if tokio::time::Instant::now() >= deadline => {
                return Ok(sync_error_response_with(
                    StatusCode::GATEWAY_TIMEOUT,
                    "result_pending",
                    "server_error",
                    "the request did not finish within the time limit; it is treated as failed",
                ));
            }
            _ => tokio::time::sleep(Duration::from_millis(250)).await,
        }
    }
}

/// 成功：`{created, data:[{url|b64_json}]}`——渠道给什么就是什么，平台不下载、不转码。
fn sync_image_response(view: JobView) -> Response {
    Json(SyncImageResponse {
        created: view.updated_at.timestamp(),
        data: view.data.unwrap_or_default(),
    })
    .into_response()
}

/// 失败：仍用 OpenAI 的错误信封，对客码沿用平台那三个。
///
/// 落库的 `error_code` 有 CHECK 约束保证只可能是这三个；认不出的按平台侧故障说。
fn sync_error_response(public_code: &str) -> Response {
    match public_code {
        "content_rejected" => sync_error_response_with(
            StatusCode::BAD_REQUEST,
            "content_rejected",
            "invalid_request_error",
            "the submitted content was rejected",
        ),
        "outcome_unknown" => sync_error_response_with(
            StatusCode::BAD_GATEWAY,
            "outcome_unknown",
            "server_error",
            "the platform could not confirm the outcome of this request",
        ),
        _ => sync_error_response_with(
            StatusCode::BAD_GATEWAY,
            "platform_unavailable",
            "server_error",
            "the platform could not complete this request",
        ),
    }
}

fn sync_error_response_with(
    status: StatusCode,
    code: &'static str,
    error_type: &'static str,
    message: &'static str,
) -> Response {
    (
        status,
        Json(SyncErrorResponse {
            error: SyncErrorBody {
                message,
                error_type,
                code,
            },
        }),
    )
        .into_response()
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
            // 调用方这次请求在参数上不成立（例如合同没声明图片字段却带了图）：与一般校验失败分开，
            // 说得更具体，调用方才知道该去掉哪个字段或换模型。
            ApplicationError::InvalidParameter(_) => (StatusCode::BAD_REQUEST, "invalid_parameter"),
            ApplicationError::NoEligibleOffering(ref reason) => {
                // 请求本身没违反合同，是平台的供给面承载不了它：对客说成平台侧故障，不是参数错。
                // 它发生在受理之前、没有 Job 可以记录，所以这里留一条日志——平台侧的供给问题
                // 必须能被运营发现（"一条候选都承载不了"往往意味着发布时少声明了一个字段）。
                tracing::warn!(reason = %reason, "no offering can carry the request");
                (StatusCode::SERVICE_UNAVAILABLE, "platform_unavailable")
            }
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

/// 同步入口等任务跑完的窗口（秒，默认 120）：超了就按失败回 504——对客没有可查询的
/// 执行记录，所以这个窗口之外拿不到图，只能由调用方自己重来。
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
