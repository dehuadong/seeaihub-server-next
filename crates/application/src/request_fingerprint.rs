//! 请求指纹与幂等键摘要：带密钥摘要与稳定规范化（RFC 0017 §2，Spec 0005 §2、§4）。
//!
//! 两类摘要分开：幂等键只做**不可逆标识**，用稳定的 lookup 密钥，跨 API 副本一致、不随请求指纹
//! 密钥轮换；请求指纹覆盖端点、平台型号、已识别参数、参考图与 mask 内容及 n，用按版本轮换的密钥。
//! 两者都只从环境变量取密钥，缺失或非法一律配置错误——摘要必须可复现，不能用默认或随机值。

use std::collections::BTreeMap;
use std::fmt::{Debug, Formatter};

use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::ApplicationError;

/// 摘要密钥长度（字节），与 CUSTOMER_HISTORY_CURSOR_KEY 同一口径。
pub const FINGERPRINT_KEY_LEN: usize = 32;
/// 同一把密钥在不同用途下不产生可互相替换的摘要。
const FINGERPRINT_DOMAIN: &[u8] = b"seeai/request-fingerprint/v1";
const LOOKUP_DOMAIN: &[u8] = b"seeai/idempotency-lookup/v1";

/// 一次请求里参与指纹的**已识别**输入。
///
/// 未知字段在进入这里之前就被冻结合同过滤掉；图片是公网 URL 或 data URL 的**原值**，平台不下载、
/// 不解码，摘要只吃内容本身。n 是候选截断前的输出张数。
#[derive(Debug, Clone)]
pub struct RequestFingerprintInput<'a> {
    pub endpoint: &'a str,
    pub gateway_model: &'a str,
    pub parameters: &'a Value,
    pub reference_images: &'a [String],
    pub mask: Option<&'a str>,
    pub n: u64,
}

impl RequestFingerprintInput<'_> {
    /// 版本化请求指纹：对象键排序、数组保序、字符串原值；图片逐项喂入，不为图片构造整份
    /// canonical 大 JSON。同一份输入与同一把密钥必得同一摘要。
    pub fn fingerprint(&self, key: &[u8]) -> Result<String, ApplicationError> {
        let mut canonical = self.parameters.clone();
        crate::canonicalize_json(&mut canonical);
        let encoded = serde_json::to_vec(&canonical)
            .map_err(|error| ApplicationError::Validation(error.to_string()))?;
        let count = self.reference_images.len().to_string();
        let n = self.n.to_string();
        let mut parts: Vec<&[u8]> = Vec::with_capacity(6 + self.reference_images.len());
        parts.push(self.endpoint.as_bytes());
        parts.push(self.gateway_model.as_bytes());
        parts.push(encoded.as_slice());
        parts.push(count.as_bytes());
        for image in self.reference_images {
            parts.push(image.as_bytes());
        }
        match self.mask {
            Some(mask) => {
                parts.push(b"1");
                parts.push(mask.as_bytes());
            }
            None => parts.push(b"0"),
        }
        parts.push(n.as_bytes());
        Ok(digest_hex(key, FINGERPRINT_DOMAIN, &parts))
    }
}

/// 幂等键的不可逆标识：用**稳定 lookup 密钥**，所有 API 副本一致，不随请求指纹密钥轮换。
/// 明文键不进新协议记录（Spec 0005 §2）。
#[must_use]
pub fn idempotency_key_digest(key: &[u8], idempotency_key: &str) -> String {
    digest_hex(key, LOOKUP_DOMAIN, &[idempotency_key.as_bytes()])
}

/// 请求指纹与幂等键的密钥配置：一份稳定 lookup 密钥加一组按版本轮换的请求指纹密钥。
///
/// 旧版本密钥保留在配置里直到相应记录退出保证范围：查到旧记录后用记录的版本重算才能比较
/// （RFC 0017 §2）。密钥只从环境变量读，不得与 Provider 凭证混用。
#[derive(Clone)]
pub struct FingerprintKeys {
    lookup_key: Vec<u8>,
    lookup_key_version: i16,
    request_keys: BTreeMap<i16, Vec<u8>>,
    current_request_key_version: i16,
}

impl Debug for FingerprintKeys {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("FingerprintKeys([REDACTED])")
    }
}

impl FingerprintKeys {
    /// 构造并校验：两把密钥都必须是 FINGERPRINT_KEY_LEN 字节，版本必须为正，当前版本必须在表里。
    pub fn new(
        lookup_key: Vec<u8>,
        lookup_key_version: i16,
        request_keys: BTreeMap<i16, Vec<u8>>,
        current_request_key_version: i16,
    ) -> Result<Self, ApplicationError> {
        if lookup_key.len() != FINGERPRINT_KEY_LEN {
            return Err(ApplicationError::Configuration(format!(
                "the idempotency lookup key must be {FINGERPRINT_KEY_LEN} bytes"
            )));
        }
        if lookup_key_version < 1 || current_request_key_version < 1 {
            return Err(ApplicationError::Configuration(
                "fingerprint key versions must be positive".to_owned(),
            ));
        }
        if request_keys.is_empty() {
            return Err(ApplicationError::Configuration(
                "at least one request fingerprint key is required".to_owned(),
            ));
        }
        for (version, key) in &request_keys {
            if *version < 1 || key.len() != FINGERPRINT_KEY_LEN {
                return Err(ApplicationError::Configuration(format!(
                    "request fingerprint key v{version} must be {FINGERPRINT_KEY_LEN} bytes"
                )));
            }
        }
        if !request_keys.contains_key(&current_request_key_version) {
            return Err(ApplicationError::Configuration(format!(
                "REQUEST_FINGERPRINT_KEY_V{current_request_key_version} is missing"
            )));
        }
        Ok(Self {
            lookup_key,
            lookup_key_version,
            request_keys,
            current_request_key_version,
        })
    }

    /// 从环境变量读：IDEMPOTENCY_LOOKUP_KEY 与 REQUEST_FINGERPRINT_KEY_V<version> 是 32 字节的
    /// base64；IDEMPOTENCY_LOOKUP_KEY_VERSION 缺省 1，REQUEST_FINGERPRINT_KEY_VERSION 缺省 1，
    /// 且 1..=当前版本每一档都必须给出密钥。缺失或非法报配置错误，不让进程起来。
    pub fn from_env() -> Result<Self, ApplicationError> {
        let lookup_key = read_key("IDEMPOTENCY_LOOKUP_KEY")?;
        let lookup_key_version = read_version("IDEMPOTENCY_LOOKUP_KEY_VERSION", 1)?;
        let current = read_version("REQUEST_FINGERPRINT_KEY_VERSION", 1)?;
        let mut request_keys = BTreeMap::new();
        for version in 1..=current {
            let name = format!("REQUEST_FINGERPRINT_KEY_V{version}");
            request_keys.insert(version, read_key(&name)?);
        }
        Self::new(lookup_key, lookup_key_version, request_keys, current)
    }

    #[must_use]
    pub fn lookup_key(&self) -> &[u8] {
        &self.lookup_key
    }

    #[must_use]
    pub fn lookup_key_version(&self) -> i16 {
        self.lookup_key_version
    }

    #[must_use]
    pub fn current_request_key_version(&self) -> i16 {
        self.current_request_key_version
    }

    #[must_use]
    pub fn current_request_key(&self) -> &[u8] {
        self.request_keys
            .get(&self.current_request_key_version)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// 某个版本的请求指纹密钥：旧记录用它的版本重算时取这里。
    #[must_use]
    pub fn request_key(&self, version: i16) -> Option<&[u8]> {
        self.request_keys.get(&version).map(Vec::as_slice)
    }

    pub fn request_fingerprint(
        &self,
        version: i16,
        input: &RequestFingerprintInput<'_>,
    ) -> Result<Option<String>, ApplicationError> {
        match self.request_key(version) {
            Some(key) => input.fingerprint(key).map(Some),
            None => Ok(None),
        }
    }

    #[must_use]
    pub fn idempotency_key_digest(&self, idempotency_key: &str) -> String {
        idempotency_key_digest(&self.lookup_key, idempotency_key)
    }
}

fn read_key(name: &str) -> Result<Vec<u8>, ApplicationError> {
    let raw = match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => value,
        _ => {
            return Err(ApplicationError::Configuration(format!(
                "{name} must be set"
            )));
        }
    };
    let bytes = STANDARD
        .decode(raw.trim())
        .map_err(|_| ApplicationError::Configuration(format!("{name} must be base64")))?;
    if bytes.len() != FINGERPRINT_KEY_LEN {
        return Err(ApplicationError::Configuration(format!(
            "{name} must decode to {FINGERPRINT_KEY_LEN} bytes (got {})",
            bytes.len()
        )));
    }
    Ok(bytes)
}

fn read_version(name: &str, default: i16) -> Result<i16, ApplicationError> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => value
            .trim()
            .parse::<i16>()
            .map_err(|_| ApplicationError::Configuration(format!("{name} must be an integer"))),
        _ => Ok(default),
    }
}

fn digest_hex(key: &[u8], domain: &[u8], parts: &[&[u8]]) -> String {
    let mut all: Vec<&[u8]> = Vec::with_capacity(parts.len() + 1);
    all.push(domain);
    all.extend_from_slice(parts);
    hex::encode(hmac_sha256(key, &all))
}

/// HMAC-SHA256，每个片段带 8 字节长度前缀——拼接不会产生跨字段的歧义。
fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut normalized = [0u8; BLOCK];
    if key.len() > BLOCK {
        let digest = Sha256::digest(key);
        normalized[..digest.len()].copy_from_slice(&digest);
    } else {
        normalized[..key.len()].copy_from_slice(key);
    }
    let mut inner_pad = [0x36u8; BLOCK];
    let mut outer_pad = [0x5cu8; BLOCK];
    for (index, byte) in normalized.iter().enumerate() {
        inner_pad[index] ^= *byte;
        outer_pad[index] ^= *byte;
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    for part in parts {
        inner.update(u64::try_from(part.len()).unwrap_or(u64::MAX).to_be_bytes());
        inner.update(part);
    }
    let inner_digest = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    let outer_digest = outer.finalize();
    let mut result = [0u8; 32];
    result.copy_from_slice(&outer_digest);
    result
}

#[cfg(test)]
mod tests;
