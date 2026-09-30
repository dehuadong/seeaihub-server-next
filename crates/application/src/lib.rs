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
    ImageParameterKind, JobId, JobState, LedgerEntry, MeteringEvidence, OfferingCandidate,
    OfferingId, ParameterRenames, PriceRates, PriceSnapshot, PricingFormula, ProviderCostFact,
    ProviderCostSource, PublishedModel, PublishedOffering, PublishedRevision, RoutePolicy,
    RouteStrategy, RuntimeRevisionId, TokenUsage, apply_enum_maps, apply_parameter_defaults,
    apply_parameter_renames, apply_size_mapping, carries_parameter, contract_image_parameter_kind,
    contract_model_identity, declared_defaults, declared_enum_maps, declared_field_names,
    declared_parameter_names, declared_reference_image_limit, declared_renames,
    declared_size_mapping, declares_mask_parameter, declares_parameter,
    declares_reference_image_parameter, is_used_parameter_value, literal_parameter_text,
    place_image_inputs, platform_image_parameters, resolve_size_tier, unit_amount_microusd,
    wire_parameter_name,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    num::NonZeroU64,
    sync::Arc,
    time::Duration,
};
use thiserror::Error;
use uuid::Uuid;

mod alerts;
pub use alerts::{
    AlertCounters, AlertSink, ExecutionAlert, LedgerMismatchAlert, PlatformAlert,
    PlatformAlertExit, PlatformAlerter, is_platform_event,
};

mod auth;
pub use auth::{
    AdminLogin, CustomerLogin, MIN_SECRET_LENGTH, check_secret, current_admin_id,
    dummy_verifications, hash_password, invalid_credentials, new_session_token, normalize_email,
    session_expiry, session_is_valid, session_token_hash, verify_dummy_password,
    verify_login_secret, verify_password, with_admin_id,
};

mod ledger_audit;
pub use ledger_audit::{
    LedgerAuditPolicy, LedgerAuditReport, LedgerAuditor, LedgerBalanceMismatch,
    OpenLedgerCaseCommand,
};

mod declared_images;
pub use declared_images::{DeclaredOutputImages, declared_output_images};

mod request_timeout;
pub use request_timeout::{
    DEFAULT_BASE_SECONDS, DEFAULT_INCLUDED_IMAGES, DEFAULT_PER_IMAGE_SECONDS,
    NO_CONTRACT_MAX_OUTPUT_IMAGES, RequestTimeoutPolicy, SYNC_WAIT_OVERHEAD_SECONDS,
    requested_image_count, upstream_timeout,
};

mod retry;
pub use retry::{DEFAULT_BACKOFF_BASE_MS, DEFAULT_MAX_ATTEMPTS, MAX_BACKOFF_SECONDS, RetryPolicy};

mod cost_ceiling;
pub use cost_ceiling::{RequestCostCeiling, single_request_cost_cny};

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
    /// 厂商标识。**引用式发布（`references`）里可以不给**：它由被引用的 Offering 所属的厂商模型决定，
    /// 服务端从库里取。内联那条老路仍必填。
    ///
    /// 为什么是"可缺省、给了就校验一致"而不是随便给个默认值：厂商模型的身份本来就在素材里，让调用方
    /// 再抄一遍，抄错了就会写进一次发布——而这是**运营不该提供的字段**。
    #[serde(default)]
    pub vendor_id: Option<String>,
    /// 厂商原生名。与 `vendor_id` 同理，引用式发布里可以不给。
    #[serde(default)]
    pub native_model_id: Option<String>,
    /// **平台对客名**（网关模型名）：调用方提交 `model` 时用的那个名字，也是这次发布
    /// **原子替换**的对象。缺省时回退取 `native_model_id`——今天两者同值，老素材、老已发布
    /// 数据与老测试因此逐位不变。
    #[serde(default)]
    pub gateway_model: Option<String>,
    /// 厂商的修订标识。与 `vendor_id` 同理，引用式发布里可以不给。
    #[serde(default)]
    pub native_revision: Option<String>,
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
    /// **引用式候选**：运营给的是"选中的 Offering + 这条候选的价"，技术定义由被引用的 Offering 决定
    /// （见 `docs/design/0012-platform-model-publishing.md` §4）。与 `offerings` **二选一**。
    ///
    /// 给了它的时候，`vendor_id` / `native_model_id` / `native_revision` / `capability_schema` 都不必给：
    /// 它们由被引用的 Offering 所属的厂商模型决定，服务端从库里取。这与内联那条老路（`offerings`）的
    /// 区别正是这一件事——那条路要运营把工程师做过的技术定义再写一遍。
    #[serde(default)]
    pub references: Option<Vec<OfferingReference>>,
    pub actor: String,
}

/// 一条**引用式**候选：引用哪条 Offering、放在哪一档、以及**这条候选的价**。
///
/// 它不带任何技术字段：驱动器、供应商模型名、渠道三要素、承载面、参数映射、限制都由被引用的
/// Offering 决定（`docs/design/0012-platform-model-publishing.md` §2）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfferingReference {
    pub offering_id: OfferingId,
    /// 档位：数字小者优先。缺省取它在数组里的下标，与内联那条路同一口径。
    #[serde(default)]
    pub routing_priority: Option<i32>,
    /// 档内分流比（正整数，缺省 1）。
    #[serde(default)]
    pub weight: Option<u32>,
    /// 按 token 计量量时的对客四档 CNY 费率向量（`0007` §2）。
    #[serde(default)]
    pub consumer_rates_cny: Option<ConsumerRatesCny>,
    /// 该候选的**对客计价形态**（运营按候选选，与成本形态独立）；缺省 = 等于成本形态。
    #[serde(default)]
    pub consumer_formula: Option<String>,
    /// 成本币种；缺省取该 Offering 实际生效的成本币种。
    #[serde(default)]
    pub cost_currency: Option<String>,
    #[serde(default)]
    pub reference_cost_microusd: Option<u64>,
    #[serde(default)]
    pub cost_basis: Option<String>,
    #[serde(default)]
    pub tier_prices: Option<Value>,
    #[serde(default)]
    pub floor_amounts: Option<Value>,
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
    /// 候选的**技术定义**从哪里取：`true` = 由被引用的 Offering 行决定（引用式发布），`false` = 用
    /// 请求里内联的那些值（老形状）。仓储据此决定发布时写进条目快照的是哪一份。
    pub definitions_from_offerings: bool,
    /// 候选集：档位与档内权重都已在归一阶段定好（见 [`NormalizedOffering`]）。
    pub offerings: Vec<NormalizedOffering>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfferingDraft {
    /// **引用式发布选中的那一行供给**（内联形状不给）。
    ///
    /// 引用式发布按它**直查**那行——不是按身份四元组反查：同一个厂商模型、同一条渠道下可能有多行供给，
    /// 反查会挑错行或挑不到。`None` = 内联形状，按身份复用或新建。
    #[serde(default)]
    pub offering_id: Option<OfferingId>,
    /// 渠道身份的三个字段与驱动器：**可以整组省略**（见 `docs/design/0010` §4.1 的增量发布），
    /// 省略时由服务端从该型号当前生效的修订按 `provider_kind` + `provider_model_id` 复用。
    ///
    /// 用 `Option` 而不是空串：这样"没给"与"给了空串"分得开——前者是沿用，后者是发布者显式声明了
    /// 一个空值，按参数错误拒绝。
    #[serde(default)]
    pub provider_kind: Option<String>,
    #[serde(default)]
    pub adapter_key: Option<String>,
    /// 认同一条候选用的另一半身份；**不可省略**（省略它连"这条候选是谁"都说不清）。
    #[serde(default)]
    pub provider_model_id: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub credential_env: Option<String>,
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
    /// 该候选的**对客计价形态**（运营按候选选，与成本形态独立）；缺省 = 等于成本形态。
    #[serde(default)]
    pub consumer_formula: Option<String>,
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
    /// **这次发布的候选指向哪一行现成的供给**（引用式发布才有）。
    ///
    /// 它是引用式发布的**身份**，与上面那组技术字段是两件事：技术字段是"这条供给长什么样"（发布时快照
    /// 下来），而这个标识是"运营选的是哪一条"。按身份四元组去反查一条供给在这里是**不成立**的——同一个
    /// 厂商模型下、同一条渠道上可能有多行供给，反查会挑错行或者挑不到。所以选中的那条由它自己带着。
    pub offering_id: Option<OfferingId>,
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
    /// 这条供给的**对客费率向量**（对客选 `token_rates` 时的售价）；`None` = 没给（旧口径按
    /// Price Plan 的费率收，或对客选上游声明金额形态）。
    pub consumer_rates_cny: Option<ConsumerRatesCny>,
    /// 这条候选的**对客计价形态**（运营按候选选）；缺省 = 等于成本形态。
    pub consumer_formula: PricingFormula,
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
        definitions_from_offerings: bool,
    ) -> PublishRuntimeRequest {
        // 平台对客名缺省回退取厂商原生名：今天两者同值，老素材不带这个字段也照常可发布。
        // 只写空白等于没写（名字是全空白的话，对客目录会列出一个调不动的名字）。
        //
        // 这三个身份字段在这里是 `Option`，而到这儿已经是"解析之后"：引用式发布的那条路会把它们从被
        // 引用的 Offering 上填好再归一（见 `resolve_referenced_offerings`），内联那条路必须自己带。
        // 所以到这里还是 `None` 只可能是内联形态漏了字段——那不是可以回退成空串的情况，回退会把
        // "这次发布的是哪个厂商模型"变成一个查不出来的空身份，宁可在归一时就拒（见 `normalize_array`
        // 之前的那条校验）。
        let vendor_id = self.vendor_id.unwrap_or_default();
        let native_model_id = self.native_model_id.unwrap_or_default();
        let native_revision = self.native_revision.unwrap_or_default();
        let gateway_model = self
            .gateway_model
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| native_model_id.clone());
        PublishRuntimeRequest {
            vendor_id,
            native_model_id,
            gateway_model,
            native_revision,
            actor: self.actor,
            capability_schema,
            markup_bps: self.markup_bps,
            // 引用式发布的技术定义由仓储从被引用的 Offering 行取：草稿里根本没有它们（那是这次改动的
            // 要点——运营不给技术字段）。内联那条老路自带定义，仓储用请求里的值。
            definitions_from_offerings,
            offerings,
        }
    }

    /// 把命令归一成"一份合同 + 一个有序候选列表"。
    ///
    /// 这是发布接口唯一的入口校验点：`apps/api` 的 `Json<PublishRuntimeCommand>` 反序列化
    /// 之后，下游只处理 [`NormalizedPublication`]。
    pub fn normalize(&self) -> Result<NormalizedPublication, ApplicationError> {
        // 厂商模型的身份要有一个来源：内联形态由调用方给，引用形态由被引用的 Offering 决定
        // （`resolve_referenced_offerings` 已经把它们填进来了）。两条路都在这里会合，所以缺了就是
        // 真的缺——**不拿空串往下走**：那会把"这次发布的是哪个厂商模型"变成查不出来的空身份，
        // 而它决定合同、路由与 Job 固化。
        for (field, value) in [
            ("vendor_id", self.vendor_id.as_deref()),
            ("native_model_id", self.native_model_id.as_deref()),
            ("native_revision", self.native_revision.as_deref()),
        ] {
            if value.is_none_or(|text| text.trim().is_empty()) {
                return Err(ApplicationError::Validation(format!(
                    "{field} is required: give the vendor model identity, or publish by \
                     referencing offerings whose vendor model is known"
                )));
            }
        }
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
        // 倍率只在**对客选"上游金额 × 倍率"**时才被读：其余三种对客形态的价格是运营给的对客价目
        // （token 四档 / 每张 / 每次单价），倍率只用于推导初始值、不是结算的乘数。
        let derives_its_price = offerings
            .iter()
            .any(|offering| offering.consumer_formula == PricingFormula::UpstreamDeclared);
        match self.markup_bps {
            Some(bps) if bps < 0 => Err(ApplicationError::Validation(
                "markup_bps must not be negative".to_owned(),
            )),
            None if derives_its_price => Err(ApplicationError::Validation(
                "markup_bps is required: a candidate whose consumer form is upstream_declared \
                 sells at the amount its provider declares times the markup coefficient"
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
                    // 内联那条老路没有"选中的现成供给"这回事：它按身份去复用或新建一行（渠道字段在
                    // 归一前已经补齐）。所以这里是 `None`，由零候选校验保持必填的语义不变。
                    offering_id: draft.offering_id,
                    carrier_schema,
                    parameter_mapping: draft.parameter_mapping.clone(),
                    restrictions: draft.restrictions.clone(),
                    // 渠道字段在这之前已经补齐（增量发布），所以这里按"必填"取；取不到就是空串，
                    // 由逐候选校验按"渠道身份不完整"拒绝。
                    provider_kind: draft.provider_kind.clone().unwrap_or_default(),
                    adapter_key: draft.adapter_key.clone().unwrap_or_default(),
                    provider_model_id: draft.provider_model_id.clone(),
                    base_url: draft.base_url.clone().unwrap_or_default(),
                    credential_env: draft.credential_env.clone().unwrap_or_default(),
                    formula: billing.formula,
                    rates: billing.rates,
                    price_source_url: billing.price_source_url,
                    cost_unit_price_microusd: billing.cost_unit_price_microusd,
                    cost_currency: billing.cost_currency,
                    consumer_rates_cny: billing.consumer_rates_cny,
                    consumer_formula: billing.consumer_formula,
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
    /// 这条候选的**对客计价形态**（缺省 = 等于成本形态 `formula`）。
    consumer_formula: PricingFormula,
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
    // **对客计价形态**：运营按候选选，与成本形态独立；只有 `token_rates`（按 token 四档）与
    // `upstream_declared`（上游声明金额 × 倍率）两个取值。缺省 = 等于成本形态（旧形状 / 历史
    // 修订）——该解释只在成本形态也是这两个取值时成立；成本形态是按张 / 按次时没有可沿用的
    // 对客形态，必须显式给出。
    let consumer = match draft.consumer_formula.as_deref() {
        None => match declared {
            PricingFormula::TokenRates | PricingFormula::UpstreamDeclared => declared,
            PricingFormula::PerImage | PricingFormula::PerCall => {
                return Err(ApplicationError::Validation(format!(
                    "offerings[{index}].consumer_formula is required when the cost form is {}: \
                     the consumer form is token_rates or upstream_declared",
                    declared.as_str()
                )));
            }
        },
        Some(value) => match PricingFormula::parse(value) {
            Some(PricingFormula::TokenRates) => PricingFormula::TokenRates,
            Some(PricingFormula::UpstreamDeclared) => PricingFormula::UpstreamDeclared,
            _ => {
                return Err(ApplicationError::Validation(format!(
                    "offerings[{index}].consumer_formula must be token_rates or \
                     upstream_declared, got {value}"
                )));
            }
        },
    };
    if consumer == PricingFormula::TokenRates {
        // 成本本身就按 token 计量量时，那份四档费率可兼作对客费率（旧口径），向量可以不给；
        // 成本不是 token 计量量时没有任何费率可沿用，必须显式给对客向量。
        if draft.consumer_rates_cny.is_none() && declared != PricingFormula::TokenRates {
            return Err(ApplicationError::Validation(format!(
                "offerings[{index}].consumer_rates_cny is required when the consumer form is \
                 token_rates and the cost form is {}: there is no cost rate to fall back on",
                declared.as_str()
            )));
        }
    } else if draft.consumer_rates_cny.is_some() {
        // 对客选上游声明金额时，四档向量永远不会被读，留着它只会让人以为它在生效。
        return Err(ApplicationError::Validation(format!(
            "offerings[{index}].consumer_rates_cny does not apply to consumer_formula {}: the \
             four CNY rates are the token_rates price",
            consumer.as_str()
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
        consumer_formula: consumer,
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
    // "这条候选带定价"的判据是**调用方声明了成本侧的定价**。`cost_basis` 与 `floor_amounts` 不算声明：
    // 它们两态/空表都能从渠道与计价形态推出来（引用式发布就是服务端填的），把它们算进来会让"只给对客
    // 费率、不给参考成本"的正当发布被要求补一个它根本不需要的成本。
    let carries_pricing = draft.reference_cost_microusd.is_some() || draft.tier_prices.is_some();
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
/// 合格 = 启用复核放行（见 `enabled_offerings`），**且**该候选自己的 `restrictions` 允许本次分支
/// 与图片张数，**且**这条供给的承载面能承载这次请求**实际用到**的每个字段（图片要能落到它声明的
/// 参数名上）。后两个条件都必须用该候选自己的声明判断——这正是「每条供给各自声明承载面、限制只
/// 收窄」的落地方式。
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
    enabled_offerings: Option<&HashSet<OfferingId>>,
) -> Result<(PublishedOffering, Value, RoutingDecision), ApplicationError> {
    // 零配置路径：一条策略都没有时的选路，也就是策略层引入之前的行为。
    let choice = RouteChoice {
        strategy: RouteStrategy::PriorityFailover,
        discount_rates: &BTreeMap::new(),
        tag_channel_map: &BTreeMap::new(),
        account_tag: None,
    };
    select_candidate_with_strategy(request, branch, candidates, enabled_offerings, &choice)
}

/// 复核发现供给或它的渠道已停用时的落选原因：它进判定记录，是运营解释"为什么没走这条"的依据。
const DISABLED_OFFERING_REASON: &str = "this offering or its channel is disabled";

/// 复核是否把这条候选判掉了：只有复核结果存在、且这条供给不在"仍然启用"里时才算停用。
///
/// 复核结果不存在表示这批候选刚回源读来（取数时已经判过开关），此时一条都不判停用：没有缓存的
/// 那条路径因此与复核引入之前逐位相同。
fn reviewed_as_disabled(
    candidate: &OfferingCandidate,
    enabled_offerings: Option<&HashSet<OfferingId>>,
) -> bool {
    enabled_offerings.is_some_and(|enabled| !enabled.contains(&candidate.offering_id))
}

/// 同 [`select_candidate`]，但由调用方给出这次受理用什么策略、以及该策略要吃的输入。
///
/// 策略只决定"在一批合格候选里挑哪一条"：候选合格与否仍由启用复核与承载面判定，策略不改它们，
/// 也不改选中之后的参数准备与冻结路径。取值空间只有合格候选——不合格的既不进权重之和，也不在
/// 分摊区间里，**策略指定不了它们**。
fn select_candidate_with_strategy(
    request: &CreateImageGenerationRequest,
    branch: ImageBranch,
    candidates: &[OfferingCandidate],
    enabled_offerings: Option<&HashSet<OfferingId>>,
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
            // 复核（只在缓存给出的候选集上做）说这条供给或它的渠道已经停用 ⇒ 不合格，与"承载面
            // 表达不了"同一条路：不进权重之和、不被任何策略指定。判在承载面之前：开关关掉是更准确
            // 的落选原因（进判定记录，运营据此解释"为什么没走这条"），也无须替一条已经停用的候选
            // 再准备参数。
            if reviewed_as_disabled(candidate, enabled_offerings) {
                skip_reason = Some(DISABLED_OFFERING_REASON.to_owned());
            } else {
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

/// 每把 API Key 的请求速率上限：**一个窗口**内允许多少次请求，以及这个窗口有多长。
///
/// 默认是"每分钟 60 次"，取的是运维口径而不是产品口径：它要挡住的是**一把密钥刷满整个平台**这种
/// 形态（脚本没退避、密钥被贴进别人的工具里），不是给正常调用方设精算过的档位。因此这个数只保证
/// "明显异常的密度必然被挡"，正常用量离它很远；要按客户分级，改的是部署期的环境变量，不是这里。
///
/// 计数落在缓存里、且**缓存不可用时放行**，理由见 [`AccelerationService::consume_request_slot`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenerationRateLimit {
    pub max_requests: u64,
    pub window: Duration,
}

impl GenerationRateLimit {
    /// 运维默认值：每分钟 60 次。
    #[must_use]
    pub fn default_limit() -> Self {
        Self {
            max_requests: 60,
            window: Duration::from_secs(60),
        }
    }

    pub fn new(max_requests: u64, window: Duration) -> Result<Self, ApplicationError> {
        if max_requests == 0 {
            return Err(ApplicationError::Configuration(
                "the generation rate limit must allow at least one request per window".to_owned(),
            ));
        }
        // 窗口为 0 时"每个窗口的次数"没有意义，而且 `set` 的存活时间会变成 0。
        if window.is_zero() {
            return Err(ApplicationError::Configuration(
                "the generation rate limit window must be positive".to_owned(),
            ));
        }
        Ok(Self {
            max_requests,
            window,
        })
    }

    /// 从环境变量读运维取值：`GENERATION_RATE_LIMIT_REQUESTS_PER_WINDOW` 与
    /// `GENERATION_RATE_LIMIT_WINDOW_MS`，两项都没给就用默认值。
    ///
    /// 窗口按**毫秒**读，好让部署与用例能配到秒以下的窗口；默认值仍是设计里那个"每分钟"。
    pub fn from_env() -> Result<Self, ApplicationError> {
        Self::new(
            rate_limit_env("GENERATION_RATE_LIMIT_REQUESTS_PER_WINDOW", 60)?,
            Duration::from_millis(rate_limit_env("GENERATION_RATE_LIMIT_WINDOW_MS", 60_000)?),
        )
    }
}

/// 读一个"次数"或"毫秒数"的整数参数；没给或给空取默认值。
fn rate_limit_env(name: &str, default: u64) -> Result<u64, ApplicationError> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => value
            .trim()
            .parse::<u64>()
            .map_err(|_| ApplicationError::Configuration(format!("{name} must be an integer"))),
        _ => Ok(default),
    }
}

/// 每账户**当天已经花掉**多少（microusd）的**上限配置**：默认值与它算不算产品档位。
///
/// 判据本身——每天一行的已完成实收合计、成功结算在写 `capture` 的同一事务累加、受理只读
/// 当天一行而不扫历史流水、且不读缓存——归 [`HubRepository::daily_spend_microusd`]。
///
/// 默认是每账户每天 50 美元等值（`50_000_000` microusd）。这是**运营取值**而不是产品档位：
/// 它挡的是"没人看管的脚本把账户余额在一天里烧光"这种形态，不是一个精算过的额度；要按客户
/// 分级，改的是部署期的环境变量，不是这里。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenerationDailySpendLimit {
    /// 一个自然日（UTC）内允许扣掉的总额度，单位 microusd。
    pub max_daily_spend_microusd: u64,
}

impl GenerationDailySpendLimit {
    /// 运维默认值：每天 50 美元等值。
    #[must_use]
    pub fn default_limit() -> Self {
        Self {
            max_daily_spend_microusd: 50_000_000,
        }
    }

    /// 上限必须是正数：0 等于"这个账户一次都不许花"，那不是额度、是关停；真要关停应当走
    /// 账户与密钥那条路，而不是把额度设成 0 让每个请求都撞在一个说不清的错误上。
    pub fn new(max_daily_spend_microusd: u64) -> Result<Self, ApplicationError> {
        if max_daily_spend_microusd == 0 {
            return Err(ApplicationError::Configuration(
                "the daily spend limit must be positive".to_owned(),
            ));
        }
        Ok(Self {
            max_daily_spend_microusd,
        })
    }

    /// 从环境变量读运维取值：`GENERATION_MAX_DAILY_SPEND_MICROUSD`；没给或给空用默认值。
    pub fn from_env() -> Result<Self, ApplicationError> {
        Self::new(rate_limit_env(
            "GENERATION_MAX_DAILY_SPEND_MICROUSD",
            50_000_000,
        )?)
    }
}

/// 判这次受理会不会把账户当天花超，并在超了时给出"到次日零点还有多久"。
///
/// 判据是 `spent_microusd >= limit`：**已花到顶**就拒，而不是"要超过才拒"——额度是一天的
/// 天花板，花到正好等于天花板时，今天已经没有余量再受理一次了。
///
/// `retry_after` 是**到当天结束**的秒数，不是某个窗口的长度：配额按自然日恢复，消费者要的
/// 答案就是"明天零点之后再来"。向上取整到秒由对客那一层做，这里不提前取整，否则一个
/// "还有 0.4 秒"的余量会被写成 1 秒以外的值、或干脆写成 0。
fn daily_spend_limit_error(
    max_daily_spend_microusd: u64,
    spent_microusd: u64,
    now: DateTime<Utc>,
) -> Option<ApplicationError> {
    if spent_microusd < max_daily_spend_microusd {
        return None;
    }
    let next_day = (now.date_naive() + ChronoDuration::days(1))
        .and_hms_opt(0, 0, 0)
        .map(|naive| DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc));
    let retry_after = next_day
        .map(|next_day| next_day - now)
        .and_then(|remaining| remaining.to_std().ok())
        // 时钟落在当天最后一刻时余量可能不足 1 纳秒：给 1 毫秒，不给出 0——0 会被对客那一层
        // 当成"没有值"或"立刻可重试"，两种都不是事实。
        .unwrap_or_else(|| Duration::from_millis(1));
    Some(ApplicationError::DailySpendLimitExceeded {
        retry_after: retry_after.max(Duration::from_millis(1)),
    })
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
    /// 这次失败在**上游受理**这件事上的可判定性（Driver 如实区分的那一态）。
    ///
    /// 落库不需要它（Attempt 的状态由 `target_state` 决定），但重投的判据只有它：
    /// `SafeBeforeAcceptance` 是"可证明上游没有受理"，重投不会付两次上游成本；其余两态一律
    /// 不重投。放在这里而不是在编排层按错误码再猜一遍——能用哪一态只有 Driver 手里的报文说得清。
    pub retry_safety: RetrySafety,
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

/// 一次"可证明未受理"的失败：把这次执行收尾，并让同一台 Job 在稍后重投。
///
/// 与 [`AttemptFailure`] 分开，是因为它**不是终态处置**：预授权在重投期间原样保留
/// （不释放、不重新扣一遍），Job 回到"等执行"，只结算一次——也就是这台 Job 最后成功或
/// 用尽额度失败的那一次。把它塞进 [`AttemptFailure`] 会让"失败处置"同时有两种意思
/// （终态 / 还要再来一次），而这两种意思在账上的后果完全不同。
#[derive(Debug, Clone)]
pub struct UnacceptedAttempt {
    pub job_id: JobId,
    pub worker_id: String,
    pub attempt_id: AttemptId,
    /// 这次是第几次执行（`attempt_no`），用来算退避与判定还有没有额度。
    pub attempt_no: u32,
    /// 这次执行自己的失败事实（渠道码、原文、成本四列）——**逐次归**，与终态那次一样。
    pub failure: AttemptFailure,
    /// 下一次允许领取这台 Job 的时刻（由编排层按退避算好）。
    pub next_attempt_at: DateTime<Utc>,
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
/// 两种来源共用一个案例：**某次执行**（`job_id` / `attempt_id` 有值——上游是否受理不确定，或结果
/// 交付不了），与**某个账户的账实不符**（两个都为空、只有 `account_id`：被核对的是余额与它的
/// 账本，不是某一次执行）。两种共用同一张表、同一套状态与同一个清单，不另立一套。
///
/// `provider_trace_id` 是**人工去上游核对的依据**（任务式上游的 task id；
/// 逐请求式上游的响应头标识）。没有它，对账的人不知道该查哪个任务——
/// 所以它必须出现在列表里，而不是只能去翻数据库。账户级案例没有它：没有上游请求可查。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciliationCaseView {
    pub id: Uuid,
    pub job_id: Option<JobId>,
    pub attempt_id: Option<AttemptId>,
    pub account_id: AccountId,
    pub reason: String,
    pub provider_trace_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// 流水里的一条账目（管理员的读模型）：条目身份、类别、金额，以及归属的执行记录与写入时刻。
///
/// `kind` 用字符串落在这个视图上（`credit` / `hold` / `capture` / `release` / `adjustment`）：
/// 它是**管理端取值契约**，与账本里的存储取值同名——改枚举名即改接口。金额带上正负号，符号是
/// 语义的一部分。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEntryView {
    pub account_id: AccountId,
    pub kind: String,
    /// 人民币微单位；持有与扣费为负、释放与调整为正。
    pub amount_microusd: i64,
    pub job_id: Option<JobId>,
    pub created_at: DateTime<Utc>,
}

impl From<LedgerEntry> for LedgerEntryView {
    fn from(entry: LedgerEntry) -> Self {
        Self {
            account_id: entry.account_id,
            kind: entry.kind.as_str().to_owned(),
            amount_microusd: entry.amount_microusd,
            job_id: entry.job_id,
            created_at: entry.created_at,
        }
    }
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

/// 管理端看到的**一个账户**：账户标识、余额、标签、绑定的登录邮箱与两个时刻。
///
/// 它是"先找到再操作"那条路径上的列表项，所以只带**用来挑出目标账户**的字段，不带流水与密钥——
/// 那些等选中之后按账户读。**没有持有中**：那是"这笔钱扣没扣"的第二个数，与余额并列才有意义，
/// 列表里放不下这个对比，放进详情读。
///
/// `email` 是**登录身份**那一侧的事实（一个账户最多一个邮箱）：运营按邮箱找账户，列表里就得能看见
/// 它，否则搜出来的行认不出是谁。没有登录身份时为 `None`——那是"运营直接建的账户"，不是"取不到"。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountSummary {
    pub account_id: AccountId,
    pub balance_microusd: i64,
    /// 运营设的标签；没设过就是 `None`。
    pub tag: Option<String>,
    /// 绑定的登录邮箱；这个账户还没有登录身份时为 `None`。
    pub email: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 一条 API Key 的**只读视图**（对客自助列表用）。
///
/// **没有明文**：密钥在库里只有摘要，创建那一次之后就再也拿不回来，所以这里只有标签、创建时间与
/// 吊销时间。`revoked_at` 有值表示这把已经停用。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyView {
    pub key_id: Uuid,
    pub label: String,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

/// 对客的**一次生成请求**（用量记录里的一行）。
///
/// 它是执行记录的**对客投影**，不是执行记录本身：[`CONTEXT.md`](../../CONTEXT.md) 把 Generation Job
/// 定为"对客不可见、不投射成对客协议"，所以这里**没有任务号与内部状态**——客户要看的是"什么时候、
/// 什么型号、几张、扣了多少"，不是平台内部的任务标识。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomerUsageView {
    /// 这一次请求的 Job 标识。对客投影**不**回它；管理员面要它来回答"哪一笔扣费对应哪次调用"。
    pub job_id: JobId,
    /// K 平台型号名（对客的那个名字）。
    pub gateway_model: String,
    /// 对客状态：`succeeded` / `failed` / `pending`（内部 Job 状态收敛过的三值）。
    pub status: CustomerUsageStatus,
    /// 这一次是同步生成还是图片编辑（对客协议里本来就有的两类调用）。
    pub kind: CustomerUsageKind,
    /// 这一次请求的**受理时刻**；区间过滤时，处理中的请求按它归属（`0002` §5）。
    pub created_at: DateTime<Utc>,
    /// 这一次请求的**终态时刻**（`succeeded` / `failed` / `canceled`）；未定终态时为 `None`。
    ///
    /// 已完成请求按它归属区间，所以跨 UTC 日结算的扣费落在结算日那一笔（`0002` §5）。
    pub terminal_at: Option<DateTime<Utc>>,
    /// 产出张数；失败或未完成时是 0。
    pub image_count: u32,
    /// 这一次实际扣掉的钱（人民币微单位）。
    pub charged_microusd: i64,
}

/// 对客状态：**收敛过的取值**，不是内部 `JobState` 的取值面。
///
/// 映射写在 [`customer_usage_status`] 上，与既有对客错误改写同一条纪律：内部状态取值不进对客响应。
/// 取消单独一个值而不是并进 `pending`：取消是**终态**，并进去客户会一直看到"处理中"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CustomerUsageStatus {
    Succeeded,
    Failed,
    Pending,
    Canceled,
}

/// 对客的调用类别：同步生成 / 图片编辑。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CustomerUsageKind {
    Generation,
    Edit,
}

/// 把内部 Job 状态收敛成对客取值。
#[must_use]
pub fn customer_usage_status(state: seeai_domain::JobState) -> CustomerUsageStatus {
    match state {
        seeai_domain::JobState::Succeeded => CustomerUsageStatus::Succeeded,
        // 失败与对账中：对客都是"这次没成"。对账终会走向失败或补回，不会对客可见地悬着。
        seeai_domain::JobState::Failed | seeai_domain::JobState::ReconciliationRequired => {
            CustomerUsageStatus::Failed
        }
        // 取消是终态，不能并进"处理中"。
        seeai_domain::JobState::Canceled => CustomerUsageStatus::Canceled,
        seeai_domain::JobState::Accepted
        | seeai_domain::JobState::Leased
        | seeai_domain::JobState::Submitting => CustomerUsageStatus::Pending,
    }
}

/// 对客的**账单汇总**：一段时间内发生了什么、花了多少。
///
/// 与逐笔用量**口径不同**：汇总按整段区间**全量**算，明细按条数上限截断——所以汇总不会随页大小
/// 变化（否则"明细求和等于汇总"这条验收条件会随分页摇摆）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CustomerBillingSummary {
    pub requests: i64,
    pub images: i64,
    pub charged_microusd: i64,
}

/// 对客账务读的查询条件：**半开区间 `[since, until)`**，按 UTC 解释。
///
/// `since` 缺省为不限、`until` 缺省为"到此刻"；半开是为了相邻区间既不漏算也不重复算。
/// `limit` 只作用于逐笔列表，不影响汇总。
#[derive(Debug, Clone, Copy)]
pub struct CustomerBillingQuery {
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub limit: u32,
}

/// 管理端看到的**一条客户**：邮箱身份与它指向的账户。
///
/// 它回答的是"这个邮箱是哪个账户"——给客户充值、替客户签重置令牌都要先拿到 `account_id`。
/// **不含口令哈希、会话与余额**：余额是账本的事实，按 `account_id` 走账户那条读。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomerView {
    pub customer_id: Uuid,
    pub email: String,
    pub account_id: AccountId,
    pub created_at: DateTime<Utc>,
    pub last_login_at: Option<DateTime<Utc>>,
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
    /// 该候选的**对客计价形态**（随修订发布）；不带定价时为 `null`。
    pub consumer_formula: Option<PricingFormula>,
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
    /// 这次生效发布的**厂商模型合同**（Vendor Model Contract）。
    ///
    /// 回显它不是回显渠道配置：合同是厂商给的结构声明，调用方本来就按它提交参数、对客目录也发布它。
    /// 管理端需要它才能在**改价**时重发同一份合同——合同不属于"发布者这次要改变的东西"，让运营重贴
    /// 一遍只会引入抄错的机会。
    pub capability_schema: Value,
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
    /// 这次受理按该候选的计价形态算下来，可能花掉的上游成本超过了运营设的**单次请求成本上限**。
    ///
    /// 与 [`Self::InsufficientBalance`] 分开：那个是**客户**的钱不够，这个是**平台**自己划的护栏
    /// ——请求本身没问题、客户的余额也够，是这次执行可能让平台付得太多（上限被调小、折算率变差、
    /// 或发布物配错了）。对客必须是**平台侧故障**，不是"你余额不足"。
    /// 带的是这次算出来的成本与上限本身，写给运营看（对客那层只说平台不可用）。
    #[error("request cost ceiling exceeded: {0}")]
    RequestCostCeilingExceeded(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("insufficient balance")]
    InsufficientBalance,
    #[error("too many requests in flight")]
    TooManyInFlight,
    /// 这把密钥在当前窗口内已经用满每分钟请求数。
    ///
    /// 与 [`Self::TooManyInFlight`] 分成两个错误：那个是"上一个还没跑完"，等一会儿重发同一个请求
    /// 就行；这个是"这一分钟发得太密"，要等到下一个窗口。对客因此要能用两个不同的码区分。
    /// 窗口长度随错误一起给出，调用方才知道该说"多久之后再来"。
    #[error("rate limit exceeded; retry after {retry_after:?}")]
    RateLimitExceeded { retry_after: Duration },
    /// 该账户**当天已经花掉**的钱达到了运营设的每日上限。
    ///
    /// 与 [`Self::RateLimitExceeded`]、[`Self::TooManyInFlight`] 三者互不相同，因为对客该做
    /// 的事完全不同：那个是"这一分钟发得太密，等一下再发"、那个是"上一个还没跑完"、这个是
    /// "今天的额度用完了，明天再来"——合成一个码，消费者只能盲目重试。
    /// `retry_after` 是到次日零点（UTC）的秒数。
    #[error("daily spend limit reached; retry after {retry_after:?}")]
    DailySpendLimitExceeded { retry_after: Duration },
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

    /// 该型号**当前生效**的修订里，可以按身份被沿用的候选：按 `provider_kind` 给出候选与其渠道三要素。
    ///
    /// 它是增量发布的依据（见 `docs/design/0010` §4.1）：发布命令省略渠道三要素时，服务端要能从上一版
    /// 取回它们。**不过滤 `enabled`**——停用的候选也可能要被"沿用之后重新启用"，按启用状态过滤会让它
    /// 在改价时突然找不到。型号无从查起（没有生效修订）时返回空 `Vec`。
    async fn active_offering_channels(
        &self,
        gateway_model: &str,
    ) -> Result<Vec<ActiveOfferingChannel>, ApplicationError>;

    /// 按标识取一组 Offering 连同它们所属的厂商模型与渠道——引用式发布的解析依据。
    ///
    /// 返回的顺序与传入的 `offering_ids` 一致，且**漏掉取不到的**标识：调用方要按标识逐个核对，
    /// 缺了哪条就在错误里点名（见 [`RuntimeService::resolve_referenced_offerings`]）。
    /// 不过滤 `enabled`：停用由逐候选校验按活表判并给出可读的理由，不在这里默默丢掉。
    async fn offerings_by_id(
        &self,
        offering_ids: &[OfferingId],
    ) -> Result<Vec<ReferencedOffering>, ApplicationError>;

    /// 管理员读：可被运营选中的 Offering 清单——发布页"选 vendor → 勾 Offering"的数据来源。
    ///
    /// 一条供给一项，按厂商、厂商模型名与渠道稳定排序（调用方据此分组）。**不含渠道地址与凭证变量名**：
    /// 那是渠道部署事实，选择用不到（`docs/design/0012-platform-model-publishing.md` §2.1）。
    ///
    /// **不过滤 `enabled`**：停用的照样列出来并带上它的状态，运营才看得出"为什么这条选不了"——藏起来
    /// 等于"关掉之后再也找不到怎么打开"。库里一条供给都没有时返回空 `Vec`，不是错误。
    async fn selectable_offerings(&self) -> Result<Vec<SelectableOfferingView>, ApplicationError>;

    /// 取该型号当前的 **active 候选集合**，按 `routing_priority` 升序。
    ///
    /// 同一模型的 active 候选集**永远来自同一个 Revision**（发布即原子替换）。
    /// 无任何 active 候选时返回空 `Vec`，不是错误——由调用方判定"无合格候选"。
    async fn active_offering(
        &self,
        gateway_model: &str,
    ) -> Result<Vec<OfferingCandidate>, ApplicationError>;

    /// 受理路径的**开关复核**：这些供给里，此刻仍然启用的有哪些（它的渠道也启用）。
    ///
    /// 判据与 [`Self::active_offering`] 那条 `o.enabled AND c.enabled` **同一条**，取值方式不同：
    /// 这里按供给的主键点读（一次一批，不逐条查），不重新解析发布、不看修订标识。启停是可变表里
    /// 的事、**不改变修订标识**——route 缓存里那份候选集在停用之后仍然"看起来是新的"，所以停用
    /// 要立刻生效只能靠这次复核，而它与那次失效有没有成功无关。
    ///
    /// 只回"还作数的供给 id"，不回候选本身：候选由缓存给出，这里判的是它还作不作数。空集合直接
    /// 回空集合，不查库。读失败按平台侧故障往外抛（出错就整次受理失败），不退化成"不复核"——
    /// 静默放行会让停用在读失败的那几次请求上重新失效。
    async fn enabled_offerings(
        &self,
        offering_ids: &[OfferingId],
    ) -> Result<HashSet<OfferingId>, ApplicationError>;

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
    /// 启停不改变修订标识，失效因此只影响命中率；停用本身的生效见 [`Self::enabled_offerings`]。
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

    /// 列出账户，供运营**先找到再操作**：按 `created_at` 倒序，`email` 与 `tag` 都是精确匹配。
    ///
    /// `email` 走 `identity.customers` 的绑定关系（一个邮箱指向一个账户），`tag` 走
    /// `ledger.accounts.tag`。两个条件都给时是**与**的关系。没有条件时就是"最近创建的若干条"——
    /// 运营打开账户页先看到的应是最近动过的账户，而不是一个要他先知道 id 的空表单。
    ///
    /// 只读、不写审计。返回条数由调用方夹过上限，这里不再二次截断。
    async fn list_accounts(
        &self,
        email: Option<&str>,
        tag: Option<&str>,
        limit: u32,
    ) -> Result<Vec<AccountSummary>, ApplicationError>;

    /// 按账户读账本流水：**时间倒序**、只取 `since` 之后的、最多 `limit` 条。
    ///
    /// 权威是 `ledger.entries` 本身，这条读不改写任何东西、也不写审计。`since` 是**开区间**：
    /// 上一次拉到的位置本身不该被重复计入增量。同一个事务里写的多条共用 `created_at`（`now()`
    /// 是事务时间），所以分页**只保证时间倒序**，同一时刻内部的先后不承诺——调用方要按整段
    /// 事务去理解它们。账户不存在时返回 [`ApplicationError::NotFound`]，让 404 与"没有流水"
    /// 分得开。
    ///
    /// `until` 是**半开**上界（不含）。与对客账单汇总同一条口径：同一区间下明细与汇总必须对得上，
    /// 否则边界那一笔会被一边算进去、另一边不算。翻页用 `offset` 而不是靠 `since`/`until` 去切——
    /// 那两个参数是给"按区间看"用的，同一时刻可能有多条。
    /// `kind` 只读某一类（例如充值记录只看 `credit`），`None` 读全部；取值面是 `ledger.entries`
    /// 的五个类别。
    async fn read_ledger_entries(
        &self,
        account_id: AccountId,
        since: Option<DateTime<Utc>>,
        until: Option<DateTime<Utc>>,
        kind: Option<&str>,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<LedgerEntry>, ApplicationError>;

    /// 该账户在 `[since, until)` 内的流水**总条数**，供调用方判断还有没有下一页。
    ///
    /// 区间语义与 `read_ledger_entries` 相同，所以"翻到最后一页"这个判断不会因为两处口径不同
    /// 而错位。账户不存在时返回 [`ApplicationError::NotFound`]。
    async fn count_ledger_entries(
        &self,
        account_id: AccountId,
        since: Option<DateTime<Utc>>,
        until: Option<DateTime<Utc>>,
        kind: Option<&str>,
    ) -> Result<u64, ApplicationError>;

    /// 该账户**当前持有中**的金额（人民币微单位）：`ledger.holds` 里还没结算的那些预授权之和。
    ///
    /// 权威在库；它与余额是**两个数**，不合成一个"总资产"——持有中是已预授权未结算的部分，
    /// 也就是说这笔钱**还没真的扣**（预授权不是扣款），合成一个总数会让"这笔钱到底扣没扣"
    /// 说不清。账户不存在时返回 [`ApplicationError::NotFound`]。
    async fn held_microusd(&self, account_id: AccountId) -> Result<i64, ApplicationError>;

    /// 探一次**事实源**是否可达：只做一次 `SELECT 1`，不读任何业务表。
    ///
    /// 它是健康检查的唯一依赖判据。**缓存不可用不算不健康**：缓存是加速层，不可用时系统按"没有
    /// 缓存"继续服务（直查数据库那条路径本来就在），把它算不健康会让编排系统重启一个本来能服务
    /// 的实例。探活是只读幂等的，可以被很短间隔反复调用。
    async fn probe(&self) -> Result<(), ApplicationError>;

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

    /// 建一把密钥。返回新行的 id：调用方（发密钥的响应）要把它交给管理员，好让"吊销哪一把"不必回库捞。
    async fn create_api_key(
        &self,
        account_id: AccountId,
        label: &str,
        key_hash: &str,
        actor: &str,
    ) -> Result<Uuid, ApplicationError>;

    /// 吊销一把密钥：写 `revoked_at`，**不删行**——创建与吊销都是要留痕的历史事实。
    ///
    /// 幂等：已吊销的再调一次仍然成功。调用方要的是"它现在不可用"这个状态，不是"这次调用改变了
    /// 什么"；把重复吊销报成错误只会让重发求助变成故障。键不存在返回 `NotFound`。
    async fn revoke_api_key(&self, key_id: Uuid, actor: &str) -> Result<(), ApplicationError>;

    /// 按密钥摘要查这个调用方是谁：**账户**与**密钥标识**。吊销判定在这一条读里（只认
    /// `revoked_at IS NULL`），因此认证路径每次都要走它。
    async fn api_key_identity(&self, key_hash: &str)
    -> Result<(Uuid, AccountId), ApplicationError>;

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

    /// 开始一次执行：把 Job 推成"提交中"，并**为这次执行写一行 Attempt**。
    ///
    /// `attempt_no` 是同一台 Job 内的第几次（从 1 起）：重投出来的每一次执行都占一行、各记
    /// 自己的用量与成本，所以"这是第几次"必须是落库时确定的序号，不是一个可以事后重排的排序。
    /// **返回值就是这次执行落库的号**——调用方拿它判重投额度，所以定号与写入必须是同一个动作。
    async fn begin_attempt(
        &self,
        job_id: JobId,
        worker_id: &str,
        attempt_id: AttemptId,
        request_digest: &str,
    ) -> Result<u32, ApplicationError>;

    /// 可证明未受理的失败：这次执行收尾，同一台 Job 稍后重投。
    ///
    /// **预授权原样保留**（`ledger.holds` 不动、余额不动）：这次失败上游没开始计费，重投的不是
    /// 一笔新业务，重新预授权等于把同一笔钱扣两遍。余额因此也没有变化，调用方不必写穿缓存。
    ///
    /// Job 回到"等执行"（`state = 'accepted'`）并把 `next_attempt_at` 推到退避之后：领取那条
    /// 查询本来就按 `next_attempt_at <= now()` 取，退避因此不需要另外的调度器。
    ///
    /// 执行到上限仍失败时**不走这里**：那时按既有失败处置（[`Self::fail_job`]）落终态与释放。
    async fn requeue_after_unaccepted(
        &self,
        command: UnacceptedAttempt,
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

    /// 该账户**当天（UTC 自然日）已完成实收**的合计（microusd）。
    ///
    /// 与在飞计数不同，这里问的是**事实**而不是计数：判据是 PostgreSQL 里每账户每 UTC 自然日
    /// 一行的每日合计，成功结算在写 `capture` 的同一事务累加；受理只读当天一行，不扫历史流水
    /// （`0002` §5）。它也不读缓存——缓存一份"今日累计"就得管它的失效与漂移，而漂移出来的数
    /// 恰好会用来决定"要不要拒"，那种错不可接受（详情见 [`GenerationDailySpendLimit`]）。
    ///
    /// 刻意**不给缺省实现**：受理路径上的这道门必须每次都能拿到答案，一个"没实现就当 0
    /// （今天还没花）"的缺省会让新仓库在无声无息中把上限关掉，而"少花钱"这件事没人会发现。
    async fn daily_spend_microusd(&self, account_id: AccountId) -> Result<u64, ApplicationError>;

    async fn list_open_reconciliation_cases(
        &self,
    ) -> Result<Vec<ReconciliationCaseView>, ApplicationError>;

    /// 平台侧失败清单（供运营发现欠费/凭证/配置问题）：按类别与时间筛。
    async fn provider_failures(
        &self,
        query: ProviderFailureQuery,
    ) -> Result<Vec<ProviderFailureView>, ApplicationError>;

    /// 某条候选（供给）**最近** `window` 次已定终态的执行里，开头连续失败了几次。
    ///
    /// 判据只看终态：`failed` 与 `reconciliation_required` 都算失败（都不是"结果交付成功"），
    /// `succeeded` 截断连续段；还没定终态的执行既不计入也不截断——它们还没有结论。
    ///
    /// 这只是**现有执行记录的读法**：计数从 Job 的终态现算，不新增表、也不落一份会漂移的计数。
    /// 读失败按平台侧故障抛出，由调用方决定"这一次不外发"。
    async fn consecutive_offering_failures(
        &self,
        offering_id: OfferingId,
        window: u32,
    ) -> Result<u64, ApplicationError>;

    /// 对账退款（释放预授权）。返回**账户**与退款后的余额：调用方要按账户把余额写穿缓存。
    async fn refund_reconciliation(
        &self,
        command: RefundReconciliationCommand,
    ) -> Result<BalanceChange, ApplicationError>;

    /// 余额与**它自己那本账**对不上的账户（只读）。
    ///
    /// 判据是库内两个事实的比对：`ledger.accounts.balance_microusd` 与该账户
    /// `ledger.entries.amount_microusd` 的符号和。**这不是缓存对账**：两个数都取自数据库，
    /// 缓存不参与；它也**不改任何账**——发现不符是这条查询的全部职责。
    ///
    /// 每一笔账都同时改这两边：充值写一条正数条目并加余额；预授权写一条负数条目并减余额；
    /// 结算写"释放"（正）与"实收"（负）两条并加回差额；失败释放、对账退款同理。所以"两边相等"
    /// 是一条不变量，对不上就意味着有人只改了一边——那正是要报出来的东西。
    async fn accounts_with_ledger_mismatch(
        &self,
    ) -> Result<Vec<LedgerBalanceMismatch>, ApplicationError>;

    /// 给一个对不上的账户建一条对账案例。已经有未结案的那条时什么都不做，返回 `false`。
    ///
    /// 这条案例**没有 Job、也没有 Attempt**：被核对的是账户的余额与它的账本，不是某一次执行。
    /// 状态沿用既有取值（`open` / `resolved`），不新造状态。
    async fn open_ledger_reconciliation_case(
        &self,
        command: OpenLedgerCaseCommand,
    ) -> Result<bool, ApplicationError>;

    /// 列一个账户下的密钥（对客自助；**不含明文**）。
    async fn list_api_keys(
        &self,
        account_id: AccountId,
    ) -> Result<Vec<ApiKeyView>, ApplicationError>;

    /// 对客用量：该账户在 `[since, until)` 里的每一次生成请求，按时间倒序、最多 `limit` 条。
    ///
    /// 投影成对客事实（型号、对客状态、类别、产出张数、扣费金额），**不含 Job 标识与内部状态**。
    async fn customer_usage(
        &self,
        account_id: AccountId,
        query: CustomerBillingQuery,
    ) -> Result<Vec<CustomerUsageView>, ApplicationError>;

    /// 对客账单汇总：同一区间**全量**的请求数、产出张数与扣费总额。
    ///
    /// 扣费总额只算账本里的**扣费与调整**条目（`capture` / `adjustment`）：预授权 `hold` 与它的
    /// 释放 `release` 是一进一出、加起来恒为零，计进来只会得到"平台占用过多少"，那不是扣费。
    async fn customer_billing(
        &self,
        account_id: AccountId,
        query: CustomerBillingQuery,
    ) -> Result<CustomerBillingSummary, ApplicationError>;

    /// 每个币种**当前生效**的那一行折算率：`(币种, 每单位折多少 CNY 微单位, 生效时刻)`。
    async fn current_fx_rates(&self)
    -> Result<Vec<(String, u64, DateTime<Utc>)>, ApplicationError>;

    /// 吊销**属于这个账户**的一把密钥；返回 `false` 表示那个标识不在这个账户名下。
    async fn revoke_api_key_of_account(
        &self,
        account_id: AccountId,
        key_id: Uuid,
        actor: &str,
    ) -> Result<bool, ApplicationError>;

    /// 按邮箱找一个管理员账号：返回 `(id, 口令哈希)`；没有这个邮箱时 `None`。
    ///
    /// 邮箱判据**大小写不敏感**（库里存小写，见迁移 `0020`）：同一个邮箱不该因为大小写不同变成两个人。
    async fn find_admin_by_email(
        &self,
        email: &str,
    ) -> Result<Option<(Uuid, String)>, ApplicationError>;

    /// **只在账号不存在时**建立它，返回 `(admin_id, 是否新建)`。
    ///
    /// 引导走这条而不是 [`Self::upsert_admin_password`]：运维改过口令之后，每次重启再把环境变量里
    /// 那个值写回去，等于把口令打回初始值——引导必须幂等且**不改已有账号**。
    async fn ensure_admin_account(
        &self,
        email: &str,
        password_hash: &str,
    ) -> Result<(Uuid, bool), ApplicationError>;

    /// 写入（或覆盖）一个管理员账号的口令：按邮箱 upsert。
    async fn upsert_admin_password(
        &self,
        email: &str,
        password_hash: &str,
    ) -> Result<Uuid, ApplicationError>;

    /// 记一次登录成功：`last_login_at` 与一条审计（`admin.login`），同一个事务。
    async fn record_admin_login(&self, admin_id: Uuid) -> Result<(), ApplicationError>;

    /// 落一条管理员会话：`(会话 id, 管理员 id, 令牌摘要, 过期时刻)`。
    async fn create_admin_session(
        &self,
        admin_id: Uuid,
        token_hash: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<Uuid, ApplicationError>;

    /// 按令牌摘要取一条管理员会话：返回 `(管理员 id, 邮箱, 过期时刻)`；没有这条会话时 `None`
    /// （过期的判定与清理在用例层，那里才有时钟）。
    async fn find_admin_session(
        &self,
        token_hash: &str,
    ) -> Result<Option<(Uuid, String, DateTime<Utc>)>, ApplicationError>;

    /// 删掉一条管理员会话（退出）。不存在的摘要也算成功：调用方在意的是"它现在不可用"。
    async fn delete_admin_session(&self, token_hash: &str) -> Result<(), ApplicationError>;

    /// 记一条**没有账本副作用**的审计（签发重置令牌这类"只是留痕"的动作）。
    async fn record_audit(
        &self,
        actor: &str,
        action: &str,
        subject_type: &str,
        subject_id: &str,
    ) -> Result<(), ApplicationError>;

    /// 改口令：写新的哈希、吊销该身份全部会话、写一条审计（`admin.password_change`），
    /// **同一个事务**。返回 `false` 表示没有这个管理员。
    ///
    /// 三件事必须一起成功：只成一件会留下"新口令生效、旧会话还能用"或"改了但查不到是谁改的"这类
    /// 半截状态，而 Spec 要求改完旧凭据**立刻**不能再用、且改动留痕。
    async fn set_admin_password(
        &self,
        admin_id: Uuid,
        password_hash: &str,
        actor: &str,
    ) -> Result<bool, ApplicationError>;

    /// 按 id 取管理员的口令哈希（改口令要先比对当前口令）。
    async fn find_admin_password(&self, admin_id: Uuid)
    -> Result<Option<String>, ApplicationError>;

    /// 按**账户**找一个客户的 id（运营按账户签重置令牌时用）。
    async fn find_customer_by_account(
        &self,
        account_id: Uuid,
    ) -> Result<Option<Uuid>, ApplicationError>;

    /// 签发一枚口令重置令牌：先作废该身份此前**未兑换**的令牌，再落新的（同一事务）。
    async fn create_password_reset(
        &self,
        subject_kind: &str,
        subject_id: Uuid,
        token_hash: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<Uuid, ApplicationError>;

    /// 取一枚重置令牌：返回 `(subject_kind, subject_id, expires_at, redeemed_at)`；没有这条摘要时 `None`。
    ///
    /// 有效性与"用过没用过"由用例层判，这里只负责把事实取出来。
    async fn find_password_reset(
        &self,
        token_hash: &str,
    ) -> Result<Option<(String, Uuid, DateTime<Utc>, Option<DateTime<Utc>>)>, ApplicationError>;

    /// 把一枚重置令牌标记为已用（一次性）。影响 0 行说明它已被兑换过。
    async fn redeem_password_reset(&self, token_hash: &str) -> Result<bool, ApplicationError>;

    /// 建一个对客账户与它的登录身份，**一次事务里一起写**：`(客户 id, 账户 id)`。
    ///
    /// 邮箱已被占用时返回 [`ApplicationError::Conflict`]——注册撞邮箱是调用方能自己改的事。
    async fn create_customer(
        &self,
        email: &str,
        password_hash: &str,
    ) -> Result<(Uuid, Uuid), ApplicationError>;

    /// 按邮箱找一个客户：`(客户 id, 账户 id, 口令哈希)`；没有时 `None`。
    async fn find_customer_by_email(
        &self,
        email: &str,
    ) -> Result<Option<(Uuid, Uuid, String)>, ApplicationError>;

    /// 按客户 id 取它的账户（对客会话每次请求都要换出账户来）：没有时 `None`。
    async fn find_customer_account(
        &self,
        customer_id: Uuid,
    ) -> Result<Option<Uuid>, ApplicationError>;

    /// 记一次登录成功：`last_login_at` 与一条审计（`customer.login`），同一个事务。
    async fn record_customer_login(&self, customer_id: Uuid) -> Result<(), ApplicationError>;

    /// 落一条对客会话。
    async fn create_customer_session(
        &self,
        customer_id: Uuid,
        token_hash: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<Uuid, ApplicationError>;

    /// 按令牌摘要取一条对客会话：返回 `(客户 id, 账户 id, 过期时刻)`。
    async fn find_customer_session(
        &self,
        token_hash: &str,
    ) -> Result<Option<(Uuid, Uuid, DateTime<Utc>)>, ApplicationError>;

    /// 删掉一条对客会话（退出）。
    async fn delete_customer_session(&self, token_hash: &str) -> Result<(), ApplicationError>;

    /// 客户改口令（需当前口令）：先按 id 取现有哈希来比对。
    async fn find_customer_password(
        &self,
        customer_id: Uuid,
    ) -> Result<Option<String>, ApplicationError>;

    /// 改口令：写新的哈希、吊销该客户全部会话、写一条审计（`customer.password_change`），
    /// **同一个事务**。返回 `false` 表示没有这个客户。
    async fn set_customer_password(
        &self,
        customer_id: Uuid,
        password_hash: &str,
        actor: &str,
    ) -> Result<bool, ApplicationError>;

    /// 运营替客户开户：给 `email` 配一个登录身份。
    ///
    /// `account_id` 为 `None` 时新建一个空账户；给了就把身份配到那个**已有账户**上（账户可以还没有身份）。
    /// 邮箱已被占用、或该账户已被别的身份绑定时返回 [`ApplicationError::Conflict`]。
    async fn open_customer_account(
        &self,
        email: &str,
        password_hash: &str,
        account_id: Option<Uuid>,
    ) -> Result<(Uuid, Uuid), ApplicationError>;

    /// 按邮箱找一个客户：`(customer_id, account_id, email, created_at, last_login_at)`；没有时 `None`。
    async fn find_customer_view(
        &self,
        email: &str,
    ) -> Result<Option<CustomerView>, ApplicationError>;

    /// 列客户（按创建时间倒序，最近 `limit` 条）。
    async fn list_customers(&self, limit: u32) -> Result<Vec<CustomerView>, ApplicationError>;
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

    /// 每个币种**当前生效**的那一行折算率（折算率页显示录入结果用）。
    ///
    /// 与受理时同一条判据：取"此刻之前已生效、其中最新的一行"。页面因此看到的就是平台真正在用的
    /// 那个数，而不是历史上录过的某一条。
    pub async fn current_fx_rates(
        &self,
    ) -> Result<Vec<(String, u64, DateTime<Utc>)>, ApplicationError> {
        self.repository.current_fx_rates().await
    }
}

/// 认证成功后这个调用方是谁：**哪个账户**、以及**哪把密钥**。
///
/// 密钥标识单独带出来，是因为限流、审计、排障都以"哪把密钥"为单位：一个账户可以有多把密钥，
/// 按账户限流会让一把失控的密钥拖住同一账户的其它密钥。密钥标识本身不含明文，它本来就是
/// 管理员吊销时要用的那个 id。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApiKeyIdentity {
    pub account_id: AccountId,
    pub key_id: Uuid,
}

#[derive(Clone)]
pub struct IdentityService {
    repository: Arc<dyn HubRepository>,
    /// 加速层：认证路径**每次**读库换账户（吊销要即刻生效），只把**速率计数**放进缓存；
    /// 计数怎么判、缓存挂了怎么办，见 [`AccelerationService::consume_request_slot`]。
    acceleration: Arc<AccelerationService>,
    /// 每把密钥的请求速率上限。
    rate_limit: GenerationRateLimit,
}

impl IdentityService {
    /// 没有加速层、也没有额外配置的认证：速率计数没有缓存可落，于是不设限。
    #[must_use]
    pub fn new(repository: Arc<dyn HubRepository>) -> Self {
        let acceleration = Arc::new(AccelerationService::disabled(repository.clone()));
        Self {
            repository,
            acceleration,
            rate_limit: GenerationRateLimit::default_limit(),
        }
    }

    /// 装上加速层与运维给的速率上限。**上限是配置项**：它随部署形态变（内部工具、压测、
    /// 单机开发各要一个数），所以由调用方给，而不是写死在这里。
    ///
    /// 加速层没配缓存（[`AccelerationService::disabled`]）时速率计数无处可落，限流**不生效**：
    /// 调用方要按部署事实把这件事讲出来（见 `apps/api` 启动时那条日志），别让它静悄悄地不发生。
    #[must_use]
    pub fn with_rate_limit(
        mut self,
        acceleration: Arc<AccelerationService>,
        rate_limit: GenerationRateLimit,
    ) -> Self {
        self.acceleration = acceleration;
        self.rate_limit = rate_limit;
        self
    }

    /// 发一把密钥。返回 `(key_id, 明文)`。
    ///
    /// 明文只在这一刻存在：库里只有摘要，事后**没有**任何路径能把它还原出来，也就没有"再查一次密钥"的
    /// 接口。正因如此，吊销要用的标识必须和明文一起回给调用方——不然管理员除了回库翻 id 别无他法，
    /// 而管理员恰恰没有库权限。
    pub async fn issue_api_key(
        &self,
        account_id: AccountId,
        label: &str,
        actor: &str,
    ) -> Result<(Uuid, String), ApplicationError> {
        if label.trim().is_empty() {
            return Err(ApplicationError::Validation(
                "api key label must not be empty".to_owned(),
            ));
        }
        let first = Uuid::new_v4().simple();
        let second = Uuid::new_v4().simple();
        let plaintext = format!("sk_seeai_{first}{second}");
        let key_hash = sha256_hex(plaintext.as_bytes());
        let key_id = self
            .repository
            .create_api_key(account_id, label, &key_hash, actor)
            .await?;
        Ok((key_id, plaintext))
    }

    /// 吊销一把密钥（管理员，写审计）。
    ///
    /// 幂等：已经吊销过的再吊销一次仍然成功——调用方在意的是"它现在不可用"。写完提交，**下一个**
    /// 请求就走不通了：认证路径每次读库判吊销状态（见 [`Self::authenticate`]），这里没有中间缓存
    /// 要等。
    pub async fn revoke_api_key(&self, key_id: Uuid, actor: &str) -> Result<(), ApplicationError> {
        self.repository.revoke_api_key(key_id, actor).await
    }

    /// 列一个账户下的密钥（对客自助）。
    ///
    /// **只回标签、创建时间与吊销状态**：明文在创建那一次之后就再也拿不回来了，这里没有可回的东西。
    pub async fn list_api_keys(
        &self,
        account_id: AccountId,
    ) -> Result<Vec<ApiKeyView>, ApplicationError> {
        self.repository.list_api_keys(account_id).await
    }

    /// 吊销**属于这个账户**的一把密钥（对客自助）。返回 `false` 表示那个标识不在这个账户名下。
    ///
    /// 判据是"账户 + 密钥标识"一起收窄，而不是先查存在再判归属：后者会让"别人的密钥存在吗"
    /// 从 403 与 404 的差异里读出来。
    pub async fn revoke_api_key_of_account(
        &self,
        account_id: AccountId,
        key_id: Uuid,
        actor: &str,
    ) -> Result<bool, ApplicationError> {
        self.repository
            .revoke_api_key_of_account(account_id, key_id, actor)
            .await
    }

    /// 引导管理员账号：按邮箱**只建不改**。运维在部署时用它设初始邮箱与口令。
    ///
    /// **明文口令进得来、出不去**：只把哈希交给仓储。邮箱或口令不合形状时明确失败，不悄悄建一个
    /// 登不进去的账号。账号已存在时**什么都不做**（连口令都不动）——否则每次重启都会把运维改过的
    /// 口令打回环境变量里的那个。
    pub async fn seed_admin(&self, email: &str, password: &str) -> Result<Uuid, ApplicationError> {
        let email = normalize_email(email)?;
        check_secret(password, "admin password")?;
        let hash = hash_password(password)?;
        let (admin_id, created) = self.repository.ensure_admin_account(&email, &hash).await?;
        if created {
            tracing::info!(%email, %admin_id, "admin account is ready");
        } else {
            tracing::info!(%email, %admin_id, "admin account already exists; the bootstrap left it untouched");
        }
        Ok(admin_id)
    }

    /// 管理员登录：邮箱 + 口令 → 一条会话令牌。
    ///
    /// 邮箱不存在与口令不对**回同一个错误**，而且两条路都走一遍 argon2 校验：分开说（或让快慢不同）
    /// 等于把"这个邮箱是不是管理员"告诉任何来试的人。
    pub async fn login_admin(
        &self,
        email: &str,
        password: &str,
        ttl: ChronoDuration,
    ) -> Result<AdminLogin, ApplicationError> {
        let email = normalize_email(email)?;
        let found = self.repository.find_admin_by_email(&email).await?;
        // 找到账号校验它的哈希，没找到校验对照哈希——**两条路都付一次 argon2**，所以"这个邮箱
        // 是不是我们的账号"不能从响应快慢上看出来。分支收在 `verify_login_secret` 里，这里跳不掉。
        let verified =
            verify_login_secret(found.as_ref().map(|(_, stored)| stored.as_str()), password);
        let Some((admin_id, _)) = found.filter(|_| verified) else {
            return Err(invalid_credentials());
        };
        let token = new_session_token();
        let expires_at = session_expiry(Utc::now(), ttl);
        self.repository
            .create_admin_session(admin_id, &session_token_hash(&token), expires_at)
            .await?;
        self.repository.record_admin_login(admin_id).await?;
        Ok(AdminLogin {
            admin_id,
            email,
            token,
            expires_at,
        })
    }

    /// 用会话令牌认一次管理员：有效则返回 `(admin_id, 邮箱)`。
    ///
    /// 过期会话按"没这条会话"处理——HTTP 层据此回 401，让浏览器重新登录；过期那一行顺手删掉，
    /// 它已经没有任何用处。
    pub async fn authenticate_admin_session(
        &self,
        token: &str,
    ) -> Result<Option<(Uuid, String)>, ApplicationError> {
        let hash = session_token_hash(token);
        let Some((admin_id, email, expires_at)) = self.repository.find_admin_session(&hash).await?
        else {
            return Ok(None);
        };
        if !session_is_valid(expires_at, Utc::now()) {
            // 顺手删掉：留一条已过期的行只会让之后每次请求都白查一次库。
            self.repository.delete_admin_session(&hash).await?;
            return Ok(None);
        }
        Ok(Some((admin_id, email)))
    }

    /// 退出：删掉这条会话。令牌无效/已删也算成功。
    pub async fn logout_admin(&self, token: &str) -> Result<(), ApplicationError> {
        self.repository
            .delete_admin_session(&session_token_hash(token))
            .await
    }

    /// 对客注册：邮箱 + 口令 → 一个新账户与一条会话。
    ///
    /// **账户与身份同一个事务**（仓储那一层保证）：注册出来的账户必须能立刻登录、立刻发 Key，
    /// 不能出现"有账户没身份"或反过来的半截状态。
    pub async fn register_customer(
        &self,
        email: &str,
        password: &str,
        ttl: ChronoDuration,
    ) -> Result<CustomerLogin, ApplicationError> {
        let email = normalize_email(email)?;
        check_secret(password, "password")?;
        let hash = hash_password(password)?;
        let (customer_id, account_id) = self.repository.create_customer(&email, &hash).await?;
        let token = new_session_token();
        let expires_at = session_expiry(Utc::now(), ttl);
        self.repository
            .create_customer_session(customer_id, &session_token_hash(&token), expires_at)
            .await?;
        Ok(CustomerLogin {
            customer_id,
            account_id,
            email,
            token,
            expires_at,
        })
    }

    /// 对客登录：与管理员那条同一条判据（邮箱不存在与口令不对回同一个错误、都算一遍哈希）。
    pub async fn login_customer(
        &self,
        email: &str,
        password: &str,
        ttl: ChronoDuration,
    ) -> Result<CustomerLogin, ApplicationError> {
        let email = normalize_email(email)?;
        let found = self.repository.find_customer_by_email(&email).await?;
        // 同 `login_admin`：账号不存在也付一次 argon2 校验的代价，两条路不能从快慢上分开。
        let verified = verify_login_secret(
            found.as_ref().map(|(_, _, stored)| stored.as_str()),
            password,
        );
        let Some((customer_id, account_id, _)) = found.filter(|_| verified) else {
            return Err(invalid_credentials());
        };
        let token = new_session_token();
        let expires_at = session_expiry(Utc::now(), ttl);
        self.repository
            .create_customer_session(customer_id, &session_token_hash(&token), expires_at)
            .await?;
        self.repository.record_customer_login(customer_id).await?;
        Ok(CustomerLogin {
            customer_id,
            account_id,
            email,
            token,
            expires_at,
        })
    }

    /// 用会话令牌认一次客户：有效则返回 `(customer_id, account_id)`。
    pub async fn authenticate_customer_session(
        &self,
        token: &str,
    ) -> Result<Option<(Uuid, Uuid)>, ApplicationError> {
        let hash = session_token_hash(token);
        let Some((customer_id, account_id, expires_at)) =
            self.repository.find_customer_session(&hash).await?
        else {
            return Ok(None);
        };
        if !session_is_valid(expires_at, Utc::now()) {
            self.repository.delete_customer_session(&hash).await?;
            return Ok(None);
        }
        Ok(Some((customer_id, account_id)))
    }

    /// 对客退出。
    pub async fn logout_customer(&self, token: &str) -> Result<(), ApplicationError> {
        self.repository
            .delete_customer_session(&session_token_hash(token))
            .await
    }

    /// 管理员改自己的口令（需当前口令）。
    ///
    /// 改完**吊销该管理员的全部会话**（Spec A4）：新口令生效而旧凭据还能用，等于没改。
    pub async fn change_admin_password(
        &self,
        admin_id: Uuid,
        current: &str,
        new: &str,
    ) -> Result<(), ApplicationError> {
        let stored = self
            .repository
            .find_admin_password(admin_id)
            .await?
            .ok_or_else(|| ApplicationError::NotFound("admin account".to_owned()))?;
        if !verify_password(current, &stored) {
            return Err(invalid_credentials());
        }
        check_secret(new, "new password")?;
        let hash = hash_password(new)?;
        // 写哈希、吊销该身份全部会话、写审计在**同一个事务**里：只成一件会留下"新口令生效、
        // 旧会话还能用"这类半截状态。
        if !self
            .repository
            .set_admin_password(admin_id, &hash, "admin-self")
            .await?
        {
            return Err(ApplicationError::NotFound("admin account".to_owned()));
        }
        Ok(())
    }

    /// 签发一枚管理员口令重置令牌（运维自救，或另一个管理员代办）。
    ///
    /// 返回 `(admin_id, 明文令牌, 过期时刻)`。明文只这一次；库里只有摘要。
    pub async fn issue_admin_password_reset(
        &self,
        email: &str,
        ttl: ChronoDuration,
    ) -> Result<(Uuid, String, DateTime<Utc>), ApplicationError> {
        let email = normalize_email(email)?;
        let Some((admin_id, _)) = self.repository.find_admin_by_email(&email).await? else {
            return Err(ApplicationError::NotFound("admin account".to_owned()));
        };
        let (token, expires_at) = self.issue_reset_token("admin", admin_id, ttl).await?;
        self.repository
            .record_audit(
                "admin-self",
                "admin.password_reset",
                "admin_user",
                &admin_id.to_string(),
            )
            .await?;
        Ok((admin_id, token, expires_at))
    }

    /// 凭重置令牌设置新口令（不需要旧口令、也不需要会话）。
    ///
    /// 令牌一次性、有独立过期；用过之后该身份全部会话失效。
    pub async fn redeem_password_reset(
        &self,
        token: &str,
        new_password: &str,
    ) -> Result<(), ApplicationError> {
        check_secret(new_password, "new password")?;
        let (kind, subject_id) = self.consume_reset_token(token).await?;
        let hash = hash_password(new_password)?;
        // 令牌已经烧掉了：到这里必须是"口令真的改了"。写不进去说明被重置者不在了，如实报错而不是
        // 回一个成功的空操作——那会让调用方以为能用新口令登录。
        let written = match kind.as_str() {
            "admin" => {
                self.repository
                    .set_admin_password(subject_id, &hash, "password-reset")
                    .await?
            }
            _ => {
                self.repository
                    .set_customer_password(subject_id, &hash, "password-reset")
                    .await?
            }
        };
        if !written {
            return Err(ApplicationError::NotFound(format!(
                "{kind} account of the reset token"
            )));
        }
        Ok(())
    }

    /// 客户改自己的口令（需当前口令）。改完吊销该客户全部会话。
    pub async fn change_customer_password(
        &self,
        customer_id: Uuid,
        current: &str,
        new: &str,
    ) -> Result<(), ApplicationError> {
        let stored = self
            .repository
            .find_customer_password(customer_id)
            .await?
            .ok_or_else(|| ApplicationError::NotFound("customer".to_owned()))?;
        if !verify_password(current, &stored) {
            return Err(invalid_credentials());
        }
        check_secret(new, "new password")?;
        let hash = hash_password(new)?;
        if !self
            .repository
            .set_customer_password(customer_id, &hash, "customer-self")
            .await?
        {
            return Err(ApplicationError::NotFound("customer".to_owned()));
        }
        Ok(())
    }

    /// 运营为某个客户账户签发重置令牌（Spec C12 的管理端一侧）。
    pub async fn issue_customer_password_reset(
        &self,
        account_id: AccountId,
        ttl: ChronoDuration,
    ) -> Result<(Uuid, String, DateTime<Utc>), ApplicationError> {
        let customer_id = self
            .repository
            .find_customer_by_account(account_id.0)
            .await?
            .ok_or_else(|| {
                ApplicationError::NotFound(format!(
                    "account {} has no email login identity",
                    account_id.0
                ))
            })?;
        let (token, expires_at) = self.issue_reset_token("customer", customer_id, ttl).await?;
        self.repository
            .record_audit(
                "admin-self",
                "customer.password_reset",
                "customer",
                &customer_id.to_string(),
            )
            .await?;
        Ok((customer_id, token, expires_at))
    }

    /// 运营替客户开户（Spec C13）：给邮箱配身份，账户可以是新的，也可以是已有的那个。
    ///
    /// `password` 为 `None` 时不设初始口令 —— 运营改用重置令牌让客户自己设（Spec C14）。
    pub async fn open_customer_account(
        &self,
        email: &str,
        password: Option<&str>,
        account_id: Option<AccountId>,
    ) -> Result<CustomerView, ApplicationError> {
        let email = normalize_email(email)?;
        let hash = match password {
            Some(password) => {
                check_secret(password, "initial password")?;
                hash_password(password)?
            }
            // 没有初始口令时也要占住那一列：写一条**永远匹配不上**的口令，等重置令牌换掉它。
            None => hash_password(&new_session_token())?,
        };
        let (customer_id, account_id) = self
            .repository
            .open_customer_account(&email, &hash, account_id.map(|id| id.0))
            .await?;
        self.customer_view(customer_id, account_id, &email).await
    }

    /// 按邮箱找客户账户（Spec M5）：运营为已有账户配身份、给客户充值都要先拿到账户标识。
    pub async fn find_customer(
        &self,
        email: &str,
    ) -> Result<Option<CustomerView>, ApplicationError> {
        let email = normalize_email(email)?;
        self.repository.find_customer_view(&email).await
    }

    /// 列客户。
    pub async fn list_customers(&self, limit: u32) -> Result<Vec<CustomerView>, ApplicationError> {
        self.repository.list_customers(limit).await
    }

    /// 签发一枚重置令牌：作废该身份此前未兑换的那些，再落新的。
    async fn issue_reset_token(
        &self,
        subject_kind: &str,
        subject_id: Uuid,
        ttl: ChronoDuration,
    ) -> Result<(String, DateTime<Utc>), ApplicationError> {
        let token = new_session_token();
        let expires_at = session_expiry(Utc::now(), ttl);
        self.repository
            .create_password_reset(
                subject_kind,
                subject_id,
                &session_token_hash(&token),
                expires_at,
            )
            .await?;
        Ok((token, expires_at))
    }

    /// 兑换一枚重置令牌：判过期、判用过，然后标记为已用。返回 `(身份域, 被重置者 id)`。
    async fn consume_reset_token(&self, token: &str) -> Result<(String, Uuid), ApplicationError> {
        let hash = session_token_hash(token);
        let Some((kind, subject_id, expires_at, redeemed_at)) =
            self.repository.find_password_reset(&hash).await?
        else {
            return Err(invalid_credentials());
        };
        if redeemed_at.is_some() || expires_at <= Utc::now() {
            return Err(invalid_credentials());
        }
        if !self.repository.redeem_password_reset(&hash).await? {
            // 竞态：两个请求同时兑换，另一个先标记成功。一次性就是一次性。
            return Err(invalid_credentials());
        }
        Ok((kind, subject_id))
    }

    /// 拼一条管理端视图（开户之后要把它回给运营）。
    async fn customer_view(
        &self,
        customer_id: Uuid,
        account_id: Uuid,
        email: &str,
    ) -> Result<CustomerView, ApplicationError> {
        match self.repository.find_customer_view(email).await? {
            Some(view) => Ok(view),
            // 刚写完就该读到；读不到说明落库与读的口径不一致，如实报错而不是编一条。
            None => Err(ApplicationError::Persistence(format!(
                "customer {customer_id} for account {account_id} was written but cannot be read back"
            ))),
        }
    }

    /// 认证：把明文密钥哈希之后**每次**读库换账户，吊销判定就在那条读里；读到了就为这把密钥占一个
    /// 当前窗口的速率名额。
    ///
    /// 刻意**不**缓存"这把密钥还有效吗"：吊销的语义是"立刻停止使用"，任何缓存都会让吊销在 TTL
    /// 内不生效——而吊销恰恰是那种"多延迟一秒都在放行不该放行的请求"的动作。这里一次唯一索引点查
    /// 很便宜，用它换"吊销即生效"是划算的。
    ///
    /// 速率判定放在这里、而不是生成流程里面：它只依赖"哪把密钥"，是入口就能判的事；放进生成流程
    /// 等于让每个入口各自记得调用一次。被吊销的密钥读不到账户，因此也**不占**速率名额。
    pub async fn authenticate(&self, plaintext: &str) -> Result<ApiKeyIdentity, ApplicationError> {
        if !plaintext.starts_with("sk_seeai_") {
            return Err(ApplicationError::NotFound("api key".to_owned()));
        }
        let (key_id, account_id) = self
            .repository
            .api_key_identity(&sha256_hex(plaintext.as_bytes()))
            .await?;
        // 窗口按进程时钟取：窗口只是"多久算一轮"，时钟漂移只会让某一轮稍长或稍短，不会让计数
        // 跑到别的键或别的窗口上去；为它多打一次数据库不值。
        self.acceleration
            .consume_request_slot(key_id, self.rate_limit, Utc::now())
            .await?;
        Ok(ApiKeyIdentity { account_id, key_id })
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

    /// 列出账户（按创建时间倒序，可按邮箱或标签收窄）。
    ///
    /// 这是"先找到再操作"的入口：没有它，运营必须已经知道账户标识才能做事。只读、不读缓存——
    /// 列表里的余额要能与详情里的余额对得上。
    pub async fn list_accounts(
        &self,
        email: Option<&str>,
        tag: Option<&str>,
        limit: u32,
    ) -> Result<Vec<AccountSummary>, ApplicationError> {
        self.repository.list_accounts(email, tag, limit).await
    }

    /// 读账户**账目流水**（权威在账本；管理员面与对客面共用这条读）。
    ///
    /// 与 [`Self::read_balance`] 同一条口径：走仓库、不读缓存——流水的用途也是查看与核对。
    /// 它**只读**：不改状态，也不写审计。`until` 是闭区间上界，`offset` 供翻页。
    pub async fn read_entries(
        &self,
        account_id: AccountId,
        since: Option<DateTime<Utc>>,
        until: Option<DateTime<Utc>>,
        kind: Option<&str>,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<LedgerEntry>, ApplicationError> {
        self.repository
            .read_ledger_entries(account_id, since, until, kind, offset, limit)
            .await
    }

    /// 读账户在 `[since, until]` 内的流水总条数，与 [`Self::read_entries`] 同一套区间语义。
    pub async fn count_entries(
        &self,
        account_id: AccountId,
        since: Option<DateTime<Utc>>,
        until: Option<DateTime<Utc>>,
        kind: Option<&str>,
    ) -> Result<u64, ApplicationError> {
        self.repository
            .count_ledger_entries(account_id, since, until, kind)
            .await
    }

    /// 读账户**持有中**的金额（权威在库）。
    ///
    /// 它与余额是**两个数**，调用方不合并：
    ///
    /// * 余额是**可用额**——已经真的从账上扣掉的部分；
    /// * 持有中是**已预授权、还没结算**的部分——这笔钱**还没有被扣**，预授权只是先把钱占住。
    ///
    /// 合成一个"总资产"会让"这笔钱到底扣没扣"说不清，而这两条读的用途正是让人看清这件事。
    pub async fn read_held(&self, account_id: AccountId) -> Result<i64, ApplicationError> {
        self.repository.held_microusd(account_id).await
    }

    /// 对客用量：`[since, until)` 里的每一次生成请求（对客投影，不含 Job 标识与内部状态）。
    pub async fn customer_usage(
        &self,
        account_id: AccountId,
        query: CustomerBillingQuery,
    ) -> Result<Vec<CustomerUsageView>, ApplicationError> {
        self.repository.customer_usage(account_id, query).await
    }

    /// 对客账单汇总：同一区间**全量**的请求数、产出张数与扣费总额（不随明细条数上限变化）。
    pub async fn customer_billing(
        &self,
        account_id: AccountId,
        query: CustomerBillingQuery,
    ) -> Result<CustomerBillingSummary, ApplicationError> {
        self.repository.customer_billing(account_id, query).await
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
/// 新鲜窗口必须**显著小于**对账周期：对账写回的条目来源标记是 `reconciler`、本来就**不作为**
/// 新鲜提示，这条比例关系是第二道保险——它保证"能用作提示的值"实际都来自写穿路径。
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
    /// 已结算余额（可以为负：透支发生在结算）。
    pub balance_microusd: i64,
    /// 占用合计：active 预授权之和。
    pub held_microusd: i64,
    /// 可用额 = 已结算余额 − 占用合计（同一时点、同一条语句读出来）。
    pub available_microusd: i64,
    /// 账户金额的单调递增版本：供缓存拒绝倒序写回（`0013` §3）。
    pub version: i64,
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

/// 余额缓存条目的来源：写穿路径写下的值**可以**用作新鲜提示，对账写回的不行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BalanceSource {
    /// 数据库事务提交后由写穿路径写下（充值、受理预授权扣减、结算、失败释放、对账退款）。
    DbCommit,
    /// 定时对账写回的副本：它只保证"与数据库一致"，不是"刚有一笔钱变动过"的证据。
    Reconciler,
}

/// 一次候选取数的结果：候选集，外加"这次复核到的可用供给集"。
///
/// 复核只发生在缓存给出的候选集上：回源数据库那一支的取数 SQL 已经判过开关，复核结果因此是
/// `None`。为什么缓存那一支必须复核，见 [`HubRepository::enabled_offerings`]。
///
/// 复核结果**不写回缓存**：缓存里的值始终是那一版发布的候选集，写回被裁过的集合会让"重新启用"
/// 在一个 TTL 内看不见。
#[derive(Debug, Clone)]
pub struct CandidateSet {
    /// 候选，`routing_priority` 升序。
    pub candidates: Vec<OfferingCandidate>,
    /// `Some`：只有出现在集合里的供给才合格。`None`：这份候选集刚回源读来，不需要复核。
    pub enabled_offerings: Option<HashSet<OfferingId>>,
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

/// 余额缓存的值：一次账户读取的完整快照 + 写入时间（数据库盖章）+ 来源标记。
///
/// 三个金额与版本同属一次读取（见 [`HubRepository::read_account_balance`]），因此
/// `available = balance − held` 在缓存里同样成立；`version` 是倒序写回闸门的判据
/// （[`AccelerationService::write_balance`]）。
#[derive(Debug, Serialize, Deserialize)]
struct CachedBalance {
    balance_microusd: i64,
    held_microusd: i64,
    available_microusd: i64,
    version: i64,
    written_at: DateTime<Utc>,
    source: BalanceSource,
}

impl CachedBalance {
    /// 这条值能不能作为**新鲜提示**。两条判据都要满足：
    ///
    /// 1. 来源是**写穿路径**——对账写回的只是"与数据库一致"的副本，不构成"刚有一笔钱变动过"；
    /// 2. 写入时间落在新鲜窗口内，且**不晚于数据库当前时刻**。晚于它只可能是两个时钟不同步，
    ///    那种值一律当不新鲜：不拿一个来路不明的时间去提示。
    ///
    /// 它只影响预检日志，**不决定受理**：402 一律由数据库条件更新确认
    /// （[`AccelerationService::precheck_balance`]）。
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

/// 速率计数缓存的值：**哪个窗口**、这个窗口里已经数到几。
///
/// 窗口标识一起存进去，是因为键的存活时间由缓存自己管、与窗口边界只是"差不多同时"：缓存里可能
/// 留着上一个窗口的键（时钟有偏差、写入失败过）。比对窗口标识把这种残留判成"这个窗口从零开始"，
/// 而不是把上个窗口的次数接着往下数——计数偏高会拒掉本来合规的请求。
#[derive(Debug, Serialize, Deserialize)]
struct CachedRateLimit {
    window: i64,
    count: u64,
}

/// 加速层：把"哪些东西可以缓、值长什么样、什么时候能拿它下结论"收在一处。
///
/// 三条不变量，改这个类型时必须一起守住：
///
/// 1. **任何一次缓存操作失败都只是"这次没命中"**——回源数据库，绝不让请求因为缓存出问题而失败；
/// 2. **扣减与余额事实只在数据库事务里发生**：这里的写入一律发生在提交**之后**、写的是提交后的
///    值，从不用 `DECRBY` 之类的增量命令（增量表达不了"以数据库为准"，重放还会漂移）；
/// 3. **缓存从不决定资金结果**：预检只读新鲜值作提示，受理与 402 一律由数据库条件更新判定。
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

    /// 速率计数的键：**密钥标识 + 窗口序号**。窗口写进键里，跨窗口因此天然是一份新计数，
    /// 不依赖上一次写入的存活时间算得准。
    fn rate_limit_key(key_id: Uuid, window: i64) -> String {
        format!("rate_limit:{key_id}:{window}")
    }

    /// 为这把密钥占用当前窗口的一个名额。返回 `Err` 表示这个窗口已经用满，并带上"多久之后
    /// 可以再来"。
    ///
    /// 计数落在**缓存**里：限流是保护机制、不是业务事实，写库会把每个请求变成一次写——而限流要挡
    /// 的恰恰是"请求很多"这种形态，用它自己把数据库写满是最糟的失败方式。
    ///
    /// **缓存不可用时放行。** 限流是保护，不是准入：读不到计数就当作"这个窗口还没数过"，把请求
    /// 放过去。反过来（读不到就拒）会让加速层的一次降级直接把全部请求拒掉——一次降级放大成一次
    /// 故障。同理，写入失败只记日志：那一笔少数的请求会过去，下一个请求接着读不到、接着放行，
    /// 直到缓存回来为止；宁可少挡几次，也不要拒掉合规的调用方。
    ///
    /// 计数**不是严格原子**的：`CacheStore` 只有读写两条命令（没有 `INCR`），并发请求可能读到同
    /// 一个数再各自写回，于是这个窗口里实际能过去的请求可能比上限多几个。这是刻意的取舍——限流
    /// 要挡的是数量级上的异常（脚本没退避），不是精确的第 61 次；为此把缓存接口撑大、让实现方也懂
    /// "计数"反而会把两边的语义各自漂移（见 [`CacheStore`]）。
    ///
    /// `now` 由调用方给：窗口只是"多久算一轮"，时钟漂移的唯一后果是某一轮稍长或稍短，不改变
    /// "计数在缓存、缓存挂了就放行"这两条。
    pub async fn consume_request_slot(
        &self,
        key_id: Uuid,
        limit: GenerationRateLimit,
        now: DateTime<Utc>,
    ) -> Result<(), ApplicationError> {
        let window = self.rate_limit_window(limit.window, now);
        let current = match self.read_rate_limit(key_id, window).await {
            Some(cached) => cached,
            // 读不到（缓存不可用、或这个窗口还没有计数）就当从零开始：放行。
            None => CachedRateLimit { window, count: 0 },
        };
        let count = current.count.saturating_add(1);
        self.write(
            &Self::rate_limit_key(key_id, window),
            &json!({ "window": window, "count": count }).to_string(),
            self.rate_limit_ttl(limit.window, now),
        )
        .await;
        if count > limit.max_requests {
            return Err(ApplicationError::RateLimitExceeded {
                retry_after: self.rate_limit_retry_after(limit.window, now),
            });
        }
        Ok(())
    }

    /// 这次请求落在第几个窗口。窗口按钟点等分，序号本身没有含义，只用来判断"是不是同一个窗口"。
    fn rate_limit_window(&self, window: Duration, now: DateTime<Utc>) -> i64 {
        let millis = u64::try_from(window.as_millis()).unwrap_or(u64::MAX).max(1);
        let now_millis = now.timestamp_millis().max(0) as u64;
        (now_millis / millis) as i64
    }

    /// 这条计数该活多久：本窗口剩下的时间。窗口一过它就该消失——留着只会让下一个窗口的第一次
    /// 写入多一次"读到旧值"的机会。至少 1 毫秒：存活时间为 0 会被缓存当成非法命令。
    fn rate_limit_ttl(&self, window: Duration, now: DateTime<Utc>) -> Duration {
        let remaining = self.window_remaining(window, now);
        remaining.max(Duration::from_millis(1))
    }

    /// 到下一个窗口还有多久（对客的 `Retry-After`）。
    fn rate_limit_retry_after(&self, window: Duration, now: DateTime<Utc>) -> Duration {
        // 据实取值，不四舍五入到窗口长度：窗口只剩 2 秒时告诉调用方"等 2 秒"，它才真的能接着用。
        self.window_remaining(window, now)
            .max(Duration::from_millis(1))
    }

    fn window_remaining(&self, window: Duration, now: DateTime<Utc>) -> Duration {
        let millis = u64::try_from(window.as_millis()).unwrap_or(u64::MAX).max(1);
        let now_millis = now.timestamp_millis().max(0) as u64;
        Duration::from_millis(millis - (now_millis % millis))
    }

    async fn read_rate_limit(&self, key_id: Uuid, window: i64) -> Option<CachedRateLimit> {
        let key = Self::rate_limit_key(key_id, window);
        let raw = self.read(&key).await?;
        match serde_json::from_str::<CachedRateLimit>(&raw) {
            Ok(cached) if cached.window == window => Some(cached),
            // 值读不出来、或它是**别的窗口**留下的：当这个窗口还没数过。计数偏高会拒掉合规的
            // 请求，偏低只是少挡几次——两种错里只有前者会伤到调用方。
            Ok(_) => None,
            Err(error) => {
                tracing::warn!(
                    key,
                    error = %error,
                    "the cached rate limit counter is unreadable; counting this window from zero"
                );
                None
            }
        }
    }

    /// 取该网关模型的候选集：缓存命中且**修订标识一致**才用缓存，否则回源数据库并重建缓存。
    ///
    /// 修订标识由受理前那次 [`AcceptanceProbe`] 读来，这里不再查库。不一致（或值里根本没有这个
    /// 标识、值读不出来）⇒ 当未命中。因此 route 缓存的陈旧是**可检的**，不依赖"发布后的失效
    /// 一定成功"。
    ///
    /// 关掉（或从未发布）的模型**不看缓存**：开关与启停都是可变表里的事、不改变修订标识，缓存里
    /// 那份候选集在关掉或停用之后仍然"看起来是新的"（口径见 [`HubRepository::enabled_offerings`]）。
    ///
    /// 命中缓存时按主键复核一遍供给与渠道的启用状态（判据见 [`HubRepository::enabled_offerings`]），
    /// 复核结果随候选集一起交出；回源那一支取数已经判过开关，不需要复核，因此复核结果只在缓存给出
    /// 的候选集上出现（见 [`CandidateSet`]）。
    pub async fn candidates(
        &self,
        gateway_model: &str,
        probe: &AcceptanceProbe,
    ) -> Result<CandidateSet, ApplicationError> {
        let Some(effective) = probe.effective_revision_id.filter(|_| probe.enabled) else {
            return Ok(CandidateSet {
                candidates: Vec::new(),
                enabled_offerings: None,
            });
        };
        if let Some(cached) = self.read_route(gateway_model).await
            && cached.runtime_revision_id == effective
        {
            let offering_ids = cached
                .candidates
                .iter()
                .map(|candidate| candidate.offering_id)
                .collect::<Vec<_>>();
            let enabled_offerings = self.repository.enabled_offerings(&offering_ids).await?;
            return Ok(CandidateSet {
                candidates: cached.candidates,
                enabled_offerings: Some(enabled_offerings),
            });
        }
        let candidates = self.repository.active_offering(gateway_model).await?;
        // **空候选集不入缓存**：它是"这个型号现在调不动"的瞬时状态，而回源它只发生在错误路径上。
        // 缓存下来反而会让"供给被重新启用"（今天没有写入方，但将来会有）在一个 TTL 内看不见。
        if !candidates.is_empty() {
            self.write_route(gateway_model, effective, &candidates)
                .await;
        }
        Ok(CandidateSet {
            candidates,
            enabled_offerings: None,
        })
    }

    /// 受理前的余额预检：读缓存里的**可用额**作提示，**不决定受理**。
    ///
    /// 缓存不可用、值过期、不是写穿来源或这次是重放时都不提示。命中一条新鲜且可用额低于本次
    /// 保底额的值时只记一条日志——它说明"这次很可能受理不了"，但**不产生 402**：受理闸门是
    /// 数据库那条条件更新（`balance − held ≥ 保底额` 且 `kind = consumer`），缓存不足也要由它
    /// 确认才回 402（`0013` §3）。重放会去重成原来那个 Job，不新建也不扣款，提示对它是噪音。
    pub async fn precheck_balance(
        &self,
        account_id: AccountId,
        hold_microusd: u64,
        gateway_model: &str,
        probe: &AcceptanceProbe,
    ) {
        if probe.replay {
            return;
        }
        let Some(cached) = self.read_balance(account_id).await else {
            return;
        };
        if !cached.is_fresh(probe.database_now, self.policy.freshness_window) {
            return;
        }
        let Ok(hold) = i64::try_from(hold_microusd) else {
            return;
        };
        if cached.available_microusd >= hold {
            return;
        }
        tracing::warn!(
            account_id = %account_id,
            gateway_model,
            cached_available_microusd = cached.available_microusd,
            cached_version = cached.version,
            hold_microusd,
            "the fresh cached available amount is below the hold; the database conditional update decides"
        );
    }

    /// 把**数据库提交后**的余额快照写进缓存（写穿），并拒绝倒序写回。
    ///
    /// 写的是提交后的值而不是增量：`DECRBY` 表达不了"以数据库为准"，重放还会漂移。写入时间用
    /// 数据库给出的 `updated_at`，于是"新鲜"提示与日志里的时间都是数据库的时间。
    ///
    /// **版本闸门**：只有版本**不低于**缓存当前值的快照才写。数据库对同一账户的金额更新是串行的，
    /// 版本随每次变更递增；两个事务提交后的异步写回若乱序到达，旧快照会试图覆盖新快照——这里按
    /// 版本挡掉它（`0013` §3）。读不到当前值（键不存在、缓存不可用或值不可读）时照写：数据库是
    /// 权威，覆盖一个读不出来的值是恢复而不是降级；写失败只记日志，不回滚数据库。
    pub async fn write_balance(&self, change: &BalanceChange, source: BalanceSource) {
        if !self.is_enabled() {
            return;
        }
        if let Some(current) = self.read_balance(change.account_id).await
            && current.version > change.version
        {
            tracing::warn!(
                account_id = %change.account_id,
                cached_version = current.version,
                incoming_version = change.version,
                "skipped a cache write-back older than the cached snapshot"
            );
            return;
        }
        let value = CachedBalance {
            balance_microusd: change.balance_microusd,
            held_microusd: change.held_microusd,
            available_microusd: change.available_microusd,
            version: change.version,
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
    /// 失效失败不影响正确性：旧值带着旧修订标识，受理时的比对必然不一致 ⇒ 回源数据库；启停那一项
    /// 由受理路径的复核兜住（见 [`HubRepository::enabled_offerings`]），失效只影响命中率。失败只记
    /// 一条日志（运营要能发现命中率在下降）。
    pub async fn invalidate_route(&self, gateway_model: &str) {
        let Some(cache) = self.cache.as_ref() else {
            return;
        };
        let key = Self::route_key(gateway_model);
        if let Err(error) = cache.delete(&key).await {
            tracing::warn!(
                key,
                error = %error,
                "cache invalidation failed; a stale entry is still caught at acceptance time"
            );
        }
    }

    /// 定时对账兜底：以数据库为准把缓存覆盖回去，并把**真的不一致**记下来。
    ///
    /// 只覆盖不一致的条目：已经等于数据库值的条目不动——重写会把它的来源降级成 `reconciler`
    /// （等于这条值不再能作为新鲜提示），没必要为一次没发生的不一致付这个代价。缓存比这次读到的
    /// 数据库行**新**时也不动它：那次读发生在新提交之前，覆盖回去才是倒序写回。
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
            if let Some(cached) = &cached {
                // 缓存比这次读到的数据库行**新**：两次读取之间又提交了一笔，这一份是旧读数，
                // 不动它，等下一轮对账带着更新的行再来。
                if cached.version > change.version {
                    continue;
                }
                // 版本与三个金额都相同才是同一个快照；版本相同而金额不同只可能是缓存被改坏。
                if cached.version == change.version
                    && cached.balance_microusd == change.balance_microusd
                    && cached.held_microusd == change.held_microusd
                    && cached.available_microusd == change.available_microusd
                {
                    continue;
                }
                report.balances_corrected += 1;
                tracing::warn!(
                    account_id = %change.account_id,
                    cached_balance_microusd = cached.balance_microusd,
                    cached_held_microusd = cached.held_microusd,
                    cached_version = cached.version,
                    database_balance_microusd = change.balance_microusd,
                    database_version = change.version,
                    "the cached balance disagreed with the database; overwriting it"
                );
                self.record_reconcile_correction(
                    "cache.balance_corrected",
                    "account",
                    &change.account_id.to_string(),
                    json!({
                        "cached_balance_microusd": cached.balance_microusd,
                        "cached_held_microusd": cached.held_microusd,
                        "cached_available_microusd": cached.available_microusd,
                        "cached_version": cached.version,
                        "cached_written_at": cached.written_at,
                        "cached_source": cached.source,
                        "database_balance_microusd": change.balance_microusd,
                        "database_held_microusd": change.held_microusd,
                        "database_available_microusd": change.available_microusd,
                        "database_version": change.version,
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
                    "the cached balance is unreadable; treating it as a cache miss"
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
    /// 成本护栏：单次请求可能花掉的上游成本上限（运营取值，与受理侧**同一个数**）。
    cost_ceiling: RequestCostCeiling,
}

impl RuntimeService {
    #[must_use]
    pub fn new(repository: Arc<dyn HubRepository>, adapters: Arc<dyn AdapterFactory>) -> Self {
        let acceleration = Arc::new(AccelerationService::disabled(repository.clone()));
        Self {
            repository,
            adapters,
            acceleration,
            cost_ceiling: RequestCostCeiling::default_ceiling(),
        }
    }

    /// 装上运维给的**单次请求成本上限**。
    ///
    /// 上限是配置项：它随部署形态与上游价格变，所以由调用方给，而不是写死在这里。
    #[must_use]
    pub fn with_cost_ceiling(mut self, ceiling: RequestCostCeiling) -> Self {
        self.cost_ceiling = ceiling;
        self
    }

    /// 装上加速层：发布与启停都要失效该型号的 route 缓存。
    #[must_use]
    pub fn with_acceleration(mut self, acceleration: Arc<AccelerationService>) -> Self {
        self.acceleration = acceleration;
        self
    }

    /// 发布一个 Vendor Model 的供给（完整候选集合）。
    ///
    /// 顺序：**补齐可省略的渠道字段**（见下）→ 形状归一到 [`NormalizedPublication`] → 命令级字段校验 →
    /// **合同**校验 → **逐候选**校验（承载面落在合同与 Driver 之内、base_url、计价、Adapter 兼容性）→
    /// 交给仓库逐项写入。校验不通过时不产生任何 revision 行。
    ///
    /// **增量发布**（`docs/design/0010` §4.1）：候选的渠道三要素与驱动器可以整组省略，从该型号当前生效
    /// 的修订按 `provider_kind` + `provider_model_id` 复用同一条候选的值。读上一版与写新修订是两次仓库
    /// 调用，但它们之间不会插进另一次发布——发布事务按网关模型取排他锁，同一型号的发布是串行的。
    pub async fn publish(
        &self,
        mut command: PublishRuntimeCommand,
    ) -> Result<PublishedRevision, ApplicationError> {
        // 引用式发布的技术定义由仓储从被引用的 Offering 行取；内联那条老路用请求里的值。
        let definitions_from_offerings = command.references.is_some();
        if command.actor.trim().is_empty() {
            return Err(ApplicationError::Validation(
                "actor must not be empty".to_owned(),
            ));
        }
        // 身份三件（厂商、原生名、修订）的校验在 `normalize` 里：引用式发布的三件由被引用的 Offering
        // 决定，**调用方给的那个 `command` 上本来就没有它们**，所以这里不能先查 `command`——那条路会被
        // 自己拒掉。解析把它算出来，下面用**算出来的**那一个。
        let (publication, native_model_id) = self.resolve_inheritance(&mut command).await?;
        let NormalizedPublication {
            contract,
            offerings,
        } = publication;
        validate_contract(&native_model_id, &contract)?;
        let mut normalized = Vec::with_capacity(offerings.len());
        for offering in offerings {
            normalized.push(self.validate_offering(&contract, offering)?);
        }
        validate_supply_identities(&normalized)?;
        self.validate_cost_ceiling(&native_model_id, &contract, &normalized)
            .await?;
        let request = command.into_request(contract, normalized, definitions_from_offerings);
        validate_gateway_model_identity(&request)?;
        let gateway_model = request.gateway_model.clone();
        let revision = self.repository.publish_runtime(request).await?;
        // 发布已经提交：这时才失效缓存。失效失败不影响正确性——旧值带着旧修订标识，
        // 受理时的比对必然不一致（见 `AccelerationService::candidates`）。
        self.acceleration.invalidate_route(&gateway_model).await;
        Ok(revision)
    }

    /// 把**引用式**候选解析成一次发布：技术定义与合同都从被引用的 Offering 取。
    ///
    /// 这是运营那条路（`docs/design/0012-platform-model-publishing.md` §4）：他给的是"选中的 Offering +
    /// 这条候选的价"，驱动器、供应商模型名、渠道三要素、承载面、参数映射、限制与合同一律由服务端从库里
    /// 取——所以**引用一条不存在的 Offering 要在发布期拒绝并点它的标识**，不能默默少一条候选：少一条
    /// 就意味着这次发布出来的模型少一条路，而那正是运营以为自己选上的那条。
    ///
    /// 同样拒绝**已停用/不可用**的那条（判据见 [`HubRepository::enabled_offerings`]，与受理期同一处）：
    /// 受理期会把停用候选筛掉，放过它等于发出去一个当场就有一条路走不通的模型；要复现一条曾被停用的
    /// 候选，该由运营先把供给启用回来。
    ///
    /// 契约只在"这次发布第一次引用这个厂商模型"时用到：同一个厂商模型的合同是同一份（模型级唯一）。
    /// 引用必须落在同一个厂商模型上，否则拒绝——一个网关模型在一个时刻只属于一个厂商（`0012` §2.3）。
    async fn resolve_referenced_offerings(
        &self,
        command: &mut PublishRuntimeCommand,
        references: &[OfferingReference],
    ) -> Result<(NormalizedPublication, String), ApplicationError> {
        if references.is_empty() {
            return Err(ApplicationError::Validation(
                "references must not be empty: a gateway model needs at least one offering"
                    .to_owned(),
            ));
        }
        if command.offerings.is_some() {
            return Err(ApplicationError::Validation(
                "offerings and references are mutually exclusive: give the referenced offerings, \
                 not the inline technical definitions"
                    .to_owned(),
            ));
        }
        let ids: Vec<OfferingId> = references.iter().map(|item| item.offering_id).collect();
        let resolved = self.repository.offerings_by_id(&ids).await?;
        // **可用性复核**：取到行不等于能选。判据与受理期同一处（供给自己启用、且它所属渠道启用，
        // 见 [`HubRepository::enabled_offerings`]）——发布出来的候选必须真的能受理，否则"发布成功、
        // 一条路都走不通"是个要查半天的状态，而引用式发布是最好的拒绝时机：那一刻运营正指着这条供给。
        //
        // 要复现一条**曾被停用**的候选，先把供给启用回来再发：停用是运营设的运行状态，不该由发布
        // 替他悄悄翻回去。所以这里不把 `enabled` 塞进草稿（`0012` §5：技术定义进快照，两个开关不进），
        // 只当场拒绝。
        let enabled = self.repository.enabled_offerings(&ids).await?;
        let drafts = references
            .iter()
            .map(|reference| {
                let found = resolved
                    .iter()
                    .find(|row| row.offering_id == reference.offering_id)
                    .ok_or_else(|| {
                        ApplicationError::Validation(format!(
                            "offering {} does not exist: pick it from the selectable offering list",
                            reference.offering_id.0
                        ))
                    })?;
                // 顺序是刻意的：先报**取不到**（不存在），再报**取到了但不可用**。停用不是不存在，
                // 运营该做的事也不同——去把哪一条启用回来，而不是重选一条。
                if !enabled.contains(&reference.offering_id) {
                    return Err(ApplicationError::Validation(format!(
                        "offering {} is disabled or unavailable: enable this supply (and its \
                         channel) again before publishing it",
                        reference.offering_id.0
                    )));
                }
                Ok((reference, found))
            })
            .collect::<Result<Vec<_>, ApplicationError>>()?;
        // 一次发布只属于一个厂商模型：同一个网关模型在一次发布里跨厂商会让"它按谁的语义调用"没有答案。
        let first = drafts[0].1;
        for (_, found) in &drafts {
            if found.native_model_id != first.native_model_id
                || found.vendor_id != first.vendor_id
                || found.native_revision != first.native_revision
            {
                return Err(ApplicationError::Validation(
                    "referenced offerings belong to different vendor models: a gateway model \
                     points at one vendor model revision at a time"
                        .to_owned(),
                ));
            }
        }
        let offerings = drafts
            .iter()
            .map(|(reference, found)| OfferingDraft {
                // **选中的是哪一行**由引用自己带着：按身份四元组反查在同一渠道下多行供给时会挑错。
                offering_id: Some(reference.offering_id),
                // 技术定义原样取自被引用的 Offering 行：引用式发布里运营**不给**技术字段，而发布期的
                // 校验（承载面 ⊆ 合同、adapter 兼容、计价形态与参数配套）与老形状走的是同一条路——
                // 所以要把整份定义填回来，不能留空。留空会让引用形态被自己的校验拒掉。
                provider_kind: Some(found.provider_kind.clone()),
                adapter_key: Some(found.adapter_key.clone()),
                provider_model_id: found.provider_model_id.clone(),
                base_url: Some(found.base_url.clone()),
                credential_env: Some(found.credential_env.clone()),
                routing_priority: reference.routing_priority,
                weight: reference.weight,
                restrictions: found.restrictions.clone(),
                carrier_schema: Some(found.carrier_schema.clone()),
                parameter_mapping: found.parameter_mapping.clone(),
                capability_schema: None,
                formula: Some(found.formula.clone()),
                // 渠道费率也取自那一行：它是**渠道怎么结算**的事实，不是运营这次要改的东西。
                price_plan: found.plan.as_ref().map(|plan| PricePlanDraft {
                    currency: plan.currency.clone(),
                    text_input_microusd_per_million: plan.text_input_microusd_per_million,
                    image_input_microusd_per_million: plan.image_input_microusd_per_million,
                    text_output_microusd_per_million: plan.text_output_microusd_per_million,
                    image_output_microusd_per_million: plan.image_output_microusd_per_million,
                    source_url: plan.source_url.clone(),
                }),
                // 按张 / 按次的成本单价与成本币种同样是渠道事实，缺省取行上的值：运营只决定对客卖多少。
                cost_unit_price_microusd: found.cost_unit_price_microusd,
                cost_currency: reference
                    .cost_currency
                    .clone()
                    .or_else(|| found.cost_currency.clone())
                    .or_else(|| found.plan.as_ref().map(|plan| plan.currency.clone())),
                reference_cost_microusd: reference.reference_cost_microusd,
                consumer_rates_cny: reference.consumer_rates_cny.clone(),
                consumer_formula: reference.consumer_formula.clone(),
                // **成本口径与保底表也由服务端定**，不要运营给：这两样是渠道与结算的事实，而且
                // 它们不是"运营的选择"——`cost_basis` 两态由计价形态唯一决定（渠道终态给金额就是
                // `declared`，否则平台按用量自算就是 `computed`）；保底表缺省是空表（没声明保底）。
                //
                // 为什么不能留给"没给就报错"：运营给参考成本只是给一个**定价参考**，而报错会把他挡在
                // 门外去猜一个他无从知道的枚举值——那正是这次改动要收掉的东西。
                cost_basis: Some(
                    if found.formula == "upstream_declared" {
                        "declared"
                    } else {
                        "computed"
                    }
                    .to_owned(),
                ),
                tier_prices: reference.tier_prices.clone(),
                floor_amounts: Some(
                    reference
                        .floor_amounts
                        .clone()
                        .unwrap_or_else(|| Value::Object(serde_json::Map::new())),
                ),
            })
            .collect::<Vec<_>>();
        // **回写到调用方的命令上**：身份是这次发布真正定义的东西，而 `publish` 随后要拿 `command`
        // 去 `into_request` 产出请求。只在这里构造一个临时命令的话，落库的那份身份会是空的
        // （`None` → 空串），于是会建出一行 vendor_id / native_model_id / native_revision 全空的
        // 厂商模型——它连着一次看起来成功的发布，而谁都查不出这次发布的是哪个模型。
        command.vendor_id = Some(first.vendor_id.clone());
        command.native_model_id = Some(first.native_model_id.clone());
        command.native_revision = Some(first.native_revision.clone());
        command.capability_schema = Some(first.capability_schema.clone());
        PublishRuntimeCommand {
            vendor_id: Some(first.vendor_id.clone()),
            native_model_id: Some(first.native_model_id.clone()),
            gateway_model: command.gateway_model.clone(),
            native_revision: Some(first.native_revision.clone()),
            capability_schema: Some(first.capability_schema.clone()),
            offerings: Some(offerings),
            references: None,
            markup_bps: command.markup_bps,
            actor: command.actor.clone(),
        }
        .normalize()
        .map(|publication| (publication, first.native_model_id.clone()))
    }

    /// 把命令里可省略的渠道字段补齐，再归一成一份合同与一个有序候选列表。
    ///
    /// 省略的判定在 [`command_omits_channel`] 的文档里：**渠道三要素与驱动器要么整组给、要么整组省**。
    /// 补的来源是该型号当前生效的修订，按 `provider_kind` + `provider_model_id` 认同一条候选；同一
    /// `provider_kind` 在该型号下有多条候选时拒绝——不选"最便宜"或"下标最近"的那条顶替。
    async fn resolve_inheritance(
        &self,
        command: &mut PublishRuntimeCommand,
    ) -> Result<(NormalizedPublication, String), ApplicationError> {
        // 引用先**取出来**再解析：解析要把算出的身份回写到 `command` 上，而借用中的 `command.references`
        // 与那次可变借用冲突。`Vec` 的克隆成本在一次发布面前可以忽略，换来的是"解析只写一个地方"。
        if let Some(references) = command.references.clone() {
            return self
                .resolve_referenced_offerings(command, &references)
                .await;
        }
        // 内联那条老路：身份由调用方给，`normalize` 会拒掉空的那种。
        let native_model_id = command.native_model_id.clone().unwrap_or_default();
        // 渠道三要素与驱动器可整组省略（改价那条路）：省略时从当前生效修订按身份沿用。一条都没省略
        // 就不必读上一版。
        let drafts = command.offerings.as_deref().ok_or_else(|| {
            ApplicationError::Validation(
                "offerings is required: publish the model's complete, ordered offering list"
                    .to_owned(),
            )
        })?;
        if !drafts
            .iter()
            .any(|draft| command_omits_channel(draft) || command_omits_pricing(draft))
        {
            // 没有一条省略：不必读上一版，走原路径。
            let publication = command.normalize()?;
            return Ok((publication, native_model_id));
        }
        let gateway_model = command
            .gateway_model
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(native_model_id.as_str());
        let previous = self
            .repository
            .active_offering_channels(gateway_model)
            .await?;
        let completed = drafts
            .iter()
            .enumerate()
            .map(|(index, draft)| inherit_offering(index, draft, &previous))
            .collect::<Result<Vec<_>, _>>()?;
        let native_model_id = command.native_model_id.clone().unwrap_or_default();
        let publication = PublishRuntimeCommand {
            offerings: Some(completed),
            ..command.clone()
        }
        .normalize()?;
        Ok((publication, native_model_id))
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

    /// 管理员读：可被运营选中的 Offering 清单，按厂商与厂商模型名稳定排序。
    ///
    /// 它是发布页"选 vendor → 勾 Offering"的数据来源（`docs/design/0012-platform-model-publishing.md`
    /// §2.1）。**不含渠道地址与凭证变量名**——那是渠道部署事实，选择用不到。停用的供给照样列出来并
    /// 带上 `enabled: false`，运营要能看出"为什么它选不了"，而不是在清单里凭空少一条。
    pub async fn selectable_offerings(
        &self,
    ) -> Result<Vec<SelectableOfferingView>, ApplicationError> {
        let mut offerings = self.repository.selectable_offerings().await?;
        for offering in &mut offerings {
            // 渠道能力是**驱动器的事实**，不是仓库能读出来的：能不能声明金额，决定"上游声明金额 ×
            // 倍率"这条对客形态在这条通路上成不成立。界面据此过滤下拉，发布期据此拒绝。
            offering.declares_cost = self
                .adapters
                .descriptor(&offering.adapter_key)
                .is_some_and(|descriptor| descriptor.declares_cost);
        }
        Ok(offerings)
    }

    /// 管理员写：只改运维开关。没发布过的名字由仓库判成"不存在"。
    ///
    /// 改完失效该型号的 route 缓存：开关**不改变修订标识**，缓存里那份候选集在关掉之后仍然
    /// "看起来是新的"，只能靠失效把它拿掉。失效失败也不影响正确性——受理前那次轻量读
    /// （`acceptance_probe`）取的就是这一个开关，关掉的模型在它那里被当成"模型不存在"。
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
    /// 停用即刻影响之后的受理，**不需要重发修订**——启停是运行状态、不是定义，候选查询与受理路径
    /// 的复核都直接读这两列（见 [`HubRepository::enabled_offerings`]），失效只影响命中率。已受理的
    /// Job 不受影响（候选已冻结在它们的快照里）。
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

    /// 发布期的**成本护栏**：任何一条候选的**最大单次成本**超过上限就拒整份发布。
    ///
    /// 判据按**合同允许的最大输出张数**算（见 [`single_request_cost_native`]）——发布期问的是
    /// "这条供给最坏能花掉多少"，那正是合同允许的最坏情况。折算率取**此刻生效的那一行**：发布期
    /// 没有"受理时刻"这个东西，而这道判据要的是"按今天的折算率看它是不是离谱"。
    ///
    /// **算不出成本的候选跳过**（没有参考成本、没有成本币种、或该币种没有生效折算率）：判不出
    /// "有没有超"时不动它——当作"超了"会让旧形状的素材发不出去，当作 0 又等于静默放行。真正
    /// 落地的那一笔由受理期那道兜底判。
    ///
    /// 整份发布一起拒，而不是"把超了的那条候选剔掉"：候选集是发布者给的一个整体（档位与权重
    /// 合起来才是路由），替发布者删一条会让路由悄悄变成另一副样子。
    async fn validate_cost_ceiling(
        &self,
        native_model_id: &str,
        contract: &Value,
        offerings: &[NormalizedOffering],
    ) -> Result<(), ApplicationError> {
        // 合同的 `n` 上限：模型级唯一一份，全平台的候选共用它。
        let max_images = declared_output_images(native_model_id, contract)
            .map(|declared| declared.maximum)
            // 合同没声明 `n` = 这个模型收不到 `n`，一次请求只生成一张。
            .unwrap_or(1);
        // 折算率按币种取一次就够：同一份发布里的候选常常共用币种，而这是一条管理员路径，
        // 不值得为每个候选各读一次汇率表。
        let mut rates: BTreeMap<String, Option<FxRate>> = BTreeMap::new();
        for (index, offering) in offerings.iter().enumerate() {
            let Some(currency) = offering.cost_currency() else {
                continue;
            };
            let fx_rate = match rates.get(currency) {
                Some(rate) => rate.clone(),
                None => {
                    let rate = self.repository.effective_fx_rate(currency).await?;
                    rates.insert(currency.to_owned(), rate.clone());
                    rate
                }
            };
            let Some(cost_cny) = single_request_cost_cny(
                offering.formula,
                offering.cost_unit_price_microusd,
                offering
                    .pricing
                    .as_ref()
                    .map(|pricing| pricing.reference_cost_microusd),
                Some(currency),
                fx_rate.as_ref(),
                max_images,
            ) else {
                continue;
            };
            if self.cost_ceiling.exceeded_by(cost_cny) {
                return Err(ApplicationError::Validation(format!(
                    "offerings[{index}] ({} {}) may cost up to {cost_cny} microusd of upstream cost \
                     for a single request (n up to {max_images}), over the ceiling of {} microusd \
                     set by GENERATION_MAX_REQUEST_COST_MICROUSD: fix the candidate's pricing or \
                     raise the ceiling",
                    offering.provider_kind,
                    offering.provider_model_id,
                    self.cost_ceiling.max_request_cost_microusd()
                )));
            }
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
        // **渠道能力**：声明"上游给金额"的候选，要求这条通路真的会把金额交回来。能力是驱动器的
        // 事实——AIHubMix 只回四分项 `usage`，金额由平台按费率自算；APIMart 的终态另带 `cost`。
        // 工程师把成本形态或对客形态写歪就在这里点名拒绝，不等到受理或结算才发现收不到钱。
        if !descriptor.declares_cost {
            if offering.formula == PricingFormula::UpstreamDeclared {
                return Err(ApplicationError::Validation(format!(
                    "offering {}: formula is upstream_declared but adapter {} does not declare a \
                     cost (this channel returns usage only)",
                    offering.provider_model_id, offering.adapter_key
                )));
            }
            if offering.consumer_formula == PricingFormula::UpstreamDeclared {
                return Err(ApplicationError::Validation(format!(
                    "offering {}: consumer_formula is upstream_declared but adapter {} does not \
                     declare a cost (the consumer price cannot be computed)",
                    offering.provider_model_id, offering.adapter_key
                )));
            }
        }
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
/// - `max_reference_images`：不得超过承载面对参考图数量的声明。
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
    if let Some(max_reference_images) = offering
        .restrictions
        .get("max_reference_images")
        .and_then(Value::as_u64)
    {
        // 限制只能收窄：承载面没承诺收图上限（数组没写 `maxItems`）时，任何正的
        // `max_reference_images` 都算凭空放宽，同样拒绝。`0` 不需要承载面声明任何参考图参数。
        if max_reference_images > 0 {
            match declared_reference_image_limit(schema) {
                Some(declared) if max_reference_images <= declared => {}
                Some(declared) => {
                    return Err(ApplicationError::Validation(format!(
                        "restriction allows at most {max_reference_images} reference image(s), but the carrier surface declares at most {declared}"
                    )));
                }
                None => {
                    return Err(ApplicationError::Validation(format!(
                        "restriction allows at most {max_reference_images} reference image(s), but the carrier surface declares no reference image count"
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

/// 该型号当前生效修订里**可以按身份被沿用**的一条候选：渠道三要素、驱动器，以及**计价依据**。
///
/// 计价依据一并带上，是因为改价场景下它与渠道地址是同一类东西：它们都是"渠道怎么结算"的事实，
/// 这次发布一个字都没变，而管理端视图也不回显它们（视图给的是对客价与参考成本）。少了这一半，
/// 运营改一个倍率就会把渠道的四档费率与价目出处写成空值——**看起来改了价，其实改坏了渠道价目**。
///
/// 它不带 `enabled`：停用的候选也要能被沿用（"改价之后重新启用"是常见动作），按启用状态过滤会让它
/// 在改价时突然找不到。
#[derive(Debug, Clone)]
pub struct ActiveOfferingChannel {
    pub provider_kind: String,
    pub provider_model_id: String,
    pub adapter_key: String,
    pub base_url: String,
    pub credential_env: String,
    /// 计价形态（`token_rates` / `per_image` / `per_call` / `upstream_declared`）。
    pub formula: String,
    /// `token_rates` 的那份四档渠道费率与价目出处；别的形态是 `None`。
    /// 存平铺的数字而不是 [`PricePlanDraft`]：这个结构是**读回来**的事实，不是一次发布的输入。
    pub plan: Option<PricePlanRates>,
    /// `per_image` / `per_call` 的单价。
    pub cost_unit_price_microusd: Option<u64>,
    pub cost_currency: Option<String>,
    /// 定价参考成本与它的口径。
    pub reference_cost_microusd: Option<u64>,
    pub cost_basis: Option<String>,
    pub tier_prices: Option<Value>,
    pub floor_amounts: Option<Value>,
}

/// 一条**被引用**的 Offering：它的标识、它所属的厂商模型（身份与合同）、渠道与它的技术定义。
///
/// 它是引用式发布的解析结果：运营给一个 `offering_id`，服务端据此取出"这条候选是谁、技术定义是什么"，
/// 技术定义本身由仓库在发布时快照进条目（见 `docs/design/0012-platform-model-publishing.md` §5）。
///
/// 技术定义与计价形态在这里**读回来**，是为了让引用式候选能走内联那条老路的同一套校验与归一：
/// 草稿里只有运营给的那几样（档位、权重、价），承载面、参数映射、限制、渠道三要素、驱动器与形态
/// 都必须由这一行补齐，否则引用形态会在 `normalize` 里被"承载面缺失 / 渠道身份不完整"拒掉。
#[derive(Debug, Clone)]
pub struct ReferencedOffering {
    pub offering_id: OfferingId,
    pub vendor_id: String,
    pub native_model_id: String,
    pub native_revision: String,
    /// 该厂商模型的调用方合同（模型级唯一一份）。
    pub capability_schema: Value,
    /// 渠道三要素：这次发布要按它把候选落到既有的那条供给行上。
    pub provider_kind: String,
    pub base_url: String,
    pub credential_env: String,
    /// 驱动器。
    pub adapter_key: String,
    /// 渠道侧的模型名。
    pub provider_model_id: String,
    /// 这条供给**能承载**合同里的哪些字段。
    pub carrier_schema: Value,
    pub parameter_mapping: Value,
    pub restrictions: Value,
    /// 计价形态（`token_rates` / `per_image` / `per_call` / `upstream_declared`）。
    pub formula: String,
    /// 该 Offering 当前那行渠道费率（`token_rates` 才有）；别的形态是 `None`。
    /// 存平铺的数字而不是 [`PricePlanDraft`]：它是**读回来**的事实，不是一次发布的输入。
    pub plan: Option<PricePlanRates>,
    /// 该 Offering **实际生效的**成本币种（有 Price Plan 时是它的币种，否则是这条供给最近一次发布
    /// 声明的那个）：运营不给币种时由它兜底。按张 / 按次计价的供给没有 Price Plan，缺了它就会被
    /// "必须显式声明成本币种"拒掉——而币种是渠道事实，不该要运营每条候选重报一遍。
    pub cost_currency: Option<String>,
    /// 按张 / 按次的渠道成本单价（`per_image` / `per_call` 才有；别的形态是 `None`）。
    ///
    /// 它是**渠道怎么结算**的事实，不是运营的决定——运营给的是对客卖多少钱。所以引用式发布里它缺省取
    /// 行上的值，不由 `OfferingReference` 携带（携带就等于允许运营改渠道成本，毛利口径会跟着漂）。
    pub cost_unit_price_microusd: Option<u64>,
}

/// 一条**可被运营选中**的 Offering：引用式发布里那个 `offering_id` 指向的东西。
///
/// 它与 [`GatewayModelCandidateView`] 不是同一个读模型：后者是"**某个已发布型号**的候选长什么样"，
/// 而这一条是"库里有哪些现成的供给可以选"——它在发布**之前**就要看得到，按厂商分组，所以要带厂商身份。
///
/// 它**不含渠道地址与凭证变量名**：那是渠道部署事实，运营选一条供给不需要它们（`0012` §2.1）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectableOfferingView {
    pub offering_id: OfferingId,
    /// 厂商（例如 OpenAI）与它的模型身份——运营先选厂商，再在这个分组里选供给。
    pub vendor_id: String,
    pub native_model_id: String,
    pub native_revision: String,
    /// 渠道与渠道侧模型名。
    pub provider_kind: String,
    pub provider_model_id: String,
    pub adapter_key: String,
    /// 这条供给按什么计价（`token_rates` / `per_image` / `per_call` / `upstream_declared`）。
    pub formula: String,
    /// 这条通路的驱动器**会不会从上游响应里取到金额**（`AdapterDescriptor::declares_cost`）。
    ///
    /// 它决定"上游声明金额 × 倍率"这条对客形态在这条通路上成不成立：界面据此过滤下拉、发布期据此
    /// 拒绝。由应用层按驱动器声明填——仓库不认识驱动器。
    pub declares_cost: bool,
    /// 渠道成本币种与 `token_rates` 的四档费率（别的形态没有费率）。
    pub cost_currency: Option<String>,
    pub cost_rates: Option<PricePlanRates>,
    /// 能不能选：供给自己启用、且它所属渠道启用。停用的仍列出来并标明，运营要能看出"为什么它选不了"。
    pub enabled: bool,
}

/// 生效修订里一条候选的渠道费率（`token_rates` 的参数）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricePlanRates {
    pub currency: String,
    pub text_input_microusd_per_million: u64,
    pub image_input_microusd_per_million: u64,
    pub text_output_microusd_per_million: u64,
    pub image_output_microusd_per_million: u64,
    pub source_url: String,
}

/// 这条候选是否**省略**了渠道字段。
///
/// 判据是渠道三要素里**任一个**为空：合同要求它们**整组给或整组省**（见 `docs/design/0010` §4.1），
/// 所以"有一个空"就按"整组省略"处理，再由沿用补齐；补齐后若渠道身份仍不完整，由既有的逐候选校验
/// 拒绝。这样"半新半旧"的渠道（地址沿用旧账号、凭证换了新账号）不会落库。
#[must_use]
fn command_omits_channel(draft: &OfferingDraft) -> bool {
    let blank = |value: &Option<String>| value.as_deref().is_none_or(|text| text.trim().is_empty());
    blank(&draft.provider_kind) || blank(&draft.base_url) || blank(&draft.credential_env)
}

/// 这条候选是否**省略**了计价依据（形态）。
///
/// 判据只是 `formula` 有没有给：形态是计价参数的**判别式**——不知道形态就不知道那份四档费率、单价与
/// 成本币种该不该在这儿，所以"形态没给"就是"整套计价依据都沿用上一版"。反过来，给了形态就必须把它
/// 配套的参数一起给（那条配套校验在 [`normalize_billing`] 里）——**改价要改的就是这些**。
#[must_use]
fn command_omits_pricing(draft: &OfferingDraft) -> bool {
    draft
        .formula
        .as_deref()
        .is_none_or(|value| value.trim().is_empty())
}

/// 补齐一条候选省略掉的**渠道字段与计价依据**，来源是该型号当前生效修订里的同一条候选。
///
/// 按 `provider_kind` + `provider_model_id` 认同一条候选。**同名多条时拒绝**——两条不同渠道可以提供
/// 同一个渠道模型名，那时"沿用哪一条"没有唯一答案，由发布者显式给出渠道三要素。
///
/// 两组字段**各自独立**判断：只省渠道、却显式给了新的计价形态时，计价用新的、渠道沿用旧的。少了这条
/// 独立判断，"重签渠道价目但不动渠道地址"会被旧价目覆盖掉。
///
/// 省略了东西却给不出 `provider_kind` 或 `provider_model_id` 时拒绝：那时连"这条候选是谁"都说不清，
/// 无从判断沿用是否得当。型号还没有生效修订、或上一版没有同身份的候选时同样拒绝，理由相同。
fn inherit_offering(
    index: usize,
    draft: &OfferingDraft,
    previous: &[ActiveOfferingChannel],
) -> Result<OfferingDraft, ApplicationError> {
    let omits_channel = command_omits_channel(draft);
    let omits_pricing = command_omits_pricing(draft);
    if !omits_channel && !omits_pricing {
        return Ok(draft.clone());
    }
    let provider_kind = draft.provider_kind.as_deref().unwrap_or_default().trim();
    let provider_model_id = draft.provider_model_id.trim();
    if omits_channel && (provider_kind.is_empty() || provider_model_id.is_empty()) {
        return Err(ApplicationError::Validation(format!(
            "offering {} omits the channel (provider_kind / base_url / credential_env) and does not \
             say which offering it continues: provider_kind and provider_model_id are required when \
             the channel is omitted",
            draft.provider_model_id
        )));
    }
    // 只省计价时也要认同一条候选，但身份由修订自己给（`provider_model_id` + 上一版里唯一的那条）。
    let mut matched = previous.iter().filter(|row| {
        row.provider_model_id == provider_model_id
            && (!omits_channel || row.provider_kind == provider_kind)
    });
    let Some(first) = matched.next() else {
        // 沿用不到时**说回"必填"**而不是"沿用不到"：那种情形下没有可沿用的来源，发布者要做的事就是
        // 把字段给全，所以要点名缺的是哪个字段（既有合同用例按这条措辞判定）。
        if omits_pricing {
            return Err(ApplicationError::Validation(format!(
                "offerings[{index}].formula is required: state how this supply is priced \
                 (token_rates / per_image / per_call / upstream_declared)"
            )));
        }
        return Err(ApplicationError::Validation(format!(
            "offering {provider_kind}/{provider_model_id} omits the channel and the model's current \
             revision has no offering with that identity: give base_url and credential_env explicitly"
        )));
    };
    if matched.next().is_some() {
        return Err(ApplicationError::Validation(format!(
            "offering {provider_kind}/{provider_model_id} omits {}, and the model's current revision \
             has more than one offering with that identity: give base_url and credential_env \
             explicitly so the right one is inherited",
            if omits_channel {
                "the channel"
            } else {
                "its pricing"
            }
        )));
    }
    let mut inherited = draft.clone();
    if omits_channel {
        inherited.provider_kind = Some(first.provider_kind.clone());
        inherited.adapter_key = Some(first.adapter_key.clone());
        inherited.base_url = Some(first.base_url.clone());
        inherited.credential_env = Some(first.credential_env.clone());
    }
    if omits_pricing {
        inherited.formula = Some(first.formula.clone());
        inherited.price_plan = first.plan.as_ref().map(|plan| PricePlanDraft {
            currency: plan.currency.clone(),
            text_input_microusd_per_million: plan.text_input_microusd_per_million,
            image_input_microusd_per_million: plan.image_input_microusd_per_million,
            text_output_microusd_per_million: plan.text_output_microusd_per_million,
            image_output_microusd_per_million: plan.image_output_microusd_per_million,
            source_url: plan.source_url.clone(),
        });
        inherited.cost_unit_price_microusd = first.cost_unit_price_microusd;
        inherited.cost_currency = first.cost_currency.clone();
        inherited.reference_cost_microusd = first.reference_cost_microusd;
        inherited.cost_basis = first.cost_basis.clone();
        inherited.tier_prices = first.tier_prices.clone();
        inherited.floor_amounts = first.floor_amounts.clone();
    }
    Ok(inherited)
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
    // 比的是**输入参考图**张数上限：这个数由 Driver 说，供给的 `max_reference_images` 只能更小。
    let max_reference_images = offering
        .restrictions
        .get("max_reference_images")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if max_reference_images > descriptor.max_reference_images {
        return Err(ApplicationError::Validation(format!(
            "adapter {} supports at most {} reference image inputs",
            descriptor.key, descriptor.max_reference_images
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
    /// 该账户**当天**最多能花掉多少（microusd，运营取值，见 [`GenerationDailySpendLimit`]）。
    ///
    /// 它与并发上限守的不是同一件事：并发上限守的是"同时在跑几个"，钱烧光的形态却是**串行**
    /// 的——一个接一个地跑、每一个都合规，照样能在一天里把余额花完。所以这里问的是当日合计里
    /// "今天已经扣掉多少"，而不是任何计数器。
    max_daily_spend_microusd: u64,
    /// 加速层：候选集从缓存取、受理后把余额快照写穿、以及**只在新鲜时**的不足提示。
    acceleration: Arc<AccelerationService>,
    /// 成本护栏：单次请求可能花掉的上游成本上限（运营取值）。
    cost_ceiling: RequestCostCeiling,
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
            max_daily_spend_microusd: GenerationDailySpendLimit::default_limit()
                .max_daily_spend_microusd,
            acceleration,
            cost_ceiling: RequestCostCeiling::default_ceiling(),
        }
    }

    /// 装上运维给的**单次请求成本上限**。
    ///
    /// 上限是配置项：它随部署形态与上游价格变，所以由调用方给，而不是写死在这里。
    #[must_use]
    pub fn with_cost_ceiling(mut self, ceiling: RequestCostCeiling) -> Self {
        self.cost_ceiling = ceiling;
        self
    }

    /// 装上运维给的**每日扣费上限**。
    ///
    /// 上限是配置项：它随部署形态与客户分级变，所以由调用方给，而不是写死在这里。
    #[must_use]
    pub fn with_daily_spend_limit(mut self, limit: GenerationDailySpendLimit) -> Self {
        self.max_daily_spend_microusd = limit.max_daily_spend_microusd;
        self
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
        // 每日扣费上限：与上面那条并发判定是**两道不同的门**——那是"同时在跑几个"（保护上游
        // 与渠道），这是"今天已经花掉多少钱"（保护钱）；花钱可以是完全串行的，并发计数看不见
        // 它。判据与口径见 [`HubRepository::daily_spend_microusd`]。放在受理路径上、紧挨并发
        // 判定：两者都是"这次请求进不进得来"的门，进门之前判。
        let spent_microusd = self
            .repository
            .daily_spend_microusd(request.account_id)
            .await?;
        if let Some(rejected) =
            daily_spend_limit_error(self.max_daily_spend_microusd, spent_microusd, Utc::now())
        {
            return Err(rejected);
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
            None => CandidateSet {
                candidates: self.repository.active_offering(&request.model).await?,
                // 没有缓存：候选刚由数据库给出，取数时已经判过供给与渠道的启用状态。
                enabled_offerings: None,
            },
        };
        // 复核结果只在缓存给出的候选集上是 `Some`（见 [`CandidateSet`]）：停用因此不依赖那次失效
        // 有没有成功——被停用的供给或渠道在这一步变成不合格候选，一条都不合格时按平台侧故障处置。
        let enabled_offerings = candidates.enabled_offerings.as_ref();
        let candidates = &candidates.candidates;
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
                select_candidate_with_strategy(
                    &request,
                    branch,
                    candidates,
                    enabled_offerings,
                    &choice,
                )?
            }
            None => select_candidate(&request, branch, candidates, enabled_offerings)?,
        };
        // 受理时把定价随快照冻结，并算定这次的预授权额（保底额）。策略在受理时已经定下候选，
        // 所以售价**不必等上游回来**；冻结之后结算只读那份快照，受理之后改汇率、改加价系数、
        // 重发修订都不影响这一个 Job。
        let hold_microusd = self.freeze_pricing(&request, &mut offering).await?;
        // 成本护栏（兜底）：发布期已经按"这条供给最坏能花多少"判过一次，这里按**这一次请求**再判
        // 一次——上限被调小、或折算率变差之后，**已经发布出去的**供给不会自己重判，而它此刻真的
        // 会花掉这么多。判在预检与建 Job 之前：不建 Job、不扣款、不留预授权。
        //
        // 判的是**成本**，不是售价、也不是客户余额：对客是平台侧故障（503），不是"你余额不足"。
        if let Some(cost_cny) = single_request_cost_cny(
            offering.price_snapshot.formula,
            offering.price_snapshot.cost_unit_price_microusd,
            offering.price_snapshot.reference_cost_microusd,
            offering.price_snapshot.cost_currency.as_deref(),
            offering.price_snapshot.fx_rate.as_ref(),
            requested_image_count(&native_parameters),
        ) && self.cost_ceiling.exceeded_by(cost_cny)
        {
            return Err(ApplicationError::RequestCostCeilingExceeded(format!(
                "model {} offering {} could cost up to {cost_cny} microusd of upstream cost for \
                 this request, over the ceiling of {} microusd set by \
                 GENERATION_MAX_REQUEST_COST_MICROUSD",
                request.model,
                offering.offering_id,
                self.cost_ceiling.max_request_cost_microusd()
            )));
        }
        // 预检：读缓存里的可用额作提示（新鲜且不足时记一条日志）。它**不决定受理**：缓存不足
        // 也要由下面的数据库条件更新确认才回 402（`0013` §3）。
        if let Some(probe) = &probe {
            self.acceleration
                .precheck_balance(request.account_id, hold_microusd, &request.model, probe)
                .await;
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
    /// 旧修订没有定价（快照里没有该供给的保底表）：这一步什么都不做，返回平台兜底数——它也按
    /// 请求张数缩放（`n = 1` 时与旧口径逐位相同）。
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
        // 保底额按**请求张数**缩放：保底表里给的是每张额，`hold = n × 每张额`（`n` 缺省 1，
        // `ADR-0009` ②）。回落链的每一层都乘 `n`——请求 10 张时"档位查不到"也不能按 1 张冻。
        let images = requested_image_count(&request.native_parameters);
        let scale = |per_image: u64| {
            per_image.checked_mul(images).ok_or_else(|| {
                ApplicationError::Configuration(format!(
                    "the hold overflows: {per_image} micros per image × {images} images"
                ))
            })
        };
        if offering.price_snapshot.floor_amounts.is_none() {
            return scale(self.max_cost_microusd);
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
        let (per_image_microusd, hold_source) = table
            .lookup(
                tier.as_ref(),
                literal_parameter_text(&request.native_parameters, "quality"),
            )
            .unwrap_or((self.max_cost_microusd, HoldSource::PlatformDefault));
        let hold_microusd = scale(per_image_microusd)?;
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
    /// 一次上游调用的超时**取值口径**：按本次请求的张数算，封顶在上限。
    ///
    /// 为什么不是固定值：对客是同步接口，`n` 张图就是一次上游调用，耗时随 `n` 增长；固定超时会
    /// 在 `n` 大时把还在生成的上游调用掐断——上游照样计费，我们却拿不到结果。
    timeouts: RequestTimeoutPolicy,
    /// 可证明未受理时的重投策略（上限与退避）。见 [`RetryPolicy`]。
    retry_policy: RetryPolicy,
    /// 加速层：结算与失败收尾都改余额，提交后要把新余额写穿缓存。
    acceleration: Arc<AccelerationService>,
    /// 平台故障告警出口：**配了才有**。没配时连候选的连续失败计数都不读。
    alerts: Option<PlatformAlertExit>,
}

impl WorkerService {
    pub fn new(
        repository: Arc<dyn HubRepository>,
        adapters: Arc<dyn AdapterFactory>,
        credentials: Arc<dyn CredentialProvider>,
        worker_id: String,
        lease_duration: ChronoDuration,
        timeouts: RequestTimeoutPolicy,
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
            timeouts,
            // 缺省策略就是**开着**的重投（次数有限、退避有上限）：这条能力不靠部署时记得配开关，
            // 而"关掉重投"是显式配 `GENERATION_RETRY_MAX_ATTEMPTS=1`。
            retry_policy: RetryPolicy::default(),
            acceleration,
            alerts: None,
        })
    }

    /// 装上运维给的重投策略（上限与退避基）。
    ///
    /// 它是配置项：最坏情况下一个请求会占用对客同步窗口多久，由这两项决定，而窗口与上游的
    /// 抖动程度都随部署形态变，写死在代码里就只能靠改代码调。
    #[must_use]
    pub fn with_retry_policy(mut self, retry_policy: RetryPolicy) -> Self {
        self.retry_policy = retry_policy;
        self
    }

    /// 装上加速层：结算与失败收尾之后要把余额写穿缓存。
    #[must_use]
    pub fn with_acceleration(mut self, acceleration: Arc<AccelerationService>) -> Self {
        self.acceleration = acceleration;
        self
    }

    /// 装上平台故障告警出口。
    ///
    /// `consecutive_failures` 是配置项：某条候选连续失败到这个次数才外发。不装这个出口就是今天的
    /// 路径——不告警，也不为告警多读一次库。
    #[must_use]
    pub fn with_platform_alerts(
        mut self,
        alerter: Arc<PlatformAlerter>,
        consecutive_failures: NonZeroU64,
    ) -> Self {
        self.alerts = Some(PlatformAlertExit::new(alerter, consecutive_failures));
        self
    }

    /// 失败收尾 + 写穿余额。
    ///
    /// 释放预授权的那些分支会改动余额，**不写穿的话缓存里会留着一个刚写过、但偏高的余额**——
    /// 那正好是"看起来新鲜、其实已经不对"的那类值，下一次受理就可能凭它误拒。
    ///
    /// 告警外发在**处置提交之后**：那时 Job 的终态与这条失败已经落库，告警是旁路，读的是已提交的
    /// 事实，也不会反过来改它。
    async fn fail_and_refresh(
        &self,
        job: &GenerationJob,
        attempt_id: AttemptId,
        failure: AttemptFailure,
    ) -> Result<(), ApplicationError> {
        let change = self
            .repository
            .fail_job(job.id, &self.worker_id, Some(attempt_id), failure.clone())
            .await?;
        self.acceleration
            .write_balance(&change, BalanceSource::DbCommit)
            .await;
        self.raise_platform_alerts(job, &failure).await;
        Ok(())
    }

    /// 这次执行**可证明上游没有受理**，还有额度 ⇒ 稍后重投；否则按既有失败处置。
    ///
    /// 判据只有一条，来自 Driver 如实区分的失败分类（[`RetrySafety`]）：
    ///
    /// - `SafeBeforeAcceptance`：可证明未受理（连不上、上传失败、参考图取不到、上游明确拒绝
    ///   受理），上游**没开始计费**——重投不会付两次，且还有额度时重投；
    /// - `AcceptanceUnknown`（超时、5xx、响应读不出）：状态不确定，**一律不重投**，按既有口径
    ///   进对账。宁可进对账，也不重投——这是"不会为同一个请求付两次上游成本"的保证；
    /// - `NotRetryable`（参数/凭证类确定性拒绝）：重投同一份请求只会得到同一个答复，不重投。
    ///
    /// 重投**不重新预授权**：预授权在重投期间原样保留，到最后那次成功（结算一次）或用尽额度
    /// 失败（释放一次）才动。所以这里只把这次执行收尾、把 Job 推回可领取。
    ///
    /// 额度用尽时走 [`Self::fail_and_refresh`]：不新增任何对账态语义，这一次失败的处置与今天
    /// 逐位相同（释放预授权、Job 落 `failed`）。
    async fn retry_or_fail(
        &self,
        job: &GenerationJob,
        attempt_id: AttemptId,
        attempt_no: u32,
        failure: AttemptFailure,
    ) -> Result<(), ApplicationError> {
        let unaccepted = failure.retry_safety == RetrySafety::SafeBeforeAcceptance;
        if !unaccepted || !self.retry_policy.allows_another_attempt(attempt_no) {
            return self.fail_and_refresh(job, attempt_id, failure).await;
        }
        let backoff = self.retry_policy.backoff_for(attempt_no);
        tracing::info!(
            job_id = %job.id,
            attempt_no,
            backoff_ms = backoff.as_millis(),
            max_attempts = self.retry_policy.max_attempts,
            "the provider provably did not accept this request; the job will be retried"
        );
        self.repository
            .requeue_after_unaccepted(UnacceptedAttempt {
                job_id: job.id,
                worker_id: self.worker_id.clone(),
                attempt_id,
                attempt_no,
                failure,
                // 退避的时刻用**进程时钟**算，但落库之后一律由库的 `now()` 比较：领取那条查询
                // 判的是 `next_attempt_at <= now()`，所以这个值只是"从现在起等这么久"。
                next_attempt_at: Utc::now()
                    + ChronoDuration::from_std(backoff).map_err(|_| {
                        ApplicationError::Configuration(
                            "GENERATION_RETRY_BACKOFF_BASE_MS is out of range".to_owned(),
                        )
                    })?,
            })
            .await
    }

    /// 这次失败要不要外发一条平台故障告警。
    ///
    /// 三个触发条件都是**平台侧事件**，同一次失败**最多外发一条**：平台欠费或凭证类失败、对账
    /// 案例新增、某候选连续失败 N 次。前两条看这次失败本身，第三条才去数这条候选最近的终态——
    /// 前两条成立时不再数，是因为再发一条逐字相同的告警没有信息量（聚合与静默期是接入方的事，
    /// 但这里连重复都不产生）。
    ///
    /// 全程不返回错误：读不到计数只记日志，这一次失败就不外发——旁路出问题不改主流程的结论。
    async fn raise_platform_alerts(&self, job: &GenerationJob, failure: &AttemptFailure) {
        let Some(exit) = &self.alerts else {
            return;
        };
        if is_platform_event(failure) {
            exit.notify(PlatformAlert::of(job, failure.kind)).await;
            return;
        }
        match self
            .repository
            .consecutive_offering_failures(job.offering.offering_id, exit.window())
            .await
        {
            Ok(count) if count >= exit.threshold() => {
                exit.notify(PlatformAlert::of(job, failure.kind)).await;
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(
                job_id = %job.id,
                offering_id = %job.offering.offering_id,
                error = %error,
                "could not count the candidate's consecutive failures; no alert is sent for this failure"
            ),
        }
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
        // 这次是同一台 Job 内的第几次执行：**由库在写入那一刻定号**（现有行数 + 1），返回值就是
        // 这次执行的号。不在这里读一次行数再自己加一——两次执行并发时读到的行数会同时是同一个，
        // 而重投的次数上限正是拿这个号比的，号重复等于上限失效。
        let attempt_no = self
            .repository
            .begin_attempt(claimed.job.id, &self.worker_id, attempt_id, &request_digest)
            .await?;
        let credential = match self
            .credentials
            .resolve(&claimed.job.offering.credential_env)
        {
            Ok(credential) => credential,
            Err(error) => {
                self.fail_and_refresh(
                    &claimed.job,
                    attempt_id,
                    AttemptFailure {
                        provider_code: "credential_unavailable".to_owned(),
                        public_code: PublicErrorCode::PlatformUnavailable,
                        message: error.to_string(),
                        trace_id: None,
                        kind: ProviderFailureKind::PlatformInternal,
                        // 取不到凭证确实"请求没交出去"，但重投同一份配置只会得到同一个结果：
                        // 这是平台自己的配置问题，重投解决不了，按确定性失败处置。
                        retry_safety: RetrySafety::NotRetryable,
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
        // 这次上游调用要用多久：按**冻结在这条 Job 上的张数**算（缺 `n` 就是一张），不是配置里
        // 那个固定值。取 Job 上的那份，而不是本次执行恰好带进来的什么值——受理时定下的请求形状
        // 才是这次要在上游生成几张的依据。
        let provider_timeout = self
            .timeouts
            .upstream_timeout_for(requested_image_count(&claimed.job.native_parameters));
        let adapter = match self.adapters.create(
            &claimed.job.offering.adapter_key,
            &claimed.job.offering.base_url,
            provider_timeout,
        ) {
            Ok(adapter) => adapter,
            Err(error) => {
                self.fail_and_refresh(
                    &claimed.job,
                    attempt_id,
                    AttemptFailure {
                        provider_code: "adapter_configuration_failed".to_owned(),
                        public_code: PublicErrorCode::PlatformUnavailable,
                        message: error.to_string(),
                        trace_id: None,
                        kind: ProviderFailureKind::PlatformInternal,
                        // 同上：装不起来是我们自己的配置问题，重投不会有别的结果。
                        retry_safety: RetrySafety::NotRetryable,
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
                        &claimed.job,
                        attempt_id,
                        AttemptFailure {
                            provider_code: "result_delivery_failed".to_owned(),
                            public_code: PublicErrorCode::OutcomeUnknown,
                            message: error.to_string(),
                            trace_id: None,
                            kind: ProviderFailureKind::PlatformInternal,
                            // 结果已经生成、只是交付不了：这不是"未受理"，绝不重投
                            // （重投等于为同一个请求再付一次上游成本）。
                            retry_safety: RetrySafety::AcceptanceUnknown,
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
                self.retry_or_fail(&claimed.job, attempt_id, attempt_no, failure)
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
    // 这里判的是**带进来的参考图**张数，与"这次要出几张图"（合同声明的是 `n`）无关。
    let max_reference_images = restrictions
        .get("max_reference_images")
        .and_then(Value::as_u64)
        .unwrap_or(1);
    if u64::try_from(image_count).unwrap_or(u64::MAX) > max_reference_images {
        return Err(ApplicationError::Validation(format!(
            "offering accepts at most {max_reference_images} reference image(s)"
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
        // 合同为**输出张数**声明的取值面是唯一在这里判取值的参数：它与别的参数不同，平台自己就要
        // 按它算超时与成本（见 [`validate_declared_integer`]）。
        validate_declared_integer(contract, &parameters, "n", &request.model)?;
        Ok(parameters)
    } else {
        Err(ApplicationError::Validation(format!(
            "missing required parameter(s): {}",
            missing.join(", ")
        )))
    }
}

/// 按**合同自己**为某个整数参数声明的取值面（`type: integer` 与 `minimum` / `maximum`）校验它的值。
///
/// 只对**输出张数** `n` 做这件事，理由是它与别的参数在平台这一侧的分量不同：这一次请求的超时窗口
/// （按张数推导）与成本护栏（按张数乘单价）都拿它当输入，而它同时也是发给上游的"要几张"。放它
/// 过去，合同里那句 `maximum` 就只是一句文档：调用方给 `n = 100`，平台按合同的 10 算超时与成本，
/// 上游却可能真的生成 100 张。别的参数（`quality`、`seed`…）的取值仍然不在这里判——那是上游按
/// 自己的 schema 处置的事，平台替它判会把"上游认得的取值"变成平台要维护的清单。
///
/// 判据取自**合同自己那份声明**：不同型号声明不同的界（`config/bootstrap` 里顶层合同是 10，
/// APIMart 那条候选的承载面是 4），平台没有、也不该有一个统一的数。
///
/// 只判声明过的部分：合同没声明这个参数、或没声明 `type` 与上下界时**不判**——把一条不存在的
/// 条款变成对客错误，比放过它更糟。对客是**调用方的参数问题**（`400 invalid_parameter`），不是
/// 平台侧故障：值是他给的，改法也在他那一侧。
fn validate_declared_integer(
    contract: &Value,
    parameters: &Map<String, Value>,
    name: &str,
    model: &str,
) -> Result<(), ApplicationError> {
    let Some(value) = parameters.get(name).filter(|value| !value.is_null()) else {
        return Ok(());
    };
    let Some(schema) = contract
        .get("properties")
        .and_then(|properties| properties.get(name))
    else {
        return Ok(());
    };
    let declared_integer = schema.get("type").and_then(Value::as_str) == Some("integer");
    let minimum = schema.get("minimum").and_then(Value::as_i64);
    let maximum = schema.get("maximum").and_then(Value::as_i64);
    if !declared_integer && minimum.is_none() && maximum.is_none() {
        return Ok(());
    }
    let Some(number) = declared_integer_value(value) else {
        return Err(ApplicationError::InvalidParameter(format!(
            "{name} for model {model} must be an integer, got {value}"
        )));
    };
    if let Some(minimum) = minimum
        && number < minimum
    {
        return Err(ApplicationError::InvalidParameter(format!(
            "{name} for model {model} must be at least {minimum}, got {number}"
        )));
    }
    if let Some(maximum) = maximum
        && number > maximum
    {
        return Err(ApplicationError::InvalidParameter(format!(
            "{name} for model {model} must be at most {maximum}, got {number}"
        )));
    }
    Ok(())
}

/// 这个值是不是一个整数（`3.0` 也算：JSON Schema 的 `integer` 就是"没有小数部分"）。
fn declared_integer_value(value: &Value) -> Option<i64> {
    if let Some(number) = value.as_i64() {
        return Some(number);
    }
    let number = value.as_f64()?;
    (number.fract() == 0.0).then_some(number as i64)
}

/// 把参数面上的输出张数 `n` 夹到**这条候选承载面**为它声明的 `maximum`。
///
/// 请求里的 `n` 是"最多要几张"，不是"必须给我几张"：合同声明 `10`、这条候选只声明 `4` 时，
/// 调用方给 `6` 表达的是"至少 4 张也成，多多益善"。夹到 `4` 照常受理，比为此判这条候选承载不了
/// （换候选，全不行即 503）更贴合那个意思——值在合同之内，调用方没有说错话。
///
/// 上界按**承载面最终落到的那个名字**找（承载面自己声明了 `n` 就用它，否则看改名表把 `n` 落到
/// 哪个名字上，与承载校验是同一条规则）：这样"承载面线上叫 `num_images`"的候选照样夹得住，
/// 不会因为换了个线上名字就把超界的值原样发出去。平台拿这个数算超时与单次成本（见
/// [`requested_image_count`] 与 [`single_request_cost_cny`]），所以改了它两处跟着改。承载面声明的
/// 下界不在这里判：把值往上抬等于替调用方多要图，平台不做这件事。
///
/// 这里不套用 [`declared_output_image_maximum`]：那个读法把 `maximum: 0` 当"没声明"（超时链的
/// 口径），而这条路上 0 就是"一张都出不了"——照读照夹，不额外发明一个语义。
fn cap_output_image_count(
    carrier: &Value,
    renames: Option<&ParameterRenames>,
    parameters: &mut Map<String, Value>,
) {
    // 判据是承载面**自己**那份声明：各候选的界不同（同一份合同下 AIHubMix 声明 10、APIMart 声明 4）。
    let Some(wire_name) = wire_parameter_name(carrier, renames, "n") else {
        return;
    };
    let Some(maximum) = carrier
        .get("properties")
        .and_then(|properties| properties.get(&wire_name))
        .and_then(|n| n.get("maximum"))
        .and_then(Value::as_u64)
    else {
        return;
    };
    let over = parameters
        .get("n")
        .and_then(declared_integer_value)
        .is_some_and(|requested| requested > maximum as i64);
    if over {
        parameters.insert("n".to_owned(), Value::from(maximum));
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
/// **输出张数 `n` 超过这条候选声明的上界不算承载不了**：值取该上界发出去（见
/// [`cap_output_image_count`]）——`n` 是"最多要几张"，请求给得更多是"少给几张也成"。承载校验
/// 不判取值：这条候选声明的 `n` 下界不是判据，低于它的值原样上行，由上游按自己的 schema 处置。
///
/// 合格之后才组装要落进 Job、并发给上游的参数面：
/// 1. 按承载留下名字：承载面声明了这个名字就用它，否则用改名表映射出来的**线上名字**；两边都
///    落不到的名字（含调用方给了空值的）在这里去掉——空值不携带信息，而发一个承载不了的字段名
///    给上游，只会得到上游自己的一套解释；
/// 2. 把参考图与遮罩落到这条候选**自己声明的**参数名上（声明不了就是不合格，绝不静默丢图）；
/// 3. 注入映射声明的**显式默认值**：调用方没给的字段由平台定，而不是由渠道自己的默认值定；
/// 4. 把输出张数 `n` 夹到这条候选**自己声明的**上限（见 [`cap_output_image_count`]）；
/// 5. 按映射声明做**尺寸换算**：这一步在改名之前做，因为换算的源字段是**合同字段名**；
/// 6. 改名：把还没落到线上的合同字段名换成这条供给线上要发的名字；
/// 7. 按**取值映射表**把取值换成线上取值：表里没有的取值让这条候选不合格（不猜、不透传原值）；
/// 8. 最后看一眼承载面**自己声明的必填字段**是否都在场：供给说了"这次请求必须带上它"，
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
    cap_output_image_count(&offering.carrier_schema, renames.as_ref(), &mut parameters);
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
            // 失败分类**原样带过来**：它是 Driver 对"上游有没有可能已经受理"的唯一判定，编排层
            // 只消费它、不再按状态码或错误码猜一遍（猜一遍就等于平台自己另立了一套判据）。
            retry_safety: provider.retry_safety,
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
        // 这类也不重投：同一份参数与配置再交一次，Driver 会以同样的方式挡下。
        AdapterError::Configuration(message) | AdapterError::UnsupportedInput(message) => {
            AttemptFailure {
                provider_code: "adapter_rejected".to_owned(),
                public_code: PublicErrorCode::PlatformUnavailable,
                message,
                trace_id: None,
                kind: ProviderFailureKind::PlatformInternal,
                retry_safety: RetrySafety::NotRetryable,
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
