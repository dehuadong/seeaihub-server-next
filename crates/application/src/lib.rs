use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
pub use seeai_adapter_sdk::ProviderFailureKind;
use seeai_adapter_sdk::{
    AdapterDescriptor, AdapterError, ImageAdapter, PreparedImageRequest, ProviderCredential,
    ProviderSuccess, ResolvedAsset, RetrySafety,
};
use seeai_domain::{
    AccountId, AssetBinding, AssetId, AssetParameterKind, AttemptId, CreateImageGeneration,
    GenerationJob, ImageBranch, JobId, JobState, MeteringEvidence, OfferingCandidate, OfferingId,
    PriceRates, PublishedOffering, PublishedRevision, RuntimeRevisionId, asset_parameter_path,
    is_image_parameter_name, is_mask_parameter_name, set_native_parameter_at_path,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use thiserror::Error;
use uuid::Uuid;

/// 发布一个 Vendor Model 的供给。
///
/// 一次发布携带该模型**完整、有序**的候选集合；
/// 候选的 `routing_priority` **由数组下标决定**（`0..n-1`），不接受调用方赋号——只有一个来源。
///
/// 形状判别（确定性三例，见 `normalize`）：
/// - `offerings` 为 `Some(非空)` ⇒ **数组形式**；扁平字段必须全部为空；
/// - `offerings` 为 `None` ⇒ **扁平形式**；扁平字段必须全部齐备，等价于一元素数组；
/// - `offerings` 为 `Some(空)` ⇒ 拒绝。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishRuntimeCommand {
    pub vendor_id: String,
    pub native_model_id: String,
    pub native_revision: String,
    /// 数组形式下由每个 `Offerings` 自带；扁平形式下必填。
    #[serde(default)]
    pub capability_schema: Option<Value>,
    #[serde(default = "empty_object")]
    pub restrictions: Value,
    /// 扁平形式的单个供给。数组形式下必须为 `None`。
    #[serde(default)]
    pub provider_kind: Option<String>,
    #[serde(default)]
    pub adapter_key: Option<String>,
    #[serde(default)]
    pub provider_model_id: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub credential_env: Option<String>,
    /// 数组形式的多个供给，顺序即 `routing_priority`。
    #[serde(default)]
    pub offerings: Option<Vec<OfferingDraft>>,
    /// 扁平形式的计价。数组形式下必须为 `None`。
    #[serde(default)]
    pub price_plan: Option<PricePlanDraft>,
    pub actor: String,
}

/// 发布请求的**已校验**形态：由 [`PublishRuntimeCommand::into_request`] 产出
/// （在逐候选校验之后），是仓库端口 `publish_runtime` 接收的唯一形态。
///
/// 为什么与 [`PublishRuntimeCommand`] 分开：命令是"线上格式"，允许两种线格式与
/// 各自的必填规则；请求是"已经检查过、可以落库的东西"。分开之后，数据库那层的入口
/// **在类型上**就只接受已核验的数据——绕开 `RuntimeService::publish` 直接调端口不再可能。
#[derive(Debug, Clone)]
pub struct PublishRuntimeRequest {
    pub vendor_id: String,
    pub native_model_id: String,
    pub native_revision: String,
    pub actor: String,
    /// 有序候选集：下标即 `routing_priority`。
    pub offerings: Vec<NormalizedOffering>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfferingDraft {
    pub provider_kind: String,
    pub adapter_key: String,
    pub provider_model_id: String,
    pub base_url: String,
    pub credential_env: String,
    #[serde(default = "empty_object")]
    pub restrictions: Value,
    #[serde(default)]
    pub capability_schema: Option<Value>,
    #[serde(default)]
    pub price_plan: Option<PricePlanDraft>,
}

/// 计价合同草案。
///
/// `formula` 只在**发布期**用于判别与校验，**不落库**——`pricing.price_plans` 没有该列
/// 本阶段唯一启用 `token_rates`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricePlanDraft {
    pub formula: String,
    pub currency: String,
    pub text_input_microusd_per_million: u64,
    pub image_input_microusd_per_million: u64,
    pub text_output_microusd_per_million: u64,
    pub image_output_microusd_per_million: u64,
    pub source_url: String,
}

impl PricePlanDraft {
    #[must_use]
    pub fn into_rates(self) -> PriceRates {
        PriceRates {
            currency: self.currency,
            text_input_microusd_per_million: self.text_input_microusd_per_million,
            image_input_microusd_per_million: self.image_input_microusd_per_million,
            text_output_microusd_per_million: self.text_output_microusd_per_million,
            image_output_microusd_per_million: self.image_output_microusd_per_million,
        }
    }
}

/// 归一后的单个供给：形状判别与必填校验都已完成，`routing_priority` 已按下标定好。
#[derive(Debug, Clone)]
pub struct NormalizedOffering {
    pub capability_schema: Value,
    pub restrictions: Value,
    pub provider_kind: String,
    pub adapter_key: String,
    pub provider_model_id: String,
    pub base_url: String,
    pub credential_env: String,
    pub rates: PriceRates,
    pub price_source_url: String,
    pub routing_priority: i32,
}

impl PublishRuntimeCommand {
    /// 消费命令，产出已校验的发布请求。
    #[must_use]
    pub fn into_request(self, offerings: Vec<NormalizedOffering>) -> PublishRuntimeRequest {
        PublishRuntimeRequest {
            vendor_id: self.vendor_id,
            native_model_id: self.native_model_id,
            native_revision: self.native_revision,
            actor: self.actor,
            offerings,
        }
    }

    /// 把两种形状归一到同一个有序候选列表。
    ///
    /// 这是发布接口**唯一**的形状判别点：`apps/api` 的 `Json<PublishRuntimeCommand>` 反序列化
    /// 之后，下游只处理 `Vec<NormalizedOffering>`。
    pub fn normalize(&self) -> Result<Vec<NormalizedOffering>, ApplicationError> {
        match &self.offerings {
            Some(drafts) => self.normalize_array(drafts),
            None => self.normalize_flat(),
        }
    }

    fn normalize_array(
        &self,
        drafts: &[OfferingDraft],
    ) -> Result<Vec<NormalizedOffering>, ApplicationError> {
        if drafts.is_empty() {
            return Err(ApplicationError::Validation(
                "offerings must not be empty".to_owned(),
            ));
        }
        if let Some(field) = self.first_present_flat_field() {
            return Err(ApplicationError::Validation(format!(
                "offerings is present, so the flat field {field} must be omitted"
            )));
        }
        if let Some(field) = self.flat_extras_present() {
            return Err(ApplicationError::Validation(format!(
                "offerings is present, so the flat field {field} must be omitted"
            )));
        }
        if self.flat_restrictions_present() {
            return Err(ApplicationError::Validation(
                "offerings is present, so the flat field restrictions must be omitted".to_owned(),
            ));
        }
        drafts
            .iter()
            .enumerate()
            .map(|(index, draft)| {
                let capability_schema = draft.capability_schema.clone().ok_or_else(|| {
                    ApplicationError::Validation(format!(
                        "offerings[{index}].capability_schema is required"
                    ))
                })?;
                let price_plan = draft.price_plan.clone().ok_or_else(|| {
                    ApplicationError::Validation(format!(
                        "offerings[{index}].price_plan is required"
                    ))
                })?;
                validate_price_formula(&price_plan).map_err(|message| {
                    ApplicationError::Validation(format!(
                        "offerings[{index}].price_plan: {message}"
                    ))
                })?;
                Ok(NormalizedOffering {
                    capability_schema,
                    restrictions: draft.restrictions.clone(),
                    provider_kind: draft.provider_kind.clone(),
                    adapter_key: draft.adapter_key.clone(),
                    provider_model_id: draft.provider_model_id.clone(),
                    base_url: draft.base_url.clone(),
                    credential_env: draft.credential_env.clone(),
                    price_source_url: price_plan.source_url.clone(),
                    rates: price_plan.into_rates(),
                    routing_priority: i32::try_from(index).map_err(|_| {
                        ApplicationError::Validation("too many offerings".to_owned())
                    })?,
                })
            })
            .collect()
    }

    fn normalize_flat(&self) -> Result<Vec<NormalizedOffering>, ApplicationError> {
        let missing = |field: &str| {
            ApplicationError::Validation(format!("{field} is required when offerings is absent"))
        };
        // 同一份字段清单驱动这里：任何一个缺失都拒绝（与数组形式的"必须全部为空"对称）。
        for (field, value) in self.flat_fields() {
            if value.is_none() {
                return Err(missing(field));
            }
        }
        let capability_schema = self
            .capability_schema
            .clone()
            .ok_or_else(|| missing("capability_schema"))?;
        let price_plan = self
            .price_plan
            .clone()
            .ok_or_else(|| missing("price_plan"))?;
        validate_price_formula(&price_plan)
            .map_err(|message| ApplicationError::Validation(format!("price_plan: {message}")))?;
        Ok(vec![NormalizedOffering {
            capability_schema,
            restrictions: self.restrictions.clone(),
            provider_kind: self
                .provider_kind
                .clone()
                .ok_or_else(|| missing("provider_kind"))?,
            adapter_key: self
                .adapter_key
                .clone()
                .ok_or_else(|| missing("adapter_key"))?,
            provider_model_id: self
                .provider_model_id
                .clone()
                .ok_or_else(|| missing("provider_model_id"))?,
            base_url: self.base_url.clone().ok_or_else(|| missing("base_url"))?,
            credential_env: self
                .credential_env
                .clone()
                .ok_or_else(|| missing("credential_env"))?,
            price_source_url: price_plan.source_url.clone(),
            rates: price_plan.into_rates(),
            routing_priority: 0,
        }])
    }

    /// 扁平形式必填的字段清单——**唯一一份**。
    ///
    /// 它同时驱动两件事：数组形式下的"必须全部为空"检查，与扁平形式下的"必须齐备"检查。
    /// 只留一处枚举的理由：分成两份时，漏改一处就会产生"检查了一半"——
    /// 程序不报错，但校验已经不完整。
    fn flat_fields(&self) -> [(&'static str, Option<&str>); 5] {
        [
            ("provider_kind", self.provider_kind.as_deref()),
            ("adapter_key", self.adapter_key.as_deref()),
            ("provider_model_id", self.provider_model_id.as_deref()),
            ("base_url", self.base_url.as_deref()),
            ("credential_env", self.credential_env.as_deref()),
        ]
    }

    /// 数组形式下**必须全部为空**的扁平字段。
    ///
    /// 除上表外还含 `capability_schema` 与 `price_plan`（见 [`Self::flat_extras_present`]）。
    /// `restrictions` 只在**非空**时才算"被给出"：它带 `#[serde(default)]`，缺省即空对象，
    /// 无法与显式写 `{}` 区分——而空的 `restrictions` 不携带信息，忽略它没有风险。
    fn first_present_flat_field(&self) -> Option<&'static str> {
        self.flat_fields()
            .into_iter()
            .find_map(|(name, value)| value.is_some().then_some(name))
    }

    /// 扁平形式里那两个"不是简单字符串"的字段是否被给出。
    fn flat_extras_present(&self) -> Option<&'static str> {
        if self.capability_schema.is_some() {
            return Some("capability_schema");
        }
        if self.price_plan.is_some() {
            return Some("price_plan");
        }
        None
    }

    /// `restrictions` 是否被显式给出（见 [`Self::first_present_flat_field`] 的说明）。
    fn flat_restrictions_present(&self) -> bool {
        self.restrictions
            .as_object()
            .is_some_and(|map| !map.is_empty())
    }
}

/// 一个候选在本次受理中的取舍结果，写入 `generation.routing_decisions.considered`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsideredCandidate {
    pub offering_id: OfferingId,
    pub provider_kind: String,
    pub routing_priority: i32,
    pub eligible: bool,
    /// 不合格时的原因；合格时为 `None`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
}

/// 受理时的路由判定记录。
///
/// **不复制** `base_url`/`credential_env` 等发布字段——候选集与顺序的权威是发布物
/// （`runtime_revisions.snapshot`），本记录只记「受理时用哪些请求侧事实判成了什么」。
#[derive(Debug, Clone)]
pub struct RoutingDecision {
    pub runtime_revision_id: RuntimeRevisionId,
    pub chosen_offering_id: OfferingId,
    pub considered: Vec<ConsideredCandidate>,
}

/// 校验计价形态。
///
/// 本阶段**唯一启用** `token_rates`。`formula` 不落库（`pricing.price_plans` 没有该列），
/// 它只是发布期的判别符——因此必须在这里拒绝未知取值，否则"只启用一种形态"只是注释。
/// 后续若引入新的计价形态，在此放行并同时落地对应列与结算路径。
fn validate_price_formula(price_plan: &PricePlanDraft) -> Result<(), String> {
    if price_plan.formula != "token_rates" {
        return Err(format!(
            "unsupported formula {}; this phase only enables token_rates",
            price_plan.formula
        ));
    }
    Ok(())
}

fn empty_object() -> Value {
    Value::Object(Map::new())
}

/// 按 `routing_priority` 升序取**第一个合格候选**。
///
/// 合格 = 该候选自己的 `restrictions` 允许本次分支与绑定，**且**请求满足该候选**自己的**
/// `capability_schema`。两个条件都必须用该候选自己的声明判断——这正是「每个 Provider
/// 各自声明支持面、限制只收窄」的落地方式。
///
/// **无合格候选时返回 `Validation` 错误**，即"在调用上游之前失败"，不回退到能力更宽但
/// 优先级更低的候选。
///
/// 不做的事：不因价格重排候选（价格不参与选中）。
/// 把调用方给的角色落到某个候选的装载路径上。
///
/// 候选表达不了调用方要的图片输入（参数面里没有装参考图/遮罩的参数）就是**不合格**——
/// 选路按"这份合同它能不能表达"判定，而不是按调用方恰好写了哪个渠道字段名。
fn bind_assets(
    capability_schema: &Value,
    request: &CreateImageGenerationRequest,
) -> Result<Vec<AssetBinding>, ApplicationError> {
    let mut bindings = Vec::with_capacity(request.image_asset_ids.len() + 1);
    for (index, asset_id) in request.image_asset_ids.iter().enumerate() {
        let path = asset_parameter_path(capability_schema, AssetParameterKind::Image, index)
            .ok_or_else(|| {
                ApplicationError::Validation(
                    "this offering cannot take a reference image".to_owned(),
                )
            })?;
        bindings.push(AssetBinding {
            native_parameter_path: path,
            asset_id: *asset_id,
            position: u16::try_from(index).unwrap_or(u16::MAX),
        });
    }
    if let Some(mask_id) = request.mask_asset_id {
        let path = asset_parameter_path(capability_schema, AssetParameterKind::Mask, 0)
            .ok_or_else(|| {
                ApplicationError::Validation("this offering cannot take a mask".to_owned())
            })?;
        bindings.push(AssetBinding {
            native_parameter_path: path,
            asset_id: mask_id,
            position: 0,
        });
    }
    Ok(bindings)
}

fn select_candidate(
    request: &CreateImageGenerationRequest,
    branch: ImageBranch,
    candidates: &[OfferingCandidate],
) -> Result<(PublishedOffering, Vec<AssetBinding>, RoutingDecision), ApplicationError> {
    if candidates.is_empty() {
        // 该型号没有任何 active 供给 ⇒ 对调用方是"不存在"，不是参数错误。
        return Err(ApplicationError::NotFound(format!(
            "no active offering for model {}",
            request.model
        )));
    }
    let revision_id = candidates[0].runtime_revision_id;
    // 把每个候选连它的取舍结果一起算出来。`considered` 要记录**完整**的取舍画面，
    // 而不是"评估到命中为止"的部分清单——它是判定记录，不是求值轨迹。
    let evaluated: Vec<(PublishedOffering, Vec<AssetBinding>, ConsideredCandidate)> = candidates
        .iter()
        .map(|candidate| {
            let published = candidate.clone().into_published();
            let mut skip_reason = None;
            let mut bindings = Vec::new();
            match bind_assets(&published.capability_schema, request) {
                Err(error) => skip_reason = Some(error.to_string()),
                Ok(resolved) => {
                    bindings = resolved;
                    if let Err(error) =
                        validate_restrictions(branch, &bindings, &candidate.restrictions)
                    {
                        skip_reason = Some(error.to_string());
                    } else if let Err(error) =
                        validate_native_request(request, &published, &bindings)
                    {
                        skip_reason = Some(error.to_string());
                    }
                }
            }
            let considered = ConsideredCandidate {
                offering_id: candidate.offering_id,
                provider_kind: candidate.provider_kind.clone(),
                routing_priority: candidate.routing_priority,
                eligible: skip_reason.is_none(),
                skip_reason,
            };
            (published, bindings, considered)
        })
        .collect();
    // 第一个合格候选胜出；不合格的留作诊断信息。
    let chosen = evaluated
        .iter()
        .position(|(_, _, considered)| considered.eligible);
    if let Some(chosen) = chosen {
        let considered = evaluated
            .iter()
            .map(|(_, _, considered)| considered.clone())
            .collect::<Vec<_>>();
        let (published, bindings, _) = evaluated.into_iter().nth(chosen).expect("index just found");
        let decision = RoutingDecision {
            runtime_revision_id: revision_id,
            chosen_offering_id: published.offering_id,
            considered,
        };
        return Ok((published, bindings, decision));
    }
    let reasons = evaluated
        .iter()
        .map(|(_, _, considered)| {
            format!(
                "{}#{}: {}",
                considered.provider_kind,
                considered.routing_priority,
                considered.skip_reason.as_deref().unwrap_or("unknown")
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    Err(ApplicationError::Validation(format!(
        "no eligible offering for model {} (revision {revision_id}): {reasons}",
        request.model
    )))
}

/// 对客受理请求：调用方按**合同**给字段，图片用平台资产 id 指名。
///
/// 这是**接收入口**的形状，与落库的 [`CreateImageGeneration`] 分开：那个是"已经落到某个候选
/// 的装载面"的形态（图片带具体参数路径），落库与 Worker 只看后者。两者之间的换算就是
/// Offering Parameter Mapping 的第一块：调用方给 `image` / `mask`，平台按选中候选声明的
/// 参数面决定装到哪个字段（`/image`、`/image_urls/0`、`/mask_url`…）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateImageGenerationRequest {
    pub account_id: AccountId,
    /// **对外的模型字段**：平台型号名（发布时的型号标识）。它与厂商原生名、
    /// 以及真正发给渠道的模型名是三个分开的角色。
    pub model: String,
    /// 合同里的模型参数（扁平，不再有 `native_parameters` 外壳；图片不走这里）。
    pub native_parameters: Value,
    #[serde(default)]
    pub image_asset_ids: Vec<AssetId>,
    #[serde(default)]
    pub mask_asset_id: Option<AssetId>,
    /// 幂等键：来自 `Idempotency-Key` 请求头，缺省时由接口层生成一个。
    pub idempotency_key: String,
}

impl CreateImageGenerationRequest {
    /// 这个请求属于哪条图片分支：有图无遮罩=图生图、两者都有=带遮罩、都没=文生图。
    ///
    /// 只有遮罩没有参考图直接拒绝（遮罩是"编辑范围"，没有可编辑的图没有意义）。
    pub fn branch(&self) -> Result<ImageBranch, ApplicationError> {
        match (
            self.image_asset_ids.is_empty(),
            self.mask_asset_id.is_some(),
        ) {
            (true, true) => Err(ApplicationError::Validation(
                "mask requires an input image".to_owned(),
            )),
            (true, false) => Ok(ImageBranch::PromptOnly),
            (false, false) => Ok(ImageBranch::ImageConditioned),
            (false, true) => Ok(ImageBranch::Masked),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetRecord {
    pub id: AssetId,
    pub account_id: AccountId,
    pub role: String,
    pub object_key: String,
    pub media_type: String,
    pub byte_count: u64,
    pub width: u32,
    pub height: u32,
    pub sha256: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputAsset {
    pub record: AssetRecord,
    #[serde(skip)]
    pub bytes: Bytes,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobView {
    pub id: JobId,
    pub account_id: AccountId,
    pub state: String,
    pub branch: ImageBranch,
    /// 对外的模型字段：平台型号名。
    pub model: String,
    pub result_asset_ids: Vec<AssetId>,
    pub error_code: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct ClaimedJob {
    pub job: GenerationJob,
    pub lease_owner: String,
    pub lease_expires_at: DateTime<Utc>,
}

/// 对客错误码：**消费者能看到的只有这三种**。渠道的 HTTP 状态码、错误码与原文一律不出现在对客响应里。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicErrorCode {
    /// 平台侧故障：平台在渠道侧欠费、凭证或权限问题、我们自己的参数或配置问题、渠道不可用、被限流。
    PlatformUnavailable,
    /// 受理状态不明（已进对账）：结果可能已经产生，消费者应当等对账结论。
    OutcomeUnknown,
    /// 消费者的内容被渠道拒绝（审核类）。
    ContentRejected,
}

impl PublicErrorCode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PlatformUnavailable => "platform_unavailable",
            Self::OutcomeUnknown => "outcome_unknown",
            Self::ContentRejected => "content_rejected",
        }
    }

    /// 写进 Job 的平台侧文案：**不含渠道原文**。
    #[must_use]
    pub fn default_message(self) -> &'static str {
        match self {
            Self::PlatformUnavailable => "the platform could not complete this request",
            Self::OutcomeUnknown => "the request outcome is unknown; see reconciliation",
            Self::ContentRejected => "the submitted content was rejected",
        }
    }

    /// 从落库值还原。数据库有 CHECK 约束保证取值；解析不到说明存储被绕过，按错误处理。
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "platform_unavailable" => Some(Self::PlatformUnavailable),
            "outcome_unknown" => Some(Self::OutcomeUnknown),
            "content_rejected" => Some(Self::ContentRejected),
            _ => None,
        }
    }
}

/// 对客码的**唯一**派生规则：消费者内容被拒 → `content_rejected`；受理状态不明 → `outcome_unknown`；否则平台侧故障。
///
/// 拿不准一律按平台侧处理——渠道说的"账户余额不足"指的是平台在渠道侧的账户，原样返回会让消费者去充值。
#[must_use]
pub fn public_error_code(kind: ProviderFailureKind, retry_safety: RetrySafety) -> PublicErrorCode {
    if kind == ProviderFailureKind::ConsumerContent {
        PublicErrorCode::ContentRejected
    } else if retry_safety == RetrySafety::AcceptanceUnknown {
        PublicErrorCode::OutcomeUnknown
    } else {
        PublicErrorCode::PlatformUnavailable
    }
}

#[derive(Debug, Clone)]
pub struct AttemptFailure {
    /// 渠道原始码或平台内部码：**只留内部**（Attempt 与对账），不进对客响应。
    pub provider_code: String,
    /// 对客码：写进 Job，消费者能看到的唯一一种错误码。
    pub public_code: PublicErrorCode,
    /// 渠道原文或平台说明：只留内部。
    pub message: String,
    pub trace_id: Option<String>,
    /// 平台侧失败类别：决定对客码与"是否属平台侧事件"。
    pub kind: ProviderFailureKind,
    pub target_state: JobState,
    pub hold_disposition: HoldDisposition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldDisposition {
    Release,
    RetainForReconciliation,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LeaseRecovery {
    pub returned_to_queue: u64,
    pub sent_to_reconciliation: u64,
}

#[derive(Debug, Clone)]
pub struct CompleteJob {
    pub job_id: JobId,
    pub worker_id: String,
    pub attempt_id: AttemptId,
    pub outputs: Vec<AssetRecord>,
    pub evidence: MeteringEvidence,
    pub charge_microusd: u64,
    /// 上游逐请求标识，写入 `attempts.provider_trace_id` 供人工对账。
    pub provider_trace_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RefundReconciliationCommand {
    pub job_id: JobId,
    pub note: String,
    pub business_key: String,
    pub actor: String,
}

/// 一个待人工处置的对账案例。
///
/// `provider_trace_id` 是**人工去上游核对的依据**（任务式上游的 task id；
/// 逐请求式上游的响应头标识）。没有它，对账的人不知道该查哪个任务——
/// 所以它必须出现在列表里，而不是只能去翻数据库。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciliationCaseView {
    pub id: Uuid,
    pub job_id: JobId,
    pub attempt_id: AttemptId,
    pub account_id: AccountId,
    pub reason: String,
    pub provider_trace_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// 一条平台侧失败记录：运营用它发现平台在渠道侧欠费、凭证/配置问题，以及平台自己的 bug。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderFailureView {
    pub job_id: JobId,
    pub account_id: AccountId,
    /// 客户选的型号；列表里可以直接看出是哪条供给出的问题。
    pub native_model_id: String,
    /// 当时选中的 Offering。
    pub offering_id: OfferingId,
    /// 渠道类别（例如 AIHubMix / APIMart）；没有渠道信息时为 `None`。
    pub provider_kind: Option<String>,
    pub kind: ProviderFailureKind,
    pub error_code: PublicErrorCode,
    pub provider_trace_id: Option<String>,
    /// 渠道原始码：**只在这个管理端视图里出现**，对客响应看不到。
    pub provider_error_code: Option<String>,
    /// 渠道原文（已过滤密钥类片段）。
    pub provider_error_message: Option<String>,
    pub updated_at: DateTime<Utc>,
}

/// 平台侧失败清单的筛选条件。
#[derive(Debug, Clone)]
pub struct ProviderFailureQuery {
    /// 要筛的类别；空表示不在这一层过滤（用例层会把空展开成"平台侧类别"）。
    pub kinds: Vec<ProviderFailureKind>,
    pub since: Option<DateTime<Utc>>,
    pub limit: u32,
}

#[derive(Debug, Error)]
pub enum ApplicationError {
    #[error("validation failed: {0}")]
    Validation(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("insufficient balance")]
    InsufficientBalance,
    #[error("configuration error: {0}")]
    Configuration(String),
    #[error("persistence error: {0}")]
    Persistence(String),
    #[error("object storage error: {0}")]
    ObjectStorage(String),
    #[error("provider result requires reconciliation: {0}")]
    Reconciliation(String),
}

#[async_trait]
pub trait HubRepository: Send + Sync {
    /// 发布一次 Runtime Revision。**只接受已核验的请求**（见 [`PublishRuntimeRequest`]）。
    /// 返回该 Revision 与它为这个型号写入的**完整候选集合**。
    async fn publish_runtime(
        &self,
        request: PublishRuntimeRequest,
    ) -> Result<PublishedRevision, ApplicationError>;

    /// 取该型号当前的 **active 候选集合**，按 `routing_priority` 升序。
    ///
    /// 同一模型的 active 候选集**永远来自同一个 Revision**（发布即原子替换）。
    /// 无任何 active 候选时返回空 `Vec`，不是错误——由调用方判定"无合格候选"。
    async fn active_offering(
        &self,
        native_model_id: &str,
    ) -> Result<Vec<OfferingCandidate>, ApplicationError>;

    async fn create_account(
        &self,
        account_id: AccountId,
        initial_credit_microusd: u64,
        actor: &str,
    ) -> Result<(), ApplicationError>;

    async fn credit_account(
        &self,
        account_id: AccountId,
        amount_microusd: u64,
        business_key: &str,
        actor: &str,
    ) -> Result<(), ApplicationError>;

    async fn create_api_key(
        &self,
        account_id: AccountId,
        label: &str,
        key_hash: &str,
        actor: &str,
    ) -> Result<(), ApplicationError>;

    async fn account_for_api_key(&self, key_hash: &str) -> Result<AccountId, ApplicationError>;

    async fn insert_asset(&self, asset: AssetRecord) -> Result<(), ApplicationError>;

    async fn get_asset(
        &self,
        account_id: AccountId,
        asset_id: AssetId,
    ) -> Result<AssetRecord, ApplicationError>;

    /// 创建 Job，并与 Job **同事务**写入路由判定记录。
    async fn create_job(
        &self,
        command: CreateImageGeneration,
        branch: ImageBranch,
        offering: PublishedOffering,
        request_hash: String,
        routing: RoutingDecision,
    ) -> Result<GenerationJob, ApplicationError>;

    async fn get_job(
        &self,
        account_id: AccountId,
        job_id: JobId,
    ) -> Result<JobView, ApplicationError>;

    async fn claim_next_job(
        &self,
        worker_id: &str,
        lease_duration: ChronoDuration,
    ) -> Result<Option<ClaimedJob>, ApplicationError>;

    async fn recover_expired_leases(&self) -> Result<LeaseRecovery, ApplicationError>;

    async fn begin_attempt(
        &self,
        job_id: JobId,
        worker_id: &str,
        attempt_id: AttemptId,
        request_digest: &str,
    ) -> Result<(), ApplicationError>;

    async fn renew_lease(
        &self,
        job_id: JobId,
        worker_id: &str,
        lease_duration: ChronoDuration,
    ) -> Result<(), ApplicationError>;

    async fn complete_job(&self, completion: CompleteJob) -> Result<(), ApplicationError>;

    async fn fail_job(
        &self,
        job_id: JobId,
        worker_id: &str,
        attempt_id: Option<AttemptId>,
        failure: AttemptFailure,
    ) -> Result<(), ApplicationError>;

    async fn list_open_reconciliation_cases(
        &self,
    ) -> Result<Vec<ReconciliationCaseView>, ApplicationError>;

    /// 平台侧失败清单（供运营发现欠费/凭证/配置问题）：按类别与时间筛。
    async fn provider_failures(
        &self,
        query: ProviderFailureQuery,
    ) -> Result<Vec<ProviderFailureView>, ApplicationError>;

    async fn refund_reconciliation(
        &self,
        command: RefundReconciliationCommand,
    ) -> Result<(), ApplicationError>;
}

/// 平台侧失败清单不传类别时的默认集合：只列**平台侧事件**。
///
/// 渠道不可用、被限流、消费者内容被拒虽然也记在库里，但不是运营要去修的东西——
/// 它们要显式按类别才查得到。
fn default_failure_kinds() -> Vec<ProviderFailureKind> {
    ProviderFailureKind::ALL
        .into_iter()
        .filter(|kind| kind.is_platform_side())
        .collect()
}

#[derive(Clone)]
pub struct ReconciliationService {
    repository: Arc<dyn HubRepository>,
}

impl ReconciliationService {
    #[must_use]
    pub fn new(repository: Arc<dyn HubRepository>) -> Self {
        Self { repository }
    }

    pub async fn list_open(&self) -> Result<Vec<ReconciliationCaseView>, ApplicationError> {
        self.repository.list_open_reconciliation_cases().await
    }

    /// 平台侧失败清单。
    ///
    /// 不传类别时只列**平台侧事件**（欠费、凭证/配置问题、平台自己的 bug）；渠道不可用、
    /// 被限流、消费者内容被拒虽然在库里，但不是运营要去修的，要显式按类别才查得到。
    pub async fn provider_failures(
        &self,
        query: ProviderFailureQuery,
    ) -> Result<Vec<ProviderFailureView>, ApplicationError> {
        let kinds = if query.kinds.is_empty() {
            default_failure_kinds()
        } else {
            query.kinds
        };
        self.repository
            .provider_failures(ProviderFailureQuery {
                kinds,
                since: query.since,
                limit: query.limit,
            })
            .await
    }

    pub async fn refund(
        &self,
        command: RefundReconciliationCommand,
    ) -> Result<(), ApplicationError> {
        if command.note.trim().is_empty() || command.business_key.trim().is_empty() {
            return Err(ApplicationError::Validation(
                "reconciliation note and business_key are required".to_owned(),
            ));
        }
        self.repository.refund_reconciliation(command).await
    }
}

#[derive(Clone)]
pub struct IdentityService {
    repository: Arc<dyn HubRepository>,
}

impl IdentityService {
    #[must_use]
    pub fn new(repository: Arc<dyn HubRepository>) -> Self {
        Self { repository }
    }

    pub async fn issue_api_key(
        &self,
        account_id: AccountId,
        label: &str,
        actor: &str,
    ) -> Result<String, ApplicationError> {
        if label.trim().is_empty() {
            return Err(ApplicationError::Validation(
                "api key label must not be empty".to_owned(),
            ));
        }
        let first = Uuid::new_v4().simple();
        let second = Uuid::new_v4().simple();
        let plaintext = format!("sk_seeai_{first}{second}");
        let key_hash = sha256_hex(plaintext.as_bytes());
        self.repository
            .create_api_key(account_id, label, &key_hash, actor)
            .await?;
        Ok(plaintext)
    }

    pub async fn authenticate(&self, plaintext: &str) -> Result<AccountId, ApplicationError> {
        if !plaintext.starts_with("sk_seeai_") {
            return Err(ApplicationError::NotFound("api key".to_owned()));
        }
        self.repository
            .account_for_api_key(&sha256_hex(plaintext.as_bytes()))
            .await
    }
}

#[async_trait]
pub trait AssetStore: Send + Sync {
    async fn put(
        &self,
        object_key: &str,
        bytes: Bytes,
        media_type: &str,
    ) -> Result<(), ApplicationError>;

    async fn get(&self, object_key: &str) -> Result<Bytes, ApplicationError>;
}

pub trait AdapterFactory: Send + Sync {
    fn descriptor(&self, adapter_key: &str) -> Option<AdapterDescriptor>;

    fn validate_publication(
        &self,
        adapter_key: &str,
        capability_schema: &Value,
        restrictions: &Value,
    ) -> Result<(), String>;

    fn create(
        &self,
        adapter_key: &str,
        base_url: &str,
        timeout: Duration,
    ) -> Result<Arc<dyn ImageAdapter>, ApplicationError>;
}

/// 按 `adapter_key` 分派的组合工厂。
///
/// 存在的理由：同一进程要同时服务多个渠道（每个渠道一族 Driver），而
/// `AdapterFactory` 是单一 trait 对象。**它只是装配，不含渠道语义**——
/// 每个键对应的行为仍完全由各自的 adapter crate 拥有。
#[derive(Default)]
pub struct AdapterRegistry {
    factories: Vec<Arc<dyn AdapterFactory>>,
}

impl AdapterRegistry {
    #[must_use]
    pub fn new(factories: Vec<Arc<dyn AdapterFactory>>) -> Self {
        Self { factories }
    }

    fn find(&self, adapter_key: &str) -> Option<&Arc<dyn AdapterFactory>> {
        self.factories
            .iter()
            .find(|factory| factory.descriptor(adapter_key).is_some())
    }
}

impl AdapterFactory for AdapterRegistry {
    fn descriptor(&self, adapter_key: &str) -> Option<AdapterDescriptor> {
        self.find(adapter_key)
            .and_then(|factory| factory.descriptor(adapter_key))
    }

    fn validate_publication(
        &self,
        adapter_key: &str,
        capability_schema: &Value,
        restrictions: &Value,
    ) -> Result<(), String> {
        match self.find(adapter_key) {
            Some(factory) => {
                factory.validate_publication(adapter_key, capability_schema, restrictions)
            }
            None => Err(format!("unknown adapter {adapter_key}")),
        }
    }

    fn create(
        &self,
        adapter_key: &str,
        base_url: &str,
        timeout: Duration,
    ) -> Result<Arc<dyn ImageAdapter>, ApplicationError> {
        match self.find(adapter_key) {
            Some(factory) => factory.create(adapter_key, base_url, timeout),
            None => Err(ApplicationError::Configuration(format!(
                "unknown adapter {adapter_key}"
            ))),
        }
    }
}

pub trait CredentialProvider: Send + Sync {
    fn resolve(&self, reference: &str) -> Result<ProviderCredential, ApplicationError>;
}

#[derive(Clone)]
pub struct RuntimeService {
    repository: Arc<dyn HubRepository>,
    adapters: Arc<dyn AdapterFactory>,
}

impl RuntimeService {
    #[must_use]
    pub fn new(repository: Arc<dyn HubRepository>, adapters: Arc<dyn AdapterFactory>) -> Self {
        Self {
            repository,
            adapters,
        }
    }

    /// 发布一个 Vendor Model 的供给（完整候选集合）。
    ///
    /// 顺序：形状归一到 `Vec<NormalizedOffering>` → 命令级字段校验 → **逐候选**校验
    /// （schema 封闭性、`model.const`、base_url、计价、Adapter 兼容性）→ 交给仓库逐项写入。
    /// 校验不通过时不产生任何 revision 行。
    pub async fn publish(
        &self,
        command: PublishRuntimeCommand,
    ) -> Result<PublishedRevision, ApplicationError> {
        for (name, value) in [
            ("vendor_id", command.vendor_id.as_str()),
            ("native_model_id", command.native_model_id.as_str()),
            ("native_revision", command.native_revision.as_str()),
            ("actor", command.actor.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(ApplicationError::Validation(format!(
                    "{name} must not be empty"
                )));
            }
        }
        let offerings = command.normalize()?;
        let mut normalized = Vec::with_capacity(offerings.len());
        for offering in offerings {
            normalized.push(self.validate_offering(&command, offering)?);
        }
        let request = command.into_request(normalized);
        let revision = self.repository.publish_runtime(request).await?;
        Ok(revision)
    }

    /// 校验单个候选，并归一化它的 `base_url`。
    fn validate_offering(
        &self,
        command: &PublishRuntimeCommand,
        mut offering: NormalizedOffering,
    ) -> Result<NormalizedOffering, ApplicationError> {
        jsonschema::validator_for(&offering.capability_schema)
            .map_err(|error| ApplicationError::Validation(error.to_string()))?;
        // 候选的身份必须与发布声明的型号一致：`model.const` 就是该 Provider 自己的
        // 模型名，发布期据此拒绝「把 A 型号的 Profile 挂到 B 型号上」。
        if offering
            .capability_schema
            .pointer("/properties/model/const")
            .and_then(Value::as_str)
            != Some(command.native_model_id.as_str())
        {
            return Err(ApplicationError::Validation(
                "capability_schema model.const must equal native_model_id".to_owned(),
            ));
        }
        if offering
            .capability_schema
            .get("type")
            .and_then(Value::as_str)
            != Some("object")
            || offering
                .capability_schema
                .get("additionalProperties")
                .and_then(Value::as_bool)
                != Some(false)
        {
            return Err(ApplicationError::Validation(
                "capability_schema must be a closed object schema".to_owned(),
            ));
        }
        let mut base_url = offering.base_url.trim().trim_end_matches('/').to_owned();
        if base_url.is_empty() {
            return Err(ApplicationError::Validation(
                "base_url must not be empty".to_owned(),
            ));
        }
        let parsed = url::Url::parse(&base_url)
            .map_err(|error| ApplicationError::Validation(error.to_string()))?;
        if parsed.scheme() != "https" && parsed.host_str() != Some("127.0.0.1") {
            return Err(ApplicationError::Validation(
                "provider base_url must use https outside local development".to_owned(),
            ));
        }
        if !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(ApplicationError::Validation(
                "provider base_url must not contain credentials, query, or fragment".to_owned(),
            ));
        }
        base_url.truncate(base_url.trim_end_matches('/').len());
        offering.base_url = base_url;
        if offering.credential_env.trim().is_empty() {
            return Err(ApplicationError::Validation(
                "credential_env must not be empty".to_owned(),
            ));
        }
        if offering.provider_kind.trim().is_empty()
            || offering.adapter_key.trim().is_empty()
            || offering.provider_model_id.trim().is_empty()
        {
            return Err(ApplicationError::Validation(
                "provider_kind, adapter_key and provider_model_id must not be empty".to_owned(),
            ));
        }
        if offering.rates.currency != "USD" {
            return Err(ApplicationError::Validation(
                "price currency must be USD for microUSD rates".to_owned(),
            ));
        }
        let price_source = url::Url::parse(&offering.price_source_url)
            .map_err(|error| ApplicationError::Validation(error.to_string()))?;
        if price_source.scheme() != "https" {
            return Err(ApplicationError::Validation(
                "price_source_url must use https".to_owned(),
            ));
        }
        let descriptor = self
            .adapters
            .descriptor(&offering.adapter_key)
            .ok_or_else(|| {
                ApplicationError::Validation(format!("unknown adapter {}", offering.adapter_key))
            })?;
        validate_adapter_compatibility(&offering, &descriptor)?;
        // 限制只能收窄：供货方不得声明 Profile 自己都没声明的能力。
        validate_restrictions_within_profile(&offering)?;
        self.adapters
            .validate_publication(
                &offering.adapter_key,
                &offering.capability_schema,
                &offering.restrictions,
            )
            .map_err(ApplicationError::Validation)?;
        self.adapters.create(
            &offering.adapter_key,
            &offering.base_url,
            Duration::from_secs(1),
        )?;
        Ok(offering)
    }
}

/// 校验「Offering 的限制不超出该候选 Profile 自己声明的范围」。
///
/// 这是"Provider 限制只能**收窄**，不能放宽"的落地。
/// 与 [`validate_adapter_compatibility`] 的区别：后者比对的是 **Adapter 的能力面**（Driver
/// 做不到的不许声明）；本函数比对的是 **Profile 自己声明的能力**（供货方不许替厂商放宽）。
///
/// Restrictions 的形状很小，目前只有两项，因此可判定地检查两项：
/// - `allowed_branches`：每个分支都必须能在 Profile 的 `required`/`properties` 下成立；
/// - `max_images`：不得超过 Profile 对参考图数量的声明。
fn validate_restrictions_within_profile(
    offering: &NormalizedOffering,
) -> Result<(), ApplicationError> {
    let schema = &offering.capability_schema;
    let properties = schema.get("properties").and_then(Value::as_object);
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let declares = |name: &str| {
        properties.is_some_and(|map| map.contains_key(name)) || required.contains(&name)
    };
    if let Some(branches) = offering
        .restrictions
        .get("allowed_branches")
        .and_then(Value::as_array)
    {
        for branch in branches {
            let (name, described) = match branch.as_str() {
                Some("prompt_only") => ("prompt_only", declares("prompt")),
                // 图生图/编辑需要参考图输入：Profile 必须声明一个名字以 `image`
                // 开头的参数（`image`/`images`/`image_urls`）。
                Some("image_conditioned") => {
                    ("image_conditioned", declares_image_parameter(schema))
                }
                // 遮罩编辑还需要遮罩输入，且遮罩不能脱离参考图。
                Some("masked") => (
                    "masked",
                    declares_image_parameter(schema) && declares_mask_parameter(schema),
                ),
                Some(other) => {
                    return Err(ApplicationError::Validation(format!(
                        "restriction contains an unknown image branch {other}"
                    )));
                }
                None => {
                    return Err(ApplicationError::Validation(
                        "allowed_branches must contain strings".to_owned(),
                    ));
                }
            };
            if !described {
                return Err(ApplicationError::Validation(format!(
                    "restriction allows branch {name}, which the profile does not declare"
                )));
            }
        }
    }
    if let Some(max_images) = offering
        .restrictions
        .get("max_images")
        .and_then(Value::as_u64)
    {
        let declared = declared_image_maximum(schema);
        if max_images > declared {
            return Err(ApplicationError::Validation(format!(
                "restriction allows {max_images} image(s), but the profile declares at most {declared}"
            )));
        }
    }
    Ok(())
}

/// Profile 是否声明了参考图参数。
///
/// 与运行期用的是**同一个判定**（[`is_image_parameter_name`]），因此"发布期允许的分支"
/// 与"运行期认得的绑定路径"不会各判一套。
fn declares_image_parameter(schema: &Value) -> bool {
    declared_image_maximum(schema) > 0
}

/// Profile 是否声明了遮罩参数（名字含 `mask`）。
fn declares_mask_parameter(schema: &Value) -> bool {
    declared_properties(schema)
        .is_some_and(|properties| properties.keys().any(|name| is_mask_parameter_name(name)))
}

fn declared_properties(schema: &Value) -> Option<&serde_json::Map<String, Value>> {
    schema.get("properties").and_then(Value::as_object)
}

/// Profile 对参考图数量的声明上限。
///
/// 只看名字以 `image` 开头的参数（与运行期的绑定判定是同一个函数）：数组形式取
/// `maxItems`；只有单值形式（`image`）时按 1 计；声明了数组却没写 `maxItems`
/// 视为**没有承诺上限**，也就不能支撑任何 `max_images > 0` 的限制
/// （限制只能收窄，不能凭空放宽）。
fn declared_image_maximum(schema: &Value) -> u64 {
    let Some(properties) = declared_properties(schema) else {
        return 0;
    };
    let mut scalar = false;
    let mut maximum = 0_u64;
    for (name, field) in properties {
        if !is_image_parameter_name(name) {
            continue;
        }
        match field.get("type").and_then(Value::as_str) {
            Some("array") => {
                maximum = maximum.max(field.get("maxItems").and_then(Value::as_u64).unwrap_or(0));
            }
            Some("string") => scalar = true,
            _ => {}
        }
    }
    if maximum > 0 {
        maximum
    } else if scalar {
        1
    } else {
        0
    }
}

fn validate_adapter_compatibility(
    offering: &NormalizedOffering,
    descriptor: &AdapterDescriptor,
) -> Result<(), ApplicationError> {
    let properties = offering
        .capability_schema
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            ApplicationError::Validation("capability_schema.properties is required".to_owned())
        })?;
    for parameter in properties.keys() {
        if !descriptor
            .supported_top_level_parameters
            .contains(&parameter.as_str())
        {
            return Err(ApplicationError::Validation(format!(
                "adapter {} does not support native parameter {parameter}",
                descriptor.key
            )));
        }
    }
    if let Some(extra) = properties
        .get("extra")
        .and_then(|value| value.get("properties"))
        .and_then(Value::as_object)
    {
        for parameter in extra.keys() {
            if !descriptor
                .supported_extra_parameters
                .contains(&parameter.as_str())
            {
                return Err(ApplicationError::Validation(format!(
                    "adapter {} does not support native parameter extra.{parameter}",
                    descriptor.key
                )));
            }
        }
    }
    let max_images = offering
        .restrictions
        .get("max_images")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if max_images > descriptor.max_images {
        return Err(ApplicationError::Validation(format!(
            "adapter {} supports at most {} image inputs",
            descriptor.key, descriptor.max_images
        )));
    }
    if let Some(branches) = offering
        .restrictions
        .get("allowed_branches")
        .and_then(Value::as_array)
    {
        for branch in branches {
            let parsed = match branch.as_str() {
                Some("prompt_only") => ImageBranch::PromptOnly,
                Some("image_conditioned") => ImageBranch::ImageConditioned,
                Some("masked") => ImageBranch::Masked,
                _ => {
                    return Err(ApplicationError::Validation(
                        "restriction contains an unknown image branch".to_owned(),
                    ));
                }
            };
            if !descriptor.supported_branches.contains(&parsed) {
                return Err(ApplicationError::Validation(format!(
                    "adapter {} does not support branch {parsed:?}",
                    descriptor.key
                )));
            }
        }
    }
    Ok(())
}

#[derive(Clone)]
pub struct AssetService {
    repository: Arc<dyn HubRepository>,
    store: Arc<dyn AssetStore>,
}

impl AssetService {
    #[must_use]
    pub fn new(repository: Arc<dyn HubRepository>, store: Arc<dyn AssetStore>) -> Self {
        Self { repository, store }
    }

    pub async fn upload(
        &self,
        account_id: AccountId,
        role: &str,
        media_type: &str,
        bytes: Bytes,
    ) -> Result<AssetRecord, ApplicationError> {
        if !matches!(role, "image" | "mask") {
            return Err(ApplicationError::Validation(
                "asset role must be image or mask".to_owned(),
            ));
        }
        validate_input_media(media_type, &bytes)?;
        if role == "mask" && (media_type != "image/png" || !matches!(bytes.get(25), Some(4 | 6))) {
            return Err(ApplicationError::Validation(
                "mask must be a PNG with an alpha channel".to_owned(),
            ));
        }
        let dimensions = imagesize::blob_size(&bytes)
            .map_err(|error| ApplicationError::Validation(error.to_string()))?;
        let asset_id = AssetId::new();
        let digest = sha256_hex(&bytes);
        let extension = match media_type {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            "image/webp" => "webp",
            _ => {
                return Err(ApplicationError::Validation(format!(
                    "unsupported media type {media_type}"
                )));
            }
        };
        let object_key = format!("inputs/{account_id}/{asset_id}.{extension}");
        self.store
            .put(&object_key, bytes.clone(), media_type)
            .await?;
        let record = AssetRecord {
            id: asset_id,
            account_id,
            role: role.to_owned(),
            object_key,
            media_type: media_type.to_owned(),
            byte_count: u64::try_from(bytes.len())
                .map_err(|_| ApplicationError::Validation("asset is too large".to_owned()))?,
            width: u32::try_from(dimensions.width)
                .map_err(|_| ApplicationError::Validation("asset width is too large".to_owned()))?,
            height: u32::try_from(dimensions.height).map_err(|_| {
                ApplicationError::Validation("asset height is too large".to_owned())
            })?,
            sha256: digest,
            created_at: Utc::now(),
        };
        self.repository.insert_asset(record.clone()).await?;
        Ok(record)
    }

    pub async fn download(
        &self,
        account_id: AccountId,
        asset_id: AssetId,
    ) -> Result<(AssetRecord, Bytes), ApplicationError> {
        let record = self.repository.get_asset(account_id, asset_id).await?;
        let bytes = self.store.get(&record.object_key).await?;
        if sha256_hex(&bytes) != record.sha256 {
            return Err(ApplicationError::ObjectStorage(format!(
                "asset {asset_id} digest mismatch"
            )));
        }
        Ok((record, bytes))
    }
}

#[derive(Clone)]
pub struct GenerationService {
    repository: Arc<dyn HubRepository>,
    /// 预授权额（microusd）：**服务端定的固定数**，不由调用方自报。
    ///
    /// 现状是"一个固定数"，属粗判；按 Price Snapshot 算这次请求的最坏成本是后续优化
    /// （`docs/adr/0009` 要求的"低于最小可能成本就受理前拒绝"要在那时一并实现）。
    max_cost_microusd: u64,
}

impl GenerationService {
    #[must_use]
    pub fn new(repository: Arc<dyn HubRepository>, max_cost_microusd: u64) -> Self {
        Self {
            repository,
            max_cost_microusd,
        }
    }

    pub async fn create(
        &self,
        request: CreateImageGenerationRequest,
    ) -> Result<GenerationJob, ApplicationError> {
        validate_idempotency_key(&request.idempotency_key)?;
        if self.max_cost_microusd == 0 {
            return Err(ApplicationError::Configuration(
                "generation max cost must be positive".to_owned(),
            ));
        }
        let branch = request.branch()?;
        self.validate_input_assets(&request).await?;
        let candidates = self.repository.active_offering(&request.model).await?;
        let (offering, asset_bindings, routing) = select_candidate(&request, branch, &candidates)?;
        // 幂等哈希取**调用方看到的那份请求**（不含按候选解析出的路径）：上游目录变了不该
        // 让同一个幂等键算出不同的哈希。
        let request_hash = request_hash(&request)?;
        self.repository
            .create_job(
                CreateImageGeneration {
                    account_id: request.account_id,
                    native_model_id: request.model,
                    native_parameters: request.native_parameters,
                    asset_bindings,
                    idempotency_key: request.idempotency_key,
                    max_cost_microusd: self.max_cost_microusd,
                },
                branch,
                offering,
                request_hash,
                routing,
            )
            .await
    }

    /// 输入资产必须属于本账户、角色对得上，且遮罩与参考图同尺寸。
    async fn validate_input_assets(
        &self,
        request: &CreateImageGenerationRequest,
    ) -> Result<(), ApplicationError> {
        let mut image_dimensions = None;
        for asset_id in &request.image_asset_ids {
            let asset = self
                .repository
                .get_asset(request.account_id, *asset_id)
                .await?;
            if !matches!(asset.role.as_str(), "image" | "output") {
                return Err(ApplicationError::Validation(format!(
                    "asset {} cannot be used as an image",
                    asset.id
                )));
            }
            image_dimensions.get_or_insert((asset.width, asset.height));
        }
        if let Some(mask_id) = request.mask_asset_id {
            let asset = self
                .repository
                .get_asset(request.account_id, mask_id)
                .await?;
            if asset.role != "mask" {
                return Err(ApplicationError::Validation(format!(
                    "asset {} is not a mask",
                    asset.id
                )));
            }
            if Some((asset.width, asset.height)) != image_dimensions {
                return Err(ApplicationError::Validation(
                    "mask dimensions must match the input image".to_owned(),
                ));
            }
        }
        Ok(())
    }

    pub async fn get(
        &self,
        account_id: AccountId,
        job_id: JobId,
    ) -> Result<JobView, ApplicationError> {
        self.repository.get_job(account_id, job_id).await
    }
}

pub struct WorkerService {
    repository: Arc<dyn HubRepository>,
    store: Arc<dyn AssetStore>,
    adapters: Arc<dyn AdapterFactory>,
    credentials: Arc<dyn CredentialProvider>,
    worker_id: String,
    lease_duration: ChronoDuration,
    provider_timeout: Duration,
}

impl WorkerService {
    pub fn new(
        repository: Arc<dyn HubRepository>,
        store: Arc<dyn AssetStore>,
        adapters: Arc<dyn AdapterFactory>,
        credentials: Arc<dyn CredentialProvider>,
        worker_id: String,
        lease_duration: ChronoDuration,
        provider_timeout: Duration,
    ) -> Result<Self, ApplicationError> {
        if worker_id.trim().is_empty() {
            return Err(ApplicationError::Configuration(
                "worker_id must not be empty".to_owned(),
            ));
        }
        Ok(Self {
            repository,
            store,
            adapters,
            credentials,
            worker_id,
            lease_duration,
            provider_timeout,
        })
    }

    pub async fn run_once(&self) -> Result<bool, ApplicationError> {
        self.repository.recover_expired_leases().await?;
        let Some(claimed) = self
            .repository
            .claim_next_job(&self.worker_id, self.lease_duration)
            .await?
        else {
            return Ok(false);
        };
        self.execute(claimed).await?;
        Ok(true)
    }

    async fn execute(&self, claimed: ClaimedJob) -> Result<(), ApplicationError> {
        let attempt_id = AttemptId::new();
        let prepared = match self.prepare(&claimed.job).await {
            Ok(prepared) => prepared,
            Err(error) => {
                self.repository
                    .fail_job(
                        claimed.job.id,
                        &self.worker_id,
                        None,
                        AttemptFailure {
                            provider_code: "worker_prepare_failed".to_owned(),
                            public_code: PublicErrorCode::PlatformUnavailable,
                            message: error.to_string(),
                            trace_id: None,
                            kind: ProviderFailureKind::PlatformInternal,
                            target_state: JobState::Failed,
                            hold_disposition: HoldDisposition::Release,
                        },
                    )
                    .await?;
                return Ok(());
            }
        };
        let request_digest = request_digest(&prepared)?;
        self.repository
            .begin_attempt(claimed.job.id, &self.worker_id, attempt_id, &request_digest)
            .await?;
        let credential = match self
            .credentials
            .resolve(&claimed.job.offering.credential_env)
        {
            Ok(credential) => credential,
            Err(error) => {
                self.repository
                    .fail_job(
                        claimed.job.id,
                        &self.worker_id,
                        Some(attempt_id),
                        AttemptFailure {
                            provider_code: "credential_unavailable".to_owned(),
                            public_code: PublicErrorCode::PlatformUnavailable,
                            message: error.to_string(),
                            trace_id: None,
                            kind: ProviderFailureKind::PlatformInternal,
                            target_state: JobState::Failed,
                            hold_disposition: HoldDisposition::Release,
                        },
                    )
                    .await?;
                return Ok(());
            }
        };
        let adapter = match self.adapters.create(
            &claimed.job.offering.adapter_key,
            &claimed.job.offering.base_url,
            self.provider_timeout,
        ) {
            Ok(adapter) => adapter,
            Err(error) => {
                self.repository
                    .fail_job(
                        claimed.job.id,
                        &self.worker_id,
                        Some(attempt_id),
                        AttemptFailure {
                            provider_code: "adapter_configuration_failed".to_owned(),
                            public_code: PublicErrorCode::PlatformUnavailable,
                            message: error.to_string(),
                            trace_id: None,
                            kind: ProviderFailureKind::PlatformInternal,
                            target_state: JobState::Failed,
                            hold_disposition: HoldDisposition::Release,
                        },
                    )
                    .await?;
                return Ok(());
            }
        };
        match self
            .execute_with_heartbeat(claimed.job.id, adapter, prepared, &credential)
            .await
        {
            Ok(success) => {
                if let Err(error) = self
                    .complete_success(&claimed.job, attempt_id, success)
                    .await
                {
                    self.repository
                        .fail_job(
                            claimed.job.id,
                            &self.worker_id,
                            Some(attempt_id),
                            AttemptFailure {
                                provider_code: "result_delivery_failed".to_owned(),
                                public_code: PublicErrorCode::OutcomeUnknown,
                                message: error.to_string(),
                                trace_id: None,
                                kind: ProviderFailureKind::PlatformInternal,
                                target_state: JobState::ReconciliationRequired,
                                hold_disposition: HoldDisposition::RetainForReconciliation,
                            },
                        )
                        .await?;
                }
            }
            Err(error) => {
                let failure = failure_from_adapter(error);
                self.repository
                    .fail_job(claimed.job.id, &self.worker_id, Some(attempt_id), failure)
                    .await?;
            }
        }
        Ok(())
    }

    async fn execute_with_heartbeat(
        &self,
        job_id: JobId,
        adapter: Arc<dyn ImageAdapter>,
        prepared: PreparedImageRequest,
        credential: &ProviderCredential,
    ) -> Result<ProviderSuccess, AdapterError> {
        let heartbeat_seconds = (self.lease_duration.num_seconds() / 3).max(1);
        let mut interval = tokio::time::interval(Duration::from_secs(
            u64::try_from(heartbeat_seconds).unwrap_or(1),
        ));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let call = adapter.execute(prepared, credential);
        tokio::pin!(call);
        let mut heartbeat_failure = None;
        loop {
            tokio::select! {
                result = &mut call => {
                    if let Some(message) = heartbeat_failure {
                        return Err(seeai_adapter_sdk::ProviderCallError {
                            code: "lease_heartbeat_failed".to_owned(),
                            message,
                            trace_id: None,
                            retry_safety: RetrySafety::AcceptanceUnknown,
                            kind: ProviderFailureKind::PlatformInternal,
                        }.into());
                    }
                    return result;
                },
                _ = interval.tick() => {
                    if let Err(error) = self.repository
                        .renew_lease(job_id, &self.worker_id, self.lease_duration)
                        .await
                    {
                        heartbeat_failure.get_or_insert_with(|| error.to_string());
                    }
                }
            }
        }
    }

    async fn prepare(&self, job: &GenerationJob) -> Result<PreparedImageRequest, ApplicationError> {
        let mut assets = Vec::with_capacity(job.asset_bindings.len());
        for binding in &job.asset_bindings {
            let record = self
                .repository
                .get_asset(job.account_id, binding.asset_id)
                .await?;
            let bytes = self.store.get(&record.object_key).await?;
            let digest = sha256_hex(&bytes);
            if digest != record.sha256 {
                return Err(ApplicationError::ObjectStorage(format!(
                    "asset {} digest mismatch",
                    record.id
                )));
            }
            assets.push(ResolvedAsset {
                native_parameter_path: binding.native_parameter_path.clone(),
                position: binding.position,
                media_type: record.media_type,
                sha256: record.sha256,
                bytes,
            });
        }
        Ok(PreparedImageRequest {
            provider_model_id: job.offering.provider_model_id.clone(),
            branch: job.branch,
            native_parameters: job.native_parameters.clone(),
            assets,
        })
    }

    async fn complete_success(
        &self,
        job: &GenerationJob,
        attempt_id: AttemptId,
        success: ProviderSuccess,
    ) -> Result<(), ApplicationError> {
        let charge = job
            .offering
            .price_snapshot
            .charge_microusd(&success.usage)
            .map_err(|error| ApplicationError::Reconciliation(error.to_string()))?;
        if charge > job.max_cost_microusd {
            return Err(ApplicationError::Reconciliation(format!(
                "actual charge {charge} exceeds authorization {}",
                job.max_cost_microusd
            )));
        }
        let mut outputs = Vec::with_capacity(success.images.len());
        for (index, image) in success.images.into_iter().enumerate() {
            let asset_id = AssetId::new();
            let dimensions = imagesize::blob_size(&image.bytes)
                .map_err(|error| ApplicationError::ObjectStorage(error.to_string()))?;
            let extension = match image.media_type.as_str() {
                "image/jpeg" => "jpg",
                "image/webp" => "webp",
                _ => "png",
            };
            let object_key = format!("outputs/{}/{index}-{asset_id}.{extension}", job.id);
            self.store
                .put(&object_key, image.bytes.clone(), &image.media_type)
                .await?;
            outputs.push(AssetRecord {
                id: asset_id,
                account_id: job.account_id,
                role: "output".to_owned(),
                object_key,
                media_type: image.media_type,
                byte_count: u64::try_from(image.bytes.len()).map_err(|_| {
                    ApplicationError::ObjectStorage("output is too large".to_owned())
                })?,
                width: u32::try_from(dimensions.width).map_err(|_| {
                    ApplicationError::ObjectStorage("output width is too large".to_owned())
                })?,
                height: u32::try_from(dimensions.height).map_err(|_| {
                    ApplicationError::ObjectStorage("output height is too large".to_owned())
                })?,
                sha256: image.sha256,
                created_at: Utc::now(),
            });
        }
        self.repository
            .complete_job(CompleteJob {
                job_id: job.id,
                worker_id: self.worker_id.clone(),
                attempt_id,
                outputs,
                evidence: MeteringEvidence {
                    attempt_id,
                    provider_response_digest: success.response_digest,
                    usage: success.usage,
                },
                charge_microusd: charge,
                provider_trace_id: success.provider_trace_id,
            })
            .await
    }
}

fn validate_idempotency_key(value: &str) -> Result<(), ApplicationError> {
    let length = value.len();
    if !(8..=128).contains(&length)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(ApplicationError::Validation(
            "idempotency_key must be 8-128 ASCII letters, digits, '.', '_' or '-'".to_owned(),
        ));
    }
    Ok(())
}

fn validate_restrictions(
    branch: ImageBranch,
    bindings: &[AssetBinding],
    restrictions: &Value,
) -> Result<(), ApplicationError> {
    if let Some(allowed) = restrictions
        .get("allowed_branches")
        .and_then(Value::as_array)
    {
        let name = match branch {
            ImageBranch::PromptOnly => "prompt_only",
            ImageBranch::ImageConditioned => "image_conditioned",
            ImageBranch::Masked => "masked",
        };
        if !allowed.iter().any(|value| value.as_str() == Some(name)) {
            return Err(ApplicationError::Validation(format!(
                "offering does not allow branch {name}"
            )));
        }
    }
    let image_count = bindings
        .iter()
        .filter(|binding| binding.kind() == Some(AssetParameterKind::Image))
        .count();
    let max_images = restrictions
        .get("max_images")
        .and_then(Value::as_u64)
        .unwrap_or(1);
    if u64::try_from(image_count).unwrap_or(u64::MAX) > max_images {
        return Err(ApplicationError::Validation(format!(
            "offering accepts at most {max_images} image(s)"
        )));
    }
    Ok(())
}

fn validate_native_request(
    request: &CreateImageGenerationRequest,
    offering: &PublishedOffering,
    bindings: &[AssetBinding],
) -> Result<(), ApplicationError> {
    let mut instance = request.native_parameters.clone();
    let object = instance.as_object_mut().ok_or_else(|| {
        ApplicationError::Validation("native_parameters must be an object".to_owned())
    })?;
    object.insert("model".to_owned(), Value::String(request.model.clone()));
    for binding in bindings {
        inject_asset_placeholder(object, binding)?;
    }
    let validator = jsonschema::validator_for(&offering.capability_schema)
        .map_err(|error| ApplicationError::Configuration(error.to_string()))?;
    let errors = validator
        .iter_errors(&instance)
        .map(|error| error.to_string())
        .collect::<Vec<_>>();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ApplicationError::Validation(errors.join("; ")))
    }
}

fn inject_asset_placeholder(
    object: &mut Map<String, Value>,
    binding: &AssetBinding,
) -> Result<(), ApplicationError> {
    // 写入规则（标量赋值 / 数组追加）与 Driver 回填 URL 时共用同一份实现。
    set_native_parameter_at_path(
        object,
        &binding.native_parameter_path,
        Value::String(format!("asset://{}", binding.asset_id)),
    )
    .map(|_| ())
    .map_err(ApplicationError::Validation)
}

fn request_hash<T: Serialize>(value: &T) -> Result<String, ApplicationError> {
    let mut value = serde_json::to_value(value)
        .map_err(|error| ApplicationError::Validation(error.to_string()))?;
    canonicalize_json(&mut value);
    let encoded = serde_json::to_vec(&value)
        .map_err(|error| ApplicationError::Validation(error.to_string()))?;
    Ok(sha256_hex(&encoded))
}

fn request_digest(request: &PreparedImageRequest) -> Result<String, ApplicationError> {
    let assets = request
        .assets
        .iter()
        .map(|asset| {
            serde_json::json!({
                "path": asset.native_parameter_path,
                "position": asset.position,
                "media_type": asset.media_type,
                "sha256": asset.sha256,
            })
        })
        .collect::<Vec<_>>();
    let mut value = serde_json::json!({
        "provider_model_id": request.provider_model_id,
        "branch": request.branch,
        "native_parameters": request.native_parameters,
        "assets": assets,
    });
    canonicalize_json(&mut value);
    let bytes = serde_json::to_vec(&value)
        .map_err(|error| ApplicationError::Validation(error.to_string()))?;
    Ok(sha256_hex(&bytes))
}

fn canonicalize_json(value: &mut Value) {
    match value {
        Value::Object(object) => {
            let original = std::mem::take(object);
            let mut sorted = BTreeMap::new();
            for (key, mut child) in original {
                canonicalize_json(&mut child);
                sorted.insert(key, child);
            }
            object.extend(sorted);
        }
        Value::Array(items) => items.iter_mut().for_each(canonicalize_json),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn failure_from_adapter(error: AdapterError) -> AttemptFailure {
    match error {
        AdapterError::Provider(provider) => AttemptFailure {
            public_code: public_error_code(provider.kind, provider.retry_safety),
            provider_code: provider.code,
            message: provider.message,
            trace_id: provider.trace_id,
            kind: provider.kind,
            target_state: if provider.retry_safety == RetrySafety::AcceptanceUnknown {
                JobState::ReconciliationRequired
            } else {
                JobState::Failed
            },
            hold_disposition: if provider.retry_safety == RetrySafety::AcceptanceUnknown {
                HoldDisposition::RetainForReconciliation
            } else {
                HoldDisposition::Release
            },
        },
        AdapterError::Configuration(message) | AdapterError::UnsupportedInput(message) => {
            AttemptFailure {
                provider_code: "adapter_rejected".to_owned(),
                public_code: PublicErrorCode::PlatformUnavailable,
                message,
                trace_id: None,
                kind: ProviderFailureKind::PlatformInternal,
                target_state: JobState::Failed,
                hold_disposition: HoldDisposition::Release,
            }
        }
    }
}

fn validate_input_media(media_type: &str, bytes: &[u8]) -> Result<(), ApplicationError> {
    let valid = match media_type {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "image/webp" => bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
        _ => false,
    };
    if !valid {
        return Err(ApplicationError::Validation(
            "media type does not match file signature".to_owned(),
        ));
    }
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(ApplicationError::Validation(
            "asset exceeds 16 MiB".to_owned(),
        ));
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use seeai_adapter_sdk::{GeneratedImage, ProviderCallError};
    use seeai_domain::{
        ChannelId, OfferingId, PricePlanId, PriceSnapshot, RuntimeRevisionId, TokenUsage,
        VendorModelId,
    };
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    fn offering() -> PublishedOffering {
        PublishedOffering {
            runtime_revision_id: RuntimeRevisionId::new(),
            vendor_model_id: VendorModelId::new(),
            offering_id: OfferingId::new(),
            channel_id: ChannelId::new(),
            native_model_id: "gpt-image-2".to_owned(),
            native_revision: "2026-04-21".to_owned(),
            capability_schema: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["model", "prompt"],
                "properties": {
                    "model": {"const": "gpt-image-2"},
                    "prompt": {"type": "string", "minLength": 1},
                    "image": {"type": "string"},
                    "mask": {"type": "string"}
                }
            }),
            restrictions: serde_json::json!({
                "allowed_branches": ["prompt_only", "image_conditioned", "masked"],
                "max_images": 1
            }),
            adapter_key: "aihubmix-image-v1".to_owned(),
            provider_model_id: "gpt-image-2".to_owned(),
            provider_kind: "AIHubMix".to_owned(),
            base_url: "https://api.inferera.com".to_owned(),
            credential_env: "AIHUBMIX_API_KEY".to_owned(),
            price_snapshot: PriceSnapshot {
                price_plan_id: PricePlanId::new(),
                rates: PriceRates {
                    currency: "USD".to_owned(),
                    text_input_microusd_per_million: 5_000_000,
                    image_input_microusd_per_million: 8_000_000,
                    text_output_microusd_per_million: 10_000_000,
                    image_output_microusd_per_million: 30_000_000,
                },
                captured_at: Utc::now(),
            },
        }
    }

    fn schema(model: &str) -> Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["model", "prompt"],
            "properties": {
                "model": {"const": model},
                "prompt": {"type": "string", "minLength": 1}
            }
        })
    }

    fn base_command() -> PublishRuntimeCommand {
        PublishRuntimeCommand {
            vendor_id: "OpenAI".to_owned(),
            native_model_id: "gpt-image-2.5-flare".to_owned(),
            native_revision: "test-1".to_owned(),
            capability_schema: None,
            restrictions: serde_json::json!({}),
            provider_kind: None,
            adapter_key: None,
            provider_model_id: None,
            base_url: None,
            credential_env: None,
            offerings: None,
            price_plan: None,
            actor: "tester".to_owned(),
        }
    }

    fn price_plan() -> PricePlanDraft {
        PricePlanDraft {
            formula: "token_rates".to_owned(),
            currency: "USD".to_owned(),
            text_input_microusd_per_million: 5_000_000,
            image_input_microusd_per_million: 8_000_000,
            text_output_microusd_per_million: 10_000_000,
            image_output_microusd_per_million: 30_000_000,
            source_url: "https://example.invalid/price".to_owned(),
        }
    }

    fn draft(provider_model_id: &str) -> OfferingDraft {
        OfferingDraft {
            provider_kind: "AIHubMix".to_owned(),
            adapter_key: "aihubmix-image-v1".to_owned(),
            provider_model_id: provider_model_id.to_owned(),
            base_url: "https://api.inferera.com".to_owned(),
            credential_env: "AIHUBMIX_API_KEY".to_owned(),
            restrictions: serde_json::json!({}),
            capability_schema: Some(schema("gpt-image-2.5-flare")),
            price_plan: Some(price_plan()),
        }
    }

    #[test]
    fn normalize_rejects_an_empty_offering_array() {
        let command = PublishRuntimeCommand {
            offerings: Some(Vec::new()),
            ..base_command()
        };
        let error = command
            .normalize()
            .expect_err("empty array must be rejected");
        assert!(error.to_string().contains("must not be empty"), "{error}");
    }

    #[test]
    fn normalize_assigns_priority_from_array_index() {
        let command = PublishRuntimeCommand {
            offerings: Some(vec![draft("pm-a"), draft("pm-b"), draft("pm-c")]),
            ..base_command()
        };
        let normalized = command.normalize().expect("array form is valid");
        assert_eq!(normalized.len(), 3);
        // 优先级只有一个来源：数组下标。
        assert_eq!(
            normalized
                .iter()
                .map(|offering| offering.routing_priority)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(normalized[1].provider_model_id, "pm-b");
    }

    #[test]
    fn normalize_flat_form_equals_a_single_zero_priority_offering() {
        let command = PublishRuntimeCommand {
            capability_schema: Some(schema("gpt-image-2.5-flare")),
            provider_kind: Some("AIHubMix".to_owned()),
            adapter_key: Some("aihubmix-image-v1".to_owned()),
            provider_model_id: Some("gpt-image-2.5-flare".to_owned()),
            base_url: Some("https://api.inferera.com".to_owned()),
            credential_env: Some("AIHUBMIX_API_KEY".to_owned()),
            price_plan: Some(price_plan()),
            ..base_command()
        };
        let normalized = command.normalize().expect("flat form is valid");
        assert_eq!(normalized.len(), 1);
        assert_eq!(normalized[0].routing_priority, 0);
    }

    #[test]
    fn normalize_rejects_mixing_array_and_flat_forms() {
        let command = PublishRuntimeCommand {
            capability_schema: Some(schema("gpt-image-2.5-flare")),
            provider_kind: Some("AIHubMix".to_owned()),
            offerings: Some(vec![draft("pm-a")]),
            ..base_command()
        };
        let error = command
            .normalize()
            .expect_err("mixing both forms must be rejected");
        assert!(error.to_string().contains("must be omitted"), "{error}");
    }

    #[test]
    fn normalize_requires_every_flat_field_when_offerings_absent() {
        let command = base_command();
        let error = command
            .normalize()
            .expect_err("flat form without fields must be rejected");
        assert!(error.to_string().contains("is required"), "{error}");
    }

    #[test]
    fn normalize_requires_capability_schema_and_price_plan_per_offering() {
        let mut without_schema = draft("pm-a");
        without_schema.capability_schema = None;
        let command = PublishRuntimeCommand {
            offerings: Some(vec![without_schema]),
            ..base_command()
        };
        let error = command.normalize().expect_err("schema is required");
        assert!(error.to_string().contains("capability_schema"), "{error}");

        let mut without_price = draft("pm-a");
        without_price.price_plan = None;
        let command = PublishRuntimeCommand {
            offerings: Some(vec![without_price]),
            ..base_command()
        };
        let error = command.normalize().expect_err("price plan is required");
        assert!(error.to_string().contains("price_plan"), "{error}");
    }

    /// 造一个用于兼容性校验的候选：Profile 只声明给定的字段。
    fn offering_with(schema_properties: Value, restrictions: Value) -> NormalizedOffering {
        NormalizedOffering {
            capability_schema: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["model", "prompt"],
                "properties": schema_properties
            }),
            restrictions,
            provider_kind: "AIHubMix".to_owned(),
            adapter_key: "aihubmix-image-v1".to_owned(),
            provider_model_id: "m".to_owned(),
            base_url: "https://api.inferera.com".to_owned(),
            credential_env: "AIHUBMIX_API_KEY".to_owned(),
            rates: PriceRates {
                currency: "USD".to_owned(),
                text_input_microusd_per_million: 5_000_000,
                image_input_microusd_per_million: 8_000_000,
                text_output_microusd_per_million: 10_000_000,
                image_output_microusd_per_million: 30_000_000,
            },
            price_source_url: "https://example.invalid/price".to_owned(),
            routing_priority: 0,
        }
    }

    #[test]
    fn restriction_declaring_an_undeclared_branch_is_rejected() {
        // 反例一：Profile 只声明 prompt，却把 image_conditioned 放进允许分支。
        // 这是"限制放宽"——供货方声明了 Profile 自己都没声明的东西。
        let offering = offering_with(
            serde_json::json!({"model": {"const": "m"}, "prompt": {"type": "string"}}),
            serde_json::json!({"allowed_branches": ["prompt_only", "image_conditioned"]}),
        );
        let error = validate_restrictions_within_profile(&offering)
            .expect_err("an undeclared branch must be rejected");
        assert!(error.to_string().contains("does not declare"), "{error}");
    }

    #[test]
    fn restriction_allowing_more_images_than_declared_is_rejected() {
        // 反例二：Profile 只声明一张参考图，限制却允许 4 张。
        let offering = offering_with(
            serde_json::json!({
                "model": {"const": "m"},
                "prompt": {"type": "string"},
                "images": {"type": "array", "maxItems": 1}
            }),
            serde_json::json!({"allowed_branches": ["image_conditioned"], "max_images": 4}),
        );
        let error = validate_restrictions_within_profile(&offering)
            .expect_err("more images than declared must be rejected");
        assert!(error.to_string().contains("declares at most"), "{error}");
    }

    #[test]
    fn restriction_staying_within_the_profile_is_accepted() {
        // 正例：收窄（声明 image_conditioned/masked 且真的有对应字段；收图数不超过声明）。
        let offering = offering_with(
            serde_json::json!({
                "model": {"const": "m"},
                "prompt": {"type": "string"},
                "images": {"type": "array", "maxItems": 4},
                "mask": {"type": "string"}
            }),
            serde_json::json!({
                "allowed_branches": ["image_conditioned", "masked"],
                "max_images": 2
            }),
        );
        assert!(validate_restrictions_within_profile(&offering).is_ok());
    }

    #[test]
    fn restriction_cannot_allow_edits_when_only_one_image_is_supported() {
        // 收窄的另一面：Profile 只有单图字段时，`max_images: 1` 合法、`2` 不合法。
        let single = offering_with(
            serde_json::json!({
                "model": {"const": "m"},
                "prompt": {"type": "string"},
                "image": {"type": "string"}
            }),
            serde_json::json!({"allowed_branches": ["image_conditioned"], "max_images": 1}),
        );
        assert!(validate_restrictions_within_profile(&single).is_ok());
        let widened = offering_with(
            serde_json::json!({
                "model": {"const": "m"},
                "prompt": {"type": "string"},
                "image": {"type": "string"}
            }),
            serde_json::json!({"allowed_branches": ["image_conditioned"], "max_images": 2}),
        );
        assert!(validate_restrictions_within_profile(&widened).is_err());
    }

    #[test]
    fn restriction_recognises_image_and_mask_parameters_by_name() {
        // 参考图/遮罩参数按渠道各自的原生名给出：APIMart 的参考图字段叫 `image_urls`、
        // 遮罩叫 `mask_url`。判定办法是名字约定——参考图以 `image` 开头，遮罩含 `mask`。
        let vendor_names = offering_with(
            serde_json::json!({
                "model": {"const": "m"},
                "prompt": {"type": "string"},
                "image_urls": {"type": "array", "items": {"type": "string"}, "maxItems": 16},
                "mask_url": {"type": "string"}
            }),
            serde_json::json!({
                "allowed_branches": ["prompt_only", "image_conditioned", "masked"],
                "max_images": 16
            }),
        );
        assert!(validate_restrictions_within_profile(&vendor_names).is_ok());
        // 声明了遮罩却没有任何参考图参数：`masked` 不成立（遮罩不能脱离参考图）。
        let mask_without_image = offering_with(
            serde_json::json!({
                "model": {"const": "m"},
                "prompt": {"type": "string"},
                "mask_url": {"type": "string"}
            }),
            serde_json::json!({"allowed_branches": ["masked"]}),
        );
        assert!(validate_restrictions_within_profile(&mask_without_image).is_err());
        // 数组形式没写 `maxItems` ＝ Profile 没有承诺上限，不能据它接受 `max_images`。
        let unbounded = offering_with(
            serde_json::json!({
                "model": {"const": "m"},
                "prompt": {"type": "string"},
                "image_urls": {"type": "array", "items": {"type": "string"}}
            }),
            serde_json::json!({"allowed_branches": ["image_conditioned"], "max_images": 16}),
        );
        assert!(validate_restrictions_within_profile(&unbounded).is_err());
    }

    #[test]
    fn normalize_rejects_unsupported_price_formula() {
        // 本阶段只启用 token_rates；其余计价形态（例如按上游声明金额计价）必须显式拒绝，
        // 而不是静默落库成一个它并不支持的计价形态。
        let mut draft = draft("pm-a");
        draft.price_plan = Some(PricePlanDraft {
            formula: "upstream_charge".to_owned(),
            ..price_plan()
        });
        let command = PublishRuntimeCommand {
            offerings: Some(vec![draft]),
            ..base_command()
        };
        let error = command.normalize().expect_err("unknown formula must fail");
        assert!(error.to_string().contains("token_rates"), "{error}");
    }

    #[test]
    fn normalize_rejects_unsupported_formula_in_flat_form() {
        let command = PublishRuntimeCommand {
            capability_schema: Some(schema("gpt-image-2.5-flare")),
            provider_kind: Some("AIHubMix".to_owned()),
            adapter_key: Some("aihubmix-image-v1".to_owned()),
            provider_model_id: Some("gpt-image-2.5-flare".to_owned()),
            base_url: Some("https://api.inferera.com".to_owned()),
            credential_env: Some("AIHUBMIX_API_KEY".to_owned()),
            price_plan: Some(PricePlanDraft {
                formula: "amount_only".to_owned(),
                ..price_plan()
            }),
            ..base_command()
        };
        let error = command.normalize().expect_err("unknown formula must fail");
        assert!(error.to_string().contains("token_rates"), "{error}");
    }

    /// 一条最小的对客请求（文生图）；参考图与遮罩由各用例自己加。
    fn image_request(parameters: Value) -> CreateImageGenerationRequest {
        CreateImageGenerationRequest {
            account_id: AccountId::new(),
            model: "gpt-image-2".to_owned(),
            native_parameters: parameters,
            image_asset_ids: Vec::new(),
            mask_asset_id: None,
            idempotency_key: "request-0001".to_owned(),
        }
    }

    #[test]
    fn validates_prompt_only_native_request() {
        let request = image_request(serde_json::json!({"prompt": "hello"}));
        assert!(validate_native_request(&request, &offering(), &[]).is_ok());
    }

    #[test]
    fn injects_image_binding_before_schema_validation() {
        let mut request = image_request(serde_json::json!({"prompt": "hello"}));
        request.image_asset_ids.push(AssetId::new());
        let bindings = bind_assets(&offering().capability_schema, &request).expect("image binding");
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].native_parameter_path, "/image");
        assert!(validate_native_request(&request, &offering(), &bindings).is_ok());
    }

    #[test]
    fn injects_array_bindings_into_the_vendors_own_array_parameter() {
        // 调用方只给 image/mask；装到 `image_urls` / `mask_url` 是平台按候选声明做的映射。
        let mut vendor = offering();
        vendor.capability_schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["model", "prompt"],
            "properties": {
                "model": {"const": "gpt-image-2"},
                "prompt": {"type": "string", "minLength": 1},
                "image_urls": {"type": "array", "items": {"type": "string"}, "minItems": 1},
                "mask_url": {"type": "string"}
            }
        });
        let mut request = image_request(serde_json::json!({"prompt": "hello"}));
        request.image_asset_ids.push(AssetId::new());
        request.mask_asset_id = Some(AssetId::new());
        let bindings = bind_assets(&vendor.capability_schema, &request).expect("bindings");
        let paths = bindings
            .iter()
            .map(|binding| binding.native_parameter_path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(paths, vec!["/image_urls/0", "/mask_url"]);
        assert!(validate_native_request(&request, &vendor, &bindings).is_ok());

        // 候选的参数面里没有装参考图的参数：这个候选表达不了，直接不合格。
        let mut text_only = offering();
        text_only.capability_schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["model", "prompt"],
            "properties": {
                "model": {"const": "gpt-image-2"},
                "prompt": {"type": "string", "minLength": 1}
            }
        });
        assert!(bind_assets(&text_only.capability_schema, &request).is_err());
    }

    #[test]
    fn mask_without_an_image_is_rejected() {
        let mut request = image_request(serde_json::json!({"prompt": "hello"}));
        request.mask_asset_id = Some(AssetId::new());
        assert!(request.branch().is_err());
    }

    #[test]
    fn canonical_request_hash_ignores_object_key_order() {
        let first = image_request(serde_json::json!({"prompt":"x", "n":1}));
        let mut second = first.clone();
        second.native_parameters =
            serde_json::from_str(r#"{"n":1,"prompt":"x"}"#).expect("fixture should parse");
        assert_eq!(
            request_hash(&first).expect("hash"),
            request_hash(&second).expect("hash")
        );
    }

    struct WorkerRepository {
        job: Mutex<Option<GenerationJob>>,
        completion: Mutex<Option<CompleteJob>>,
        failure: Mutex<Option<AttemptFailure>>,
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl WorkerRepository {
        fn new(job: GenerationJob, events: Arc<Mutex<Vec<&'static str>>>) -> Self {
            Self {
                job: Mutex::new(Some(job)),
                completion: Mutex::new(None),
                failure: Mutex::new(None),
                events,
            }
        }
    }

    fn unused_repository<T>() -> Result<T, ApplicationError> {
        Err(ApplicationError::Persistence(
            "unused repository operation in worker test".to_owned(),
        ))
    }

    #[async_trait]
    impl HubRepository for WorkerRepository {
        async fn publish_runtime(
            &self,
            _request: PublishRuntimeRequest,
        ) -> Result<PublishedRevision, ApplicationError> {
            unused_repository()
        }

        async fn active_offering(
            &self,
            _native_model_id: &str,
        ) -> Result<Vec<OfferingCandidate>, ApplicationError> {
            unused_repository()
        }

        async fn create_account(
            &self,
            _account_id: AccountId,
            _initial_credit_microusd: u64,
            _actor: &str,
        ) -> Result<(), ApplicationError> {
            unused_repository()
        }

        async fn credit_account(
            &self,
            _account_id: AccountId,
            _amount_microusd: u64,
            _business_key: &str,
            _actor: &str,
        ) -> Result<(), ApplicationError> {
            unused_repository()
        }

        async fn create_api_key(
            &self,
            _account_id: AccountId,
            _label: &str,
            _key_hash: &str,
            _actor: &str,
        ) -> Result<(), ApplicationError> {
            unused_repository()
        }

        async fn account_for_api_key(
            &self,
            _key_hash: &str,
        ) -> Result<AccountId, ApplicationError> {
            unused_repository()
        }

        async fn insert_asset(&self, _asset: AssetRecord) -> Result<(), ApplicationError> {
            unused_repository()
        }

        async fn get_asset(
            &self,
            _account_id: AccountId,
            _asset_id: AssetId,
        ) -> Result<AssetRecord, ApplicationError> {
            unused_repository()
        }

        async fn create_job(
            &self,
            _command: CreateImageGeneration,
            _branch: ImageBranch,
            _offering: PublishedOffering,
            _request_hash: String,
            _routing: RoutingDecision,
        ) -> Result<GenerationJob, ApplicationError> {
            unused_repository()
        }

        async fn get_job(
            &self,
            _account_id: AccountId,
            _job_id: JobId,
        ) -> Result<JobView, ApplicationError> {
            unused_repository()
        }

        async fn claim_next_job(
            &self,
            worker_id: &str,
            lease_duration: ChronoDuration,
        ) -> Result<Option<ClaimedJob>, ApplicationError> {
            let job = self
                .job
                .lock()
                .map_err(|error| ApplicationError::Persistence(error.to_string()))?
                .take();
            Ok(job.map(|job| ClaimedJob {
                job,
                lease_owner: worker_id.to_owned(),
                lease_expires_at: Utc::now() + lease_duration,
            }))
        }

        async fn recover_expired_leases(&self) -> Result<LeaseRecovery, ApplicationError> {
            Ok(LeaseRecovery::default())
        }

        async fn begin_attempt(
            &self,
            _job_id: JobId,
            _worker_id: &str,
            _attempt_id: AttemptId,
            _request_digest: &str,
        ) -> Result<(), ApplicationError> {
            Ok(())
        }

        async fn renew_lease(
            &self,
            _job_id: JobId,
            _worker_id: &str,
            _lease_duration: ChronoDuration,
        ) -> Result<(), ApplicationError> {
            Ok(())
        }

        async fn complete_job(&self, completion: CompleteJob) -> Result<(), ApplicationError> {
            self.events
                .lock()
                .map_err(|error| ApplicationError::Persistence(error.to_string()))?
                .push("complete");
            *self
                .completion
                .lock()
                .map_err(|error| ApplicationError::Persistence(error.to_string()))? =
                Some(completion);
            Ok(())
        }

        async fn fail_job(
            &self,
            _job_id: JobId,
            _worker_id: &str,
            _attempt_id: Option<AttemptId>,
            failure: AttemptFailure,
        ) -> Result<(), ApplicationError> {
            *self
                .failure
                .lock()
                .map_err(|error| ApplicationError::Persistence(error.to_string()))? = Some(failure);
            Ok(())
        }

        async fn provider_failures(
            &self,
            _query: ProviderFailureQuery,
        ) -> Result<Vec<ProviderFailureView>, ApplicationError> {
            Ok(Vec::new())
        }

        async fn list_open_reconciliation_cases(
            &self,
        ) -> Result<Vec<ReconciliationCaseView>, ApplicationError> {
            unused_repository()
        }

        async fn refund_reconciliation(
            &self,
            _command: RefundReconciliationCommand,
        ) -> Result<(), ApplicationError> {
            unused_repository()
        }
    }

    struct WorkerStore {
        events: Arc<Mutex<Vec<&'static str>>>,
        keys: Mutex<Vec<String>>,
        /// 置 true 时 `put` 失败，用于构造"已确认生成但归档失败"的路径。
        fail_put: bool,
    }

    #[async_trait]
    impl AssetStore for WorkerStore {
        async fn put(
            &self,
            object_key: &str,
            _bytes: Bytes,
            _media_type: &str,
        ) -> Result<(), ApplicationError> {
            self.events
                .lock()
                .map_err(|error| ApplicationError::ObjectStorage(error.to_string()))?
                .push("store");
            if self.fail_put {
                return Err(ApplicationError::ObjectStorage(
                    "archive unavailable in worker test".to_owned(),
                ));
            }
            self.keys
                .lock()
                .map_err(|error| ApplicationError::ObjectStorage(error.to_string()))?
                .push(object_key.to_owned());
            Ok(())
        }

        async fn get(&self, _object_key: &str) -> Result<Bytes, ApplicationError> {
            Err(ApplicationError::ObjectStorage(
                "unused object read in worker test".to_owned(),
            ))
        }
    }

    struct WorkerAdapter {
        succeeds: bool,
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ImageAdapter for WorkerAdapter {
        fn key(&self) -> &'static str {
            "aihubmix-image-v1"
        }

        async fn execute(
            &self,
            _request: PreparedImageRequest,
            _credential: &ProviderCredential,
        ) -> Result<ProviderSuccess, AdapterError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if !self.succeeds {
                return Err(ProviderCallError {
                    code: "provider_response_invalid".to_owned(),
                    message: "missing verified usage".to_owned(),
                    trace_id: None,
                    retry_safety: RetrySafety::AcceptanceUnknown,
                    kind: ProviderFailureKind::PlatformInternal,
                }
                .into());
            }
            let bytes = Bytes::from(
                hex::decode("89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4890000000d4944415478da6364f8cff01f0005fe02fe5dc638590000000049454e44ae426082")
                    .expect("PNG fixture must decode"),
            );
            Ok(ProviderSuccess {
                images: vec![GeneratedImage {
                    media_type: "image/png".to_owned(),
                    sha256: sha256_hex(&bytes),
                    bytes,
                }],
                usage: TokenUsage {
                    input_tokens: 9,
                    input_text_tokens: 9,
                    input_image_tokens: 0,
                    output_tokens: 196,
                    output_text_tokens: 0,
                    output_image_tokens: 196,
                    total_tokens: 205,
                },
                response_digest: "provider-response-digest".to_owned(),
                provider_trace_id: Some("provider-request-1".to_owned()),
            })
        }
    }

    struct WorkerAdapterFactory {
        adapter: Arc<WorkerAdapter>,
    }

    impl AdapterFactory for WorkerAdapterFactory {
        fn descriptor(&self, _adapter_key: &str) -> Option<AdapterDescriptor> {
            None
        }

        fn validate_publication(
            &self,
            _adapter_key: &str,
            _capability_schema: &Value,
            _restrictions: &Value,
        ) -> Result<(), String> {
            Ok(())
        }

        fn create(
            &self,
            _adapter_key: &str,
            _base_url: &str,
            _timeout: Duration,
        ) -> Result<Arc<dyn ImageAdapter>, ApplicationError> {
            Ok(self.adapter.clone())
        }
    }

    struct WorkerCredentialProvider;

    impl CredentialProvider for WorkerCredentialProvider {
        fn resolve(&self, _reference: &str) -> Result<ProviderCredential, ApplicationError> {
            ProviderCredential::new("test-credential".to_owned())
                .map_err(|error| ApplicationError::Configuration(error.to_string()))
        }
    }

    fn worker_job() -> GenerationJob {
        GenerationJob {
            id: JobId::new(),
            account_id: AccountId::new(),
            state: JobState::Leased,
            branch: ImageBranch::PromptOnly,
            native_model_id: "gpt-image-2".to_owned(),
            native_parameters: serde_json::json!({"prompt": "worker contract"}),
            asset_bindings: Vec::new(),
            offering: offering(),
            idempotency_key: "worker-contract-1".to_owned(),
            request_hash: "request-hash".to_owned(),
            max_cost_microusd: 20_000,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn worker(
        repository: Arc<WorkerRepository>,
        store: Arc<WorkerStore>,
        adapter: Arc<WorkerAdapter>,
    ) -> WorkerService {
        WorkerService::new(
            repository,
            store,
            Arc::new(WorkerAdapterFactory { adapter }),
            Arc::new(WorkerCredentialProvider),
            "worker-test".to_owned(),
            ChronoDuration::seconds(30),
            Duration::from_secs(1),
        )
        .expect("worker fixture must be valid")
    }

    #[tokio::test]
    async fn worker_archives_output_before_evidence_settlement() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let repository = Arc::new(WorkerRepository::new(worker_job(), events.clone()));
        let store = Arc::new(WorkerStore {
            events: events.clone(),
            keys: Mutex::new(Vec::new()),
            fail_put: false,
        });
        let adapter = Arc::new(WorkerAdapter {
            succeeds: true,
            calls: AtomicUsize::new(0),
        });

        assert!(
            worker(repository.clone(), store, adapter.clone())
                .run_once()
                .await
                .expect("worker run must succeed")
        );
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            *events.lock().expect("event lock"),
            vec!["store", "complete"]
        );
        let completion = repository
            .completion
            .lock()
            .expect("completion lock")
            .take()
            .expect("job must complete");
        assert_eq!(completion.evidence.usage.total_tokens, 205);
        assert_eq!(completion.charge_microusd, 5_925);
        assert_eq!(completion.outputs.len(), 1);
        assert!(repository.failure.lock().expect("failure lock").is_none());
    }

    #[tokio::test]
    async fn worker_sends_ambiguous_provider_response_to_reconciliation_once() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let repository = Arc::new(WorkerRepository::new(worker_job(), events.clone()));
        let store = Arc::new(WorkerStore {
            events,
            keys: Mutex::new(Vec::new()),
            fail_put: false,
        });
        let adapter = Arc::new(WorkerAdapter {
            succeeds: false,
            calls: AtomicUsize::new(0),
        });

        assert!(
            worker(repository.clone(), store, adapter.clone())
                .run_once()
                .await
                .expect("worker run must converge")
        );
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 1);
        let failure = repository
            .failure
            .lock()
            .expect("failure lock")
            .take()
            .expect("job must fail to reconciliation");
        assert_eq!(failure.target_state, JobState::ReconciliationRequired);
        assert_eq!(
            failure.hold_disposition,
            HoldDisposition::RetainForReconciliation
        );
        assert!(
            repository
                .completion
                .lock()
                .expect("completion lock")
                .is_none()
        );
    }

    /// 第二种对账：**已确认生成、但归档失败**。
    ///
    /// 与第一种（创建阶段失联）的区别在这条路径上体现为**错误码不同**：
    /// 这里是 `result_delivery_failed`，而创建阶段失联用 adapter 报的错误码。
    /// 两者都进对账并保留预授权，但性质可分。
    #[tokio::test]
    async fn worker_sends_delivery_failure_to_reconciliation_with_its_own_code() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let repository = Arc::new(WorkerRepository::new(worker_job(), events.clone()));
        // 上游成功，但归档不可用——生成已经发生，因此只能对账，不能当失败。
        let store = Arc::new(WorkerStore {
            events,
            keys: Mutex::new(Vec::new()),
            fail_put: true,
        });
        let adapter = Arc::new(WorkerAdapter {
            succeeds: true,
            calls: AtomicUsize::new(0),
        });

        assert!(
            worker(repository.clone(), store, adapter.clone())
                .run_once()
                .await
                .expect("worker run must converge")
        );
        assert_eq!(
            adapter.calls.load(Ordering::SeqCst),
            1,
            "the provider was called exactly once; delivery failure must not retry it"
        );
        let failure = repository
            .failure
            .lock()
            .expect("failure lock")
            .take()
            .expect("delivery failure must be recorded");
        assert_eq!(
            failure.provider_code, "result_delivery_failed",
            "the delivery failure must carry its own code, distinct from acceptance-unknown"
        );
        assert_eq!(
            failure.public_code,
            PublicErrorCode::OutcomeUnknown,
            "the result already exists, so the consumer must wait for the reconciliation outcome"
        );
        assert_eq!(failure.kind, ProviderFailureKind::PlatformInternal);
        assert_eq!(failure.target_state, JobState::ReconciliationRequired);
        assert_eq!(
            failure.hold_disposition,
            HoldDisposition::RetainForReconciliation,
            "a generated result must keep the hold for reconciliation"
        );
        assert!(
            repository
                .completion
                .lock()
                .expect("completion lock")
                .is_none(),
            "no settlement may happen when the result could not be archived"
        );
    }

    /// 渠道侧的失败一律说成平台侧故障；只有消费者内容被拒才是消费者的错。
    #[test]
    fn channel_failures_are_reported_as_platform_problems() {
        // (场景, 类别, `retry_safety`, 对客码)
        let rows = [
            (
                "渠道账户欠费：AIHubMix 403 insufficient_user_quota / APIMart 402 payment_required",
                ProviderFailureKind::PlatformFunding,
                RetrySafety::NotRetryable,
                PublicErrorCode::PlatformUnavailable,
            ),
            (
                "渠道侧凭证、权限或渠道被禁用：AIHubMix 403 的其余分支",
                ProviderFailureKind::PlatformCredential,
                RetrySafety::NotRetryable,
                PublicErrorCode::PlatformUnavailable,
            ),
            (
                "渠道用 500 承载的参数错误：APIMart 500 build_request_failed",
                ProviderFailureKind::UpstreamRejected,
                RetrySafety::NotRetryable,
                PublicErrorCode::PlatformUnavailable,
            ),
            (
                "渠道限流",
                ProviderFailureKind::UpstreamRateLimited,
                RetrySafety::SafeBeforeAcceptance,
                PublicErrorCode::PlatformUnavailable,
            ),
            (
                "渠道 5xx、网络中断或响应形状不可用",
                ProviderFailureKind::UpstreamUnavailable,
                RetrySafety::SafeBeforeAcceptance,
                PublicErrorCode::PlatformUnavailable,
            ),
            (
                "拿不准的渠道失败",
                ProviderFailureKind::Unknown,
                RetrySafety::NotRetryable,
                PublicErrorCode::PlatformUnavailable,
            ),
            (
                "受理状态不明：APIMart 409 idempotency_result_indeterminate",
                ProviderFailureKind::Unknown,
                RetrySafety::AcceptanceUnknown,
                PublicErrorCode::OutcomeUnknown,
            ),
            (
                "渠道按幂等子类拒了平台的请求：APIMart 409 idempotency_in_progress / key_reused",
                ProviderFailureKind::UpstreamRejected,
                RetrySafety::AcceptanceUnknown,
                PublicErrorCode::OutcomeUnknown,
            ),
            (
                "渠道不可用且拿不准是否已受理",
                ProviderFailureKind::UpstreamUnavailable,
                RetrySafety::AcceptanceUnknown,
                PublicErrorCode::OutcomeUnknown,
            ),
            (
                "消费者内容被渠道拒绝",
                ProviderFailureKind::ConsumerContent,
                RetrySafety::NotRetryable,
                PublicErrorCode::ContentRejected,
            ),
            (
                "平台自己的问题：租约过期、结果交付失败、配置错误",
                ProviderFailureKind::PlatformInternal,
                RetrySafety::AcceptanceUnknown,
                PublicErrorCode::OutcomeUnknown,
            ),
        ];
        for (scenario, kind, retry_safety, expected) in rows {
            assert_eq!(
                public_error_code(kind, retry_safety),
                expected,
                "场景的对客码不符：{scenario}"
            );
        }
    }

    /// 对客码只有三个取值：新增类别或新增 `retry_safety` 都不能让第四个值溜出去。
    #[test]
    fn every_failure_kind_stays_inside_the_public_error_whitelist() {
        let safeties = [
            RetrySafety::SafeBeforeAcceptance,
            RetrySafety::NotRetryable,
            RetrySafety::AcceptanceUnknown,
        ];
        for kind in ProviderFailureKind::ALL {
            for retry_safety in safeties {
                let code = public_error_code(kind, retry_safety);
                assert!(
                    PublicErrorCode::parse(code.as_str()).is_some(),
                    "{kind:?} 与 {retry_safety:?} 组合出了白名单外的对客码"
                );
                assert!(!code.default_message().is_empty());
            }
        }
    }

    /// 平台内部码走的是同一条白名单：`platform_unavailable`，或"结果不明"时的 `outcome_unknown`。
    ///
    /// 这些码由平台自己产生（不经渠道），因此必须逐个钉住——它们是"平台自己的 bug/运维缺口"
    /// 唯一对外的说法。
    #[test]
    fn platform_internal_codes_stay_inside_the_public_error_whitelist() {
        // (平台内部码, 类别, `retry_safety`)——与各写入点的取值一致。
        let rows = [
            (
                "adapter_rejected",
                ProviderFailureKind::PlatformInternal,
                RetrySafety::NotRetryable,
            ),
            (
                "worker_prepare_failed",
                ProviderFailureKind::PlatformInternal,
                RetrySafety::NotRetryable,
            ),
            (
                "credential_unavailable",
                ProviderFailureKind::PlatformInternal,
                RetrySafety::NotRetryable,
            ),
            (
                "adapter_configuration_failed",
                ProviderFailureKind::PlatformInternal,
                RetrySafety::NotRetryable,
            ),
            (
                "result_delivery_failed",
                ProviderFailureKind::PlatformInternal,
                RetrySafety::AcceptanceUnknown,
            ),
            (
                "worker_lease_expired",
                ProviderFailureKind::PlatformInternal,
                RetrySafety::AcceptanceUnknown,
            ),
            (
                "reconciliation_refunded",
                ProviderFailureKind::PlatformInternal,
                RetrySafety::NotRetryable,
            ),
        ];
        for (code, kind, retry_safety) in rows {
            let public = public_error_code(kind, retry_safety);
            assert!(
                PublicErrorCode::parse(public.as_str()).is_some(),
                "{code} 落到了白名单外：{public:?}"
            );
            assert_ne!(
                public,
                PublicErrorCode::ContentRejected,
                "{code} 是平台自己的问题，不许说成消费者内容被拒"
            );
        }
    }

    /// 不传类别时只列平台侧事件：渠道不可用、被限流、消费者内容被拒都不在内。
    #[test]
    fn the_default_failure_list_covers_only_platform_side_events() {
        assert_eq!(
            default_failure_kinds(),
            vec![
                ProviderFailureKind::PlatformFunding,
                ProviderFailureKind::PlatformCredential,
                ProviderFailureKind::PlatformInternal,
                ProviderFailureKind::UpstreamRejected,
                ProviderFailureKind::Unknown,
            ]
        );
    }

    /// `content_rejected` 专指消费者的内容被拒：别的类别都不许用它。
    #[test]
    fn only_consumer_content_becomes_a_consumer_error() {
        let safeties = [
            RetrySafety::SafeBeforeAcceptance,
            RetrySafety::NotRetryable,
            RetrySafety::AcceptanceUnknown,
        ];
        for retry_safety in safeties {
            for kind in ProviderFailureKind::ALL {
                let code = public_error_code(kind, retry_safety);
                if kind == ProviderFailureKind::ConsumerContent {
                    assert_eq!(code, PublicErrorCode::ContentRejected);
                } else {
                    assert_ne!(code, PublicErrorCode::ContentRejected);
                }
            }
        }
    }

    /// 落库值与类型必须一一对应：数据库的 CHECK 约束用的是同一批字符串。
    #[test]
    fn stored_failure_kinds_and_public_codes_round_trip() {
        for kind in ProviderFailureKind::ALL {
            assert_eq!(ProviderFailureKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(ProviderFailureKind::parse("provider_error"), None);
        for code in [
            PublicErrorCode::PlatformUnavailable,
            PublicErrorCode::OutcomeUnknown,
            PublicErrorCode::ContentRejected,
        ] {
            assert_eq!(PublicErrorCode::parse(code.as_str()), Some(code));
        }
        assert_eq!(PublicErrorCode::parse("402"), None);
    }

    /// 渠道原始码留在内部，对客码独立派生。
    #[test]
    fn adapter_failures_keep_the_channel_code_internal() {
        let failure = failure_from_adapter(AdapterError::Provider(ProviderCallError {
            code: "payment_required".to_owned(),
            message: "account balance is insufficient".to_owned(),
            trace_id: Some("trace-payment".to_owned()),
            retry_safety: RetrySafety::NotRetryable,
            kind: ProviderFailureKind::PlatformFunding,
        }));
        assert_eq!(failure.provider_code, "payment_required");
        assert_eq!(failure.public_code, PublicErrorCode::PlatformUnavailable);
        assert_eq!(failure.kind, ProviderFailureKind::PlatformFunding);
        assert_eq!(failure.target_state, JobState::Failed);
        assert_eq!(failure.hold_disposition, HoldDisposition::Release);

        let unknown = failure_from_adapter(AdapterError::Provider(ProviderCallError {
            code: "idempotency_result_indeterminate".to_owned(),
            message: "the request may already be accepted".to_owned(),
            trace_id: None,
            retry_safety: RetrySafety::AcceptanceUnknown,
            kind: ProviderFailureKind::UpstreamRejected,
        }));
        assert_eq!(unknown.public_code, PublicErrorCode::OutcomeUnknown);
        assert_eq!(unknown.target_state, JobState::ReconciliationRequired);
        assert_eq!(
            unknown.hold_disposition,
            HoldDisposition::RetainForReconciliation
        );

        // 可证明未受理（渠道按第一方依据明确"这次请求没执行"）与确定性拒绝走同一处置：
        // 失败并释放预授权——区别只留在 Attempt 的渠道原始码上。
        let unaccepted = failure_from_adapter(AdapterError::Provider(ProviderCallError {
            code: "429".to_owned(),
            message: "rate_limit_error".to_owned(),
            trace_id: None,
            retry_safety: RetrySafety::SafeBeforeAcceptance,
            kind: ProviderFailureKind::UpstreamRateLimited,
        }));
        assert_eq!(unaccepted.provider_code, "429");
        assert_eq!(unaccepted.public_code, PublicErrorCode::PlatformUnavailable);
        assert_eq!(unaccepted.target_state, JobState::Failed);
        assert_eq!(unaccepted.hold_disposition, HoldDisposition::Release);
    }
}
