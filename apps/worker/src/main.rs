use anyhow::{Context, Result};
use chrono::Duration as ChronoDuration;
use seeai_adapter_aihubmix::AihubmixAdapterFactory;
use seeai_adapter_apimart::ApimartAdapterFactory;
use seeai_adapter_sdk::ProviderCredential;
use seeai_alert_webhook::WebhookAlertSink;
use seeai_application::{
    AccelerationService, AdapterRegistry, ApplicationError, CachePolicy, CredentialProvider,
    HubRepository, NO_CONTRACT_MAX_OUTPUT_IMAGES, PlatformAlerter, RequestTimeoutPolicy,
    RetryPolicy, WorkerService,
};
use seeai_cache_redis::RedisCache;
use seeai_persistence::{PgHubRepository, max_declared_output_images};
use std::{env, num::NonZeroU64, sync::Arc, time::Duration};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Default)]
struct EnvironmentCredentialProvider;

impl CredentialProvider for EnvironmentCredentialProvider {
    fn resolve(&self, reference: &str) -> Result<ProviderCredential, ApplicationError> {
        let value = env::var(reference)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                ApplicationError::Configuration(format!(
                    "provider credential environment {reference} is missing"
                ))
            })?;
        ProviderCredential::new(value)
            .map_err(|error| ApplicationError::Configuration(error.to_string()))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    init_tracing();
    let database_url = required_env("DATABASE_URL")?;
    let worker_id = env::var("WORKER_ID").unwrap_or_else(|_| "worker-local-1".to_owned());
    let poll_interval = Duration::from_millis(parse_env("WORKER_POLL_INTERVAL_MS", 1_000_u64)?);
    let repository = Arc::new(PgHubRepository::connect(&database_url, 10).await?);
    repository.migrate().await?;
    // 超时链整条校验：输出张数上限取自**合同自己声明的取值面**（读库，所以要连库之后才知道），
    // 两条链的比较因此比的是"合同允许的最大一档请求"。租约短于上游超时会让同一个 Job 被另一个
    // worker 领走再调一次上游（付两次钱），而对客窗口短于上游超时是消费者拿到 504、上游照样计费
    // 的那条路——窗口在 API 进程上，所以这个进程也要看到它、也校验它。校验不通过就带着点名到
    // 具体那条链与两边当前值的报错退出，不让进程带着一条断链的服务跑起来。
    let (max_output_images, declared_by, undecodable) =
        max_declared_output_images(repository.pool(), NO_CONTRACT_MAX_OUTPUT_IMAGES).await?;
    let timeouts =
        RequestTimeoutPolicy::from_env(max_output_images).map_err(anyhow::Error::from)?;
    timeouts.validate().map_err(anyhow::Error::from)?;
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
    let lease_seconds = i64::try_from(timeouts.worker_lease.as_secs())
        .context("WORKER_LEASE_SECONDS is out of range")?;
    // 重投策略也是运维取值：**上限**决定最坏情况下一个请求会用掉几次上游调用（也就决定了最坏
    // 情况下多花多少钱），**退避基**决定这几次调用摊在多长的窗口里。两者都随上游的抖动程度与
    // 对客的同步窗口变，所以既不在代码里写死，也不给一个"看起来合理"的隐藏取值：读不到就用
    // 缺省值，读到不合法就带着点名到那个变量的报错退出。
    let retry_policy = RetryPolicy::from_env().map_err(anyhow::Error::from)?;
    info!(
        max_attempts = retry_policy.max_attempts,
        backoff_base_ms = retry_policy.backoff_base.as_millis(),
        "safe retries are configured: a retry only happens when the provider provably did not \
         accept the request"
    );
    let repository_port: Arc<dyn HubRepository> = repository;
    // 组合工厂：按 adapter_key 分派到各渠道自己的 Driver（纯装配）。
    let adapters: Arc<dyn seeai_application::AdapterFactory> =
        Arc::new(AdapterRegistry::new(vec![
            Arc::new(AihubmixAdapterFactory),
            Arc::new(ApimartAdapterFactory),
        ]));
    // 加速层：Worker 只用到它的一半——结算与失败收尾都改余额，提交后要把新余额写穿缓存。
    // `REDIS_URL` 没配时它是空操作，结算路径与没有它时逐位相同。
    let acceleration = match RedisCache::from_env()? {
        Some(cache) => Arc::new(AccelerationService::new(
            repository_port.clone(),
            Arc::new(cache),
            CachePolicy::from_env()?,
        )),
        None => Arc::new(AccelerationService::disabled(repository_port.clone())),
    };
    let worker = WorkerService::new(
        repository_port,
        adapters,
        Arc::new(EnvironmentCredentialProvider),
        worker_id.clone(),
        ChronoDuration::seconds(lease_seconds),
        timeouts,
    )?
    .with_acceleration(acceleration)
    .with_retry_policy(retry_policy);
    // 平台故障告警出口是**配置项**：`PROVIDER_ALERT_WEBHOOK` 没配就没有出口，一条也不外发；
    // 阈值（某候选连续失败几次才告警）只在有出口时才读。地址写错在这里就失败，不让进程带着一个
    // "永远发不出去"的出口跑起来。
    let worker = match WebhookAlertSink::from_env()? {
        Some(sink) => {
            let consecutive_failures = parse_env("PROVIDER_ALERT_CONSECUTIVE_FAILURES", 3_u64)?;
            let consecutive_failures = NonZeroU64::new(consecutive_failures)
                .context("PROVIDER_ALERT_CONSECUTIVE_FAILURES must be at least 1")?;
            info!(
                consecutive_failures = consecutive_failures.get(),
                "platform failure alerts are enabled"
            );
            worker.with_platform_alerts(
                Arc::new(PlatformAlerter::new(Arc::new(sink))),
                consecutive_failures,
            )
        }
        None => worker,
    };
    info!(%worker_id, "worker started");
    // 终止信号只决定"**不再领下一轮**"：正在跑的那一轮（上游调用 + 落账 + 结算）要让它跑完，
    // 否则在飞调用被丢掉，Job 会留在提交中直到租约过期才被回收。因此信号不放在 select 的
    // 分支里直接返回，而是先置位，再把手上这一轮的 future 等完。
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);
    loop {
        let iteration = worker.run_once();
        tokio::pin!(iteration);
        let mut draining = false;
        let result = loop {
            tokio::select! {
                signal = &mut shutdown => {
                    signal.context("failed to listen for shutdown signal")?;
                    if !draining {
                        draining = true;
                        info!("worker draining: finishing the in-flight iteration before exit");
                    }
                }
                result = &mut iteration => break result,
            }
        };
        match result {
            Ok(true) => {}
            Ok(false) => tokio::time::sleep(poll_interval).await,
            Err(error) => {
                error!(error = %error, "worker iteration failed");
                tokio::time::sleep(poll_interval).await;
            }
        }
        if draining {
            info!("worker stopped");
            return Ok(());
        }
    }
}

fn required_env(name: &str) -> Result<String> {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("missing environment {name}"))
}

fn parse_env<T>(name: &str, default: T) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match env::var(name) {
        Ok(value) => value
            .parse()
            .map_err(|error| anyhow::anyhow!("invalid {name}: {error}")),
        Err(_) => Ok(default),
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .json()
        .init();
}
