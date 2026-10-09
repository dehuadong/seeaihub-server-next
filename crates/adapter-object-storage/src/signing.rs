//! 阿里云 OSS V4 签名：header 模式的请求签名。
//!
//! 派生链是 `HMAC("aliyun_v4" + SK, date) → region → "oss" → "aliyun_v4_request"`；canonical
//! request 是 Verb、canonical URI、canonical query、canonical headers、附加签名头列表与
//! `UNSIGNED-PAYLOAD`。默认签名头集合是所有 `x-oss-*`、`content-type` 与
//! `content-md5`，其余显式头进附加签名头；`host` 不进 canonical headers。规则与对拍
//! 向量的唯一属主是[对象存储上传设计](../../../.agents/notes/implemented/platform/2026-10-04-reference-image-upload.md)，
//! 固定输入与期望值落档在 [`out-reference/oss-v4-signing-golden.md`](../../../out-reference/oss-v4-signing-golden.md)。

use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// 签名的 payload 占位：平台不把对象字节纳入签名材料。
pub(crate) const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";

/// 一次 header 模式签名的全部输入。
///
/// `headers` 是这次请求真正会发出的头（名字小写、值原样），`additional_headers` 是其中
/// **非默认**签名头的名字；`host` 即便出现在 `headers` 里也不参与签名。
pub(crate) struct SignedRequest<'a> {
    pub method: &'a str,
    /// 恒为 `/{bucket}/{key}`，与寻址形态无关。
    pub canonical_uri: &'a str,
    pub canonical_query: &'a str,
    pub headers: &'a [(String, String)],
    pub additional_headers: &'a [String],
    pub hashed_payload: &'a str,
    /// `x-oss-date` 的完整取值（`YYYYMMDDTHHMMSSZ`）。
    pub timestamp: &'a str,
    /// scope 日期（`YYYYMMDD`）。
    pub date: &'a str,
    pub region: &'a str,
    pub access_key_id: &'a str,
    pub access_key_secret: &'a str,
}

/// 默认签名头：所有 `x-oss-*`、`content-type` 与 `content-md5`。
pub(crate) fn is_default_signed_header(name: &str) -> bool {
    name.starts_with("x-oss-") || name == "content-type" || name == "content-md5"
}

/// 对象地址的 canonical URI：`/{bucket}/{key}`；路径段按 `quote(s, safe)` 编码，`/` 保留为分隔符。
pub(crate) fn canonical_uri(bucket: &str, key: &str) -> String {
    let encoded_key = key
        .split('/')
        .map(percent_encode)
        .collect::<Vec<_>>()
        .join("/");
    format!("/{}/{}", percent_encode(bucket), encoded_key)
}

/// `quote(s, safe)` 口径：未保留字符不编码，其余按大写十六进制。
pub(crate) fn percent_encode(value: &str) -> String {
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

/// 派生签名密钥：`HMAC("aliyun_v4" + SK, date) → region → "oss" → "aliyun_v4_request"`。
pub(crate) fn signing_key(secret: &str, date: &str, region: &str) -> [u8; 32] {
    let k_date = hmac_sha256(format!("aliyun_v4{secret}").as_bytes(), date.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, b"oss");
    hmac_sha256(&k_service, b"aliyun_v4_request")
}

/// canonical request：Verb、canonical URI、canonical query、canonical headers、附加签名头列表与
/// hashed payload，逐行拼装。
pub(crate) fn canonical_request(request: &SignedRequest<'_>) -> String {
    let canonical_headers = canonical_headers(request.headers, request.additional_headers);
    let additional = additional_header_list(request.additional_headers);
    format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        request.method,
        request.canonical_uri,
        request.canonical_query,
        canonical_headers,
        additional,
        request.hashed_payload
    )
}

/// string-to-sign：算法、`x-oss-date`、scope 与 canonical request 的 SHA-256 十六进制。
pub(crate) fn string_to_sign(request: &SignedRequest<'_>) -> String {
    let scope = format!("{}/{}/oss/aliyun_v4_request", request.date, request.region);
    format!(
        "OSS4-HMAC-SHA256\n{}\n{}\n{}",
        request.timestamp,
        scope,
        hex::encode(Sha256::digest(canonical_request(request).as_bytes()))
    )
}

/// 签名值：派生密钥对 string-to-sign 做 HMAC-SHA256 的十六进制。
pub(crate) fn signature(request: &SignedRequest<'_>) -> String {
    let key = signing_key(request.access_key_secret, request.date, request.region);
    hex::encode(hmac_sha256(&key, string_to_sign(request).as_bytes()))
}

/// `Authorization` 头的完整取值。
pub(crate) fn authorization(request: &SignedRequest<'_>) -> String {
    format!(
        "OSS4-HMAC-SHA256 Credential={}/{}/{}/oss/aliyun_v4_request,Signature={}",
        request.access_key_id,
        request.date,
        request.region,
        signature(request)
    )
}

/// canonical headers：参与签名的头按名字小写、字典序排列，每行是 `name:value` 加一个换行。
fn canonical_headers(headers: &[(String, String)], additional: &[String]) -> String {
    let mut signed: Vec<(String, String)> = headers
        .iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value.clone()))
        .filter(|(name, _)| {
            name != "host"
                && (is_default_signed_header(name) || additional.iter().any(|extra| extra == name))
        })
        .collect();
    signed.sort_by(|left, right| left.0.cmp(&right.0));
    signed
        .into_iter()
        .map(|(name, value)| format!("{name}:{value}\n"))
        .collect()
}

/// 附加签名头列表：小写名字按字典序、分号分隔。
fn additional_header_list(additional: &[String]) -> String {
    let mut names: Vec<String> = additional
        .iter()
        .map(|name| name.to_ascii_lowercase())
        .collect();
    names.sort();
    names.join(";")
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    let output = mac.finalize().into_bytes();
    let mut bytes = [0_u8; 32];
    bytes.copy_from_slice(&output);
    bytes
}

#[cfg(test)]
mod tests;
