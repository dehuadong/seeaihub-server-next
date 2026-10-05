use super::*;
use seeai_adapter_sdk::ProviderCredential;
use std::{
    collections::VecDeque,
    sync::{
        Mutex,
        atomic::{AtomicI64, Ordering},
    },
};
use url::Url;

const BUCKET: &str = "my-bucket";
const KEY: &str = "reference-media/2026-08-12/uuid-123.jpg";

#[derive(Clone)]
struct RecordedRequest {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body_len: usize,
}

impl RecordedRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// 脚本化传输：按顺序吐出预先排好的响应，并记录每次请求。
struct ScriptedTransport {
    responses: Mutex<VecDeque<Result<HttpResponse, TransportError>>>,
    requests: Mutex<Vec<RecordedRequest>>,
}

impl ScriptedTransport {
    fn new(responses: Vec<Result<HttpResponse, TransportError>>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> Vec<RecordedRequest> {
        self.requests
            .lock()
            .expect("the scripted transport lock")
            .clone()
    }
}

#[async_trait]
impl HttpTransport for ScriptedTransport {
    async fn execute(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        self.requests
            .lock()
            .expect("the scripted transport lock")
            .push(RecordedRequest {
                method: request.method.as_str().to_owned(),
                url: request.url.to_string(),
                headers: request.headers.clone(),
                body_len: request.body.len(),
            });
        self.responses
            .lock()
            .expect("the scripted transport lock")
            .pop_front()
            .unwrap_or(Ok(HttpResponse {
                status: 200,
                headers: Vec::new(),
            }))
    }
}

struct FixedClock(DateTime<Utc>);

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

/// 每读一次就往前走的时钟：用来证明每次请求重新取时刻。
struct AdvancingClock(AtomicI64);

impl Clock for AdvancingClock {
    fn now(&self) -> DateTime<Utc> {
        let offset = self.0.fetch_add(1, Ordering::SeqCst);
        DateTime::from_timestamp(1_760_000_000 + offset, 0).expect("a timestamp")
    }
}

/// 落档金标准用的固定时刻：2026-08-12T10:30:00Z。
fn fixed() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-08-12T10:30:00Z")
        .expect("a timestamp")
        .with_timezone(&Utc)
}

fn credentials() -> (ProviderCredential, ProviderCredential) {
    (
        ProviderCredential::new("AKIDEXAMPLE".to_owned()).expect("an access key id"),
        ProviderCredential::new("SKEXAMPLE123456".to_owned()).expect("an access key secret"),
    )
}

fn object_url() -> Url {
    Url::parse(
        "https://my-bucket.oss-cn-hangzhou.aliyuncs.com/reference-media/2026-08-12/uuid-123.jpg",
    )
    .expect("an object url")
}

fn build_storage(transport: Arc<ScriptedTransport>, clock: Arc<dyn Clock>) -> OssObjectStorage {
    OssObjectStorage {
        region: "cn-hangzhou".to_owned(),
        transport,
        clock,
    }
}

async fn put(storage: &OssObjectStorage, body: &[u8]) -> Result<(), UploadWriteFailure> {
    let (access_key_id, access_key_secret) = credentials();
    let url = object_url();
    storage
        .put_object(
            PutObjectRequest {
                bucket: BUCKET,
                key: KEY,
                url: &url,
                content_type: "image/jpeg",
                body,
            },
            ObjectStorageCredentials {
                access_key_id: &access_key_id,
                access_key_secret: &access_key_secret,
            },
        )
        .await
}

#[tokio::test]
async fn put_sends_exactly_one_signed_request() {
    let transport = ScriptedTransport::new(vec![Ok(HttpResponse {
        status: 200,
        headers: Vec::new(),
    })]);
    // 2026-08-12T10:30:00Z 对应落档的 20260812T103000Z。
    let storage = build_storage(transport.clone(), Arc::new(FixedClock(fixed())));
    put(&storage, b"jpeg-bytes")
        .await
        .expect("a written object");
    let requests = transport.requests();
    assert_eq!(requests.len(), 1, "one port call issues one request");
    let request = &requests[0];
    assert_eq!(request.method, "PUT");
    assert_eq!(
        request.url,
        "https://my-bucket.oss-cn-hangzhou.aliyuncs.com/reference-media/2026-08-12/uuid-123.jpg"
    );
    assert_eq!(request.header("content-type"), Some("image/jpeg"));
    assert_eq!(request.header("x-oss-forbid-overwrite"), Some("true"));
    assert_eq!(
        request.header("x-oss-content-sha256"),
        Some("UNSIGNED-PAYLOAD")
    );
    assert_eq!(request.header("x-oss-date"), Some("20260812T103000Z"));
    assert_eq!(
        request.header("authorization"),
        Some(
            "OSS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260812/cn-hangzhou/oss/aliyun_v4_request,\
             Signature=c00baf659ad74992fd003d49e754bb137dfd35f235251529a398517dbce5e093"
        )
    );
    assert_eq!(request.body_len, 10);
}

#[tokio::test]
async fn head_sends_one_request_without_a_content_type() {
    let transport = ScriptedTransport::new(vec![Ok(HttpResponse {
        status: 200,
        headers: vec![
            ("content-length".to_owned(), "10".to_owned()),
            ("content-type".to_owned(), "image/jpeg".to_owned()),
        ],
    })]);
    let storage = build_storage(transport.clone(), Arc::new(FixedClock(fixed())));
    let (access_key_id, access_key_secret) = credentials();
    let url = object_url();
    let metadata = storage
        .head_object(
            HeadObjectRequest {
                bucket: BUCKET,
                key: KEY,
                url: &url,
            },
            ObjectStorageCredentials {
                access_key_id: &access_key_id,
                access_key_secret: &access_key_secret,
            },
        )
        .await
        .expect("metadata");
    assert_eq!(metadata.byte_length, 10);
    assert_eq!(metadata.content_type.as_deref(), Some("image/jpeg"));
    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "HEAD");
    assert_eq!(requests[0].header("content-type"), None);
    assert_eq!(requests[0].header("x-oss-date"), Some("20260812T103000Z"));
    assert!(requests[0].header("authorization").is_some());
}

#[tokio::test]
async fn head_without_a_content_length_fails_closed() {
    let transport = ScriptedTransport::new(vec![Ok(HttpResponse {
        status: 200,
        headers: vec![("content-type".to_owned(), "image/jpeg".to_owned())],
    })]);
    let storage = build_storage(transport, Arc::new(FixedClock(fixed())));
    let (access_key_id, access_key_secret) = credentials();
    let url = object_url();
    assert_eq!(
        storage
            .head_object(
                HeadObjectRequest {
                    bucket: BUCKET,
                    key: KEY,
                    url: &url,
                },
                ObjectStorageCredentials {
                    access_key_id: &access_key_id,
                    access_key_secret: &access_key_secret,
                },
            )
            .await,
        Err(UploadWriteFailure::Terminal)
    );
}

#[tokio::test]
async fn http_status_maps_to_the_designed_failure_classes() {
    // 终态：401 / 403 / 404 / 409 与其他 4xx 都不重试。
    for status in [400, 401, 403, 404, 409, 412] {
        let transport = ScriptedTransport::new(vec![Ok(HttpResponse {
            status,
            headers: Vec::new(),
        })]);
        let storage = build_storage(transport, Arc::new(FixedClock(fixed())));
        assert_eq!(
            put(&storage, b"jpeg-bytes").await,
            Err(UploadWriteFailure::Terminal),
            "status {status} is terminal"
        );
    }
    // 可重试：408 没有 Retry-After，429 与 5xx 采信有界整数秒。
    let transport = ScriptedTransport::new(vec![Ok(HttpResponse {
        status: 408,
        headers: Vec::new(),
    })]);
    let storage = build_storage(transport, Arc::new(FixedClock(fixed())));
    assert_eq!(
        put(&storage, b"jpeg-bytes").await,
        Err(UploadWriteFailure::Retryable {
            retry_after_seconds: None
        })
    );
    let transport = ScriptedTransport::new(vec![Ok(HttpResponse {
        status: 429,
        headers: vec![("retry-after".to_owned(), "2".to_owned())],
    })]);
    let storage = build_storage(transport, Arc::new(FixedClock(fixed())));
    assert_eq!(
        put(&storage, b"jpeg-bytes").await,
        Err(UploadWriteFailure::Retryable {
            retry_after_seconds: Some(2)
        })
    );
    let transport = ScriptedTransport::new(vec![Ok(HttpResponse {
        status: 503,
        headers: vec![("retry-after".to_owned(), "900".to_owned())],
    })]);
    let storage = build_storage(transport, Arc::new(FixedClock(fixed())));
    assert_eq!(
        put(&storage, b"jpeg-bytes").await,
        Err(UploadWriteFailure::Retryable {
            retry_after_seconds: None
        }),
        "an out-of-range Retry-After falls back to the fixed backoff"
    );
}

#[tokio::test]
async fn a_transport_failure_is_retryable() {
    let transport =
        ScriptedTransport::new(vec![Err(TransportError("connection reset".to_owned()))]);
    let storage = build_storage(transport, Arc::new(FixedClock(fixed())));
    assert_eq!(
        put(&storage, b"jpeg-bytes").await,
        Err(UploadWriteFailure::Retryable {
            retry_after_seconds: None
        })
    );
}

#[tokio::test]
async fn every_request_reads_the_clock_again() {
    let transport = ScriptedTransport::new(vec![
        Ok(HttpResponse {
            status: 200,
            headers: Vec::new(),
        }),
        Ok(HttpResponse {
            status: 200,
            headers: Vec::new(),
        }),
    ]);
    let storage = build_storage(
        transport.clone(),
        Arc::new(AdvancingClock(AtomicI64::new(0))),
    );
    put(&storage, b"jpeg-bytes").await.expect("the first put");
    put(&storage, b"jpeg-bytes").await.expect("the second put");
    let requests = transport.requests();
    assert_eq!(requests.len(), 2);
    assert_ne!(
        requests[0].header("x-oss-date"),
        requests[1].header("x-oss-date"),
        "the timestamp is taken per request, never cached"
    );
}
