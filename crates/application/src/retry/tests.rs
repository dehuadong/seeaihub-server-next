use super::*;

/// 退避是**指数**的，且封顶在单次上限。
///
/// 逐位断言而不是"大概递增"：重投的等待时间直接决定一个请求最坏占用对客同步窗口多久，
/// 差一档就是几百毫秒到几十秒的区别。
#[test]
fn the_backoff_doubles_from_the_base_and_stops_at_the_ceiling() {
    let policy = RetryPolicy {
        max_attempts: 10,
        backoff_base: Duration::from_millis(500),
    };
    assert_eq!(policy.backoff_for(1), Duration::from_millis(500));
    assert_eq!(policy.backoff_for(2), Duration::from_secs(1));
    assert_eq!(policy.backoff_for(3), Duration::from_secs(2));
    assert_eq!(policy.backoff_for(4), Duration::from_secs(4));
    // 封顶：再往上翻也停在这个数。
    assert_eq!(
        policy.backoff_for(9),
        Duration::from_secs(MAX_BACKOFF_SECONDS)
    );
    assert_eq!(
        policy.backoff_for(40),
        Duration::from_secs(MAX_BACKOFF_SECONDS),
        "指数不许因为次数大而溢出"
    );
    // 次数从 1 起算（1 是第一次执行），0 不该出现；出现了也按"第一次之后"退避，不 panic。
    assert_eq!(policy.backoff_for(0), Duration::from_millis(500));
}

/// 上限只回答"还有额度吗"：用掉的次数没到上限就还能再来一次。
#[test]
fn another_attempt_is_allowed_up_to_the_limit() {
    let policy = RetryPolicy {
        max_attempts: 3,
        backoff_base: Duration::from_millis(1),
    };
    assert!(policy.allows_another_attempt(1));
    assert!(policy.allows_another_attempt(2));
    assert!(
        !policy.allows_another_attempt(3),
        "第 3 次已经用掉上限，不许再有第 4 次"
    );
    // 上限为 1 就是关掉重投：第一次之后不再有额度。
    let off = RetryPolicy {
        max_attempts: 1,
        backoff_base: Duration::from_millis(1),
    };
    assert!(!off.allows_another_attempt(1));
}

/// 缺省值就是"最多三次上游调用、第一次重投等 1 秒"：它们必须在没有环境变量时是可用的。
#[test]
fn the_defaults_keep_retry_on_and_bounded() {
    let policy = RetryPolicy::default();
    assert_eq!(policy.max_attempts, DEFAULT_MAX_ATTEMPTS);
    assert_eq!(
        policy.backoff_base,
        Duration::from_millis(DEFAULT_BACKOFF_BASE_MS)
    );
    assert!(
        policy.max_attempts > 1,
        "缺省必须真的会重投，否则这条能力在缺省部署上等于没接"
    );
}
