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
fn an_unknown_account_still_pays_for_one_password_check() {
    // 这条是 Spec 要的那个形式：**断言"账号不存在时也走了一次校验"**，而不是只比对两条错误响应相同
    // ——后者把对照校验整个删掉也照样成立，等于没验。
    //
    // 它证明的是：`verify_login_secret` 在"没有存储哈希"这条路上确实调用了对照校验。
    // 它**不**证明 `login_admin` 一定调了 `verify_login_secret`——那由类型上无路可绕（该方法体内没有
    // 别的分支能跳过它）与端到端用例 `a_wrong_password_and_an_unknown_email_are_indistinguishable`
    // 各承担一半。
    //
    // 计数是进程级的，所以要按**调用前后之差**断言；本模块的用例不并发（cargo test 会把同一模块的
    // 用例分摊到多个线程，所以这里只断言"至少加了一次"，不断言恰好一次）。
    let before = dummy_verifications();
    assert!(!verify_login_secret(None, "any-password"));
    assert!(
        dummy_verifications() > before,
        "账号不存在时必须也走一遍对照校验"
    );

    // 反方向：**有**存储哈希时不走对照校验（走的是那条真哈希）。
    let real = hash_password("correct horse battery").expect("hashing must succeed");
    let before = dummy_verifications();
    assert!(verify_login_secret(Some(&real), "correct horse battery"));
    assert!(!verify_login_secret(Some(&real), "wrong"));
    assert_eq!(dummy_verifications(), before, "有真哈希时不该多走对照校验");
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

#[test]
fn a_session_is_valid_until_its_expiry_instant() {
    // Spec 要的那个形式：**替身时钟**覆盖判定，而不是只能在真库里把 `expires_at` 改到过去。
    // 判定是纯函数，所以任意 `now` 都能构造——过期、未过期、以及"正好到点"这个边界。
    let expiry = Utc::now();

    assert!(
        session_is_valid(expiry, expiry - ChronoDuration::seconds(1)),
        "到点之前仍然有效"
    );
    // **正好到点算已过期**：有效期是半开区间 `[签发, 过期)`，与账单口径同一个约定。
    assert!(!session_is_valid(expiry, expiry), "到点这一刻必须算已过期");
    assert!(
        !session_is_valid(expiry, expiry + ChronoDuration::seconds(1)),
        "过点之后必须失效"
    );

    // 签发出来的会话在它的整个有效期里有效、T​TL 走完之后失效——把两件事接起来看一次。
    let issued_at = Utc::now();
    let ttl = ChronoDuration::minutes(30);
    let expires_at = session_expiry(issued_at, ttl);
    assert!(session_is_valid(
        expires_at,
        issued_at + ChronoDuration::minutes(29)
    ));
    assert!(!session_is_valid(
        expires_at,
        issued_at + ChronoDuration::minutes(31)
    ));
}
