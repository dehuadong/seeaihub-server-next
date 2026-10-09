//! 上传用例：配置装载、媒体与上限校验、对象键、写入与元数据核验、失败分类到对客码的收口。
//!
//! 上传存储是平台自己调用的**外部服务**，不是生成上游：它没有 Offering、不进 Runtime Revision、
//! 不参与选路，也不产生执行记录、资金占用与计量。端口是 `ObjectStorage`（只有 PUT 与 HEAD
//! 两个操作），访问密钥经既有 `CredentialProvider` **按请求**解析，密钥只作调用参数传给端口。
//! 行为合同由[图片上传与对象存储 Spec](../../../docs/contracts/0007-image-upload-and-object-storage.md)拥有，
//! 取值域、启动期形状校验与签名机制由[对象存储上传设计](../../../.agents/notes/implemented/platform/2026-10-04-reference-image-upload.md)拥有。

use crate::{ApplicationError, CredentialProvider};
use async_trait::async_trait;
use seeai_adapter_sdk::ProviderCredential;
use seeai_domain::{
    AccountId, UploadMediaType, UploadWriteFailure, new_object_key, within_single_file_limit,
};
use std::{sync::Arc, time::Duration};
use thiserror::Error;
use url::{Host, Url};

/// 访问密钥标识的环境变量名：两条固定引用之一。
pub const UPLOAD_STORAGE_ACCESS_KEY_ID_ENV: &str = "UPLOAD_STORAGE_ACCESS_KEY_ID";
/// 访问密钥的环境变量名：两条固定引用之一。
pub const UPLOAD_STORAGE_ACCESS_KEY_SECRET_ENV: &str = "UPLOAD_STORAGE_ACCESS_KEY_SECRET";

/// 上传请求体上限的缺省值：单文件 20 MiB 加 multipart 协议余量。
pub const DEFAULT_UPLOAD_MAX_REQUEST_BYTES: usize = 21 * 1024 * 1024;
/// 本机上传并发许可数的缺省值。
pub const DEFAULT_UPLOAD_SLOTS: usize = 4;
/// 本机上传内存预算的缺省值：足以覆盖全部上传许可各一次请求体。
pub const DEFAULT_UPLOAD_MAX_BUFFER_BYTES: usize = 96 * 1024 * 1024;
/// 单次写对象存储请求超时的缺省值（秒）。
pub const DEFAULT_UPLOAD_REQUEST_TIMEOUT_SECONDS: u64 = 30;
/// 上传正文慢读上限的缺省值（秒）。
pub const DEFAULT_UPLOAD_SLOW_READ_TIMEOUT_SECONDS: u64 = 30;
/// 单次上传写入的总尝试次数上限缺省值。
pub const DEFAULT_UPLOAD_RETRY_MAX_ATTEMPTS: u32 = 3;
/// 固定退避基准秒数的缺省值。
pub const DEFAULT_UPLOAD_RETRY_BACKOFF_BASE_SECONDS: u64 = 1;

/// 一次对象存储调用的访问密钥：两条固定引用各自解析出来的值，成对传进端口。
#[derive(Clone, Copy)]
pub struct ObjectStorageCredentials<'a> {
    pub access_key_id: &'a ProviderCredential,
    pub access_key_secret: &'a ProviderCredential,
}

/// 写入一个对象所需的入参：桶与键用于拼 canonical URI，`url` 是已定好寻址形态的完整地址。
pub struct PutObjectRequest<'a> {
    pub bucket: &'a str,
    pub key: &'a str,
    pub url: &'a Url,
    pub content_type: &'a str,
    pub body: &'a [u8],
}

/// 读取一个对象元数据所需的入参。
pub struct HeadObjectRequest<'a> {
    pub bucket: &'a str,
    pub key: &'a str,
    pub url: &'a Url,
}

/// 对象的元数据：写入之后核验字节长度与内容类型用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectMetadata {
    pub byte_length: u64,
    pub content_type: Option<String>,
}

/// 上传路径要用的对象存储操作：只有 PUT 与 HEAD，不发 GET、也不签发预签名 URL。
///
/// 失败分类由[对象存储上传设计](../../../.agents/notes/implemented/platform/2026-10-04-reference-image-upload.md) 判定，实现方
/// 只做 HTTP 状态到 [`UploadWriteFailure`] 的映射，重试编排在应用层。
#[async_trait]
pub trait ObjectStorage: Send + Sync {
    /// 写入对象，禁止覆盖同名对象。
    async fn put_object(
        &self,
        request: PutObjectRequest<'_>,
        credentials: ObjectStorageCredentials<'_>,
    ) -> Result<(), UploadWriteFailure>;

    /// 读取对象元数据（字节长度与内容类型）。
    async fn head_object(
        &self,
        request: HeadObjectRequest<'_>,
        credentials: ObjectStorageCredentials<'_>,
    ) -> Result<ObjectMetadata, UploadWriteFailure>;
}

/// 上传期间客户端是否已经离开；重试编排在每次退避与下一次请求之前读它。
///
/// 它只表达"服务端观测到对端离开"这一事实，不产生对客错误码。
pub trait UploadCancellation: Send + Sync {
    fn is_cancelled(&self) -> bool;
}

/// 永不断开的取消信号：没有连接监视的调用方与用例用它。
pub struct NeverCancelled;

impl UploadCancellation for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// 上传存储的配置：region、bucket 与已定好寻址形态的 endpoint。
///
/// 访问密钥不在这个类型里：它们是两条固定引用，每次上传各经 [`CredentialProvider`] 解析一次。
#[derive(Debug, Clone)]
pub struct UploadStorageConfig {
    pub region: String,
    pub bucket: String,
    /// endpoint origin（含 scheme）；真实 OSS 必须是 `https`，loopback 允许 `http`。
    pub endpoint: Url,
    /// 寻址形态：loopback 端点走 path-style，其余走虚拟主机式。
    pub path_style: bool,
}

impl UploadStorageConfig {
    /// 从环境变量装载：整组都不给返回 `None`，只给一部分或形状不合法返回 `Configuration`。
    ///
    /// 启动期只判形状、不探测对象存储：可达性、bucket 是否存在与桶是否匿名可读都不在这里判。
    pub fn from_env() -> Result<Option<Self>, ApplicationError> {
        let region = optional_env("UPLOAD_STORAGE_REGION");
        let bucket = optional_env("UPLOAD_STORAGE_BUCKET");
        let endpoint = optional_env("UPLOAD_STORAGE_ENDPOINT");
        let key_id = optional_env(UPLOAD_STORAGE_ACCESS_KEY_ID_ENV);
        let key_secret = optional_env(UPLOAD_STORAGE_ACCESS_KEY_SECRET_ENV);
        if region.is_none()
            && bucket.is_none()
            && endpoint.is_none()
            && key_id.is_none()
            && key_secret.is_none()
        {
            return Ok(None);
        }
        let region = region.ok_or_else(|| missing_var("UPLOAD_STORAGE_REGION"))?;
        if !valid_region(region.trim()) {
            return Err(ApplicationError::Configuration(
                "UPLOAD_STORAGE_REGION must be lowercase letters, digits and hyphens, start and \
                 end with a letter or digit, and be at most 63 characters"
                    .to_owned(),
            ));
        }
        let bucket = bucket.ok_or_else(|| missing_var("UPLOAD_STORAGE_BUCKET"))?;
        if !valid_bucket(bucket.trim()) {
            return Err(ApplicationError::Configuration(
                "UPLOAD_STORAGE_BUCKET must be 3 to 63 lowercase letters, digits and hyphens, \
                 start and end with a letter or digit, and contain no dot"
                    .to_owned(),
            ));
        }
        match (&key_id, &key_secret) {
            (Some(_), Some(_)) => {}
            (Some(_), None) => return Err(missing_var(UPLOAD_STORAGE_ACCESS_KEY_SECRET_ENV)),
            (None, Some(_)) => return Err(missing_var(UPLOAD_STORAGE_ACCESS_KEY_ID_ENV)),
            (None, None) => {
                return Err(ApplicationError::Configuration(
                    "UPLOAD_STORAGE_ACCESS_KEY_ID and UPLOAD_STORAGE_ACCESS_KEY_SECRET must be \
                     given together"
                        .to_owned(),
                ));
            }
        }
        let (endpoint, path_style) = match endpoint {
            Some(value) => parse_endpoint(value.trim())?,
            None => (
                Url::parse(&format!("https://oss-{}.aliyuncs.com", region.trim())).map_err(
                    |error| {
                        ApplicationError::Configuration(format!(
                            "the derived UPLOAD_STORAGE_ENDPOINT is invalid: {error}"
                        ))
                    },
                )?,
                false,
            ),
        };
        Ok(Some(Self {
            region: region.trim().to_owned(),
            bucket: bucket.trim().to_owned(),
            endpoint,
            path_style,
        }))
    }

    /// 这次操作的对象 URL：真实 OSS 走虚拟主机式，loopback 端点走 path-style。
    ///
    /// 它同时就是返回给调用方的公网 URL（不带签名、不带过期参数）。
    pub fn object_url(&self, key: &str) -> Result<Url, ApplicationError> {
        let mut url = self.endpoint.clone();
        if self.path_style {
            url.set_path(&format!("/{}/{}", self.bucket, key));
            return Ok(url);
        }
        let host = url.host_str().ok_or_else(|| {
            ApplicationError::Configuration("the upload endpoint has no host".to_owned())
        })?;
        url.set_host(Some(&format!("{}.{}", self.bucket, host)))
            .map_err(|error| {
                ApplicationError::Configuration(format!(
                    "the virtual-host object address is invalid: {error}"
                ))
            })?;
        url.set_path(&format!("/{key}"));
        Ok(url)
    }
}

/// 上传端点的全部配置：存储、上限、超时与重试。
#[derive(Debug, Clone)]
pub struct ImageUploadConfig {
    /// 整组上传存储变量都不给时为 `None`：进程照常启动，上传端点对该请求回
    /// `503 upload_storage_unavailable`。
    pub storage: Option<UploadStorageConfig>,
    /// 上传请求体上限（路由级正文上限）。
    pub max_request_bytes: usize,
    /// 本机同时读上传正文的许可数。
    pub slots: usize,
    /// 本机上传内存预算。
    pub max_buffer_bytes: usize,
    /// 单次写对象存储的请求超时。
    pub request_timeout: Duration,
    /// 上传正文从开始接收到读完的上限。
    pub slow_read_timeout: Duration,
    /// 单次上传写入的总尝试次数上限。
    pub retry_max_attempts: u32,
    /// 固定退避基准。
    pub retry_backoff_base: Duration,
}

impl ImageUploadConfig {
    /// 从环境变量装载全部上传配置；形状不合法时返回 `Configuration` 并点名变量。
    pub fn from_env() -> Result<Self, ApplicationError> {
        let max_request_bytes = upload_env_u64(
            "UPLOAD_MAX_REQUEST_BYTES",
            DEFAULT_UPLOAD_MAX_REQUEST_BYTES as u64,
        )?;
        if max_request_bytes == 0 {
            return Err(ApplicationError::Configuration(
                "UPLOAD_MAX_REQUEST_BYTES must be positive".to_owned(),
            ));
        }
        let slots = upload_env_u64("UPLOAD_SLOTS", DEFAULT_UPLOAD_SLOTS as u64)?;
        if slots == 0 {
            return Err(ApplicationError::Configuration(
                "UPLOAD_SLOTS must be positive".to_owned(),
            ));
        }
        let max_buffer_bytes = upload_env_u64(
            "UPLOAD_MAX_BUFFER_BYTES",
            DEFAULT_UPLOAD_MAX_BUFFER_BYTES as u64,
        )?;
        if max_buffer_bytes == 0 {
            return Err(ApplicationError::Configuration(
                "UPLOAD_MAX_BUFFER_BYTES must be positive".to_owned(),
            ));
        }
        let request_timeout = upload_env_u64(
            "UPLOAD_REQUEST_TIMEOUT_SECONDS",
            DEFAULT_UPLOAD_REQUEST_TIMEOUT_SECONDS,
        )?;
        if request_timeout == 0 {
            return Err(ApplicationError::Configuration(
                "UPLOAD_REQUEST_TIMEOUT_SECONDS must be positive".to_owned(),
            ));
        }
        let slow_read_timeout = upload_env_u64(
            "UPLOAD_SLOW_READ_TIMEOUT_SECONDS",
            DEFAULT_UPLOAD_SLOW_READ_TIMEOUT_SECONDS,
        )?;
        if slow_read_timeout == 0 {
            return Err(ApplicationError::Configuration(
                "UPLOAD_SLOW_READ_TIMEOUT_SECONDS must be positive".to_owned(),
            ));
        }
        let retry_max_attempts = upload_env_u64(
            "UPLOAD_RETRY_MAX_ATTEMPTS",
            u64::from(DEFAULT_UPLOAD_RETRY_MAX_ATTEMPTS),
        )?;
        if retry_max_attempts == 0 {
            return Err(ApplicationError::Configuration(
                "UPLOAD_RETRY_MAX_ATTEMPTS must allow at least one attempt".to_owned(),
            ));
        }
        let retry_backoff_base = upload_env_u64(
            "UPLOAD_RETRY_BACKOFF_BASE_SECONDS",
            DEFAULT_UPLOAD_RETRY_BACKOFF_BASE_SECONDS,
        )?;
        if retry_backoff_base == 0 {
            return Err(ApplicationError::Configuration(
                "UPLOAD_RETRY_BACKOFF_BASE_SECONDS must be positive".to_owned(),
            ));
        }
        Ok(Self {
            storage: UploadStorageConfig::from_env()?,
            max_request_bytes: usize::try_from(max_request_bytes).map_err(|_| {
                ApplicationError::Configuration(
                    "UPLOAD_MAX_REQUEST_BYTES does not fit in this platform's address space"
                        .to_owned(),
                )
            })?,
            slots: usize::try_from(slots).map_err(|_| {
                ApplicationError::Configuration("UPLOAD_SLOTS is too large".to_owned())
            })?,
            max_buffer_bytes: usize::try_from(max_buffer_bytes).map_err(|_| {
                ApplicationError::Configuration(
                    "UPLOAD_MAX_BUFFER_BYTES does not fit in this platform's address space"
                        .to_owned(),
                )
            })?,
            request_timeout: Duration::from_secs(request_timeout),
            slow_read_timeout: Duration::from_secs(slow_read_timeout),
            retry_max_attempts: u32::try_from(retry_max_attempts).map_err(|_| {
                ApplicationError::Configuration("UPLOAD_RETRY_MAX_ATTEMPTS is too large".to_owned())
            })?,
            retry_backoff_base: Duration::from_secs(retry_backoff_base),
        })
    }
}

/// 上传失败到对客结果的收口。
///
/// 对客错误码见[图片上传与对象存储 Spec](../../../docs/contracts/0007-image-upload-and-object-storage.md) §5；
/// 客户端断开不对客返回错误码，只是服务端观测事实。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ImageUploadError {
    /// 类型不在允许范围、0 字节或魔数无法识别。
    #[error("the uploaded file type is not accepted")]
    UnsupportedMediaType,
    /// 声明的 `Content-Type` 与魔数判定不一致。
    #[error("the declared content type does not match the file content")]
    MediaTypeMismatch,
    /// 单文件达到或超过 20 MiB。
    #[error("the uploaded file is too large")]
    ImageTooLarge,
    /// 上传存储未配置，或密钥解析返回 `Configuration` 类错误。
    #[error("upload storage is not configured")]
    UploadStorageUnavailable,
    /// 对象存储不可达、终态失败、重试耗尽或元数据核验不一致。
    #[error("object storage is unavailable")]
    ObjectStoreUnavailable,
    /// 客户端在上传完成前断开：不返回 URL，也不产生对客错误码。
    #[error("the client disconnected before the upload finished")]
    ClientDisconnected,
}

/// 一次成功上传的结果：公网可读 URL、服务端判定的规范 MIME 与实际写入字节数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadedImage {
    pub url: String,
    pub media_type: &'static str,
    pub byte_length: u64,
}

/// 一次上传的写入目标与调用材料：键、地址、内容类型、字节、凭证与取消信号。
struct UploadTarget<'a> {
    storage: &'a UploadStorageConfig,
    key: &'a str,
    url: &'a Url,
    content_type: &'a str,
    bytes: &'a [u8],
    credentials: ObjectStorageCredentials<'a>,
    cancellation: &'a dyn UploadCancellation,
}

/// 上传用例：校验媒体与上限、构造对象键、写入、核验元数据、组装结果。
#[derive(Clone)]
pub struct ImageUploadService {
    storage: Arc<dyn ObjectStorage>,
    credentials: Arc<dyn CredentialProvider>,
    config: Arc<ImageUploadConfig>,
}

impl ImageUploadService {
    #[must_use]
    pub fn new(
        storage: Arc<dyn ObjectStorage>,
        credentials: Arc<dyn CredentialProvider>,
        config: ImageUploadConfig,
    ) -> Self {
        Self {
            storage,
            credentials,
            config: Arc::new(config),
        }
    }

    /// 上传一个文件：校验 → 构造对象键 → PUT（同一键重试）→ HEAD 核验 → 返回公网 URL。
    ///
    /// `account_id` 是上传端点鉴权得到的调用者账户，只用于对象键前缀（`reference-media/{account_id}/…`）。
    /// 上传不触碰账户、账本与执行记录：它不计费、不计量、不限配额，也不产生执行记录。
    pub async fn upload(
        &self,
        account_id: AccountId,
        bytes: &[u8],
        declared_content_type: Option<&str>,
        cancellation: &dyn UploadCancellation,
    ) -> Result<UploadedImage, ImageUploadError> {
        let storage = self
            .config
            .storage
            .as_ref()
            .ok_or(ImageUploadError::UploadStorageUnavailable)?;
        let byte_length =
            u64::try_from(bytes.len()).map_err(|_| ImageUploadError::ImageTooLarge)?;
        if !within_single_file_limit(byte_length) {
            return Err(ImageUploadError::ImageTooLarge);
        }
        let media_type = UploadMediaType::from_magic_bytes(bytes)
            .ok_or(ImageUploadError::UnsupportedMediaType)?;
        if let Some(declared) = declared_content_type
            && declared != media_type.canonical_mime()
        {
            return Err(ImageUploadError::MediaTypeMismatch);
        }
        // 访问密钥按请求解析：两条固定引用各取一次，密钥只作调用参数，不存进服务对象。
        let access_key_id = self.resolve(UPLOAD_STORAGE_ACCESS_KEY_ID_ENV)?;
        let access_key_secret = self.resolve(UPLOAD_STORAGE_ACCESS_KEY_SECRET_ENV)?;
        let credentials = ObjectStorageCredentials {
            access_key_id: &access_key_id,
            access_key_secret: &access_key_secret,
        };
        let key = new_object_key(account_id, media_type);
        let url = storage
            .object_url(&key)
            .map_err(|_| ImageUploadError::UploadStorageUnavailable)?;
        let content_type = media_type.canonical_mime();

        let target = UploadTarget {
            storage,
            key: &key,
            url: &url,
            content_type,
            bytes,
            credentials,
            cancellation,
        };
        self.put_with_retry(&target).await?;
        let metadata = self.head_with_retry(&target).await?;
        if metadata.byte_length != byte_length
            || metadata.content_type.as_deref() != Some(content_type)
        {
            // 写入后元数据与提交不一致即失败关闭：不返回 URL，对象成为孤儿。
            return Err(ImageUploadError::ObjectStoreUnavailable);
        }
        Ok(UploadedImage {
            url: url.to_string(),
            media_type: content_type,
            byte_length,
        })
    }

    fn resolve(&self, reference: &str) -> Result<ProviderCredential, ImageUploadError> {
        self.credentials
            .resolve(reference)
            // 密钥解析返回 Configuration 类错误按"上传存储不可用"回，不是 500。
            .map_err(|_| ImageUploadError::UploadStorageUnavailable)
    }

    async fn put_with_retry(&self, target: &UploadTarget<'_>) -> Result<(), ImageUploadError> {
        let mut attempt = 1;
        loop {
            if target.cancellation.is_cancelled() {
                return Err(ImageUploadError::ClientDisconnected);
            }
            let request = PutObjectRequest {
                bucket: &target.storage.bucket,
                key: target.key,
                url: target.url,
                content_type: target.content_type,
                body: target.bytes,
            };
            match self.storage.put_object(request, target.credentials).await {
                Ok(()) => return Ok(()),
                Err(failure) => {
                    self.after_failure(failure, attempt, target.cancellation)
                        .await?;
                    attempt += 1;
                }
            }
        }
    }

    async fn head_with_retry(
        &self,
        target: &UploadTarget<'_>,
    ) -> Result<ObjectMetadata, ImageUploadError> {
        let mut attempt = 1;
        loop {
            if target.cancellation.is_cancelled() {
                return Err(ImageUploadError::ClientDisconnected);
            }
            let request = HeadObjectRequest {
                bucket: &target.storage.bucket,
                key: target.key,
                url: target.url,
            };
            match self.storage.head_object(request, target.credentials).await {
                Ok(metadata) => return Ok(metadata),
                Err(failure) => {
                    self.after_failure(failure, attempt, target.cancellation)
                        .await?;
                    attempt += 1;
                }
            }
        }
    }

    /// 一次失败之后：终态直接收口；可重试且还有额度就退避，否则按终态收口。
    async fn after_failure(
        &self,
        failure: UploadWriteFailure,
        attempt: u32,
        cancellation: &dyn UploadCancellation,
    ) -> Result<(), ImageUploadError> {
        let retry_after_seconds = match failure {
            UploadWriteFailure::Terminal => {
                return Err(ImageUploadError::ObjectStoreUnavailable);
            }
            UploadWriteFailure::Retryable {
                retry_after_seconds,
            } => retry_after_seconds,
        };
        if attempt >= self.config.retry_max_attempts {
            return Err(ImageUploadError::ObjectStoreUnavailable);
        }
        if !self.wait_backoff(retry_after_seconds, cancellation).await {
            return Err(ImageUploadError::ClientDisconnected);
        }
        Ok(())
    }

    /// 退避：对象存储给了有界整数秒 `Retry-After` 就按它等，否则按固定基准。
    ///
    /// 取消先于退避到达时不再等待；等待期间客户端离开时下一次请求也不会发出。
    async fn wait_backoff(
        &self,
        retry_after_seconds: Option<u64>,
        cancellation: &dyn UploadCancellation,
    ) -> bool {
        if cancellation.is_cancelled() {
            return false;
        }
        let wait = retry_after_seconds
            .map(Duration::from_secs)
            .unwrap_or(self.config.retry_backoff_base);
        tokio::time::sleep(wait).await;
        !cancellation.is_cancelled()
    }
}

/// 读一个非空环境变量；不存在或只有空白时返回 `None`。
fn optional_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn missing_var(name: &str) -> ApplicationError {
    ApplicationError::Configuration(format!(
        "{name} is required when upload storage is configured"
    ))
}

/// 读一个整数环境变量；没给或给空取缺省值。
fn upload_env_u64(name: &str, default: u64) -> Result<u64, ApplicationError> {
    match optional_env(name) {
        Some(value) => value
            .trim()
            .parse::<u64>()
            .map_err(|_| ApplicationError::Configuration(format!("{name} must be an integer"))),
        None => Ok(default),
    }
}

/// region 形状：小写字母、数字与连字符，首尾是字母或数字，长度不超过 63。
fn valid_region(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && starts_and_ends_alphanumeric(value)
}

/// bucket 形状：3–63 位同类字符，首尾是字母或数字，不含点号。
fn valid_bucket(value: &str) -> bool {
    (3..=63).contains(&value.len())
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && starts_and_ends_alphanumeric(value)
}

fn starts_and_ends_alphanumeric(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let last = value.chars().next_back().unwrap_or(first);
    first.is_ascii_alphanumeric() && last.is_ascii_alphanumeric()
}

/// 解析显式 endpoint：含 scheme 的 origin，`https` 或 loopback 的 `http`，不带凭证、
/// path、query 与 fragment。返回地址与寻址形态（loopback 走 path-style）。
fn parse_endpoint(value: &str) -> Result<(Url, bool), ApplicationError> {
    let invalid = |reason: &str| {
        ApplicationError::Configuration(format!(
            "UPLOAD_STORAGE_ENDPOINT must be an origin such as https://host[:port]: {reason}"
        ))
    };
    let url = Url::parse(value).map_err(|error| invalid(&error.to_string()))?;
    let loopback = match url.host() {
        Some(Host::Ipv4(address)) => address == std::net::Ipv4Addr::LOCALHOST,
        Some(Host::Ipv6(address)) => address == std::net::Ipv6Addr::LOCALHOST,
        Some(Host::Domain(domain)) => domain == "localhost",
        None => return Err(invalid("no host")),
    };
    let scheme_ok = url.scheme() == "https" || (url.scheme() == "http" && loopback);
    if !scheme_ok {
        return Err(invalid(
            "the scheme must be https, or http only for a loopback host",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid("credentials are not allowed"));
    }
    if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
        return Err(invalid("a path, query or fragment is not allowed"));
    }
    Ok((url, loopback))
}

#[cfg(test)]
mod tests;
