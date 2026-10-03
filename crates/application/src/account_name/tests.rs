//! 账户名称规则与生成（Spec `0003` N2/N3）的用例。
//!
//! 这里只验规则本身；“省略走生成、显式 `null` 报错、客户只能改自己的账户”由 API 合同用例在真库上验。

use super::*;

/// 首选生成名称＝候选序列的第一个（生产路径只用候选序列，这里给用例一个好读的名字）。
fn preferred_generated_name(account_id: AccountId, email: Option<&str>) -> String {
    generated_account_name_attempts(account_id, email)
        .into_iter()
        .next()
        .expect("候选序列不为空")
}
use uuid::Uuid;

fn account_id(bits: u128) -> AccountId {
    AccountId(Uuid::from_u128(bits))
}

/// 一个 id 形如 `00000000-0000-4000-8000-...` 的账户，取前 4/8 位便于断言。
const FIXED: u128 = 0x3f9a_2b1c_0000_4000_8000_0000_0000_0001;

#[test]
fn a_name_is_trimmed_and_measured_in_unicode_scalar_values() {
    assert_eq!(
        normalize_account_name("  星尘工作室  ").expect("合法名称"),
        "星尘工作室"
    );
    // 内部连续空格原样保留。
    assert_eq!(
        normalize_account_name("星尘  工作室").expect("合法名称"),
        "星尘  工作室"
    );
    // 100 个字符可以，101 个不行；emoji 组合按 scalar value 计数。
    let hundred = "a".repeat(ACCOUNT_NAME_MAX_CHARS);
    assert!(normalize_account_name(&hundred).is_ok());
    let over = "a".repeat(ACCOUNT_NAME_MAX_CHARS + 1);
    assert!(normalize_account_name(&over).is_err());
    assert!(normalize_account_name("👨‍👩‍👧").is_ok());
}

#[test]
fn rejected_characters_are_control_and_format_but_emoji_joiners_pass() {
    for rejected in ["\u{0009}", "\u{000A}", "\u{007F}", "\u{202E}"] {
        assert!(
            normalize_account_name(&format!("星尘{rejected}工作室")).is_err(),
            "{rejected:?} 应当被拒"
        );
    }
    // 零宽连接符与非连接符是 emoji 组合的一部分，必须放行。
    assert!(normalize_account_name("星尘\u{200D}工作室").is_ok());
    assert!(normalize_account_name("星尘\u{200C}工作室").is_ok());
    // 空白与空串一律拒。
    assert!(normalize_account_name("   ").is_err());
    assert!(normalize_account_name("").is_err());
}

#[test]
fn a_generated_name_uses_the_email_local_part_and_an_id_fragment() {
    let generated = preferred_generated_name(account_id(FIXED), Some("zhangsan@example.com"));
    assert_eq!(generated, "zhangsan_3f9a");
    assert!(normalize_account_name(&generated).is_ok());
}

#[test]
fn a_generated_name_falls_back_to_the_account_id_without_a_usable_email() {
    let from_nothing = preferred_generated_name(account_id(FIXED), None);
    assert_eq!(from_nothing, "账户_3f9a2b1c");
    // 本地部分为空、含被拒字符、或邮箱没有 `@`：都退回 id 形式。
    for unusable in ["@example.com", "zhang\tsan@example.com", "no-at-sign"] {
        assert_eq!(
            preferred_generated_name(account_id(FIXED), Some(unusable)),
            from_nothing,
            "{unusable} 不该被用作生成来源"
        );
    }
}

#[test]
fn the_candidate_sequence_grows_the_id_fragment_and_ends_with_the_account_form() {
    let attempts = generated_account_name_attempts(account_id(FIXED), Some("zhangsan@example.com"));
    assert_eq!(attempts[0], "zhangsan_3f9a", "首选仍是短片段");
    assert_eq!(attempts[1], "zhangsan_3f9a2b", "撞名后加长");
    assert_eq!(attempts[2], "zhangsan_3f9a2b1c");
    assert!(
        attempts.contains(&"账户_3f9a2b1c".to_owned()),
        "兜底形式也在候选里"
    );
    // 每一项都是合法名称，且列表里没有重复。
    let mut seen = Vec::new();
    for candidate in &attempts {
        assert!(normalize_account_name(candidate).is_ok(), "{candidate}");
        assert!(!seen.contains(candidate), "候选不该重复：{attempts:?}");
        seen.push(candidate.clone());
    }
    // 没有可用邮箱时，从 8 位片段起步（与 N3 的首选形式一致）。
    let without_email = generated_account_name_attempts(account_id(FIXED), None);
    assert_eq!(without_email[0], "账户_3f9a2b1c");
    assert!(without_email[1].len() > without_email[0].len());
}

#[test]
fn an_over_long_local_part_is_truncated_before_the_id_fragment() {
    let long = format!("{}@example.com", "a".repeat(200));
    let generated = preferred_generated_name(account_id(FIXED), Some(&long));
    assert_eq!(generated, format!("{}_3f9a", "a".repeat(95)));
    // 95 个字符的本地部分 + `_` + 4 位片段，正好顶到上限。
    assert_eq!(generated.chars().count(), ACCOUNT_NAME_MAX_CHARS);
}

#[test]
fn a_generated_name_keeps_whitespace_out_of_its_edges() {
    // 本地部分带首尾空白：先按 N2 的规则收边，再做后缀拼接。
    let generated = preferred_generated_name(account_id(FIXED), Some("  zhangsan  @example.com"));
    assert_eq!(generated, "zhangsan_3f9a");
}
