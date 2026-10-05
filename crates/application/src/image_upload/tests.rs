use super::*;
use seeai_domain::MAX_UPLOAD_BYTES;
use std::{
    collections::VecDeque,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

/// 一次被记录的 PUT：桶、键、地址、内容类型与字节数。
struct RecordedPut {
    bucket: String,
    key: String,
    url: String,
    content_type: String,
    body_len: usize,
}

/// 脚本化的假对象存储：按顺序吐出预先排好的结果，并记录每次调用的入参。
#[derive(Default)]
struct FakeStorage {
    put_results: Mutex<VecDeque<Result<(), UploadWriteFailure>>>,
    head_results: Mutex<VecDeque<Result<ObjectMetadata, UploadWriteFailure>>>,
    puts: Mutex<Vec<RecordedPut>>,
    heads: Mutex<usize>,
}

impl FakeStorage {
    fn with_puts(results: Vec<Result<(), UploadWriteFailure>>) -> Self {
        Self {
            put_results: Mutex::new(results.into()),
            ..Self::default()
        }
    }

    fn with_head(results: Vec<Result<ObjectMetadata, UploadWriteFailure>>) -> Self {
        Self {
            head_results: Mutex::new(results.into()),
            ..Self::default()
        }
    }

    fn put_keys(&self) -> Vec<String> {
        self.puts
            .lock()
            .expect("the fake storage lock")
            .iter()
            .map(|put| put.key.clone())
            .collect()
    }

    fn head_count(&self) -> usize {
        *self.heads.lock().expect("the fake storage lock")
    }
}

#[async_trait]
impl ObjectStorage for FakeStorage {
    async fn put_object(
        &self,
        request: PutObjectRequest<'_>,
        _credentials: ObjectStorageCredentials<'_>,
    ) -> Result<(), UploadWriteFailure> {
        self.puts
            .lock()
            .expect("the fake storage lock")
            .push(RecordedPut {
                bucket: request.bucket.to_owned(),
                key: request.key.to_owned(),
                url: request.url.to_string(),
                content_type: request.content_type.to_owned(),
                body_len: request.body.len(),
            });
        self.put_results
            .lock()
            .expect("the fake storage lock")
            .pop_front()
            .unwrap_or(Ok(()))
    }

    async fn head_object(
        &self,
        _request: HeadObjectRequest<'_>,
        _credentials: ObjectStorageCredentials<'_>,
    ) -> Result<ObjectMetadata, UploadWriteFailure> {
        *self.heads.lock().expect("the fake storage lock") += 1;
        self.head_results
            .lock()
            .expect("the fake storage lock")
            .pop_front()
            .unwrap_or(Ok(ObjectMetadata {
                byte_length: 0,
                content_type: None,
            }))
    }
}

/// 假凭证解析：记录被解析的引用名，并可按需返回 `Configuration` 类错误。
struct FakeCredentials {
    records: Mutex<Vec<String>>,
    fail: bool,
}

impl FakeCredentials {
    fn working() -> Self {
        Self {
            records: Mutex::new(Vec::new()),
            fail: false,
        }
    }

    fn failing() -> Self {
        Self {
            records: Mutex::new(Vec::new()),
            fail: true,
        }
    }

    fn references(&self) -> Vec<String> {
        self.records
            .lock()
            .expect("the fake credentials lock")
            .clone()
    }
}

impl CredentialProvider for FakeCredentials {
    fn resolve(&self, reference: &str) -> Result<ProviderCredential, ApplicationError> {
        self.records
            .lock()
            .expect("the fake credentials lock")
            .push(reference.to_owned());
        if self.fail {
            return Err(ApplicationError::Configuration(format!(
                "provider credential environment {reference} is missing"
            )));
        }
        ProviderCredential::new(format!("value-for-{reference}"))
            .map_err(|error| ApplicationError::Configuration(error.to_string()))
    }
}

fn loopback_storage() -> UploadStorageConfig {
    UploadStorageConfig {
        region: "cn-hangzhou".to_owned(),
        bucket: "my-bucket".to_owned(),
        endpoint: Url::parse("http://127.0.0.1:9000").expect("a loopback endpoint"),
        path_style: true,
    }
}

fn config(storage: Option<UploadStorageConfig>, retry_max_attempts: u32) -> ImageUploadConfig {
    ImageUploadConfig {
        storage,
        max_request_bytes: DEFAULT_UPLOAD_MAX_REQUEST_BYTES,
        slots: DEFAULT_UPLOAD_SLOTS,
        max_buffer_bytes: DEFAULT_UPLOAD_MAX_BUFFER_BYTES,
        request_timeout: Duration::from_secs(1),
        slow_read_timeout: Duration::from_secs(1),
        retry_max_attempts,
        retry_backoff_base: Duration::from_millis(1),
        rate_limit: GenerationRateLimit::default_limit(),
    }
}

fn service(
    storage: Arc<FakeStorage>,
    credentials: Arc<FakeCredentials>,
    max_attempts: u32,
) -> ImageUploadService {
    ImageUploadService::new(
        storage,
        credentials,
        config(Some(loopback_storage()), max_attempts),
    )
}

fn png() -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x0D]);
    bytes
}

fn metadata(byte_length: usize, content_type: &str) -> ObjectMetadata {
    ObjectMetadata {
        byte_length: u64::try_from(byte_length).expect("a length"),
        content_type: Some(content_type.to_owned()),
    }
}

#[tokio::test]
async fn upload_writes_then_verifies_and_returns_the_public_url() {
    let bytes = png();
    let storage = Arc::new(FakeStorage::with_head(vec![Ok(metadata(
        bytes.len(),
        "image/png",
    ))]));
    let credentials = Arc::new(FakeCredentials::working());
    let service = service(storage.clone(), credentials.clone(), 3);

    let uploaded = service
        .upload(&bytes, Some("image/png"), &NeverCancelled)
        .await
        .expect("a successful upload");

    assert_eq!(uploaded.media_type, "image/png");
    assert_eq!(uploaded.byte_length, bytes.len() as u64);
    assert!(
        uploaded
            .url
            .starts_with("http://127.0.0.1:9000/my-bucket/reference-media/")
    );
    assert!(uploaded.url.ends_with(".png"));
    let puts = storage.puts.lock().expect("the fake storage lock");
    assert_eq!(puts.len(), 1);
    assert_eq!(puts[0].bucket, "my-bucket");
    assert!(puts[0].key.starts_with("reference-media/"));
    assert_eq!(puts[0].content_type, "image/png");
    assert_eq!(puts[0].body_len, bytes.len());
    assert_eq!(puts[0].url, uploaded.url);
    assert_eq!(storage.head_count(), 1);
    // 密钥按请求解析两条固定引用，不落进服务对象。
    assert_eq!(
        credentials.references(),
        vec![
            UPLOAD_STORAGE_ACCESS_KEY_ID_ENV.to_owned(),
            UPLOAD_STORAGE_ACCESS_KEY_SECRET_ENV.to_owned()
        ]
    );
}

#[tokio::test]
async fn upload_resolves_credentials_on_every_request() {
    let bytes = png();
    let storage = Arc::new(FakeStorage::with_head(vec![
        Ok(metadata(bytes.len(), "image/png")),
        Ok(metadata(bytes.len(), "image/png")),
    ]));
    let credentials = Arc::new(FakeCredentials::working());
    let service = service(storage, credentials.clone(), 3);
    service
        .upload(&bytes, None, &NeverCancelled)
        .await
        .expect("the first upload");
    service
        .upload(&bytes, None, &NeverCancelled)
        .await
        .expect("the second upload");
    assert_eq!(credentials.references().len(), 4);
}

#[tokio::test]
async fn upload_without_storage_configuration_is_unavailable() {
    let storage = Arc::new(FakeStorage::default());
    let credentials = Arc::new(FakeCredentials::working());
    let service = ImageUploadService::new(storage.clone(), credentials, config(None, 3));
    assert_eq!(
        service.upload(&png(), None, &NeverCancelled).await,
        Err(ImageUploadError::UploadStorageUnavailable)
    );
    assert!(storage.put_keys().is_empty());
}

#[tokio::test]
async fn upload_rejects_unrecognized_and_oversized_content_before_writing() {
    let storage = Arc::new(FakeStorage::default());
    let service = service(storage.clone(), Arc::new(FakeCredentials::working()), 3);
    assert_eq!(
        service.upload(&[], None, &NeverCancelled).await,
        Err(ImageUploadError::UnsupportedMediaType)
    );
    assert_eq!(
        service.upload(b"not an image", None, &NeverCancelled).await,
        Err(ImageUploadError::UnsupportedMediaType)
    );
    // 单文件上限严格小于：等于上限即 413，且不写对象。
    let mut oversized = vec![0_u8; usize::try_from(MAX_UPLOAD_BYTES).expect("the limit")];
    oversized[0..8].copy_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    assert_eq!(
        service.upload(&oversized, None, &NeverCancelled).await,
        Err(ImageUploadError::ImageTooLarge)
    );
    assert!(storage.put_keys().is_empty());
}

#[tokio::test]
async fn upload_rejects_a_declared_content_type_that_disagrees_with_the_magic_bytes() {
    let storage = Arc::new(FakeStorage::default());
    let service = service(storage.clone(), Arc::new(FakeCredentials::working()), 3);
    assert_eq!(
        service
            .upload(&png(), Some("image/jpeg"), &NeverCancelled)
            .await,
        Err(ImageUploadError::MediaTypeMismatch)
    );
    assert!(storage.put_keys().is_empty());
}

#[tokio::test]
async fn upload_retries_the_same_object_key_on_retryable_failures() {
    let bytes = png();
    let storage = Arc::new(FakeStorage::with_puts(vec![
        Err(UploadWriteFailure::Retryable {
            retry_after_seconds: Some(1),
        }),
        Ok(()),
    ]));
    storage
        .head_results
        .lock()
        .expect("the fake storage lock")
        .push_back(Ok(metadata(bytes.len(), "image/png")));
    let service = service(storage.clone(), Arc::new(FakeCredentials::working()), 3);
    let uploaded = service
        .upload(&bytes, None, &NeverCancelled)
        .await
        .expect("a retried upload");
    let keys = storage.put_keys();
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0], keys[1], "retries reuse the same object key");
    assert!(uploaded.url.ends_with(".png"));
}

#[tokio::test]
async fn upload_stops_after_the_attempt_limit() {
    let storage = Arc::new(FakeStorage::with_puts(vec![
        Err(UploadWriteFailure::Retryable {
            retry_after_seconds: None,
        }),
        Err(UploadWriteFailure::Retryable {
            retry_after_seconds: None,
        }),
        Ok(()),
    ]));
    let service = service(storage.clone(), Arc::new(FakeCredentials::working()), 2);
    assert_eq!(
        service.upload(&png(), None, &NeverCancelled).await,
        Err(ImageUploadError::ObjectStoreUnavailable)
    );
    assert_eq!(storage.put_keys().len(), 2);
}

#[tokio::test]
async fn upload_does_not_retry_terminal_failures() {
    let storage = Arc::new(FakeStorage::with_puts(vec![
        Err(UploadWriteFailure::Terminal),
        Ok(()),
    ]));
    let service = service(storage.clone(), Arc::new(FakeCredentials::working()), 3);
    assert_eq!(
        service.upload(&png(), None, &NeverCancelled).await,
        Err(ImageUploadError::ObjectStoreUnavailable)
    );
    assert_eq!(storage.put_keys().len(), 1);
}

#[tokio::test]
async fn upload_fails_closed_when_head_metadata_disagrees() {
    let bytes = png();
    let storage = Arc::new(FakeStorage::with_head(vec![Ok(metadata(
        bytes.len() + 1,
        "image/png",
    ))]));
    let service = service(storage.clone(), Arc::new(FakeCredentials::working()), 3);
    assert_eq!(
        service.upload(&bytes, None, &NeverCancelled).await,
        Err(ImageUploadError::ObjectStoreUnavailable)
    );
    assert_eq!(storage.put_keys().len(), 1);
}

#[tokio::test]
async fn upload_retries_head_without_writing_the_object_again() {
    let bytes = png();
    let storage = Arc::new(FakeStorage::with_head(vec![
        Err(UploadWriteFailure::Retryable {
            retry_after_seconds: None,
        }),
        Ok(metadata(bytes.len(), "image/png")),
    ]));
    let service = service(storage.clone(), Arc::new(FakeCredentials::working()), 3);
    service
        .upload(&bytes, None, &NeverCancelled)
        .await
        .expect("a successful upload after a head retry");
    assert_eq!(storage.put_keys().len(), 1, "the object is written once");
    assert_eq!(storage.head_count(), 2);
}

#[tokio::test]
async fn upload_maps_a_credential_configuration_failure_to_storage_unavailable() {
    let storage = Arc::new(FakeStorage::default());
    let service = service(storage.clone(), Arc::new(FakeCredentials::failing()), 3);
    assert_eq!(
        service.upload(&png(), None, &NeverCancelled).await,
        Err(ImageUploadError::UploadStorageUnavailable)
    );
    assert!(storage.put_keys().is_empty());
}

#[tokio::test]
async fn upload_stops_when_the_client_left_before_a_retry() {
    let bytes = png();
    let storage = Arc::new(FakeStorage::with_puts(vec![
        Err(UploadWriteFailure::Retryable {
            retry_after_seconds: None,
        }),
        Ok(()),
    ]));
    let service = service(storage.clone(), Arc::new(FakeCredentials::working()), 3);
    let cancelled = FlagCancellation(Arc::new(AtomicBool::new(true)));
    assert_eq!(
        service.upload(&bytes, None, &cancelled).await,
        Err(ImageUploadError::ClientDisconnected)
    );
    assert!(storage.put_keys().is_empty());
}

struct FlagCancellation(Arc<AtomicBool>);

impl UploadCancellation for FlagCancellation {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[test]
fn endpoint_shape_rules_accept_https_and_loopback_http_only() {
    assert!(parse_endpoint("https://oss-cn-hangzhou.aliyuncs.com").is_ok());
    assert!(parse_endpoint("https://oss-cn-hangzhou.aliyuncs.com:443").is_ok());
    assert_eq!(
        parse_endpoint("http://127.0.0.1:9000").expect("loopback http"),
        (Url::parse("http://127.0.0.1:9000").expect("a url"), true)
    );
    assert!(parse_endpoint("http://localhost:9000").is_ok());
    assert!(parse_endpoint("http://[::1]:9000").is_ok());
    // 非 loopback 的 http、带凭证、带 path / query / fragment 都是形状不合法。
    assert!(parse_endpoint("http://oss-cn-hangzhou.aliyuncs.com").is_err());
    assert!(parse_endpoint("https://user:secret@oss.example.com").is_err());
    assert!(parse_endpoint("https://oss.example.com/path").is_err());
    assert!(parse_endpoint("https://oss.example.com?x=1").is_err());
    assert!(parse_endpoint("https://oss.example.com#frag").is_err());
}

#[test]
fn region_and_bucket_shape_rules() {
    assert!(valid_region("cn-hangzhou"));
    assert!(valid_region("a"));
    assert!(!valid_region(""));
    assert!(!valid_region("Cn-Hangzhou"));
    assert!(!valid_region("-cn"));
    assert!(!valid_region("cn-"));
    assert!(!valid_region(&"a".repeat(64)));
    assert!(valid_bucket("my-bucket"));
    assert!(valid_bucket("abc"));
    assert!(!valid_bucket("ab"));
    assert!(!valid_bucket("My-Bucket"));
    assert!(!valid_bucket("my.bucket"));
    assert!(!valid_bucket("-bucket"));
    assert!(!valid_bucket(&"a".repeat(64)));
}

#[test]
fn object_url_uses_virtual_host_addressing_except_for_loopback() {
    let virtual_host = UploadStorageConfig {
        region: "cn-hangzhou".to_owned(),
        bucket: "my-bucket".to_owned(),
        endpoint: Url::parse("https://oss-cn-hangzhou.aliyuncs.com").expect("an endpoint"),
        path_style: false,
    };
    assert_eq!(
        virtual_host
            .object_url("reference-media/2026-08-12/x.jpg")
            .expect("a url")
            .as_str(),
        "https://my-bucket.oss-cn-hangzhou.aliyuncs.com/reference-media/2026-08-12/x.jpg"
    );
    assert_eq!(
        loopback_storage()
            .object_url("reference-media/2026-08-12/x.jpg")
            .expect("a url")
            .as_str(),
        "http://127.0.0.1:9000/my-bucket/reference-media/2026-08-12/x.jpg"
    );
}
