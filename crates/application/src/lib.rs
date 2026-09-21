use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use seeai_adapter_sdk::{
    AdapterDescriptor, AdapterError, ImageAdapter, PreparedImageRequest, ProviderCredential,
    ProviderSuccess, RetrySafety,
};
pub use seeai_adapter_sdk::{GeneratedImage, ProviderFailureKind};
use seeai_domain::{
    AccountId, AttemptId, CreateImageGeneration, GenerationJob, ImageBranch, ImageParameterKind,
    JobId, JobState, MeteringEvidence, OfferingCandidate, OfferingId, PriceRates,
    PublishedOffering, PublishedRevision, RuntimeRevisionId, apply_parameter_defaults,
    contract_image_parameter_kind, declared_field_names, declared_parameter_names,
    declared_reference_image_limit, declares_mask_parameter, declares_parameter,
    declares_reference_image_parameter, is_used_parameter_value, place_image_inputs,
    platform_image_parameters,
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
/// 合同是**模型级唯一一份**（[`Self::capability_schema`]）；每个候选各自声明它**能承载**的
/// 字段面（[`OfferingDraft::carrier_schema`]）。
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
    /// **Vendor Model Contract**：调用方合同的唯一一份，模型级。
    ///
    /// 数组形式下可以省略：那时回退用候选自带的旧字段（过渡期里承载面与合同还是同一份），
    /// 但要求它们彼此完全一致——合同只有一份，同一个模型落成两份合同正是要收掉的分叉。
    /// 扁平形式下必填。
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
    /// 扁平形式的单个供给**能承载**的字段面；缺省时与扁平形式的合同同值。
    #[serde(default)]
    pub carrier_schema: Option<Value>,
    /// 扁平形式的合同值 → 渠道包装声明。数组形式下必须为 `None`。
    #[serde(default = "empty_object")]
    pub parameter_mapping: Value,
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
    /// 该模型的调用方合同（模型级唯一一份，落库后不再改）。
    pub capability_schema: Value,
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
    /// 这条供给**能承载**合同里的哪些字段。
    #[serde(default)]
    pub carrier_schema: Option<Value>,
    /// 把合同值转成渠道包装的声明。本阶段只随行落库并随 Job 冻结，映射内容由后续步骤补。
    #[serde(default = "empty_object")]
    pub parameter_mapping: Value,
    /// 承载面的**旧名字**（过渡期）：只在没有 `carrier_schema` 时顶替它。
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
    /// 这条供给**能承载**合同里的哪些字段。
    pub carrier_schema: Value,
    /// 这条供给自己的合同值 → 渠道包装声明（本阶段只携带）。
    pub parameter_mapping: Value,
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

/// 归一后的整份发布：**一份模型级合同** + 有序候选集。
#[derive(Debug, Clone)]
pub struct NormalizedPublication {
    pub contract: Value,
    pub offerings: Vec<NormalizedOffering>,
}

impl PublishRuntimeCommand {
    /// 消费命令，产出已校验的发布请求。
    #[must_use]
    pub fn into_request(
        self,
        capability_schema: Value,
        offerings: Vec<NormalizedOffering>,
    ) -> PublishRuntimeRequest {
        PublishRuntimeRequest {
            vendor_id: self.vendor_id,
            native_model_id: self.native_model_id,
            native_revision: self.native_revision,
            actor: self.actor,
            capability_schema,
            offerings,
        }
    }

    /// 把两种形状归一到"一份合同 + 一个有序候选列表"。
    ///
    /// 这是发布接口**唯一**的形状判别点：`apps/api` 的 `Json<PublishRuntimeCommand>` 反序列化
    /// 之后，下游只处理 [`NormalizedPublication`]。
    pub fn normalize(&self) -> Result<NormalizedPublication, ApplicationError> {
        let offerings = match &self.offerings {
            Some(drafts) => self.normalize_array(drafts)?,
            None => self.normalize_flat()?,
        };
        Ok(NormalizedPublication {
            contract: self.resolve_contract()?,
            offerings,
        })
    }

    /// 解析本次发布的**唯一一份合同**。
    ///
    /// 顶层给了就用顶层；顶层没给才回退到候选自带的旧字段——过渡期里老素材（承载面与合同
    /// 还是同一份）因此照常可发布。回退时要求所有候选的旧字段**完全一致**：合同是模型级的
    /// 唯一一份，两份不同的内容不能同时成为同一个模型的合同，否则"客户端按合同提交"就没了依据。
    fn resolve_contract(&self) -> Result<Value, ApplicationError> {
        if let Some(contract) = &self.capability_schema {
            return Ok(contract.clone());
        }
        let mut resolved: Option<Value> = None;
        for (index, draft) in self
            .offerings
            .as_deref()
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            let Some(legacy) = &draft.capability_schema else {
                return Err(ApplicationError::Validation(format!(
                    "capability_schema is required: declare the vendor model contract at the top level, \
                     or a legacy per-offering capability_schema (offerings[{index}] has neither)"
                )));
            };
            match &resolved {
                None => resolved = Some(legacy.clone()),
                Some(first) if first == legacy => {}
                Some(_) => {
                    return Err(ApplicationError::Validation(
                        "offerings declare different capability schemas; the contract is one per vendor \
                         model, so declare it once at the top level"
                            .to_owned(),
                    ));
                }
            }
        }
        resolved
            .ok_or_else(|| ApplicationError::Validation("capability_schema is required".to_owned()))
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
        if self.flat_restrictions_present() {
            return Err(ApplicationError::Validation(
                "offerings is present, so the flat field restrictions must be omitted".to_owned(),
            ));
        }
        if self.flat_parameter_mapping_present() {
            return Err(ApplicationError::Validation(
                "offerings is present, so the flat field parameter_mapping must be omitted"
                    .to_owned(),
            ));
        }
        drafts
            .iter()
            .enumerate()
            .map(|(index, draft)| {
                // 承载面：新名字优先，缺了才用旧名字顶替（过渡期）。两者都没有就拒绝——
                // 供给说不清自己能承载什么，发布期就没法判它是否落在合同与 Driver 之内。
                let carrier_schema = draft
                    .carrier_schema
                    .clone()
                    .or_else(|| draft.capability_schema.clone())
                    .ok_or_else(|| {
                        ApplicationError::Validation(format!(
                            "offerings[{index}].carrier_schema is required"
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
                    carrier_schema,
                    parameter_mapping: draft.parameter_mapping.clone(),
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
            // 扁平形式只有一个候选：承载面缺省时与合同同值，等价于"这份供给承载合同的全部字段"。
            carrier_schema: self
                .carrier_schema
                .clone()
                .unwrap_or_else(|| capability_schema.clone()),
            parameter_mapping: self.parameter_mapping.clone(),
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
    /// 除上表外还含 `carrier_schema` 与 `price_plan`：它们是"单个供给"的东西，数组形式里
    /// 每个候选自带。**顶层 `capability_schema` 不在此列**——它是模型级合同，数组形式下同样
    /// 允许（也推荐）写在顶层。
    /// `restrictions` 与 `parameter_mapping` 只在**非空**时才算"被给出"：它们带
    /// `#[serde(default)]`，缺省即空对象，无法与显式写 `{}` 区分——而空值不携带信息，
    /// 忽略它没有风险。
    fn first_present_flat_field(&self) -> Option<&'static str> {
        self.flat_fields()
            .into_iter()
            .find_map(|(name, value)| value.is_some().then_some(name))
            .or_else(|| self.carrier_schema.is_some().then_some("carrier_schema"))
            .or_else(|| self.price_plan.is_some().then_some("price_plan"))
    }

    /// `restrictions` 是否被显式给出（见 [`Self::first_present_flat_field`] 的说明）。
    fn flat_restrictions_present(&self) -> bool {
        self.restrictions
            .as_object()
            .is_some_and(|map| !map.is_empty())
    }

    /// `parameter_mapping` 是否被显式给出（判法同上：非空才算）。
    fn flat_parameter_mapping_present(&self) -> bool {
        self.parameter_mapping
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
/// 合格 = 该候选自己的 `restrictions` 允许本次分支与图片张数，**且**这条供给的承载面能承载
/// 这次请求**实际用到**的每个字段（图片要能落到它声明的参数名上）。两个条件都必须用该候选
/// 自己的声明判断——这正是「每条供给各自声明承载面、限制只收窄」的落地方式。
///
/// 请求本身先按**合同**校验一次（缺必填、合同外的字段）：那是调用方的参数问题，与选路无关，
/// 因此在这里直接失败，不进候选取舍。
///
/// **一条候选都不合格时返回 [`ApplicationError::NoEligibleOffering`]**：请求本身没违反合同，
/// 是平台的供给面承载不了它——对客必须表现为平台侧故障，不是参数错。同样在调用上游之前失败，
/// 不回退到能力更宽但优先级更低的候选（候选已经全试过了）。
///
/// 不做的事：不因价格重排候选（价格不参与选中）。
fn select_candidate(
    request: &CreateImageGenerationRequest,
    branch: ImageBranch,
    candidates: &[OfferingCandidate],
) -> Result<(PublishedOffering, Value, RoutingDecision), ApplicationError> {
    if candidates.is_empty() {
        // 该型号没有任何 active 供给 ⇒ 对调用方是"不存在"，不是参数错误。
        return Err(ApplicationError::NotFound(format!(
            "no active offering for model {}",
            request.model
        )));
    }
    let revision_id = candidates[0].runtime_revision_id;
    // 合同是模型级唯一一份，同一型号的候选共享它：请求按合同校验只做一次，与选路无关。
    let contract_parameters = contract_parameter_face(request, &candidates[0].capability_schema)?;
    // 把每个候选连它的取舍结果一起算出来。`considered` 要记录**完整**的取舍画面，
    // 而不是"评估到命中为止"的部分清单——它是判定记录，不是求值轨迹。
    let evaluated: Vec<(PublishedOffering, Value, ConsideredCandidate)> = candidates
        .iter()
        .map(|candidate| {
            let published = candidate.clone().into_published();
            let mut skip_reason = None;
            let mut parameters = Value::Null;
            match prepare_carrier_parameters(&contract_parameters, request, &published) {
                Err(reason) => skip_reason = Some(reason),
                Ok(prepared) => {
                    if let Err(error) = validate_restrictions(
                        branch,
                        request.reference_images.len(),
                        &candidate.restrictions,
                    ) {
                        skip_reason = Some(error.to_string());
                    } else {
                        parameters = prepared;
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
            (published, parameters, considered)
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
        let (published, parameters, _) =
            evaluated.into_iter().nth(chosen).expect("index just found");
        let decision = RoutingDecision {
            runtime_revision_id: revision_id,
            chosen_offering_id: published.offering_id,
            considered,
        };
        return Ok((published, parameters, decision));
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
    Err(ApplicationError::NoEligibleOffering(format!(
        "no offering can carry this request for model {} (revision {revision_id}): {reasons}",
        request.model
    )))
}

/// 对客受理请求：调用方按**合同**给字段，图片直接给公网 URL 或 data URL。
///
/// 这是**接收入口**的形状，与落库的 [`CreateImageGeneration`] 分开：后者的 `native_parameters`
/// 里图片已经落在被选中候选自己的参数名上（`image`、`image_urls`、`mask`、`mask_url`…），
/// 落库与 Worker 只看后者。两者之间的换算就是 Offering Parameter Mapping 的第一块。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateImageGenerationRequest {
    pub account_id: AccountId,
    /// **对外的模型字段**：平台型号名（发布时的型号标识）。它与厂商原生名、
    /// 以及真正发给渠道的模型名是三个分开的角色。
    pub model: String,
    /// 合同里的模型参数（扁平，不再有 `native_parameters` 外壳；图片不走这里）。
    pub native_parameters: Value,
    /// 参考图：调用方给的 `image` / `image_urls`（同义）归一到这里，每项是公网 URL 或 data URL。
    #[serde(default)]
    pub reference_images: Vec<String>,
    /// 遮罩：PNG data URL。
    #[serde(default)]
    pub mask: Option<String>,
    /// 幂等键：来自 `Idempotency-Key` 请求头，缺省时由接口层生成一个。
    pub idempotency_key: String,
}

impl CreateImageGenerationRequest {
    /// 这个请求属于哪条图片分支：有图无遮罩=图生图、两者都有=带遮罩、都没=文生图。
    ///
    /// 只有遮罩没有参考图直接拒绝（遮罩是"编辑范围"，没有可编辑的图没有意义）。
    pub fn branch(&self) -> Result<ImageBranch, ApplicationError> {
        match (self.reference_images.is_empty(), self.mask.is_some()) {
            (true, true) => Err(ApplicationError::Validation(
                "mask requires an input image".to_owned(),
            )),
            (true, false) => Ok(ImageBranch::PromptOnly),
            (false, false) => Ok(ImageBranch::ImageConditioned),
            (false, true) => Ok(ImageBranch::Masked),
        }
    }
}

/// **内部**的 Job 视图：同步门面等终态时读它，管理员面与测试也从这里看结果。
///
/// 它不是对客的异步任务协议——对客只有同步入口，不会拿到 `job_id`，也没有可查询的任务接口；
/// Job 是内部的执行与审计记录（状态机服务于崩溃恢复与运营处置）。
///
/// `data` 只在成功时出现、`error_code` 只在失败时出现——两者不会同时在场。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobView {
    pub job_id: JobId,
    pub state: String,
    pub branch: ImageBranch,
    /// 对外的模型字段：平台型号名。
    pub model: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    /// 当次结果的信封：每项只有渠道给的 `url` 或 `b64_json`，平台不改写、不下载。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Vec<GeneratedImage>>,
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
    /// 当次结果的信封：渠道给什么就是什么。
    pub images: Vec<GeneratedImage>,
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
    /// 平台型号名。
    pub gateway_model: String,
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
    /// 该型号有 active 供给，但**没有一条能承载这次请求**。
    ///
    /// 与 [`Self::Validation`] 分开：请求本身违反合同（缺必填）是调用方的问题；一条候选都
    /// 表达不了这次请求，是平台的供给面不够宽——对客必须说成平台侧故障，不是参数错。
    #[error("no eligible offering: {0}")]
    NoEligibleOffering(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("insufficient balance")]
    InsufficientBalance,
    #[error("too many requests in flight")]
    TooManyInFlight,
    #[error("configuration error: {0}")]
    Configuration(String),
    #[error("persistence error: {0}")]
    Persistence(String),
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
        gateway_model: &str,
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

    /// 该账户当前**在跑**的 Job 数（`accepted`/`leased`/`submitting`）。
    ///
    /// `except_idempotency_key` 那个不算在内：同一个键重发时，`create_job` 会把它去重成
    /// 原来那个 Job，并发上限不该把这个重发拒掉。
    async fn count_in_flight_jobs(
        &self,
        account_id: AccountId,
        except_idempotency_key: &str,
    ) -> Result<u64, ApplicationError>;

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

pub trait AdapterFactory: Send + Sync {
    fn descriptor(&self, adapter_key: &str) -> Option<AdapterDescriptor>;

    /// Driver 侧的发布校验：这份**承载面**（这条供给会往线文里写的字段面）与这些限制，
    /// 本 Driver 能不能执行。
    ///
    /// 看的是承载面而不是合同：合同是客户端那一侧的面，Driver 不据它判自己能否执行。
    fn validate_publication(
        &self,
        adapter_key: &str,
        carrier_schema: &Value,
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
        carrier_schema: &Value,
        restrictions: &Value,
    ) -> Result<(), String> {
        match self.find(adapter_key) {
            Some(factory) => {
                factory.validate_publication(adapter_key, carrier_schema, restrictions)
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
    /// 顺序：形状归一到 [`NormalizedPublication`] → 命令级字段校验 → **合同**校验 →
    /// **逐候选**校验（承载面落在合同与 Driver 之内、base_url、计价、Adapter 兼容性）→
    /// 交给仓库逐项写入。校验不通过时不产生任何 revision 行。
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
        let NormalizedPublication {
            contract,
            offerings,
        } = command.normalize()?;
        validate_contract(&command.native_model_id, &contract)?;
        let mut normalized = Vec::with_capacity(offerings.len());
        for offering in offerings {
            normalized.push(self.validate_offering(&contract, offering)?);
        }
        let request = command.into_request(contract, normalized);
        let revision = self.repository.publish_runtime(request).await?;
        Ok(revision)
    }

    /// 校验单个候选，并归一化它的 `base_url`。
    fn validate_offering(
        &self,
        contract: &Value,
        mut offering: NormalizedOffering,
    ) -> Result<NormalizedOffering, ApplicationError> {
        jsonschema::validator_for(&offering.carrier_schema)
            .map_err(|error| ApplicationError::Validation(error.to_string()))?;
        // 供给不能凭空多出调用方可提交的字段：承载面必须落在合同里。
        validate_carrier_within_contract(contract, &offering.carrier_schema)?;
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
        // 限制只能收窄：供货方不得声明这条供给的承载面自己都没声明的能力。
        validate_restrictions_within_profile(&offering)?;
        // Driver 侧的发布校验看的是**承载面**：这条供给实际会往线文里写的字段面。
        // 合同是客户端那一侧的面，Driver 不需要、也不该据它判自己能不能执行。
        self.adapters
            .validate_publication(
                &offering.adapter_key,
                &offering.carrier_schema,
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

/// 校验「Offering 的限制不超出这条供给**自己承载的面**」。
///
/// 这是"Provider 限制只能**收窄**，不能放宽"的落地。
/// 与 [`validate_adapter_compatibility`] 的区别：后者比对的是 **Driver 的传输能力**（线上写不出去
/// 的字段名不许声明）；本函数比对的是 **这条供给自己声明的承载面**（供货方不许替厂商放宽）。
///
/// Restrictions 的形状很小，目前只有两项，因此可判定地检查两项：
/// - `allowed_branches`：每个分支都必须能在承载面的 `required`/`properties` 下成立；
/// - `max_images`：不得超过承载面对参考图数量的声明。
fn validate_restrictions_within_profile(
    offering: &NormalizedOffering,
) -> Result<(), ApplicationError> {
    let schema = &offering.carrier_schema;
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
                // 图生图/编辑需要参考图输入：承载面必须声明一个名字以 `image`
                // 开头的参数（`image`/`images`/`image_urls`）。
                Some("image_conditioned") => (
                    "image_conditioned",
                    declares_reference_image_parameter(schema),
                ),
                // 遮罩编辑还需要遮罩输入，且遮罩不能脱离参考图。
                Some("masked") => (
                    "masked",
                    declares_reference_image_parameter(schema) && declares_mask_parameter(schema),
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
                    "restriction allows branch {name}, which the carrier surface does not declare"
                )));
            }
        }
    }
    if let Some(max_images) = offering
        .restrictions
        .get("max_images")
        .and_then(Value::as_u64)
    {
        // 限制只能收窄：承载面没承诺收图上限（数组没写 `maxItems`）时，任何正的 `max_images`
        // 都算凭空放宽，同样拒绝。`0` 不需要承载面声明任何参考图参数。
        if max_images > 0 {
            match declared_reference_image_limit(schema) {
                Some(declared) if max_images <= declared => {}
                Some(declared) => {
                    return Err(ApplicationError::Validation(format!(
                        "restriction allows {max_images} image(s), but the carrier surface declares at most {declared}"
                    )));
                }
                None => {
                    return Err(ApplicationError::Validation(format!(
                        "restriction allows {max_images} image(s), but the carrier surface declares no reference image count"
                    )));
                }
            }
        }
    }
    Ok(())
}

/// 校验**模型级合同**：必须是封闭对象 schema，且 `model.const` 就是本次发布的型号。
///
/// 为什么校验在合同上而不是在每个候选上：合同是模型级唯一一份，落库后不再改；
/// 候选的承载面只是它的子集（见 [`validate_carrier_within_contract`]），
/// 因此"身份与型号一致"这件事只需在这里判一次。
fn validate_contract(native_model_id: &str, contract: &Value) -> Result<(), ApplicationError> {
    jsonschema::validator_for(contract)
        .map_err(|error| ApplicationError::Validation(error.to_string()))?;
    // 合同的身份必须与发布声明的型号一致：`model.const` 就是该 Provider 自己的模型名，
    // 发布期据此拒绝「把 A 型号的合同挂到 B 型号上」。
    if contract
        .pointer("/properties/model/const")
        .and_then(Value::as_str)
        != Some(native_model_id)
    {
        return Err(ApplicationError::Validation(
            "capability_schema model.const must equal native_model_id".to_owned(),
        ));
    }
    if contract.get("type").and_then(Value::as_str) != Some("object")
        || contract
            .get("additionalProperties")
            .and_then(Value::as_bool)
            != Some(false)
    {
        return Err(ApplicationError::Validation(
            "capability_schema must be a closed object schema".to_owned(),
        ));
    }
    Ok(())
}

/// 校验「承载面 ⊆ 合同」：供给不能凭空多出调用方可提交的字段。
///
/// 判据是**顶层字段名**：承载面声明了这个名字，合同就必须也声明它——否则这条供给会承载一个
/// 客户端根本提交不了的参数（客户端按合同提交，合同里没有的名字它不会发），声明与行为就分了家。
///
/// 只比名字、不比定义：同一个名字在两边各自描述（例如 `size` 的取值形态）是后续步骤的事，
/// 本步先把"字段面"这条边界立住。
fn validate_carrier_within_contract(
    contract: &Value,
    carrier: &Value,
) -> Result<(), ApplicationError> {
    carrier_properties(carrier)?;
    for field in declared_field_names(carrier) {
        if !declares_parameter(contract, field) {
            return Err(ApplicationError::Validation(format!(
                "carrier schema declares {field}, which the vendor model contract does not"
            )));
        }
    }
    Ok(())
}

/// 承载面的 `properties`：承载面必须是一份**声明了字段**的对象 schema。
///
/// 缺了它，两条边界校验都会"没有字段可查"而静默通过——那种通过毫无意义，因此在这里明确失败。
fn carrier_properties(
    carrier: &Value,
) -> Result<&serde_json::Map<String, Value>, ApplicationError> {
    carrier
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            ApplicationError::Validation("carrier_schema.properties is required".to_owned())
        })
}

/// 校验「承载面 ⊆ 该 Driver 能写上线文的字段名」，分支与图片数上限照旧。
///
/// `AdapterDescriptor::supported_top_level_parameters` 的语义是**传输能力**：这个 Driver 能往
/// 线文里写哪些字段名。它**不是**"调用方能提交哪些参数"——调用方看到的是合同，渠道包装的差异
/// 由承载面与映射承担。声明了写不出去的字段名，等于声明了一个发不出去的参数，因此在这里拒绝。
fn validate_adapter_compatibility(
    offering: &NormalizedOffering,
    descriptor: &AdapterDescriptor,
) -> Result<(), ApplicationError> {
    let properties = carrier_properties(&offering.carrier_schema)?;
    for parameter in declared_field_names(&offering.carrier_schema) {
        if !descriptor
            .supported_top_level_parameters
            .contains(&parameter)
        {
            return Err(ApplicationError::Validation(format!(
                "adapter {} cannot write parameter {parameter} on the wire",
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
pub struct GenerationService {
    repository: Arc<dyn HubRepository>,
    /// 预授权额（microusd）：**服务端定的固定数**，不由调用方自报。
    ///
    /// 现状是"一个固定数"，属粗判：它够跑通，也是上限。按 Price Snapshot 算出这次请求
    /// 最坏要花多少、并在低于该成本时于受理前拒绝，是后续优化——那时这项才会变准。
    max_cost_microusd: u64,
    /// 该账户同时能有多少个**在跑**的生成任务（默认 1）。
    ///
    /// 这是最初的并发设计：一个账户同时只跑一个，超出的直接拒（429），
    /// 免得一次提交一堆把上游额度与平台成本一起打满。
    max_concurrent_jobs: u64,
}

impl GenerationService {
    #[must_use]
    pub fn new(
        repository: Arc<dyn HubRepository>,
        max_cost_microusd: u64,
        max_concurrent_jobs: u64,
    ) -> Self {
        Self {
            repository,
            max_cost_microusd,
            max_concurrent_jobs,
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
        // 并发上限：一个账户同时只跑这么多个，多出来的在受理前就拒掉。
        // 同一个幂等键的重发不占名额：那种请求会去重成原来那个 Job（见 `create_job`）。
        if self
            .repository
            .count_in_flight_jobs(request.account_id, &request.idempotency_key)
            .await?
            >= self.max_concurrent_jobs
        {
            return Err(ApplicationError::TooManyInFlight);
        }
        let candidates = self.repository.active_offering(&request.model).await?;
        let (offering, native_parameters, routing) =
            select_candidate(&request, branch, &candidates)?;
        // 幂等哈希取**调用方看到的那份请求**（不含按候选解析出的参数名，也不含按承载面过滤的结果）：
        // 上游目录变了、或另一个候选的承载面更窄，都不该让同一个幂等键算出不同的哈希。
        let request_hash = request_hash(&request)?;
        self.repository
            .create_job(
                CreateImageGeneration {
                    account_id: request.account_id,
                    gateway_model: request.model,
                    native_parameters,
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
    adapters: Arc<dyn AdapterFactory>,
    credentials: Arc<dyn CredentialProvider>,
    worker_id: String,
    lease_duration: ChronoDuration,
    provider_timeout: Duration,
}

impl WorkerService {
    pub fn new(
        repository: Arc<dyn HubRepository>,
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
        // 图片输入已经在 Job 的原生参数里（受理期落在候选自己的参数名上，未声明的名字那时就被
        // 丢掉了），这里只是把它交给 Driver：平台不读字节、不核对摘要。
        //
        // 一并把"哪些名字是平台自己装好的"算出来交给 Driver：归属由**候选承载面 + 分支**决定，
        // 两者都在 Job 里冻结了，所以这份名单是确定的、与调用方这一次恰好给了什么取值无关。
        // Driver 不自己按取值的形状猜归属——那样会把一个像图的普通参数误当成平台的图。
        let prepared = PreparedImageRequest {
            provider_model_id: claimed.job.offering.provider_model_id.clone(),
            branch: claimed.job.branch,
            native_parameters: claimed.job.native_parameters.clone(),
            platform_parameters: platform_image_parameters(
                &claimed.job.offering.carrier_schema,
                claimed.job.branch,
            ),
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
        // 结果只是"当次信封"：渠道给 url 就留 url、给 base64 就留 base64，平台不看内容。
        if success.images.is_empty() {
            return Err(ApplicationError::Reconciliation(
                "provider returned no image".to_owned(),
            ));
        }
        self.repository
            .complete_job(CompleteJob {
                job_id: job.id,
                worker_id: self.worker_id.clone(),
                attempt_id,
                images: success.images,
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
    image_count: usize,
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

/// 受理的第一步：请求按**合同**校验，产出"这次请求在合同面里的参数"。
///
/// 合同是模型级唯一一份，同一型号的候选共享它，所以这件事只做一次、与选路无关。它回答两件事：
///
/// - **字段归属**：合同里没有的字段在这里丢掉，**不报错**——调用方多发一个平台不认的字段不该
///   让整次请求失败；而"这个字段在命中的候选上存不存在"本身随选路变化，逐次报错会把选路结果
///   变成调用方的负担。
/// - **必填在场**：合同说必填的字段必须给出。`model` 由平台自己落，参考图与遮罩已按契约字段名
///   从参数面里取出（图片不走普通参数），所以这两处单独算在场。
///
/// 判据是**合同**而不是承载面：合同说客户端能提交什么，承载面说这条供给能把它带到线上——
/// 后者由 [`prepare_carrier_parameters`] 逐候选判。请求里的取值本身仍**不**校验
/// （枚举、区间、类型都不管）：合同声明过的参数取值原样交给上游，平台不替它改写。
///
/// "在场"与 [`is_used_parameter_value`] 的"用到"是**两个判据**：前者回答"调用方说了这个字段吗"
/// （缺位或 `null` 算没说），后者回答"这次请求真的依赖它吗"（空串、空数组也算没给）。
/// 必填按前者判——合同要的是这个字段出现，取值合不合适不是这里的事。
fn contract_parameter_face(
    request: &CreateImageGenerationRequest,
    contract: &Value,
) -> Result<Map<String, Value>, ApplicationError> {
    let supplied = request.native_parameters.as_object().ok_or_else(|| {
        ApplicationError::Validation("native_parameters must be an object".to_owned())
    })?;
    let mut parameters = declared_parameter_names(contract, supplied);
    // `model` 是对外的平台型号名，由平台自己落；它本来就在合同里（`model.const`）。
    parameters.insert("model".to_owned(), Value::String(request.model.clone()));
    let required = contract
        .get("required")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut missing = Vec::new();
    for name in required.iter().filter_map(Value::as_str) {
        if parameters.get(name).is_some_and(|value| !value.is_null())
            || contract_image_input_present(request, name)
        {
            continue;
        }
        missing.push(name.to_owned());
    }
    if missing.is_empty() {
        Ok(parameters)
    } else {
        Err(ApplicationError::Validation(format!(
            "missing required parameter(s): {}",
            missing.join(", ")
        )))
    }
}

/// 合同字段名下的图片输入是否"在场"。
///
/// 参考图与遮罩在受理侧就按契约字段名从参数面里取了出来（它们有自己的去处：选路后落到候选声明的
/// 参数名上），因此 `parameters` 里没有它们。但调用方**确实给了**——合同把它们声明成必填时，
/// 不能因为"平台自己把图挪走了"就判成缺参数。
fn contract_image_input_present(request: &CreateImageGenerationRequest, name: &str) -> bool {
    match contract_image_parameter_kind(name) {
        Some(ImageParameterKind::Reference) => !request.reference_images.is_empty(),
        Some(ImageParameterKind::Mask) => request.mask.is_some(),
        None => false,
    }
}

/// 受理的第二步：这条供给承载得了这次请求吗？承载得了就把参数面组装出来。
///
/// **承载校验**：请求里**实际用到**的每个字段（非空值）都必须在这条候选的承载面里；缺一个就是
/// 这条候选不合格，返回原因写进路由判定记录。这正是"声明了承载面"的意义——供给说了自己能把哪些
/// 字段带到线上，平台不替它加码。
///
/// 合格之后才组装要落进 Job、并发给上游的参数面：
/// 1. 按承载面留下名字：调用方给了空值、承载面又没声明这个字段时，在这里去掉（空值不携带信息，
///    而发一个承载面没声明的名字给上游，只会得到上游自己的一套解释）；
/// 2. 把参考图与遮罩落到这条候选**自己声明的**参数名上（声明不了就是不合格，绝不静默丢图）；
/// 3. 注入映射声明的**显式默认值**：调用方没给的字段由平台定，而不是由渠道自己的默认值定；
/// 4. 最后看一眼承载面**自己声明的必填字段**是否都在场：供给说了"这次请求必须带上它"，
///    平台不替它省。放在最后是因为前两步都可能把必填项补上（图落在承载面的名字上、默认值注入），
///    先判会把"其实跑得通"的候选误判成不合格。
///
/// 返回的是"这条候选不合格"的原因，不是请求级错误：换一条承载面更宽的候选仍然可能跑通，
/// 所以它写进路由判定记录，而不是直接回给调用方。
fn prepare_carrier_parameters(
    contract_parameters: &Map<String, Value>,
    request: &CreateImageGenerationRequest,
    offering: &PublishedOffering,
) -> Result<Value, String> {
    for (name, value) in contract_parameters {
        if !is_used_parameter_value(value) {
            continue;
        }
        if !declares_parameter(&offering.carrier_schema, name) {
            return Err(format!(
                "this offering cannot carry parameter {name}, which the request uses"
            ));
        }
    }
    let mut parameters = declared_parameter_names(&offering.carrier_schema, contract_parameters);
    place_image_inputs(
        &offering.carrier_schema,
        &mut parameters,
        &request.reference_images,
        request.mask.as_deref(),
    )?;
    apply_parameter_defaults(
        &offering.capability_schema,
        &offering.carrier_schema,
        &offering.parameter_mapping,
        &mut parameters,
    );
    let missing: Vec<&str> = offering
        .carrier_schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|name| parameters.get(*name).is_none_or(Value::is_null))
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "this offering requires parameter(s) {}, which the request does not provide",
            missing.join(", ")
        ));
    }
    Ok(Value::Object(parameters))
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
    // 摘要是"这一次请求"的身份，所以连平台自己装好的参数名一起纳入：名单决定了 Driver 把哪些
    // 名字当图片、哪些按原样交给上游，它变了就是另一次请求。名单由受理时的候选面与分支算出，
    // 因此同一份 Job 重复执行得到的摘要稳定可比。
    let mut value = serde_json::json!({
        "provider_model_id": request.provider_model_id,
        "branch": request.branch,
        "native_parameters": request.native_parameters,
        "platform_parameters": request.platform_parameters,
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
        let carrier = carrier_schema();
        PublishedOffering {
            runtime_revision_id: RuntimeRevisionId::new(),
            vendor_model_id: VendorModelId::new(),
            offering_id: OfferingId::new(),
            channel_id: ChannelId::new(),
            gateway_model: "gpt-image-2".to_owned(),
            native_revision: "2026-04-21".to_owned(),
            // 这份夹具里合同与承载面同值：它要覆盖的是受理侧"按承载面过滤与装载"的行为。
            capability_schema: carrier.clone(),
            carrier_schema: carrier,
            parameter_mapping: serde_json::json!({}),
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

    /// 夹具里那条供给**能承载**的字段面。
    fn carrier_schema() -> Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["model", "prompt"],
            "properties": {
                "model": {"const": "gpt-image-2"},
                "prompt": {"type": "string", "minLength": 1},
                "image": {"type": "string"},
                "mask": {"type": "string"}
            }
        })
    }

    /// 把一个已发布供给变成"发布物里的候选"：字段完全一致，只多 `routing_priority`。
    fn candidate_of(offering: &PublishedOffering, routing_priority: i32) -> OfferingCandidate {
        OfferingCandidate {
            runtime_revision_id: offering.runtime_revision_id,
            vendor_model_id: offering.vendor_model_id,
            offering_id: offering.offering_id,
            channel_id: offering.channel_id,
            gateway_model: offering.gateway_model.clone(),
            native_revision: offering.native_revision.clone(),
            capability_schema: offering.capability_schema.clone(),
            carrier_schema: offering.carrier_schema.clone(),
            parameter_mapping: offering.parameter_mapping.clone(),
            restrictions: offering.restrictions.clone(),
            adapter_key: offering.adapter_key.clone(),
            provider_model_id: offering.provider_model_id.clone(),
            provider_kind: offering.provider_kind.clone(),
            base_url: offering.base_url.clone(),
            credential_env: offering.credential_env.clone(),
            price_snapshot: offering.price_snapshot.clone(),
            routing_priority,
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
            carrier_schema: None,
            parameter_mapping: serde_json::json!({}),
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

    /// 一个候选：**过渡期的老素材形状**——只给 offering 级旧字段，合同由它回退得来。
    fn draft(provider_model_id: &str) -> OfferingDraft {
        OfferingDraft {
            provider_kind: "AIHubMix".to_owned(),
            adapter_key: "aihubmix-image-v1".to_owned(),
            provider_model_id: provider_model_id.to_owned(),
            base_url: "https://api.inferera.com".to_owned(),
            credential_env: "AIHUBMIX_API_KEY".to_owned(),
            restrictions: serde_json::json!({}),
            carrier_schema: None,
            parameter_mapping: serde_json::json!({}),
            capability_schema: Some(schema("gpt-image-2.5-flare")),
            price_plan: Some(price_plan()),
        }
    }

    /// 一个候选：**新形状**——承载面用新名字声明，合同留给顶层。
    fn carrier_draft(provider_model_id: &str, carrier: Value) -> OfferingDraft {
        OfferingDraft {
            carrier_schema: Some(carrier),
            capability_schema: None,
            ..draft(provider_model_id)
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
        assert_eq!(normalized.offerings.len(), 3);
        // 优先级只有一个来源：数组下标。
        assert_eq!(
            normalized
                .offerings
                .iter()
                .map(|offering| offering.routing_priority)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(normalized.offerings[1].provider_model_id, "pm-b");
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
        assert_eq!(normalized.offerings.len(), 1);
        assert_eq!(normalized.offerings[0].routing_priority, 0);
        // 扁平形式没另给承载面：它承载合同声明的全部字段。
        assert_eq!(normalized.offerings[0].carrier_schema, normalized.contract);
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
    fn normalize_requires_a_carrier_schema_and_price_plan_per_offering() {
        let mut without_carrier = draft("pm-a");
        without_carrier.carrier_schema = None;
        without_carrier.capability_schema = None;
        let command = PublishRuntimeCommand {
            offerings: Some(vec![without_carrier]),
            ..base_command()
        };
        let error = command.normalize().expect_err("carrier is required");
        assert!(error.to_string().contains("carrier_schema"), "{error}");

        let mut without_price = draft("pm-a");
        without_price.price_plan = None;
        let command = PublishRuntimeCommand {
            offerings: Some(vec![without_price]),
            ..base_command()
        };
        let error = command.normalize().expect_err("price plan is required");
        assert!(error.to_string().contains("price_plan"), "{error}");
    }

    /// 数组形式下顶层给合同、候选给承载面：新形状的正常用法。
    #[test]
    fn array_form_takes_the_contract_from_the_top_level_and_the_carrier_from_each_offering() {
        let contract = schema("gpt-image-2.5-flare");
        let carrier = surface(serde_json::json!({
            "model": {"const": "gpt-image-2.5-flare"},
            "prompt": {"type": "string", "minLength": 1}
        }));
        let command = PublishRuntimeCommand {
            capability_schema: Some(contract.clone()),
            offerings: Some(vec![
                carrier_draft("pm-a", carrier.clone()),
                carrier_draft("pm-b", carrier.clone()),
            ]),
            ..base_command()
        };
        let normalized = command.normalize().expect("array form is valid");
        assert_eq!(normalized.contract, contract);
        assert_eq!(normalized.offerings[0].carrier_schema, carrier);
        assert_eq!(normalized.offerings[1].carrier_schema, carrier);
    }

    /// 顶层合同优先：候选自带的旧字段这时是**承载面**，不再是合同。
    #[test]
    fn top_level_contract_wins_over_the_legacy_offering_field() {
        let contract = schema("gpt-image-2.5-flare");
        let legacy = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["model", "prompt"],
            "properties": {
                "model": {"const": "gpt-image-2.5-flare"},
                "prompt": {"type": "string"}
            }
        });
        let command = PublishRuntimeCommand {
            capability_schema: Some(contract.clone()),
            offerings: Some(vec![OfferingDraft {
                capability_schema: Some(legacy.clone()),
                ..draft("pm-a")
            }]),
            ..base_command()
        };
        let normalized = command.normalize().expect("top-level contract wins");
        assert_eq!(normalized.contract, contract);
        assert_eq!(normalized.offerings[0].carrier_schema, legacy);
    }

    /// 过渡期回退：顶层没给合同，老素材那份 offering 级声明面同时当合同与承载面。
    #[test]
    fn legacy_offering_schema_falls_back_to_the_contract() {
        let command = PublishRuntimeCommand {
            offerings: Some(vec![draft("pm-a")]),
            ..base_command()
        };
        let normalized = command
            .normalize()
            .expect("legacy material still publishes");
        assert_eq!(normalized.contract, schema("gpt-image-2.5-flare"));
        assert_eq!(normalized.offerings[0].carrier_schema, normalized.contract);
    }

    /// 回退时各候选的旧字段必须一致：合同只有一份，两份内容不能同时当合同。
    #[test]
    fn legacy_offering_schemas_that_disagree_are_rejected() {
        let other = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["model", "prompt"],
            "properties": {
                "model": {"const": "gpt-image-2.5-flare"},
                "prompt": {"type": "string"},
                "image_urls": {"type": "array", "items": {"type": "string"}}
            }
        });
        let command = PublishRuntimeCommand {
            offerings: Some(vec![
                draft("pm-a"),
                OfferingDraft {
                    capability_schema: Some(other),
                    ..draft("pm-b")
                },
            ]),
            ..base_command()
        };
        let error = command
            .normalize()
            .expect_err("two different contracts for one model must be rejected");
        assert!(
            error.to_string().contains("different capability schemas"),
            "{error}"
        );
    }

    /// 造一个用于兼容性校验的候选：承载面只声明给定的字段。
    fn offering_with(schema_properties: Value, restrictions: Value) -> NormalizedOffering {
        NormalizedOffering {
            carrier_schema: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["model", "prompt"],
                "properties": schema_properties
            }),
            parameter_mapping: serde_json::json!({}),
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

    /// 一份只声明给定顶层字段的合同/承载面。
    fn surface(properties: Value) -> Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["model", "prompt"],
            "properties": properties
        })
    }

    /// 一个"能写这几个字段名上线文"的 Driver。
    fn descriptor() -> AdapterDescriptor {
        AdapterDescriptor {
            key: "aihubmix-image-v1",
            supported_top_level_parameters: &["model", "prompt", "image", "mask", "quality"],
            supported_extra_parameters: &[],
            supported_branches: &[
                ImageBranch::PromptOnly,
                ImageBranch::ImageConditioned,
                ImageBranch::Masked,
            ],
            max_images: 1,
        }
    }

    /// R1：承载面声明了合同里没有的字段 ⇒ 供给凭空多出调用方可提交的参数，拒绝。
    #[test]
    fn carrier_field_the_contract_does_not_declare_is_rejected() {
        let contract = surface(serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"}
        }));
        let carrier = surface(serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"},
            "quality": {"type": "string"}
        }));
        let error = validate_carrier_within_contract(&contract, &carrier)
            .expect_err("a field outside the contract must be rejected");
        assert!(
            error
                .to_string()
                .contains("which the vendor model contract does not"),
            "{error}"
        );
    }

    /// R2：承载面声明了这个 Driver 写不出去的字段名 ⇒ 声明了发不出去，拒绝。
    #[test]
    fn carrier_field_the_driver_cannot_write_is_rejected() {
        let mut offering = offering_with(
            serde_json::json!({
                "model": {"const": "m"},
                "prompt": {"type": "string"},
                "resolution": {"type": "string"}
            }),
            serde_json::json!({}),
        );
        offering.carrier_schema = surface(serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"},
            "resolution": {"type": "string"}
        }));
        let error = validate_adapter_compatibility(&offering, &descriptor())
            .expect_err("a field the driver cannot write must be rejected");
        assert!(
            error
                .to_string()
                .contains("cannot write parameter resolution"),
            "{error}"
        );
    }

    /// 承载面落在合同与 Driver 之内时通过：两个边界各判一次，缺一不可。
    #[test]
    fn carrier_within_the_contract_and_the_driver_is_accepted() {
        let contract = surface(serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"},
            "image": {"type": "string"},
            "quality": {"type": "string"}
        }));
        let mut offering = offering_with(
            serde_json::json!({
                "model": {"const": "m"},
                "prompt": {"type": "string"},
                "image": {"type": "string"},
                "quality": {"type": "string"}
            }),
            serde_json::json!({"allowed_branches": ["prompt_only"], "max_images": 0}),
        );
        offering.carrier_schema = contract.clone();
        assert!(validate_carrier_within_contract(&contract, &offering.carrier_schema).is_ok());
        assert!(validate_adapter_compatibility(&offering, &descriptor()).is_ok());
    }

    /// 合同的身份就是发布的型号：`model.const` 不符即拒绝（换型号要发新的合同）。
    #[test]
    fn contract_identity_must_match_the_published_model() {
        let contract = schema("gpt-image-2.5-flare");
        assert!(validate_contract("gpt-image-2.5-flare", &contract).is_ok());
        let error = validate_contract("another-model", &contract)
            .expect_err("a contract for another model must be rejected");
        assert!(error.to_string().contains("model.const"), "{error}");
        // 不封闭的 schema 不是合同：调用方写错字段名会被静默收下。
        let open = serde_json::json!({
            "type": "object",
            "properties": {"model": {"const": "m"}, "prompt": {"type": "string"}}
        });
        assert!(validate_contract("m", &open).is_err());
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
            reference_images: Vec::new(),
            mask: None,
            idempotency_key: "request-0001".to_owned(),
        }
    }

    /// 请求按合同校验之后留下的参数面（合同外的字段已丢弃）。测试里用它把"按合同校验"与
    /// "逐候选承载校验"分开看。
    fn contract_face(
        request: &CreateImageGenerationRequest,
        offering: &PublishedOffering,
    ) -> Map<String, Value> {
        contract_parameter_face(request, &offering.capability_schema)
            .expect("the fixture request satisfies the contract")
    }

    #[test]
    fn validates_prompt_only_native_request() {
        let request = image_request(serde_json::json!({"prompt": "hello"}));
        let vendor = offering();
        let face = contract_face(&request, &vendor);
        assert_eq!(
            face.keys().collect::<Vec<_>>(),
            vec!["model", "prompt"],
            "`model` 由平台自己落，其余按合同留下"
        );
        assert!(prepare_carrier_parameters(&face, &request, &vendor).is_ok());
    }

    /// 合同里没有的字段在受理期丢掉——不报错，也不会跟着 Job 走去上游。
    ///
    /// 判据是**合同**：调用方多发一个平台不认的字段（渠道一手参数、`seed`、纯属多余的 `foo`）
    /// 不该让整次请求失败；而"这个字段在命中的候选上存不存在"本身随选路变化，逐次报错会把
    /// 选路结果变成调用方的负担。
    #[test]
    fn parameters_the_contract_never_declared_are_dropped_without_an_error() {
        let mut vendor = offering();
        // 合同与承载面这次同值：这条用例验的是"合同外的字段"，与承载面无关。
        let declared = surface(serde_json::json!({
            "model": {"const": "gpt-image-2"},
            "prompt": {"type": "string", "minLength": 1},
            "n": {"type": "integer"},
            "quality": {"enum": ["low", "high"]}
        }));
        vendor.capability_schema = declared.clone();
        vendor.carrier_schema = declared;
        let request = image_request(serde_json::json!({
            "prompt": "hello",
            "n": "not-a-number",
            "quality": "high",
            "channel_specific_knob": {"a": 1},
            "seed": 7,
            "foo": "bar",
            "image_with_roles": [{"role": "reference", "url": "https://example.invalid/a.png"}]
        }));
        let face = contract_face(&request, &vendor);
        assert_eq!(
            face.keys().collect::<Vec<_>>(),
            vec!["model", "n", "prompt", "quality"],
            "只有合同声明过的名字留到 Job 里：{face:?}"
        );
        // 合同声明过的参数取值不校验：类型不对也照原样留下。
        assert_eq!(face.get("n"), Some(&serde_json::json!("not-a-number")));
        assert_eq!(face.get("quality"), Some(&serde_json::json!("high")));
        let prepared = prepare_carrier_parameters(&face, &request, &vendor)
            .expect("the carrier declares every field the request uses");
        assert_eq!(prepared, Value::Object(face));
    }

    /// 请求**用到的**字段必须在这条候选的承载面里；缺了就是这条候选不合格。
    ///
    /// 注意它与"请求违反合同"是两件事：请求本身没问题（`quality` 在合同里），只是这条供给
    /// 承载不了它——所以这条候选落选、换下一条，而不是把整次请求判成参数错。
    #[test]
    fn a_used_field_the_carrier_cannot_carry_makes_the_candidate_ineligible() {
        let mut vendor = offering();
        vendor.capability_schema = surface(serde_json::json!({
            "model": {"const": "gpt-image-2"},
            "prompt": {"type": "string"},
            "quality": {"enum": ["low", "high"]}
        }));
        // 承载面收窄：这条供给承载不了 `quality`。
        vendor.carrier_schema = surface(serde_json::json!({
            "model": {"const": "gpt-image-2"},
            "prompt": {"type": "string"}
        }));
        let request = image_request(serde_json::json!({"prompt": "hello", "quality": "high"}));
        let face = contract_face(&request, &vendor);
        let reason = prepare_carrier_parameters(&face, &request, &vendor)
            .expect_err("the carrier cannot carry quality");
        assert!(reason.contains("quality"), "{reason}");

        // 调用方没用这个字段（空值＝没给）：这条候选照样合格，空位也不会被发上去。
        for empty in [
            serde_json::json!(null),
            serde_json::json!(""),
            serde_json::json!([]),
        ] {
            let request =
                image_request(serde_json::json!({"prompt": "hello", "quality": empty.clone()}));
            let face = contract_face(&request, &vendor);
            let prepared = prepare_carrier_parameters(&face, &request, &vendor)
                .unwrap_or_else(|error| panic!("`{empty}` 是没给，候选该合格：{error}"));
            assert!(
                prepared.get("quality").is_none(),
                "承载面没声明的空位不该发上去：{prepared}"
            );
        }
    }

    #[test]
    fn filtering_does_not_drop_the_images_the_platform_places() {
        // 装载用的名字取自同一份声明面，所以"先过滤、再装载"不会把图丢掉。
        let mut vendor = offering();
        vendor.carrier_schema = serde_json::json!({
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
        let mut request = image_request(serde_json::json!({
            "prompt": "hello",
            "image_with_roles": [],
            "seed": 1
        }));
        request.reference_images = vec!["data:image/png;base64,AAAA".to_owned()];
        request.mask = Some("data:image/png;base64,BBBB".to_owned());
        let face = contract_face(&request, &vendor);
        let prepared =
            prepare_carrier_parameters(&face, &request, &vendor).expect("images are placed");
        assert_eq!(
            prepared.get("image_urls"),
            Some(&serde_json::json!(["data:image/png;base64,AAAA"]))
        );
        assert_eq!(
            prepared.get("mask_url"),
            Some(&Value::String("data:image/png;base64,BBBB".to_owned()))
        );
        assert_eq!(
            prepared
                .as_object()
                .expect("an object")
                .keys()
                .collect::<Vec<_>>(),
            vec!["image_urls", "mask_url", "model", "prompt"],
            "未声明的名字一个都不留：{prepared}"
        );
    }

    /// 合同说必填的字段必须给出；缺了就是调用方的参数错（400），与选路无关。
    #[test]
    fn missing_required_parameters_are_rejected() {
        let request = image_request(serde_json::json!({}));
        let error = contract_parameter_face(&request, &offering().capability_schema)
            .expect_err("a missing required parameter must fail");
        assert!(error.to_string().contains("prompt"), "{error}");

        // 参考图与遮罩已被受理侧按契约字段名取出，但合同把它们声明成必填时不能算缺：
        // 调用方**确实给了**这张图。
        let with_image_required = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["model", "prompt", "image"],
            "properties": {
                "model": {"const": "gpt-image-2"},
                "prompt": {"type": "string"},
                "image": {"type": "string"}
            }
        });
        let mut request = image_request(serde_json::json!({"prompt": "hello"}));
        assert!(
            contract_parameter_face(&request, &with_image_required)
                .expect_err("no image was given")
                .to_string()
                .contains("image")
        );
        request.reference_images = vec!["https://example.invalid/a.png".to_owned()];
        assert!(contract_parameter_face(&request, &with_image_required).is_ok());
    }

    #[test]
    fn places_reference_images_on_the_candidates_own_parameter() {
        let mut request = image_request(serde_json::json!({"prompt": "hello"}));
        request.reference_images = vec!["https://example.invalid/a.png".to_owned()];
        let vendor = offering();
        let face = contract_face(&request, &vendor);
        let prepared =
            prepare_carrier_parameters(&face, &request, &vendor).expect("images are placed");
        assert_eq!(
            prepared.get("image"),
            Some(&Value::String("https://example.invalid/a.png".to_owned()))
        );
    }

    #[test]
    fn places_images_into_the_vendors_own_array_parameter() {
        // 调用方只给参考图/遮罩；装到 `image_urls` / `mask_url` 是平台按候选声明做的映射。
        let mut vendor = offering();
        vendor.carrier_schema = serde_json::json!({
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
        request.reference_images = vec!["data:image/png;base64,AAAA".to_owned()];
        request.mask = Some("data:image/png;base64,BBBB".to_owned());
        // 合同（`offering()` 的那一份）把参考图与遮罩声明成 `image` / `mask`，
        // 而这条承载面把同一件事声明成 `image_urls` / `mask_url`：图片按**承载面**的名字落。
        let face = contract_face(&request, &vendor);
        let prepared =
            prepare_carrier_parameters(&face, &request, &vendor).expect("images are placed");
        assert_eq!(
            prepared.get("image_urls"),
            Some(&serde_json::json!(["data:image/png;base64,AAAA"]))
        );
        assert_eq!(
            prepared.get("mask_url"),
            Some(&Value::String("data:image/png;base64,BBBB".to_owned()))
        );

        // 承载面里没有装参考图的参数：这条供给表达不了，直接不合格（不静默丢图）。
        let mut text_only = offering();
        text_only.capability_schema = vendor.capability_schema.clone();
        text_only.carrier_schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["model", "prompt"],
            "properties": {
                "model": {"const": "gpt-image-2"},
                "prompt": {"type": "string", "minLength": 1}
            }
        });
        let face = contract_face(&request, &text_only);
        assert!(prepare_carrier_parameters(&face, &request, &text_only).is_err());
    }

    /// 显式默认值：调用方没给、映射声明了、且合同与承载面都声明了这个字段 → 注入。
    ///
    /// 注入发生在组装参数面的最后一步，所以它会跟着 Job 落库、并出现在发给上游的报文里——
    /// 渠道自己那套默认值（例如上游把水印默认打开）因此再也用不上。
    #[test]
    fn explicit_defaults_fill_the_fields_the_caller_left_out() {
        let mut vendor = offering();
        vendor.capability_schema = surface(serde_json::json!({
            "model": {"const": "gpt-image-2"},
            "prompt": {"type": "string"},
            "watermark": {"type": "boolean"}
        }));
        vendor.carrier_schema = vendor.capability_schema.clone();
        vendor.parameter_mapping = serde_json::json!({"defaults": {"watermark": false}});

        // 调用方没给：注入默认值。
        let request = image_request(serde_json::json!({"prompt": "hello"}));
        let face = contract_face(&request, &vendor);
        let prepared = prepare_carrier_parameters(&face, &request, &vendor).expect("carried");
        assert_eq!(
            prepared.get("watermark"),
            Some(&serde_json::json!(false)),
            "调用方没给的字段该由平台定，而不是由渠道的默认值定：{prepared}"
        );

        // 调用方给了：用调用方的值，一个字都不改。
        let request = image_request(serde_json::json!({"prompt": "hello", "watermark": true}));
        let face = contract_face(&request, &vendor);
        let prepared = prepare_carrier_parameters(&face, &request, &vendor).expect("carried");
        assert_eq!(prepared.get("watermark"), Some(&serde_json::json!(true)));

        // 承载面承载不了这个字段：不注入（发出去只会得到上游自己的一套解释）。
        vendor.carrier_schema = surface(serde_json::json!({
            "model": {"const": "gpt-image-2"},
            "prompt": {"type": "string"}
        }));
        let request = image_request(serde_json::json!({"prompt": "hello"}));
        let face = contract_face(&request, &vendor);
        let prepared = prepare_carrier_parameters(&face, &request, &vendor).expect("carried");
        assert!(prepared.get("watermark").is_none(), "{prepared}");
    }

    /// 承载面**自己声明的必填字段**也得在场：供给说了"这次请求必须带上它"，平台不替它省。
    ///
    /// 与"请求用到的字段"是两件事：这是承载面**要求**的字段，不是调用方用到的字段。判它的时机
    /// 也重要——要等图落到承载面的名字上、默认值注入之后，否则会把跑得通的候选误判成不合格。
    #[test]
    fn a_carrier_required_field_the_request_never_provides_makes_the_candidate_ineligible() {
        let mut vendor = offering();
        vendor.capability_schema = surface(serde_json::json!({
            "model": {"const": "gpt-image-2"},
            "prompt": {"type": "string"},
            "watermark": {"type": "boolean"}
        }));
        // 承载面把 `watermark` 声明成必填（在合同里它只是可选项）。
        vendor.carrier_schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["model", "prompt", "watermark"],
            "properties": {
                "model": {"const": "gpt-image-2"},
                "prompt": {"type": "string"},
                "watermark": {"type": "boolean"}
            }
        });
        let request = image_request(serde_json::json!({"prompt": "hello"}));
        let face = contract_face(&request, &vendor);
        let reason = prepare_carrier_parameters(&face, &request, &vendor)
            .expect_err("the carrier requires watermark");
        assert!(reason.contains("watermark"), "{reason}");

        // 映射给了默认值：承载面要的字段被补上，这条候选就合格了。
        vendor.parameter_mapping = serde_json::json!({"defaults": {"watermark": false}});
        let prepared = prepare_carrier_parameters(&face, &request, &vendor)
            .expect("the default fills the required field");
        assert_eq!(prepared.get("watermark"), Some(&serde_json::json!(false)));

        // 参考图同理：承载面要的参考图字段由平台装载的图补上。
        let mut vendor = offering();
        vendor.capability_schema = surface(serde_json::json!({
            "model": {"const": "gpt-image-2"},
            "prompt": {"type": "string"},
            "image": {"type": "string"}
        }));
        vendor.carrier_schema = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["model", "prompt", "image"],
            "properties": {
                "model": {"const": "gpt-image-2"},
                "prompt": {"type": "string"},
                "image": {"type": "string"}
            }
        });
        let mut request = image_request(serde_json::json!({"prompt": "hello"}));
        let face = contract_face(&request, &vendor);
        assert!(prepare_carrier_parameters(&face, &request, &vendor).is_err());
        request.reference_images = vec!["https://example.invalid/a.png".to_owned()];
        let face = contract_face(&request, &vendor);
        let prepared = prepare_carrier_parameters(&face, &request, &vendor)
            .expect("the placed image fills the required field");
        assert_eq!(
            prepared.get("image"),
            Some(&Value::String("https://example.invalid/a.png".to_owned()))
        );
    }

    /// 选路：第一个候选承载不了请求用到的字段 → 落到下一条，判定记录写明为什么。
    ///
    /// 一条都不合格时**不是**参数错：请求本身没违反合同，是平台的供给面承载不了它。
    #[test]
    fn routing_skips_a_candidate_that_cannot_carry_a_used_field() {
        let contract = surface(serde_json::json!({
            "model": {"const": "gpt-image-2"},
            "prompt": {"type": "string"},
            "quality": {"enum": ["low", "high"]}
        }));
        // 优先级 0 的候选承载面窄（承载不了 `quality`），优先级 1 的候选承载得了。
        let mut narrow = offering();
        narrow.capability_schema = contract.clone();
        narrow.carrier_schema = surface(serde_json::json!({
            "model": {"const": "gpt-image-2"},
            "prompt": {"type": "string"}
        }));
        let mut wide = offering();
        wide.capability_schema = contract.clone();
        wide.carrier_schema = contract.clone();
        // 两条候选是不同的供给：判定记录按 offering_id 记选中者。
        wide.offering_id = OfferingId::new();

        let request = image_request(serde_json::json!({"prompt": "hello", "quality": "high"}));
        let branch = request.branch().expect("prompt only");
        let (chosen, parameters, decision) = select_candidate(
            &request,
            branch,
            &[candidate_of(&narrow, 0), candidate_of(&wide, 1)],
        )
        .expect("the second candidate can carry quality");
        assert_eq!(chosen.offering_id, wide.offering_id);
        assert_eq!(parameters.get("quality"), Some(&serde_json::json!("high")));
        assert_eq!(
            decision.considered.len(),
            2,
            "判定记录要记全，不是记到命中为止"
        );
        assert!(!decision.considered[0].eligible);
        assert!(
            decision.considered[0]
                .skip_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("quality")),
            "落选原因必须写明承载不了哪个字段：{:?}",
            decision.considered[0].skip_reason
        );
        assert!(decision.considered[1].eligible);
        assert!(decision.considered[1].skip_reason.is_none());

        // 一条都不合格：平台侧供给问题，与"请求违反合同"分开报。
        let error = select_candidate(&request, branch, &[candidate_of(&narrow, 0)])
            .expect_err("no candidate can carry quality");
        assert!(
            matches!(error, ApplicationError::NoEligibleOffering(_)),
            "{error}"
        );
        // 请求本身违反合同（缺必填）：仍然是参数错。
        let missing_prompt = image_request(serde_json::json!({"quality": "high"}));
        let error = select_candidate(&missing_prompt, branch, &[candidate_of(&narrow, 0)])
            .expect_err("prompt is missing");
        assert!(matches!(error, ApplicationError::Validation(_)), "{error}");
        // 该型号一条 active 供给都没有：是"不存在"，不是"承载不了"。
        let error = select_candidate(&request, branch, &[]).expect_err("no active offering");
        assert!(matches!(error, ApplicationError::NotFound(_)), "{error}");
    }

    #[test]
    fn branch_follows_the_images_the_caller_sent() {
        let mut request = image_request(serde_json::json!({"prompt": "hello"}));
        assert_eq!(
            request.branch().expect("prompt only"),
            ImageBranch::PromptOnly
        );
        request.reference_images = vec!["https://example.invalid/a.png".to_owned()];
        assert_eq!(
            request.branch().expect("image conditioned"),
            ImageBranch::ImageConditioned
        );
        request.mask = Some("data:image/png;base64,BBBB".to_owned());
        assert_eq!(request.branch().expect("masked"), ImageBranch::Masked);
        // 只有遮罩没有参考图：结构性规则，直接拒绝。
        request.reference_images.clear();
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

        async fn count_in_flight_jobs(
            &self,
            _account_id: AccountId,
            _except_idempotency_key: &str,
        ) -> Result<u64, ApplicationError> {
            Ok(0)
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
            Ok(ProviderSuccess {
                images: vec![GeneratedImage::from_base64("iVBORw0KGgo=".to_owned())],
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

    fn worker_job(max_cost_microusd: u64) -> GenerationJob {
        GenerationJob {
            id: JobId::new(),
            account_id: AccountId::new(),
            state: JobState::Leased,
            branch: ImageBranch::PromptOnly,
            gateway_model: "gpt-image-2".to_owned(),
            native_parameters: serde_json::json!({"prompt": "worker contract"}),
            offering: offering(),
            idempotency_key: "worker-contract-1".to_owned(),
            request_hash: "request-hash".to_owned(),
            max_cost_microusd,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn worker(repository: Arc<WorkerRepository>, adapter: Arc<WorkerAdapter>) -> WorkerService {
        WorkerService::new(
            repository,
            Arc::new(WorkerAdapterFactory { adapter }),
            Arc::new(WorkerCredentialProvider),
            "worker-test".to_owned(),
            ChronoDuration::seconds(30),
            Duration::from_secs(1),
        )
        .expect("worker fixture must be valid")
    }

    #[tokio::test]
    async fn worker_settles_the_provider_image_envelope_with_the_evidence() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let repository = Arc::new(WorkerRepository::new(worker_job(20_000), events.clone()));
        let adapter = Arc::new(WorkerAdapter {
            succeeds: true,
            calls: AtomicUsize::new(0),
        });

        assert!(
            worker(repository.clone(), adapter.clone())
                .run_once()
                .await
                .expect("worker run must succeed")
        );
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 1);
        assert_eq!(*events.lock().expect("event lock"), vec!["complete"]);
        let completion = repository
            .completion
            .lock()
            .expect("completion lock")
            .take()
            .expect("job must complete");
        assert_eq!(completion.evidence.usage.total_tokens, 205);
        assert_eq!(completion.charge_microusd, 5_925);
        assert_eq!(
            completion.images,
            vec![GeneratedImage::from_base64("iVBORw0KGgo=".to_owned())],
            "结果信封必须原样落到 Job：渠道给 base64 就留 base64"
        );
        assert!(repository.failure.lock().expect("failure lock").is_none());
    }

    #[tokio::test]
    async fn worker_sends_ambiguous_provider_response_to_reconciliation_once() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let repository = Arc::new(WorkerRepository::new(worker_job(20_000), events.clone()));
        let adapter = Arc::new(WorkerAdapter {
            succeeds: false,
            calls: AtomicUsize::new(0),
        });

        assert!(
            worker(repository.clone(), adapter.clone())
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

    /// 第二种对账：**上游已确认生成、但平台无法结清**（实际费用超过受理时的预授权）。
    ///
    /// 与第一种（创建阶段失联）的区别在这条路径上体现为**错误码不同**：
    /// 这里是 `result_delivery_failed`，而创建阶段失联用 adapter 报的错误码。
    /// 两者都进对账并保留预授权，但性质可分。
    #[tokio::test]
    async fn worker_sends_settlement_failure_to_reconciliation_with_its_own_code() {
        let events = Arc::new(Mutex::new(Vec::new()));
        // 预授权 1 microusd，而这次生成的费用是 5_925：生成已经发生，因此只能对账，不能当失败。
        let repository = Arc::new(WorkerRepository::new(worker_job(1), events.clone()));
        let adapter = Arc::new(WorkerAdapter {
            succeeds: true,
            calls: AtomicUsize::new(0),
        });

        assert!(
            worker(repository.clone(), adapter.clone())
                .run_once()
                .await
                .expect("worker run must converge")
        );
        assert_eq!(
            adapter.calls.load(Ordering::SeqCst),
            1,
            "the provider was called exactly once; a settlement failure must not retry it"
        );
        let failure = repository
            .failure
            .lock()
            .expect("failure lock")
            .take()
            .expect("settlement failure must be recorded");
        assert_eq!(
            failure.provider_code, "result_delivery_failed",
            "the settlement failure must carry its own code, distinct from acceptance-unknown"
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
            "no settlement may happen when the actual charge exceeds the authorization"
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
