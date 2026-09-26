//! 邮箱 + 口令的身份与会话。
//!
//! **密码学与哈希在这里，读写库在仓储端口**：口令用 argon2id 加盐哈希后落库，会话令牌是 32 字节
//! 随机数、库里只存它的 SHA-256。明文（口令与会话令牌）从不落库、也不进日志——它们只在"登录"
//! 那一次响应里出现。
//!
//! 会话令牌用 32 字节随机数而不是签名令牌：吊销要**立刻**生效，而签名令牌只能等它自己过期。代价
//! 是每次请求多一次按摘要点读，与既有的 API Key 认证同一条路（那条也是每次读库判吊销）。

use argon2::{
    Argon2,
    password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash},
};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use std::sync::OnceLock;
use uuid::Uuid;

use crate::ApplicationError;

/// 口令与令牌的最小长度。短的直接拒，不留"以后再说"的口子。
pub const MIN_SECRET_LENGTH: usize = 8;

/// 一条**永远匹配不上**的 argon2id 哈希。
///
/// 它的用途只有一个：邮箱不存在时也算一遍校验，于是"这个邮箱有没有账号"不能从响应快慢上看出来。
/// 它是随机口令的哈希（明文当场丢弃），进程内算一次就够了——所以是惰性常量，而不是硬编码字符串：
/// 硬编码一个写错的哈希会让校验提前返回，"恒定耗时"就没了。
fn dummy_password_hash() -> &'static str {
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY.get_or_init(|| {
        hash_password(&new_session_token()).expect("hashing a random secret cannot fail")
    })
}

/// 登录失败。**邮箱不存在与口令不对回同一个错误**：分开说等于把"这个邮箱是不是我们的账号"
/// 告诉任何来试的人。
#[must_use]
pub fn invalid_credentials() -> ApplicationError {
    ApplicationError::InvalidParameter("email or password is incorrect".to_owned())
}

/// 一次登录校验的**恒定耗时对照**：邮箱不存在时也走一遍 argon2 校验。
#[must_use]
pub fn verify_dummy_password(password: &str) -> bool {
    verify_password(password, dummy_password_hash())
}

/// 邮箱里必须有一个 `@`，且两侧都不空。完整的 RFC 校验不在这里做——那是投递时才知道的事，
/// 这里只挡明显不成形状的输入。
pub fn normalize_email(email: &str) -> Result<String, ApplicationError> {
    let trimmed = email.trim().to_lowercase();
    let (local, domain) = trimmed.split_once('@').ok_or_else(|| {
        ApplicationError::Validation("email must look like name@domain".to_owned())
    })?;
    if local.is_empty() || domain.is_empty() || domain.contains('@') {
        return Err(ApplicationError::Validation(
            "email must look like name@domain".to_owned(),
        ));
    }
    Ok(trimmed)
}

/// 口令/令牌的长度检查。**不做复杂度规则**：长度之外的限制（大小写、符号）只会逼出难记的口令，
/// 而真正要挡的是"一个字母的密码"。
pub fn check_secret(secret: &str, what: &str) -> Result<(), ApplicationError> {
    if secret.chars().count() < MIN_SECRET_LENGTH {
        return Err(ApplicationError::Validation(format!(
            "{what} must be at least {MIN_SECRET_LENGTH} characters"
        )));
    }
    Ok(())
}

/// 口令 → argon2id 哈希（自动生成盐）。同一个口令两次哈希结果不同，这是对的。
pub fn hash_password(password: &str) -> Result<String, ApplicationError> {
    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|hash| hash.to_string())
        .map_err(|error| ApplicationError::Configuration(format!("cannot hash password: {error}")))
}

/// 校验口令是否匹配这条哈希。哈希本身不成形状时按"不匹配"处理——那是库里的数据坏了，
/// 不该把细节回给调用方。
#[must_use]
pub fn verify_password(password: &str, stored: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(stored) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

/// 新会话令牌的明文：两个 UUIDv4 拼接的十六进制，约 244 位随机。
///
/// 不需要更多：它的强度只用来抵挡"猜一条有效令牌"，而令牌在库里以 SHA-256 存放、按摘要点查，
/// 猜中一条相当于命中一条随机会话行。
#[must_use]
pub fn new_session_token() -> String {
    format!(
        "{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    )
}

/// 会话令牌在库里的样子：它的 SHA-256。明文只有调用方手里那一份。
#[must_use]
pub fn session_token_hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// 一次管理员登录的结果。令牌明文**只在这里**出现。
#[derive(Debug, Clone)]
pub struct AdminLogin {
    pub admin_id: Uuid,
    pub email: String,
    pub token: String,
    pub expires_at: DateTime<Utc>,
}

/// 一次对客登录的结果。
#[derive(Debug, Clone)]
pub struct CustomerLogin {
    pub customer_id: Uuid,
    pub account_id: Uuid,
    pub email: String,
    pub token: String,
    pub expires_at: DateTime<Utc>,
}

/// 把 `now + ttl` 算成过期时刻。TTL 是部署期配置，由调用方给。
#[must_use]
pub fn session_expiry(now: DateTime<Utc>, ttl: ChronoDuration) -> DateTime<Utc> {
    now + ttl
}
