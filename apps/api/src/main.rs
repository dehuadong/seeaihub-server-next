use anyhow::{Context, Result, bail};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Multipart, Path, Query, State, multipart::Field},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post, put},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use seeai_adapter_aihubmix::AihubmixAdapterFactory;
use seeai_adapter_apimart::ApimartAdapterFactory;
use seeai_alert_webhook::WebhookAlertSink;
use seeai_application::{
    AccelerationService, AccountSummary, AccountsService, AdapterRegistry, ApplicationError,
    CachePolicy, CreateImageGenerationRequest, CursorPosition, CustomerBillingQuery,
    CustomerLedgerQuery, CustomerUsageKind, CustomerUsageQuery, CustomerUsageScope,
    CustomerUsageStatus, CustomerView, GatewayModelView, GeneratedImage, GenerationDailySpendLimit,
    GenerationRateLimit, GenerationService, HISTORY_CURSOR_KEY_LEN, HistoryFilter, HistoryStream,
    HubRepository, IdentityService, JobView, LedgerAuditor, LedgerEntryView, MAX_OPERATIONAL_LIMIT,
    NO_CONTRACT_MAX_OUTPUT_IMAGES, NewFxRate, PlatformAlerter, PricingService, ProviderCostGapView,
    ProviderFailureKind, ProviderFailureQuery, ProviderFailureView, PublishRuntimeCommand,
    ReconciliationService, RefundReconciliationCommand, RequestCostCeiling, RequestTimeoutPolicy,
    RoutePolicyService, RuntimeService, SelectableOfferingView, decode_history_cursor,
    encode_history_cursor, invalid_history_cursor, with_admin_id,
};
use seeai_cache_redis::RedisCache;
use seeai_domain::{
    AccountId, ChannelId, ImageInputs, ImageParameterKind, JobId, LedgerEntryKind, OfferingId,
    PublishedModel, RoutePolicy, RouteStrategy, contract_image_parameter_kind,
    replace_contract_model_identity,
};
use seeai_persistence::{
    PgHubRepository, material_import::import_supply_materials_from_env, max_declared_output_images,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{collections::BTreeMap, env, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
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
    /// 账实核对：管理员按账户触发，后台执行；只读账本与账户当前值，不在资金路径上。
    ledger_audit: Arc<LedgerAuditor>,
    /// 定价侧的管理员面：折算率的录入与取值（汇率不进不可变修订）。
    pricing: PricingService,
    /// 账户面的管理员用例：建账户与充值——两件事都要在提交成功后把余额写进缓存。
    accounts: AccountsService,
    /// 路由策略的管理员面：读清单与写入（运行期配置，不进不可变修订）。
    route_policies: RoutePolicyService,
    generations: GenerationService,
    /// 同步入口等任务跑完的最长时间。
    sync_wait: Duration,
    /// 健康探测的依赖判据：探一次事实源是否可达（只 `SELECT 1`）。
    repository: Arc<dyn HubRepository>,
    /// 会话有效期（管理员与客户同一档）：部署期配置，缺省 12 小时。
    session_ttl: ChronoDuration,
    /// 口令重置令牌的有效期：比会话更短，缺省 30 分钟。
    password_reset_ttl: ChronoDuration,
    /// 客户历史翻页游标的加密密钥：部署期配置，同一部署的所有 API 实例必须一致；缺失或格式无效时
    /// 进程启动失败（`docs/design/0014-customer-console-navigation-and-history.md` §5）。
    history_cursor_key: [u8; HISTORY_CURSOR_KEY_LEN],
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
    // 两条链的比较因此比的是"合同允许的最大一档请求"。这一进程持有**对客同步等待窗口**——窗口
    // 短于上游超时就是"消费者拿到 504、而上游还在生成、照样计费"那条路；租约在 Worker 上，但两个
    // 进程读同一组变量，所以这里也看得到、也一起校验。校验不过就点名报错退出，不让服务带着一条
    // 断链跑起来。
    let (max_output_images, declared_by, undecodable) =
        max_declared_output_images(repository.pool(), NO_CONTRACT_MAX_OUTPUT_IMAGES).await?;
    let timeouts =
        RequestTimeoutPolicy::from_env(max_output_images).map_err(anyhow::Error::from)?;
    timeouts.validate().map_err(anyhow::Error::from)?;
    let sync_wait = timeouts.sync_wait;
    info!(
        worker_lease_seconds = timeouts.worker_lease.as_secs(),
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
    } else {
        // 速率计数就落在这个缓存上：没有缓存时它无处可落，限流这一层等于不生效（见
        // `AccelerationService::consume_request_slot`）。这是部署期看得见的事实，不是静默降级。
        tracing::warn!(
            "no cache service configured; the per-API-key rate limit does not apply in this process"
        );
    }
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
            .with_rate_limit(acceleration.clone(), generation_rate_limit()?),
        runtime: RuntimeService::new(repository_port.clone(), adapters)
            .with_acceleration(acceleration.clone())
            .with_cost_ceiling(cost_ceiling()?),
        reconciliation: ReconciliationService::new(repository_port.clone())
            .with_acceleration(acceleration.clone()),
        ledger_audit,
        pricing: PricingService::new(repository_port.clone()),
        accounts: AccountsService::new(repository_port.clone())
            .with_acceleration(acceleration.clone()),
        route_policies: RoutePolicyService::new(repository_port.clone()),
        sync_wait,
        repository: repository_port.clone(),
        generations: GenerationService::new(
            repository_port,
            generation_max_cost_microusd()?,
            generation_max_concurrent_jobs()?,
        )
        // 每日扣费上限：定额度是运维取值，判定每次回账本读（见 `GenerationService::create`）。
        .with_daily_spend_limit(generation_daily_spend_limit()?)
        // 成本护栏：发布期与受理期判的是同一个数（见 `RequestCostCeiling`）。
        .with_cost_ceiling(cost_ceiling()?)
        .with_acceleration(acceleration),
        // 会话与重置令牌的有效期：部署期取值（缺省 12 小时 / 30 分钟）。
        session_ttl: session_ttl()?,
        password_reset_ttl: password_reset_ttl()?,
        history_cursor_key: history_cursor_key()?,
    };
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
        .route("/api/v1/api-keys/{key_id}", delete(revoke_api_key))
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
        .route("/v1/account", get(read_own_account))
        .route("/v1/images/generations", post(generate_image))
        .route("/v1/images/edits", post(edit_image))
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
        .route("/v1/customer/ledger", get(read_customer_ledger))
        .route("/v1/customer/usage", get(read_customer_usage))
        .route("/v1/customer/billing", get(read_customer_billing));
    let app = admin
        .merge(public)
        .layer(DefaultBodyLimit::max(16 * 1024 * 1024))
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
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
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
}

#[derive(Debug, Serialize)]
struct CreateAccountResponse {
    account_id: AccountId,
}

async fn create_account(
    State(state): State<AppState>,
    Json(body): Json<CreateAccountBody>,
) -> Result<Json<CreateAccountResponse>, ApiError> {
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
    image_count: u32,
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
            image_count: row.image_count,
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
    /// 密钥标识，就是吊销那条路径上的 `{key_id}`。
    ///
    /// 明文只在这一次响应里出现，事后谁也拿不回来；只回明文的话，"要不要吊销这一把"就只能回库捞 id，
    /// 而管理员没有库权限——所以标识必须和明文一起交到发密钥的人手里。
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

/// 管理员写：吊销一把 API Key（`DELETE /api/v1/api-keys/{key_id}`）。
///
/// 吊销**不删行**：创建与吊销都是历史事实，排障要看这把密钥什么时候被停掉，所以只写 `revoked_at`。
/// 它**立刻**生效——认证路径每次读库判吊销状态、不缓存"有效"，所以这里成功返回之后紧接着的那次
/// 使用就会被拒。重复吊销是成功的空操作（调用方在意的是"现在不可用"，不是这次调用改变了什么）；
/// 键不存在是 404——这里只给已经发出来的行盖章，不创建任何东西。
async fn revoke_api_key(
    State(state): State<AppState>,
    Path(key_id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    state.identity.revoke_api_key(key_id, "admin-api").await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- 身份：登录、会话、口令 ----

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
    let view = state
        .identity
        .open_customer_account(
            &body.email,
            body.password.as_deref(),
            body.account_id.map(AccountId),
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
    let accounts = state.accounts.list_accounts(email, tag, limit).await?;
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
    Json(body): Json<LoginBody>,
) -> Result<(StatusCode, Json<CustomerLoginResponse>), ApiError> {
    let login = state
        .identity
        .register_customer(&body.email, &body.password, state.session_ttl)
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
    Json(body): Json<LoginBody>,
) -> Result<Json<CustomerLoginResponse>, ApiError> {
    let login = state
        .identity
        .login_customer(&body.email, &body.password, state.session_ttl)
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
    Json(body): Json<RedeemPasswordResetBody>,
) -> Result<StatusCode, ApiError> {
    state
        .identity
        .redeem_password_reset(&body.reset_token, &body.new_password)
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
    image_count: u32,
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
) -> Result<Json<OwnAccountResponse>, ApiError> {
    let (_, account_id) = state.require_customer(&headers).await?;
    let account_id = AccountId(account_id);
    let change = state.accounts.read_balance(account_id).await?;
    Ok(Json(OwnAccountResponse {
        balance_microusd: change.balance_microusd,
        held_microusd: change.held_microusd,
        available_microusd: change.available_microusd,
        updated_at: change.updated_at,
    }))
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
            image_count: row.image_count,
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
        "images": summary.images,
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
    Ok(Json(state.runtime.publish(command).await?))
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

/// 受理请求：**平铺**的模型参数 + 图片字段（`image` 与 `image_urls` 同义二选一，`mask` 是遮罩）。
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
        Err(_) => Err(ApiError {
            status: StatusCode::UNAUTHORIZED,
            code: "invalid_api_key",
            message: "API key is invalid or revoked".to_owned(),
            retry_after: None,
        }),
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
        // 每日扣费上限那条路要把"到次日零点还有多久"带到对客响应上，所以它在匹配之前先被
        // 取出来。这是这个转换里**唯一**需要另带走一个值的错误：`Retry-After` 不是"你错了"，
        // 是"什么时候再来"，消费者要的答案就在那个数里。
        let retry_after = match &error {
            ApplicationError::DailySpendLimitExceeded { retry_after } => Some(*retry_after),
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
            ApplicationError::InsufficientBalance => {
                (StatusCode::PAYMENT_REQUIRED, "insufficient_balance")
            }
            ApplicationError::TooManyInFlight => {
                (StatusCode::TOO_MANY_REQUESTS, "too_many_in_flight")
            }
            // 速率超限走到这里时（不是入口那条路径），也只说"太密了"：`Retry-After` 在
            // [`ApiError::from`] 的通用路径上没有位置放，因此认证入口自己那条分支才是对客的正常路径。
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
        | ApplicationError::InsufficientBalance
        | ApplicationError::TooManyInFlight
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

/// 每把 API Key 的请求速率上限（默认每分钟 60 次）。读法与上一项相同，两个环境变量：
/// `GENERATION_RATE_LIMIT_REQUESTS_PER_WINDOW` 与 `GENERATION_RATE_LIMIT_WINDOW_MS`。
///
/// 它是**运维取值**，不是产品档位：不同部署（内部工具、压测、开发机）要挡住的数量级差得很远，
/// 所以留成部署期可调，而不是写死在代码里。
fn generation_rate_limit() -> Result<GenerationRateLimit> {
    Ok(GenerationRateLimit::from_env()?)
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
