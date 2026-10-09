//! `crates/persistence` 的进程内用例：不需要真实数据库的那一部分。

use super::{DEFAULT_DATABASE_MAX_CONNECTIONS, parse_max_connections};

/// 没配、配空串与配空白都取缺省：三种写法表达的是同一件事——这一项没给。
#[test]
fn an_absent_pool_limit_takes_the_default() {
    for raw in [None, Some(""), Some("   ")] {
        assert_eq!(
            parse_max_connections(raw).expect("an absent value is not an error"),
            DEFAULT_DATABASE_MAX_CONNECTIONS,
            "raw = {raw:?}"
        );
    }
}

/// 配了就用它，首尾空白不算内容。
#[test]
fn a_configured_pool_limit_is_used_as_given() {
    for (raw, expected) in [("32", 32), (" 32 ", 32), ("1", 1)] {
        assert_eq!(
            parse_max_connections(Some(raw)).expect("a positive integer is accepted"),
            expected,
            "raw = {raw:?}"
        );
    }
}

/// `0` 与非法值都拒绝启动，且**不**退回缺省：前者会让请求排到获取超时，后者说明部署写错了。
///
/// 两者的报错都点名变量，运维据此能直接改。
#[test]
fn a_zero_or_unparsable_pool_limit_is_a_configuration_error() {
    for raw in ["0", "-1", "ten", "4294967296"] {
        let error =
            parse_max_connections(Some(raw)).expect_err(&format!("{raw} must not be accepted"));
        let message = error.to_string();
        assert!(
            message.contains("DATABASE_MAX_CONNECTIONS"),
            "{raw} 的报错要点名变量：{message}"
        );
    }
}
