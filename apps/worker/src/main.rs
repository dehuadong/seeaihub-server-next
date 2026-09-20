use anyhow::{Context, Result};
use chrono::Duration as ChronoDuration;
use seeai_adapter_aihubmix::AihubmixAdapterFactory;
use seeai_adapter_apimart::ApimartAdapterFactory;
use seeai_adapter_sdk::ProviderCredential;
use seeai_application::{
    AdapterRegistry, ApplicationError, AssetStore, CredentialProvider, HubRepository, WorkerService,
};
use seeai_object_storage::ObjectStoreAssetStore;
use seeai_persistence::PgHubRepository;
use std::{env, sync::Arc, time::Duration};
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
    let lease_seconds = parse_env("WORKER_LEASE_SECONDS", 900_i64)?;
    let provider_timeout = Duration::from_secs(parse_env("PROVIDER_TIMEOUT_SECONDS", 660_u64)?);
    if lease_seconds <= i64::try_from(provider_timeout.as_secs()).unwrap_or(i64::MAX) {
        anyhow::bail!("WORKER_LEASE_SECONDS must exceed PROVIDER_TIMEOUT_SECONDS");
    }
    let repository = Arc::new(PgHubRepository::connect(&database_url, 10).await?);
    repository.migrate().await?;
    let repository_port: Arc<dyn HubRepository> = repository;
    let store: Arc<dyn AssetStore> = Arc::new(ObjectStoreAssetStore::from_env()?);
    // 组合工厂：按 adapter_key 分派到各渠道自己的 Driver（纯装配）。
    let adapters: Arc<dyn seeai_application::AdapterFactory> =
        Arc::new(AdapterRegistry::new(vec![
            Arc::new(AihubmixAdapterFactory),
            Arc::new(ApimartAdapterFactory),
        ]));
    let worker = WorkerService::new(
        repository_port,
        store,
        adapters,
        Arc::new(EnvironmentCredentialProvider),
        worker_id.clone(),
        ChronoDuration::seconds(lease_seconds),
        provider_timeout,
    )?;
    info!(%worker_id, "worker started");
    loop {
        tokio::select! {
            signal = tokio::signal::ctrl_c() => {
                signal.context("failed to listen for shutdown signal")?;
                info!("worker stopping");
                return Ok(());
            }
            result = worker.run_once() => {
                match result {
                    Ok(true) => {}
                    Ok(false) => tokio::time::sleep(poll_interval).await,
                    Err(error) => {
                        error!(error = %error, "worker iteration failed");
                        tokio::time::sleep(poll_interval).await;
                    }
                }
            }
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
