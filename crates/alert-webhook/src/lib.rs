//! 告警出口的 HTTP 实现：把一条 [`PlatformAlert`] 以 **POST JSON** 发给配置的地址。
//!
//! 地址是**配置项**（`PROVIDER_ALERT_WEBHOOK`）：没配就没有出口（[`WebhookAlertSink::from_env`]
//! 返回 `None`），不内置默认地址，代码里也不写死任何 URL。
//!
//! 超时与重试都**有界**、都是配置项（`PROVIDER_ALERT_TIMEOUT_MS` / `PROVIDER_ALERT_RETRIES`）：
//! 告警是旁路，不能因为对端不响应把发它的那一轮拖住。重试之间是固定的小退避，不做指数增长。
//!
//! 请求体就是 [`PlatformAlert`] 的四个字段——不带凭证，也不带提示词与图片。

use async_trait::async_trait;
use reqwest::Client;
use seeai_application::{AlertSink, ApplicationError, PlatformAlert};
use std::{env, time::Duration};

/// 单次外发的默认超时（毫秒）。
const DEFAULT_TIMEOUT_MS: u64 = 5_000;
/// 默认重试次数（不含第一次）。
const DEFAULT_RETRIES: u32 = 2;
/// 重试之间的固定退避：告警是旁路，等太久不如把这一条记下来。
const RETRY_BACKOFF: Duration = Duration::from_millis(200);

/// 一个只做"POST JSON"的告警出口。
pub struct WebhookAlertSink {
    client: Client,
    url: reqwest::Url,
    retries: u32,
}

impl WebhookAlertSink {
    /// 从环境变量构造：`PROVIDER_ALERT_WEBHOOK` 没配或为空就是**没有出口**（返回 `None`，不是错误）。
    pub fn from_env() -> Result<Option<Self>, ApplicationError> {
        let Ok(url) = env::var("PROVIDER_ALERT_WEBHOOK") else {
            return Ok(None);
        };
        let url = url.trim();
        if url.is_empty() {
            return Ok(None);
        }
        let timeout =
            Duration::from_millis(parse_env("PROVIDER_ALERT_TIMEOUT_MS", DEFAULT_TIMEOUT_MS)?);
        let retries = parse_env("PROVIDER_ALERT_RETRIES", DEFAULT_RETRIES)?;
        Self::new(url, timeout, retries).map(Some)
    }

    /// 按地址、超时与重试次数构造。**不在这里发任何请求**。
    ///
    /// 地址不是能 POST 的 http(s) 地址时**直接失败**：那是配置写错了，不是"告警暂时发不出去"。
    /// 把配置错误悄悄降级成"每次都发不出去"，运维就永远看不到自己写错了地址。
    pub fn new(url: &str, timeout: Duration, retries: u32) -> Result<Self, ApplicationError> {
        let parsed = reqwest::Url::parse(url).map_err(|error| {
            ApplicationError::Configuration(format!(
                "PROVIDER_ALERT_WEBHOOK is not a usable address: {error}"
            ))
        })?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(ApplicationError::Configuration(format!(
                "PROVIDER_ALERT_WEBHOOK must be an http(s) address, got scheme {}",
                parsed.scheme()
            )));
        }
        let client = Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|error| {
                ApplicationError::Configuration(format!(
                    "could not build the alert webhook client: {error}"
                ))
            })?;
        Ok(Self {
            client,
            url: parsed,
            retries,
        })
    }
}

#[async_trait]
impl AlertSink for WebhookAlertSink {
    async fn send(&self, alert: &PlatformAlert) -> Result<(), ApplicationError> {
        // 载荷就是这条告警本身：不从 Job 上顺手多带任何东西。
        let payload = serde_json::to_value(alert).map_err(|error| {
            ApplicationError::Configuration(format!(
                "the platform alert does not serialize: {error}"
            ))
        })?;
        let attempts = self.retries.saturating_add(1);
        let mut last_failure = String::new();
        for attempt in 1..=attempts {
            if attempt > 1 {
                tokio::time::sleep(RETRY_BACKOFF).await;
            }
            match self
                .client
                .post(self.url.clone())
                .json(&payload)
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => return Ok(()),
                Ok(response) => {
                    last_failure = format!("the webhook answered {}", response.status().as_u16());
                }
                Err(error) => last_failure = format!("the webhook request failed: {error}"),
            }
        }
        Err(ApplicationError::Persistence(format!(
            "the platform alert was not delivered after {attempts} attempt(s): {last_failure}"
        )))
    }
}

fn parse_env<T>(name: &str, default: T) -> Result<T, ApplicationError>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => value.trim().parse().map_err(|error| {
            ApplicationError::Configuration(format!("{name} is not usable: {error}"))
        }),
        _ => Ok(default),
    }
}

#[cfg(test)]
mod tests;
