//! 阿里云 OSS 对象存储适配器：实现 application 的 `ObjectStorage` 端口。
//!
//! 这一层只做请求构造、V4 签名（header 模式）、有界传输与 HTTP 状态到失败分类的映射：不读环境
//! 变量、不认数据库、不做重试编排（重试在应用层），也不发 GET 或签发预签名 URL。访问密钥只作
//! 调用参数，不进日志。签名规则与对拍向量见[对象存储上传设计](../../docs/design/0021-object-storage-upload.md) §5。

mod signing;
mod transport;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use seeai_adapter_sdk::AdapterError;
use seeai_application::{
    HeadObjectRequest, ObjectMetadata, ObjectStorage, ObjectStorageCredentials, PutObjectRequest,
};
use seeai_domain::UploadWriteFailure;
use signing::{SignedRequest, UNSIGNED_PAYLOAD};
use std::{sync::Arc, time::Duration};
use transport::{HttpRequest, HttpResponse, HttpTransport, ReqwestTransport, TransportError};

/// `Retry-After` 的有界整数秒上界：超出这个范围的不采信，按固定退避等待。
const MAX_RETRY_AFTER_SECONDS: u64 = 300;

/// 本机时钟：每次请求重新取当前 UTC 时刻，不缓存、不复用时间戳。
pub(crate) trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

/// 生产时钟。
pub(crate) struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// 阿里云 OSS 对象存储适配器。
pub struct OssObjectStorage {
    region: String,
    transport: Arc<dyn HttpTransport>,
    clock: Arc<dyn Clock>,
}

impl OssObjectStorage {
    /// 装配一个只支持阿里云 OSS 的适配器；单请求超时由调用方给，不在这里读环境变量。
    pub fn new(region: impl Into<String>, request_timeout: Duration) -> Result<Self, AdapterError> {
        Ok(Self {
            region: region.into(),
            transport: Arc::new(ReqwestTransport::new(request_timeout)?),
            clock: Arc::new(SystemClock),
        })
    }

    /// 这次请求的 `x-oss-date` 与 scope 日期。
    fn request_time(&self) -> (String, String) {
        let now = self.clock.now();
        (
            now.format("%Y%m%dT%H%M%SZ").to_string(),
            now.format("%Y%m%d").to_string(),
        )
    }

    /// 给一组请求头补上 V4 签名；canonical URI 恒为 `/{bucket}/{key}`，`host` 不进 canonical headers。
    fn signed_headers(
        &self,
        method: &str,
        canonical_uri: &str,
        headers: Vec<(String, String)>,
        timestamp: &str,
        date: &str,
        credentials: ObjectStorageCredentials<'_>,
    ) -> Vec<(String, String)> {
        let signed = SignedRequest {
            method,
            canonical_uri,
            canonical_query: "",
            headers: &headers,
            additional_headers: &[],
            hashed_payload: UNSIGNED_PAYLOAD,
            timestamp,
            date,
            region: &self.region,
            access_key_id: credentials.access_key_id.expose(),
            access_key_secret: credentials.access_key_secret.expose(),
        };
        let authorization = signing::authorization(&signed);
        let mut outgoing = headers;
        outgoing.push(("authorization".to_owned(), authorization));
        outgoing
    }
}

#[async_trait]
impl ObjectStorage for OssObjectStorage {
    async fn put_object(
        &self,
        request: PutObjectRequest<'_>,
        credentials: ObjectStorageCredentials<'_>,
    ) -> Result<(), UploadWriteFailure> {
        let (timestamp, date) = self.request_time();
        let headers = vec![
            ("content-type".to_owned(), request.content_type.to_owned()),
            (
                "x-oss-content-sha256".to_owned(),
                UNSIGNED_PAYLOAD.to_owned(),
            ),
            ("x-oss-date".to_owned(), timestamp.clone()),
            ("x-oss-forbid-overwrite".to_owned(), "true".to_owned()),
        ];
        let outgoing = self.signed_headers(
            "PUT",
            &signing::canonical_uri(request.bucket, request.key),
            headers,
            &timestamp,
            &date,
            credentials,
        );
        classify(
            self.transport
                .execute(HttpRequest {
                    method: reqwest::Method::PUT,
                    url: request.url.clone(),
                    headers: outgoing,
                    body: request.body.to_vec(),
                })
                .await,
        )
        .map(|_| ())
    }

    async fn head_object(
        &self,
        request: HeadObjectRequest<'_>,
        credentials: ObjectStorageCredentials<'_>,
    ) -> Result<ObjectMetadata, UploadWriteFailure> {
        let (timestamp, date) = self.request_time();
        let headers = vec![
            (
                "x-oss-content-sha256".to_owned(),
                UNSIGNED_PAYLOAD.to_owned(),
            ),
            ("x-oss-date".to_owned(), timestamp.clone()),
        ];
        let outgoing = self.signed_headers(
            "HEAD",
            &signing::canonical_uri(request.bucket, request.key),
            headers,
            &timestamp,
            &date,
            credentials,
        );
        let response = classify(
            self.transport
                .execute(HttpRequest {
                    method: reqwest::Method::HEAD,
                    url: request.url.clone(),
                    headers: outgoing,
                    body: Vec::new(),
                })
                .await,
        )?;
        // 拿不到字节长度就无法核验写入，按终态失败关闭。
        let Some(byte_length) = header_value(&response.headers, "content-length")
            .and_then(|value| value.trim().parse::<u64>().ok())
        else {
            return Err(UploadWriteFailure::Terminal);
        };
        Ok(ObjectMetadata {
            byte_length,
            content_type: header_value(&response.headers, "content-type").map(str::to_owned),
        })
    }
}

/// 传输失败与 HTTP 状态到失败分类的映射。
fn classify(
    response: Result<HttpResponse, TransportError>,
) -> Result<HttpResponse, UploadWriteFailure> {
    let response = response.map_err(|error| {
        // 有界脱敏诊断：只记传输层的错误文本，不记密钥、签名与对象字节。
        tracing::debug!(error = %error.0, "the object storage request failed");
        UploadWriteFailure::Retryable {
            retry_after_seconds: None,
        }
    })?;
    let retry_after = if response.status == 429 || (500..=599).contains(&response.status) {
        retry_after_seconds(&response.headers)
    } else {
        None
    };
    match response.status {
        200..=299 => Ok(response),
        // 对象存储的 408 是可重试的传输超时；对客的 408 是受理前正文慢读，与这里无关。
        408 => Err(UploadWriteFailure::Retryable {
            retry_after_seconds: None,
        }),
        429 | 500..=599 => Err(UploadWriteFailure::Retryable {
            retry_after_seconds: retry_after,
        }),
        // 401 / 403 / 404 / 409 与其他 4xx 都是终态：不重试、不换键重写。
        _ => Err(UploadWriteFailure::Terminal),
    }
}

/// `Retry-After`：只采信 [1, 300] 的整数秒，其余当作没给。
fn retry_after_seconds(headers: &[(String, String)]) -> Option<u64> {
    let raw = header_value(headers, "retry-after")?.trim();
    let seconds = raw.parse::<u64>().ok()?;
    (1..=MAX_RETRY_AFTER_SECONDS)
        .contains(&seconds)
        .then_some(seconds)
}

fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

#[cfg(test)]
mod tests;
