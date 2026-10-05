//! 有界传输：单请求超时、无隐式重试、不跟随重定向。
//!
//! 平台只发 PUT 与 HEAD 两个请求，重试编排由应用层拥有；这一层不认环境变量、不认数据库，
//! 也不跟随重定向（重定向会把签名材料带到另一个地址）。

use async_trait::async_trait;
use seeai_adapter_sdk::AdapterError;
use std::time::Duration;
use url::Url;

/// 一次出站 HTTP 请求；头名小写、值原样。
pub(crate) struct HttpRequest {
    pub method: reqwest::Method,
    pub url: Url,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// 一次出站 HTTP 响应：状态与响应头（头名小写）。
pub(crate) struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
}

/// 传输失败（网络错误、连接超时）：按可重试分类，不带 `Retry-After`。
#[derive(Debug)]
pub(crate) struct TransportError(pub String);

#[async_trait]
pub(crate) trait HttpTransport: Send + Sync {
    async fn execute(&self, request: HttpRequest) -> Result<HttpResponse, TransportError>;
}

/// reqwest 实现：单请求超时、不跟随重定向、无隐式重试。
pub(crate) struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    pub(crate) fn new(request_timeout: Duration) -> Result<Self, AdapterError> {
        let client = reqwest::Client::builder()
            .timeout(request_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| AdapterError::Configuration(error.to_string()))?;
        Ok(Self { client })
    }
}

#[async_trait]
impl HttpTransport for ReqwestTransport {
    async fn execute(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let mut builder = self.client.request(request.method, request.url);
        for (name, value) in request.headers {
            builder = builder.header(name, value);
        }
        let response = builder
            .body(request.body)
            .send()
            .await
            .map_err(|error| TransportError(error.to_string()))?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_ascii_lowercase(),
                    value.to_str().unwrap_or_default().to_owned(),
                )
            })
            .collect();
        Ok(HttpResponse { status, headers })
    }
}
