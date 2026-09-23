use super::sanitize_provider_text;

#[test]
fn credential_fragments_are_removed_from_provider_text() {
    assert_eq!(
        sanitize_provider_text("Forbidden – key(ab12cd) allowed only from approved IP ranges."),
        "Forbidden –  allowed only from approved IP ranges."
    );
    // 一条消息里出现多次也要全去掉。
    assert_eq!(
        sanitize_provider_text("key(aaa) then key(bbb) done"),
        " then  done"
    );
}

#[test]
fn text_without_credential_fragments_is_untouched() {
    assert_eq!(
        sanitize_provider_text("insufficient_user_quota: quota exhausted"),
        "insufficient_user_quota: quota exhausted"
    );
}

#[test]
fn an_unclosed_fragment_drops_the_rest_of_the_message() {
    // 宁可丢掉后半句，也不能把括号里的内容放出去。
    assert_eq!(
        sanitize_provider_text("Forbidden – key(ab12cd nothing closes this"),
        "Forbidden – "
    );
}
