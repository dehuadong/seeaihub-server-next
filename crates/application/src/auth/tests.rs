//! 身份基础件的用例：口令哈希与校验、邮箱归一化、口令下限、会话令牌与会话过期。
//!
//! 这些是**纯函数**，不需要数据库；真实读写与端到端行为由 `crates/persistence` 与 `apps/api` 的
//! 合同用例覆盖。

use super::*;

#[test]
fn a_password_can_be_verified_and_a_wrong_one_cannot() {
    let hash = hash_password("correct horse battery").expect("hashing must succeed");
    assert!(verify_password("correct horse battery", &hash));
    assert!(!verify_password("correct horse batteru", &hash));
    assert!(!verify_password("", &hash));
}

#[test]
fn the_same_password_hashes_differently_every_time() {
    // 加盐的意义就在这里：两条哈希不同 ⇒ 不能靠比对哈希猜出"这两个账号用了同一个口令"。
    let first = hash_password("correct horse battery").expect("hashing must succeed");
    let second = hash_password("correct horse battery").expect("hashing must succeed");
    assert_ne!(first, second);
    assert!(verify_password("correct horse battery", &first));
    assert!(verify_password("correct horse battery", &second));
}

#[test]
fn a_malformed_stored_hash_never_matches() {
    // 库里的数据坏了：按"不匹配"处理，不把解析错误回给调用方。
    assert!(!verify_password("anything", "not-a-phc-string"));
    assert!(!verify_password("anything", ""));
}

#[test]
fn the_dummy_hash_is_a_real_hash_and_never_matches() {
    // 它要顶替"邮箱不存在"那条路径的耗时，所以必须是一条**能走完校验**的真哈希；
    // 同时它对应的明文是随机的，任何输入都不该匹配。
    let dummy = dummy_password_hash();
    assert!(
        PasswordHash::new(dummy).is_ok(),
        "dummy must parse as a PHC hash"
    );
    assert!(!verify_dummy_password("correct horse battery"));
    assert!(!verify_dummy_password(""));
}

#[test]
fn emails_are_normalized_to_lowercase_and_trimmed() {
    assert_eq!(
        normalize_email("  Ops@Example.COM ").expect("valid"),
        "ops@example.com"
    );
    assert_eq!(normalize_email("a@b").expect("valid"), "a@b");
}

#[test]
fn an_email_without_two_sides_is_rejected() {
    for bad in ["", "no-at-sign", "@domain", "local@", "a@@b", "  "] {
        assert!(normalize_email(bad).is_err(), "{bad:?} must be rejected");
    }
}

#[test]
fn secrets_shorter_than_the_floor_are_rejected() {
    assert!(check_secret("12345678", "password").is_ok());
    assert!(check_secret("1234567", "password").is_err());
    // 门槛按**字符**数算，不是字节数：8 个汉字是 8 个字符、24 个字节，应当通过；
    // 4 个汉字是 4 个字符、12 个字节，按字节算会误判为够长，必须拒。
    assert!(check_secret("这是一个够长的口令", "password").is_ok());
    assert!(check_secret("短口令啊", "password").is_err());
}

#[test]
fn a_session_token_is_long_and_not_repeated() {
    let first = new_session_token();
    let second = new_session_token();
    assert_ne!(first, second);
    // 两个 UUIDv4 的十六进制：32 个十六进制字符 × 2。
    assert_eq!(first.len(), 64);
    assert!(first.chars().all(|character| character.is_ascii_hexdigit()));
}

#[test]
fn the_session_hash_is_stable_and_hides_the_token() {
    let token = new_session_token();
    let hash = session_token_hash(&token);
    assert_eq!(hash, session_token_hash(&token), "同一令牌摘要必须一致");
    assert_ne!(hash, token, "库里存的不能是明文");
    assert_eq!(hash.len(), 64, "SHA-256 的十六进制长度");
    assert_ne!(hash, session_token_hash(&new_session_token()));
}

#[test]
fn expiry_is_now_plus_ttl() {
    let now = Utc::now();
    let expires_at = session_expiry(now, ChronoDuration::minutes(30));
    assert_eq!(expires_at, now + ChronoDuration::minutes(30));
    assert!(expires_at > now);
}
