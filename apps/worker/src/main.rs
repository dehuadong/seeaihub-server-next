use anyhow::{Context, Result};
use chrono::Duration as ChronoDuration;
use seeai_adapter_aihubmix::AihubmixAdapterFactory;
use seeai_adapter_apimart::ApimartAdapterFactory;
use seeai_adapter_sdk::ProviderCredential;
use seeai_alert_webhook::WebhookAlertSink;
use seeai_application::{
    AccelerationService, AdapterRegistry, ApplicationError, CachePolicy, CredentialProvider,
    DEFAULT_EXECUTION_LEASE_SECONDS, ExecutionReconciliationService, ExecutionRepository,
    HubRepository, NO_CONTRACT_MAX_OUTPUT_IMAGES, PlatformAlerter, ReconciliationPolicy,
    RequestTimeoutPolicy, RetryPolicy,
};
use seeai_cache_redis::RedisCache;
use seeai_persistence::{PgHubRepository, max_connections_from_env, max_declared_output_images};
use std::{env, future::Future, num::NonZeroU64, pin::Pin, sync::Arc, task::Poll, time::Duration};
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
    let repository =
        Arc::new(PgHubRepository::connect(&database_url, max_connections_from_env()?).await?);
    repository.migrate().await?;
    // 超时链整条校验：输出张数上限取自**合同自己声明的取值面**（读库，所以要连库之后才知道），
    // 比较因此比的是"合同允许的最大一档请求"。对客窗口短于上游超时是消费者拿到 504、上游照样
    // 计费的那条路——窗口在 API 进程上，所以这个进程也要看到它、也校验它。校验不通过就带着点名
    // 到具体那条链与两边当前值的报错退出，不让进程带着一条断链的服务跑起来。
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
    // 重投策略也是运维取值：**上限**决定最坏情况下一个请求会用掉几次上游调用（也就决定了最坏
    // 情况下多花多少钱），**退避基**决定这几次调用摊在多长的窗口里。对账 Worker 只在"可证明未
    // 受理"时按它重投。
    let retry_policy = RetryPolicy::from_env().map_err(anyhow::Error::from)?;
    info!(
        max_attempts = retry_policy.max_attempts,
        backoff_base_ms = retry_policy.backoff_base.as_millis(),
        "safe retries are configured: a retry only happens when the provider provably did not \
         accept the request"
    );
    let repository_port: Arc<dyn HubRepository> = repository.clone();
    let executions: Arc<dyn ExecutionRepository> = repository;
    // 组合工厂：按 adapter_key 分派到各渠道自己的 Driver（纯装配）。
    let adapters: Arc<dyn seeai_application::AdapterFactory> =
        Arc::new(AdapterRegistry::new(vec![
            Arc::new(AihubmixAdapterFactory),
            Arc::new(ApimartAdapterFactory),
        ]));
    // 加速层：对账的结算与失败收尾都改余额，提交后要把新余额写穿缓存。`REDIS_URL` 没配时
    // 它是空操作，对账路径与没有它时逐位相同。
    let acceleration = match RedisCache::from_env()? {
        Some(cache) => Arc::new(AccelerationService::new(
            repository_port.clone(),
            Arc::new(cache),
            CachePolicy::from_env()?,
        )),
        None => Arc::new(AccelerationService::disabled(repository_port.clone())),
    };
    // 平台故障告警出口是**配置项**：`PROVIDER_ALERT_WEBHOOK` 没配就没有出口，一条也不外发；
    // 阈值（某候选连续失败几次才告警）只在有出口时才读。地址写错在这里就失败，不让进程带着一个
    // "永远发不出去"的出口跑起来。
    let alerting = match WebhookAlertSink::from_env()? {
        Some(sink) => {
            let consecutive_failures = parse_env("PROVIDER_ALERT_CONSECUTIVE_FAILURES", 3_u64)?;
            let consecutive_failures = NonZeroU64::new(consecutive_failures)
                .context("PROVIDER_ALERT_CONSECUTIVE_FAILURES must be at least 1")?;
            info!(
                consecutive_failures = consecutive_failures.get(),
                "platform failure alerts are enabled"
            );
            Some((
                Arc::new(PlatformAlerter::new(Arc::new(sink))),
                consecutive_failures,
            ))
        }
        None => None,
    };
    // 异常对账循环：只接管过期所有权、只读查询、按证据幂等结算或建案。它绝不领取生成任务、
    // 不重发生成请求，也不读正文。
    let execution_lease_seconds = parse_env(
        "GENERATION_EXECUTION_LEASE_SECONDS",
        DEFAULT_EXECUTION_LEASE_SECONDS,
    )?;
    let reconciliation_policy = ReconciliationPolicy::from_env().map_err(anyhow::Error::from)?;
    info!(
        batch_limit = reconciliation_policy.batch_limit,
        query_max_attempts = reconciliation_policy.query_max_attempts,
        query_backoff_base_seconds = reconciliation_policy.query_backoff_base.as_secs(),
        query_backoff_max_seconds = reconciliation_policy.query_backoff_max.as_secs(),
        "reconciliation query scheduling is configured"
    );
    let mut reconciliation = ExecutionReconciliationService::new(
        repository_port,
        executions,
        adapters,
        Arc::new(EnvironmentCredentialProvider),
        worker_id.clone(),
        ChronoDuration::seconds(execution_lease_seconds),
    )
    .with_policy(reconciliation_policy)
    .with_retry_policy(retry_policy)
    .with_acceleration(acceleration);
    if let Some((alerter, _)) = &alerting {
        reconciliation = reconciliation.with_platform_alerts(alerter.clone());
    }
    info!(%worker_id, "worker started");
    let (_drain_signal, drain_control) = tokio::sync::watch::channel(false);
    let signals = ShutdownSignals {
        drain_control,
        interrupt: shutdown_signal(),
    };
    run_until_shutdown(Some(&reconciliation), signals, poll_interval).await;
    Ok(())
}

/// 跑一轮异常对账。
///
/// 对账出错不向上抛：一个轮次的失败不该让进程退出，下一轮再试；错误在这里记日志，返回"这一轮
/// 没干活"让主循环按空闲退避。`reconciliation` 为空表示这轮不带对账（只验停机时序的用例用它）；
/// 生产里始终给 `Some`。
async fn run_round(reconciliation: Option<&ExecutionReconciliationService>) -> bool {
    match reconciliation {
        Some(service) => match service.run_once().await {
            Ok(report) => report.did_work(),
            Err(error) => {
                tracing::error!(error = %error, "the reconciliation iteration failed");
                false
            }
        },
        None => false,
    }
}

/// 主循环等的终止信号 future。
type ShutdownFuture = Pin<Box<dyn Future<Output = std::io::Result<()>> + Send>>;

/// 进程终止信号：SIGINT（Ctrl+C）或 SIGTERM（systemd 与容器的默认信号）。两者走同一条排空路径
/// （口径见 `docs/design/0009-operational-baseline.md` §2）。
///
/// **触发之后一直就绪**：停机源在选定之前会被反复询问，做成可重复轮询让每次询问都给出同一结论；
/// 普通 `async fn` 或 `oneshot` 完成后再被轮询会 panic。
fn shutdown_signal() -> ShutdownFuture {
    let mut interrupt = Box::pin(tokio::signal::ctrl_c());
    #[cfg(unix)]
    let mut terminate =
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(stream) => Some(stream),
            Err(error) => {
                tracing::error!(%error, "failed to listen for SIGTERM; waiting for Ctrl+C only");
                None
            }
        };
    let mut fired = false;
    Box::pin(std::future::poll_fn(move |context| {
        if fired {
            return Poll::Ready(Ok(()));
        }
        if let Poll::Ready(result) = interrupt.as_mut().poll(context) {
            fired = true;
            return Poll::Ready(result);
        }
        #[cfg(unix)]
        if let Some(stream) = terminate.as_mut()
            && let Poll::Ready(Some(())) = stream.poll_recv(context)
        {
            fired = true;
            return Poll::Ready(Ok(()));
        }
        Poll::Pending
    }))
}

/// 停机输入：一个"停止跑下一轮对账"的开关，以及一个"进程要退了"的终止信号。
struct ShutdownSignals {
    /// 由别处置位（例如编排系统的排水接口）；置位之后不再跑下一轮。`watch` 可以反复轮询。
    drain_control: tokio::sync::watch::Receiver<bool>,
    /// 终止信号（SIGINT / SIGTERM）。钉成 `Pin<Box<..>>` 是因为它只在**一个**地方被轮询：每轮现造一个会把"监听"
    /// 反复注册一遍，而"等停机"这件事不需要它对每个轮次都重新就绪。
    interrupt: ShutdownFuture,
}

/// 对账的主循环：直到停机条件成立为止。
///
/// 停机分两级，**在飞的那一轮都不许丢**（丢了 Job 会留在提交中直到所有权过期才被回收）：
/// - `drain_control` 置位 = **排空**：不再跑下一轮，手上这一轮跑完；
/// - `interrupt` 就绪（SIGINT / SIGTERM）= **终止**：同样不打断在飞的那一轮，等它跑完再退。
///
/// 对账一轮与"等停机"**同时**推进：停机请求不必等到某一轮结束才被看见，而停机一旦成立也不再跑
/// 下一轮——手上那一轮仍旧完整跑完（`select!` 只让停机**先被看见**，取消的是"再跑一轮"，
/// 不是那一轮里已经发出的那次上游查询）。
///
/// 停机输入当参数传进来，是为了让这条合同能在测试里**确定地**验：真信号没法在进程内精确投递，
/// 而"什么时候停、停的时候在飞的那一轮怎么办"与信号从哪来无关。
async fn run_until_shutdown(
    reconciliation: Option<&ExecutionReconciliationService>,
    mut signals: ShutdownSignals,
    poll_interval: Duration,
) {
    let mut interrupted = false;
    loop {
        // 已经在排空、或已经收到终止信号：一轮都不再领。
        if interrupted || *signals.drain_control.borrow_and_update() {
            info!("worker stopped");
            return;
        }
        let iteration = run_round(reconciliation);
        tokio::pin!(iteration);
        let mut draining = false;
        let handled = loop {
            // 停机只决定"不再领下一轮"：一旦看见，就不再回到 select!，手上这一轮等完。
            if draining {
                break iteration.await;
            }
            // biased：停机优先于"这一轮刚好跑完"，已就绪的终止不会被同轮完成的迭代盖过。
            tokio::select! {
                biased;
                stop = await_stop(&mut signals) => {
                    if let Some(is_interrupted) = stop {
                        interrupted = interrupted || is_interrupted;
                    }
                    draining = true;
                }
                result = &mut iteration => break result,
            }
        };
        let idle = !handled;
        // 跑完先看要不要停，再谈退避：退避是"没活干、也没人要求停"时的事，排在停机判定之前
        // 会让一次停机白等一个退避周期（运维取值可以是分钟级）。
        if draining || interrupted || *signals.drain_control.borrow_and_update() {
            info!("worker stopped");
            return;
        }
        if idle {
            tokio::time::sleep(poll_interval).await;
        }
    }
}

/// 等一个停机条件成立。
///
/// 返回 `Some(true)` 是终止信号、`Some(false)` 是排空开关；`None` 表示排空开关的发送端没了——
/// 那时没人再能要求排空，继续跑下去等于一个再也停不下来的进程，所以按停止处置。
///
/// 只在这两个信号上等：`watch` 的值变化会唤醒它，终止信号就绪也会。**不设超时分支**，
/// 因为"没人要求停机"本来就该一直等下去（真正的让步由每轮之后的退避负责）。
async fn await_stop(signals: &mut ShutdownSignals) -> Option<bool> {
    tokio::select! {
        result = &mut signals.interrupt => {
            if let Err(error) = result {
                error!(error = %error, "failed to listen for the shutdown signal");
            }
            Some(true)
        }
        changed = signals.drain_control.changed() => match changed {
            Ok(()) => {
                if *signals.drain_control.borrow_and_update() {
                    info!("worker draining: finishing the in-flight iteration before exit");
                }
                Some(false)
            }
            Err(_) => None,
        },
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

#[cfg(test)]
#[path = "worker_loop_tests.rs"]
mod worker_loop_tests;
