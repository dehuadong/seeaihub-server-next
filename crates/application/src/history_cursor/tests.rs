use chrono::{TimeZone as _, Utc};
use seeai_domain::AccountId;
use uuid::Uuid;

use super::*;

fn key(seed: u8) -> [u8; HISTORY_CURSOR_KEY_LEN] {
    [seed; HISTORY_CURSOR_KEY_LEN]
}

fn at(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(seconds, 0).unwrap()
}

fn cursor() -> HistoryCursor {
    HistoryCursor {
        stream: HistoryStream::UsageCompleted,
        account_id: AccountId(Uuid::from_u128(7)),
        since: Some(at(1_700_000_000)),
        until: Some(at(1_700_086_400)),
        kind: None,
        position: CursorPosition {
            at: at(1_700_043_200),
            id: Uuid::from_u128(99),
        },
    }
}

#[test]
fn a_cursor_round_trips_through_its_own_key() {
    let original = cursor();
    let token = encode_history_cursor(&key(1), &original).expect("encode");
    let decoded = decode_history_cursor(&key(1), &token).expect("decode");
    assert_eq!(decoded, original);
}

/// 载荷是**加密**的，不只是编码：排序位置里的标识不能以可解码的文本出现在游标里。
#[test]
fn the_token_does_not_carry_the_position_in_clear_text() {
    let token = encode_history_cursor(&key(2), &cursor()).expect("encode");
    let raw = URL_SAFE_NO_PAD.decode(&token).expect("base64");
    assert!(
        !raw.windows(16)
            .any(|window| window == Uuid::from_u128(99).as_bytes()),
        "定序键不该以明文出现在游标里"
    );
    assert!(
        serde_json::from_slice::<serde_json::Value>(&raw).is_err(),
        "游标载荷不该是一段可解析的 JSON"
    );
}

/// 同一个位置编两次得到不同的令牌：nonce 每次都新取，令牌不可被当成"同一页"的指纹比对。
#[test]
fn two_encodings_of_the_same_cursor_differ() {
    let first = encode_history_cursor(&key(3), &cursor()).expect("encode");
    let second = encode_history_cursor(&key(3), &cursor()).expect("encode");
    assert_ne!(first, second);
}

#[test]
fn a_tampered_or_foreign_token_is_a_parameter_error() {
    let token = encode_history_cursor(&key(4), &cursor()).expect("encode");
    // 改一个字节：认证标签对不上。
    let mut bytes = URL_SAFE_NO_PAD.decode(&token).expect("base64");
    let last = bytes.len() - 1;
    bytes[last] ^= 0x01;
    let tampered = URL_SAFE_NO_PAD.encode(&bytes);
    assert!(matches!(
        decode_history_cursor(&key(4), &tampered),
        Err(ApplicationError::InvalidParameter(_))
    ));

    // 换一把密钥：解不开，同样是参数错误（不泄露"密钥不对"）。
    assert!(matches!(
        decode_history_cursor(&key(5), &token),
        Err(ApplicationError::InvalidParameter(_))
    ));

    // 根本不是令牌。
    for garbage in ["", "not-a-token", "AAAA"] {
        assert!(
            matches!(
                decode_history_cursor(&key(4), garbage),
                Err(ApplicationError::InvalidParameter(_))
            ),
            "{garbage:?} 应当被拒"
        );
    }
}

/// 游标只在**同一账户、同一流、同一筛选面**下有效：任何一项不同都算"不属于这次查询"。
#[test]
fn a_cursor_only_matches_its_own_query() {
    let original = cursor();
    let same = HistoryFilter {
        stream: HistoryStream::UsageCompleted,
        account_id: original.account_id,
        since: original.since,
        until: original.until,
        kind: None,
    };
    assert!(original.matches(&same));
    assert!(!original.matches(&HistoryFilter {
        stream: HistoryStream::Ledger,
        ..same.clone()
    }));
    assert!(!original.matches(&HistoryFilter {
        account_id: AccountId(Uuid::from_u128(8)),
        ..same.clone()
    }));
    assert!(!original.matches(&HistoryFilter {
        since: Some(at(1_700_000_001)),
        ..same.clone()
    }));
    assert!(!original.matches(&HistoryFilter {
        kind: Some("credit".to_owned()),
        ..same.clone()
    }));
}

/// 由筛选面与位置编出的游标，回头必须认得出它属于同一次查询。
#[test]
fn a_cursor_built_from_a_filter_matches_that_filter() {
    let filter = HistoryFilter {
        stream: HistoryStream::Ledger,
        account_id: AccountId(Uuid::from_u128(7)),
        since: Some(at(1_700_000_000)),
        until: Some(at(1_700_086_400)),
        kind: Some("credit".to_owned()),
    };
    let cursor = filter.cursor_at(CursorPosition {
        at: at(1_700_043_200),
        id: Uuid::from_u128(99),
    });
    assert!(cursor.matches(&filter));
    let token = encode_history_cursor(&key(6), &cursor).expect("encode");
    assert_eq!(
        decode_history_cursor(&key(6), &token).expect("decode"),
        cursor
    );
}
