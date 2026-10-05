use super::*;

/// 落档金标准 §1 的固定输入。
const AK: &str = "AKIDEXAMPLE";
const SK: &str = "SKEXAMPLE123456";
const REGION: &str = "cn-hangzhou";
const BUCKET: &str = "my-bucket";
const OBJECT: &str = "reference-media/2026-08-12/uuid-123.jpg";
const CANONICAL_URI: &str = "/my-bucket/reference-media/2026-08-12/uuid-123.jpg";
const TIMESTAMP: &str = "20260812T103000Z";
const DATE: &str = "20260812";

fn headers(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect()
}

/// 给显式头补上每个请求都会发的默认签名头：`x-oss-date` 与 `x-oss-content-sha256`。
fn with_defaults(mut pairs: Vec<(String, String)>) -> Vec<(String, String)> {
    pairs.push(("x-oss-date".to_owned(), TIMESTAMP.to_owned()));
    pairs.push((
        "x-oss-content-sha256".to_owned(),
        UNSIGNED_PAYLOAD.to_owned(),
    ));
    pairs
}

fn request<'a>(
    method: &'a str,
    canonical_query: &'a str,
    signed: &'a [(String, String)],
    additional: &'a [String],
) -> SignedRequest<'a> {
    SignedRequest {
        method,
        canonical_uri: CANONICAL_URI,
        canonical_query,
        headers: signed,
        additional_headers: additional,
        hashed_payload: UNSIGNED_PAYLOAD,
        timestamp: TIMESTAMP,
        date: DATE,
        region: REGION,
        access_key_id: AK,
        access_key_secret: SK,
    }
}

#[test]
fn header_mode_vectors_match_the_landed_golden() {
    // 落档 §2 第 1 行：canonical query 是禁覆盖的 query 形态。
    let query_form = with_defaults(headers(&[("content-type", "image/jpeg")]));
    assert_eq!(
        signature(&request(
            "PUT",
            "x-oss-forbid-overwrite=true",
            &query_form,
            &[]
        )),
        "b3a10a1219b3e7a8dc32be76c288025027602d8227a591db3317f9be10cc8b14"
    );
    // 落档 §2 第 2 行：HEAD，无 query、无显式头。
    let none = with_defaults(headers(&[]));
    assert_eq!(
        signature(&request("HEAD", "", &none, &[])),
        "90cf7b7b9c6fc4062c5bcb51b2526cf11c24a7e6ded03d07d3532863438fc06a"
    );
    // 落档 §2 第 3 行：GET header 模式。
    let get = with_defaults(headers(&[("content-type", "application/octet-stream")]));
    assert_eq!(
        signature(&request("GET", "", &get, &[])),
        "205d5ebe2322cc91d4657f9947681f95c13af8980e6315fe957bcbf017c486cc"
    );
    // 落档 §2 第 4 行：本设计真正要发的 PUT，禁覆盖放请求头。
    let production = with_defaults(headers(&[
        ("content-type", "image/jpeg"),
        ("x-oss-forbid-overwrite", "true"),
    ]));
    assert_eq!(
        signature(&request("PUT", "", &production, &[])),
        "c00baf659ad74992fd003d49e754bb137dfd35f235251529a398517dbce5e093"
    );
}

#[test]
fn additional_signed_headers_match_the_golden() {
    // 这一组的 `x-oss-date` 是它自己的固定输入，不是 §1 的那一个。
    let mut signed = headers(&[
        ("content-type", "text/plain"),
        ("content-md5", "ICy5YqxZB1uWSwcVLSNLcA=="),
        ("content-length", "3"),
        ("content-disposition", "attachment"),
    ]);
    signed.push(("x-oss-date".to_owned(), "20250411T064124Z".to_owned()));
    signed.push((
        "x-oss-content-sha256".to_owned(),
        UNSIGNED_PAYLOAD.to_owned(),
    ));
    let additional = vec![
        "content-disposition".to_owned(),
        "content-length".to_owned(),
    ];
    let request = SignedRequest {
        method: "PUT",
        canonical_uri: "/examplebucket/exampleobject",
        canonical_query: "",
        headers: &signed,
        additional_headers: &additional,
        hashed_payload: UNSIGNED_PAYLOAD,
        timestamp: "20250411T064124Z",
        date: "20250411",
        region: REGION,
        access_key_id: "LTAIEXAMPLE",
        access_key_secret: "yourAccessKeySecret",
    };
    assert_eq!(
        additional_header_list(&additional),
        "content-disposition;content-length"
    );
    assert_eq!(
        signature(&request),
        "d3694c2dfc5371ee6acd35e88c4871ac95a7ba01d3a2f476768fe61218590097"
    );
}

#[test]
fn canonical_uri_always_carries_the_bucket_and_host_is_not_signed() {
    let with_host = with_defaults(headers(&[(
        "host",
        "my-bucket.oss-cn-hangzhou.aliyuncs.com",
    )]));
    let request = request("HEAD", "", &with_host, &[]);
    let canonical = canonical_request(&request);
    // host 不进 canonical headers：那条请求头在签名材料里根本不出现。
    assert!(!canonical.contains("host:"));
    assert!(canonical.contains("x-oss-content-sha256:UNSIGNED-PAYLOAD"));
    assert!(canonical.contains("x-oss-date:20260812T103000Z"));
    // canonical URI 恒为 /{bucket}/{key}，与寻址形态无关。
    assert!(canonical.contains(CANONICAL_URI));
    assert_eq!(request.canonical_uri, CANONICAL_URI);
}

#[test]
fn canonical_headers_are_lowercase_and_sorted() {
    let signed = headers(&[
        ("X-OSS-Forbid-Overwrite", "true"),
        ("Content-Type", "image/jpeg"),
    ]);
    let canonical = canonical_headers(&signed, &[]);
    assert_eq!(
        canonical,
        "content-type:image/jpeg\nx-oss-forbid-overwrite:true\n"
    );
}

#[test]
fn derived_key_and_string_to_sign_are_stable() {
    // 派生密钥与 string-to-sign 的独立锚（Node crypto 复算）。
    assert_eq!(
        hex::encode(signing_key(SK, DATE, REGION)),
        "96eb1be8fc20ce1adaa70c67213eef4112b2f741f7d3604f844b03b38b1a7ff4"
    );
    let production = with_defaults(headers(&[
        ("content-type", "image/jpeg"),
        ("x-oss-forbid-overwrite", "true"),
    ]));
    let request = request("PUT", "", &production, &[]);
    assert_eq!(
        canonical_request(&request),
        "PUT\n/my-bucket/reference-media/2026-08-12/uuid-123.jpg\n\ncontent-type:image/jpeg\nx-oss-content-sha256:UNSIGNED-PAYLOAD\nx-oss-date:20260812T103000Z\nx-oss-forbid-overwrite:true\n\n\nUNSIGNED-PAYLOAD"
    );
    assert_eq!(
        string_to_sign(&request),
        "OSS4-HMAC-SHA256\n20260812T103000Z\n20260812/cn-hangzhou/oss/aliyun_v4_request\n762ccdb078be82990c801730fa399ca5f672f43ec477709be8cd1723dacf23ec"
    );
}

/// 落档 §3 的预签名 GET：query 模式的参数编码与签名串只在测试里构造，不是生产能力。
#[test]
fn presigned_get_matches_the_golden() {
    let (canonical_uri, canonical_query) =
        presigned_query(OBJECT, TIMESTAMP, DATE, REGION, AK, 86400, BUCKET);
    assert_eq!(
        canonical_uri,
        "/my-bucket/reference-media/2026-08-12/uuid-123.jpg"
    );
    assert_eq!(
        canonical_query,
        "x-oss-credential=AKIDEXAMPLE%2F20260812%2Fcn-hangzhou%2Foss%2Faliyun_v4_request&x-oss-date=20260812T103000Z&x-oss-expires=86400&x-oss-signature-version=OSS4-HMAC-SHA256"
    );
    assert_eq!(
        presigned_signature(
            &canonical_uri,
            &canonical_query,
            TIMESTAMP,
            DATE,
            REGION,
            AK,
            SK
        ),
        "9b7e5830a4e4226af611d9a5b05e3abddcbf12ef17e7ad0e2fadd9731ecb9b2c"
    );
    // 特殊对象键：路径段按 percent-encoding 编码，签名随之改变。
    let (canonical_uri, canonical_query) = presigned_query(
        "reference-media/2026-08-12/uuid 图.jpg",
        TIMESTAMP,
        DATE,
        REGION,
        AK,
        86400,
        BUCKET,
    );
    assert_eq!(
        canonical_uri,
        "/my-bucket/reference-media/2026-08-12/uuid%20%E5%9B%BE.jpg"
    );
    assert_eq!(
        presigned_signature(
            &canonical_uri,
            &canonical_query,
            TIMESTAMP,
            DATE,
            REGION,
            AK,
            SK
        ),
        "c34b0269ce103db552b72b14c9c54949d34028023532cee8373933544a35ea95"
    );
}

/// 预签名 GET 的 canonical URI 与 canonical query（测试专用）。
fn presigned_query(
    object: &str,
    timestamp: &str,
    date: &str,
    region: &str,
    access_key_id: &str,
    expires: u64,
    bucket: &str,
) -> (String, String) {
    let encoded_path = object
        .split('/')
        .map(percent_encode)
        .collect::<Vec<_>>()
        .join("/");
    let canonical_uri = format!("/{bucket}/{encoded_path}");
    let mut params = [
        (
            "x-oss-credential".to_owned(),
            format!("{access_key_id}/{date}/{region}/oss/aliyun_v4_request"),
        ),
        ("x-oss-date".to_owned(), timestamp.to_owned()),
        ("x-oss-expires".to_owned(), expires.to_string()),
        (
            "x-oss-signature-version".to_owned(),
            "OSS4-HMAC-SHA256".to_owned(),
        ),
    ];
    params.sort_by(|left, right| left.0.cmp(&right.0));
    let canonical_query = params
        .iter()
        .map(|(name, value)| format!("{}={}", percent_encode(name), percent_encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    (canonical_uri, canonical_query)
}

#[allow(clippy::too_many_arguments)]
fn presigned_signature(
    canonical_uri: &str,
    canonical_query: &str,
    timestamp: &str,
    date: &str,
    region: &str,
    access_key_id: &str,
    access_key_secret: &str,
) -> String {
    signature(&SignedRequest {
        method: "GET",
        canonical_uri,
        canonical_query,
        headers: &[],
        additional_headers: &[],
        hashed_payload: UNSIGNED_PAYLOAD,
        timestamp,
        date,
        region,
        access_key_id,
        access_key_secret,
    })
}

/// `quote(s, safe)` 口径：未保留字符不编码，其余按大写十六进制。
fn percent_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.as_bytes() {
        let character = char::from(*byte);
        if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | '~') {
            encoded.push(character);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}
