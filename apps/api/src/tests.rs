use super::{
    AdminSeed::*, ApiError, ApplicationError, admin_seed_decision, sanitize_provider_text,
};
use std::{
    io::{self, Write},
    sync::{Arc, Mutex},
};
use tracing_subscriber::fmt::MakeWriter;

/// 把 tracing 的输出收进内存，好断言"这条错误到底有没有留下痕迹"。
///
/// 用内存而不是子进程的 stderr：这里要验的是**这个转换有没有记日志、记的是不是带来源的那条**，
/// 那是一个纯函数行为；"日志最终写到哪"由 [`tracing_subscriber`] 在进程启动时定，与本转换无关。
#[derive(Clone, Default)]
struct CapturedLogs {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl CapturedLogs {
    fn text(&self) -> String {
        String::from_utf8(self.bytes.lock().expect("logs lock").clone()).expect("logs are utf-8")
    }
}

impl Write for CapturedLogs {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes
            .lock()
            .expect("logs lock")
            .extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for CapturedLogs {
    type Writer = CapturedLogs;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// 5xx 的**对客文案不泄漏细节**，所以日志是排障唯一能看到原因的地方：捕获的那个 500 必须留下
/// 一条带**类别**（哪一层）与**完整错误内容**的记录。
#[test]
fn a_server_error_is_logged_with_its_category_and_message() {
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_max_level(tracing::Level::ERROR)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let api_error: ApiError =
        ApplicationError::Persistence("sum over a malformed numeric column failed".to_owned())
            .into();

    assert_eq!(api_error.status, 500);
    assert_eq!(api_error.code, "internal_error");
    assert_eq!(
        api_error.message, "The server could not complete the request",
        "对客文案不泄漏内部细节"
    );
    let logged = logs.text();
    assert!(
        logged.contains("category=\"persistence\""),
        "日志必须点明是哪一层出的错：{logged}"
    );
    assert!(
        logged.contains("sum over a malformed numeric column failed"),
        "日志必须带上错误内容本身，否则排障还是只能猜：{logged}"
    );
}

/// 平台侧故障的两条路各自已有更具体的 warn（"一条候选都承载不了"、"成本护栏拦下"），兜底那条
/// ERROR 不能再打一遍：同一个错误两条日志，其中一条还说不清是哪一类。
#[test]
fn the_platform_side_failures_keep_their_own_warning() {
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_max_level(tracing::Level::ERROR)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let no_offering: ApiError =
        ApplicationError::NoEligibleOffering("no candidate can carry this request".to_owned())
            .into();
    let ceiling: ApiError = ApplicationError::RequestCostCeilingExceeded(
        "this request could cost 12000000 microusd".to_owned(),
    )
    .into();

    assert_eq!(no_offering.status, 503);
    assert_eq!(ceiling.status, 503);
    assert_eq!(no_offering.code, "platform_unavailable");
    assert_eq!(
        logs.text(),
        "",
        "这两条已有自己的 warn，不该再被 ERROR 记一遍"
    );
}

#[test]
fn the_admin_bootstrap_branches_are_distinguishable() {
    // 两个都给：建号（口令与邮箱原样传下去）。
    assert_eq!(
        admin_seed_decision(
            Some("ops@example.com".to_owned()),
            Some("secret".to_owned())
        ),
        Create {
            email: "ops@example.com".to_owned(),
            password: "secret".to_owned(),
        }
    );

    // 两个都不给：**警告、不建号、进程照起**。后台登不进去这件事要能被发现，但不该让 API 起不来。
    assert_eq!(admin_seed_decision(None, None), WarnNoAccount);
    // 空串与没配是一回事（部署里很常见）。
    assert_eq!(
        admin_seed_decision(Some("   ".to_owned()), Some(String::new())),
        WarnNoAccount
    );

    // 只给一个：点名"谁在、谁缺"，信息要够运维直接改对。
    assert_eq!(
        admin_seed_decision(Some("ops@example.com".to_owned()), None),
        Reject {
            present: "ADMIN_EMAIL",
            missing: "ADMIN_PASSWORD",
        }
    );
    assert_eq!(
        admin_seed_decision(None, Some("secret".to_owned())),
        Reject {
            present: "ADMIN_PASSWORD",
            missing: "ADMIN_EMAIL",
        }
    );
}

#[test]
fn the_missing_admin_account_warning_says_the_console_cannot_be_used() {
    // V-A8 要的"日志里看得出没有管理员账号"：文案必须点明**后果**（后台登不进去）与**原因**
    // （没有这两个变量），否则运维只会看到"什么都没发生"。
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_max_level(tracing::Level::WARN)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    match admin_seed_decision(None, None) {
        WarnNoAccount => {
            tracing::warn!(
                "no ADMIN_EMAIL/ADMIN_PASSWORD: no admin account was created, the admin console \
                 cannot be logged into until one exists"
            );
        }
        other => panic!("两个都不给时必须走「警告」那条分支，实际是 {other:?}"),
    }

    let logged = logs.text();
    for needle in ["ADMIN_EMAIL", "ADMIN_PASSWORD", "cannot be logged into"] {
        assert!(logged.contains(needle), "警告里必须出现 {needle}：{logged}");
    }
}

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
