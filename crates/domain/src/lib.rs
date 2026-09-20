use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt::{Display, Formatter};
use thiserror::Error;
use uuid::Uuid;

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl Display for $name {
            fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

id_type!(AccountId);
id_type!(AssetId);
id_type!(AttemptId);
id_type!(ChannelId);
id_type!(JobId);
id_type!(OfferingId);
id_type!(PricePlanId);
id_type!(RuntimeRevisionId);
id_type!(VendorModelId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageBranch {
    PromptOnly,
    ImageConditioned,
    Masked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Accepted,
    Leased,
    Submitting,
    Succeeded,
    Failed,
    ReconciliationRequired,
    Canceled,
}

impl JobState {
    pub fn transition(self, next: Self) -> Result<Self, DomainError> {
        let allowed = matches!(
            (self, next),
            (Self::Accepted, Self::Leased)
                | (Self::Accepted, Self::Canceled)
                | (Self::Leased, Self::Submitting)
                | (Self::Leased, Self::Accepted)
                | (Self::Leased, Self::Failed)
                | (Self::Submitting, Self::Succeeded)
                | (Self::Submitting, Self::Failed)
                | (Self::Submitting, Self::ReconciliationRequired)
                | (Self::ReconciliationRequired, Self::Failed)
        );
        if allowed {
            Ok(next)
        } else {
            Err(DomainError::InvalidStateTransition {
                from: self,
                to: next,
            })
        }
    }

    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Canceled)
    }
}

impl Display for JobState {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Accepted => "accepted",
            Self::Leased => "leased",
            Self::Submitting => "submitting",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::ReconciliationRequired => "reconciliation_required",
            Self::Canceled => "canceled",
        };
        formatter.write_str(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetBinding {
    pub native_parameter_path: String,
    pub asset_id: AssetId,
    pub position: u16,
}

impl AssetBinding {
    /// 原生参数的**名字**：路径的第一段。
    pub fn parameter_name(&self) -> &str {
        asset_parameter_name(&self.native_parameter_path)
    }

    /// 路径是否指向数组元素（`/image_urls/0`）。
    pub fn is_array(&self) -> bool {
        asset_parameter_is_array(&self.native_parameter_path)
    }

    /// 这个绑定装的是参考图还是遮罩；`None` 表示指向的平台不认识的参数。
    pub fn kind(&self) -> Option<AssetParameterKind> {
        AssetParameterKind::classify(self.parameter_name())
    }
}

/// 资产绑定能装的两类图片输入。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetParameterKind {
    /// 参考图（图生图/编辑的输入图）。
    Image,
    /// 遮罩。
    Mask,
}

impl AssetParameterKind {
    /// 按**参数名**判定这个绑定装的是哪一类图片。
    ///
    /// 这是平台唯一的一处名字约定（发布期校验与运行期用的是同一个函数，不会各判一套）：
    /// 名字里含 `mask` 的就是遮罩、以 `image` 开头的就是参考图，**其余一律不认**——
    /// 宁可拒绝，也不猜。
    ///
    /// 已知代价（**有意的收窄**）：名字不以 `image` 开头、也不含 `mask` 时（例如
    /// `reference_images`），平台会拒绝该绑定，而不是按渠道加名字特例。
    pub fn classify(parameter_name: &str) -> Option<Self> {
        if is_mask_parameter_name(parameter_name) {
            Some(Self::Mask)
        } else if is_image_parameter_name(parameter_name) {
            Some(Self::Image)
        } else {
            None
        }
    }
}

/// 在某个候选声明的参数面里，找出装这一类图片的参数，返回**装载路径**。
///
/// 这是"调用方只给 `image` / `mask`，平台自己落到该候选的字段上"的落点：名字约定仍是
/// [`AssetParameterKind::classify`] 那一处；数组型参数（`image_urls`）取下标，标量型
/// （`image` / `mask_url`）直接赋值。找不到就返回 `None`——调用方需要这类图片而该候选
/// 表达不了，候选因此不合格（选路按映射能力判定，而不是按调用方写了哪个字段名）。
#[must_use]
pub fn asset_parameter_path(
    capability_schema: &serde_json::Value,
    kind: AssetParameterKind,
    index: usize,
) -> Option<String> {
    let properties = capability_schema.get("properties")?.as_object()?;
    let (name, schema) = properties
        .iter()
        .find(|(name, _)| AssetParameterKind::classify(name) == Some(kind))?;
    let is_array = schema
        .get("type")
        .and_then(|value| value.as_str())
        .is_some_and(|value| value == "array")
        || schema.get("items").is_some();
    Some(if is_array {
        format!("/{name}/{index}")
    } else {
        format!("/{name}")
    })
}

/// 资产绑定路径（`/image_urls/0`）的第一段：参数名。
pub fn asset_parameter_name(path: &str) -> &str {
    path.trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or_default()
}

/// 资产绑定路径是否指向数组元素（带非空的第二段）。
///
/// 运行期（Driver 回填 URL）与受理期（平台注入资产占位符）用的是这一个判定，
/// 不允许两边各判一套。
pub fn asset_parameter_is_array(path: &str) -> bool {
    path.trim_start_matches('/')
        .split('/')
        .nth(1)
        .is_some_and(|index| !index.is_empty())
}

/// 参数名是否表示"这是参考图"：以 `image` 开头（`image`、`images`、`image_urls`）。
pub fn is_image_parameter_name(name: &str) -> bool {
    name.starts_with("image")
}

/// 参数名是否表示"这是遮罩"：名字里含 `mask`（`mask`、`mask_url`）。
pub fn is_mask_parameter_name(name: &str) -> bool {
    name.contains("mask")
}

/// 把一个值写到 `native_parameter_path` 指向的位置，返回被写入的参数名。
///
/// 受理期（平台注入 `asset://…` 占位符）与运行期（Driver 回填上传后的 URL）都走这里，
/// 因此"标量赋值 / 数组追加"这两种形状只有一处实现。
pub fn set_native_parameter_at_path(
    object: &mut serde_json::Map<String, Value>,
    path: &str,
    value: Value,
) -> Result<String, String> {
    let name = asset_parameter_name(path);
    if name.is_empty() {
        return Err(format!("asset binding path {path} names no parameter"));
    }
    if asset_parameter_is_array(path) {
        let mut items = object
            .remove(name)
            .and_then(|existing| existing.as_array().cloned())
            .unwrap_or_default();
        items.push(value);
        object.insert(name.to_owned(), Value::Array(items));
    } else {
        object.insert(name.to_owned(), value);
    }
    Ok(name.to_owned())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateImageGeneration {
    pub account_id: AccountId,
    pub native_model_id: String,
    pub native_parameters: Value,
    #[serde(default)]
    pub asset_bindings: Vec<AssetBinding>,
    pub idempotency_key: String,
    pub max_cost_microusd: u64,
}

impl CreateImageGeneration {
    /// 这个请求属于哪条图片分支。
    ///
    /// 认不出的绑定路径**直接拒绝**，不当作"没有绑定"：静默忽略会让一张图悄悄不生效。
    pub fn branch(&self) -> Result<ImageBranch, DomainError> {
        let mut has_image = false;
        let mut has_mask = false;
        for binding in &self.asset_bindings {
            match binding.kind() {
                Some(AssetParameterKind::Image) => has_image = true,
                Some(AssetParameterKind::Mask) => has_mask = true,
                None => {
                    return Err(DomainError::UnsupportedAssetParameter(
                        binding.native_parameter_path.clone(),
                    ));
                }
            }
        }
        match (has_image, has_mask) {
            (false, false) => Ok(ImageBranch::PromptOnly),
            (true, false) => Ok(ImageBranch::ImageConditioned),
            (true, true) => Ok(ImageBranch::Masked),
            (false, true) => Err(DomainError::MaskRequiresImage),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub input_text_tokens: u64,
    pub input_image_tokens: u64,
    pub output_tokens: u64,
    pub output_text_tokens: u64,
    pub output_image_tokens: u64,
    pub total_tokens: u64,
}

impl TokenUsage {
    pub fn validate(&self) -> Result<(), DomainError> {
        let input = self
            .input_text_tokens
            .checked_add(self.input_image_tokens)
            .ok_or(DomainError::ArithmeticOverflow)?;
        let output = self
            .output_text_tokens
            .checked_add(self.output_image_tokens)
            .ok_or(DomainError::ArithmeticOverflow)?;
        let total = self
            .input_tokens
            .checked_add(self.output_tokens)
            .ok_or(DomainError::ArithmeticOverflow)?;
        if input != self.input_tokens || output != self.output_tokens || total != self.total_tokens
        {
            return Err(DomainError::InconsistentUsage);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeteringEvidence {
    pub attempt_id: AttemptId,
    pub provider_response_digest: String,
    pub usage: TokenUsage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceRates {
    pub currency: String,
    pub text_input_microusd_per_million: u64,
    pub image_input_microusd_per_million: u64,
    pub text_output_microusd_per_million: u64,
    pub image_output_microusd_per_million: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceSnapshot {
    pub price_plan_id: PricePlanId,
    pub rates: PriceRates,
    pub captured_at: DateTime<Utc>,
}

impl PriceSnapshot {
    pub fn charge_microusd(&self, usage: &TokenUsage) -> Result<u64, DomainError> {
        usage.validate()?;
        let terms = [
            (
                usage.input_text_tokens,
                self.rates.text_input_microusd_per_million,
            ),
            (
                usage.input_image_tokens,
                self.rates.image_input_microusd_per_million,
            ),
            (
                usage.output_text_tokens,
                self.rates.text_output_microusd_per_million,
            ),
            (
                usage.output_image_tokens,
                self.rates.image_output_microusd_per_million,
            ),
        ];
        let numerator = terms.into_iter().try_fold(0_u128, |sum, (tokens, rate)| {
            let term = u128::from(tokens)
                .checked_mul(u128::from(rate))
                .ok_or(DomainError::ArithmeticOverflow)?;
            sum.checked_add(term).ok_or(DomainError::ArithmeticOverflow)
        })?;
        let rounded_up = numerator.div_ceil(1_000_000);
        u64::try_from(rounded_up).map_err(|_| DomainError::ArithmeticOverflow)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublishedOffering {
    pub runtime_revision_id: RuntimeRevisionId,
    pub vendor_model_id: VendorModelId,
    pub offering_id: OfferingId,
    pub channel_id: ChannelId,
    pub native_model_id: String,
    pub native_revision: String,
    pub capability_schema: Value,
    pub restrictions: Value,
    pub adapter_key: String,
    pub provider_model_id: String,
    pub provider_kind: String,
    pub base_url: String,
    pub credential_env: String,
    pub price_snapshot: PriceSnapshot,
}

/// 同一 Vendor Model 的一个候选供给。
///
/// 同一型号可有多个 active Offering，选中顺序由 `routing_priority`
/// 决定（数字小者优先，来自发布顺序）。**每个候选自带它自己的 `capability_schema`**——
/// 因为 `catalog.vendor_models` 的唯一键含 `schema_hash`，两个 Provider 的 Profile 内容
/// 不同时会产生两行 `vendor_model`。
///
/// 与 [`PublishedOffering`] 的关系：字段完全一致，只多 `routing_priority`。
/// `PublishedOffering` 表示**受理时被选中并固化进 Job 的那一份**；本类型表示**发布物中的候选**。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OfferingCandidate {
    pub runtime_revision_id: RuntimeRevisionId,
    pub vendor_model_id: VendorModelId,
    pub offering_id: OfferingId,
    pub channel_id: ChannelId,
    pub native_model_id: String,
    pub native_revision: String,
    /// 该候选**自己的**能力声明，由它自己的 `vendor_model` 行带来。
    pub capability_schema: Value,
    pub restrictions: Value,
    pub adapter_key: String,
    pub provider_model_id: String,
    pub provider_kind: String,
    pub base_url: String,
    pub credential_env: String,
    pub price_snapshot: PriceSnapshot,
    /// 选择顺序：数字小者优先。发布时由候选数组下标决定，只有一个来源。
    pub routing_priority: i32,
}

impl OfferingCandidate {
    /// 选中后固化进 Job 的形态（丢掉仅发布侧需要的 `routing_priority`）。
    #[must_use]
    pub fn into_published(self) -> PublishedOffering {
        PublishedOffering {
            runtime_revision_id: self.runtime_revision_id,
            vendor_model_id: self.vendor_model_id,
            offering_id: self.offering_id,
            channel_id: self.channel_id,
            native_model_id: self.native_model_id,
            native_revision: self.native_revision,
            capability_schema: self.capability_schema,
            restrictions: self.restrictions,
            adapter_key: self.adapter_key,
            provider_model_id: self.provider_model_id,
            provider_kind: self.provider_kind,
            base_url: self.base_url,
            credential_env: self.credential_env,
            price_snapshot: self.price_snapshot,
        }
    }
}

/// 一次发布的产物：一个 Runtime Revision 及其为该型号写入的**完整、有序**候选集合。
///
/// 一次发布携带该模型完整的候选集合，发布即原子替换该模型既有 active 条目，
/// 因此同一模型的 active 候选集**永远来自同一个 Revision**，不会出现半套候选。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublishedRevision {
    pub runtime_revision_id: RuntimeRevisionId,
    pub native_model_id: String,
    pub candidates: Vec<OfferingCandidate>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenerationJob {
    pub id: JobId,
    pub account_id: AccountId,
    pub state: JobState,
    pub branch: ImageBranch,
    pub native_model_id: String,
    pub native_parameters: Value,
    pub asset_bindings: Vec<AssetBinding>,
    pub offering: PublishedOffering,
    pub idempotency_key: String,
    pub request_hash: String,
    pub max_cost_microusd: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DomainError {
    #[error("mask requires an image asset")]
    MaskRequiresImage,
    #[error("unsupported asset parameter path {0}")]
    UnsupportedAssetParameter(String),
    #[error("invalid job transition from {from} to {to}")]
    InvalidStateTransition { from: JobState, to: JobState },
    #[error("provider usage fields are inconsistent")]
    InconsistentUsage,
    #[error("arithmetic overflow")]
    ArithmeticOverflow,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage() -> TokenUsage {
        TokenUsage {
            input_tokens: 1051,
            input_text_tokens: 27,
            input_image_tokens: 1024,
            output_tokens: 196,
            output_text_tokens: 0,
            output_image_tokens: 196,
            total_tokens: 1247,
        }
    }

    #[test]
    fn derives_masked_branch_from_asset_bindings() {
        let command = CreateImageGeneration {
            account_id: AccountId::new(),
            native_model_id: "gpt-image-2".to_owned(),
            native_parameters: serde_json::json!({"prompt": "test"}),
            asset_bindings: vec![
                AssetBinding {
                    native_parameter_path: "/image".to_owned(),
                    asset_id: AssetId::new(),
                    position: 0,
                },
                AssetBinding {
                    native_parameter_path: "/mask".to_owned(),
                    asset_id: AssetId::new(),
                    position: 0,
                },
            ],
            idempotency_key: "test-key".to_owned(),
            max_cost_microusd: 20_000,
        };
        assert_eq!(command.branch(), Ok(ImageBranch::Masked));
    }

    #[test]
    fn refuses_mask_without_image() {
        let command = CreateImageGeneration {
            account_id: AccountId::new(),
            native_model_id: "gpt-image-2".to_owned(),
            native_parameters: serde_json::json!({"prompt": "test"}),
            asset_bindings: vec![AssetBinding {
                native_parameter_path: "/mask".to_owned(),
                asset_id: AssetId::new(),
                position: 0,
            }],
            idempotency_key: "test-key".to_owned(),
            max_cost_microusd: 20_000,
        };
        assert_eq!(command.branch(), Err(DomainError::MaskRequiresImage));
    }

    #[test]
    fn derives_parameter_name_kind_and_array_from_path() {
        // 参考图/遮罩参数按渠道各自的原生名给出：APIMart 的参考图叫 `image_urls`、
        // 遮罩叫 `mask_url`，平台不做统一改名。
        let binding = |path: &str, position: u16| AssetBinding {
            native_parameter_path: path.to_owned(),
            asset_id: AssetId::new(),
            position,
        };
        let command = |bindings: Vec<AssetBinding>| CreateImageGeneration {
            account_id: AccountId::new(),
            native_model_id: "gpt-image-2.5-flare".to_owned(),
            native_parameters: serde_json::json!({"prompt": "test"}),
            asset_bindings: bindings,
            idempotency_key: "test-key".to_owned(),
            max_cost_microusd: 20_000,
        };
        let images = binding("/image_urls/0", 0);
        assert_eq!(images.parameter_name(), "image_urls");
        assert!(images.is_array());
        assert_eq!(images.kind(), Some(AssetParameterKind::Image));

        let mask = binding("/mask_url", 0);
        assert_eq!(mask.parameter_name(), "mask_url");
        assert!(!mask.is_array());
        assert_eq!(mask.kind(), Some(AssetParameterKind::Mask));

        assert_eq!(
            command(vec![binding("/image_urls/0", 0)]).branch(),
            Ok(ImageBranch::ImageConditioned)
        );
        assert_eq!(
            command(vec![binding("/image_urls/0", 0), binding("/mask_url", 0)]).branch(),
            Ok(ImageBranch::Masked)
        );
        assert_eq!(
            command(vec![binding("/mask_url", 0)]).branch(),
            Err(DomainError::MaskRequiresImage)
        );
    }

    #[test]
    fn refuses_binding_paths_whose_parameter_it_does_not_recognise() {
        // 认不出就拒绝：静默忽略会让某张图悄悄不生效。
        let command = CreateImageGeneration {
            account_id: AccountId::new(),
            native_model_id: "gpt-image-2.5-flare".to_owned(),
            native_parameters: serde_json::json!({"prompt": "test"}),
            asset_bindings: vec![AssetBinding {
                native_parameter_path: "/seed_image".to_owned(),
                asset_id: AssetId::new(),
                position: 0,
            }],
            idempotency_key: "test-key".to_owned(),
            max_cost_microusd: 20_000,
        };
        assert_eq!(
            command.branch(),
            Err(DomainError::UnsupportedAssetParameter(
                "/seed_image".to_owned()
            ))
        );
        // 遮罩判定优先：名字里既有 `image` 又有 `mask` 的，按遮罩算。
        assert_eq!(
            AssetParameterKind::classify("image_mask"),
            Some(AssetParameterKind::Mask)
        );
    }

    #[test]
    fn calculates_edit_charge_from_verified_usage() {
        let snapshot = PriceSnapshot {
            price_plan_id: PricePlanId::new(),
            rates: PriceRates {
                currency: "USD".to_owned(),
                text_input_microusd_per_million: 5_000_000,
                image_input_microusd_per_million: 8_000_000,
                text_output_microusd_per_million: 10_000_000,
                image_output_microusd_per_million: 30_000_000,
            },
            captured_at: Utc::now(),
        };
        assert_eq!(snapshot.charge_microusd(&usage()), Ok(14_207));
    }

    #[test]
    fn rejects_inconsistent_usage() {
        let mut invalid = usage();
        invalid.total_tokens = 1;
        assert_eq!(invalid.validate(), Err(DomainError::InconsistentUsage));
    }

    #[test]
    fn rejects_unsafe_state_jump() {
        assert!(matches!(
            JobState::Accepted.transition(JobState::Succeeded),
            Err(DomainError::InvalidStateTransition { .. })
        ));
    }

    #[test]
    fn reconciliation_cannot_be_promoted_to_success_without_evidence() {
        assert!(matches!(
            JobState::ReconciliationRequired.transition(JobState::Succeeded),
            Err(DomainError::InvalidStateTransition { .. })
        ));
        assert_eq!(
            JobState::ReconciliationRequired.transition(JobState::Failed),
            Ok(JobState::Failed)
        );
    }

    #[test]
    fn allows_lease_failure_before_provider_submission() {
        assert_eq!(
            JobState::Leased.transition(JobState::Failed),
            Ok(JobState::Failed)
        );
    }
}
