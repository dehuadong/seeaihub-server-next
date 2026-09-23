use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use seeai_adapter_sdk::{
    AdapterDescriptor, AdapterError, ImageAdapter, PreparedImageRequest, ProviderCost,
    ProviderCredential, ProviderSuccess, RetrySafety,
};
pub use seeai_adapter_sdk::{GeneratedImage, ProviderFailureKind};
use seeai_domain::{
    AccountId, AttemptId, ChannelId, ChargeFacts, ConsumerRatesCny, CostBasis,
    CreateImageGeneration, FloorTable, FxRate, GenerationJob, HoldSource, ImageBranch,
    ImageParameterKind, JobId, JobState, MeteringEvidence, OfferingCandidate, OfferingId,
    ParameterRenames, PriceRates, PriceSnapshot, PricingFormula, ProviderCostFact,
    ProviderCostSource, PublishedModel, PublishedOffering, PublishedRevision, RoutePolicy,
    RouteStrategy, RuntimeRevisionId, TokenUsage, apply_enum_maps, apply_parameter_defaults,
    apply_parameter_renames, apply_size_mapping, carries_parameter, contract_image_parameter_kind,
    contract_model_identity, declared_defaults, declared_enum_maps, declared_field_names,
    declared_parameter_names, declared_reference_image_limit, declared_renames,
    declared_size_mapping, declares_mask_parameter, declares_parameter,
    declares_reference_image_parameter, is_used_parameter_value, literal_parameter_text,
    place_image_inputs, platform_image_parameters, resolve_size_tier, unit_amount_microusd,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};
use thiserror::Error;
use uuid::Uuid;

/// 发布一个 Vendor Model 的供给。
///
/// 一次发布携带该模型**完整、有序**的候选集合（`offerings`，必填且非空）；
/// 候选的 `routing_priority` **缺省取数组下标**（`0..n-1`）——"顺序即优先级"的常规来源；
/// 显式给值时可以让**多条候选落在同一档**，档内再按 `weight` 分摊。
///
/// 合同是**模型级唯一一份**（[`Self::capability_schema`]）；每个候选各自声明它**能承载**的
/// 字段面（[`OfferingDraft::carrier_schema`]）。
///
/// 发布命令只有"候选数组"这一种形状：每条供给自带渠道、承载面与计价，同一个网关模型的
/// 不同候选因此能有不同的价。`offerings` 用 `Option` 收口，是为了让缺省与 `null` 落到同一条
/// 校验错误上（空数组另有一条），不是允许省略。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishRuntimeCommand {
    pub vendor_id: String,
    pub native_model_id: String,
    /// **平台对客名**（网关模型名）：调用方提交 `model` 时用的那个名字，也是这次发布
    /// **原子替换**的对象。缺省时回退取 `native_model_id`——今天两者同值，老素材、老已发布
    /// 数据与老测试因此逐位不变。
    #[serde(default)]
    pub gateway_model: Option<String>,
    pub native_revision: String,
    /// **Vendor Model Contract**：调用方合同的唯一一份，模型级。
    ///
    /// 顶层可以省略：省略时回退用候选自带的旧字段（承载面与合同还是同一份），
    /// 但要求它们彼此完全一致——合同只有一份，同一个模型落成两份合同正是要收掉的分叉。
    #[serde(default)]
    pub capability_schema: Option<Value>,
    /// 本次发布的**完整、有序**候选集合：必填且非空。
    ///
    /// 每条候选自带渠道、承载面与计价；候选的档位缺省取它在数组里的下标，也可以自己声明
    /// （同档多候选时按 `weight` 分摊）。缺省、`null` 与空数组都拒绝——发布的内容就是这份
    /// 候选集合，没有它就没有可发布的东西。
    #[serde(default)]
    pub offerings: Option<Vec<OfferingDraft>>,
    /// **加价系数**（基点，避免浮点）：**每个网关模型一个**，随修订发布、随 Job 快照冻结。
    ///
    /// 它不放在可变的开关表里：定价是修订的内容——放进可变表就等于"改价不用发布"，而
    /// 已受理的 Job 必须固定受理时那一版。**具体数值由后台录入，不属设计决策**。
    ///
    /// 它**参与设定**对客价：按 token 计量量的候选由管理员按"该候选成本单价 × 倍率 × 折算率"
    /// 推导那份四档向量（直接录入时它一次都不参与计算，所以那种发布可以不给）；按张 / 按次 /
    /// 上游给金额的候选没有对客价载体，**必须给**——它们的对客价由结算按冻结的这份倍率算出来。
    #[serde(default)]
    pub markup_bps: Option<i32>,
    pub actor: String,
}

/// 发布请求的**已校验**形态：由 [`PublishRuntimeCommand::into_request`] 产出
/// （在逐候选校验之后），是仓库端口 `publish_runtime` 接收的唯一形态。
///
/// 为什么与 [`PublishRuntimeCommand`] 分开：命令是"线上格式"，自带必填规则；
/// 请求是"已经检查过、可以落库的东西"。分开之后，数据库那层的入口
/// **在类型上**就只接受已核验的数据——绕开 `RuntimeService::publish` 直接调端口不再可能。
#[derive(Debug, Clone)]
pub struct PublishRuntimeRequest {
    pub vendor_id: String,
    /// 厂商原生名：只属于厂商模型与合同的身份，**不进对客面**。
    pub native_model_id: String,
    /// 平台对客名：这次发布定义并原子替换的那个网关模型。
    pub gateway_model: String,
    pub native_revision: String,
    pub actor: String,
    /// 该模型的调用方合同（模型级唯一一份，落库后不再改）。
    pub capability_schema: Value,
    /// 加价系数（基点）：随修订发布、随 Job 快照冻结；没有候选带定价时为 `None`。
    pub markup_bps: Option<i32>,
    /// 候选集：档位与档内权重都已在归一阶段定好（见 [`NormalizedOffering`]）。
    pub offerings: Vec<NormalizedOffering>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfferingDraft {
    pub provider_kind: String,
    pub adapter_key: String,
    pub provider_model_id: String,
    pub base_url: String,
    pub credential_env: String,
    /// **这条候选的档位**：数字小者优先。
    ///
    /// 缺省时取它在 `offerings` 数组里的**下标**——这是今天的口径，也是"顺序即优先级"的
    /// 唯一来源。**显式给值**是为了让多条候选落在**同一档**：档内按 [`Self::weight`] 分摊，
    /// 而"同档多候选"这件事没法用下标表达（下标天然互不相同）。
    #[serde(default)]
    pub routing_priority: Option<i32>,
    /// **档位内的分流比**：正整数，缺省 `1`。
    ///
    /// 只在同一档内起作用；显式 `0` 会被拒——"不参与分流"不是权重的取值。
    #[serde(default)]
    pub weight: Option<u32>,
    #[serde(default = "empty_object")]
    pub restrictions: Value,
    /// 这条供给**能承载**合同里的哪些字段。
    #[serde(default)]
    pub carrier_schema: Option<Value>,
    /// 把合同值转成渠道包装的声明（显式默认值与尺寸换算）。随发布落库、随 Job 冻结。
    #[serde(default = "empty_object")]
    pub parameter_mapping: Value,
    /// 承载面的**旧名字**（过渡期）：只在没有 `carrier_schema` 时顶替它。
    #[serde(default)]
    pub capability_schema: Option<Value>,
    /// 这条供给的**计价形态**（渠道事实）：这个渠道的这个模型按什么计价，决定成本怎么算。
    ///
    /// 取值受控（`token_rates` / `per_image` / `per_call` / `upstream_declared`），必填：说不清
    /// 一条供给按什么计价，它的成本就没有算法。**它不是平台的定价选择**——对客卖多少钱走
    /// [`Self::consumer_rates_cny`]，与这里无关。
    #[serde(default)]
    pub formula: Option<String>,
    /// **该渠道按 token 计量量计价时的那份四档费率**（`formula = token_rates` 的参数）。
    ///
    /// 渠道不按 token 计量量计价时**不必发它**——那时这条供给没有 Price Plan。
    #[serde(default)]
    pub price_plan: Option<PricePlanDraft>,
    /// `per_image` / `per_call` 的**单价**（成本平面微单位，币种见 [`Self::cost_currency`]）。
    ///
    /// 按张 / 按次计价时它是成本自算唯一的参数；另外两种形态不给（给了会被拒：那个数永远不会
    /// 被读，留着只会让人以为它在生效）。
    #[serde(default)]
    pub cost_unit_price_microusd: Option<u64>,
    /// 这条供给**声明的成本币种**（渠道自己的钱是什么币）。
    ///
    /// 缺省取它的 Price Plan 币种；没有 Price Plan 时必须显式声明——成本要折算成人民币算毛利，
    /// 单价与上游声明的金额也都要说清是哪个币种的钱。
    #[serde(default)]
    pub cost_currency: Option<String>,
    /// 该候选的渠道成本（**原币种**微单位）：**只作定价参考，不是售价的被乘数**。
    ///
    /// 发布者给每个候选取一个可核的值：`computed` 按该渠道四档费率 × 参考用量、`declared`
    /// 取上游声明过的金额。
    #[serde(default)]
    pub reference_cost_microusd: Option<u64>,
    /// 该候选的**对客四档 CNY 费率向量**：按 token 计量量计价时的对客价（实收按它算）。
    ///
    /// 按张 / 按次计价或直接由上游给金额时**不给**（给了会被拒：那份向量是 `token_rates` 的价格，
    /// 在别的形态下永远不会被读），那时对客价由成本单价乘倍率算出来。
    #[serde(default)]
    pub consumer_rates_cny: Option<ConsumerRatesCny>,
    /// 该候选的成本来源口径：`computed` 或 `declared`。
    #[serde(default)]
    pub cost_basis: Option<String>,
    /// 档位价目表（CNY）：**只作定价参考与展示**，不参与预授权、也不参与结算。
    #[serde(default)]
    pub tier_prices: Option<Value>,
    /// 该供给的**保底表**（CNY）：受理时算预授权额的唯一来源。
    #[serde(default)]
    pub floor_amounts: Option<Value>,
}

/// 一条候选**已校验**的定价参考与保底（随修订发布、受理时随 Job 快照冻结）。
///
/// 为什么打包成一个整体、而不是散成几个可空字段：这几样按候选**全有或全无**——只给参考成本而
/// 没给成本来源与保底表，发布出来的候选就是"说不清成本怎么记、也算不出预授权"的半成品。
/// 打包之后"这条候选不带这些"与"带了一半"在类型上就分得开：前者是 `None`，后者发布期就拒。
///
/// **对客费率向量不在这一组里**：它是 `token_rates` 那一种形态的价格，与参考成本、保底表各有
/// 各的用途。成本币种也不在这里：它是这条供给声明的渠道事实（[`OfferingDraft::cost_currency`]）。
#[derive(Debug, Clone, PartialEq)]
pub struct CandidatePricing {
    /// 该候选的渠道成本（**原币种**微单位）：只作定价参考，不是售价的被乘数。
    pub reference_cost_microusd: u64,
    /// 该候选的成本来源口径（两态）。
    pub cost_basis: CostBasis,
    /// 档位价目表（CNY，展示用）。
    pub tier_prices: Value,
    /// 该供给的保底表（CNY）。
    pub floor_amounts: Value,
}

/// Price Plan 草案：**该渠道按 token 计量量计价时的那份四档费率**。
///
/// 它是 `token_rates` 这一种计价形态的参数，不是每条供给的必填——渠道按张 / 按次计价、或直接
/// 由上游给实扣金额时，这条供给没有 Price Plan。四档费率的币种就是该渠道成本币种。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PricePlanDraft {
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

/// 归一后的单个供给：必填校验已完成，`routing_priority` 与 `weight` 已定好。
#[derive(Debug, Clone)]
pub struct NormalizedOffering {
    /// 这条供给**能承载**合同里的哪些字段。
    pub carrier_schema: Value,
    /// 这条供给自己的合同值 → 渠道包装声明。
    pub parameter_mapping: Value,
    pub restrictions: Value,
    pub provider_kind: String,
    pub adapter_key: String,
    pub provider_model_id: String,
    pub base_url: String,
    pub credential_env: String,
    /// 这条供给的**计价形态**（渠道事实，决定成本怎么算）。
    pub formula: PricingFormula,
    /// Price Plan（`token_rates` 的费率参数）；渠道不按 token 计量量计价时为 `None`。
    pub rates: Option<PriceRates>,
    /// Price Plan 的来源 URL（渠道价目的出处）；没有 Price Plan 时为 `None`。
    pub price_source_url: Option<String>,
    /// `per_image` / `per_call` 的单价；另外两种形态为 `None`。
    pub cost_unit_price_microusd: Option<u64>,
    /// 这条供给声明的成本币种；`None` = 没显式声明（取 Price Plan 的币种，旧形状的素材）。
    pub cost_currency: Option<String>,
    /// 这条供给的**对客费率向量**（按 token 计量量的对客价）；`None` = 没给（旧口径按 Price Plan
    /// 的费率收，或这条供给按张 / 按次 / 上游给金额计价、对客价由成本单价乘倍率算出来）。
    pub consumer_rates_cny: Option<ConsumerRatesCny>,
    /// 档位：显式给值就用它，没给就取数组下标。同一档可以有多条候选。
    pub routing_priority: i32,
    /// 档位内的分流比，至少为 1。
    pub weight: u32,
    /// 这条候选的定价；`None` = 它不带定价（旧形状的素材、或只发布了成本费率）。
    pub pricing: Option<CandidatePricing>,
}

impl NormalizedOffering {
    /// 这条供给的**成本币种**：显式声明优先，缺省取它的 Price Plan 币种。
    ///
    /// 两样都没有 = 这条供给没说清它的钱是什么币种（发布期已拒），所以读侧拿到的要么是一个
    /// 答案、要么是旧形状的 `None`。
    #[must_use]
    pub fn cost_currency(&self) -> Option<&str> {
        self.cost_currency
            .as_deref()
            .or_else(|| self.rates.as_ref().map(|rates| rates.currency.as_str()))
    }
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
        // 平台对客名缺省回退取厂商原生名：今天两者同值，老素材不带这个字段也照常可发布。
        // 只写空白等于没写（名字是全空白的话，对客目录会列出一个调不动的名字）。
        let gateway_model = self
            .gateway_model
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| self.native_model_id.clone());
        PublishRuntimeRequest {
            vendor_id: self.vendor_id,
            native_model_id: self.native_model_id,
            gateway_model,
            native_revision: self.native_revision,
            actor: self.actor,
            capability_schema,
            markup_bps: self.markup_bps,
            offerings,
        }
    }

    /// 把命令归一成"一份合同 + 一个有序候选列表"。
    ///
    /// 这是发布接口唯一的入口校验点：`apps/api` 的 `Json<PublishRuntimeCommand>` 反序列化
    /// 之后，下游只处理 [`NormalizedPublication`]。
    pub fn normalize(&self) -> Result<NormalizedPublication, ApplicationError> {
        let drafts = self.offerings.as_deref().ok_or_else(|| {
            ApplicationError::Validation(
                "offerings is required: publish the model's complete, ordered offering list"
                    .to_owned(),
            )
        })?;
        let offerings = self.normalize_array(drafts)?;
        self.validate_markup(&offerings)?;
        Ok(NormalizedPublication {
            contract: self.resolve_contract(drafts)?,
            offerings,
        })
    }

    /// 加价系数**可以缺省**，但不可为负，且不能是一条没人读的记录。
    ///
    /// 它只是**定价时的参考口径**：管理员按"成本单价 × 倍率 × 折算率"推导对客价，按 token 计量量
    /// 的候选也可以直接录入那份四档向量——直接录入时加价系数一次都不参与计算，所以"带对客费率就
    /// 必须给加价系数"会把一条正当的录入挡在门外。要拒的是两件明显自相矛盾的事：负加价等于平台
    /// 倒贴，不是定价（库层也有同一条约束，这里先拒是为了给出说得清的错误）；给了加价系数却没有任何
    /// 候选带**对客费率向量或定价参考**，那它没有任何东西可以解释。
    ///
    /// **反过来，缺它也可能拒**：按张 / 按次计价、或直接由上游给金额的候选没有对客价载体，它们的
    /// 对客价就是"成本单价 × 倍率 × 折算率"——倍率是这条修订唯一的那份，缺了就算不出该收多少钱。
    /// 那是"这条供给没有对客计费基准"，必须发布期拒：按 0 收等于白送，等到结算才发现就晚了一批请求。
    fn validate_markup(&self, offerings: &[NormalizedOffering]) -> Result<(), ApplicationError> {
        let priced = offerings
            .iter()
            .any(|offering| offering.consumer_rates_cny.is_some() || offering.pricing.is_some());
        // 按张 / 按次 / 上游给金额的候选的对客价就是**成本单价乘倍率**：它们的倍率是有人读的。
        let derives_its_price = offerings
            .iter()
            .any(|offering| offering.formula != PricingFormula::TokenRates);
        match self.markup_bps {
            Some(bps) if bps < 0 => Err(ApplicationError::Validation(
                "markup_bps must not be negative".to_owned(),
            )),
            Some(_) if !priced && !derives_its_price => Err(ApplicationError::Validation(
                "markup_bps is given but no offering carries pricing or derives its price from it"
                    .to_owned(),
            )),
            None if derives_its_price => Err(ApplicationError::Validation(
                "markup_bps is required: a supply priced per image / per call / by the amount its \
                 provider declares sells at its cost unit price times the markup coefficient"
                    .to_owned(),
            )),
            _ => Ok(()),
        }
    }

    /// 解析本次发布的**唯一一份合同**。
    ///
    /// 顶层给了就用顶层；顶层没给才回退到候选自带的旧字段——承载面与合同还是同一份的候选
    /// 因此照常可发布。回退时要求所有候选的旧字段**完全一致**：合同是模型级的
    /// 唯一一份，两份不同的内容不能同时成为同一个模型的合同，否则"客户端按合同提交"就没了依据。
    fn resolve_contract(&self, drafts: &[OfferingDraft]) -> Result<Value, ApplicationError> {
        if let Some(contract) = &self.capability_schema {
            return Ok(contract.clone());
        }
        let mut resolved: Option<Value> = None;
        for (index, draft) in drafts.iter().enumerate() {
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

    /// 逐候选归一：承载面、计价形态与参数、档位与档内权重。
    fn normalize_array(
        &self,
        drafts: &[OfferingDraft],
    ) -> Result<Vec<NormalizedOffering>, ApplicationError> {
        if drafts.is_empty() {
            return Err(ApplicationError::Validation(
                "offerings must not be empty".to_owned(),
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
                let billing = normalize_billing(index, draft)?;
                let pricing = normalize_candidate_pricing(index, draft)?;
                Ok(NormalizedOffering {
                    carrier_schema,
                    parameter_mapping: draft.parameter_mapping.clone(),
                    restrictions: draft.restrictions.clone(),
                    provider_kind: draft.provider_kind.clone(),
                    adapter_key: draft.adapter_key.clone(),
                    provider_model_id: draft.provider_model_id.clone(),
                    base_url: draft.base_url.clone(),
                    credential_env: draft.credential_env.clone(),
                    formula: billing.formula,
                    rates: billing.rates,
                    price_source_url: billing.price_source_url,
                    cost_unit_price_microusd: billing.cost_unit_price_microusd,
                    cost_currency: billing.cost_currency,
                    consumer_rates_cny: billing.consumer_rates_cny,
                    routing_priority: normalize_routing_priority(index, draft)?,
                    weight: normalize_weight(index, draft)?,
                    pricing,
                })
            })
            .collect()
    }
}

/// 一个候选在本次受理中的取舍结果，写入 `generation.routing_decisions.considered`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsideredCandidate {
    pub offering_id: OfferingId,
    pub provider_kind: String,
    pub routing_priority: i32,
    /// 这条候选的档位内分流比（它自己的发布值）。
    pub weight: u32,
    /// 本次判定的**分流落点**：`hash(账户 ‖ 幂等键)` 映射到命中档权重之和以内的那个位置。
    ///
    /// 同一次判定里逐项同值（它是"这次分摊落在哪"的一个数，不是每条候选各有一个）。记在判定
    /// 记录里是为了让"为什么是它"**事后可重建**：账户与幂等键随 Job 落库，权重与落点在这里，
    /// 按区间走一遍即可复现选中项——不必依赖任何随机数发生器或外部状态。
    pub weight_draw: u64,
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

/// 归一一条候选的**档位**：显式给了就用它，没给就取数组下标。
///
/// 下标是今天的口径（"顺序即优先级"），保留为缺省值之后，不带这个字段的老素材与老测试
/// 行为逐位不变。显式给值只有一个用途：把**多条候选放进同一档**——下标天然互不相同，
/// 档内按权重分流因此需要一条别的路来表达"这两条是同档"。
///
/// 负数直接拒：档位是顺序而不是偏移量，负号没有含义，放行只会让"最小的档位"变成一个
/// 靠数据才看得出来的约定。
fn normalize_routing_priority(
    index: usize,
    draft: &OfferingDraft,
) -> Result<i32, ApplicationError> {
    match draft.routing_priority {
        Some(priority) if priority < 0 => Err(ApplicationError::Validation(format!(
            "offerings[{index}].routing_priority must not be negative"
        ))),
        Some(priority) => Ok(priority),
        None => i32::try_from(index)
            .map_err(|_| ApplicationError::Validation("too many offerings".to_owned())),
    }
}

/// 归一一条候选的**权重**：缺省 `1`；显式 `0` 拒绝。
///
/// 0 不是"不参与分流"的表达——想不参与就不发这条候选。放行 0 之后，这条候选会永远分不到，
/// 而"为什么分不到"要读一遍分摊代码才知道，那是把配置错误伪装成运行结果。
fn normalize_weight(index: usize, draft: &OfferingDraft) -> Result<u32, ApplicationError> {
    match draft.weight {
        Some(0) => Err(ApplicationError::Validation(format!(
            "offerings[{index}].weight must be a positive integer"
        ))),
        Some(weight) => Ok(weight),
        None => Ok(1),
    }
}

/// 一条候选归一后的**计价事实**：形态 + 它自己的参数 + 成本币种。
struct Billing {
    formula: PricingFormula,
    rates: Option<PriceRates>,
    price_source_url: Option<String>,
    cost_unit_price_microusd: Option<u64>,
    cost_currency: Option<String>,
    consumer_rates_cny: Option<ConsumerRatesCny>,
}

/// 归一一条候选的**计价形态与它的参数**，判据是"这个渠道按什么计价"。
///
/// 形态**必填且取值受控**：说不清一条供给按什么计价，它的成本就没有算法——受理时算不出成本
/// 要么被别的数顶替（把成本算成售价或 0），要么要等到运营核账单才发现。形态与参数**配套**，
/// 不配套就拒并指出缺哪一个：
/// - `token_rates` 要那份四档费率（`price_plan`）；
/// - `per_image` / `per_call` 要一个单价（并按张 / 按次的单位算成本）；
/// - `upstream_declared` 什么参数都不要：金额由渠道在终态直接给出，平台没有可算的东西。
///
/// 反向也拒：给了这种形态用不到的参数（例如 `upstream_declared` 带单价、按张计价带一份四档费率、
/// 或按张计价带一份对客四档向量）说明发布者的意图与声明的形态对不上，而那个数永远不会被读——
/// 留着它只会让人以为它在生效。
///
/// 成本币种取**显式声明**，缺省取 Price Plan 的币种，两份都在就必须一致：成本平面记账、
/// 折算与上游声明的金额都要以它为准，两个字段各说各的就没有唯一答案。没有 Price Plan 时必须
/// 显式声明——那正是"这条供给的钱是什么币种"唯一还剩的来源。
fn normalize_billing(index: usize, draft: &OfferingDraft) -> Result<Billing, ApplicationError> {
    let declared = draft
        .formula
        .as_deref()
        .ok_or_else(|| {
            ApplicationError::Validation(format!(
                "offerings[{index}].formula is required: state how this supply is priced \
                 (token_rates / per_image / per_call / upstream_declared)"
            ))
        })
        .and_then(|value| {
            PricingFormula::parse(value).ok_or_else(|| {
                ApplicationError::Validation(format!(
                    "offerings[{index}].formula must be token_rates, per_image, per_call or \
                     upstream_declared, got {value}"
                ))
            })
        })?;
    if declared == PricingFormula::TokenRates && draft.price_plan.is_none() {
        return Err(ApplicationError::Validation(format!(
            "offerings[{index}].price_plan is required: a supply priced by token metering needs \
             its four rates"
        )));
    }
    if declared != PricingFormula::TokenRates && draft.price_plan.is_some() {
        return Err(ApplicationError::Validation(format!(
            "offerings[{index}].price_plan does not apply to formula {}: the four rates are the \
             parameter of token_rates only",
            declared.as_str()
        )));
    }
    if declared.takes_unit_price() && draft.cost_unit_price_microusd.is_none() {
        return Err(ApplicationError::Validation(format!(
            "offerings[{index}].cost_unit_price_microusd is required: formula {} is priced per \
             unit",
            declared.as_str()
        )));
    }
    if !declared.takes_unit_price() && draft.cost_unit_price_microusd.is_some() {
        return Err(ApplicationError::Validation(format!(
            "offerings[{index}].cost_unit_price_microusd does not apply to formula {}",
            declared.as_str()
        )));
    }
    // 对客四档向量是 `token_rates` 那一种形态的价格：按张 / 按次 / 上游给金额的候选对客价由
    // 成本单价按"× 倍率 × 折算率"算出来，一份向量在这里永远不会被读。留着它只会让人以为
    // 它在生效——发布者的意图与声明的形态对不上时，就该在发布期说清，而不是等对账时才发现
    // 自己录的价没被用。
    if declared != PricingFormula::TokenRates && draft.consumer_rates_cny.is_some() {
        return Err(ApplicationError::Validation(format!(
            "offerings[{index}].consumer_rates_cny does not apply to formula {}: the four CNY \
             rates are the token_rates price, and a supply priced per image / per call / by the \
             amount its provider declares sells at its cost unit price times the markup coefficient",
            declared.as_str()
        )));
    }
    let (rates, price_source_url, plan_currency) = match draft.price_plan.clone() {
        Some(price_plan) => {
            let currency = price_plan.currency.clone();
            let source_url = price_plan.source_url.clone();
            (
                Some(price_plan.into_rates()),
                Some(source_url),
                Some(currency),
            )
        }
        None => (None, None, None),
    };
    let cost_currency = match (&draft.cost_currency, &plan_currency) {
        (Some(declared), Some(plan)) if declared != plan => {
            return Err(ApplicationError::Validation(format!(
                "offerings[{index}].cost_currency ({declared}) must match the price plan currency \
                 ({plan})"
            )));
        }
        (Some(declared), _) => Some(declared.clone()),
        (None, Some(plan)) => Some(plan.clone()),
        (None, None) => {
            return Err(ApplicationError::Validation(format!(
                "offerings[{index}].cost_currency is required when the supply has no price plan: \
                 the declared amount and unit price must say which currency they are in"
            )));
        }
    };
    Ok(Billing {
        formula: declared,
        rates,
        price_source_url,
        cost_unit_price_microusd: draft.cost_unit_price_microusd,
        cost_currency,
        consumer_rates_cny: draft.consumer_rates_cny.clone(),
    })
}

fn empty_object() -> Value {
    Value::Object(Map::new())
}

/// 归一一条候选的**定价参考与保底**：**全有或全无**，形状与取值都在这里拒掉。
///
/// 判据是"这条候选有没有带这几样"，不是"字段齐不齐"：只给参考成本而没给成本来源与保底表，
/// 发布出来的候选就是"说不清成本怎么记、也算不出预授权"的半成品——那种候选一旦生效，
/// 问题要等到结算才暴露。因此带了一半就明确拒绝，并指出缺哪一个。
///
/// **对客费率向量不在这组里**：它是 `token_rates` 那一种形态的价格，与参考成本、保底表各有各的
/// 用途（一个是售价，一个是定价参考与预授权）。渠道按张 / 按次计价或直接由上游给金额时，参考成本
/// 与保底表可以没有着落，但这条供给照样要能卖——它的对客价由成本单价乘倍率算出来。
///
/// 成本币种也不在这里：它是这条供给声明的渠道事实（见 [`normalize_billing`]），带不带定价都要有，
/// 而且只有一个来源。
fn normalize_candidate_pricing(
    index: usize,
    draft: &OfferingDraft,
) -> Result<Option<CandidatePricing>, ApplicationError> {
    let carries_pricing = draft.reference_cost_microusd.is_some()
        || draft.cost_basis.is_some()
        || draft.tier_prices.is_some()
        || draft.floor_amounts.is_some();
    if !carries_pricing {
        return Ok(None);
    }
    let missing = |name: &str| {
        ApplicationError::Validation(format!(
            "offerings[{index}].{name} is required when the candidate carries pricing"
        ))
    };
    let reference_cost_microusd = draft
        .reference_cost_microusd
        .ok_or_else(|| missing("reference_cost_microusd"))?;
    let cost_basis = draft
        .cost_basis
        .as_deref()
        .ok_or_else(|| missing("cost_basis"))
        .and_then(|value| {
            CostBasis::parse(value).ok_or_else(|| {
                ApplicationError::Validation(format!(
                    "offerings[{index}].cost_basis must be computed or declared, got {value}"
                ))
            })
        })?;
    let floor_amounts = draft
        .floor_amounts
        .clone()
        .ok_or_else(|| missing("floor_amounts"))?;
    // 保底表的形状在这里就拒掉：表要在受理时查，等到受理才发现写错，受影响的是一批请求。
    FloorTable::from_json(&floor_amounts).map_err(|message| {
        ApplicationError::Validation(format!("offerings[{index}].floor_amounts: {message}"))
    })?;
    let tier_prices = draft.tier_prices.clone().unwrap_or_else(empty_object);
    if !tier_prices.is_object() {
        return Err(ApplicationError::Validation(format!(
            "offerings[{index}].tier_prices must be an object of (size, quality) → CNY amount"
        )));
    }
    Ok(Some(CandidatePricing {
        reference_cost_microusd,
        cost_basis,
        tier_prices,
        floor_amounts,
    }))
}

/// 选出这次请求走的那条候选：**先定档位，再在档内按权重分摊**。
///
/// 合格 = 该候选自己的 `restrictions` 允许本次分支与图片张数，**且**这条供给的承载面能承载
/// 这次请求**实际用到**的每个字段（图片要能落到它声明的参数名上）。两个条件都必须用该候选
/// 自己的声明判断——这正是「每条供给各自声明承载面、限制只收窄」的落地方式。
///
/// **合格性先于分流**：不合格的候选连分摊的资格都没有——它们不进权重之和、也不在区间里。
/// 于是"权重写得再大"也换不来一次选中，这是"任何策略都不得选中不合格候选"这条硬约束在
/// 本层的落点（策略层接的就是这里算出来的合格集合）。
///
/// 分摊规则：取**合格候选里最小的 `routing_priority`** 作为命中档（这就是"数字小者优先"），
/// 在该档的合格候选里按 `weight` 分摊。落点用 `(账户, 幂等键)` 的哈希，**不用随机数发生器**：
/// 同一请求重放必然落同一条候选，离线也能断言。
///
/// 请求本身先按**合同**校验一次（缺必填、合同外的字段）：那是调用方的参数问题，与选路无关，
/// 因此在这里直接失败，不进候选取舍。
///
/// **一条候选都不合格时返回 [`ApplicationError::NoEligibleOffering`]**：请求本身没违反合同，
/// 是平台的供给面承载不了它——对客必须表现为平台侧故障，不是参数错。同样在调用上游之前失败，
/// 不回退到能力更宽但优先级更低的候选（候选已经全试过了）。
///
/// 不做的事：不因价格重排候选（价格不参与选中），不改写参数映射与承载面——分摊只决定
/// "选中谁"，选中之后的参数准备与冻结路径一字不动。
fn select_candidate(
    request: &CreateImageGenerationRequest,
    branch: ImageBranch,
    candidates: &[OfferingCandidate],
) -> Result<(PublishedOffering, Value, RoutingDecision), ApplicationError> {
    // 零配置路径：一条策略都没有时的选路，也就是策略层引入之前的行为。
    let choice = RouteChoice {
        strategy: RouteStrategy::PriorityFailover,
        discount_rates: &BTreeMap::new(),
        tag_channel_map: &BTreeMap::new(),
        account_tag: None,
    };
    select_candidate_with_strategy(request, branch, candidates, &choice)
}

/// 同 [`select_candidate`]，但由调用方给出这次受理用什么策略、以及该策略要吃的输入。
///
/// 策略只决定"在一批合格候选里挑哪一条"：候选合格与否仍由承载面与分支/张数判定，策略不改它们，
/// 也不改选中之后的参数准备与冻结路径。取值空间只有合格候选——不合格的既不进权重之和，也不在
/// 分摊区间里，**策略指定不了它们**。
fn select_candidate_with_strategy(
    request: &CreateImageGenerationRequest,
    branch: ImageBranch,
    candidates: &[OfferingCandidate],
    choice: &RouteChoice<'_>,
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
                weight: candidate.weight,
                // 落点先占位，定下命中档之后统一回填（它是本次判定一个数，不是每条候选各一个）。
                weight_draw: 0,
                eligible: skip_reason.is_none(),
                skip_reason,
            };
            (published, parameters, considered)
        })
        .collect();
    let Some(chosen) = choose_candidate(&evaluated, request, choice) else {
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
        return Err(ApplicationError::NoEligibleOffering(format!(
            "no offering can carry this request for model {} (revision {revision_id}): {reasons}",
            request.model
        )));
    };
    let (chosen, weight_draw) = chosen;
    let considered = evaluated
        .iter()
        .map(|(_, _, considered)| ConsideredCandidate {
            weight_draw,
            ..considered.clone()
        })
        .collect::<Vec<_>>();
    let (published, parameters, _) = evaluated
        .into_iter()
        .nth(chosen)
        .expect("index just computed");
    let decision = RoutingDecision {
        runtime_revision_id: revision_id,
        chosen_offering_id: published.offering_id,
        considered,
    };
    Ok((published, parameters, decision))
}

/// 选路要用的策略输入：策略本身，以及只有 `user_tag` 才消费的账户标签。
///
/// 输入装在一个结构里而不是逐个当参数：四个策略各吃不同的输入，散成位置参数之后"哪个策略吃
/// 哪个量"就只能靠读调用点才知道。
struct RouteChoice<'a> {
    strategy: RouteStrategy,
    discount_rates: &'a BTreeMap<String, u32>,
    tag_channel_map: &'a BTreeMap<String, String>,
    /// 账户标签：只有 `user_tag` 消费它。别的策略下调用方不会为它多查一次库。
    account_tag: Option<&'a str>,
}

/// 按策略在**合格候选**里选出命中那条：返回下标与本次的分流落点。
///
/// 四条策略共用两条底线：① **取值空间只有合格候选**——不合格的既不进权重之和，也不参与成本比较，
/// 更不会被标签映射指定；② 落点只在按权重分摊的两种策略里有意义，其余策略记 `0`。
///
/// `least_cost` 与 `user_tag` 在"该策略给不出答案"时退回默认顺序（`priority_failover`）：
/// 前者是没有任何候选带成本估算，后者是标签没配映射、或映射指向的候选这次承载不了。退回而不是
/// 判失败——别的候选明明能承载这次请求，把它们一起判掉没有任何好处；退回的顺序是确定的，
/// 仍然满足"同一请求重放落同一条"。
fn choose_candidate(
    evaluated: &[(PublishedOffering, Value, ConsideredCandidate)],
    request: &CreateImageGenerationRequest,
    choice: &RouteChoice<'_>,
) -> Option<(usize, u64)> {
    match choice.strategy {
        RouteStrategy::PriorityFailover => choose_by_priority_and_weight(evaluated, request),
        RouteStrategy::WeightedRandom => choose_by_weight_across_all(evaluated, request),
        RouteStrategy::LeastCost => choose_least_cost(evaluated, choice)
            .or_else(|| choose_by_priority_and_weight(evaluated, request)),
        RouteStrategy::UserTag => choose_by_tag(evaluated, choice)
            .or_else(|| choose_by_priority_and_weight(evaluated, request)),
    }
}

/// 档位顺序 + 档内按权重分摊：默认策略，也是策略层引入之前的行为。
///
/// 定档位时权重不参与：合格候选里最小的 `routing_priority` 先定下来，因此"档 0 有合格候选"时
/// 权重再小的候选也不会被后面的档抢走。
fn choose_by_priority_and_weight(
    evaluated: &[(PublishedOffering, Value, ConsideredCandidate)],
    request: &CreateImageGenerationRequest,
) -> Option<(usize, u64)> {
    let tier = evaluated
        .iter()
        .filter(|(_, _, considered)| considered.eligible)
        .map(|(_, _, considered)| considered.routing_priority)
        .min()?;
    let pool: Vec<usize> = (0..evaluated.len())
        .filter(|index| {
            let considered = &evaluated[*index].2;
            considered.eligible && considered.routing_priority == tier
        })
        .collect();
    split_by_weight(evaluated, request, pool)
}

/// 不看档位：**全部**合格候选按权重分摊。
fn choose_by_weight_across_all(
    evaluated: &[(PublishedOffering, Value, ConsideredCandidate)],
    request: &CreateImageGenerationRequest,
) -> Option<(usize, u64)> {
    let pool: Vec<usize> = (0..evaluated.len())
        .filter(|index| evaluated[*index].2.eligible)
        .collect();
    split_by_weight(evaluated, request, pool)
}

/// 折后成本估算最小的一条。
///
/// 估算 = 该候选的**参考成本** × 它配的折扣率（没配就是不打折）。参考成本是发布者给的**定价
/// 参考**，不是成本事实：这里只拿它排序，成本事实仍按实际扣费记（渠道声明多少就是多少）。
/// 没有参考成本的候选排最后——拿不到估算就没法参与比较，但它仍是合格候选，只有在**谁都没有**
/// 估算时才整体退回默认顺序。
///
/// 比较用 `u128`：参考成本是 `u64`，乘上万分比会溢出 `u64`。
fn choose_least_cost(
    evaluated: &[(PublishedOffering, Value, ConsideredCandidate)],
    choice: &RouteChoice<'_>,
) -> Option<(usize, u64)> {
    let mut best: Option<(usize, u128)> = None;
    for (index, (published, _, considered)) in evaluated.iter().enumerate() {
        if !considered.eligible {
            continue;
        }
        let Some(cost) = published.price_snapshot.reference_cost_microusd else {
            continue;
        };
        let rate = choice
            .discount_rates
            .get(&considered.offering_id.0.to_string())
            .copied()
            .unwrap_or(NO_DISCOUNT_RATE);
        let discounted = u128::from(cost) * u128::from(rate);
        let better = match best {
            None => true,
            // 同价时按 `offering_id` 升序定胜负：比较结果不能取决于取数顺序。
            Some((best_index, current)) => {
                (discounted, considered.offering_id.0)
                    < (current, evaluated[best_index].2.offering_id.0)
            }
        };
        if better {
            best = Some((index, discounted));
        }
    }
    best.map(|(index, _)| (index, 0))
}

/// 账户标签经映射指定的那条候选——它**必须合格**。
///
/// 标签没配映射、映射指向的候选这次承载不了这次请求，两者都不算数：返回 `None`，由调用方退回
/// 默认顺序。映射**不是**绕过承载校验的入口。
fn choose_by_tag(
    evaluated: &[(PublishedOffering, Value, ConsideredCandidate)],
    choice: &RouteChoice<'_>,
) -> Option<(usize, u64)> {
    let mapped = choice.tag_channel_map.get(choice.account_tag?)?;
    (0..evaluated.len())
        .find(|index| {
            let considered = &evaluated[*index].2;
            considered.eligible && considered.offering_id.0.to_string() == *mapped
        })
        .map(|index| (index, 0))
}

/// 把候选集合按 `weight` 分成区间，返回落点所在的那一条与落点本身。
///
/// 区间划分的**顺序按 `offering_id` 升序**，不按数据库返回的行序：落点是哈希出来的一个数，
/// 若区间划分依赖行序，同一请求换个取数顺序就会分到另一条候选，"可重放"就成了空话。
/// 定序键必须是与请求无关的发布数据，`offering_id` 满足这一点。
///
/// 权重之和用 `u64` 累加：权重本身是 `u32`，多条候选相加可能溢出 `u32`。
fn split_by_weight(
    evaluated: &[(PublishedOffering, Value, ConsideredCandidate)],
    request: &CreateImageGenerationRequest,
    mut pool: Vec<usize>,
) -> Option<(usize, u64)> {
    // 定序键用 `offering_id` 里的 UUID 本身：`OfferingId` 是个新类型，没有比较语义，
    // 而这里要的只是"每次取数都排出同一个顺序"，不是任何业务顺序。
    pool.sort_by_key(|index| evaluated[*index].2.offering_id.0);
    // 集合里至少有一条合格候选 ⇒ 权重至少是 1 ⇒ 总和至少是 1，取模不会除以零。
    let total: u64 = pool
        .iter()
        .map(|index| u64::from(evaluated[*index].2.weight))
        .sum();
    let draw = weight_split_draw(request.account_id, &request.idempotency_key) % total;
    let mut cursor = 0_u64;
    for index in pool {
        cursor += u64::from(evaluated[index].2.weight);
        if draw < cursor {
            return Some((index, draw));
        }
    }
    // 落点必然落在某条候选的区间里（总和就是全部区间），走不到这里。
    None
}

/// 不打折的折扣率：万分比。没给某条候选配折扣率时用它，免得把"没配"读成"零成本"。
const NO_DISCOUNT_RATE: u32 = 10_000;

/// 权重分摊的落点：`sha256(账户 ‖ 幂等键)` 取前 8 字节（大端）。
///
/// 输入取 `(账户, 幂等键)` 而不是 JobId：选路发生在 JobId 生成**之前**，拿一个当时还不存在的
/// 值当输入是因果倒置。这两个值在受理前就已知，而且**幂等键只在账户内唯一**——把账户也放进来，
/// 不同账户用同一个键时才不会互相关联。
///
/// 账户是定宽 UUID，直接拼在幂等键前面即可：定宽前缀让"拼在哪里断开"没有歧义，不需要分隔符。
/// 幂等键是调用方给的文本，因此这里用哈希而不是取模原始字节——哈希把它摊平到整个取值空间。
fn weight_split_draw(account_id: AccountId, idempotency_key: &str) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(account_id.0.as_bytes());
    hasher.update(idempotency_key.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(bytes)
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
    /// 这次执行**已经看到**的成本事实（成本平面）。
    ///
    /// 执行已经发生、上游成本也拿得到，成本事实就必须有去处——只有成功路径才落成本，等于把
    /// "这笔到底花了多少钱"丢在一条已经付过钱的路径上。Driver 在终态之后判定失败时把已读到的
    /// 成本附在错误上，这里原样落库；Driver 没报回来时记 `unavailable`（来源可辨、进缺口清单）。
    /// 只有"请求根本没交到渠道"的执行（凭证取不到、Driver 装不起来、参数被挡在请求之外）才是
    /// `None`：那时的 NULL 说的是"根本没采"，不是"成本是 0"。
    pub provider_cost: Option<ProviderCostFact>,
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
    /// 这次执行看到的**成本事实**（成本平面，原币种）。与 `evidence` 并列但**不是同一件事**：
    /// 计量事实是上游给的分项 token，成本只进毛利口径，不改对客金额。
    pub provider_cost: ProviderCostFact,
}

#[derive(Debug, Clone)]
pub struct RefundReconciliationCommand {
    pub job_id: JobId,
    pub note: String,
    pub business_key: String,
    pub actor: String,
}

/// 待录入的一行折算率：`effective_at` 为 `None` 表示"立即生效"，**由数据库盖章**。
///
/// 它与 [`FxRate`] 回答的不是同一个问题：[`FxRate`] 是"库里那一行已生效的折算率"（读出来带着
/// 库给的时刻），这里还没定时刻——不给就是让库用它的 `now()` 定。生效时刻之所以不能由进程
/// 时钟给：发布期校验与受理取值用的都是库的 `now()`，两个时钟一旦漂移，"录完立刻发布"就会被
/// 误判成"该币种还没有生效的折算率"。
#[derive(Debug, Clone)]
pub struct NewFxRate {
    pub currency: String,
    pub rate_micros: u64,
    pub effective_at: Option<DateTime<Utc>>,
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

/// 管理员视图里的一条候选供给。
///
/// 它是**只读投影**：候选的定义（承载面、映射、顺序）来自生效修订，可走与否来自供给与渠道
/// 自己的开关。**不回显渠道凭证**——`credential_env` 只是变量名，本来就不进响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayModelCandidateView {
    pub offering_id: OfferingId,
    /// 渠道类别（例如 AIHubMix / APIMart）。
    pub provider_kind: String,
    /// 这条供给在渠道侧用的模型名。
    pub provider_model_id: String,
    /// 用哪个 Driver 发出去。
    pub adapter_key: String,
    /// 选择顺序：数字小者优先。它**缺省等于候选在发布数组里的下标**，也可以由发布者显式给出
    /// （显式给值是为了让多条候选落在同一档）。它是**档位**，同一档内再按权重分摊。
    pub routing_priority: i32,
    /// 这条候选在**档位内**的分流比；同一档有多条合格候选时按它分摊。
    pub weight: u32,
    /// 这条候选现在**真的能走**吗：供给与它所在渠道都启用。
    ///
    /// 与目录/受理的判据同一条——运营要能一眼看出"目录里为什么没有它"。
    pub enabled: bool,
    /// 这条供给**能承载**合同里的哪些字段。
    pub carrier_schema: Value,
    /// 这条供给自己的合同值 → 渠道包装声明。
    pub parameter_mapping: Value,
    /// 该候选的**对客四档 CNY 费率向量**（随修订发布）；这条候选不带定价时为 `null`。
    pub consumer_rates_cny: Option<ConsumerRatesCny>,
    /// 该候选的渠道成本（**原币种**微单位）：**只作定价参考，不是售价的被乘数**。
    pub reference_cost_microusd: Option<u64>,
    /// 该候选的成本币种（不假定 USD）。
    pub cost_currency: Option<String>,
    /// 该候选的成本来源口径（两态）。
    pub cost_basis: Option<CostBasis>,
    /// 档位价目表（CNY）：只作定价参考与展示，不参与预授权。
    pub tier_prices: Option<Value>,
    /// 该供给的**保底表**（CNY）：受理时算预授权额的查表依据。
    pub floor_amounts: Option<Value>,
}

/// 管理员视图里的一个网关模型：一条只读投影。
///
/// 数据源是生效修订（条目 + 修订 + 厂商模型合同）加运维开关。定义只能由发布产生，
/// 这里**不新增编辑态**，也不回显渠道凭证。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayModelView {
    /// 平台对客名：客户端提交 `model` 时用的那个名字。
    pub gateway_model: String,
    /// 运维开关：关掉之后它从对客目录消失、受理得到"模型不存在"；已受理的 Job 不受影响。
    pub enabled: bool,
    pub vendor_id: String,
    /// 厂商原生名：**只在管理端出现**，对客面看不到它。
    pub native_model_id: String,
    /// 合同修订。
    pub native_revision: String,
    /// 当前生效的那一次发布。
    pub runtime_revision_id: RuntimeRevisionId,
    pub published_at: DateTime<Utc>,
    /// **加价系数**（基点）：每个网关模型一个，随修订发布；没有带定价的候选时为 `null`。
    pub markup_bps: Option<i32>,
    /// 候选清单，按 `routing_priority` 升序。
    pub candidates: Vec<GatewayModelCandidateView>,
}

/// 一条**成本缺口**：执行发生了、成本本该有金额，却拿不到（`unavailable`）。
///
/// 它**不进对账态、也不开对账案例**：对账态是"受理/执行状态不明"，会把消费者的钱扣在对账里；
/// 成本缺口是**平台侧的账务缺口**——对客结算照常按费率快照完成，消费者的钱该扣的照扣。所以
/// 缺口由这张运营清单承载，毛利侧标"成本未知"（金额与折算值留空，不写 0、不用费率顶替）。
///
/// `provider_trace_id` 是人工去上游核账单的依据——没有它，核账的人不知道该查哪个任务。
/// 人工核对后补录金额归账实核对那条线；补录完成后这一笔不再出现在清单里。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderCostGapView {
    pub job_id: JobId,
    pub attempt_id: AttemptId,
    pub account_id: AccountId,
    /// 平台型号名。
    pub gateway_model: String,
    /// 渠道类别（例如 AIHubMix / APIMart）；没有渠道信息时为 `None`。
    pub provider_kind: Option<String>,
    pub provider_trace_id: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Error)]
pub enum ApplicationError {
    #[error("validation failed: {0}")]
    Validation(String),
    /// 调用方这次请求本身在**参数上**不成立：图片字段是"合同外字段丢弃"的例外，合同没为它留位置
    /// 时不能丢（丢图等于悄悄生成一张没有参考图的图），因此单独一个类别——对客要说得比一般校验
    /// 失败更具体。
    #[error("invalid parameter: {0}")]
    InvalidParameter(String),
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

    /// 对客目录的取数：当前真的能调的模型，一个型号一条，带它那份模型级合同。
    ///
    /// 判据与 [`Self::active_offering`] **同一条**（生效的发布条目 + 启用的供给 + 启用的渠道）：
    /// 目录里列出的型号必须真的受理得起来——取不到任何候选的型号，受理期对调用方是"不存在"，
    /// 因此也不该出现在目录里。一个可调型号都没有时返回空集合，不是错误。
    async fn published_models(&self) -> Result<Vec<PublishedModel>, ApplicationError>;

    /// 管理员读：当前有生效定义的网关模型，一条一项，带候选清单与运维开关。
    ///
    /// 一条都不可调（候选全被停用）的网关模型**照样列出来**——运营要能看见它、并据此决定
    /// 是重新启用还是重发；把它藏起来等于"关掉之后再也找不到怎么打开"。
    async fn gateway_models(&self) -> Result<Vec<GatewayModelView>, ApplicationError>;

    /// 管理员写：只改运维开关，写一条审计事件。
    ///
    /// 没发布过的名字返回 [`ApplicationError::NotFound`]：定义只能由发布产生，这里**不创建**
    /// 任何东西（不做分步 CRUD）。
    async fn set_gateway_model_enabled(
        &self,
        gateway_model: &str,
        enabled: bool,
        actor: &str,
    ) -> Result<(), ApplicationError>;

    /// 管理员写：只改一条**供给**的启用开关，写一条审计事件。
    ///
    /// 返回这条供给现在出现在哪些网关模型的**生效**候选集里——调用方拿这些名字失效 route 缓存。
    /// 启停不改变修订标识，缓存里那份候选集在停用之后仍然"看起来是新的"，只能靠失效拿掉。
    ///
    /// 没发布过的供给 id 返回 [`ApplicationError::NotFound`]：定义只能由发布产生，这里**不创建**
    /// 任何东西。停用只影响之后的受理——已受理 Job 的候选与定价早已随快照冻结在 Job 上。
    async fn set_offering_enabled(
        &self,
        offering_id: OfferingId,
        enabled: bool,
        actor: &str,
    ) -> Result<Vec<String>, ApplicationError>;

    /// 管理员写：只改一条**渠道**的启用开关，写一条审计事件。
    ///
    /// 判据与 [`Self::set_offering_enabled`] 同一条：渠道经它名下的供给影响候选集，返回的也是
    /// 受影响的网关模型名。
    async fn set_channel_enabled(
        &self,
        channel_id: ChannelId,
        enabled: bool,
        actor: &str,
    ) -> Result<Vec<String>, ApplicationError>;

    /// 管理员写：录入一行折算率（渠道币种 → CNY），写一条审计事件。
    ///
    /// 汇率是**外部事实**，按币种维护、带生效时间；它不是修订的内容——同一时刻同一币种全平台
    /// 必须是同一个数才对账得起来，放进每份发布里改一次汇率就要重发所有型号。
    ///
    /// 请求没给生效时刻时，端口**不替它取一个时钟**：由实现交给数据库的 `now()` 盖章。发布期
    /// 校验与受理取值都用库的 `now()`，盖章的时钟必须是同一个。
    async fn upsert_fx_rate(&self, rate: NewFxRate, actor: &str) -> Result<(), ApplicationError>;

    /// 取该币种**受理时刻生效的那一行**折算率（受理时刻之前已生效、其中最新的一行）。
    ///
    /// 没有可用行时返回 `None`：发布期已经拒绝过"没有折算率的币种"，所以这里取不到只可能是
    /// 汇率表被改过，由调用方按平台侧配置问题处置。
    async fn effective_fx_rate(&self, currency: &str) -> Result<Option<FxRate>, ApplicationError>;

    /// 成本缺口清单（运营只读）：执行发生了、成本本该有金额却拿不到的那些执行尝试。
    ///
    /// 按完成时间倒序，`limit` 为条数上限——由调用方按 [`MAX_OPERATIONAL_LIMIT`] 收窄一次，
    /// 这里不再重复收窄（两处各收一次，两边一旦改成不同的数，响应里的 `truncated` 就会与
    /// 实际返回的条数对不上）。它不进对账态——见 [`ProviderCostGapView`]。
    async fn provider_cost_gaps(
        &self,
        limit: u32,
    ) -> Result<Vec<ProviderCostGapView>, ApplicationError>;

    /// 受理前的**轻量读**：网关模型开关、当前生效的修订标识与数据库时钟。
    ///
    /// 它只服务加速层：route 缓存里的值带着写它那次发布的修订标识，与这里读到的比对，不一致就
    /// 回源；开关必须**单独**读，因为 `PATCH enabled` 不改变修订标识——交给缓存判定的话，关掉的
    /// 模型会在缓存的有效期内继续被受理。时钟也一起取，好让"缓存值新不新鲜"用**同一个时钟**判。
    ///
    /// 账户与幂等键只用来判**这次是不是重放**（同一个键已经建过 Job）：重放不新建、不扣款，
    /// 因此余额预检不该管它。这一项折在同一条查询里读，受理不会因此多一次往返。
    ///
    /// 没有这个网关模型时返回 `enabled = false` 与 `None`，不是错误：受理侧对它的处置与
    /// "取不到任何候选"一样（对客是"模型不存在"）。
    async fn acceptance_probe(
        &self,
        gateway_model: &str,
        account_id: AccountId,
        idempotency_key: &str,
    ) -> Result<AcceptanceProbe, ApplicationError>;

    /// 对账用的增量取数：`updated_at` 落在最近 `window` 内的账户与它们的余额。
    ///
    /// 窗口在**库侧**用 `now() - interval` 算：受理、对账、缓存里的写入时间取的都是数据库的
    /// 时间，换成进程时钟就会因为漂移把刚变过的账户漏掉（或把没变过的算进来）。
    async fn accounts_updated_within(
        &self,
        window: Duration,
    ) -> Result<Vec<BalanceChange>, ApplicationError>;

    /// 写一条审计事件（平台侧事件必须可发现）。
    ///
    /// 与业务写入**分开一个事务**：它的两个调用点都是"业务已经定局、现在要留痕"——凭缓存提前
    /// 拒绝（根本没有业务写入）与对账覆盖（缓存不是账本）。塞进业务事务里会让留痕变成"能不能
    /// 拒绝"的前置条件，那是反过来的依赖。
    async fn insert_audit_event(
        &self,
        actor: &str,
        action: &str,
        subject_type: &str,
        subject_id: &str,
        payload: Value,
    ) -> Result<(), ApplicationError>;

    /// 建账户。返回的是**数据库里那个账户**变更后的余额与写入时刻：调用方要把它写进缓存
    /// （写穿），而缓存里的写入时间要参与新鲜度判定与审计，只能用数据库盖章的那个时间。
    async fn create_account(
        &self,
        account_id: AccountId,
        initial_credit_microusd: u64,
        actor: &str,
    ) -> Result<BalanceChange, ApplicationError>;

    /// 充值。返回提交后的余额与写入时刻（同一个幂等键重放时返回**当前**余额，让缓存跟着刷新）。
    async fn credit_account(
        &self,
        account_id: AccountId,
        amount_microusd: u64,
        business_key: &str,
        actor: &str,
    ) -> Result<BalanceChange, ApplicationError>;

    /// 读账户**当前**的余额与写入时刻。
    ///
    /// 权威是 `ledger.accounts` 那一行，**不读缓存**：这条读服务于运营查看与对账，缓存里的值
    /// 可能滞后、也可能来自对账覆盖，用它当答案会把账实不符读成账实相符。
    async fn read_account_balance(
        &self,
        account_id: AccountId,
    ) -> Result<BalanceChange, ApplicationError>;

    /// 账户标签（运营设）：只有生效的 `user_tag` 策略消费它，所以只有那种策略下才查它。
    async fn account_tag(&self, account_id: AccountId) -> Result<Option<String>, ApplicationError>;

    /// 设账户标签：`None` 表示清掉。写审计——它是管理员面的配置，改它会影响之后的受理。
    async fn set_account_tag(
        &self,
        account_id: AccountId,
        tag: Option<&str>,
        actor: &str,
    ) -> Result<(), ApplicationError>;

    /// 该网关模型生效的**路由策略**：按模型覆盖优先，其次全局那条；都没有就是 `None`
    /// （调用方按默认 `priority_failover` 走）。
    ///
    /// 策略是运行期配置，**不进不可变修订**：改它即刻影响之后的受理；已受理的 Job 早已把候选
    /// 固定在快照里，不受后续改策略影响。
    async fn route_policy(
        &self,
        gateway_model: &str,
    ) -> Result<Option<RoutePolicy>, ApplicationError>;

    /// 写入（或覆盖）一条策略：`gateway_model` 为 `None` 写全局那条。每次写入换新的版本标识，
    /// 缓存拿它判断自己是不是旧的。
    async fn upsert_route_policy(
        &self,
        policy: &RoutePolicy,
        actor: &str,
    ) -> Result<(), ApplicationError>;

    /// 管理员看的策略清单：全局那条（若有）与各网关模型的覆盖。
    async fn route_policies(&self) -> Result<Vec<RoutePolicy>, ApplicationError>;

    async fn create_api_key(
        &self,
        account_id: AccountId,
        label: &str,
        key_hash: &str,
        actor: &str,
    ) -> Result<(), ApplicationError>;

    async fn account_for_api_key(&self, key_hash: &str) -> Result<AccountId, ApplicationError>;

    /// 创建 Job，并与 Job **同事务**写入路由判定记录。
    ///
    /// 返回 Job 与**预授权扣减之后**的余额：受理的预授权扣减同样要写穿缓存，否则缓存会滞后
    /// 一个预授权额。幂等重放（没有扣减）也返回当前余额——把缓存刷成数据库的值不会有坏处。
    async fn create_job(
        &self,
        command: CreateImageGeneration,
        branch: ImageBranch,
        offering: PublishedOffering,
        request_hash: String,
        routing: RoutingDecision,
    ) -> Result<(GenerationJob, BalanceChange), ApplicationError>;

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

    /// 结算（`release` + `capture`）。返回结算**之后**的余额，供调用方写穿缓存。
    async fn complete_job(
        &self,
        completion: CompleteJob,
    ) -> Result<BalanceChange, ApplicationError>;

    /// 失败收尾。释放预授权的那些分支会改动余额，因此同样返回提交后的余额供写穿缓存——
    /// 不写的话，缓存里会留着一个"刚写过、但偏高"的余额，那正是能被用来误拒的那类值。
    async fn fail_job(
        &self,
        job_id: JobId,
        worker_id: &str,
        attempt_id: Option<AttemptId>,
        failure: AttemptFailure,
    ) -> Result<BalanceChange, ApplicationError>;

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

    /// 对账退款（释放预授权）。返回**账户**与退款后的余额：调用方要按账户把余额写穿缓存。
    async fn refund_reconciliation(
        &self,
        command: RefundReconciliationCommand,
    ) -> Result<BalanceChange, ApplicationError>;
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
    acceleration: Arc<AccelerationService>,
}

impl ReconciliationService {
    #[must_use]
    pub fn new(repository: Arc<dyn HubRepository>) -> Self {
        let acceleration = Arc::new(AccelerationService::disabled(repository.clone()));
        Self {
            repository,
            acceleration,
        }
    }

    /// 装上加速层：退款释放了预授权、余额变了，缓存要跟着刷新。
    #[must_use]
    pub fn with_acceleration(mut self, acceleration: Arc<AccelerationService>) -> Self {
        self.acceleration = acceleration;
        self
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
        let change = self.repository.refund_reconciliation(command).await?;
        self.acceleration
            .write_balance(&change, BalanceSource::DbCommit)
            .await;
        Ok(())
    }
}

/// 运营清单类接口一次最多返回多少条（不翻页，所以必须有个上限）。
///
/// 上限**只收在这一处**：HTTP 层解析查询参数时按它收窄一次，用例侧不再重复收窄。两处各收一次
/// 的代价是两边会各自漂移，而漂移的表现是"清单被截断了吗"这个判断（响应里的 `truncated`）
/// 与实际返回的条数对不上。
pub const MAX_OPERATIONAL_LIMIT: u32 = 500;

/// 定价侧的管理员用例：**折算率**的录入与取值，以及**成本缺口**的只读清单。
///
/// 它不碰修订内容——汇率不进不可变修订（改一次汇率要重发所有型号，而且同一时刻同一币种
/// 全平台必须是同一个数才对账得起来）。售价向量仍随修订发布、随 Job 快照冻结。
///
/// 成本缺口清单也归这里：它问的是"哪几笔成本没记上"，属成本事实那一侧，**不是对账案例**
/// ——把两件事挂在同一个服务上，会让那个服务因为两种不相干的理由被改。
#[derive(Clone)]
pub struct PricingService {
    repository: Arc<dyn HubRepository>,
}

impl PricingService {
    #[must_use]
    pub fn new(repository: Arc<dyn HubRepository>) -> Self {
        Self { repository }
    }

    /// 成本缺口清单（运营只读）：执行发生了、成本本该有金额却拿不到的那些执行尝试。
    ///
    /// 它**不是**对账案例：这些 Job 的对客结算已经按费率快照正常完成，消费者的钱该扣的照扣。
    /// 缺口是平台侧的账务缺口——运营拿上游对账标识去核账单，补录归账实核对那条线。
    pub async fn provider_cost_gaps(
        &self,
        limit: u32,
    ) -> Result<Vec<ProviderCostGapView>, ApplicationError> {
        self.repository.provider_cost_gaps(limit).await
    }

    /// 录入一行折算率（管理员，写审计）。同一币种同一生效时刻只能有一行——取值规则是
    /// "受理时刻生效的那一行"，两行同时刻就没有唯一答案。
    ///
    /// 生效时刻**原样透传**（`None` 就是没给）：这里不补一个进程时钟，缺省由数据库盖章。
    pub async fn upsert_fx_rate(
        &self,
        rate: NewFxRate,
        actor: &str,
    ) -> Result<(), ApplicationError> {
        let currency = rate.currency.trim();
        if currency.is_empty() {
            return Err(ApplicationError::Validation(
                "fx rate currency must not be empty".to_owned(),
            ));
        }
        if rate.rate_micros == 0 {
            return Err(ApplicationError::Validation(
                "fx rate must be positive".to_owned(),
            ));
        }
        self.repository
            .upsert_fx_rate(
                NewFxRate {
                    currency: currency.to_owned(),
                    ..rate
                },
                actor,
            )
            .await
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

/// 账户面的管理员用例：建账户与充值。
///
/// 单独一个服务，是因为这两件事都要在**数据库提交成功之后**把余额写进缓存（写穿）：写在仓库里
/// 会让持久化实现同时懂缓存，写在 HTTP 处理器里则会让"充值"这条路径有两处各写一次缓存。
#[derive(Clone)]
pub struct AccountsService {
    repository: Arc<dyn HubRepository>,
    acceleration: Arc<AccelerationService>,
}

impl AccountsService {
    #[must_use]
    pub fn new(repository: Arc<dyn HubRepository>) -> Self {
        let acceleration = Arc::new(AccelerationService::disabled(repository.clone()));
        Self {
            repository,
            acceleration,
        }
    }

    /// 装上加速层：充值后缓存要立即可见。
    #[must_use]
    pub fn with_acceleration(mut self, acceleration: Arc<AccelerationService>) -> Self {
        self.acceleration = acceleration;
        self
    }

    pub async fn create_account(
        &self,
        account_id: AccountId,
        initial_credit_microusd: u64,
        actor: &str,
    ) -> Result<(), ApplicationError> {
        let change = self
            .repository
            .create_account(account_id, initial_credit_microusd, actor)
            .await?;
        self.acceleration
            .write_balance(&change, BalanceSource::DbCommit)
            .await;
        Ok(())
    }

    pub async fn credit_account(
        &self,
        account_id: AccountId,
        amount_microusd: u64,
        business_key: &str,
        actor: &str,
    ) -> Result<(), ApplicationError> {
        let change = self
            .repository
            .credit_account(account_id, amount_microusd, business_key, actor)
            .await?;
        self.acceleration
            .write_balance(&change, BalanceSource::DbCommit)
            .await;
        Ok(())
    }

    /// 读账户余额与写入时刻（权威在数据库）。
    ///
    /// 刻意**不**走加速层：缓存的值可能滞后、也可能来自对账覆盖，而这条读的用途正是查看与
    /// 对账——把缓存的数当答案，等于在"账实是否相符"这个问题上拿被怀疑的一方作证。
    pub async fn read_balance(
        &self,
        account_id: AccountId,
    ) -> Result<BalanceChange, ApplicationError> {
        self.repository.read_account_balance(account_id).await
    }

    /// 设账户标签（管理员面）：只有生效的 `user_tag` 策略消费它，没有那种策略时它不改变任何
    /// 选路结果。
    pub async fn set_tag(
        &self,
        account_id: AccountId,
        tag: Option<&str>,
        actor: &str,
    ) -> Result<(), ApplicationError> {
        self.repository
            .set_account_tag(account_id, tag, actor)
            .await
    }
}

/// 路由策略的管理员面：读清单与写入。
///
/// 策略是**运行期配置**（不进不可变修订），所以这里没有"发布"这一步：写入成功即刻影响之后的
/// 受理，已经受理的 Job 不受影响——它们的候选早已固定在快照里。
#[derive(Clone)]
pub struct RoutePolicyService {
    repository: Arc<dyn HubRepository>,
}

impl RoutePolicyService {
    #[must_use]
    pub fn new(repository: Arc<dyn HubRepository>) -> Self {
        Self { repository }
    }

    /// 管理员看的清单：全局那条（若有）与各网关模型的覆盖。
    pub async fn list(&self) -> Result<Vec<RoutePolicy>, ApplicationError> {
        self.repository.route_policies().await
    }

    /// 写入（或覆盖）一条策略：`gateway_model` 为 `None` 写全局那条。
    ///
    /// 版本标识在这里换新，不由调用方给：两次写入若用同一个版本，缓存就分辨不出"改过了"，
    /// 会继续按旧策略选路。
    ///
    /// 两张输入表一起写：策略与它的输入是一次配置的两个部分，分开写会出现"策略换了、输入还是
    /// 上一套"的中间状态，而受理正是按这两者共同决定的。
    pub async fn upsert(
        &self,
        gateway_model: Option<&str>,
        strategy: RouteStrategy,
        discount_rates: BTreeMap<String, u32>,
        tag_channel_map: BTreeMap<String, String>,
        actor: &str,
    ) -> Result<RoutePolicy, ApplicationError> {
        let policy = RoutePolicy {
            gateway_model: gateway_model.map(ToOwned::to_owned),
            strategy,
            discount_rates,
            tag_channel_map,
            version: Uuid::new_v4().to_string(),
        };
        self.repository.upsert_route_policy(&policy, actor).await?;
        Ok(policy)
    }
}

/// 加速层的最小能力面：按字符串键读写一个字符串值。
///
/// **语义只到这里为止**：键名、值长什么样、什么时候能拿缓存下结论，全在
/// [`AccelerationService`] 里；实现只负责把这三条命令发给缓存服务。接口这么窄是故意的——
/// 一旦让实现方也懂"余额"与"候选集"，两边的语义就会各自漂移，而漂移的表现是"缓存说的和
/// 数据库说的不一样"。
///
/// 所有方法都可能失败。调用方一律把失败当"这次没命中"，回源数据库：缓存出问题不该让任何
/// 请求失败，也不该改变任何结果。
#[async_trait]
pub trait CacheStore: Send + Sync {
    async fn get(&self, key: &str) -> Result<Option<String>, ApplicationError>;

    /// 写入并设置存活时间。写进去的值**永远是数据库提交之后的值**（见 [`AccelerationService`]）。
    async fn set(&self, key: &str, value: &str, ttl: Duration) -> Result<(), ApplicationError>;

    async fn delete(&self, key: &str) -> Result<(), ApplicationError>;
}

/// 加速层的运行参数。
///
/// 新鲜窗口必须**显著小于**对账周期：对账写回的条目来源标记是 `reconciler`、本来就**不用于**
/// 提前拒绝，这条比例关系是第二道保险——它保证"能用来拒绝的值"实际都来自写穿路径。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CachePolicy {
    pub route_ttl: Duration,
    pub balance_ttl: Duration,
    pub freshness_window: Duration,
    pub reconcile_interval: Duration,
}

impl CachePolicy {
    /// 新鲜窗口至少要比对账周期小这么多倍。
    pub const FRESHNESS_TO_RECONCILE_RATIO: u32 = 4;

    pub fn new(
        route_ttl: Duration,
        balance_ttl: Duration,
        freshness_window: Duration,
        reconcile_interval: Duration,
    ) -> Result<Self, ApplicationError> {
        if route_ttl.is_zero()
            || balance_ttl.is_zero()
            || freshness_window.is_zero()
            || reconcile_interval.is_zero()
        {
            return Err(ApplicationError::Configuration(
                "cache durations must be positive".to_owned(),
            ));
        }
        if freshness_window.saturating_mul(Self::FRESHNESS_TO_RECONCILE_RATIO) > reconcile_interval
        {
            return Err(ApplicationError::Configuration(format!(
                "the cache freshness window ({freshness_window:?}) must be at least {} times \
                 smaller than the reconcile interval ({reconcile_interval:?})",
                Self::FRESHNESS_TO_RECONCILE_RATIO
            )));
        }
        Ok(Self {
            route_ttl,
            balance_ttl,
            freshness_window,
            reconcile_interval,
        })
    }

    /// 默认参数：设计里给的那一套（route 60 秒、余额 360 秒、新鲜窗口 5 秒、对账周期 3 分钟）。
    ///
    /// 未配置缓存时用不到它——那时这一层是空操作，参数只在"缓存启用后怎么写"上起作用。
    #[must_use]
    pub fn default_policy() -> Self {
        Self {
            route_ttl: Duration::from_secs(60),
            balance_ttl: Duration::from_secs(360),
            freshness_window: Duration::from_secs(5),
            reconcile_interval: Duration::from_secs(180),
        }
    }

    /// 从环境变量读参数；没给的项取默认值。
    ///
    /// **默认值不等于启用**：这一层启不启用只看有没有缓存服务（`REDIS_URL`），不看这些参数。
    pub fn from_env() -> Result<Self, ApplicationError> {
        Self::new(
            cache_duration_env("CACHE_ROUTE_TTL_SECONDS", 60)?,
            cache_duration_env("CACHE_BALANCE_TTL_SECONDS", 360)?,
            cache_duration_env("CACHE_FRESHNESS_WINDOW_MS", 5_000)?,
            cache_duration_env("CACHE_RECONCILE_INTERVAL_MS", 180_000)?,
        )
    }
}

/// 读一个时长参数。以 `_SECONDS` 结尾的按秒、以 `_MS` 结尾的按毫秒，统一成 [`Duration`]。
fn cache_duration_env(name: &str, default: u64) -> Result<Duration, ApplicationError> {
    let value = match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => value
            .trim()
            .parse::<u64>()
            .map_err(|_| ApplicationError::Configuration(format!("{name} must be an integer")))?,
        _ => default,
    };
    Ok(if name.ends_with("_SECONDS") {
        Duration::from_secs(value)
    } else {
        Duration::from_millis(value)
    })
}

/// 一次余额变更的结果：**哪个账户**、变更**之后**的余额、以及数据库记下的时刻。
///
/// 三个数都由**数据库**给出（`UPDATE … RETURNING balance_microusd, updated_at`，账户就是被改的
/// 那一行）。缓存里的写入时间要参与"新鲜不新鲜"的判定与审计，换成 API 进程的时钟就会因为两个
/// 时钟的漂移把刚写的值判成旧的（或反过来，把旧值当成刚写的）；账户也一律取库里那一行，
/// 不取调用方手上的 id——写穿缓存必须写回**真正被改动**的那个账户。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalanceChange {
    pub account_id: AccountId,
    pub balance_microusd: i64,
    pub updated_at: DateTime<Utc>,
}

/// 受理前的一次轻量读：网关模型开关、当前生效的修订标识、**数据库时钟**，以及这次请求是不是
/// 一次**重放**（该账户下已有同一个幂等键的 Job）。
///
/// 修订标识用来判断 route 缓存是不是陈旧的：缓存里带着写它那次发布的标识，与这里读到的比对，
/// 不一致就当未命中。开关必须**单独**读一次——`PATCH enabled` 改的是可变表、**不改变修订
/// 标识**，交给缓存判定的话，关掉的模型会在缓存有效期内继续被受理。
///
/// 时钟也在这里取：缓存里的写入时间由数据库盖章，拿它跟进程时钟比就会因为漂移判错新鲜度。
///
/// 重放这一项服务余额预检：重放会去重成原来那个 Job（不新建、不扣款），因此**不受预检管辖**
/// ——预检要避免的正是"新建一个 Job 却扣不动钱"，而重放本来就不新建。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcceptanceProbe {
    pub enabled: bool,
    pub effective_revision_id: Option<RuntimeRevisionId>,
    pub database_now: DateTime<Utc>,
    pub replay: bool,
}

/// 余额缓存条目的来源：写穿路径写下的值**可以**用于提前拒绝，对账写回的不行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BalanceSource {
    /// 数据库事务提交后由写穿路径写下（充值、受理预授权扣减、结算、失败释放、对账退款）。
    DbCommit,
    /// 定时对账写回的副本：它只保证"与数据库一致"，不是"刚有一笔钱变动过"的证据。
    Reconciler,
}

/// route 缓存的值：候选集 + **写它那次发布**的修订标识。
///
/// 修订标识是这一层的可检性来源：受理时与当前生效的修订比对，不一致就当未命中——因此
/// "发布后的失效没成功"只会让缓存里留着旧值，不会让旧候选被用上。
#[derive(Debug, Serialize, Deserialize)]
struct CachedRoute {
    runtime_revision_id: RuntimeRevisionId,
    candidates: Vec<OfferingCandidate>,
}

/// 余额缓存的值：余额 + 写入时间（数据库盖章）+ 来源标记。
#[derive(Debug, Serialize, Deserialize)]
struct CachedBalance {
    balance_microusd: i64,
    written_at: DateTime<Utc>,
    source: BalanceSource,
}

impl CachedBalance {
    /// 这条值能不能用来下结论（提前拒绝）。两条判据都要满足：
    ///
    /// 1. 来源是**写穿路径**——对账写回的只是"与数据库一致"的副本，不构成"刚有一笔钱变动过"；
    /// 2. 写入时间落在新鲜窗口内，且**不晚于数据库当前时刻**。晚于它只可能是两个时钟不同步，
    ///    那种值一律当不新鲜：宁可多打一次数据库，也不要凭一个来路不明的时间拒绝客户。
    fn is_fresh(&self, database_now: DateTime<Utc>, window: Duration) -> bool {
        if self.source != BalanceSource::DbCommit {
            return false;
        }
        if self.written_at > database_now {
            return false;
        }
        let window = ChronoDuration::from_std(window).unwrap_or_else(|_| ChronoDuration::zero());
        database_now - self.written_at < window
    }
}

/// 一轮对账的结果（供定时任务记日志；对客不可见）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub accounts_checked: u64,
    pub balances_corrected: u64,
    pub routes_invalidated: u64,
}

/// 加速层：把"哪些东西可以缓、值长什么样、什么时候能拿它下结论"收在一处。
///
/// 三条不变量，改这个类型时必须一起守住：
///
/// 1. **任何一次缓存操作失败都只是"这次没命中"**——回源数据库，绝不让请求因为缓存出问题而失败；
/// 2. **扣减与余额事实只在数据库事务里发生**：这里的写入一律发生在提交**之后**、写的是提交后的
///    值，从不用 `DECRBY` 之类的增量命令（增量表达不了"以数据库为准"，重放还会漂移）；
/// 3. **只有新鲜的值能用来拒绝**：来源必须是写穿路径且写入时间落在窗口内，其余一律交给数据库判。
#[derive(Clone)]
pub struct AccelerationService {
    repository: Arc<dyn HubRepository>,
    cache: Option<Arc<dyn CacheStore>>,
    policy: CachePolicy,
}

impl AccelerationService {
    /// 没有缓存服务时的加速层：所有方法都是空操作，受理路径**不额外查库**。
    ///
    /// 于是"未配置缓存"的行为与没有这一层时逐位相同——降级不是"多打几次数据库"，而是根本
    /// 不走这条路。
    #[must_use]
    pub fn disabled(repository: Arc<dyn HubRepository>) -> Self {
        Self {
            repository,
            cache: None,
            policy: CachePolicy::default_policy(),
        }
    }

    #[must_use]
    pub fn new(
        repository: Arc<dyn HubRepository>,
        cache: Arc<dyn CacheStore>,
        policy: CachePolicy,
    ) -> Self {
        Self {
            repository,
            cache: Some(cache),
            policy,
        }
    }

    /// 有没有缓存服务。受理路径用它决定走不走加速：没有缓存时连那次轻量读都不做。
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.cache.is_some()
    }

    #[must_use]
    pub fn policy(&self) -> CachePolicy {
        self.policy
    }

    fn route_key(gateway_model: &str) -> String {
        format!("route:{gateway_model}")
    }

    fn balance_key(account_id: AccountId) -> String {
        format!("user_balance:{account_id}")
    }

    /// 取该网关模型的候选集：缓存命中且**修订标识一致**才用缓存，否则回源数据库并重建缓存。
    ///
    /// 修订标识由受理前那次 [`AcceptanceProbe`] 读来，这里不再查库。不一致（或值里根本没有这个
    /// 标识、值读不出来）⇒ 当未命中。因此 route 缓存的陈旧是**可检的**，不依赖"发布后的失效
    /// 一定成功"。
    ///
    /// 关掉（或从未发布）的模型**不看缓存**：开关是可变表里的事、不改变修订标识，缓存里那份
    /// 候选集在模型被关掉之后仍然"看起来是新的"。
    pub async fn candidates(
        &self,
        gateway_model: &str,
        probe: &AcceptanceProbe,
    ) -> Result<Vec<OfferingCandidate>, ApplicationError> {
        let Some(effective) = probe.effective_revision_id.filter(|_| probe.enabled) else {
            return Ok(Vec::new());
        };
        if let Some(cached) = self.read_route(gateway_model).await
            && cached.runtime_revision_id == effective
        {
            return Ok(cached.candidates);
        }
        let candidates = self.repository.active_offering(gateway_model).await?;
        // **空候选集不入缓存**：它是"这个型号现在调不动"的瞬时状态，而回源它只发生在错误路径上。
        // 缓存下来反而会让"供给被重新启用"（今天没有写入方，但将来会有）在一个 TTL 内看不见。
        if !candidates.is_empty() {
            self.write_route(gateway_model, effective, &candidates)
                .await;
        }
        Ok(candidates)
    }

    /// 受理前的余额预检：**只有新鲜的值才允许提前拒绝**，返回 `true` 表示"凭缓存拒绝"。
    ///
    /// 三条判据缺一不可：条目来源是写穿路径、写入时间落在新鲜窗口内、**这次不是重放**。
    /// 重放会去重成原来那个 Job，不新建也不扣款——预检要避免的是"新建一个 Job 却扣不动钱"，
    /// 拿它拦一次重放只会让"重发同一个键"变成看余额脸色的行为。
    ///
    /// 拒绝本身没有副作用——不建 Job、不扣款、不写状态，所以事后必须解释得清"为什么拒了这个
    /// 客户"，这就是那条审计。审计写不下去时**不拒绝**：宁可多打一次数据库，也不能留下一次
    /// 没有记录的拒绝。
    pub async fn precheck_balance(
        &self,
        account_id: AccountId,
        hold_microusd: u64,
        gateway_model: &str,
        probe: &AcceptanceProbe,
    ) -> Result<bool, ApplicationError> {
        if probe.replay {
            return Ok(false);
        }
        let Some(cached) = self.read_balance(account_id).await else {
            return Ok(false);
        };
        if !cached.is_fresh(probe.database_now, self.policy.freshness_window) {
            return Ok(false);
        }
        let Ok(hold) = i64::try_from(hold_microusd) else {
            return Ok(false);
        };
        if cached.balance_microusd >= hold {
            return Ok(false);
        }
        let payload = json!({
            "reason": "the cached balance is below the hold while the entry is fresh",
            "gateway_model": gateway_model,
            "cached_balance_microusd": cached.balance_microusd,
            "cached_written_at": cached.written_at,
            "cached_source": cached.source,
            "hold_microusd": hold_microusd,
            "database_now": probe.database_now,
        });
        if let Err(error) = self
            .repository
            .insert_audit_event(
                "acceleration-precheck",
                "balance.precheck_rejected",
                "account",
                &account_id.to_string(),
                payload,
            )
            .await
        {
            tracing::warn!(
                account_id = %account_id,
                error = %error,
                "could not record the cache-based balance rejection; falling back to the database"
            );
            return Ok(false);
        }
        tracing::warn!(
            account_id = %account_id,
            cached_balance_microusd = cached.balance_microusd,
            hold_microusd,
            "rejected a request on a fresh cached balance below the hold"
        );
        Ok(true)
    }

    /// 把**数据库提交后**的余额写进缓存（写穿）。
    ///
    /// 写的是提交后的值而不是增量：`DECRBY` 表达不了"以数据库为准"，重放还会漂移。写入时间用
    /// 数据库给出的 `updated_at`，于是"新鲜"判定与审计里的时间都是数据库的时间。
    pub async fn write_balance(&self, change: &BalanceChange, source: BalanceSource) {
        if !self.is_enabled() {
            return;
        }
        let value = CachedBalance {
            balance_microusd: change.balance_microusd,
            written_at: change.updated_at,
            source,
        };
        let Ok(serialized) = serde_json::to_string(&value) else {
            // 这个结构体不可能序列化失败；真失败也只说明这次没写进缓存，不影响正确性。
            return;
        };
        self.write(
            &Self::balance_key(change.account_id),
            &serialized,
            self.policy.balance_ttl,
        )
        .await;
    }

    /// 发布成功（事务提交后）与启停开关改动后失效 route 缓存。
    ///
    /// 失效失败不影响正确性：旧值带着旧修订标识，受理时的比对必然不一致 ⇒ 回源数据库；
    /// 开关那一项由受理时按主键读的那一行兜住。失败只记一条日志（运营要能发现）。
    pub async fn invalidate_route(&self, gateway_model: &str) {
        let Some(cache) = self.cache.as_ref() else {
            return;
        };
        let key = Self::route_key(gateway_model);
        if let Err(error) = cache.delete(&key).await {
            tracing::warn!(
                key,
                error = %error,
                "cache invalidation failed; a stale entry would be detected at acceptance time"
            );
        }
    }

    /// 定时对账兜底：以数据库为准把缓存覆盖回去，并把**真的不一致**记下来。
    ///
    /// 只覆盖不一致的条目：已经等于数据库值的条目不动——重写会把它的来源降级成 `reconciler`
    /// （等于"这条值不能再用于提前拒绝"），没必要为一次没发生的不一致付这个代价。
    pub async fn reconcile_once(&self) -> Result<ReconcileReport, ApplicationError> {
        if !self.is_enabled() {
            return Ok(ReconcileReport::default());
        }
        let mut report = ReconcileReport::default();
        // 增量窗口取对账周期的三倍：够覆盖"上一轮之后变过、这一轮才轮到"的账户。
        let window = self.policy.reconcile_interval.saturating_mul(3);
        for change in self.repository.accounts_updated_within(window).await? {
            report.accounts_checked += 1;
            let cached = self.read_balance(change.account_id).await;
            if cached
                .as_ref()
                .is_some_and(|cached| cached.balance_microusd == change.balance_microusd)
            {
                continue;
            }
            if let Some(cached) = &cached {
                report.balances_corrected += 1;
                tracing::warn!(
                    account_id = %change.account_id,
                    cached_balance_microusd = cached.balance_microusd,
                    database_balance_microusd = change.balance_microusd,
                    "the cached balance disagreed with the database; overwriting it"
                );
                self.record_reconcile_correction(
                    "cache.balance_corrected",
                    "account",
                    &change.account_id.to_string(),
                    json!({
                        "cached_balance_microusd": cached.balance_microusd,
                        "cached_written_at": cached.written_at,
                        "cached_source": cached.source,
                        "database_balance_microusd": change.balance_microusd,
                        "database_updated_at": change.updated_at,
                    }),
                )
                .await;
            }
            self.write_balance(&change, BalanceSource::Reconciler).await;
        }
        for view in self.repository.gateway_models().await? {
            let Some(cached) = self.read_route(&view.gateway_model).await else {
                continue;
            };
            // 当前生效修订取自同一次只读投影：它与受理前那次轻量读取的是同一批复发行。
            if view.enabled && view.runtime_revision_id == cached.runtime_revision_id {
                continue;
            }
            report.routes_invalidated += 1;
            tracing::warn!(
                gateway_model = %view.gateway_model,
                "the cached candidate set is not the effective revision; invalidating it"
            );
            self.record_reconcile_correction(
                "cache.route_invalidated",
                "gateway_model",
                &view.gateway_model,
                json!({
                    "cached_runtime_revision_id": cached.runtime_revision_id,
                    "effective_runtime_revision_id": view.runtime_revision_id,
                    "enabled": view.enabled,
                }),
            )
            .await;
            self.invalidate_route(&view.gateway_model).await;
        }
        Ok(report)
    }

    /// 定时对账循环：由进程在启动时挂起来，与请求路径无关。
    ///
    /// 第一轮立刻跑（`interval` 的第一次 tick 立即完成），之后每 `interval` 一轮。缓存没启用时
    /// 直接返回，连循环都不进。
    pub async fn run_reconciler(self: Arc<Self>) {
        if !self.is_enabled() {
            return;
        }
        let interval = self.policy.reconcile_interval;
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            match self.reconcile_once().await {
                Ok(report) if report.balances_corrected > 0 || report.routes_invalidated > 0 => {
                    tracing::warn!(
                        accounts_checked = report.accounts_checked,
                        balances_corrected = report.balances_corrected,
                        routes_invalidated = report.routes_invalidated,
                        "cache reconciliation corrected entries against the database"
                    );
                }
                Ok(_) => {}
                Err(error) => tracing::error!(error = %error, "cache reconciliation failed"),
            }
        }
    }

    /// 记一条对账发现的审计。写不进去只记日志——**对账本身已经生效**，留痕失败不该让它回退。
    async fn record_reconcile_correction(
        &self,
        action: &str,
        subject_type: &str,
        subject_id: &str,
        payload: Value,
    ) {
        if let Err(error) = self
            .repository
            .insert_audit_event(
                "acceleration-reconciler",
                action,
                subject_type,
                subject_id,
                payload,
            )
            .await
        {
            tracing::error!(action, subject_id, error = %error, "could not record a cache reconciliation audit event");
        }
    }

    async fn write_route(
        &self,
        gateway_model: &str,
        runtime_revision_id: RuntimeRevisionId,
        candidates: &[OfferingCandidate],
    ) {
        let value = CachedRoute {
            runtime_revision_id,
            candidates: candidates.to_vec(),
        };
        let Ok(serialized) = serde_json::to_string(&value) else {
            return;
        };
        self.write(
            &Self::route_key(gateway_model),
            &serialized,
            self.policy.route_ttl,
        )
        .await;
    }

    async fn read_route(&self, gateway_model: &str) -> Option<CachedRoute> {
        let raw = self.read(&Self::route_key(gateway_model)).await?;
        match serde_json::from_str(&raw) {
            Ok(cached) => Some(cached),
            Err(error) => {
                // 值读不出来（格式变了、被改坏了）：当未命中，回源重建。
                tracing::warn!(
                    gateway_model,
                    error = %error,
                    "the cached candidate set is unreadable; falling back to the database"
                );
                None
            }
        }
    }

    async fn read_balance(&self, account_id: AccountId) -> Option<CachedBalance> {
        let raw = self.read(&Self::balance_key(account_id)).await?;
        match serde_json::from_str(&raw) {
            Ok(cached) => Some(cached),
            Err(error) => {
                tracing::warn!(
                    account_id = %account_id,
                    error = %error,
                    "the cached balance is unreadable; falling back to the database"
                );
                None
            }
        }
    }

    /// 读一条缓存值。**任何失败都是"没读到"**：连不上、超时、命令报错、值不存在，一视同仁。
    async fn read(&self, key: &str) -> Option<String> {
        let cache = self.cache.as_ref()?;
        match cache.get(key).await {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(key, error = %error, "cache read failed; falling back to the database");
                None
            }
        }
    }

    /// 写一条缓存值。失败只记一条日志：缓存里留着旧值时，受理时的比对必然不一致 ⇒ 回源数据库，
    /// 所以**正确性不依赖这次写入成功**。
    async fn write(&self, key: &str, value: &str, ttl: Duration) {
        let Some(cache) = self.cache.as_ref() else {
            return;
        };
        if let Err(error) = cache.set(key, value, ttl).await {
            tracing::warn!(
                key,
                error = %error,
                "cache write failed; the cached value stays stale until it expires"
            );
        }
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
    acceleration: Arc<AccelerationService>,
}

impl RuntimeService {
    #[must_use]
    pub fn new(repository: Arc<dyn HubRepository>, adapters: Arc<dyn AdapterFactory>) -> Self {
        let acceleration = Arc::new(AccelerationService::disabled(repository.clone()));
        Self {
            repository,
            adapters,
            acceleration,
        }
    }

    /// 装上加速层：发布与启停都要失效该型号的 route 缓存。
    #[must_use]
    pub fn with_acceleration(mut self, acceleration: Arc<AccelerationService>) -> Self {
        self.acceleration = acceleration;
        self
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
        validate_supply_identities(&normalized)?;
        let request = command.into_request(contract, normalized);
        validate_gateway_model_identity(&request)?;
        let gateway_model = request.gateway_model.clone();
        let revision = self.repository.publish_runtime(request).await?;
        // 发布已经提交：这时才失效缓存。失效失败不影响正确性——旧值带着旧修订标识，
        // 受理时的比对必然不一致（见 `AccelerationService::candidates`）。
        self.acceleration.invalidate_route(&gateway_model).await;
        Ok(revision)
    }

    /// 对客目录：当前真的能调的模型与它们的合同。
    ///
    /// 合同取的就是**发布的那一份**，不另造简化结构：目录说的与受理时校验的必须是同一份，
    /// 否则客户端照目录建的表单会被另一套规则拒掉。
    pub async fn published_models(&self) -> Result<Vec<PublishedModel>, ApplicationError> {
        self.repository.published_models().await
    }

    /// 管理员读：当前有生效定义的网关模型，一条一项，带候选清单与运维开关。
    pub async fn gateway_models(&self) -> Result<Vec<GatewayModelView>, ApplicationError> {
        self.repository.gateway_models().await
    }

    /// 管理员写：只改运维开关。没发布过的名字由仓库判成"不存在"。
    ///
    /// 改完失效该型号的 route 缓存：开关**不改变修订标识**，缓存里那份候选集在关掉之后仍然
    /// "看起来是新的"，只能靠失效把它拿掉。失效失败也不影响正确性——受理时那一次按主键读的
    /// `enabled` 会兜住（见 `AccelerationService::candidates`）。
    pub async fn set_gateway_model_enabled(
        &self,
        gateway_model: &str,
        enabled: bool,
        actor: &str,
    ) -> Result<(), ApplicationError> {
        self.repository
            .set_gateway_model_enabled(gateway_model, enabled, actor)
            .await?;
        self.acceleration.invalidate_route(gateway_model).await;
        Ok(())
    }

    /// 管理员写：只改一条**供给**的启用开关，并失效受影响型号的 route 缓存。
    ///
    /// 停用即刻影响之后的受理：候选取数本来就同时读供给与渠道的开关，写入即生效，**不需要重发
    /// 修订**——启停是运行状态，不是定义。已受理的 Job 不受影响（候选已冻结在它们的快照里）。
    /// 缓存里那份候选集带着的修订标识没变，因此只能靠失效拿掉；失效失败也只是让这次停用晚一个
    /// TTL 生效，不会让停用丢失（下一轮对账或 TTL 到期后回源）。
    pub async fn set_offering_enabled(
        &self,
        offering_id: OfferingId,
        enabled: bool,
        actor: &str,
    ) -> Result<(), ApplicationError> {
        let affected = self
            .repository
            .set_offering_enabled(offering_id, enabled, actor)
            .await?;
        for gateway_model in affected {
            self.acceleration.invalidate_route(&gateway_model).await;
        }
        Ok(())
    }

    /// 管理员写：只改一条**渠道**的启用开关，并失效受影响型号的 route 缓存。
    pub async fn set_channel_enabled(
        &self,
        channel_id: ChannelId,
        enabled: bool,
        actor: &str,
    ) -> Result<(), ApplicationError> {
        let affected = self
            .repository
            .set_channel_enabled(channel_id, enabled, actor)
            .await?;
        for gateway_model in affected {
            self.acceleration.invalidate_route(&gateway_model).await;
        }
        Ok(())
    }

    /// 校验单个候选，并归一化它的 `base_url`。
    fn validate_offering(
        &self,
        contract: &Value,
        mut offering: NormalizedOffering,
    ) -> Result<NormalizedOffering, ApplicationError> {
        jsonschema::validator_for(&offering.carrier_schema)
            .map_err(|error| ApplicationError::Validation(error.to_string()))?;
        // 供给不能凭空多出调用方可提交的字段：承载面必须落在合同里（改名的桥与尺寸换算的目标
        // 也算"从合同来的"，见该函数）。
        validate_carrier_within_contract(
            contract,
            &offering.carrier_schema,
            &offering.parameter_mapping,
        )?;
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
        // 成本币种**按渠道/供给自己声明的那个值接受**，不假定 USD：四档费率表本来就是按渠道
        // 各自记、按该渠道币种标注的，硬写"必须是 USD"等于替渠道改币种。它是这条供给在成本
        // 平面上的记账币种，所以每条供给都要有一个（没带定价的也要：上游声明的金额、按张 /
        // 按次的单价都要说清是哪个币种的钱）。
        let cost_currency = offering.cost_currency().unwrap_or_default();
        if cost_currency.trim().is_empty() {
            return Err(ApplicationError::Validation(
                "cost currency must not be empty".to_owned(),
            ));
        }
        // 价目的出处只在带 Price Plan 时存在：没有 Price Plan 就没有渠道价目可引。
        if let Some(price_source_url) = &offering.price_source_url {
            let price_source = url::Url::parse(price_source_url)
                .map_err(|error| ApplicationError::Validation(error.to_string()))?;
            if price_source.scheme() != "https" {
                return Err(ApplicationError::Validation(
                    "price_source_url must use https".to_owned(),
                ));
            }
        }
        let descriptor = self
            .adapters
            .descriptor(&offering.adapter_key)
            .ok_or_else(|| {
                ApplicationError::Validation(format!("unknown adapter {}", offering.adapter_key))
            })?;
        validate_adapter_compatibility(&offering, &descriptor)?;
        // 尺寸换算声明也是**发布数据**：源字段必须在合同里（否则客户端提交不了它）、目标字段必须
        // 被这条供给的承载面声明（否则换算出来的值发不出去），档案必须成形状。写歪了在这里拒绝，
        // 不让它到受理期才变成一条"这条候选换算不出"的平台侧故障。
        validate_size_mapping(contract, &offering)?;
        // 改名表、取值映射表与显式默认值同样是发布数据，判据同一条：声明了却做不到就不该发出去。
        validate_parameter_mapping(contract, &offering)?;
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
    // 发布期据此拒绝「把 A 型号的合同挂到 B 型号上」。这一条同时挡住"素材把**平台对客名**
    // 写进合同正文"：对客名不是厂商模型的身份，写进合同就等于把两个角色又合成一个值。
    // 读的位置与对客投射共用同一个助手——两边指向的必须是合同里同一个字段。
    if contract_model_identity(contract) != Some(native_model_id) {
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

/// 一次发布里不能出现两条**同一条供给**的候选。
///
/// 供给的身份是"它所属的 vendor model + channel"，而渠道的身份是 `provider_kind` + `base_url` +
/// `credential_env`；一次发布的所有候选都属于同一个 vendor model，所以"两条候选落在同一条供给上"
/// 等价于"两条候选共用同一个渠道身份"。发布按身份复用供给行，两条候选于是会写到同一行上：候选集
/// 里会出现两条指向同一条供给的条目（`runtime_entries` 的主键正是"修订 + 供给"），而"这条候选的
/// 档位与权重"也就无处安放。要两条候选就换一个入口（地址或凭证身份不同）。
///
/// 判据用**归一之后**的 `base_url`：末尾斜杠的写法差异不构成两个入口。
fn validate_supply_identities(offerings: &[NormalizedOffering]) -> Result<(), ApplicationError> {
    let mut seen = BTreeSet::new();
    for offering in offerings {
        if !seen.insert((
            offering.provider_kind.as_str(),
            offering.base_url.as_str(),
            offering.credential_env.as_str(),
        )) {
            return Err(ApplicationError::Validation(format!(
                "two candidates share one channel identity ({} {} {}); one supply can appear \
                 only once in a publication",
                offering.provider_kind, offering.base_url, offering.credential_env
            )));
        }
    }
    Ok(())
}

/// 校验这次发布声明的**平台对客名**：一次发布定义的就是这一个网关模型，名字不得为空白。
///
/// 守的是"同一次发布里的 `gateway_model` 不得出现第二个值"这条验收要求：名字是这次发布
/// **原子替换**的对象，空白名字等于"替换一个不存在的名字"——对客目录会因此列出一个调不动的
/// 名字。候选集是否都落在同一个名字上另由仓库在写入时守（见发布事务里的守卫）。
///
/// 为什么现在走不到这里：名字今天只有一个来源（命令顶层那一个字段），发布入口又已经校验过
/// `native_model_id` 非空、`into_request` 把空白回退成它，因此到这里时名字必然非空。留着它是
/// 为了让"发布即原子替换**这个名字**的候选集"在将来形状变化时（例如允许候选各自报名）先被
/// 拦住，而不是先悄悄生效、事后才发现替换的到底是谁说不清。
fn validate_gateway_model_identity(
    request: &PublishRuntimeRequest,
) -> Result<(), ApplicationError> {
    if request.gateway_model.trim().is_empty() {
        return Err(ApplicationError::Validation(
            "gateway_model must not be empty".to_owned(),
        ));
    }
    Ok(())
}

/// 校验「承载面 ⊆ 合同」：供给不能凭空多出调用方可提交的字段。
///
/// 判据是**顶层字段名**，但要认得出"从合同来的名字"：承载面声明的是**线上字段名**，同一个字段
/// 在这条供给的线上完全可以叫另一个名字，而那个名字同样是从合同来的、不是供给凭空多出来的。
/// 因此一个承载面字段算数，当且仅当它满足下面任意一条：
///
/// - 合同直接声明了它；
/// - 改名表把某个**合同字段**改到它身上（线上换个名字）；
/// - 它是尺寸换算的**目标字段**（换算的源字段在合同里，算出来的值写在这个名字上）。
///
/// 只比名字、不比定义：同一个名字在两边各自描述（例如 `size` 的取值形态）由映射与换算承担。
fn validate_carrier_within_contract(
    contract: &Value,
    carrier: &Value,
    mapping: &Value,
) -> Result<(), ApplicationError> {
    carrier_properties(carrier)?;
    let renames = declared_renames(mapping).map_err(|reason| {
        ApplicationError::Validation(format!("parameter_mapping.rename: {reason}"))
    })?;
    let size_target = declared_size_mapping(mapping)
        .map_err(|reason| {
            ApplicationError::Validation(format!("parameter_mapping.size: {reason}"))
        })?
        .map(|mapping| mapping.target);
    for field in declared_field_names(carrier) {
        if declares_parameter(contract, field) {
            continue;
        }
        let renamed_from_contract = renames.as_ref().is_some_and(|renames| {
            renames.iter().any(|(source, wire)| {
                wire.as_str() == field && declares_parameter(contract, source)
            })
        });
        let converted_from_contract = size_target.as_deref() == Some(field);
        if !renamed_from_contract && !converted_from_contract {
            return Err(ApplicationError::Validation(format!(
                "carrier schema declares {field}, which the vendor model contract does not"
            )));
        }
    }
    Ok(())
}

/// 校验映射里的**改名表**、**取值映射表**与**显式默认值**：它们都必须是这条供给真能做到的事。
///
/// 三条边界，与"承载面 ⊆ 合同 ⊆ Driver 能写上线文的名字"同一条道理——声明了却做不到，就是
/// 声明与行为分了家：
///
/// - 改名的**源**必须在合同里：调用方提交不了的名字没有值可改；
/// - 改名的**目标**必须被这条供给的承载面声明：改出来的名字发不出去，等于没改；
/// - 取值映射的字段必须"合同里有、这条供给承载得了"，否则这张表永远不会被用到；
/// - 显式默认值的每个键必须被这条供给承载（承载面声明，或经改名落到一个声明的名字上）：
///   声明了一个发不出去的默认值，就是"声明了却发不出去"。
///
/// 声明写歪时**不**按"没有声明"处理：那会让调用方与运营都以为映射发生了，而线上原样上行。
fn validate_parameter_mapping(
    contract: &Value,
    offering: &NormalizedOffering,
) -> Result<(), ApplicationError> {
    let mapping = &offering.parameter_mapping;
    let renames = declared_renames(mapping).map_err(|reason| {
        ApplicationError::Validation(format!("parameter_mapping.rename: {reason}"))
    })?;
    let enum_maps = declared_enum_maps(mapping).map_err(|reason| {
        ApplicationError::Validation(format!("parameter_mapping.enum_map: {reason}"))
    })?;
    if let Some(renames) = &renames {
        for (source, wire) in renames {
            if !declares_parameter(contract, source) {
                return Err(ApplicationError::Validation(format!(
                    "parameter_mapping.rename reads {source}, which the vendor model contract does not declare"
                )));
            }
            if !declares_parameter(&offering.carrier_schema, wire) {
                return Err(ApplicationError::Validation(format!(
                    "parameter_mapping.rename writes {wire}, which this offering's carrier surface does not declare"
                )));
            }
        }
    }
    if let Some(enum_maps) = &enum_maps {
        for name in enum_maps.keys() {
            validate_mapped_field(contract, offering, renames.as_ref(), name, "enum_map")?;
        }
    }
    if let Some(defaults) = declared_defaults(mapping) {
        for name in defaults.keys() {
            validate_mapped_field(contract, offering, renames.as_ref(), name, "defaults")?;
        }
    }
    Ok(())
}

/// 映射里声明的一个**合同字段名**必须"合同里有、这条供给承载得了"：两样缺一，这份声明就是死的。
///
/// 合同没声明它，调用方根本提交不了这个字段，映射没有输入；这条供给承载不了它，映射出来的东西
/// 发不出去。`kind` 只用于把出错的是哪一块说清楚。
fn validate_mapped_field(
    contract: &Value,
    offering: &NormalizedOffering,
    renames: Option<&ParameterRenames>,
    name: &str,
    kind: &str,
) -> Result<(), ApplicationError> {
    if !declares_parameter(contract, name) {
        return Err(ApplicationError::Validation(format!(
            "parameter_mapping.{kind} declares {name}, which the vendor model contract does not"
        )));
    }
    if !carries_parameter(&offering.carrier_schema, renames, name) {
        return Err(ApplicationError::Validation(format!(
            "parameter_mapping.{kind} declares {name}, which this offering's carrier surface cannot carry"
        )));
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

/// 校验映射里的**尺寸换算声明**：它必须是这条供给真能执行的一件事。
///
/// 三条边界，与"承载面 ⊆ 合同 ⊆ Driver 能写上线文的名字"同一条道理——声明了却做不到，就是
/// 声明与行为分了家：
///
/// - 源字段必须在**合同**里：客户端提交不了的名字当不了换算的输入；
/// - 目标字段必须被这条供给的**承载面**声明：换算出来的值要发得出去；
/// - 档案必须成形状（档位/比例/像素三样各就各位）：它随发布携带，写歪了就不该发出去。
///
/// 声明写歪时**不**按"没有声明"处理：那会让调用方与运营都以为换算发生了，而线上原样上行。
fn validate_size_mapping(
    contract: &Value,
    offering: &NormalizedOffering,
) -> Result<(), ApplicationError> {
    let Some(mapping) = declared_size_mapping(&offering.parameter_mapping).map_err(|reason| {
        ApplicationError::Validation(format!("parameter_mapping.size: {reason}"))
    })?
    else {
        return Ok(());
    };
    for name in &mapping.source {
        if !declares_parameter(contract, name) {
            return Err(ApplicationError::Validation(format!(
                "parameter_mapping.size reads {name}, which the vendor model contract does not declare"
            )));
        }
    }
    if !declares_parameter(&offering.carrier_schema, &mapping.target) {
        return Err(ApplicationError::Validation(format!(
            "parameter_mapping.size writes {}, which this offering's carrier surface does not declare",
            mapping.target
        )));
    }
    Ok(())
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
    /// 加速层：候选集从缓存取、受理后把余额写穿、以及**只在新鲜时**的提前拒绝。
    acceleration: Arc<AccelerationService>,
}

impl GenerationService {
    #[must_use]
    pub fn new(
        repository: Arc<dyn HubRepository>,
        max_cost_microusd: u64,
        max_concurrent_jobs: u64,
    ) -> Self {
        let acceleration = Arc::new(AccelerationService::disabled(repository.clone()));
        Self {
            repository,
            max_cost_microusd,
            max_concurrent_jobs,
            acceleration,
        }
    }

    /// 装上加速层。
    #[must_use]
    pub fn with_acceleration(mut self, acceleration: Arc<AccelerationService>) -> Self {
        self.acceleration = acceleration;
        self
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
        // 加速层开着时先做一次轻量读（生效修订标识 + 开关 + 数据库时钟），候选集再从缓存取；
        // 关着时这一步不做，取数路径与没有这一层时逐字相同。
        let probe = if self.acceleration.is_enabled() {
            Some(
                self.repository
                    .acceptance_probe(&request.model, request.account_id, &request.idempotency_key)
                    .await?,
            )
        } else {
            None
        };
        let candidates = match &probe {
            Some(probe) => self.acceleration.candidates(&request.model, probe).await?,
            None => self.repository.active_offering(&request.model).await?,
        };
        // 这次受理用哪条策略：按模型覆盖优先、其次全局那条。**一条策略都没有时走原来的选路
        // 函数**——零配置下的行为由构造保证与策略层引入之前逐位相同，而不是靠某个默认参数"应该
        // 等价"。策略是运行期配置，改它不影响已经受理的 Job：那些 Job 的候选早已固定在快照里。
        let policy = self.repository.route_policy(&request.model).await?;
        let (mut offering, native_parameters, routing) = match policy {
            Some(policy) => {
                // 标签只有 `user_tag` 消费：别的策略下不为它多查一次库。
                let account_tag = if policy.strategy == RouteStrategy::UserTag {
                    self.repository.account_tag(request.account_id).await?
                } else {
                    None
                };
                let choice = RouteChoice {
                    strategy: policy.strategy,
                    discount_rates: &policy.discount_rates,
                    tag_channel_map: &policy.tag_channel_map,
                    account_tag: account_tag.as_deref(),
                };
                select_candidate_with_strategy(&request, branch, &candidates, &choice)?
            }
            None => select_candidate(&request, branch, &candidates)?,
        };
        // 受理时把定价随快照冻结，并算定这次的预授权额（保底额）。策略在受理时已经定下候选，
        // 所以售价**不必等上游回来**；冻结之后结算只读那份快照，受理之后改汇率、改加价系数、
        // 重发修订都不影响这一个 Job。
        let hold_microusd = self.freeze_pricing(&request, &mut offering).await?;
        // 预检：缓存里的余额**新鲜**且明显不够时提前拒绝。它只读不写、不建 Job、不扣款，
        // 因此必然留下一条审计（`precheck_balance` 里落）；不新鲜一律交给下面的数据库条件更新。
        if let Some(probe) = &probe
            && self
                .acceleration
                .precheck_balance(request.account_id, hold_microusd, &request.model, probe)
                .await?
        {
            return Err(ApplicationError::InsufficientBalance);
        }
        // 幂等哈希取**调用方看到的那份请求**（不含按候选解析出的参数名，也不含按承载面过滤的结果）：
        // 上游目录变了、或另一个候选的承载面更窄，都不该让同一个幂等键算出不同的哈希。
        let request_hash = request_hash(&request)?;
        let (job, balance) = self
            .repository
            .create_job(
                CreateImageGeneration {
                    account_id: request.account_id,
                    gateway_model: request.model,
                    native_parameters,
                    idempotency_key: request.idempotency_key,
                    max_cost_microusd: hold_microusd,
                },
                branch,
                offering,
                request_hash,
                routing,
            )
            .await?;
        // 预授权扣减已经提交：把扣减后的余额写进缓存，否则缓存会滞后一个预授权额。
        self.acceleration
            .write_balance(&balance, BalanceSource::DbCommit)
            .await;
        Ok(job)
    }

    /// 受理时把定价随 Job 冻结，并算定这次的**预授权额**（保底额）。
    ///
    /// 两件事都依赖这次请求，发布侧算不出来：
    /// - **保底额**按请求的 `size` 先**归到档位**、再查该供给的保底表（回落链见
    ///   [`resolve_size_tier`] 与 `FloorTable::lookup`）；连该供给的封顶保底值都没有时回落到
    ///   平台兜底数。它**不由售价派生**——售价高不代表预授权高，两者是两件事；
    /// - **汇率**按该候选的成本币种取"受理时刻生效的那一行"，原值快照进快照（受理之后不再换算）。
    ///
    /// **归位用的档位像素表就是这条供给已发布的尺寸档案**（`parameter_mapping` 里的档位 →
    /// 比例 → 像素）：各供给的档位像素不同，只有它自己声明的那张表才是它的档位定义；这条供给
    /// 没发布尺寸档案时按最长边阈值兜底。那份映射随 Job 一起冻结，所以事后重建"这次按哪一档
    /// 冻的"用的是受理当时那一份，不是今天的发布物。
    ///
    /// 旧修订没有定价（快照里没有该供给的保底表）：这一步什么都不做，返回平台兜底数，
    /// 预授权与结算都走旧口径、与今天逐位相同。
    async fn freeze_pricing(
        &self,
        request: &CreateImageGenerationRequest,
        offering: &mut PublishedOffering,
    ) -> Result<u64, ApplicationError> {
        // 汇率只要这条候选**声明了成本币种**就冻结：成本（上游声明的金额、或按计价形态自算
        // 出来的金额）都要折成人民币才算得出毛利，而折算率只有受理时取得到。旧修订受理出的
        // 历史 Job 快照里没有这个声明（那时没有这条事实），这一步因此什么都不做——那是旧口径。
        if let Some(cost_currency) = offering.price_snapshot.cost_currency.clone() {
            // 汇率在发布期已被校验过（该币种必须有一行已生效的折算率），所以取不到只可能是
            // 汇率表被人删了行或只剩未来生效的行——那是平台自己的配置问题，不是这次请求的问题。
            let fx_rate = self
                .repository
                .effective_fx_rate(&cost_currency)
                .await?
                .ok_or_else(|| {
                    ApplicationError::Configuration(format!(
                        "no effective fx rate for {cost_currency}; publication rejects a currency \
                         without one, so the rate table lost a row it promised"
                    ))
                })?;
            offering.price_snapshot.fx_rate = Some(fx_rate);
        }
        // 判据是"这条候选带不带定价"，不是"有没有对客费率向量"：按张 / 按次 / 上游给金额的候选
        // 本来就没有那份四档向量，但它们照样有保底表要查。带定价就一定有保底表（发布期两者
        // 全有或全无），所以这里看保底表在不在。
        if offering.price_snapshot.floor_amounts.is_none() {
            return Ok(self.max_cost_microusd);
        }
        let table = offering
            .price_snapshot
            .floor_amounts
            .as_ref()
            .map(FloorTable::from_json)
            .transpose()
            .map_err(|message| {
                ApplicationError::Configuration(format!(
                    "the published floor table is malformed: {message}"
                ))
            })?
            .unwrap_or_default();
        // 尺寸档案在发布期已校验过形状，这里取不到只可能是"这条供给没发布尺寸档案"（合法）。
        let profile = declared_size_mapping(&offering.parameter_mapping)
            .map_err(|message| {
                ApplicationError::Configuration(format!(
                    "the published size mapping is malformed: {message}"
                ))
            })?
            .map(|mapping| mapping.profile)
            .unwrap_or_default();
        let tier = resolve_size_tier(
            literal_parameter_text(&request.native_parameters, "size"),
            &profile,
        );
        let (hold_microusd, hold_source) = table
            .lookup(
                tier.as_ref(),
                literal_parameter_text(&request.native_parameters, "quality"),
            )
            .unwrap_or((self.max_cost_microusd, HoldSource::PlatformDefault));
        offering.price_snapshot.hold_microusd = Some(hold_microusd);
        offering.price_snapshot.hold_source = Some(hold_source);
        Ok(hold_microusd)
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
    /// 加速层：结算与失败收尾都改余额，提交后要把新余额写穿缓存。
    acceleration: Arc<AccelerationService>,
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
        let acceleration = Arc::new(AccelerationService::disabled(repository.clone()));
        Ok(Self {
            repository,
            adapters,
            credentials,
            worker_id,
            lease_duration,
            provider_timeout,
            acceleration,
        })
    }

    /// 装上加速层：结算与失败收尾之后要把余额写穿缓存。
    #[must_use]
    pub fn with_acceleration(mut self, acceleration: Arc<AccelerationService>) -> Self {
        self.acceleration = acceleration;
        self
    }

    /// 失败收尾 + 写穿余额。
    ///
    /// 释放预授权的那些分支会改动余额，**不写穿的话缓存里会留着一个刚写过、但偏高的余额**——
    /// 那正好是"看起来新鲜、其实已经不对"的那类值，下一次受理就可能凭它误拒。
    async fn fail_and_refresh(
        &self,
        job_id: JobId,
        attempt_id: AttemptId,
        failure: AttemptFailure,
    ) -> Result<(), ApplicationError> {
        let change = self
            .repository
            .fail_job(job_id, &self.worker_id, Some(attempt_id), failure)
            .await?;
        self.acceleration
            .write_balance(&change, BalanceSource::DbCommit)
            .await;
        Ok(())
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
            // 成本币种是**受理时冻结的那份渠道声明**（价格快照里就有）：上游报出来的金额不带
            // 币种，Driver 拿不到"这个数是什么钱"，只能把这份声明原样带回来。取值只经这一个
            // 访问点——受理、执行、落账三处各拼一遍链，改一处就会漏一处。
            cost_currency: claimed
                .job
                .offering
                .price_snapshot
                .cost_currency()
                .ok_or_else(|| {
                    ApplicationError::Configuration(
                        "this job's snapshot carries no cost currency to hand the driver"
                            .to_owned(),
                    )
                })?
                .to_owned(),
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
                self.fail_and_refresh(
                    claimed.job.id,
                    attempt_id,
                    AttemptFailure {
                        provider_code: "credential_unavailable".to_owned(),
                        public_code: PublicErrorCode::PlatformUnavailable,
                        message: error.to_string(),
                        trace_id: None,
                        kind: ProviderFailureKind::PlatformInternal,
                        target_state: JobState::Failed,
                        hold_disposition: HoldDisposition::Release,
                        // 请求还没交出去：这次执行没有成本可采。
                        provider_cost: None,
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
                self.fail_and_refresh(
                    claimed.job.id,
                    attempt_id,
                    AttemptFailure {
                        provider_code: "adapter_configuration_failed".to_owned(),
                        public_code: PublicErrorCode::PlatformUnavailable,
                        message: error.to_string(),
                        trace_id: None,
                        kind: ProviderFailureKind::PlatformInternal,
                        target_state: JobState::Failed,
                        hold_disposition: HoldDisposition::Release,
                        // 请求还没交出去：这次执行没有成本可采。
                        provider_cost: None,
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
                // 成本事实**先算出来**再结算：结算失败进对账那条路径也要落成本——执行已经发生、
                // 上游成本也拿得到，把成本留在成功路径上等于"这一笔付过钱却没有成本事实"。
                let provider_cost = provider_cost_fact(
                    &claimed.job.offering.price_snapshot,
                    &success.provider_cost,
                    CostInputs::Succeeded {
                        usage: &success.usage,
                        images: success.images.len(),
                    },
                );
                if let Err(error) = self
                    .complete_success(&claimed.job, attempt_id, &success, provider_cost.clone())
                    .await
                {
                    self.fail_and_refresh(
                        claimed.job.id,
                        attempt_id,
                        AttemptFailure {
                            provider_code: "result_delivery_failed".to_owned(),
                            public_code: PublicErrorCode::OutcomeUnknown,
                            message: error.to_string(),
                            trace_id: None,
                            kind: ProviderFailureKind::PlatformInternal,
                            target_state: JobState::ReconciliationRequired,
                            hold_disposition: HoldDisposition::RetainForReconciliation,
                            provider_cost: Some(provider_cost),
                        },
                    )
                    .await?;
                }
            }
            Err(error) => {
                let failure = failure_from_adapter(&claimed.job.offering.price_snapshot, error);
                self.fail_and_refresh(claimed.job.id, attempt_id, failure)
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
                            // 心跳掉了是我们自己的问题，与这次执行的成本事实无关。
                            provider_cost: None,
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
        success: &ProviderSuccess,
        provider_cost: ProviderCostFact,
    ) -> Result<(), ApplicationError> {
        // 对客扣费（对客平面）：读受理时冻结的那份快照，只决定向消费者收多少。
        //
        // 快照按**这条供给的计价形态**算对客价：按 token 计量量的读那份随修订发布的对客费率向量；
        // 按张 / 按次 / 上游给金额的由成本单价 × 倍率 × 折算率算出来（见领域侧的 charge_microusd）。
        // 倍率与折算率都取自受理时冻结的那一份，所以受理之后改价、改汇率都不影响这一个 Job。
        //
        // **不封顶在预授权额**：预授权只是保底，实收按实际算，超出部分由余额透支吸收
        // （透支发生在结算，不在受理）。所以这里没有"超过预授权就进对账"这一条——那是旧口径，
        // 而旧口径会把一笔正常完成的生成扣在对账里。
        let charge = job
            .offering
            .price_snapshot
            .charge_microusd(ChargeFacts {
                usage: &success.usage,
                images: success.images.len(),
                // 上游这次声明的金额就是**成本单价**（原币种）——只有上游直接给金额的候选读它。
                // 上游没声明时是 `None`：那条路算不出对客价，按平台侧故障处置，不按 0 收。
                declared_cost_microusd: provider_cost.amount_microusd,
            })
            .map_err(|error| ApplicationError::Reconciliation(error.to_string()))?;
        // 结果只是"当次信封"：渠道给 url 就留 url、给 base64 就留 base64，平台不看内容。
        if success.images.is_empty() {
            return Err(ApplicationError::Reconciliation(
                "provider returned no image".to_owned(),
            ));
        }
        let change = self
            .repository
            .complete_job(CompleteJob {
                job_id: job.id,
                worker_id: self.worker_id.clone(),
                attempt_id,
                images: success.images.clone(),
                evidence: MeteringEvidence {
                    attempt_id,
                    provider_response_digest: success.response_digest.clone(),
                    usage: success.usage.clone(),
                },
                charge_microusd: charge,
                provider_trace_id: success.provider_trace_id.clone(),
                provider_cost,
            })
            .await?;
        // 结算已经提交：把实收之后的余额写进缓存（用户要求：扣减成功后立即同步）。
        self.acceleration
            .write_balance(&change, BalanceSource::DbCommit)
            .await;
        Ok(())
    }
}

/// 把 Driver 报出来的成本事实定成落库口径（成本平面：原币种原值 + 币种 + 折算后 CNY）。
///
/// 判据是**成本从哪来**，不是"金额对不对"：
/// - 上游直接给了金额 ⇒ `declared`，**直接取它**（含渠道侧折扣，比自算权威），币种也取它报的；
/// - 渠道不给金额字段 ⇒ `computed`，按这条供给的**计价形态**自算（见 [`self_computed_cost`]）；
/// - 本该有金额却拿不到 ⇒ `unavailable`，金额与币种**留空**：不写 0、不用自算顶替。
///
/// 币种的权威**分来源**：`declared` 认上游报回来的那一份，`computed` 认渠道声明的成本币种
/// （两处在实践中同源，但"以哪一份为准"必须只有一个答案）；所以没有"一个入参管三态"这回事。
///
/// **折算**用受理时冻结的汇率（该币种 → CNY），把原币种原值折成人民币——它只服务毛利核算，
/// 不改对客金额。币种与那份汇率对不上时不折（留空）：拿另一个币种的汇率去乘就是编数，
/// 而"编一个数"比"承认折算不出来"糟得多。
///
/// 自算失败（用量自相矛盾或溢出）时记成 `unavailable`：本该有金额却算不出来，也是缺口，
/// 不用别的数顶替。**这次的执行证据不在手里**（失败件没有结果张数与用量）与自算失败同处置：
/// 算不出来就是缺口。
fn provider_cost_fact(
    snapshot: &PriceSnapshot,
    provider_cost: &ProviderCost,
    inputs: CostInputs<'_>,
) -> ProviderCostFact {
    // 三态在 SDK 与领域各有一套写法，来源一律经那一处映射取，不在这里再判一次。
    let mut source = ProviderCostSource::from(provider_cost);
    // 形状只有一条规则：有金额的来源两样都在，`unavailable` 两样都不在。
    let (amount_microusd, currency) = match provider_cost {
        ProviderCost::Declared(cost) => (Some(cost.amount_microusd), Some(cost.currency.clone())),
        ProviderCost::Computed => match self_computed_cost(snapshot, inputs) {
            Some(amount) => (Some(amount), snapshot.cost_currency().map(str::to_owned)),
            None => {
                source = ProviderCostSource::Unavailable;
                (None, None)
            }
        },
        ProviderCost::Unavailable => (None, None),
    };
    let cny_microusd = match (
        amount_microusd,
        currency.as_deref(),
        snapshot.fx_rate.as_ref(),
    ) {
        (Some(amount), Some(currency), Some(rate)) if rate.currency == currency => {
            rate.to_cny_microusd(amount).ok()
        }
        _ => None,
    };
    ProviderCostFact {
        source,
        amount_microusd,
        currency,
        cny_microusd,
    }
}

/// 这次执行手上有哪些证据——自算成本能拿到的输入因此是**类型上的事实**，不是"某个参数恰好为
/// `None`"。
///
/// 成功件手里有本次实际用量与产出张数；失败件什么都没有（它只有 Driver 已经读到的金额，
/// 那是 `declared` 那条路）。把这件事写进类型，是为了让"失败件一律算不出自算成本"这条口径
/// 落在调用处看得见的地方，而不是靠一个 `None` 的含义。
enum CostInputs<'a> {
    /// 成功件：本次实际用量 + 产出的图片张数。
    Succeeded {
        usage: &'a TokenUsage,
        images: usize,
    },
    /// 失败件：只有上游可能报回来的金额，自算一律算不出来。
    Failed,
}

/// 按这条供给的**计价形态**自算成本（渠道不给金额字段时走这里）。
///
/// 形态决定算法，参数与用量都随修订发布、随 Job 冻结：
/// - `token_rates`：实际用量的四个分项 × 该渠道四档费率（Price Plan 就是它的参数）；
/// - `per_image`：**产出的张数** × 每张单价；
/// - `per_call`：**1 次** × 每次单价；
/// - `upstream_declared`：平台没有可算的东西——上游没给金额就是缺口，不编一个数。
///
/// 缺参数或缺用量（失败件、旧修订没有那份费率、快照里没有单价）⇒ `None`，由调用方落
/// `unavailable`：来源可辨、进缺口清单。
fn self_computed_cost(snapshot: &PriceSnapshot, inputs: CostInputs<'_>) -> Option<u64> {
    let CostInputs::Succeeded { usage, images } = inputs else {
        return None;
    };
    match snapshot.formula {
        PricingFormula::TokenRates => snapshot
            .cost_rates()
            .and_then(|rates| rates.amount_microusd(usage).ok()),
        PricingFormula::PerImage => {
            let unit = snapshot.cost_unit_price_microusd?;
            let count = u64::try_from(images).ok()?;
            unit_amount_microusd(count, unit).ok()
        }
        PricingFormula::PerCall => {
            let unit = snapshot.cost_unit_price_microusd?;
            unit_amount_microusd(1, unit).ok()
        }
        PricingFormula::UpstreamDeclared => None,
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
/// - **图片字段是"合同外字段丢弃"的例外**：参考图与遮罩不是多带的旋钮，而是这次请求的实质。
///   合同没为它们留位置时不能丢——丢图等于悄悄生成一张没有参考图的图，还照样计费；也不能说成
///   平台侧故障（供给面没问题，是这个模型不接图）。一律按参数错拒掉，让调用方换模型或去掉参考图。
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
    // 图片字段按**角色**认（名字以 `image` 开头的是参考图、含 `mask` 的是遮罩），与候选声明面
    // 用的是同一份判据：`image` 与 `image_urls` 在合同里同义，合同声明了其中任何一个都算留了位置。
    if !request.reference_images.is_empty() && !declares_reference_image_parameter(contract) {
        return Err(ApplicationError::InvalidParameter(format!(
            "the contract for model {} declares no reference image parameter; drop the reference image or use a model that takes one",
            request.model
        )));
    }
    if request.mask.is_some() && !declares_mask_parameter(contract) {
        return Err(ApplicationError::InvalidParameter(format!(
            "the contract for model {} declares no mask parameter; drop the mask or use a model that takes one",
            request.model
        )));
    }
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
/// **承载校验**：请求里**实际用到**的每个字段（非空值）都必须被这条候选承载；缺一个就是
/// 这条候选不合格，返回原因写进路由判定记录。这正是"声明了承载面"的意义——供给说了自己能把哪些
/// 字段带到线上，平台不替它加码。承载的判据不只看承载面声明：某个合同字段承载面没声明、但映射
/// 的**改名表**把它落到了承载面声明的名字上时，这条供给照样承载得了它（只是线上叫另一个名字）。
///
/// 两个例外都算"表达得了这次请求，只是表达成另一个样子"：
/// - **被尺寸换算消耗**的字段：那条供给把它当作换算的输入（比例 + 档位 → 像素），而不是要原样
///   发出去的字段——像素面渠道的线上根本没有 `resolution` 这个名字，正因为有换算它才承载得了；
/// - 换算本身失败（档案缺那一格、取值不成形状）同样是"这条候选不合格"，理由照旧写进判定记录。
///
/// 合格之后才组装要落进 Job、并发给上游的参数面：
/// 1. 按承载留下名字：承载面声明了这个名字就用它，否则用改名表映射出来的**线上名字**；两边都
///    落不到的名字（含调用方给了空值的）在这里去掉——空值不携带信息，而发一个承载不了的字段名
///    给上游，只会得到上游自己的一套解释；
/// 2. 把参考图与遮罩落到这条候选**自己声明的**参数名上（声明不了就是不合格，绝不静默丢图）；
/// 3. 注入映射声明的**显式默认值**：调用方没给的字段由平台定，而不是由渠道自己的默认值定；
/// 4. 按映射声明做**尺寸换算**：这一步在改名之前做，因为换算的源字段是**合同字段名**；
/// 5. 改名：把还没落到线上的合同字段名换成这条供给线上要发的名字；
/// 6. 按**取值映射表**把取值换成线上取值：表里没有的取值让这条候选不合格（不猜、不透传原值）；
/// 7. 最后看一眼承载面**自己声明的必填字段**是否都在场：供给说了"这次请求必须带上它"，
///    平台不替它省。放在最后是因为前几步都可能把必填项补上（图落在承载面的名字上、默认值注入、
///    尺寸换算写进目标字段），先判会把"其实跑得通"的候选误判成不合格。
///
/// 返回的是"这条候选不合格"的原因，不是请求级错误：换一条承载面更宽的候选仍然可能跑通，
/// 所以它写进路由判定记录，而不是直接回给调用方。
fn prepare_carrier_parameters(
    contract_parameters: &Map<String, Value>,
    request: &CreateImageGenerationRequest,
    offering: &PublishedOffering,
) -> Result<Value, String> {
    let mapping = &offering.parameter_mapping;
    let renames = declared_renames(mapping)?;
    let enum_maps = declared_enum_maps(mapping)?;
    let size = declared_size_mapping(mapping)?;
    for (name, value) in contract_parameters {
        if !is_used_parameter_value(value) {
            continue;
        }
        if size.as_ref().is_some_and(|mapping| mapping.consumes(name)) {
            continue;
        }
        if !carries_parameter(&offering.carrier_schema, renames.as_ref(), name) {
            return Err(format!(
                "this offering cannot carry parameter {name}, which the request uses"
            ));
        }
    }
    // 参数面此时还在**合同名字**上：默认值按合同字段名注入、尺寸换算按合同字段名取输入，改名与
    // 取值映射放到最后统一落到线上形态。落不进承载面的名字在这里就被丢掉，与"未声明的参数不上行"
    // 是同一条规则。
    let mut parameters = contract_parameters
        .iter()
        .filter(|(name, _)| carries_parameter(&offering.carrier_schema, renames.as_ref(), name))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect::<Map<_, _>>();
    place_image_inputs(
        &offering.carrier_schema,
        &mut parameters,
        &request.reference_images,
        request.mask.as_deref(),
    )?;
    apply_parameter_defaults(
        &offering.capability_schema,
        &offering.carrier_schema,
        renames.as_ref(),
        declared_defaults(mapping),
        &mut parameters,
    );
    if let Some(size) = &size {
        // 换算结果写进承载面声明的目标字段。发布期已经拦下"目标字段没被承载面声明"的映射，
        // 这里再判一次是因为落库的那一行也可能来自更早的发布：宁可判这条候选不合格，
        // 也不往线上写一个它没声明过的字段。
        if !declares_parameter(&offering.carrier_schema, &size.target) {
            return Err(format!(
                "this offering's size mapping writes {}, which it does not declare",
                size.target
            ));
        }
        apply_size_mapping(size, contract_parameters, &mut parameters)?;
    }
    apply_parameter_renames(&offering.carrier_schema, renames.as_ref(), &mut parameters)?;
    if let Some(enum_maps) = &enum_maps {
        apply_enum_maps(
            &offering.carrier_schema,
            renames.as_ref(),
            enum_maps,
            &mut parameters,
        )?;
    }
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

/// Driver 报回来的失败 → 失败件的落库事实。
///
/// 成本事实与成功件**同源同形**：Driver 在终态之后判定失败时把已经读到的成本一起报回来，
/// 这里用与成功路径**同一处映射**把它定成落库口径。没有报回来时按 `unavailable` 落——来源可辨、
/// 进成本缺口清单，而不是留一个 NULL 让这笔成本在账上与缺口两头都看不见。
fn failure_from_adapter(snapshot: &PriceSnapshot, error: AdapterError) -> AttemptFailure {
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
            provider_cost: Some(failure_provider_cost(
                snapshot,
                provider.provider_cost.as_ref(),
            )),
        },
        // 平台自己的配置或参数问题：请求在交给渠道之前就被 Driver 挡下，这次执行**没有成本
        // 可采**——四列留 NULL 说的是"根本没采"，与"采了没拿到"（`unavailable`）不是一件事。
        AdapterError::Configuration(message) | AdapterError::UnsupportedInput(message) => {
            AttemptFailure {
                provider_code: "adapter_rejected".to_owned(),
                public_code: PublicErrorCode::PlatformUnavailable,
                message,
                trace_id: None,
                kind: ProviderFailureKind::PlatformInternal,
                target_state: JobState::Failed,
                hold_disposition: HoldDisposition::Release,
                provider_cost: None,
            }
        }
    }
}

/// 失败件上的成本事实：Driver 报回来的那一份直接用，**没报就按 `unavailable` 落**。
///
/// 与成功件共用 [`provider_cost_fact`] 这一处映射，只是失败件手里没有本次执行证据
/// （[`CostInputs::Failed`]：没有用量、也没有产出张数），自算那几态因此一律算不出金额、
/// 落到缺口。留 NULL 而不是 `unavailable` 的话，这笔成本在账上与缺口清单
/// 两头都看不见——而"去核上游账单"正是缺口清单要承载的处置。
fn failure_provider_cost(
    snapshot: &PriceSnapshot,
    provider_cost: Option<&ProviderCost>,
) -> ProviderCostFact {
    match provider_cost {
        Some(provider_cost) => provider_cost_fact(snapshot, provider_cost, CostInputs::Failed),
        None => provider_cost_fact(snapshot, &ProviderCost::Unavailable, CostInputs::Failed),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests;
