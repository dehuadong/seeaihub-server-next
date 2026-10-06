use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Extension, Multipart, Path, Query, State, multipart::Field},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post, put},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use seeai_adapter_aihubmix::AihubmixAdapterFactory;
use seeai_adapter_apimart::ApimartAdapterFactory;
use seeai_adapter_object_storage::OssObjectStorage;
use seeai_adapter_sdk::{DecodedImage, InputImage};
use seeai_alert_webhook::WebhookAlertSink;
use seeai_application::{
    AccelerationService, AccountSummary, AccountsService, AdapterRegistry, ApplicationError,
    AuthAttemptLimits, CachePolicy, CursorPosition, CustomerBillingQuery, CustomerLedgerQuery,
    CustomerUsageKind, CustomerUsageQuery, CustomerUsageScope, CustomerUsageStatus, CustomerView,
    DirectExecutionError, DirectExecutionLimits, DirectExecutionRequest, DirectExecutionService,
    ExecutionLookup, ExecutionRepository, GatewayModelView, GeneratedImage,
    GenerationDailySpendLimit, GenerationRateLimit, HISTORY_CURSOR_KEY_LEN, HistoryFilter,
    HistoryStream, HubRepository, IdentityService, LedgerAuditor, LedgerEntryView,
    MAX_OPERATIONAL_LIMIT, NO_CONTRACT_MAX_OUTPUT_IMAGES, NewFxRate, PlatformAlerter,
    PricingService, ProviderCostGapView, ProviderFailureKind, ProviderFailureQuery,
    ProviderFailureView, PublicErrorCode, PublishRuntimeCommand, ReconciliationService,
    RecordedRequestInput, RefundReconciliationCommand, RequestCostCeiling, RequestFingerprintKeys,
    RequestTimeoutPolicy, RetryPolicy, RoutePolicyService, RuntimeService, SelectableOfferingView,
    UsageAmounts, decode_history_cursor, encode_history_cursor, invalid_history_cursor,
    settle_reserve_from_env, with_admin_id,
};
use seeai_application::{
    ApiKeyIdentity, HeadObjectRequest, ImageUploadConfig, ImageUploadError, ImageUploadService,
    NeverCancelled, ObjectMetadata, ObjectStorage, ObjectStorageCredentials, PutObjectRequest,
    UploadCancellation,
};
use seeai_cache_redis::RedisCache;
use seeai_domain::{
    AccountId, ChannelId, ImageInputs, ImageParameterKind, JobId, LedgerEntryKind, OfferingId,
    PublishedModel, RequestJsonError, RequestJsonLimits, RequestParameters, RoutePolicy,
    RouteStrategy, UploadWriteFailure, contract_image_parameter_kind,
    replace_contract_model_identity,
};
use seeai_persistence::{
    PgHubRepository, material_import::import_supply_materials_from_env,
    material_import::public_docs_dir, max_declared_output_images,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, env, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use tower_http::{request_id::MakeRequestUuid, trace::TraceLayer};
use tracing::info;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

mod supervisor;
use supervisor::{
    AuthenticatedAccount, ClientAddr, ConnectionScope, ExecutionHandle, ExecutionLease,
    OwnershipRenewalConfig, SendHold, SendLease, SlowRead, Supervisor, SupervisorConfig,
};
#[cfg(target_os = "linux")]
use supervisor::{TransportConfig, TransportObservability, serve as serve_transport};

/// 平台直接执行时随进程装配的一份用例与它的 Supervisor。
///
/// env 开关默认关闭；关着时这个字段是 `None`，两条图片入口逐字走旧路径（建 Job、Worker 领取、
/// 轮询结果），不受这里任何代码影响。
#[derive(Clone)]
struct DirectGeneration {
    service: Arc<DirectExecutionService>,
    supervisor: Arc<Supervisor>,
    /// 请求 JSON 结构的四条计数上限：缺省从支持的 wire 范围推导，配置只能收紧（RFC 0018 §2.2）。
    request_json_limits: RequestJsonLimits,
}

/// 上传端点随进程装配的一份用例、限流与配置。
#[derive(Clone)]
struct ImageUpload {
    service: Arc<ImageUploadService>,
    /// 上传的每 API Key 限流走独立命名空间，不挤占生成的配额。
    acceleration: Arc<AccelerationService>,
    config: Arc<ImageUploadConfig>,
}

/// 未配置上传存储时占位的对象存储端口：上传用例在碰它之前就返回 503，它不该被调用。
struct UnconfiguredObjectStorage;

#[async_trait]
impl ObjectStorage for UnconfiguredObjectStorage {
    async fn put_object(
        &self,
        _request: PutObjectRequest<'_>,
        _credentials: ObjectStorageCredentials<'_>,
    ) -> Result<(), UploadWriteFailure> {
        Err(UploadWriteFailure::Terminal)
    }

    async fn head_object(
        &self,
        _request: HeadObjectRequest<'_>,
        _credentials: ObjectStorageCredentials<'_>,
    ) -> Result<ObjectMetadata, UploadWriteFailure> {
        Err(UploadWriteFailure::Terminal)
    }
}

/// 客户端断开就是上传的取消信号：重试编排读到它就停止取退避与下一次 PUT。
impl UploadCancellation for ConnectionScope {
    fn is_cancelled(&self) -> bool {
        ConnectionScope::is_cancelled(self)
    }
}

/// 直接执行的渠道凭证：只从环境变量读，交给 Adapter，不落库、不日志。
#[derive(Debug, Default)]
struct EnvironmentCredentialProvider;

impl seeai_application::CredentialProvider for EnvironmentCredentialProvider {
    fn resolve(
        &self,
        reference: &str,
    ) -> Result<seeai_adapter_sdk::ProviderCredential, ApplicationError> {
        let value = env::var(reference)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                ApplicationError::Configuration(format!(
                    "provider credential environment {reference} is missing"
                ))
            })?;
        seeai_adapter_sdk::ProviderCredential::new(value)
            .map_err(|error| ApplicationError::Configuration(error.to_string()))
    }
}

#[derive(Clone)]
struct AppState {
    admin_token: Arc<str>,
    identity: IdentityService,
    runtime: RuntimeService,
    reconciliation: ReconciliationService,
    /// 账实核对：管理员按账户触发，后台执行；只读账本与账户当前值，不在资金路径上。
    ledger_audit: Arc<LedgerAuditor>,
    /// 定价侧的管理员面：折算率的录入与取值（汇率不进不可变修订）。
    pricing: PricingService,
    /// 账户面的管理员用例：建账户与充值——两件事都要在提交成功后把余额写进缓存。
    accounts: AccountsService,
    /// 路由策略的管理员面：读清单与写入（运行期配置，不进不可变修订）。
    route_policies: RoutePolicyService,
    /// 同步直接执行：唯一执行路径，没有开关。
    direct: Arc<DirectGeneration>,
    /// 上传端点：写入上传存储换公网 URL，不计费、不计量、不建执行记录。
    upload: Arc<ImageUpload>,
    /// 健康探测的依赖判据：探一次事实源是否可达（只 `SELECT 1`）。
    repository: Arc<dyn HubRepository>,
    /// 会话有效期（管理员与客户同一档）：部署期配置，缺省 12 小时。
    session_ttl: ChronoDuration,
    /// 口令重置令牌的有效期：比会话更短，缺省 30 分钟。
    password_reset_ttl: ChronoDuration,
    /// 客户历史翻页游标的加密密钥：部署期配置，同一部署的所有 API 实例必须一致；缺失或格式无效时
    /// 进程启动失败（`docs/design/0014-customer-console-navigation-and-history.md` §5）。
    history_cursor_key: [u8; HISTORY_CURSOR_KEY_LEN],
    /// 公开鉴权端点来源维采信的受信头（`AUTH_SOURCE_HEADER`）；不设时退回连接对端地址。
    auth_source_header: Option<header::HeaderName>,
    /// 平台对客基址（`SEE_BASEURL`）：公共使用文档的链接按它写成绝对地址。
    see_base_url: String,
}

impl AppState {
    /// 把 bearer 凭据认成一个管理员，返回**是哪个管理员**（共享令牌时为 `None`——它不指向具体的人）。
    ///
    /// 两条路：**共享令牌**（自动化、端到端测试与运维自救）与**会话令牌**（人在浏览器里登录）。
    /// 两条都不命中时的答复与"共享令牌写错了"**完全一样**（同一个状态码与错误码）：调用方分不出
    /// 自己拿的是哪种凭据，也分不出凭据是不存在还是过期。
    async fn admin_identity(&self, headers: &HeaderMap) -> Result<Option<Uuid>, ApiError> {
        let token = admin_bearer_token(headers)?;
        if constant_time_eq(token.as_bytes(), self.admin_token.as_bytes()) {
            return Ok(None);
        }
        match self.identity.authenticate_admin_session(token).await {
            Ok(Some((admin_id, _))) => Ok(Some(admin_id)),
            Ok(None) => Err(admin_forbidden()),
            // 会话那一侧出错（库不可用等）是平台故障，不能伪装成"凭据不对"。
            Err(error) => Err(ApiError::from(error)),
        }
    }

    /// 只认管理与会话两种凭据，并把身份放进这次请求的审计上下文。
    ///
    /// 只认**会话令牌**：用于"关于我自己"的三条端点（认身份、改口令、退出）。
    ///
    /// 共享令牌不指向任何一个管理员，用它回答"我是谁"只能编一个身份出来——所以这里不接受它，
    /// 答复仍是同一个"未授权"。返回会话对应的 `(admin_id, 邮箱)`。
    async fn require_admin_self(&self, headers: &HeaderMap) -> Result<(Uuid, String), ApiError> {
        let token = bearer_token(headers)?;
        match self.identity.authenticate_admin_session(token).await {
            Ok(Some(identity)) => Ok(identity),
            Ok(None) => Err(admin_forbidden()),
            Err(error) => Err(ApiError::from(error)),
        }
    }

    /// 这次请求的**来源**：按运维配置采信受信头，否则退回连接对端地址。
    ///
    /// 采信哪个头与它的部署前提见 [`auth_source_header`] 与设计 0016 §3。
    fn request_source(&self, headers: &HeaderMap, client: &ClientAddr) -> String {
        if let Some(name) = &self.auth_source_header
            && let Some(value) = headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
                .filter(|value| !value.is_empty())
        {
            return value.to_owned();
        }
        match client.0 {
            Some(addr) => addr.ip().to_string(),
            None => "unknown".to_owned(),
        }
    }

    /// 把 bearer 凭据认成一个客户，返回 `(customer_id, account_id)`。
    async fn require_customer(&self, headers: &HeaderMap) -> Result<(Uuid, Uuid), ApiError> {
        let token = bearer_token(headers)?;
        match self.identity.authenticate_customer_session(token).await {
            Ok(Some(identity)) => Ok(identity),
            Ok(None) => Err(unauthorized()),
            Err(error) => Err(ApiError::from(error)),
        }
    }
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
    // 平台对客基址：模型说明与公共文档的链接按它写成绝对地址。它是**部署期**事实——发布走管理端
    // 主机、读取走对客主机，从请求头取会把管理端地址写进不可变版本，所以必须显式配置、启动即校验。
    let see_base_url = required_env("SEE_BASEURL")?;
    seeai_application::model_document::validate_base_url(&see_base_url)?;
    let repository = Arc::new(PgHubRepository::connect(&database_url, 10).await?);
    repository.migrate().await?;
    // 供给素材的幂等导入（**工程侧**的动作）：渠道与 Offering 的来源是工程师写的素材。目录由
    // `SUPPLY_MATERIAL_DIR` 给，**不设时默认 `config/bootstrap`**（仓库、systemd 与 Docker 镜像
    // 都把素材放在工作目录下）；设成空白＝显式不导入，测试库与开发库用它保持干净。目录不存在或
    // 里面没有素材时什么都不做——导入不该让服务起不来。它排在迁移之后、装配之前：导入写的是供给
    // 目录，不发布修订、也不应答请求，因此与下面的超时校验（读已发布修订的合同）互不影响。
    let imported = import_supply_materials_from_env(repository.pool()).await?;
    if imported.materials > 0 {
        info!(
            materials = imported.materials,
            offerings = imported.offerings,
            price_plans = imported.price_plans,
            "supply materials imported"
        );
    }
    // 超时链整条校验：输出张数上限取自**合同自己声明的取值面**（读库，所以要连库之后才知道），
    // 比较因此比的是"合同允许的最大一档请求"。这一进程持有**对客同步等待窗口**——窗口短于上游
    // 超时就是"消费者拿到 504、而上游还在生成、照样计费"那条路。校验不过就点名报错退出，不让
    // 服务带着一条断链跑起来。
    let (max_output_images, declared_by, undecodable) =
        max_declared_output_images(repository.pool(), NO_CONTRACT_MAX_OUTPUT_IMAGES).await?;
    let timeouts =
        RequestTimeoutPolicy::from_env(max_output_images).map_err(anyhow::Error::from)?;
    timeouts.validate().map_err(anyhow::Error::from)?;
    info!(
        sync_wait_seconds = timeouts.sync_wait.as_secs(),
        provider_timeout_seconds = timeouts.provider_timeout.as_secs(),
        base_seconds = timeouts.base.as_secs(),
        included_images = timeouts.included_images,
        per_image_seconds = timeouts.per_image.as_secs(),
        max_output_images = timeouts.max_output_images,
        declared_by = declared_by.as_deref().unwrap_or("no active contract"),
        undecodable_contracts = undecodable,
        "the timeout chain is consistent"
    );
    let execution_port: Arc<dyn ExecutionRepository> = repository.clone();
    let repository_port: Arc<dyn HubRepository> = repository;
    // 组合工厂：按 adapter_key 分派到各渠道自己的 Driver（纯装配）。
    let adapters: Arc<dyn seeai_application::AdapterFactory> =
        Arc::new(AdapterRegistry::new(vec![
            Arc::new(AihubmixAdapterFactory),
            Arc::new(ApimartAdapterFactory),
        ]));
    // 加速层：`REDIS_URL` 没配就是没有缓存——那时这一层是空操作（余额写穿、速率计数都不落缓存），
    // 行为与没有它时逐位相同。配了但连不上只是写不进、读不到，业务事实一律以数据库为准。
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
    } else {
        // 速率计数就落在这个缓存上：没有缓存时它无处可落，限流这一层等于不生效（见
        // `AccelerationService::consume_request_slot`）。这是部署期看得见的事实，不是静默降级。
        tracing::warn!(
            "no cache service configured; the per-API-key rate limit does not apply in this process"
        );
    }

    // 上传端点与上传存储的配置：整组上传存储变量都不给＝未配置，进程照常启动；只给一部分或
    // 形状不合法＝拒绝启动并点名变量。对象存储只有阿里云 OSS 一种，不读数据库、不做活体探测。
    let upload_config = ImageUploadConfig::from_env().map_err(anyhow::Error::from)?;

    // 图片生成只有这一条执行路径：本进程直接调 Provider，不建生成 Job、不轮询结果。指纹密钥与
    // 渠道凭证在这里无条件读取，缺任何一项都拒绝启动——不存在"关掉它就走旧路径"的开关。
    let direct_execution = {
        let keys = RequestFingerprintKeys::from_env().map_err(anyhow::Error::from)?;
        let settle_reserve = settle_reserve_from_env().map_err(anyhow::Error::from)?;
        // 请求 JSON 结构的四条计数上限：缺省从 16 MiB 的 wire 范围推导，配置只允许收紧。
        let request_json_limits = request_json_limits_from_env()?;
        // 执行所有权租约：begin_submission 按它落 lease_expires_at，Supervisor 按它的三分之一续约。
        let ownership_lease = duration_from_env("GENERATION_EXECUTION_LEASE_SECONDS", 60)?;
        let service = DirectExecutionService::new(
            repository_port.clone(),
            execution_port.clone(),
            adapters.clone(),
            Arc::new(EnvironmentCredentialProvider),
            keys,
            timeouts,
            DirectExecutionLimits {
                max_account_in_flight: generation_max_concurrent_jobs()?,
                max_channel_in_flight: generation_max_channel_in_flight()?,
                default_hold_microusd: generation_max_cost_microusd()?,
            },
        )
        .with_settle_reserve(settle_reserve)
        .with_ownership_lease(ownership_lease)
        // 请求内安全重投沿用旧 Worker 那一组运维取值（次数与退避基），不另立一套。
        .with_retry_policy(RetryPolicy::from_env().map_err(anyhow::Error::from)?)
        // 每日扣费上限：GENERATION_MAX_DAILY_SPEND_MICROUSD，受理前按账户当日已花判定。
        .with_daily_spend_limit(generation_daily_spend_limit()?)
        .with_acceleration(acceleration.clone())
        .with_cost_ceiling(cost_ceiling()?);
        let max_memory_bytes =
            generation_env_usize("GENERATION_MAX_MEMORY_BYTES", DEFAULT_MAX_MEMORY_BYTES)?;
        // 单次执行的预留按各 Driver 声明的字节上限算：入口 wire、上游响应与编码膨胀可能同时存活，
        // 不能再用一个与真实响应无关的固定值（RFC 0018 §2）。读不到任何 Driver 的字节上限时
        // **拒绝启动**：静默退回一个更小的固定值，正是"把预算改小还装作没发生"。
        let descriptors: Vec<_> = [
            seeai_adapter_aihubmix::ADAPTER_KEY,
            seeai_adapter_apimart::ADAPTER_KEY,
        ]
        .iter()
        .filter_map(|key| adapters.descriptor(key))
        .collect();
        // 只读对账用独立、更小的响应上限（RFC 0018 §2.1）。配置值必须读得出来，且不超过任何一条
        // 声明了字节上限的 Adapter 的生成响应上限——对账读取绝不比生成读取更大，配错就拒绝启动。
        let reconciliation_read_bytes =
            seeai_adapter_sdk::reconciliation_read_bytes_from_env().map_err(anyhow::Error::msg)?;
        for descriptor in &descriptors {
            if reconciliation_read_bytes > descriptor.byte_limits.provider_response_bytes {
                anyhow::bail!(
                    "GENERATION_RECONCILIATION_READ_BYTES is {reconciliation_read_bytes} bytes, \
                     above the {} provider response limit declared by adapter {}; the \
                     reconciliation read must not be larger than the generation read",
                    descriptor.byte_limits.provider_response_bytes,
                    descriptor.key
                );
            }
        }
        // transport 用户态缓冲的上界：单次执行最多牵动的两类连接缓冲（HTTP/1 解析缓冲、HTTP/2
        // 发送缓冲）。它进单次预留，取值就是**配置自己的上限**，不是这里估出来的峰值——Hyper 在
        // 用户态的写缓冲不会超过这两项。
        let transport_buffer_bytes =
            generation_env_usize("API_MAX_BUFFER_BYTES", DEFAULT_HTTP1_MAX_BUF_BYTES)?
                .saturating_add(generation_env_usize(
                    "API_H2_MAX_SEND_BUFFER_BYTES",
                    DEFAULT_HTTP2_MAX_SEND_BUF_BYTES,
                )?);
        let execution_memory_bytes = descriptors
            .iter()
            .map(|descriptor| {
                descriptor
                    .byte_limits
                    .max_bytes_per_execution(transport_buffer_bytes)
            })
            .max()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "no registered adapter declared its byte limits; the per-execution memory \
                     reservation cannot be derived, so direct execution must not start"
                )
            })?;
        let send_slots = generation_env_usize("GENERATION_SEND_SLOTS", 64)?;
        let read_slots = generation_env_usize("GENERATION_READ_SLOTS", 64)?;
        let max_connections = generation_env_usize("API_MAX_CONNECTIONS", DEFAULT_MAX_CONNECTIONS)?;
        let h2_max_concurrent_streams = generation_env_usize(
            "API_H2_MAX_CONCURRENT_STREAMS",
            DEFAULT_H2_MAX_CONCURRENT_STREAMS,
        )?;
        // 执行名额：显式配置的按它自己校验；没配时按"内存预算 ÷ 单次预留"推导。推导出来的组合必然
        // 自洽，而显式配得比预算能覆盖的还多会被下面的组合校验拒绝——不静默把上限改小，也不让
        // 进程带着一份"有名额但永远取不到"的配置跑起来（RFC 0018 §2.3）。
        let execution_slots = match generation_env_usize_optional("GENERATION_EXECUTION_SLOTS")? {
            Some(slots) => slots,
            None => (max_memory_bytes / execution_memory_bytes).max(1),
        };
        validate_capacity_combination(&CapacityCombination {
            execution_slots,
            execution_memory_bytes,
            max_memory_bytes,
            read_slots,
            send_slots,
            max_connections,
            h2_max_concurrent_streams,
        })
        .map_err(anyhow::Error::msg)?;
        let supervisor = Supervisor::new(SupervisorConfig {
            execution_slots,
            max_memory_bytes,
            execution_memory_bytes,
            send_slots,
            read_slots,
            upload_slots: upload_config.slots,
            upload_memory_bytes: upload_config.max_request_bytes,
            upload_max_buffer_bytes: upload_config.max_buffer_bytes,
            upload_slow_read_timeout: upload_config.slow_read_timeout,
            slow_read_timeout: Duration::from_secs(generation_env_u64(
                "GENERATION_SLOW_READ_TIMEOUT_SECONDS",
                30,
            )?),
            shutdown_grace: Duration::from_secs(generation_env_u64(
                "GENERATION_SHUTDOWN_GRACE_SECONDS",
                25,
            )?),
            total_deadline: timeouts.sync_wait,
            finalization_grace: settle_reserve,
            send_window: Duration::from_secs(generation_env_u64(
                "GENERATION_SEND_TIMEOUT_SECONDS",
                30,
            )?),
            ownership: Some(OwnershipRenewalConfig {
                executions: execution_port.clone(),
                lease: ownership_lease,
            }),
        })
        .map_err(anyhow::Error::from)?;
        info!(
            execution_slots,
            send_slots,
            read_slots,
            max_connections,
            h2_max_concurrent_streams,
            max_memory_bytes,
            execution_memory_bytes,
            reconciliation_read_bytes,
            "direct synchronous execution is enabled"
        );
        Arc::new(DirectGeneration {
            service: Arc::new(service),
            supervisor,
            request_json_limits,
        })
    };
    // 上传用例：存储配置缺失时用占位端口（用例在碰它之前就返回 503），配置齐全时用 OSS 适配器。
    // 访问密钥不进这里：它们按请求经凭证端口解析。
    let upload = {
        let storage: Arc<dyn ObjectStorage> = match upload_config.storage.as_ref() {
            Some(storage) => Arc::new(
                OssObjectStorage::new(storage.region.clone(), upload_config.request_timeout)
                    .map_err(anyhow::Error::from)?,
            ),
            None => Arc::new(UnconfiguredObjectStorage),
        };
        Arc::new(ImageUpload {
            service: Arc::new(ImageUploadService::new(
                storage,
                Arc::new(EnvironmentCredentialProvider),
                upload_config.clone(),
            )),
            acceleration: acceleration.clone(),
            config: Arc::new(upload_config.clone()),
        })
    };

    // 账实核对：比对**账户当前值**与它自己的**明细**。它**不是**上面那个缓存对账——那个问的是
    // "缓存里的值还是不是库里的值"，做法是以库为准覆盖缓存，改的是缓存；这条问的是"库里那个
    // 余额与占用还是不是它自己那本账的和"，两边都是库里的**事实**。它只发现、不改账：不一致就
    // 告警并建一条对账案例，改账是人的决定。**没有默认周期**：只由管理员按账户触发
    // （`POST /api/v1/accounts/{id}/ledger-audit`），在后台任务里执行，不在资金路径上。
    let auditor = LedgerAuditor::new(repository_port.clone());
    // 告警出口与 Worker 共用同一组配置项与同一个实现：没配 `PROVIDER_ALERT_WEBHOOK` 就没有
    // 出口，那时核对照样发现、照样建案，只是一条都不外发。地址写错在构造时就失败，不让进程
    // 带着一个"永远发不出去"的出口跑起来。
    let ledger_audit = match WebhookAlertSink::from_env()? {
        Some(sink) => {
            info!("the platform alert webhook is configured for the ledger audit");
            auditor.with_alerts(Arc::new(PlatformAlerter::new(Arc::new(sink))))
        }
        None => auditor,
    };
    let ledger_audit = Arc::new(ledger_audit);
    let state = AppState {
        admin_token,
        identity: IdentityService::new(repository_port.clone())
            .with_rate_limit(acceleration.clone(), generation_rate_limit()?)
            .with_auth_attempt_limits(AuthAttemptLimits::from_env()?),
        runtime: RuntimeService::new(repository_port.clone(), adapters, see_base_url.clone())
            .with_acceleration(acceleration.clone())
            .with_cost_ceiling(cost_ceiling()?),
        reconciliation: ReconciliationService::new(repository_port.clone())
            .with_acceleration(acceleration.clone()),
        ledger_audit,
        pricing: PricingService::new(repository_port.clone()),
        accounts: AccountsService::new(repository_port.clone())
            .with_acceleration(acceleration.clone()),
        route_policies: RoutePolicyService::new(repository_port.clone()),
        repository: repository_port,
        // 会话与重置令牌的有效期：部署期取值（缺省 12 小时 / 30 分钟）。
        session_ttl: session_ttl()?,
        password_reset_ttl: password_reset_ttl()?,
        history_cursor_key: history_cursor_key()?,
        auth_source_header: auth_source_header()?,
        see_base_url,
        direct: direct_execution.clone(),
        upload: upload.clone(),
    };
    // 首次开放目录 `documentation_url` 之前，为每个当前可调用模型补齐文档快照（Spec 0008 §4）：
    // 缺素材就让进程起不来并点名，不隐藏模型，也不返回伪造正文。
    let backfilled = state.runtime.ensure_current_model_documents().await?;
    if backfilled > 0 {
        info!(models = backfilled, "current model documents backfilled");
    }
    // 引导管理员账号：运维给的环境变量只在账号**不存在**时写入，重复启动不会把改过的口令打回原值。
    seed_admin_account(&state).await?;
    // **管理面**挂一条认证中间件：一处认证、一处把"是哪个管理员"放进这次请求的审计上下文，因此所有
    // 写操作的 `admin_id` 都能填对，而不必改十几个处理器的签名。它只作用于**已匹配**的路由，所以
    // 未注册的 `/api/v1/…` 仍然走到 fallback（JSON 404），不会被这里拦成 403。
    let admin = Router::new()
        .route("/api/v1/accounts", post(create_account).get(list_accounts))
        .route("/api/v1/accounts/{account_id}", get(read_account_balance))
        .route(
            "/api/v1/accounts/{account_id}/summary",
            get(read_account_summary),
        )
        .route(
            "/api/v1/accounts/{account_id}/entries",
            get(list_account_entries),
        )
        .route(
            "/api/v1/accounts/{account_id}/usage",
            get(list_account_usage),
        )
        .route("/api/v1/accounts/{account_id}/tag", put(set_account_tag))
        .route("/api/v1/accounts/{account_id}/name", put(rename_account))
        .route(
            "/api/v1/accounts/{account_id}/credits",
            post(credit_account),
        )
        .route(
            "/api/v1/accounts/{account_id}/ledger-audit",
            post(trigger_ledger_audit),
        )
        .route(
            "/api/v1/accounts/{account_id}/api-keys",
            post(issue_api_key),
        )
        .route(
            "/api/v1/accounts/{account_id}/password-reset",
            post(issue_customer_password_reset),
        )
        .route("/api/v1/customers", get(list_customers).post(open_customer))
        .route("/api/v1/customers/{customer_id}", get(read_customer_view))
        .route("/api/v1/admin/session", get(read_admin_session))
        .route("/api/v1/admin/password", put(change_admin_password))
        .route(
            "/api/v1/admin/password-resets",
            post(issue_admin_password_reset),
        )
        .route("/api/v1/fx-rates", put(upsert_fx_rate).get(list_fx_rates))
        .route("/api/v1/runtime-revisions", post(publish_runtime))
        .route("/api/v1/gateway-models", get(list_gateway_models))
        .route(
            "/api/v1/gateway-models/{gateway_model}",
            patch(set_gateway_model_enabled),
        )
        .route("/api/v1/offerings", get(list_selectable_offerings))
        .route(
            "/api/v1/offerings/{offering_id}",
            patch(set_offering_enabled),
        )
        .route("/api/v1/channels/{channel_id}", patch(set_channel_enabled))
        .route(
            "/api/v1/route-policies",
            get(list_route_policies).put(upsert_route_policy),
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
        .route("/api/v1/provider-cost-gaps", get(list_provider_cost_gaps))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_admin_middleware,
        ));
    // 图片入口的入口中间件：认证、速率与本机读取准入都在**消费正文之前**完成，账户放进 request
    // extension，handler 不再自己认证。
    let image_routes = Router::new()
        .route("/v1/images/generations", post(generate_image))
        .route("/v1/images/edits", post(edit_image))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_generation_access,
        ));
    // 上传入口：路由级正文上限盖过合并后的全局 16 MiB 上限（Spec 0007 §2.2）；认证、上传速率与
    // 本机上传读取许可都在消费正文之前完成。
    let upload_routes = Router::new()
        .route("/v1/uploads/images", post(upload_image))
        .route_layer(DefaultBodyLimit::max(state.upload.config.max_request_bytes))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_upload_access,
        ));
    // **公开与对客面**：不挂管理认证。登录、退出、凭令牌兑换各自认自己的凭据。
    let public = Router::new()
        .route("/health", get(health))
        .route(
            "/api/v1/admin/sessions",
            post(login_admin).delete(logout_admin),
        )
        .route(
            "/api/v1/admin/password-resets/redeem",
            post(redeem_admin_password_reset),
        )
        .route("/v1/models", get(list_models))
        .route("/v1/models/{name}/llms.txt", get(read_model_document))
        .route("/v1/docs/{*path}", get(read_public_document))
        .route("/v1/account", get(read_own_account))
        .route("/v1/customers", post(register_customer))
        .route(
            "/v1/customer/sessions",
            post(login_customer).delete(logout_customer),
        )
        .route("/v1/customer/password", put(change_customer_password))
        .route(
            "/v1/customer/password-resets/redeem",
            post(redeem_customer_password_reset),
        )
        .route(
            "/v1/customer/api-keys",
            get(list_customer_api_keys).post(issue_customer_api_key),
        )
        .route(
            "/v1/customer/api-keys/{key_id}",
            delete(revoke_customer_api_key),
        )
        .route("/v1/customer/account", get(read_customer_account))
        .route("/v1/customer/account/name", put(rename_customer_account))
        .route("/v1/customer/ledger", get(read_customer_ledger))
        .route("/v1/customer/usage", get(read_customer_usage))
        .route("/v1/customer/billing", get(read_customer_billing))
        .merge(image_routes)
        .merge(upload_routes);
    let app = admin
        .merge(public)
        .layer(DefaultBodyLimit::max(
            seeai_adapter_sdk::GATEWAY_REQUEST_WIRE_BYTES,
        ))
        .layer(tower_http::request_id::SetRequestIdLayer::new(
            header::HeaderName::from_static("x-request-id"),
            MakeRequestUuid,
        ))
        .layer(TraceLayer::new_for_http())
        .with_state(state);
    // 两份前端产物（`apps/web/dist`）由这里托管，**按主机名分发**：管理主机回运营后台、客户主机回
    // 客户控制台。它挂成 `fallback`，所以**路由表优先**——未注册的 `/api/v1/…` 与 `/v1/…` 仍然回
    // 既有的 JSON 404，不会被兜底成一份 HTML（那会让调用方把"路径写错了"读成"调用成功"）。
    //
    // **没有产物时也要挂这一个兜底**：什么都不挂的话，未注册路径会落到框架自带的 404（纯文本），
    // 而不是本服务约定的 JSON 错误体——调用方按 `error.code` 分流就会读不到东西。没有产物时
    // [`StaticSpa::serve`] 对任何路径都回同一个 JSON 404（实测踩到过：这一支只在没构建时暴露）。
    let app = match static_spa()? {
        Some(spa) => app.fallback(move |request: axum::extract::Request| {
            let spa = spa.clone();
            async move { spa.serve(request).await }
        }),
        None => app.fallback(|| async { not_found() }),
    };
    let listener = tokio::net::TcpListener::bind(bind).await?;
    info!(%bind, "api listening");
    // 直接执行依赖 Linux 的独立 socket 关闭事件检测（Spec 0005 §6）：没有等效检测的平台显式拒绝
    // 启动，不退回保存业务载荷的生成链路。
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (listener, app);
        bail!(
            "direct execution requires the Linux disconnect monitor (RFC 0018 §8.2); this platform \
             has no equivalent client-close detection, so the process refuses to start"
        );
    }
    // 停机：先停止新执行并给在飞任务有限收尾，进程退出前再等在飞任务收尾到宽限期上限；到点仍
    // 有残余时它们的账务事实由应用层的对账路径接管（RFC 0017 §5、§6）。
    #[cfg(target_os = "linux")]
    {
        // 预算观测：执行侧与连接侧各自维护真实许可的计数，这里按固定周期合成一条记录
        // （RFC 0018 §2.3）。周期配 0 表示不周期记录，逐次拒绝仍在各自的拒绝点记录。
        let transport_observability = Arc::new(TransportObservability::default());
        match generation_env_u64("GENERATION_OBSERVABILITY_INTERVAL_SECONDS", 30)? {
            0 => info!("periodic budget observation is disabled"),
            seconds => spawn_budget_observability(
                direct_execution.supervisor.clone(),
                Arc::clone(&transport_observability),
                Duration::from_secs(seconds),
            ),
        }
        let supervisor_for_shutdown = direct_execution.supervisor.clone();
        serve_transport(
            listener,
            app,
            transport_config()?,
            transport_observability,
            async move {
                shutdown_signal().await;
                supervisor_for_shutdown.begin_drain();
            },
        )
        .await?;
        direct_execution.supervisor.drain().await;
    }
    Ok(())
}

/// 按固定周期把执行侧与连接侧的预算计数写进 tracing。
///
/// 只记数量，不记图片或参数；周期本身是有界的（`GENERATION_OBSERVABILITY_INTERVAL_SECONDS`），
/// 逐次容量拒绝在各自的拒绝点记录（RFC 0018 §2.3）。
#[cfg(target_os = "linux")]
fn spawn_budget_observability(
    supervisor: Arc<Supervisor>,
    transport: Arc<TransportObservability>,
    interval: Duration,
) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        // 落后时按周期顺延，不在追赶时连发一串记录。
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            supervisor.record_observability();
            transport.record();
        }
    });
}

/// 连接驱动的容量与期限：每一项都是明确上限，默认值按单机 64 路执行容量给出。
#[cfg(target_os = "linux")]
fn transport_config() -> Result<TransportConfig> {
    let max_connections = generation_env_usize("API_MAX_CONNECTIONS", DEFAULT_MAX_CONNECTIONS)?;
    let max_connection_tasks = generation_env_usize("API_MAX_CONNECTION_TASKS", 64)?;
    let control_queue_capacity = generation_env_usize("API_MONITOR_CONTROL_QUEUE", 1024)?;
    Ok(TransportConfig {
        max_connections,
        max_connection_tasks,
        monitor: supervisor::MonitorConfig {
            max_connections,
            control_queue_capacity,
            event_batch: 256,
        },
        registration_timeout: Duration::from_millis(generation_env_u64(
            "API_MONITOR_CONFIRM_MILLIS",
            2_000,
        )?),
        release_timeout: Duration::from_millis(generation_env_u64(
            "API_MONITOR_CONFIRM_MILLIS",
            2_000,
        )?),
        shutdown_grace: Duration::from_secs(generation_env_u64(
            "GENERATION_SHUTDOWN_GRACE_SECONDS",
            25,
        )?),
        http1_max_headers: generation_env_usize("API_MAX_HEADERS", 128)?,
        http1_max_buf_size: generation_env_usize(
            "API_MAX_BUFFER_BYTES",
            DEFAULT_HTTP1_MAX_BUF_BYTES,
        )?,
        http2_max_concurrent_streams: u32::try_from(generation_env_usize(
            "API_H2_MAX_CONCURRENT_STREAMS",
            DEFAULT_H2_MAX_CONCURRENT_STREAMS,
        )?)
        .context("API_H2_MAX_CONCURRENT_STREAMS must fit in 32 bits")?,
        http2_max_send_buf_size: generation_env_usize(
            "API_H2_MAX_SEND_BUFFER_BYTES",
            DEFAULT_HTTP2_MAX_SEND_BUF_BYTES,
        )?,
        http2_max_header_list_size: u32::try_from(generation_env_usize(
            "API_H2_MAX_HEADER_LIST_BYTES",
            64 * 1024,
        )?)
        .context("API_H2_MAX_HEADER_LIST_BYTES must fit in 32 bits")?,
    })
}

/// 托管的两个前端入口：管理主机回运营后台那一份、其余主机回客户那一份。
///
/// 自己按路径读文件而不是用 `ServeDir`：需要把"路径不能越出产物目录"这条防线的判定放在看得见的
/// 地方（`ServeDir` 也做了这件事，但它把类型链和兜底语义一起带进来，这里两件都不需要）。
#[derive(Clone)]
struct StaticSpa {
    console: PathBuf,
    portal: PathBuf,
    /// 本地开发/验收用的**临时出口**：这个主机（`Host` 头，不含端口）也回管理端那一份。
    ///
    /// 生产不设它——分发判据就是主机名 `admin.<domain>`。但本机起服务时往往没有域名可指（写 hosts
    /// 要管理员权限，容器里也没有），没有这个出口就**根本打不开管理端**。它只是一次取值，运行期不改。
    ///
    /// 设了之后，`/portal.html` 仍可按**文件名**直达客户入口（见 [`Self::serve`]），所以两个入口在本机
    /// 都有一条明确地址。
    console_dev_host: Option<String>,
}

impl StaticSpa {
    async fn serve(&self, request: axum::extract::Request) -> axum::response::Response {
        let path = request.uri().path();
        let host = request
            .headers()
            .get(header::HOST)
            .and_then(|value| value.to_str().ok())
            .map(|host| host.split(':').next().unwrap_or(host).to_owned());
        // 判据：**精确的入口文件名**优先，其次是主机名第一段是 `admin`（生产），最后是本地开发出口
        // （默认无）。
        //
        // 入口文件名优先是为了让两份产物在**任何**主机上都各有一条明确地址：`/console.html` 与
        // `/portal.html` 都是静态文件，只认主机名的话开发出口一开，`/portal.html` 也会被顶成管理端
        // （实际踩到过），于是本机反而拿不到客户入口。它不影响深链兜底——那条走的是不存在的路径。
        let is_console = match path {
            "/console.html" => true,
            "/portal.html" => false,
            _ => host.as_deref().is_some_and(|host| {
                host.starts_with("admin")
                    || self
                        .console_dev_host
                        .as_deref()
                        .is_some_and(|dev| dev.eq_ignore_ascii_case(host))
            }),
        };
        let root = if is_console {
            &self.console
        } else {
            &self.portal
        };
        let entry = if is_console {
            "console.html"
        } else {
            "portal.html"
        };
        serve_from(root, entry, path).await
    }
}

/// 从产物目录里读一个文件回出去；找不到且路径不含 `.` 时回入口 HTML（深链）。
///
/// **只回产物目录下的文件**：请求路径里的 `..` 一律拒，不拼接、不规范化后再判——判在前比判在后可靠。
///
/// **`/api` 与 `/v1` 下面的路径不归它管**：那两个命名空间属于 API，未注册的路径必须回既有的 JSON
/// 404。兜底成一份 HTML 会把"路径写错了"变成"调用成功"，对任何按状态码判成败的调用方都是坏消息。
async fn serve_from(root: &std::path::Path, entry: &str, path: &str) -> axum::response::Response {
    if path == "/api" || path.starts_with("/api/") || path == "/v1" || path.starts_with("/v1/") {
        return not_found();
    }
    let relative = path.trim_start_matches('/');
    // 只看**解码后**的路径里有没有 `..` 或空段：`%2e%2e%2f` 这类写法在拼接前就得挡住，
    // 而不是拼完再规范化——判在前比判在后可靠。
    if relative
        .split('/')
        .any(|segment| segment == ".." || segment == ".")
    {
        return not_found();
    }
    let candidate = if relative.is_empty() {
        root.join(entry)
    } else {
        root.join(relative)
    };
    match tokio::fs::read(&candidate).await {
        Ok(bytes) => file_response(&candidate, bytes),
        Err(_) if !relative.contains('.') => match tokio::fs::read(root.join(entry)).await {
            Ok(bytes) => file_response(&root.join(entry), bytes),
            Err(_) => not_found(),
        },
        Err(_) => not_found(),
    }
}

fn file_response(path: &std::path::Path, bytes: Vec<u8>) -> axum::response::Response {
    let content_type = match path.extension().and_then(|value| value.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") | Some("map") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    };
    ([(header::CONTENT_TYPE, content_type)], bytes).into_response()
}

fn not_found() -> axum::response::Response {
    ApiError {
        status: StatusCode::NOT_FOUND,
        code: "not_found",
        message: "no such endpoint".to_owned(),
        retry_after: None,
    }
    .into_response()
}

/// 找 `apps/web/dist` 并装配两个入口；没有构建产物时回 `None`（API 只服务 API，未命中仍是 JSON 404）。
fn static_spa() -> Result<Option<StaticSpa>> {
    let dist = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("web")
        .join("dist");
    if !dist.is_dir() {
        tracing::warn!(path = %dist.display(), "no web build found; the API serves no front end");
        return Ok(None);
    }
    Ok(Some(StaticSpa {
        console: dist.clone(),
        portal: dist,
        // 缺省不设：分发只看主机名。本机没有域名可指时才显式打开这个出口。
        console_dev_host: env::var("CONSOLE_DEV_HOST")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty()),
    }))
}

/// 进程终止信号：SIGINT（Ctrl+C）或 SIGTERM（systemd 与容器的默认信号），任一到达都触发 axum 优雅停机。
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                let result = tokio::select! {
                    result = tokio::signal::ctrl_c() => result,
                    _ = terminate.recv() => Ok(()),
                };
                if let Err(error) = result {
                    tracing::error!(error = %error, "failed to listen for shutdown signal");
                }
            }
            Err(error) => {
                tracing::error!(%error, "failed to listen for SIGTERM; waiting for Ctrl+C only");
                if let Err(error) = tokio::signal::ctrl_c().await {
                    tracing::error!(error = %error, "failed to listen for shutdown signal");
                }
            }
        }
    }

    #[cfg(not(unix))]
    {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %error, "failed to listen for shutdown signal");
        }
    }
}

/// 健康检查：探一次**事实源**（数据库）是否可达，其余依赖一概不判。
///
/// 只算数据库：缓存是加速层，它不可用时系统按"没有缓存"继续服务，把它算不健康会让编排系统
/// 重启一个本来能服务的实例；渠道是否可用是业务状态，不是进程健康状态。所以这里只做一次
/// 只读幂等的 `SELECT 1`。
///
/// 探测带超时（`HEALTH_PROBE_TIMEOUT_MS`，默认 2000ms，是部署期配置项）：探活不能被一个卡住的
/// 连接挂住——连接池排队、网络半开都可能让 `SELECT 1` 长时间不返回，那时进程已经不能在承诺的
/// 时间内给出答案，按不可用处理比把编排系统也挂住更安全。
async fn health(State(state): State<AppState>) -> Response {
    let timeout = health_probe_timeout();
    let reachable = matches!(
        tokio::time::timeout(timeout, state.repository.probe()).await,
        Ok(Ok(()))
    );
    if reachable {
        (StatusCode::OK, Json(json!({"status": "ok"}))).into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"status": "unhealthy", "database": "unreachable"})),
        )
            .into_response()
    }
}

/// 健康探测超时（毫秒）：部署期配置，缺省 2000ms。
///
/// 读不出来、或者读到 0 这种没意义的非正值时退回缺省：探活本身不该因为一个环境变量写错就把
/// 整个进程判成不健康。
fn health_probe_timeout() -> Duration {
    let millis = env::var("HEALTH_PROBE_TIMEOUT_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|millis| *millis > 0)
        .unwrap_or(2_000);
    Duration::from_millis(millis)
}

#[derive(Debug, Deserialize)]
struct CreateAccountBody {
    initial_credit_microusd: u64,
    /// 账户名称。**省略**＝由服务端生成；显式 `null`、空串或纯空白按参数错误拒（名称不允许被“清空”）。
    /// `Option<Option<_>>` 加自定义解码就是为了把这两种情况分开——默认的 `Option<String>` 会把它们
    /// 一起折成 `None`，于是“显式 null”会被悄悄当成“省略”。
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    name: Option<Option<String>>,
    /// 路由标签。省略＝不设标签。
    #[serde(default)]
    tag: Option<String>,
}

/// 把“字段缺失”与“字段为 `null`”分开：缺失交给 `#[serde(default)]` 得到外层 `None`，
/// 出现（哪怕是 `null`）则得到 `Some(内层)`。
fn deserialize_optional_field<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Debug, Serialize)]
struct CreateAccountResponse {
    account_id: AccountId,
}

async fn create_account(
    State(state): State<AppState>,
    Json(body): Json<CreateAccountBody>,
) -> Result<Json<CreateAccountResponse>, ApiError> {
    let name = match body.name {
        None => None,
        Some(Some(name)) => Some(name),
        Some(None) => {
            return Err(ApiError::bad_request(
                "invalid_name",
                "account name cannot be null; omit it to have one generated".to_owned(),
            ));
        }
    };
    let account_id = state
        .accounts
        .create_account(
            body.initial_credit_microusd,
            name.as_deref(),
            body.tag.as_deref(),
            "admin-api",
        )
        .await?;
    Ok(Json(CreateAccountResponse { account_id }))
}

#[derive(Debug, Deserialize)]
struct RenameAccountBody {
    name: String,
}

/// 改账户名称（`PUT /api/v1/accounts/{account_id}/name`）：只动资料，不碰余额、标签与凭据。
async fn rename_account(
    State(state): State<AppState>,
    Path(account_id): Path<Uuid>,
    Json(body): Json<RenameAccountBody>,
) -> Result<StatusCode, ApiError> {
    state
        .accounts
        .rename_account(AccountId(account_id), &body.name, "admin-api")
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct CreditAccountBody {
    amount_microusd: u64,
    business_key: String,
}

async fn credit_account(
    State(state): State<AppState>,
    Path(account_id): Path<Uuid>,
    Json(body): Json<CreditAccountBody>,
) -> Result<StatusCode, ApiError> {
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

#[derive(Debug, Serialize)]
struct AccountBalanceResponse {
    /// 已结算余额（可以为负）。
    balance_microusd: i64,
    /// 占用合计：active 预授权之和。
    held_microusd: i64,
    /// 可用额 = 已结算余额 − 占用合计（同一时点读出来）。
    available_microusd: i64,
    /// 账户金额版本。
    version: i64,
    updated_at: DateTime<Utc>,
}

/// 读账户余额与写入时刻（管理员）。
///
/// 读的是 `ledger.accounts` 那一行，**不读缓存**：这条读用于运营对账与查看，缓存里的值可能
/// 滞后、也可能来自对账覆盖，拿它当答案会把"账实不符"读成"账实相符"。账户不存在返回 404。
async fn read_account_balance(
    State(state): State<AppState>,
    Path(account_id): Path<Uuid>,
) -> Result<Json<AccountBalanceResponse>, ApiError> {
    let change = state.accounts.read_balance(AccountId(account_id)).await?;
    Ok(Json(AccountBalanceResponse {
        balance_microusd: change.balance_microusd,
        held_microusd: change.held_microusd,
        available_microusd: change.available_microusd,
        version: change.version,
        updated_at: change.updated_at,
    }))
}

/// 按账户标识读账户摘要（`GET /api/v1/accounts/{account_id}/summary`）：账户详情页直达与刷新用。
///
/// 字段与列表项一致；金额仍由 [`read_account_balance`] 那条读给。账户不存在返回 404，页面据此
/// 显示找不到并清掉上一个账户的读数。
async fn read_account_summary(
    State(state): State<AppState>,
    Path(account_id): Path<Uuid>,
) -> Result<Json<AccountSummary>, ApiError> {
    Ok(Json(
        state
            .accounts
            .account_summary(AccountId(account_id))
            .await?,
    ))
}

/// 按账户触发一次账实核对（`POST /api/v1/accounts/{account_id}/ledger-audit`）。
///
/// 触发即返回 `202`：核对**在后台任务里跑**，不在这个请求里——它不参与资金写入或余额读取，
/// 也不该让管理员请求等它扫完。发现不一致时由核对自己建案并告警（只建案，不改账）。
///
/// 账户不存在回 `404`：与"账户一致"的 `202` 分开，运维敲错 id 要能立刻看出来。
async fn trigger_ledger_audit(
    State(state): State<AppState>,
    Path(account_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let account_id = AccountId(account_id);
    state.accounts.read_balance(account_id).await?;
    let auditor = state.ledger_audit.clone();
    tokio::spawn(async move {
        if let Err(error) = auditor.audit_account(account_id).await {
            tracing::error!(error = %error, "the ledger audit failed");
        }
    });
    Ok(StatusCode::ACCEPTED)
}

/// 管理员看账目流水的查询参数：`since` 是 RFC3339 的增量起点（开区间），`until` 是闭区间上界，
/// `offset` 供翻页，`limit` 是条数上限。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountEntriesQuery {
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
    offset: Option<u32>,
    limit: Option<u32>,
    /// 只读某一类（例如**充值记录**只看 `credit`）；缺省读全部。
    kind: Option<String>,
}

/// `ledger.entries.kind` 的取值面。未知取值**拒**而不是静默回空：写错一个词时"没有账目"与
/// "你查的类别不存在"是两件事。
const LEDGER_ENTRY_KINDS: [&str; 4] = ["credit", "capture", "adjustment", "cost"];

const DEFAULT_ENTRIES_LIMIT: u32 = 100;

/// 流水响应：`count` 与 `total` 一起给，是因为这条读按**时间倒序**取、且要能翻页。
///
/// 倒序 + 截断时被截掉的是**更旧**的那一段，调用方要接着往下翻；`truncated` 就是那个信号
/// ——判别据是"**这个位置之后还有没有更多**"（`offset + count < total`）。要刷新的人则应该用
/// **最新一条**的 `created_at` 作下次的 `since`，不去碰旧的尾巴。
///
/// `total` 是**同一套区间条件**下的总条数，与 `count`（本页条数）不同：翻页要靠它判断还有没有
/// 下一页，而"这一页满没满"在正好整除时会骗人。
#[derive(Debug, Serialize)]
struct AccountEntriesResponse {
    entries: Vec<LedgerEntryView>,
    count: usize,
    total: u64,
    truncated: bool,
}

/// 管理员看某个账户的**账目流水**（时间倒序，`since` 增量拉、`until` 收上界、`offset` 翻页、`limit` 截断）。
///
/// 读的是 `ledger.entries`，**不读缓存**：这条读的用途是运营查看与核对，缓存里的值可能滞后、
/// 也可能刚被对账覆盖写回，拿它当答案就把"账实不符"读成了"账实相符"。它**只读**——不改状态，
/// 也不写审计（读不是变更）。账户不存在是 404：与"这个账户还没有任何流水"（空数组）分开。
async fn list_account_entries(
    State(state): State<AppState>,
    Path(account_id): Path<Uuid>,
    Query(query): Query<AccountEntriesQuery>,
) -> Result<Json<AccountEntriesResponse>, ApiError> {
    let limit = query
        .limit
        .unwrap_or(DEFAULT_ENTRIES_LIMIT)
        .clamp(1, MAX_OPERATIONAL_LIMIT);
    let account = AccountId(account_id);
    let kind = match query.kind.as_deref() {
        None => None,
        Some(value) if LEDGER_ENTRY_KINDS.contains(&value) => Some(value),
        Some(value) => {
            return Err(ApiError::bad_request(
                "invalid_kind",
                format!(
                    "unknown ledger entry kind {value}; expected one of {}",
                    LEDGER_ENTRY_KINDS.join(", ")
                ),
            ));
        }
    };
    let entries = state
        .accounts
        .read_entries(
            account,
            query.since,
            query.until,
            kind,
            query.offset.unwrap_or(0),
            limit,
        )
        .await?
        .into_iter()
        .map(LedgerEntryView::from)
        .collect::<Vec<_>>();
    let total = state
        .accounts
        .count_entries(account, query.since, query.until, kind)
        .await?;
    let offset = query.offset.unwrap_or(0);
    Ok(Json(AccountEntriesResponse {
        count: entries.len(),
        total,
        // "后面还有更多"：本页最后一条的位置还没够到总数。它同时覆盖两种被截掉的情形——`limit`
        // 截断了这一页，以及 `offset` 落在中间。`offset + count >= total` 在恰好整除时会误判成
        // "还有更多"，而那时下一页其实是空的。
        truncated: (offset as u64) + (entries.len() as u64) < total,
        entries,
    }))
}

/// 管理员读某个账户的**调用明细**：逐笔生成请求（型号、张数、扣费、**请求任务 ID**）。
///
/// 与对客那条读的是同一份事实，但**回 Job 标识**——运营要回答"哪一笔扣费对应哪次调用"（`#40`）。
/// 预授权（`hold`/`release`）不在这条读里：它是内部机制，排障走对账与诊断页。
#[derive(Debug, Serialize)]
struct AdminUsageRow {
    job_id: Uuid,
    gateway_model: String,
    status: CustomerUsageStatus,
    kind: CustomerUsageKind,
    /// 受理时刻；处理中的请求按它归属查询区间。
    created_at: DateTime<Utc>,
    /// 终态时刻；已完成请求按它归属查询区间，未定终态时为空。
    terminal_at: Option<DateTime<Utc>>,
    /// 模型类型（`image` / `video` / `chat`）：受理时引用的 Vendor Model 的类型。
    #[serde(rename = "type")]
    model_type: String,
    /// 本次执行按类型给出的量；该类型还没有量落点时为全空。
    usage: UsageAmounts,
    charged_microusd: i64,
}

#[derive(Debug, Serialize)]
struct AdminUsageResponse {
    usage: Vec<AdminUsageRow>,
    count: usize,
    truncated: bool,
}

async fn list_account_usage(
    State(state): State<AppState>,
    Path(account_id): Path<Uuid>,
    Query(query): Query<CustomerBillingQueryParams>,
) -> Result<Json<AdminUsageResponse>, ApiError> {
    let billing = query.billing_query();
    let usage = state
        .accounts
        .customer_usage(
            AccountId(account_id),
            // 管理员这条读是**合并视图**（处理中与已结束都在），与对客页面的分流是两回事。
            CustomerUsageQuery {
                scope: CustomerUsageScope::All,
                since: billing.since,
                until: billing.until,
                after: None,
                limit: billing.limit,
            },
        )
        .await?
        .into_iter()
        .map(|row| AdminUsageRow {
            job_id: row.job_id.0,
            gateway_model: row.gateway_model,
            status: row.status,
            kind: row.kind,
            created_at: row.created_at,
            terminal_at: row.terminal_at,
            model_type: row.model_type,
            usage: row.usage,
            charged_microusd: row.charged_microusd,
        })
        .collect::<Vec<_>>();
    Ok(Json(AdminUsageResponse {
        count: usage.len(),
        truncated: usage.len() as u32 == billing.limit,
        usage,
    }))
}

/// 对客的账户面：**自己的**已结算余额、持有中与可用额。
///
/// 三个数**分开给、不合成一个数**：已结算余额是已经真的扣掉的钱，持有中是已预授权但还没结算的
/// 部分——预授权不是扣款，它只是先把钱占住；可用额是前两者相减，受理只用它判能不能再占
/// （账户资金 Spec `0002` §4）。合成一个"总资产"会让"这笔钱到底扣没扣"说不清，而这三个数的
/// 用途正是让人看清这件事。客户控制台只把这个数字显示成一个「余额」——取 `available_microusd`。
///
/// 三个数都从**数据库**同一行读出、不读缓存：缓存里的值可能滞后、也可能刚被对账覆盖写回，而这条
/// 读的用途正是查看与核对，拿被怀疑的一方作证没有意义。认证沿用对客那条路径（消费者自己的 API
/// Key），所以看到的只可能是自己的账户。
#[derive(Debug, Serialize)]
struct OwnAccountResponse {
    balance_microusd: i64,
    held_microusd: i64,
    available_microusd: i64,
    updated_at: DateTime<Utc>,
}

/// 客户会话面的账户读：比 API Key 面多一个**自己的账户名称**。
///
/// 两个面分开成两个类型是刻意的：`/v1/account` 的形状是既有合同，加字段会连带改掉它。
#[derive(Debug, Serialize)]
struct CustomerAccountResponse {
    name: String,
    balance_microusd: i64,
    held_microusd: i64,
    available_microusd: i64,
    updated_at: DateTime<Utc>,
}

async fn read_own_account(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<OwnAccountResponse>, ApiError> {
    let account_id = authenticate(&state, &headers).await?;
    let change = state.accounts.read_balance(account_id).await?;
    Ok(Json(OwnAccountResponse {
        balance_microusd: change.balance_microusd,
        held_microusd: change.held_microusd,
        available_microusd: change.available_microusd,
        updated_at: change.updated_at,
    }))
}

fn route_policy_view(policy: &RoutePolicy) -> Value {
    json!({
        "gateway_model": policy.gateway_model,
        "strategy": policy.strategy.as_str(),
        "discount_rates": policy.discount_rates,
        "tag_channel_map": policy.tag_channel_map,
        "version": policy.version,
    })
}

#[derive(Debug, Deserialize)]
struct UpsertRoutePolicyBody {
    /// 作用域：不传或给 `null` 就是全局那条。
    #[serde(default)]
    gateway_model: Option<String>,
    strategy: String,
    /// 折扣率表（候选 → 万分比）：只作 `least_cost` 的比较输入，不进成本口径。
    #[serde(default)]
    discount_rates: Option<BTreeMap<String, u32>>,
    /// 标签 → 候选的映射：供 `user_tag` 用。
    #[serde(default)]
    tag_channel_map: Option<BTreeMap<String, String>>,
}

/// 管理员看策略清单：全局那条（若有）与各网关模型的覆盖。
async fn list_route_policies(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let policies = state.route_policies.list().await?;
    let views: Vec<Value> = policies.iter().map(route_policy_view).collect();
    Ok(Json(json!({"route_policies": views})))
}

/// 写入（或覆盖）一条路由策略。
///
/// `strategy` 只接受本层**已经实现**的取值：写进一个实现不了的策略，会让"配置没生效"伪装成
/// "配置生效了"，而选路正是靠它决定走哪家——所以这里直接拒绝，不悄悄落成默认。
async fn upsert_route_policy(
    State(state): State<AppState>,
    Json(body): Json<UpsertRoutePolicyBody>,
) -> Result<Json<Value>, ApiError> {
    let strategy = RouteStrategy::parse(&body.strategy).ok_or_else(|| {
        ApplicationError::InvalidParameter(format!(
            "unknown route strategy {}; supported: priority_failover, weighted_random, least_cost, user_tag",
            body.strategy
        ))
    })?;
    let policy = state
        .route_policies
        .upsert(
            body.gateway_model.as_deref(),
            strategy,
            body.discount_rates.unwrap_or_default(),
            body.tag_channel_map.unwrap_or_default(),
            "admin-api",
        )
        .await?;
    Ok(Json(json!({"route_policy": route_policy_view(&policy)})))
}

#[derive(Debug, Deserialize)]
struct SetAccountTagBody {
    /// 不传或给 `null` 就是清掉标签。
    #[serde(default)]
    tag: Option<String>,
}

/// 设账户标签（管理员面）。
///
/// 标签只在**生效的 `user_tag` 策略**下影响选路：没有那条策略时，改它不改变任何受理结果。
async fn set_account_tag(
    State(state): State<AppState>,
    Path(account_id): Path<Uuid>,
    Json(body): Json<SetAccountTagBody>,
) -> Result<StatusCode, ApiError> {
    state
        .accounts
        .set_tag(AccountId(account_id), body.tag.as_deref(), "admin-api")
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
    /// 密钥标识：这把密钥自己的身份，客户列表的行键与按行吊销都用它。
    ///
    /// 明文只在这一次响应里出现，事后谁也拿不回来；标识则一直在列表里。控制台**不把它显示在一次性
    /// 明文里**（Spec `0001` M5、C5），但接口字段保持不变。
    key_id: Uuid,
}

async fn issue_api_key(
    State(state): State<AppState>,
    Path(account_id): Path<Uuid>,
    Json(body): Json<IssueApiKeyBody>,
) -> Result<Json<IssueApiKeyResponse>, ApiError> {
    let (key_id, api_key) = state
        .identity
        .issue_api_key(AccountId(account_id), &body.label, "admin-api")
        .await?;
    Ok(Json(IssueApiKeyResponse { api_key, key_id }))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginBody {
    email: String,
    password: String,
}

#[derive(Debug, Serialize)]
struct AdminLoginResponse {
    token: String,
    expires_at: DateTime<Utc>,
    email: String,
}

#[derive(Debug, Serialize)]
struct AdminIdentityResponse {
    admin_id: Uuid,
    email: String,
}

/// 管理员登录（`POST /api/v1/admin/sessions`，无需凭据）。
///
/// 失败只有一种答复（`email or password is incorrect`）：邮箱不存在与口令不对不区分，两条路也都
/// 走一遍口令校验，因此"这个邮箱是不是管理员"既不能从文案也不能从耗时上看出来。
async fn login_admin(
    State(state): State<AppState>,
    Json(body): Json<LoginBody>,
) -> Result<Json<AdminLoginResponse>, ApiError> {
    let login = state
        .identity
        .login_admin(&body.email, &body.password, state.session_ttl)
        .await?;
    Ok(Json(AdminLoginResponse {
        token: login.token,
        expires_at: login.expires_at,
        email: login.email,
    }))
}

/// 认身份（`GET /api/v1/admin/session`，**仅会话**）：浏览器用它确认自己还是登录态。
async fn read_admin_session(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<AdminIdentityResponse>, ApiError> {
    let (admin_id, email) = state.require_admin_self(&headers).await?;
    Ok(Json(AdminIdentityResponse { admin_id, email }))
}

/// 退出（`DELETE /api/v1/admin/sessions`，**仅会话**）：删掉这条会话，幂等。
///
/// 先按[`AppState::require_admin_self`]认一遍：共享令牌不指向任何一条会话，拿它调这条端点等于
/// "退一个不存在的登录"，只会让运维以为撤销了访问。认过之后再删当前这条凭据对应的会话。
async fn logout_admin(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    state.require_admin_self(&headers).await?;
    let token = bearer_token(&headers)?;
    state.identity.logout_admin(token).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangePasswordBody {
    current_password: String,
    new_password: String,
}

/// 改自己的口令（`PUT /api/v1/admin/password`，**仅会话**）。
///
/// 成功后**该管理员的全部会话都失效**（含发起这次修改的这一条）：新口令生效而旧凭据还能用，
/// 等于没改。所以客户端拿到 204 之后应当回到登录页。
async fn change_admin_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ChangePasswordBody>,
) -> Result<StatusCode, ApiError> {
    let (admin_id, _) = state.require_admin_self(&headers).await?;
    state
        .identity
        .change_admin_password(admin_id, &body.current_password, &body.new_password)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IssuePasswordResetBody {
    email: String,
}

#[derive(Debug, Serialize)]
struct PasswordResetResponse {
    reset_token: String,
    expires_at: DateTime<Utc>,
}

/// 签发一枚管理员口令重置令牌（`POST /api/v1/admin/password-resets`）。
///
/// 认管理与会话两种凭据：它指的是**别人**（路径/体里那个邮箱），不涉及"我是谁"；这条路径也是
/// "所有管理员都进不去"时运维用共享令牌自救的入口。明文只这一次。
async fn issue_admin_password_reset(
    State(state): State<AppState>,
    Json(body): Json<IssuePasswordResetBody>,
) -> Result<(StatusCode, Json<PasswordResetResponse>), ApiError> {
    let (_, token, expires_at) = state
        .identity
        .issue_admin_password_reset(&body.email, state.password_reset_ttl)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(PasswordResetResponse {
            reset_token: token,
            expires_at,
        }),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RedeemPasswordResetBody {
    reset_token: String,
    new_password: String,
}

/// 凭重置令牌设置新口令（`POST /api/v1/admin/password-resets/redeem`，**无需凭据**）。
///
/// 口令重置的全部意义就是"进不去了"，所以兑换**只认令牌本身**：令牌是与会话同强度的凭据、
/// 只存摘要、只活一次。兑换成功不发会话，调用方用新口令正常登录。
async fn redeem_admin_password_reset(
    State(state): State<AppState>,
    Json(body): Json<RedeemPasswordResetBody>,
) -> Result<StatusCode, ApiError> {
    state
        .identity
        .redeem_password_reset(&body.reset_token, &body.new_password)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenCustomerBody {
    email: String,
    /// 缺省时不设初始口令：运营改用重置令牌让客户自己设。
    password: Option<String>,
    /// 缺省时新建账户；给了就把登录身份配到这个**已有账户**上。
    account_id: Option<Uuid>,
    /// 新账户的名称。省略＝按登录邮箱生成；与 `account_id` 同时出现是参数错误（绑定不改名）。
    /// 与 `CreateAccountBody::name` 同理，显式 `null` 不允许。
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    account_name: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CustomersQuery {
    email: Option<String>,
    limit: Option<u32>,
}

#[derive(Debug, Serialize)]
struct CustomersResponse {
    customers: Vec<CustomerView>,
}

/// 替客户开户（`POST /api/v1/customers`）：给一个邮箱配登录身份。
///
/// 账户可以是新建的，也可以是**今天已经存在的**（管理员建的账户此前只有账户没有身份）——
/// 配身份不动账本、不动密钥、也不做账户间转账。
async fn open_customer(
    State(state): State<AppState>,
    Json(body): Json<OpenCustomerBody>,
) -> Result<(StatusCode, Json<CustomerView>), ApiError> {
    let account_name = match body.account_name {
        None => None,
        Some(Some(name)) => Some(name),
        Some(None) => {
            return Err(ApiError::bad_request(
                "invalid_name",
                "account_name cannot be null; omit it to have one generated".to_owned(),
            ));
        }
    };
    let view = state
        .identity
        .open_customer_account(
            &body.email,
            body.password.as_deref(),
            body.account_id.map(AccountId),
            account_name.as_deref(),
        )
        .await?;
    Ok((StatusCode::CREATED, Json(view)))
}

/// 找客户账户（`GET /api/v1/customers`）：给客户充值、替客户签重置令牌都要先拿到账户标识。
async fn list_customers(
    State(state): State<AppState>,
    Query(query): Query<CustomersQuery>,
) -> Result<Json<CustomersResponse>, ApiError> {
    let customers = match query.email.as_deref().map(str::trim) {
        Some(email) if !email.is_empty() => state
            .identity
            .find_customer(email)
            .await?
            .into_iter()
            .collect(),
        _ => {
            let limit = query
                .limit
                .unwrap_or(DEFAULT_CUSTOMERS_LIMIT)
                .clamp(1, MAX_OPERATIONAL_LIMIT);
            state.identity.list_customers(limit).await?
        }
    };
    Ok(Json(CustomersResponse { customers }))
}

/// 按客户标识读客户视图（`GET /api/v1/customers/{customer_id}`）：客户详情页直达与刷新用。
///
/// 字段与列表项一致，不含口令、会话、API Key 明文与重置令牌；不存在返回 404。
async fn read_customer_view(
    State(state): State<AppState>,
    Path(customer_id): Path<Uuid>,
) -> Result<Json<CustomerView>, ApiError> {
    Ok(Json(state.identity.customer_view_by_id(customer_id).await?))
}

const DEFAULT_CUSTOMERS_LIMIT: u32 = 50;

/// 列账户的查询参数：`email` 与 `tag` 都是精确匹配，两个都给时是**与**；都不给即"最近创建的若干条"。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountsQuery {
    email: Option<String>,
    tag: Option<String>,
    /// 名称子串筛选；首尾空白忽略，空串按“没给”处理（与 email、tag 同一条规则）。
    name: Option<String>,
    limit: Option<u32>,
}

const DEFAULT_ACCOUNTS_LIMIT: u32 = 50;

#[derive(Debug, Serialize)]
struct AccountsResponse {
    accounts: Vec<AccountSummary>,
}

/// 列账户（`GET /api/v1/accounts`）：运营**先找到再操作**的入口。
///
/// 没有它，运营必须已经知道账户标识才能充值或看流水——而账户标识是 UUID，运营手上没有，他们有的是
/// 客户的邮箱或自己设的标签。这条读只读、不写审计，也不读缓存：列表里的余额要能与详情里的对上。
async fn list_accounts(
    State(state): State<AppState>,
    Query(query): Query<AccountsQuery>,
) -> Result<Json<AccountsResponse>, ApiError> {
    let limit = query
        .limit
        .unwrap_or(DEFAULT_ACCOUNTS_LIMIT)
        .clamp(1, MAX_OPERATIONAL_LIMIT);
    // 空串按"没给"处理：查询串里 `?email=` 与不带这个参数应当是同一件事，否则运营清空输入框再搜
    // 会得到一个永远为空的列表，而看起来什么都没错。
    let email = query
        .email
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let tag = query
        .tag
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let name = query
        .name
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let accounts = state
        .accounts
        .list_accounts(email, tag, name, limit)
        .await?;
    Ok(Json(AccountsResponse { accounts }))
}

/// 为客户账户签发重置令牌（`POST /api/v1/accounts/{account_id}/password-reset`）。
///
/// 与管理员那条同一个理由：平台不发邮件，所以重置只能由**运营触发**再转交令牌；没有登录身份
/// 的账户是 404（这条读只给已经配了身份的账户签）。
async fn issue_customer_password_reset(
    State(state): State<AppState>,
    Path(account_id): Path<Uuid>,
) -> Result<(StatusCode, Json<PasswordResetResponse>), ApiError> {
    let (_, token, expires_at) = state
        .identity
        .issue_customer_password_reset(AccountId(account_id), state.password_reset_ttl)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(PasswordResetResponse {
            reset_token: token,
            expires_at,
        }),
    ))
}

#[derive(Debug, Serialize)]
struct FxRatesResponse {
    rates: Vec<FxRateView>,
}

#[derive(Debug, Serialize)]
struct FxRateView {
    currency: String,
    rate_micros: u64,
    effective_at: DateTime<Utc>,
}

/// 读当前生效的折算率（`GET /api/v1/fx-rates`）：折算率页要显示"当前录入结果"。
///
/// 每个币种只回**当前生效的那一行**（受理时取的就是它），按币种排序，免得页面自己去挑。
async fn list_fx_rates(State(state): State<AppState>) -> Result<Json<FxRatesResponse>, ApiError> {
    let rates = state
        .pricing
        .current_fx_rates()
        .await?
        .into_iter()
        .map(|(currency, rate_micros, effective_at)| FxRateView {
            currency,
            rate_micros,
            effective_at,
        })
        .collect();
    Ok(Json(FxRatesResponse { rates }))
}

// ---- 对客身份与自助 ----

#[derive(Debug, Serialize)]
struct CustomerLoginResponse {
    token: String,
    expires_at: DateTime<Utc>,
    email: String,
    account_id: Uuid,
}

/// 对客注册（`POST /v1/customers`，无需凭据）：新建账户与身份，一步到位。
async fn register_customer(
    State(state): State<AppState>,
    Extension(client): Extension<ClientAddr>,
    headers: HeaderMap,
    Json(body): Json<LoginBody>,
) -> Result<(StatusCode, Json<CustomerLoginResponse>), ApiError> {
    let source = state.request_source(&headers, &client);
    let login = state
        .identity
        .register_customer(&body.email, &body.password, state.session_ttl, &source)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(CustomerLoginResponse {
            token: login.token,
            expires_at: login.expires_at,
            email: login.email,
            account_id: login.account_id,
        }),
    ))
}

/// 对客登录（`POST /v1/customer/sessions`，无需凭据）。失败语义与管理员那条相同。
async fn login_customer(
    State(state): State<AppState>,
    Extension(client): Extension<ClientAddr>,
    headers: HeaderMap,
    Json(body): Json<LoginBody>,
) -> Result<Json<CustomerLoginResponse>, ApiError> {
    let source = state.request_source(&headers, &client);
    let login = state
        .identity
        .login_customer(&body.email, &body.password, state.session_ttl, &source)
        .await?;
    Ok(Json(CustomerLoginResponse {
        token: login.token,
        expires_at: login.expires_at,
        email: login.email,
        account_id: login.account_id,
    }))
}

/// 对客退出（`DELETE /v1/customer/sessions`）。
/// 对客退出（`DELETE /v1/customer/sessions`）。成功后这一行会话就没了。
///
/// **先鉴别、再删**，与管理员那条（`logout_admin` 走 `require_admin_self`）同一形状：无凭据或凭据已经
/// 不作数时回答"未认证"，而不是回一个 204。少了这一步，随便递一个没用的令牌都能"退出成功"——调用方
/// 于是以为自己的会话还好好地在那儿（它并不知道对方已经把什么都当成功了），而这条答复什么都不说。
async fn logout_customer(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let token = bearer_token(&headers)?;
    if state
        .identity
        .authenticate_customer_session(token)
        .await?
        .is_none()
    {
        return Err(unauthorized());
    }
    state.identity.logout_customer(token).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 客户改自己的口令（`PUT /v1/customer/password`）：成功后该客户全部会话失效。
async fn change_customer_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ChangePasswordBody>,
) -> Result<StatusCode, ApiError> {
    let (customer_id, _) = state.require_customer(&headers).await?;
    state
        .identity
        .change_customer_password(customer_id, &body.current_password, &body.new_password)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 凭重置令牌设置新口令（`POST /v1/customer/password-resets/redeem`，无需凭据）。
///
/// 对客面**没有**"提交邮箱就拿到令牌"的入口：平台不发邮件、不做邮箱验证，那种入口等于
/// "知道邮箱就能接管账户"。令牌只由运营在管理端签发后转交。
async fn redeem_customer_password_reset(
    State(state): State<AppState>,
    Extension(client): Extension<ClientAddr>,
    headers: HeaderMap,
    Json(body): Json<RedeemPasswordResetBody>,
) -> Result<StatusCode, ApiError> {
    let source = state.request_source(&headers, &client);
    state
        .identity
        .redeem_customer_password_reset(&body.reset_token, &body.new_password, &source)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Serialize)]
struct CustomerUsageRow {
    gateway_model: String,
    status: CustomerUsageStatus,
    kind: CustomerUsageKind,
    /// 受理时刻；处理中的请求按它归属查询区间。
    created_at: DateTime<Utc>,
    /// 终态时刻；已完成请求按它归属查询区间，未定终态时为空。
    terminal_at: Option<DateTime<Utc>>,
    /// 模型类型（`image` / `video` / `chat`）：受理时引用的 Vendor Model 的类型。
    #[serde(rename = "type")]
    model_type: String,
    /// 本次执行按类型给出的量；该类型还没有量落点时为全空。
    usage: UsageAmounts,
    charged_microusd: i64,
}

#[derive(Debug, Serialize)]
struct CustomerUsageResponse {
    usage: Vec<CustomerUsageRow>,
    count: usize,
    truncated: bool,
    /// 下一页的不透明定位；没有下一页时为 `null`。只在 `view=completed` 下可能非空。
    next_cursor: Option<String>,
}

/// 对客资金流水的响应：与管理员那条同形，外加翻页定位。
///
/// 单开一个类型而不是给管理员的 `AccountEntriesResponse` 加字段：管理员那条的响应形状是既有合同，
/// 不因为对客要翻页而改变。
#[derive(Debug, Serialize)]
struct CustomerLedgerResponse {
    entries: Vec<LedgerEntryView>,
    count: usize,
    /// 同一套区间与类别条件下的总条数（与 `count` 不同：本页条数）。
    total: u64,
    truncated: bool,
    /// 下一页的不透明定位；没有下一页时为 `null`。
    next_cursor: Option<String>,
}

const DEFAULT_CUSTOMER_LEDGER_LIMIT: u32 = 100;

/// 对客账单汇总的查询参数：`since` / `until` 是 RFC3339，区间**半开** `[since, until)`、按 UTC 解释。
///
/// 汇总不吃 `view`/`kind`/`cursor`：它按整段区间全量算，与逐笔列表的翻页无关（Spec C10）。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CustomerBillingQueryParams {
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
    limit: Option<u32>,
}

impl CustomerBillingQueryParams {
    /// 夹一次条数上限，并落成用例层的查询条件。
    fn billing_query(&self) -> CustomerBillingQuery {
        CustomerBillingQuery {
            since: self.since,
            until: self.until,
            limit: clamp_customer_limit(self.limit),
        }
    }
}

/// 对客调用记录的查询参数：窗口 + 视图 + 游标。
///
/// `view` 缺省是处理中与已结束的**合并**（不带新参数的旧调用）；`cursor` 只在 `view=completed` 下
/// 有意义——处理中的请求会变，不承担稳定历史。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CustomerUsageQueryParams {
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
    limit: Option<u32>,
    view: Option<String>,
    cursor: Option<String>,
}

/// 对客真实资金流水的查询参数：窗口 + 类别 + 游标。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CustomerLedgerQueryParams {
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
    limit: Option<u32>,
    kind: Option<String>,
    cursor: Option<String>,
}

/// 对客历史流水能筛的类别。**不含 `hold`/`release`**（预授权不是资金记录）与 `cost`（平台成本不是
/// 客户的事实）（Spec C8、V-C15）。取值就是账本的分录类别，入口只做一次收窄。
const CUSTOMER_LEDGER_KINDS: [LedgerEntryKind; 3] = [
    LedgerEntryKind::Credit,
    LedgerEntryKind::Capture,
    LedgerEntryKind::Adjustment,
];

fn clamp_customer_limit(limit: Option<u32>) -> u32 {
    limit
        .unwrap_or(DEFAULT_CUSTOMER_LEDGER_LIMIT)
        .clamp(1, MAX_OPERATIONAL_LIMIT)
}

/// 把 `view` 翻成用例层的视图；未知取值拒而不是静默当成缺省。
fn customer_usage_scope(view: Option<&str>) -> Result<CustomerUsageScope, ApiError> {
    match view {
        None => Ok(CustomerUsageScope::All),
        Some("active") => Ok(CustomerUsageScope::Active),
        Some("completed") => Ok(CustomerUsageScope::Completed),
        Some(value) => Err(ApiError::bad_request(
            "invalid_view",
            format!("unknown usage view {value}; expected active or completed"),
        )),
    }
}

/// 把游标解成位置，并核对它**确实属于这次查询**：筛选面逐项相同才作数。
///
/// 解码失败、密钥不对、或与当前账户/区间/类别不符，一律按参数错误回——不静默从首页重查（那会让客户
/// 看到重复的第一页而不知道发生了什么）。
fn decode_cursor(
    state: &AppState,
    token: &str,
    filter: &HistoryFilter,
) -> Result<CursorPosition, ApiError> {
    let cursor = decode_history_cursor(&state.history_cursor_key, token)?;
    if !cursor.matches(filter) {
        return Err(invalid_history_cursor().into());
    }
    Ok(cursor.position)
}

/// 给一页的最后一行编出下一页的游标。没有下一页时是 `None`。
fn next_cursor(
    state: &AppState,
    filter: &HistoryFilter,
    position: Option<CursorPosition>,
) -> Result<Option<String>, ApiError> {
    let Some(position) = position else {
        return Ok(None);
    };
    Ok(Some(encode_history_cursor(
        &state.history_cursor_key,
        &filter.cursor_at(position),
    )?))
}

/// 对客读自己的已结算余额、持有中与可用额（`GET /v1/customer/account`）。
///
/// 认的是**客户会话**（与 `/v1/account` 的 API Key 不是一回事）：客户控制台要能登录之后直接看账。
/// 三个数分开给、不合成"总资产"——预授权不是扣款；客户控制台只把这个数字显示成一个「余额」——取
/// `available_microusd`（客户现在能用的钱），不分别展示这三项。
async fn read_customer_account(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<CustomerAccountResponse>, ApiError> {
    let (_, account_id) = state.require_customer(&headers).await?;
    let account_id = AccountId(account_id);
    let change = state.accounts.read_balance(account_id).await?;
    let summary = state.accounts.account_summary(account_id).await?;
    Ok(Json(CustomerAccountResponse {
        name: summary.name,
        balance_microusd: change.balance_microusd,
        held_microusd: change.held_microusd,
        available_microusd: change.available_microusd,
        updated_at: change.updated_at,
    }))
}

/// 客户改自己账户的名称（`PUT /v1/customer/account/name`）。
///
/// 账户由**会话**确定，请求体里没有账户标识：这条读不出“改别人的账户”这种形态。审计的操作者固定为
/// 对客自助那一个取值（与管理员改名区分）。
async fn rename_customer_account(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RenameAccountBody>,
) -> Result<StatusCode, ApiError> {
    let (_, account_id) = state.require_customer(&headers).await?;
    state
        .accounts
        .rename_account(AccountId(account_id), &body.name, "customer-self-service")
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 对客读自己的账目流水（`GET /v1/customer/ledger`）：充值与扣费都在这里，金额带符号。
///
/// 区间**半开** `[since, until)`（与账单汇总同一条口径）；可按类别筛选；按 `(created_at, id)` 倒序，
/// `cursor` 是上一页最后一行给的不透明定位。不带新参数时参数面与响应字段与旧调用一致。
async fn read_customer_ledger(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<CustomerLedgerQueryParams>,
) -> Result<Json<CustomerLedgerResponse>, ApiError> {
    let (_, account_id) = state.require_customer(&headers).await?;
    let account = AccountId(account_id);
    let kind = match query.kind.as_deref() {
        None => None,
        Some(value) => match LedgerEntryKind::parse(value) {
            Some(kind) if CUSTOMER_LEDGER_KINDS.contains(&kind) => Some(kind),
            _ => {
                return Err(ApiError::bad_request(
                    "invalid_kind",
                    format!(
                        "unknown ledger entry kind {value}; expected one of {}",
                        CUSTOMER_LEDGER_KINDS
                            .iter()
                            .map(|kind| kind.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ));
            }
        },
    };
    let limit = clamp_customer_limit(query.limit);
    let filter = HistoryFilter {
        stream: HistoryStream::Ledger,
        account_id: account,
        since: query.since,
        until: query.until,
        kind: kind.map(|kind| kind.as_str().to_owned()),
    };
    let after = match query.cursor.as_deref() {
        Some(token) => Some(decode_cursor(&state, token, &filter)?),
        None => None,
    };
    // 多取一行判断"还有没有下一页"：比拿总数减本页条数更准，也不怕条数正好整除。
    let page = state
        .accounts
        .customer_ledger(
            account,
            CustomerLedgerQuery {
                since: filter.since,
                until: filter.until,
                kind,
                after,
                limit: limit.saturating_add(1),
            },
        )
        .await?;
    let has_more = page.entries.len() > limit as usize;
    let mut entries = page.entries;
    if has_more {
        entries.truncate(limit as usize);
    }
    let cursor = if has_more {
        next_cursor(
            &state,
            &filter,
            entries.last().map(|entry| CursorPosition {
                at: entry.created_at,
                id: entry.id,
            }),
        )?
    } else {
        None
    };
    let views = entries
        .into_iter()
        .map(LedgerEntryView::from)
        .collect::<Vec<_>>();
    Ok(Json(CustomerLedgerResponse {
        count: views.len(),
        total: page.total,
        truncated: cursor.is_some(),
        next_cursor: cursor,
        entries: views,
    }))
}

/// 对客读自己的用量（`GET /v1/customer/usage`）：每一次生成请求一行，**不含任务标识与内部状态**。
///
/// `view=active` 是可刷新的处理中列表（不承担稳定历史，带游标即参数错误）；`view=completed` 是已结束
/// 历史，按 `(terminal_at, id)` 倒序翻页；不带 `view` 时是两者的合并（旧调用）。
async fn read_customer_usage(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<CustomerUsageQueryParams>,
) -> Result<Json<CustomerUsageResponse>, ApiError> {
    let (_, account_id) = state.require_customer(&headers).await?;
    let account = AccountId(account_id);
    let scope = customer_usage_scope(query.view.as_deref())?;
    let limit = clamp_customer_limit(query.limit);
    let filter = HistoryFilter {
        stream: HistoryStream::UsageCompleted,
        account_id: account,
        since: query.since,
        until: query.until,
        kind: None,
    };
    let after = match (scope, query.cursor.as_deref()) {
        (CustomerUsageScope::Completed, Some(token)) => {
            Some(decode_cursor(&state, token, &filter)?)
        }
        (CustomerUsageScope::Completed, None) => None,
        // 处理中的请求会变、合并视图混着两类：都不承担稳定历史，给了游标就是参数错误。
        (_, Some(_)) => {
            return Err(ApiError::bad_request(
                "invalid_cursor",
                "cursor is only valid with view=completed".to_owned(),
            ));
        }
        (_, None) => None,
    };
    let rows = state
        .accounts
        .customer_usage(
            account,
            CustomerUsageQuery {
                scope,
                since: filter.since,
                until: filter.until,
                after,
                limit: limit.saturating_add(1),
            },
        )
        .await?;
    let has_more = rows.len() > limit as usize;
    let mut rows = rows;
    if has_more {
        rows.truncate(limit as usize);
    }
    let cursor = if has_more && scope == CustomerUsageScope::Completed {
        next_cursor(
            &state,
            &filter,
            rows.last().and_then(|row| {
                row.terminal_at.map(|at| CursorPosition {
                    at,
                    id: row.job_id.0,
                })
            }),
        )?
    } else {
        None
    };
    let usage = rows
        .into_iter()
        .map(|row| CustomerUsageRow {
            gateway_model: row.gateway_model,
            status: row.status,
            kind: row.kind,
            created_at: row.created_at,
            terminal_at: row.terminal_at,
            model_type: row.model_type,
            usage: row.usage,
            charged_microusd: row.charged_microusd,
        })
        .collect::<Vec<_>>();
    Ok(Json(CustomerUsageResponse {
        count: usage.len(),
        truncated: has_more,
        next_cursor: cursor,
        usage,
    }))
}

/// 对客读自己的账单汇总（`GET /v1/customer/billing`）：按区间**全量**算，与明细的条数上限无关。
async fn read_customer_billing(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<CustomerBillingQueryParams>,
) -> Result<Json<Value>, ApiError> {
    let (_, account_id) = state.require_customer(&headers).await?;
    let billing = query.billing_query();
    let summary = state
        .accounts
        .customer_billing(AccountId(account_id), billing)
        .await?;
    Ok(Json(json!({
        "since": billing.since,
        "until": billing.until,
        "requests": summary.requests,
        "usage": summary.usage,
        "charged_microusd": summary.charged_microusd,
    })))
}

#[derive(Debug, Serialize)]
struct CustomerApiKeyView {
    key_id: Uuid,
    label: String,
    created_at: DateTime<Utc>,
    revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
struct CustomerApiKeysResponse {
    keys: Vec<CustomerApiKeyView>,
}

/// 列自己的密钥（`GET /v1/customer/api-keys`）：**绝不回显密钥本身**。
///
/// 明文只在创建那一次响应里出现；事后谁也拿不回来，所以列表只有标签、创建时间与吊销状态。
async fn list_customer_api_keys(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<CustomerApiKeysResponse>, ApiError> {
    let (_, account_id) = state.require_customer(&headers).await?;
    let keys = state
        .identity
        .list_api_keys(AccountId(account_id))
        .await?
        .into_iter()
        .map(|key| CustomerApiKeyView {
            key_id: key.key_id,
            label: key.label,
            created_at: key.created_at,
            revoked_at: key.revoked_at,
        })
        .collect();
    Ok(Json(CustomerApiKeysResponse { keys }))
}

/// 给自己发一把密钥（`POST /v1/customer/api-keys`）：明文只这一次。
async fn issue_customer_api_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<IssueApiKeyBody>,
) -> Result<(StatusCode, Json<IssueApiKeyResponse>), ApiError> {
    let (_, account_id) = state.require_customer(&headers).await?;
    let (key_id, api_key) = state
        .identity
        .issue_api_key(AccountId(account_id), &body.label, "customer-self-service")
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(IssueApiKeyResponse { api_key, key_id }),
    ))
}

/// 吊销自己的一把密钥（`DELETE /v1/customer/api-keys/{key_id}`）。
///
/// **不属于自己的那把一律 404**：先按账户收窄再改，而不是先查存在再判归属——后者会让
/// "别人的密钥存在吗"从 403/404 的差异里读出来。
async fn revoke_customer_api_key(
    State(state): State<AppState>,
    Path(key_id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let (_, account_id) = state.require_customer(&headers).await?;
    let revoked = state
        .identity
        .revoke_api_key_of_account(AccountId(account_id), key_id, "customer-self-service")
        .await?;
    if !revoked {
        return Err(ApiError::from(ApplicationError::NotFound(
            "api key".to_owned(),
        )));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn publish_runtime(
    State(state): State<AppState>,
    Json(mut command): Json<PublishRuntimeCommand>,
) -> Result<Json<seeai_domain::PublishedRevision>, ApiError> {
    command.actor = "admin-api".to_owned();
    resolve_inline_documentation(&mut command)?;
    Ok(Json(state.runtime.publish(command).await?))
}

/// 内联发布的文档素材可以用 `narrative_path` 引用 `public-docs/` 下的正文：在这里读成正文，
/// 交给 Application 渲染。引用式发布不带文档，读同一厂商模型已导入的素材。
fn resolve_inline_documentation(command: &mut PublishRuntimeCommand) -> Result<(), ApiError> {
    let Some(documentation) = command.documentation.as_mut() else {
        return Ok(());
    };
    if documentation.get("narrative").is_some() {
        return Ok(());
    }
    let Some(path) = documentation
        .get("narrative_path")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return Ok(());
    };
    if path
        .split('/')
        .any(|segment| segment.is_empty() || segment == "..")
    {
        return Err(ApiError::bad_request(
            "invalid_documentation",
            format!("documentation.narrative_path {path} must stay under public-docs"),
        ));
    }
    let body = std::fs::read_to_string(public_docs_dir().join(&path)).map_err(|error| {
        ApiError::bad_request(
            "invalid_documentation",
            format!("cannot read public-docs/{path}: {error}"),
        )
    })?;
    documentation["narrative"] = Value::String(body);
    Ok(())
}

async fn list_reconciliation_cases(
    State(state): State<AppState>,
) -> Result<Json<Vec<seeai_application::ReconciliationCaseView>>, ApiError> {
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
    Query(query): Query<ProviderFailuresQuery>,
) -> Result<Json<ProviderFailuresResponse>, ApiError> {
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
    Json(body): Json<RefundReconciliationBody>,
) -> Result<StatusCode, ApiError> {
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
    Json(body): Json<UpsertFxRateBody>,
) -> Result<StatusCode, ApiError> {
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
    Query(query): Query<ProviderCostGapsQuery>,
) -> Result<Json<ProviderCostGapsResponse>, ApiError> {
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
    /// 标准列表信封：`GET /v1/models` 是 OpenAI-compatible 的模型列表（Spec 0009 §2）。
    object: &'static str,
    data: Vec<ModelCatalogEntry>,
}

/// 目录里的一条：型号的公开身份 + 它那份发布的合同。
///
/// 字段名是对客协议的取值，与内部的 [`PublishedModel`] 分开：内部字段改名不该动对客协议。
#[derive(Debug, Serialize)]
struct ModelCatalogEntry {
    /// 标准模型标识：与 `name` 同值；OpenAI 兼容客户端读它，把它填进请求的 `model`（Spec 0009 §2）。
    id: String,
    /// 标准对象类型：固定 `model`（Spec 0009 §2）。
    object: &'static str,
    /// 标准创建时间：Unix 秒，该条当前发布的生效时间（Spec 0009 §2）。
    created: i64,
    /// 标准归属：与 `vendor_id` 同值（Spec 0009 §2）。
    owned_by: String,
    /// 客户端提交 `model` 时用的名字——**平台对客名**（网关模型名）。
    name: String,
    /// 厂商标识：目录属性，同一个厂商模型可以由多条渠道供给。
    ///
    /// 叫 `vendor_id` 而不是 `vendor`：它是厂商的**标识**，与 `catalog.vendor_models.vendor_id`
    /// 同义；对客协议里换名字比内部换名字代价大，因此这里一次说清。
    vendor_id: String,
    /// 合同修订。
    revision: String,
    /// 模型类型（`image` / `video` / `chat`）：客户端据此判断用量单位与表单参数面。
    #[serde(rename = "type")]
    model_type: String,
    /// 该模型的调用方合同（发布的 JSON Schema），客户端据此建表单。
    contract: Value,
    /// 该模型使用文档的公开地址：同源根相对地址，客户端直接使用，不拼路径（Spec 0008 §2）。
    documentation_url: String,
}

impl From<PublishedModel> for ModelCatalogEntry {
    fn from(model: PublishedModel) -> Self {
        let PublishedModel {
            gateway_model,
            vendor_id,
            native_revision,
            model_type,
            capability_schema,
            documentation_version,
            published_at,
        } = model;
        let documentation_url = format!(
            "/v1/models/{}/llms.txt?version={documentation_version}",
            encode_path_segment(&gateway_model)
        );
        Self {
            id: gateway_model.clone(),
            object: "model",
            created: published_at.timestamp(),
            owned_by: vendor_id.clone(),
            name: gateway_model.clone(),
            vendor_id,
            revision: native_revision,
            model_type,
            contract: consumer_contract(capability_schema, &gateway_model),
            documentation_url,
        }
    }
}

/// 把模型名编成 URL 路径段：`/`、空格、中文与 URI 保留字符都编码，客户端按原值提交 `model`。
fn encode_path_segment(segment: &str) -> String {
    let mut encoded = String::with_capacity(segment.len());
    for byte in segment.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(*byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
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
    Ok(Json(ModelCatalogResponse {
        object: "list",
        data,
    }))
}

/// 模型使用文档的查询参数：`version` 是目录返回的不透明文档版本标识。
#[derive(Debug, Deserialize)]
struct ModelDocumentParams {
    version: Option<String>,
}

/// 公开读取模型使用文档：没有 `version` 时读当前可调模型的当前说明；给了则按历史版本读。
///
/// 两种请求都不需要 API Key；成功返回 `text/plain; charset=utf-8` 的简体中文 Markdown。
/// 不可调用、未知或错配的版本一律 `404 not_found`，不重定向、不回退到当前版本（Spec 0008 §2）。
async fn read_model_document(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(params): Query<ModelDocumentParams>,
) -> Result<axum::response::Response, ApiError> {
    let body = match params.version.as_deref() {
        Some(raw) => {
            let version = Uuid::parse_str(raw)
                .map_err(|_| ApiError::not_found("no such model document version"))?;
            state
                .runtime
                .model_document_by_version(&name, version)
                .await?
        }
        None => state.runtime.current_model_document(&name).await?,
    };
    let body = body.ok_or_else(|| ApiError::not_found("no such model document"))?;
    Ok(markdown_response(body))
}

/// 公开读取一份公共使用文档；名称只允许公开文档清单里的那几份，不提供任意文件读取。
///
/// 清单只有一份属主（
/// [`seeai_application::model_document::PUBLIC_DOCUMENTS`]）：加第四份文档改那里，改这里会漂移。
async fn read_public_document(
    State(state): State<AppState>,
    Path(path): Path<String>,
) -> Result<axum::response::Response, ApiError> {
    if !seeai_application::model_document::PUBLIC_DOCUMENTS.contains(&path.as_str()) {
        return Err(ApiError::not_found("no such public document"));
    }
    let body = std::fs::read_to_string(public_docs_dir().join(&path))
        .map_err(|error| ApiError::internal(format!("cannot read public-docs/{path}: {error}")))?;
    // 源码里写 `{{SEE_BASEURL}}/v1/docs/<名>`，服务时代入平台对客基址：与模型说明同一形态（Spec 0008 §3）。
    let body =
        seeai_application::model_document::render_public_document(&body, &state.see_base_url)?;
    Ok(markdown_response(body))
}

/// Markdown 正文的响应：对客文档统一 `text/plain; charset=utf-8`（Spec 0008 §2）。
fn markdown_response(body: String) -> axum::response::Response {
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], body).into_response()
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
) -> Result<Json<GatewayModelsResponse>, ApiError> {
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
    Json(body): Json<SetGatewayModelEnabledBody>,
) -> Result<StatusCode, ApiError> {
    state
        .runtime
        .set_gateway_model_enabled(&gateway_model, body.enabled, "admin-api")
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 管理员读：可选 Offering 清单的响应。
///
/// 与 [`GatewayModelsResponse`] 同一个理由用 `offerings` 包一层：这条视图只增字段，有外层对象才
/// 不会每加一样就改一次响应的顶层形状。
#[derive(Debug, Serialize)]
struct SelectableOfferingsResponse {
    offerings: Vec<SelectableOfferingView>,
}

/// 管理员读：工程师配好的供给清单，发布页"选 vendor → 勾 Offering"的数据来源。
///
/// **需要管理员凭证**：它是运营视图。**不回显渠道地址与凭证变量名**——那是渠道部署事实，选择用不到
/// （`docs/design/0012-platform-model-publishing.md` §2.1）。停用的供给照样列出来并带
/// `enabled: false`，运营要能看出"为什么它选不了"。全部数据来自数据库，不读缓存。
async fn list_selectable_offerings(
    State(state): State<AppState>,
) -> Result<Json<SelectableOfferingsResponse>, ApiError> {
    Ok(Json(SelectableOfferingsResponse {
        offerings: state.runtime.selectable_offerings().await?,
    }))
}

/// 供给级启停的请求体：**唯一可变位**就是 `enabled`。
///
/// 不用 `deny_unknown_fields` + 结构体反序列化：那条路在 axum 里是 422，而"想顺手改承载面 /
/// 改计价"是调用方把请求写错了，按 400 回更说得通（也让调用方分得清"请求不成立"与"内容不合法"）。
/// 多一个字段就拒，不静默忽略——忽略会让调用方以为改成功了，而定义只能由发布产生。
fn take_enabled_flag(body: Value) -> Result<bool, ApiError> {
    let Value::Object(mut fields) = body else {
        return Err(ApiError::bad_request(
            "invalid_body",
            "the request body must be a JSON object",
        ));
    };
    let enabled = fields.remove("enabled");
    if let Some(extra) = fields.keys().next() {
        return Err(ApiError::bad_request(
            "invalid_body",
            format!("only `enabled` can be changed; unexpected field `{extra}`"),
        ));
    }
    match enabled {
        Some(Value::Bool(value)) => Ok(value),
        _ => Err(ApiError::bad_request(
            "invalid_body",
            "`enabled` is required and must be a boolean",
        )),
    }
}

/// 管理员写：启停一条**供给**（Offering）。
///
/// 停用的语义：该供给从所有候选集里消失，之后的受理取不到它（取不到任何候选时对客是"模型不
/// 存在"）；**已受理的 Job 不受影响**——它们的候选与定价早已随快照冻结在 Job 上。重发该模型的
/// 其它变动也不会把它顶回启用：发布按身份复用供给行，不写 `enabled`。
/// 供给 id 不存在是 404——这里只改已经发布出来的行，不创建任何东西。
async fn set_offering_enabled(
    State(state): State<AppState>,
    Path(offering_id): Path<Uuid>,
    Json(body): Json<Value>,
) -> Result<StatusCode, ApiError> {
    state
        .runtime
        .set_offering_enabled(
            OfferingId(offering_id),
            take_enabled_flag(body)?,
            "admin-api",
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 管理员写：启停一条**渠道**（Channel）。判据与 [`set_offering_enabled`] 同一条：渠道经它名下的
/// 供给影响候选集，已受理的 Job 同样不受影响。渠道 id 不存在是 404。
async fn set_channel_enabled(
    State(state): State<AppState>,
    Path(channel_id): Path<Uuid>,
    Json(body): Json<Value>,
) -> Result<StatusCode, ApiError> {
    state
        .runtime
        .set_channel_enabled(ChannelId(channel_id), take_enabled_flag(body)?, "admin-api")
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 有界 JSON 请求体：与 `axum::Json` 同一套入口语义（正文上限、`application/json` 检查），但
/// 反序列化走 [`RequestParameters::parse_with_limits`]——**边解析边计数**，容器层数、节点总数、
/// 对象字段数、累计字符串字节任一超限都在构造过程中失败（RFC 0018 §2.2）。
///
/// 不用 `Json<T>` 的原因有两个：`T` 上的 `#[serde(flatten)] Map<String, Value>` 会先把整份正文
/// 收进 serde 的中间 buffer（无界），计数就只能在建好之后做；而结构超限要走既有的参数校验失败
/// 语义（400 `invalid_parameter`），不是 `JsonRejection` 的文案。
struct BoundedRequestParameters(RequestParameters);

/// 有界请求体的拒绝：正文读失败、Content-Type 不对、结构超限三类各自的对客表现。
enum GenerationBodyRejection {
    /// 正文读取失败（含 16 MiB 正文上限、慢读超时、连接中断）。
    Read(axum::extract::rejection::BytesRejection),
    /// `Content-Type` 不是 `application/json`（与 `axum::Json` 同一判据）。
    ContentType,
    /// JSON 语法/形状错误，或结构上限被突破。
    Structure(RequestJsonError),
}

impl IntoResponse for GenerationBodyRejection {
    fn into_response(self) -> Response {
        match self {
            Self::Read(rejection) => rejection.into_response(),
            Self::ContentType => (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "Expected request with `Content-Type: application/json`",
            )
                .into_response(),
            // 超限与语法错误同属"这次请求的参数不成立"：受理前的 400，不建记录、不取 Hold。
            Self::Structure(error) => {
                ApiError::bad_request("invalid_parameter", error.to_string()).into_response()
            }
        }
    }
}

impl axum::extract::FromRequest<AppState> for BoundedRequestParameters {
    type Rejection = GenerationBodyRejection;

    async fn from_request(
        request: axum::extract::Request,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        // `Content-Type` 先判，与 `axum::Json` 一致：不读一个注定要拒的正文。
        if !json_content_type(request.headers()) {
            return Err(GenerationBodyRejection::ContentType);
        }
        let limits = state.direct.request_json_limits;
        let bytes = axum::body::Bytes::from_request(request, state)
            .await
            .map_err(GenerationBodyRejection::Read)?;
        RequestParameters::parse_with_limits(&bytes, limits)
            .map(Self)
            .map_err(GenerationBodyRejection::Structure)
    }
}

/// `Content-Type` 是不是 JSON：`application/json` 与 `application/*+json`，参数忽略。
fn json_content_type(headers: &HeaderMap) -> bool {
    let Some(value) = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let essence = value
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    essence == "application/json"
        || (essence.starts_with("application/") && essence.ends_with("+json"))
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
fn take_contract_image_inputs(parameters: &mut RequestParameters) -> Result<ImageInputs, ApiError> {
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

/// generations 入口（JSON）：与 edits 入口**是同一个能力**，只是请求编码不同。
///
/// 分支只看请求里有没有参考图 / 遮罩，不由端点断言——带图的 generations、不带图的 edits
/// 都是合法请求。
async fn generate_image(
    State(state): State<AppState>,
    account: Option<Extension<AuthenticatedAccount>>,
    slow: Option<Extension<SlowRead>>,
    scope: Option<Extension<Arc<ConnectionScope>>>,
    headers: HeaderMap,
    body: Result<BoundedRequestParameters, GenerationBodyRejection>,
) -> Result<Response, ApiError> {
    let direct = state.direct.clone();
    let BoundedRequestParameters(parameters) = match body {
        Ok(body) => body,
        Err(rejection) => {
            // 入口的读错误可能是有界慢读超时：那是受理前的 408，不建记录。
            if slow.as_ref().is_some_and(|slow| slow.0.timed_out()) {
                return Err(slow_read_timeout());
            }
            return Ok(rejection.into_response());
        }
    };
    let account = account.ok_or_else(generation_account_missing)?;
    run_direct_json(
        direct,
        account.0.account_id,
        account.0.received_at,
        scope.map(|scope| scope.0),
        &headers,
        parameters,
    )
    .await
}

/// edits 入口（`multipart/form-data`）：`image` 与 `mask` 的文本部件按公网 URL 读，文件部件在受理前被拒。
///
/// 文件部件的字节仍被解析出来（`InputImage::Bytes`）供同键重放比对，**不落盘**；命中幂等记录时
/// 按记录冻结的规则比对，未命中才按当前合同解释——那时文件部件一律 `400 public_image_url_required`。
/// 没有 `image` 的 edits 同样合法（那就是文生图）。
async fn edit_image(
    State(state): State<AppState>,
    account: Option<Extension<AuthenticatedAccount>>,
    slow: Option<Extension<SlowRead>>,
    scope: Option<Extension<Arc<ConnectionScope>>>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Response, ApiError> {
    let direct = state.direct.clone();
    let account = account.ok_or_else(generation_account_missing)?;
    let parsed = parse_multipart_direct(&mut multipart).await;
    // 慢读超时不建记录：它发生在受理前，按 408 回应，而不是当成 multipart 格式错误。
    if slow.as_ref().is_some_and(|slow| slow.0.timed_out()) {
        return Err(slow_read_timeout());
    }
    let (parameters, file_references, file_mask) = parsed?;
    let endpoint = "/v1/images/edits";
    let idempotency_key = idempotency_key(&headers);
    // 有界读取已完成，查找只用到账户与幂等键：这时还没摘图片字段、没判型号，也没占执行许可。
    if let Some(lookup) = lookup_recorded(&direct, account.0.account_id, &idempotency_key).await? {
        return Err(replay_recorded(
            &direct,
            account.0.account_id,
            endpoint,
            lookup,
            RecordedRequestInput {
                idempotency_key: &idempotency_key,
                parameters,
                file_references: &file_references,
                file_mask: file_mask.as_ref(),
            },
        )?);
    }
    let (parameters, reference_images, mask) =
        interpret_current_inputs(parameters, file_references, file_mask)?;
    run_direct_generation(
        direct,
        account.0.account_id,
        account.0.received_at,
        scope.map(|scope| scope.0),
        idempotency_key,
        endpoint,
        parameters,
        reference_images,
        mask,
    )
    .await
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

/// 直接执行入口的认证明细放进 request extension；handler 从它取账户，不再自己认证。
async fn require_generation_access(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Result<axum::response::Response, ApiError> {
    // 总期限 D 从收到请求头起算：认证、速率与读取准入都算在它里面（RFC 0017 §6）。
    let received_at = tokio::time::Instant::now();
    let direct = state.direct.clone();
    // 认证与速率在消费正文之前完成；读取准入也在这里取，取不到直接拒绝，不排队。
    let account_id = authenticate(&state, request.headers()).await?;
    let read = direct
        .supervisor
        .try_reserve_read()
        .ok_or_else(direct_capacity_unavailable)?;
    let (mut parts, body) = request.into_parts();
    parts
        .extensions
        .insert(AuthenticatedAccount::new(account_id, received_at, read));
    let (body, slow) = direct.supervisor.limit_slow_read(body);
    parts.extensions.insert(slow);
    let request = axum::extract::Request::from_parts(parts, body);
    Ok(next.run(request).await)
}

/// 没有连接监视时的上传取消信号。
static NEVER_CANCELLED: NeverCancelled = NeverCancelled;

/// 上传端点的成功响应：公网可读 URL、服务端判定的规范 MIME 与实际写入字节数。
#[derive(Debug, Serialize)]
struct UploadImageResponse {
    url: String,
    media_type: &'static str,
    byte_length: u64,
}

/// 上传入口的认证与准入：认证、独立命名空间的速率、本机上传读取许可都在消费正文之前完成。
///
/// 读取许可在中间件里持有到 handler 结束，因此正文读取与写入都在它的名额与预算之下；取不到就
/// 直接 429 upload_busy，不排队。
async fn require_upload_access(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Result<axum::response::Response, ApiError> {
    let identity = authenticate_upload(&state, request.headers()).await?;
    state
        .upload
        .acceleration
        .consume_upload_request_slot(identity.key_id, state.upload.config.rate_limit, Utc::now())
        .await
        .map_err(ApiError::from)?;
    let _lease = state
        .direct
        .supervisor
        .try_reserve_upload()
        .ok_or_else(upload_busy)?;
    // 声明的 Content-Length 超上限时零正文读取直接拒：慢读流没有长度信息，这一判要在包流之前做。
    if let Some(length) = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        && length > state.upload.config.max_request_bytes as u64
    {
        return Err(request_too_large());
    }
    let (mut parts, body) = request.into_parts();
    let (body, slow) = state.direct.supervisor.limit_upload_slow_read(body);
    parts.extensions.insert(slow);
    // 调用者账户只用于对象键前缀，随扩展递给 handler；鉴权已经在这里完成，handler 不再重复解析凭证。
    parts.extensions.insert(identity.account_id);
    let request = axum::extract::Request::from_parts(parts, body);
    Ok(next.run(request).await)
}

/// 上传端点的认证：与生成同一套凭证校验，但不占生成的速率名额（速率走独立命名空间）。
async fn authenticate_upload(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<ApiKeyIdentity, ApiError> {
    let token = bearer_token(headers)?;
    match state.identity.authenticate_identity(token).await {
        Ok(identity) => Ok(identity),
        Err(_) => Err(invalid_api_key()),
    }
}

/// 上传端点：单文件 multipart，写入上传存储换公网 URL。
///
/// 调用者账户由 `require_upload_access` 完成鉴权后随扩展递进来，只用于对象键前缀。
async fn upload_image(
    State(state): State<AppState>,
    Extension(account_id): Extension<AccountId>,
    slow: Option<Extension<SlowRead>>,
    scope: Option<Extension<Arc<ConnectionScope>>>,
    multipart: Result<Multipart, axum::extract::multipart::MultipartRejection>,
) -> Result<Response, ApiError> {
    let mut multipart =
        multipart.map_err(|error| ApiError::bad_request("invalid_multipart", error.to_string()))?;
    let parsed = read_upload_file(&mut multipart).await;
    // 慢读超时不写对象：它发生在受理前，按 408 回应；读错误先让位给这个更具体的判据。
    if slow.as_ref().is_some_and(|slow| slow.0.timed_out()) {
        return Err(slow_read_timeout());
    }
    let file = parsed?;
    let cancellation: &dyn UploadCancellation = match &scope {
        Some(scope) => scope.0.as_ref(),
        None => &NEVER_CANCELLED,
    };
    let uploaded = match state
        .upload
        .service
        .upload(
            account_id,
            &file.bytes,
            file.declared_content_type.as_deref(),
            cancellation,
        )
        .await
    {
        Ok(uploaded) => uploaded,
        Err(ImageUploadError::ClientDisconnected) => {
            // Spec 0007 §5：断开不对客返回错误码。连接已经断开，这个状态码发不到对端，也不携带
            // 错误信封与 URL；对象若已写入按孤儿处置，账户与账本不变。
            return Ok(client_disconnected().into_response());
        }
        Err(error) => return Err(upload_error(error)),
    };
    Ok(Json(UploadImageResponse {
        url: uploaded.url,
        media_type: uploaded.media_type,
        byte_length: uploaded.byte_length,
    })
    .into_response())
}

/// 客户端已离开：只为本地观测，不构造对客错误信封。
fn client_disconnected() -> StatusCode {
    StatusCode::from_u16(499).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
}

/// 一次上传请求解析出的文件：字节与部件声明的 `Content-Type`。
struct UploadFile {
    bytes: Vec<u8>,
    declared_content_type: Option<String>,
}

/// 读上传的 multipart：恰好一个带文件名的 `file` 部件，其余部件一律拒绝。
async fn read_upload_file(multipart: &mut Multipart) -> Result<UploadFile, ApiError> {
    let mut file: Option<UploadFile> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(upload_multipart_error)?
    {
        let name = field.name().unwrap_or_default().to_owned();
        if name != "file" {
            return Err(invalid_multipart(
                "the request may carry only one file part named file",
            ));
        }
        if field.file_name().is_none() {
            return Err(invalid_multipart("the file part must carry a file name"));
        }
        if file.is_some() {
            return Err(invalid_multipart(
                "the request may carry only one file part named file",
            ));
        }
        let declared_content_type = field.content_type().map(str::to_owned);
        let bytes = field.bytes().await.map_err(upload_multipart_error)?;
        file = Some(UploadFile {
            bytes: bytes.to_vec(),
            declared_content_type,
        });
    }
    file.ok_or_else(|| invalid_multipart("the request must carry a file part named file"))
}

/// 把 multipart 读取失败映射成对客码：正文超上限是 413 request_too_large，其余是 400 invalid_multipart。
fn upload_multipart_error(error: axum::extract::multipart::MultipartError) -> ApiError {
    if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
        return request_too_large();
    }
    invalid_multipart(&error.to_string())
}

/// 上传正文超过路由级上限：平台信封 413 request_too_large。
fn request_too_large() -> ApiError {
    ApiError {
        status: StatusCode::PAYLOAD_TOO_LARGE,
        code: "request_too_large",
        message: "the upload request body exceeds the configured limit".to_owned(),
        retry_after: None,
    }
}

fn invalid_multipart(message: &str) -> ApiError {
    ApiError::bad_request("invalid_multipart", message.to_owned())
}

/// 本机上传并发或内存预算取不到：429 upload_busy，不排队。
fn upload_busy() -> ApiError {
    ApiError {
        status: StatusCode::TOO_MANY_REQUESTS,
        code: "upload_busy",
        message: "the local upload capacity is exhausted; retry later".to_owned(),
        retry_after: Some(Duration::from_secs(1)),
    }
}

/// 上传失败到对客码的收口；客户端断开不对客返回错误码（连接已经断开，响应发不出去）。
fn upload_error(error: ImageUploadError) -> ApiError {
    let message = error.to_string();
    match error {
        ImageUploadError::UnsupportedMediaType => {
            ApiError::bad_request("unsupported_media_type", message)
        }
        ImageUploadError::MediaTypeMismatch => {
            ApiError::bad_request("media_type_mismatch", message)
        }
        ImageUploadError::ImageTooLarge => ApiError {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            code: "image_too_large",
            message,
            retry_after: None,
        },
        ImageUploadError::UploadStorageUnavailable => ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "upload_storage_unavailable",
            message,
            retry_after: None,
        },
        ImageUploadError::ObjectStoreUnavailable => ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "object_store_unavailable",
            message,
            retry_after: None,
        },
        ImageUploadError::ClientDisconnected => ApiError {
            // 理论上到不了这里：`upload_image` 在断开时直接回一个无信封的状态码（Spec 0007 §5）。
            status: client_disconnected(),
            code: "client_disconnected",
            message,
            retry_after: None,
        },
    }
}

/// JSON 入口转直接执行请求：**先查记录**，未命中才按当前合同摘图片字段、判型号。
///
/// 有界解析已经完成（`RequestParameters` 只能从有界解析、逐项计数或显式计数来）；查找只用到账户
/// 与幂等键，正文这时还没按任何合同解释，也还没占本机执行许可（RFC 0018 §9.1）。
async fn run_direct_json(
    direct: Arc<DirectGeneration>,
    account_id: AccountId,
    received_at: tokio::time::Instant,
    scope: Option<Arc<ConnectionScope>>,
    headers: &HeaderMap,
    parameters: RequestParameters,
) -> Result<Response, ApiError> {
    let endpoint = "/v1/images/generations";
    let idempotency_key = idempotency_key(headers);
    if let Some(lookup) = lookup_recorded(&direct, account_id, &idempotency_key).await? {
        return Err(replay_recorded(
            &direct,
            account_id,
            endpoint,
            lookup,
            RecordedRequestInput {
                idempotency_key: &idempotency_key,
                parameters,
                file_references: &[],
                file_mask: None,
            },
        )?);
    }
    let (parameters, reference_images, mask) =
        interpret_current_inputs(parameters, Vec::new(), None)?;
    run_direct_generation(
        direct,
        account_id,
        received_at,
        scope,
        idempotency_key,
        endpoint,
        parameters,
        reference_images,
        mask,
    )
    .await
}

/// 同键只读预查：命中返回原记录的比对材料，未命中返回 `None`。
///
/// 记录存在但材料缺失时仍是命中（[`ExecutionLookup`] 的材料字段为 `None`）——调用方按
/// `409 idempotency_conflict` 拒绝，不当作未命中。
async fn lookup_recorded(
    direct: &DirectGeneration,
    account_id: AccountId,
    idempotency_key: &str,
) -> Result<Option<ExecutionLookup>, ApiError> {
    direct
        .service
        .lookup_recorded(account_id, idempotency_key)
        .await
        .map_err(ApiError::from)
}

/// 命中记录后的比对与投影：用记录冻结的合同与指纹版本重算这次请求的指纹，返回它对客的拒绝。
///
/// 它消费原始请求面：命中之后这条路不会再向下执行，因此不必为比对复制一份图片字符串。
fn replay_recorded(
    direct: &DirectGeneration,
    account_id: AccountId,
    endpoint: &str,
    lookup: ExecutionLookup,
    input: RecordedRequestInput<'_>,
) -> Result<ApiError, ApiError> {
    direct
        .service
        .replay_recorded(account_id, endpoint, lookup, input)
        .map(map_direct_error)
        .map_err(ApiError::from)
}

/// 按**当前合同**解释有界解析后的入口输入：图片值只收公网 URL。
///
/// 只在幂等键未命中记录之后调用。命中时按记录冻结的合同比对，不用这里的新规则重新解释原请求
/// （Spec 0005 §4）。参考图或遮罩的取值不是 `http(s)` 公网 URL（含 `data:` URL、multipart
/// 文件部件与任何其他非法文本）时一律 `400 public_image_url_required`：不建记录、不取占用、不调上游。
/// multipart 文件部件的字节仍被解析出来，但只供重放比对用，不参与执行。
fn interpret_current_inputs(
    mut parameters: RequestParameters,
    file_references: Vec<InputImage>,
    file_mask: Option<InputImage>,
) -> Result<(RequestParameters, Vec<InputImage>, Option<InputImage>), ApiError> {
    if !file_references.is_empty() || file_mask.is_some() {
        return Err(public_image_url_required());
    }
    let text_inputs = take_contract_image_inputs(&mut parameters)?;
    let reference_images = text_inputs
        .reference_images
        .into_iter()
        .map(public_image_url)
        .collect::<Result<Vec<_>, _>>()?;
    let mask = text_inputs.mask.map(public_image_url).transpose()?;
    Ok((parameters, reference_images, mask))
}

/// 一个文本图片值必须是 `http(s)` 公网 URL；其余一律 `400 public_image_url_required`。
fn public_image_url(value: String) -> Result<InputImage, ApiError> {
    if seeai_adapter_sdk::is_http_url(&value) {
        return Ok(InputImage::Url(value));
    }
    Err(public_image_url_required())
}

/// 受理前的图片形态拒绝：不建记录、不取占用、不调上游。
fn public_image_url_required() -> ApiError {
    ApiError::bad_request(
        "public_image_url_required",
        "an input image must be a public http(s) url".to_owned(),
    )
}

/// multipart 图片部件：**直接保留字节**与声明的媒体类型；字节只供同键重放比对，不参与执行。
async fn form_image_bytes(field: Field<'_>) -> Result<InputImage, ApiError> {
    let media_type = field
        .content_type()
        .map(str::to_owned)
        .unwrap_or_else(|| "image/png".to_owned());
    let bytes = field
        .bytes()
        .await
        .map_err(|error| ApiError::bad_request("invalid_multipart", error.to_string()))?;
    Ok(InputImage::Bytes(DecodedImage { media_type, bytes }))
}

/// 解析 multipart 的**有界读取**：文件部件保留字节，文本部件逐项计数，契约字段名下的文本图片
/// 原样留在参数面里（是否摘图、按什么规则解释由调用方在查过幂等键之后决定）。
///
/// 文本部件走 [`RequestParameters::builder`] **逐项计数**：非 JSON 编码的入口同样受容器层数、
/// 字段数、累计字符串字节与节点数的约束（RFC 0018 §2.2），不能因为"不是 JSON"就绕开这一层。
async fn parse_multipart_direct(
    multipart: &mut Multipart,
) -> Result<(RequestParameters, Vec<InputImage>, Option<InputImage>), ApiError> {
    let mut parameters = RequestParameters::builder();
    let mut file_references: Vec<InputImage> = Vec::new();
    let mut file_mask: Option<InputImage> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| ApiError::bad_request("invalid_multipart", error.to_string()))?
    {
        let name = field.name().unwrap_or_default().to_owned();
        match contract_image_parameter_kind(&name) {
            Some(ImageParameterKind::Reference) if field.file_name().is_some() => {
                file_references.push(form_image_bytes(field).await?);
            }
            Some(ImageParameterKind::Mask) if field.file_name().is_some() => {
                file_mask = Some(form_image_bytes(field).await?);
            }
            _ => {
                let text = field.text().await.map_err(|error| {
                    ApiError::bad_request("invalid_multipart", error.to_string())
                })?;
                parameters
                    .insert(name.clone(), form_scalar(&name, &text))
                    .map_err(|violation| {
                        ApiError::bad_request("invalid_parameter", violation.to_string())
                    })?;
            }
        }
    }
    Ok((parameters.finish(), file_references, file_mask))
}

/// 直接执行入口：预留本机许可，起受监督的执行，等一次性结果，按内存载荷构造响应。
///
/// 成功时执行许可与发送许可随 [`SendHold`] 进入连接 owner registry，只有 transport 任务与缓冲确实
/// 销毁之后才释放；失败或断开时它们随本次调用立即释放。
#[allow(clippy::too_many_arguments)]
async fn run_direct_generation(
    direct: Arc<DirectGeneration>,
    account_id: AccountId,
    received_at: tokio::time::Instant,
    scope: Option<Arc<ConnectionScope>>,
    idempotency_key: String,
    endpoint: &str,
    mut parameters: RequestParameters,
    reference_images: Vec<InputImage>,
    mask: Option<InputImage>,
) -> Result<Response, ApiError> {
    let model = parameters
        .remove("model")
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| ApiError::bad_request("missing_model", "model is required"))?;
    let request = DirectExecutionRequest {
        account_id,
        model,
        endpoint: endpoint.to_owned(),
        native_parameters: parameters,
        reference_images,
        mask,
        idempotency_key,
    };
    // 执行与发送许可都在受理前预留：取不到就不执行这次尚未发生费用的请求。
    let lease = direct
        .supervisor
        .try_reserve_execution()
        .ok_or_else(direct_capacity_unavailable)?;
    let send = direct
        .supervisor
        .try_reserve_send()
        .ok_or_else(direct_capacity_unavailable)?;
    // 总期限 D 从收到请求头起算；执行侧与 handler 等的是同一个绝对时刻。
    let deadline = received_at + direct.supervisor.total_deadline();
    let ExecutionHandle { outcome, gate } =
        direct
            .supervisor
            .spawn(direct.service.clone(), request, lease, deadline);
    // 连接层观察到客户端断开时置取消这次执行尚未开始的外部动作；handler 结束即撤销登记。
    let _tracked = scope.map(|scope| scope.track(&gate));
    // Handler 等到 D 再加工应用层的收尾宽限：先让应用层把"确定未提交 / 已确认结算 / 事实未知"
    // 交回来，只有连这个兜底也到点才回 outcome_unknown。
    let wait = deadline.saturating_duration_since(tokio::time::Instant::now())
        + direct.supervisor.finalization_grace();
    let outcome = match tokio::time::timeout(wait, outcome).await {
        Ok(Ok(outcome)) => outcome,
        // 执行任务在投递结果前消失（异常）：没有可返回的载荷。
        Ok(Err(_recv)) => return Err(direct_internal()),
        // 收尾宽限也过了：执行事实仍未知，保留占用交对账；不谎称未提交（Spec 0005 §4）。
        Err(_) => return Err(outcome_unknown_timeout()),
    };
    match outcome.result {
        Ok(success) => direct_success_response(
            success,
            outcome.lease,
            send,
            direct.supervisor.send_window(),
        ),
        // 事实未知且总期限已过：这是"期限到达仍未确认"，按 504 回应；期限前的一般未知仍是 502。
        Err(DirectExecutionError::OutcomeUnknown) if tokio::time::Instant::now() >= deadline => {
            Err(outcome_unknown_timeout())
        }
        Err(error) => Err(map_direct_error(error)),
    }
}

/// 成功响应：created 与内存里的 data 直接构造，不读 Job、不重放结果。
///
/// 发送期限在**交接时**定为绝对时刻，交给连接层执行：body 是否继续被读取不影响它（RFC 0018 §8.1）。
fn direct_success_response(
    success: seeai_application::DirectExecutionSuccess,
    lease: ExecutionLease,
    send: SendLease,
    window: Duration,
) -> Result<Response, ApiError> {
    let body = SyncImageResponse {
        created: success
            .payload
            .created
            .unwrap_or_else(|| Utc::now().timestamp()),
        data: success.payload.images,
    };
    let payload = serde_json::to_vec(&body).map_err(|error| ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "internal_error",
        message: error.to_string(),
        retry_after: None,
    })?;
    let hold = SendHold::new(
        tokio::time::Instant::now() + window,
        lease,
        send,
        payload.len(),
    );
    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(payload))
        .map_err(|error| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal_error",
            message: error.to_string(),
            retry_after: None,
        })?;
    response.extensions_mut().insert(hold);
    Ok(response)
}

/// 直接执行的拒绝 → Spec 0005 §4 的对客码：同键四投影、结果未知、确定失败与既有应用错误。
fn map_direct_error(error: DirectExecutionError) -> ApiError {
    match error {
        DirectExecutionError::RequestInProgress { retry_after } => ApiError {
            status: StatusCode::CONFLICT,
            code: "request_in_progress",
            message: "the original request with this idempotency key is still executing".to_owned(),
            retry_after: Some(retry_after),
        },
        DirectExecutionError::ResultNotRetained => ApiError {
            status: StatusCode::CONFLICT,
            code: "result_not_retained",
            message: "the original request completed and its result is not retained; use a new idempotency key for another billable call".to_owned(),
            retry_after: None,
        },
        DirectExecutionError::OutcomeUnknown => ApiError {
            status: StatusCode::BAD_GATEWAY,
            code: "outcome_unknown",
            message: "the platform could not confirm the outcome of this request".to_owned(),
            retry_after: None,
        },
        DirectExecutionError::RequestTimeout => request_timeout(),
        DirectExecutionError::ResultDeliveryTimeout => result_delivery_timeout(),
        DirectExecutionError::OriginalFailure { code } => match code {
            PublicErrorCode::ContentRejected => ApiError {
                status: StatusCode::BAD_REQUEST,
                code: "content_rejected",
                message: "the submitted content was rejected".to_owned(),
                retry_after: None,
            },
            PublicErrorCode::OutcomeUnknown => ApiError {
                status: StatusCode::BAD_GATEWAY,
                code: "outcome_unknown",
                message: "the platform could not confirm the outcome of this request".to_owned(),
                retry_after: None,
            },
            PublicErrorCode::PlatformUnavailable => ApiError {
                status: StatusCode::BAD_GATEWAY,
                code: "platform_unavailable",
                message: "the platform could not complete this request".to_owned(),
                retry_after: None,
            },
        },
        // 同一把幂等键换了请求指纹：admit 报冲突，按 §4 的 idempotency_conflict 投影。
        DirectExecutionError::Application(ApplicationError::Conflict(_)) => ApiError {
            status: StatusCode::CONFLICT,
            code: "idempotency_conflict",
            message: "the same idempotency key was used with a different request".to_owned(),
            retry_after: None,
        },
        DirectExecutionError::Application(error) => ApiError::from(error),
    }
}

/// 本机执行/发送容量不足：这是平台侧容量，不是调用方发太密（Spec 0005 §3）。
fn direct_capacity_unavailable() -> ApiError {
    ApiError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        code: "platform_unavailable",
        message: "the platform has no local execution capacity for this request".to_owned(),
        retry_after: None,
    }
}

/// 总期限到达、事实仍未知：504 outcome_unknown，保留占用交对账；不改写成"确定未提交"。
fn outcome_unknown_timeout() -> ApiError {
    ApiError {
        status: StatusCode::GATEWAY_TIMEOUT,
        code: "outcome_unknown",
        message: "the platform could not confirm the outcome of this request within the time limit"
            .to_owned(),
        retry_after: None,
    }
}

/// 受理前正文慢读超时：408 request_timeout，不建任何执行记录（Spec 0005 §4）。
fn slow_read_timeout() -> ApiError {
    ApiError {
        status: StatusCode::REQUEST_TIMEOUT,
        code: "request_timeout",
        message: "the request body was not read within the configured limit".to_owned(),
        retry_after: None,
    }
}

/// 总期限到达时确定未提交生成且占用已释放：504 request_timeout（Spec 0005 §4）。
fn request_timeout() -> ApiError {
    ApiError {
        status: StatusCode::GATEWAY_TIMEOUT,
        code: "request_timeout",
        message: "the request timed out before the platform asked the provider to generate"
            .to_owned(),
        retry_after: None,
    }
}

/// 已确认成功结算但图片来不及准备返回：504 result_delivery_timeout，原请求已完成并收费、
/// 结果不保留（Spec 0005 §4）。
fn result_delivery_timeout() -> ApiError {
    ApiError {
        status: StatusCode::GATEWAY_TIMEOUT,
        code: "result_delivery_timeout",
        message: "the original request completed and was charged, but the result could not be delivered in time".to_owned(),
        retry_after: None,
    }
}

/// 直接执行开着但入口中间件没放账户：配置/装配错误，属于平台自身故障。
fn generation_account_missing() -> ApiError {
    ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "internal_error",
        message: "the direct execution entry is missing its authenticated account".to_owned(),
        retry_after: None,
    }
}

/// 执行任务异常消失：没有可返回的载荷。
fn direct_internal() -> ApiError {
    ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "internal_error",
        message: "the execution task ended without a result".to_owned(),
        retry_after: None,
    }
}

async fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<AccountId, ApiError> {
    let token = bearer_token(headers)?;
    match state.identity.authenticate(token).await {
        Ok(identity) => Ok(identity.account_id),
        // 速率超限是"你是谁我们知道了，但现在太密"，与"这把密钥无效"要分开：混成 401 会让
        // 调用方以为该换密钥，而它其实只需要等一会儿。
        Err(ApplicationError::RateLimitExceeded { retry_after }) => Err(ApiError {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "rate_limit_exceeded",
            message: "too many requests for this API key; retry after the seconds in the \
                      Retry-After header"
                .to_owned(),
            retry_after: Some(retry_after),
        }),
        Err(_) => Err(invalid_api_key()),
    }
}

/// 无效或已吊销的 API Key：生成与上传两个入口共用同一份对客信封。
fn invalid_api_key() -> ApiError {
    ApiError {
        status: StatusCode::UNAUTHORIZED,
        code: "invalid_api_key",
        message: "API key is invalid or revoked".to_owned(),
        retry_after: None,
    }
}

/// 管理员认证中间件：认凭据，并把"是哪个管理员"放进这次请求的审计上下文。
///
/// 一处认证、一处进入审计作用域，所以所有写操作的 `admin_id` 都能填对，而不必改十几个处理器的签名。
/// 共享令牌不指向具体的人，作用域不进入、审计那一列留空——`actor` 仍然说明"经哪条路径做的"。
async fn require_admin_middleware(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Result<axum::response::Response, ApiError> {
    match state.admin_identity(request.headers()).await? {
        Some(admin_id) => Ok(with_admin_id(admin_id, next.run(request)).await),
        None => Ok(next.run(request).await),
    }
}

/// 管理端未授权：**共享令牌写错了、会话无效、会话过期都回这一个答复**。
///
/// 状态码与错误码沿用既有实现（引入会话不改变这条既有语义）；调用方因此分不出自己拿的是哪种凭据、
/// 也分不出凭据是不存在还是过期。
fn admin_forbidden() -> ApiError {
    ApiError {
        status: StatusCode::FORBIDDEN,
        code: "admin_forbidden",
        message: "admin authorization failed".to_owned(),
        retry_after: None,
    }
}

/// 对客未认证：没凭据、会话过期、已退出都是它。
fn unauthorized() -> ApiError {
    ApiError {
        status: StatusCode::UNAUTHORIZED,
        code: "authorization_required",
        message: "Bearer authorization is required".to_owned(),
        retry_after: None,
    }
}

/// 常量时间比较：令牌判等不该因为"前几个字符对不对"而在耗时上泄漏信息。
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right.iter())
        .fold(0_u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}

/// 管理端的 bearer 凭据：**没带、格式不对、空值**都回 [`admin_forbidden`]。
///
/// 与 [`bearer_token`] 的差别就在这里：管理面对"凭据不对"回的是 403 `admin_forbidden`，所以"没带凭据"
/// 也必须回同一个答复——否则调用方按状态码就能分出自己是不是根本没带，而 Spec §4.1 要的正是两者相同。
/// 对客面用的是 [`bearer_token`]（未认证一律 401），两边各自一致。
fn admin_bearer_token(headers: &HeaderMap) -> Result<&str, ApiError> {
    bearer_token(headers).map_err(|_| admin_forbidden())
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
            retry_after: None,
        })
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    /// `Retry-After`（秒）。只有"等一下再来"这类错误有它：调用方要的答案不是"你错了"，是"什么时候
    /// 再来"，不给它就只能盲目重试。
    retry_after: Option<Duration>,
}

impl ApiError {
    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "not_found",
            message: message.into(),
            retry_after: None,
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal_error",
            message: message.into(),
            retry_after: None,
        }
    }

    fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code,
            message: message.into(),
            retry_after: None,
        }
    }
}

impl From<ApplicationError> for ApiError {
    fn from(error: ApplicationError) -> Self {
        // 两类"等一下再来"的错误要把等待时长带到对客响应上，所以它们在匹配之前先被取出来。
        // `Retry-After` 不是"你错了"，是"什么时候再来"，消费者要的答案就在那个数里。
        let retry_after = match &error {
            ApplicationError::DailySpendLimitExceeded { retry_after }
            | ApplicationError::RateLimitExceeded { retry_after } => Some(*retry_after),
            _ => None,
        };
        // 两条平台侧故障有自己的 warn（它们更常见、更值得被看见），5xx 的兜底日志因此要避开它们，
        // 否则同一个错误会有两条日志、其中一条还说不清是哪一类。
        let mut logged_as_warning = false;
        let (status, code) = match &error {
            ApplicationError::Validation(_) => (StatusCode::BAD_REQUEST, "validation_error"),
            // 调用方这次请求在参数上不成立（例如合同没声明图片字段却带了图）：与一般校验失败分开，
            // 说得更具体，调用方才知道该去掉哪个字段或换模型。
            ApplicationError::InvalidParameter(_) => (StatusCode::BAD_REQUEST, "invalid_parameter"),
            ApplicationError::NoEligibleOffering(reason) => {
                // 请求本身没违反合同，是平台的供给面承载不了它：对客说成平台侧故障，不是参数错。
                // 它发生在受理之前、没有 Job 可以记录，所以这里留一条日志——平台侧的供给问题
                // 必须能被运营发现（"一条候选都承载不了"往往意味着发布时少声明了一个字段）。
                logged_as_warning = true;
                tracing::warn!(reason = %reason, "no offering can carry the request");
                (StatusCode::SERVICE_UNAVAILABLE, "platform_unavailable")
            }
            ApplicationError::RequestCostCeilingExceeded(reason) => {
                // 平台自己划的成本护栏挡住了这次执行：客户的余额可能够、请求本身也没错，是这次
                // 执行可能让平台付得太多。同样发生在受理之前、没有 Job 可记，因此在这里留日志
                // ——这道护栏撞上的时候，运营要么调上限，要么改那条候选的定价。
                logged_as_warning = true;
                tracing::warn!(reason = %reason, "the request cost ceiling rejected an acceptance");
                (StatusCode::SERVICE_UNAVAILABLE, "platform_unavailable")
            }
            ApplicationError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            ApplicationError::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
            // 名称撞了要换的是名称本身：与邮箱、账户绑定撞了（`conflict`）区分开，界面上才能给出
            // 说得对的那句话（"换一个名称"而不是"这个邮箱已经注册过了"）。
            ApplicationError::NameTaken(_) => (StatusCode::CONFLICT, "name_taken"),
            ApplicationError::InsufficientBalance => {
                (StatusCode::PAYMENT_REQUIRED, "insufficient_balance")
            }
            ApplicationError::TooManyInFlight => {
                (StatusCode::TOO_MANY_REQUESTS, "too_many_in_flight")
            }
            // 渠道全局未决任务已满：换账户也进不来，是平台侧不可用（503），不是"你发太密"（429）。
            ApplicationError::PlatformCapacityExhausted => {
                (StatusCode::SERVICE_UNAVAILABLE, "platform_unavailable")
            }
            // 总期限在提交声明落库前到点：确定没有发出生成请求，按 504 request_timeout 回
            // （Spec 0005 §4）。
            ApplicationError::ExecutionDeadlineExceeded => {
                (StatusCode::GATEWAY_TIMEOUT, "request_timeout")
            }
            // 速率超限（每密钥速率，或公开鉴权端点的失败尝试）都是"太密了"：等待时长由上面的
            // `retry_after` 提取带出去，`Retry-After` 头对两条路都成立。
            ApplicationError::RateLimitExceeded { .. } => {
                (StatusCode::TOO_MANY_REQUESTS, "rate_limit_exceeded")
            }
            // 今天已经花到运营设的额度：与上面两个 429 各用各的码，消费者才分得清"慢点再来"
            // "并发太多""今天到头了"。
            ApplicationError::DailySpendLimitExceeded { .. } => {
                (StatusCode::TOO_MANY_REQUESTS, "daily_spend_limit_exceeded")
            }
            ApplicationError::Configuration(_)
            | ApplicationError::Persistence(_)
            | ApplicationError::Reconciliation(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
            }
        };
        let message = if status.is_server_error() {
            // 5xx 的对客文案不泄漏内部细节，因此**日志是唯一能看见原因的地方**：配置错误、仓储
            // 错误或对账错误在这里留下类别与完整错误内容，排障不必靠猜。4xx 不打这类日志——那是
            // 调用方自己的问题，数量由调用方决定。
            if status != StatusCode::SERVICE_UNAVAILABLE && !logged_as_warning {
                tracing::error!(
                    category = error_category(&error),
                    error = %error,
                    "request failed with a server error"
                );
            }
            "The server could not complete the request".to_owned()
        } else {
            error.to_string()
        };
        Self {
            status,
            code,
            message,
            retry_after,
        }
    }
}

/// 这个错误属于哪一类：写进 5xx 的那条日志，让排障一眼看出该去哪一层找（配置、仓储还是对账）。
fn error_category(error: &ApplicationError) -> &'static str {
    match error {
        ApplicationError::Configuration(_) => "configuration",
        ApplicationError::Persistence(_) => "persistence",
        ApplicationError::Reconciliation(_) => "reconciliation",
        ApplicationError::Validation(_)
        | ApplicationError::InvalidParameter(_)
        | ApplicationError::NoEligibleOffering(_)
        | ApplicationError::RequestCostCeilingExceeded(_)
        | ApplicationError::NotFound(_)
        | ApplicationError::Conflict(_)
        | ApplicationError::NameTaken(_)
        | ApplicationError::InsufficientBalance
        | ApplicationError::TooManyInFlight
        | ApplicationError::PlatformCapacityExhausted
        | ApplicationError::ExecutionDeadlineExceeded
        | ApplicationError::RateLimitExceeded { .. }
        | ApplicationError::DailySpendLimitExceeded { .. } => "request",
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut headers = HeaderMap::new();
        if let Some(retry_after) = self.retry_after {
            // 向上取整到秒：说"1 秒后可以再来"必须真的够等，向下取整会让调用方在窗口边界上再撞一次。
            let seconds = retry_after.as_secs() + u64::from(retry_after.subsec_millis() > 0);
            if let Ok(value) = HeaderValue::from_str(&seconds.max(1).to_string()) {
                headers.insert(header::RETRY_AFTER, value);
            }
        }
        (
            self.status,
            headers,
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

/// 客户历史翻页游标的加密密钥：32 字节的 base64。
///
/// **必须配**：游标是加密载荷，没有密钥就给不出也解不开下一页。缺失或格式无效时**启动失败**并点名
/// 配置，而不是等到客户翻第二页才报错。密钥只从环境变量读，不进仓库、日志或响应；换密钥会让旧游标
/// 失效（页面重新查询即可）。
fn history_cursor_key() -> Result<[u8; HISTORY_CURSOR_KEY_LEN]> {
    let raw = required_env("CUSTOMER_HISTORY_CURSOR_KEY")?;
    let decoded = STANDARD
        .decode(raw.trim())
        .context("CUSTOMER_HISTORY_CURSOR_KEY must be base64")?;
    decoded.as_slice().try_into().with_context(|| {
        format!(
            "CUSTOMER_HISTORY_CURSOR_KEY must decode to {HISTORY_CURSOR_KEY_LEN} bytes (got {})",
            decoded.len()
        )
    })
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

/// 渠道全局未决任务上限：GENERATION_MAX_CHANNEL_IN_FLIGHT（默认 32），多副本经数据库槽位共同遵守。
fn generation_max_channel_in_flight() -> Result<u64> {
    match env::var("GENERATION_MAX_CHANNEL_IN_FLIGHT") {
        Ok(value) if !value.trim().is_empty() => value
            .trim()
            .parse::<u64>()
            .context("GENERATION_MAX_CHANNEL_IN_FLIGHT must be an integer"),
        _ => Ok(32),
    }
}

/// 读一个非负整数环境变量，没给或给空取默认值。
fn generation_env_usize(name: &str, default: usize) -> Result<usize> {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => value
            .trim()
            .parse::<usize>()
            .with_context(|| format!("{name} must be an integer")),
        _ => Ok(default),
    }
}

/// 读一个可缺省的非负整数环境变量：**没配**与**配了但读不出来**是两回事。
///
/// 没配返回 `None`（调用方据此推导一个自洽的缺省），配了但不是一个非负整数就拒绝启动——那是个
/// 配错的部署，不是一个可以替它拿主意的缺省。
fn generation_env_usize_optional(name: &str) -> Result<Option<usize>> {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => value
            .trim()
            .parse::<usize>()
            .with_context(|| format!("{name} must be an integer"))
            .map(Some),
        _ => Ok(None),
    }
}

/// 本机在飞执行可预占的内存总量缺省值（2 GiB）。
const DEFAULT_MAX_MEMORY_BYTES: usize = 2 * 1024 * 1024 * 1024;

/// 请求 JSON 结构的四条计数上限（RFC 0018 §2.2）。
///
/// 缺省值只在 `seeai_domain` 里有一份推导（从 [`seeai_domain::SUPPORTED_REQUEST_WIRE_BYTES`]
/// 推出，说明在 `crates/domain/src/request_structure.rs`）。这里额外接受四个部署变量，但**只允许
/// 收紧**：配置值大于推导值就拒绝启动——放宽会让"解析结构 ≤ 18 MiB"这个预留推导不再成立，必须先
/// 重新实测 `GATEWAY_REQUEST_PARSE_BYTES` 并改常数，不能靠一个环境变量悄悄绕过。
fn request_json_limits_from_env() -> Result<RequestJsonLimits> {
    let read = |name: &str, derived: usize| -> Result<usize> {
        let value = generation_env_usize(name, derived)?;
        if value == 0 {
            bail!("{name} must be positive: a zero request structure limit rejects every request");
        }
        if value > derived {
            bail!(
                "{name}={value} is wider than the derived limit {derived}; request structure limits \
                 may only be tightened, because the per-execution parse reservation is derived from \
                 the derived values"
            );
        }
        Ok(value)
    };
    Ok(RequestJsonLimits {
        max_depth: read(
            REQUEST_JSON_MAX_DEPTH_ENV,
            seeai_domain::REQUEST_JSON_MAX_DEPTH,
        )?,
        max_nodes: read(
            REQUEST_JSON_MAX_NODES_ENV,
            seeai_domain::REQUEST_JSON_MAX_NODES,
        )?,
        max_object_fields: read(
            REQUEST_JSON_MAX_OBJECT_FIELDS_ENV,
            seeai_domain::REQUEST_JSON_MAX_OBJECT_FIELDS,
        )?,
        max_string_bytes: read(
            REQUEST_JSON_MAX_STRING_BYTES_ENV,
            seeai_domain::REQUEST_JSON_MAX_STRING_BYTES,
        )?,
    })
}

const REQUEST_JSON_MAX_DEPTH_ENV: &str = "GENERATION_REQUEST_JSON_MAX_DEPTH";
const REQUEST_JSON_MAX_NODES_ENV: &str = "GENERATION_REQUEST_JSON_MAX_NODES";
const REQUEST_JSON_MAX_OBJECT_FIELDS_ENV: &str = "GENERATION_REQUEST_JSON_MAX_OBJECT_FIELDS";
const REQUEST_JSON_MAX_STRING_BYTES_ENV: &str = "GENERATION_REQUEST_JSON_MAX_STRING_BYTES";

/// 连接驱动的缺省容量。启动组合校验与 [`transport_config`] 共用这一份，不各写一个数。
const DEFAULT_MAX_CONNECTIONS: usize = 1024;
const DEFAULT_H2_MAX_CONCURRENT_STREAMS: usize = 128;
/// 连接缓冲的缺省上限（字节）：HTTP/1 解析缓冲与 HTTP/2 发送缓冲。单次执行的预留把它们按配置
/// 计入 transport 那一项，`transport_config()` 与预留用的是同一份缺省。
const DEFAULT_HTTP1_MAX_BUF_BYTES: usize = 64 * 1024;
const DEFAULT_HTTP2_MAX_SEND_BUF_BYTES: usize = 1024 * 1024;

/// 直接执行的容量组合：名额之间必须自洽，否则有些名额永远取不到（RFC 0018 §2.3）。
///
/// 字段就是各自环境变量的取值；这里只判它们之间的关系，不判某个值本身对不对（那由各自的读取处判）。
struct CapacityCombination {
    /// `GENERATION_EXECUTION_SLOTS`：显式配的，或按内存预算推导出来的。
    execution_slots: usize,
    /// 单次执行的字节预留，由各 Driver 声明的字节上限算出。
    execution_memory_bytes: usize,
    /// `GENERATION_MAX_MEMORY_BYTES`。
    max_memory_bytes: usize,
    /// `GENERATION_READ_SLOTS`。
    read_slots: usize,
    /// `GENERATION_SEND_SLOTS`。
    send_slots: usize,
    /// `API_MAX_CONNECTIONS`。
    max_connections: usize,
    /// `API_H2_MAX_CONCURRENT_STREAMS`。
    h2_max_concurrent_streams: usize,
}

/// 启动时的组合校验：不自洽就**拒绝启动并点名那两个/三个配置**。
///
/// 三条判据都问同一件事——"配置出来的名额，上层资源盖得住吗"：
///
/// 1. 内存预算至少够一次执行（否则一次都跑不了）；
/// 2. 执行名额 × 单次预留 ≤ 内存预算（否则多出来的执行名额是空的：`try_reserve_execution` 先拿
///    名额再拿字节，预算不够时那次执行会被内存拒绝）；
/// 3. 读取/发送名额 ≤ 连接容量（每条连接最多 `API_H2_MAX_CONCURRENT_STREAMS` 条流；HTTP/1 是 1，
///    所以这是**保守可达**下界：比这还多出来的名额不可能被任何连接持有）。
///
/// 这里**不**把任何上限改小：调小 `GENERATION_EXECUTION_SLOTS` 或抬高
/// `GENERATION_MAX_MEMORY_BYTES` 是运维的决定，进程只负责说出来。
fn validate_capacity_combination(capacity: &CapacityCombination) -> Result<(), String> {
    if capacity.execution_slots == 0 {
        return Err("GENERATION_EXECUTION_SLOTS must be positive".to_owned());
    }
    if capacity.read_slots == 0 {
        return Err("GENERATION_READ_SLOTS must be positive".to_owned());
    }
    if capacity.send_slots == 0 {
        return Err("GENERATION_SEND_SLOTS must be positive".to_owned());
    }
    if capacity.execution_memory_bytes == 0 {
        return Err("the per-execution memory reservation must be positive".to_owned());
    }
    if capacity.max_memory_bytes < capacity.execution_memory_bytes {
        return Err(format!(
            "GENERATION_MAX_MEMORY_BYTES is {} bytes, below the {} bytes one execution needs; \
             the memory budget must cover at least one execution",
            capacity.max_memory_bytes, capacity.execution_memory_bytes
        ));
    }
    let slots_fit = capacity
        .execution_slots
        .checked_mul(capacity.execution_memory_bytes);
    if slots_fit.is_none_or(|needed| needed > capacity.max_memory_bytes) {
        return Err(format!(
            "GENERATION_EXECUTION_SLOTS ({}) times the {} bytes one execution needs exceeds \
             GENERATION_MAX_MEMORY_BYTES ({} bytes); those execution slots can never be reserved, \
             so raise the memory budget or lower the slot count",
            capacity.execution_slots, capacity.execution_memory_bytes, capacity.max_memory_bytes
        ));
    }
    let connection_capacity = capacity
        .max_connections
        .saturating_mul(capacity.h2_max_concurrent_streams);
    if capacity.read_slots > connection_capacity {
        return Err(format!(
            "GENERATION_READ_SLOTS ({}) exceeds what API_MAX_CONNECTIONS ({}) times \
             API_H2_MAX_CONCURRENT_STREAMS ({}) can hold ({}); raise the connection capacity or \
             lower the read slot count",
            capacity.read_slots,
            capacity.max_connections,
            capacity.h2_max_concurrent_streams,
            connection_capacity
        ));
    }
    if capacity.send_slots > connection_capacity {
        return Err(format!(
            "GENERATION_SEND_SLOTS ({}) exceeds what API_MAX_CONNECTIONS ({}) times \
             API_H2_MAX_CONCURRENT_STREAMS ({}) can hold ({}); a send permit is held until its \
             connection is destroyed, so raise the connection capacity or lower the send slot count",
            capacity.send_slots,
            capacity.max_connections,
            capacity.h2_max_concurrent_streams,
            connection_capacity
        ));
    }
    Ok(())
}

/// 读一个非负整数环境变量（字节或秒），没给或给空取默认值。
fn generation_env_u64(name: &str, default: u64) -> Result<u64> {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => value
            .trim()
            .parse::<u64>()
            .with_context(|| format!("{name} must be an integer")),
        _ => Ok(default),
    }
}

/// 每把 API Key 的请求速率上限（默认每分钟 60 次）。读法与上一项相同，两个环境变量：
/// `GENERATION_RATE_LIMIT_REQUESTS_PER_WINDOW` 与 `GENERATION_RATE_LIMIT_WINDOW_MS`。
///
/// 它是**运维取值**，不是产品档位：不同部署（内部工具、压测、开发机）要挡住的数量级差得很远，
/// 所以留成部署期可调，而不是写死在代码里。
fn generation_rate_limit() -> Result<GenerationRateLimit> {
    Ok(GenerationRateLimit::from_env()?)
}

/// 公开鉴权端点来源维采信哪个受信头：`AUTH_SOURCE_HEADER`（例如 `x-real-ip`）。
///
/// 不设时退回**连接对端地址**：开发直连与没有代理的部署都靠它。采信受信头要求 API 不能被绕过
/// 代理直连——否则调用方自带同名头就能伪造来源；这条部署边界的取舍见设计 0016 §3。
fn auth_source_header() -> Result<Option<header::HeaderName>> {
    let name = match env::var("AUTH_SOURCE_HEADER") {
        Ok(value) if !value.trim().is_empty() => value,
        _ => return Ok(None),
    };
    let name = header::HeaderName::from_bytes(name.trim().as_bytes())
        .with_context(|| format!("AUTH_SOURCE_HEADER must be a valid header name: {name}"))?;
    Ok(Some(name))
}

/// 每账户每日扣费上限（默认每天 50 美元等值）。读法与上面两项相同，一个环境变量：
/// `GENERATION_MAX_DAILY_SPEND_MICROUSD`，单位 microusd。
///
/// 它是**运营取值**，不是产品档位：它挡的是"没人看管的脚本在一天里把余额烧光"，不是一个
/// 精算过的客户额度，所以留成部署期可调。它与限流的读法相同、**判据不同**——限流读缓存里的
/// 计数，这一项每次都从账本聚合，因为"今天已经花掉多少"是事实。
fn generation_daily_spend_limit() -> Result<GenerationDailySpendLimit> {
    Ok(GenerationDailySpendLimit::from_env()?)
}

/// 单次请求的上游**成本上限**：`GENERATION_MAX_REQUEST_COST_MICROUSD`（microusd，默认 10 元等值）。
///
/// 判据、两处判定与它的边界见 [`RequestCostCeiling`]。它与 `GENERATION_MAX_COST_MICROUSD` **不是
/// 同一个量**：那个是查不到供给封顶保底值时的兜底**保底额**。
fn cost_ceiling() -> Result<RequestCostCeiling> {
    Ok(RequestCostCeiling::from_env()?)
}

/// 会话有效期：`SESSION_TTL_SECONDS`，缺省 12 小时。
fn session_ttl() -> Result<ChronoDuration> {
    duration_from_env("SESSION_TTL_SECONDS", 12 * 60 * 60)
}

/// 口令重置令牌的有效期：`PASSWORD_RESET_TTL_SECONDS`，缺省 30 分钟。
///
/// 比会话短得多：它能改口令，暴露窗口越小越好。
fn password_reset_ttl() -> Result<ChronoDuration> {
    duration_from_env("PASSWORD_RESET_TTL_SECONDS", 30 * 60)
}

/// 读一个"秒数"环境变量，缺省给 `default_seconds`；0 或不可解析都点名报错。
fn duration_from_env(name: &str, default_seconds: i64) -> Result<ChronoDuration> {
    let seconds = match env::var(name) {
        Ok(raw) => raw
            .trim()
            .parse::<i64>()
            .with_context(|| format!("{name} must be a whole number of seconds"))?,
        Err(_) => default_seconds,
    };
    if seconds <= 0 {
        bail!("{name} must be positive");
    }
    Ok(ChronoDuration::seconds(seconds))
}

/// 引导管理员账号（`ADMIN_EMAIL` + `ADMIN_PASSWORD`）。
///
/// 两个都没给 ⇒ **不建号**，只留一条 warn：后台登录不可用这件事要能被发现，但不该让 API 起不来
/// （客户侧不受影响，客户自己注册）。只给一个 ⇒ 配置错误，启动失败并点名缺哪一个。账号已存在时
/// 引导**不改口令**——否则运维改过的口令会在每次重启时被打回环境变量里的那个。
/// 引导要做什么：读两个环境变量，判定三条分支里走哪一条。
///
/// 抽成纯函数是为了让三条分支**都能被断言**，而不是只能"起一个进程看它退不退出"——两条都给的日志、
/// 两个都不给的警告文案，都属于难在子进程之外观察的东西。
#[derive(Debug, PartialEq, Eq)]
enum AdminSeed {
    /// 两个都给：建账号（已存在则什么都不做）。
    Create { email: String, password: String },
    /// 两个都没给：明确警告，**不建号**，进程照起——后台登不进去这件事要能被发现，但不该让 API 起不来。
    WarnNoAccount,
    /// 只给一个：配置错了，启动失败并点名"谁在、谁缺"。
    Reject {
        present: &'static str,
        missing: &'static str,
    },
}

/// 判定走哪条分支。两个变量都按"非空才算给了"处理：空串与没配是一回事（部署里很常见）。
fn admin_seed_decision(email: Option<String>, password: Option<String>) -> AdminSeed {
    let email = email.filter(|value| !value.trim().is_empty());
    let password = password.filter(|value| !value.is_empty());
    match (email, password) {
        (Some(email), Some(password)) => AdminSeed::Create { email, password },
        (None, None) => AdminSeed::WarnNoAccount,
        (Some(_), None) => AdminSeed::Reject {
            present: "ADMIN_EMAIL",
            missing: "ADMIN_PASSWORD",
        },
        (None, Some(_)) => AdminSeed::Reject {
            present: "ADMIN_PASSWORD",
            missing: "ADMIN_EMAIL",
        },
    }
}

/// 没有管理员账号时的警告文案。
///
/// 抽成常量是为了让用例断言的是**生产那句**，而不是在测试里再抄一遍字面量——抄一遍的写法在"把生产
/// 那句删掉"之后仍然通过，等于没验（复核抓到的就是这个）。文案本身要能让人直接改对：点明缺少哪两个
/// 变量，以及后果（后台登不进去）。
const NO_ADMIN_ACCOUNT_WARNING: &str = "no ADMIN_EMAIL/ADMIN_PASSWORD: no admin account was created, \
     the admin console cannot be logged into until one exists";

async fn seed_admin_account(state: &AppState) -> Result<()> {
    match admin_seed_decision(
        env::var("ADMIN_EMAIL").ok(),
        env::var("ADMIN_PASSWORD").ok(),
    ) {
        AdminSeed::Create { email, password } => {
            state.identity.seed_admin(&email, &password).await?;
        }
        AdminSeed::WarnNoAccount => {
            tracing::warn!("{NO_ADMIN_ACCOUNT_WARNING}");
        }
        AdminSeed::Reject { present, missing } => {
            bail!("{present} is set but {missing} is missing");
        }
    }
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .json()
        .init();
}

#[cfg(test)]
mod tests;
