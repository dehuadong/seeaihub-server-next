//! 客户历史翻页的**不透明游标**：排序位置、账户与筛选条件，经环境密钥认证加密。
//!
//! 为什么不是 offset：新记录插到首页时 offset 会把后续页整体推移，翻页会漏项。为什么把定位信息交给
//! 客户端而不是服务端存映射：不引入持久游标表与清理任务，代价是部署要提供一份稳定的密钥、且轮换会
//! 让旧游标失效（`docs/design/0014-customer-console-navigation-and-history.md` §3、§6）。
//!
//! 载荷是**加密**的，不只是签名：排序位置里有 Job 标识与分录行标识，而设计要求它们不以可解码的文本
//! 出现在响应里。密钥只从环境变量读（`apps/api` 侧的配置），缺失或格式无效时进程启动失败。

use aws_lc_rs::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use aws_lc_rs::rand::{SecureRandom, SystemRandom};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use seeai_domain::AccountId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ApplicationError;

/// 密钥长度：AES-256-GCM 要求 32 字节。
pub const HISTORY_CURSOR_KEY_LEN: usize = 32;

/// 游标属于哪一条历史流。
///
/// 两条流的排序键与筛选面不同（用量按终态时刻与 Job，流水按入账时刻与行标识），所以游标里带上它：
/// 把一条流的游标用到另一条流上必须被拒，而不是拿一个位置去查另一张表。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryStream {
    /// 已结束的调用记录（按 `terminal_at` + Job 倒序）。
    UsageCompleted,
    /// 真实资金流水（按 `created_at` + 行标识倒序）。
    Ledger,
}

/// 游标里的排序位置：时刻 + 定序键。
///
/// 时刻可能并列（同一事务写入的多条共用 `now()`），所以位置必须带定序键，否则翻页会在并列处重复或
/// 漏项。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorPosition {
    pub at: DateTime<Utc>,
    pub id: Uuid,
}

/// 一条游标携带的全部信息：**翻页要用的位置**加上**它属于哪一次查询**。
///
/// 筛选条件原样带在游标里（而不是只带一个摘要）：解码之后逐项与本次请求比对，任何一项不同就是
/// “这条游标不属于这次查询”，按参数错误回，不静默从首页重查。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryCursor {
    pub stream: HistoryStream,
    pub account_id: AccountId,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    /// 流水类别筛选；用量流没有这一项。
    pub kind: Option<String>,
    pub position: CursorPosition,
}

/// 一次历史查询的**筛选面**：流、账户、区间与类别。
///
/// 游标必须与它逐项相同才作数；把这几个值收成一个类型，编游标、解游标与比对都只传它一个。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryFilter {
    pub stream: HistoryStream,
    pub account_id: AccountId,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub kind: Option<String>,
}

impl HistoryFilter {
    /// 这次查询在 `position` 之后的下一页游标。
    #[must_use]
    pub fn cursor_at(&self, position: CursorPosition) -> HistoryCursor {
        HistoryCursor {
            stream: self.stream,
            account_id: self.account_id,
            since: self.since,
            until: self.until,
            kind: self.kind.clone(),
            position,
        }
    }
}

impl HistoryCursor {
    /// 这条游标是否正好属于这次查询：流、账户与筛选面逐项相同。
    ///
    /// 不匹配时调用方回参数错误——拿旧区间的游标去查新区间会给出看起来合理但语义错位的第二页。
    #[must_use]
    pub fn matches(&self, filter: &HistoryFilter) -> bool {
        self.stream == filter.stream
            && self.account_id == filter.account_id
            && self.since == filter.since
            && self.until == filter.until
            && self.kind == filter.kind
    }
}

/// 把一份载荷编成不透明游标：`base64url(nonce ‖ ciphertext‖tag)`。
pub fn encode_history_cursor(
    key: &[u8; HISTORY_CURSOR_KEY_LEN],
    cursor: &HistoryCursor,
) -> Result<String, ApplicationError> {
    let payload = serde_json::to_vec(cursor)
        .map_err(|error| ApplicationError::Persistence(format!("cursor payload: {error}")))?;
    let mut nonce = [0u8; NONCE_LEN];
    SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| ApplicationError::Persistence("cursor nonce".to_owned()))?;
    let sealing = sealing_key(key)?;
    let mut sealed = payload;
    sealing
        .seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce),
            Aad::empty(),
            &mut sealed,
        )
        .map_err(|_| ApplicationError::Persistence("cursor sealing failed".to_owned()))?;
    let mut token = Vec::with_capacity(NONCE_LEN + sealed.len());
    token.extend_from_slice(&nonce);
    token.extend_from_slice(&sealed);
    Ok(URL_SAFE_NO_PAD.encode(token))
}

/// 解一条游标。**任何**解不开的情形都是"这次请求的参数不成立"，回参数错误。
pub fn decode_history_cursor(
    key: &[u8; HISTORY_CURSOR_KEY_LEN],
    token: &str,
) -> Result<HistoryCursor, ApplicationError> {
    let raw = URL_SAFE_NO_PAD.decode(token).map_err(|_| cursor_error())?;
    if raw.len() <= NONCE_LEN {
        return Err(cursor_error());
    }
    let (nonce, sealed) = raw.split_at(NONCE_LEN);
    let mut buffer = sealed.to_vec();
    let opening = sealing_key(key)?;
    let plaintext = opening
        .open_in_place(
            Nonce::assume_unique_for_key(nonce.try_into().map_err(|_| cursor_error())?),
            Aad::empty(),
            &mut buffer,
        )
        .map_err(|_| cursor_error())?;
    serde_json::from_slice(plaintext).map_err(|_| cursor_error())
}

/// 解码失败、密钥不对与"游标不属于这次查询"回同一句：调用方能做的都是重新查询，多说的话只会变成
/// 猜测密钥的入口。
#[must_use]
pub fn invalid_history_cursor() -> ApplicationError {
    ApplicationError::InvalidParameter("history cursor is not valid for this query".to_owned())
}

fn cursor_error() -> ApplicationError {
    invalid_history_cursor()
}

fn sealing_key(key: &[u8; HISTORY_CURSOR_KEY_LEN]) -> Result<LessSafeKey, ApplicationError> {
    let unbound = UnboundKey::new(&AES_256_GCM, key)
        .map_err(|_| ApplicationError::Persistence("cursor key rejected".to_owned()))?;
    Ok(LessSafeKey::new(unbound))
}

#[cfg(test)]
mod tests;
