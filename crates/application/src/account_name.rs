//! 账户名称的规则与自动生成（[账户名称 Spec](../../../docs/contracts/0003-account-names-and-login-identities.md) §2）。
//!
//! 名称是账户资料：可重复、可被运营与该账户的客户本人修改，不参与认证、授权、路由或金额计算。
//! 这里的两个公开函数是**唯一的规则实现**——API 只负责区分“省略”与“显式 `null`”，持久化只负责写库，
//! Web 的同名校验只是为了少一次往返，判定权威始终在服务端。

use seeai_domain::AccountId;
use unicode_general_category::{GeneralCategory, get_general_category};

use crate::ApplicationError;

/// 名称的字符数上限，按 Unicode scalar value 计数（Spec N2）。
pub const ACCOUNT_NAME_MAX_CHARS: usize = 100;

/// 有登录邮箱时的 id 片段长度：从 Spec N3 的 4 位起步，撞名就加长。
const GENERATED_EMAIL_FRAGMENT_LENGTHS: [usize; 5] = [4, 6, 8, 12, 16];
/// 没有可用邮箱时的 id 片段长度：从 Spec N3 的 8 位起步，撞名就加长。
const GENERATED_FALLBACK_FRAGMENT_LENGTHS: [usize; 3] = [8, 12, 16];

/// 规范化并校验一个由调用方给出的名称（Spec N2）。
///
/// 去掉首尾 Unicode 空白后必须是 1–100 个字符，且不含控制字符（`Cc`）与格式字符（`Cf`）；
/// `Cf` 只为 emoji 组合放行零宽非连接符与零宽连接符。内部连续空格原样保留，不压缩。
pub fn normalize_account_name(raw: &str) -> Result<String, ApplicationError> {
    let trimmed = raw.trim();
    let length = trimmed.chars().count();
    if length == 0 {
        return Err(ApplicationError::Validation(
            "account name must not be empty".to_owned(),
        ));
    }
    if length > ACCOUNT_NAME_MAX_CHARS {
        return Err(ApplicationError::Validation(format!(
            "account name must be at most {ACCOUNT_NAME_MAX_CHARS} characters"
        )));
    }
    if has_rejected_character(trimmed) {
        return Err(ApplicationError::Validation(
            "account name must not contain control or format characters".to_owned(),
        ));
    }
    Ok(trimmed.to_owned())
}

/// 名称留空时的**候选序列**：名称必须唯一（Spec N1），首选撞名时依次加长 id 片段再试。
///
/// 顺序是"先给最好看的"：邮箱形式从 4 位片段起步，其后加长；`账户_` 形式从 8 位起步加长，并始终排在
/// 邮箱形式之后作为兜底。最后一项用 16 位片段，撞名概率已经可以忽略（真撞上就用尽候选、返回冲突）。
/// 结果按顺序去重，每一项都满足 [`normalize_account_name`]。
#[must_use]
pub fn generated_account_name_attempts(account_id: AccountId, email: Option<&str>) -> Vec<String> {
    let local_part = email
        .and_then(|email| {
            email
                .split_once('@')
                .map(|(local, _)| local.trim().to_owned())
        })
        .filter(|local| !local.is_empty() && !has_rejected_character(local));

    let mut attempts = Vec::new();
    if let Some(local_part) = local_part.as_deref() {
        for fragment in GENERATED_EMAIL_FRAGMENT_LENGTHS {
            let suffix = format!("_{}", id_fragment(account_id, fragment));
            // 本地部分超长时按 N2 的上限截断（保留前若干字符）：拼上后缀仍不超过上限。
            let allowed = ACCOUNT_NAME_MAX_CHARS
                .saturating_sub(suffix.chars().count())
                .max(1);
            let truncated = local_part.chars().take(allowed).collect::<String>();
            push_candidate(&mut attempts, format!("{truncated}{suffix}"));
        }
    }
    // 兜底形式始终参与：邮箱形式全部撞名（或本来就没有邮箱）时还有 `账户_<id>` 可用。
    for fragment in GENERATED_FALLBACK_FRAGMENT_LENGTHS {
        push_candidate(&mut attempts, fallback_name(account_id, fragment));
    }
    attempts
}

/// 候选要能过 N2 才进列表（本地部分可能超长或带空白），重复的候选只留第一次出现的位置。
fn push_candidate(attempts: &mut Vec<String>, candidate: String) {
    if let Ok(name) = normalize_account_name(&candidate)
        && !attempts.contains(&name)
    {
        attempts.push(name);
    }
}

/// 候选用尽仍撞名时的统一答复：调用方（运营或客户）自己换一个名字。
#[must_use]
pub(crate) fn name_taken_error() -> ApplicationError {
    ApplicationError::NameTaken("account name is already taken".to_owned())
}

/// `账户_<账户 id 前 n 位>`：没有可用邮箱时的生成形式，也是候选序列的兜底。
fn fallback_name(account_id: AccountId, fragment: usize) -> String {
    format!("账户_{}", id_fragment(account_id, fragment))
}

/// 账户 id 的前 `chars` 个十六进制字符（`Uuid` 的短横线形式去掉短横线也能用，这里按常规取前几位）。
fn id_fragment(account_id: AccountId, chars: usize) -> String {
    account_id
        .to_string()
        .chars()
        .take(chars)
        .collect::<String>()
}

/// 是否含 Spec N2 拒绝的字符：控制字符（`Cc`）与格式字符（`Cf`），后者只放行 emoji 组合要用的
/// 零宽非连接符与零宽连接符。判定走 Unicode 分类表，不手写区间。
fn has_rejected_character(value: &str) -> bool {
    value.chars().any(|c| match get_general_category(c) {
        GeneralCategory::Control => true,
        GeneralCategory::Format => !matches!(c, '\u{200C}' | '\u{200D}'),
        _ => false,
    })
}

#[cfg(test)]
mod tests;
