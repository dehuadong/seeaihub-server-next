//! 账户名称的规则与自动生成（[账户名称 Spec](../../../docs/specs/0003-account-names-and-login-identities.md) §2）。
//!
//! 名称是账户资料：可重复、可被运营与该账户的客户本人修改，不参与认证、授权、路由或金额计算。
//! 这里的两个公开函数是**唯一的规则实现**——API 只负责区分“省略”与“显式 `null`”，持久化只负责写库，
//! Web 的同名校验只是为了少一次往返，判定权威始终在服务端。

use seeai_domain::AccountId;
use unicode_general_category::{GeneralCategory, get_general_category};

use crate::ApplicationError;

/// 名称的字符数上限，按 Unicode scalar value 计数（Spec N2）。
pub const ACCOUNT_NAME_MAX_CHARS: usize = 100;

/// 生成时留给邮箱本地部分的上限：拼上 `_` 与 4 位 id 片段仍不超过 [`ACCOUNT_NAME_MAX_CHARS`]。
const GENERATED_LOCAL_PART_MAX_CHARS: usize = ACCOUNT_NAME_MAX_CHARS - 5;

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

/// 名称留空时生成的名称（Spec N3）。
///
/// 能取到可用的登录邮箱本地部分就用 `<本地部分>_<账户 id 前 4 位>`，否则用 `账户_<账户 id 前 8 位>`。
/// 生成不使用路由标签、日期或序号；结果一定满足 [`normalize_account_name`] 的规则。
#[must_use]
pub fn generated_account_name(account_id: AccountId, email: Option<&str>) -> String {
    if let Some(candidate) = email.and_then(|email| email_candidate(account_id, email))
        && let Ok(name) = normalize_account_name(&candidate)
    {
        return name;
    }
    format!("账户_{}", id_fragment(account_id, 8))
}

/// 邮箱之所以能被用作生成来源，只因为它是登录身份**已经**接受过的那一个；本地部分超长或含被拒字符
/// 时返回 `None`，由调用方退回账户 id 形式——邮箱校验既不限长度、也不挡控制字符，这两条必须自己兜。
fn email_candidate(account_id: AccountId, email: &str) -> Option<String> {
    let (local_part, _) = email.split_once('@')?;
    let local_part = local_part.trim();
    if local_part.is_empty() || has_rejected_character(local_part) {
        return None;
    }
    let truncated = local_part
        .chars()
        .take(GENERATED_LOCAL_PART_MAX_CHARS)
        .collect::<String>();
    Some(format!("{truncated}_{}", id_fragment(account_id, 4)))
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
