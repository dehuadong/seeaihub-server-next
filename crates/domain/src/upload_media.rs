//! 上传素材的领域规则：允许的媒体类型（按内容魔数判定）、单文件上限、对象键构造与写入失败分类。
//!
//! 这一层只有纯类型与纯函数：不依赖 HTTP、对象存储、数据库与环境变量。准入只看**内容魔数**，
//! 调用方声明的文件名与 `Content-Type` 都不影响判型；对象键的扩展名由判出的规范 MIME 反推。
//! 取值域与机制由[对象存储上传设计](../../docs/design/0021-object-storage-upload.md)拥有，
//! 对客行为合同由[图片上传与对象存储 Spec](../../docs/specs/0007-image-upload-and-object-storage.md)拥有。

use crate::AccountId;
use uuid::Uuid;

/// 单文件字节上限：**严格小于** 20 MiB。它不随部署配置变化，因此是领域常量而不是环境变量。
pub const MAX_UPLOAD_BYTES: u64 = 20 * 1024 * 1024;

/// 对象键的固定前缀。
pub const OBJECT_KEY_PREFIX: &str = "reference-media";

/// 允许上传的图片类型。取值由内容魔数判定，与调用方声明的文件名、`Content-Type` 无关。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadMediaType {
    Jpeg,
    Png,
    Webp,
}

impl UploadMediaType {
    /// 全部受理类型。
    pub const ALL: [Self; 3] = [Self::Jpeg, Self::Png, Self::Webp];

    /// 服务端判定的规范 MIME：对客响应与一致性比较都读它。
    #[must_use]
    pub fn canonical_mime(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Webp => "image/webp",
        }
    }

    /// 对象键的扩展名，由规范 MIME 反推；调用方文件名不进键。
    #[must_use]
    pub fn object_key_extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::Webp => "webp",
        }
    }

    /// 按内容魔数判型；认不出返回 `None`（0 字节、视频、音频与其他字节都在此列）。
    #[must_use]
    pub fn from_magic_bytes(bytes: &[u8]) -> Option<Self> {
        const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF];
        const RIFF: &[u8] = b"RIFF";
        const WEBP: &[u8] = b"WEBP";
        if bytes.starts_with(PNG) {
            return Some(Self::Png);
        }
        if bytes.starts_with(JPEG) {
            return Some(Self::Jpeg);
        }
        // WebP 是 RIFF 容器：前四字节 `RIFF`、第 8–12 字节 `WEBP`（中间四字节是长度）。
        if bytes.len() >= 12 && &bytes[0..4] == RIFF && &bytes[8..12] == WEBP {
            return Some(Self::Webp);
        }
        None
    }
}

/// 单文件字节是否在上限内：**严格小于** `MAX_UPLOAD_BYTES`，等于上限即超限。
#[must_use]
pub fn within_single_file_limit(byte_length: u64) -> bool {
    byte_length < MAX_UPLOAD_BYTES
}

/// 构造对象键：`reference-media/{调用者账户 id}/{uuid}.{ext}`。
///
/// 账户 id 来自上传端点的 API Key 鉴权结果，把同一账号的素材归在同一个前缀下；`identifier`
/// 是随机 v4，扩展名由规范 MIME 反推；调用方文件名不进键，也不进对象元数据。
#[must_use]
pub fn object_key(account_id: AccountId, identifier: Uuid, media_type: UploadMediaType) -> String {
    format!(
        "{OBJECT_KEY_PREFIX}/{account_id}/{identifier}.{}",
        media_type.object_key_extension()
    )
}

/// 用一个新的随机 v4 构造对象键。
#[must_use]
pub fn new_object_key(account_id: AccountId, media_type: UploadMediaType) -> String {
    object_key(account_id, Uuid::new_v4(), media_type)
}

/// 一次对象存储写入的失败分类：决定编排层重试同一对象键还是终止。
///
/// 分类由[对象存储上传设计](../../docs/design/0021-object-storage-upload.md) §7 的表判定：
/// 传输网络错误、连接超时、对象存储 `408` / `429` / `5xx` 可重试；凭证无效、
/// 权限不足、bucket 或 region 不可访问、禁止覆盖相撞、元数据核验不一致与其他 `4xx` 是终态。
/// 两类对客都是 `503 object_store_unavailable`，差别只在要不要按同一对象键重试。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadWriteFailure {
    /// 可重试。`retry_after_seconds` 只在对象存储给出有界整数秒 `Retry-After` 时是
    /// `Some`，否则编排层按固定退避等待。
    Retryable { retry_after_seconds: Option<u64> },
    /// 终态：不重试、不换键重写、不返回 URL。
    Terminal,
}

#[cfg(test)]
mod tests;
