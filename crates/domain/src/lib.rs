use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt::{Display, Formatter};
use thiserror::Error;
use uuid::Uuid;

mod image_parameters;
mod parameter_mapping;
mod size_spec;
pub use image_parameters::{
    ImageInputs, ImageParameterKind, contract_image_parameter_kind, declared_parameter_names,
    declared_reference_image_limit, declares_mask_parameter, declares_reference_image_parameter,
    image_inputs, image_parameter_kind, image_parameter_values, is_mask_parameter,
    is_reference_image_parameter, mask_value, place_image_inputs, platform_image_parameter,
    platform_image_parameters, take_contract_image_inputs,
};
pub use parameter_mapping::{
    ParameterEnumMaps, ParameterRenames, SizeMapping, apply_enum_maps, apply_parameter_defaults,
    apply_parameter_renames, apply_size_mapping, carries_parameter, declared_defaults,
    declared_enum_maps, declared_field_names, declared_renames, declared_size_mapping,
    declares_parameter, is_used_parameter_value, wire_parameter_name,
};
pub use size_spec::{SizeForm, SizeProfile, SizeSpec, convert_size};

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

/// 一次图片生成的受理结果（落库前的形态）。
///
/// 图片输入已经在 `native_parameters` 里**落到被选中候选自己的参数名上**：受理期把调用方给的
/// 参考图与遮罩换算成该候选声明的字段，此后平台不再有资产引用，Worker 与 Driver 只看这一份。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateImageGeneration {
    pub account_id: AccountId,
    pub gateway_model: String,
    pub native_parameters: Value,
    pub idempotency_key: String,
    pub max_cost_microusd: u64,
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

/// 受理时被选中、并随 Job 固化下来的那一份供给。
///
/// 三份内容各有归属，且都随 Job 冻结，事后再看仍是受理当时那一份：
/// - `capability_schema`：该 Vendor Model 的**调用方合同**（模型级唯一一份，落库后不再改）；
/// - `carrier_schema`：这条供给**能承载**合同里的哪些字段（各供给可以不同）；
/// - `parameter_mapping`：把合同值转成渠道包装的声明（显式默认值与尺寸换算，随 Job 一起冻结）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublishedOffering {
    pub runtime_revision_id: RuntimeRevisionId,
    pub vendor_model_id: VendorModelId,
    pub offering_id: OfferingId,
    pub channel_id: ChannelId,
    pub gateway_model: String,
    pub native_revision: String,
    pub capability_schema: Value,
    pub carrier_schema: Value,
    pub parameter_mapping: Value,
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
/// 决定（数字小者优先，来自发布顺序）。同一型号的候选**共享同一份合同**
/// （`catalog.vendor_models` 的唯一键是 `(vendor_id, native_model_id, native_revision)`，
/// 合同是模型级的唯一一份），**各自带自己的 `carrier_schema`**——渠道包装不同，
/// 这条供给能承载的字段面就不同。
///
/// 与 [`PublishedOffering`] 的关系：字段完全一致，只多 `routing_priority`。
/// `PublishedOffering` 表示**受理时被选中并固化进 Job 的那一份**；本类型表示**发布物中的候选**。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OfferingCandidate {
    pub runtime_revision_id: RuntimeRevisionId,
    pub vendor_model_id: VendorModelId,
    pub offering_id: OfferingId,
    pub channel_id: ChannelId,
    pub gateway_model: String,
    pub native_revision: String,
    /// 该型号的调用方合同，由它所属的那一行 `vendor_model` 带来。
    pub capability_schema: Value,
    /// 这条候选**自己**能承载的字段面，由它自己的 `offering` 行带来。
    pub carrier_schema: Value,
    /// 这条候选自己的合同值 → 渠道包装的声明。
    pub parameter_mapping: Value,
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
            gateway_model: self.gateway_model,
            native_revision: self.native_revision,
            capability_schema: self.capability_schema,
            carrier_schema: self.carrier_schema,
            parameter_mapping: self.parameter_mapping,
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
    pub gateway_model: String,
    pub candidates: Vec<OfferingCandidate>,
}

/// 对客目录里的一条：一个**当前真的能调**的对外模型，以及它那份模型级合同。
///
/// "真的能调"不由目录自己判：仓库按受理期**同一条**判据取数（该型号在生效的发布条目上至少
/// 有一条启用的供给，且那条供给的渠道也启用）。列出却受理不了的型号比不列更糟——调用方会照它
/// 建表单，然后在提交时落空。
///
/// 这里是**目录身份**，不是渠道：厂商是目录属性（同一个厂商模型可以由多条渠道供给），
/// 供给、渠道、驱动与任何执行记录都不进来。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublishedModel {
    /// 对外的模型名：客户端提交 `model` 时用的那个名字。
    pub gateway_model: String,
    /// 厂商标识。
    pub vendor_id: String,
    /// 合同修订。
    pub native_revision: String,
    /// 该模型的调用方合同：**发布的那一份**，客户端据此建表单。
    pub capability_schema: Value,
}

/// 合同里声明**型号身份**的那个字段的位置（`properties.model.const`）。
///
/// 它有两处用途，且必须指向同一个字段：发布期用它拒绝「把 A 型号的合同挂到 B 型号上」，
/// 对客投射时被替换的也**只有**这一个字段。位置写两遍就会各自漂移，而漂移的表现是
/// "校验过了，交给调用方的合同里那个常量却是另一个名字"——两边都自认为对。
const CONTRACT_MODEL_POINTER: &str = "/properties/model/const";

/// 读出合同声明的型号身份；合同没声明这个字段时返回 `None`。
#[must_use]
pub fn contract_model_identity(contract: &Value) -> Option<&str> {
    contract
        .pointer(CONTRACT_MODEL_POINTER)
        .and_then(Value::as_str)
}

/// 把合同声明的型号身份替换成 `model`。
///
/// **合同没声明这个字段时原样交回**，不凭空往里加键：加出来的键与库里那份合同对不上，
/// 而调用方拿到的合同必须能逐字对应回库里那一份——只有这一处允许替换。
pub fn replace_contract_model_identity(contract: &mut Value, model: &str) {
    if let Some(slot) = contract.pointer_mut(CONTRACT_MODEL_POINTER) {
        *slot = Value::String(model.to_owned());
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenerationJob {
    pub id: JobId,
    pub account_id: AccountId,
    pub state: JobState,
    pub branch: ImageBranch,
    pub gateway_model: String,
    pub native_parameters: Value,
    pub offering: PublishedOffering,
    pub idempotency_key: String,
    pub request_hash: String,
    pub max_cost_microusd: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DomainError {
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
