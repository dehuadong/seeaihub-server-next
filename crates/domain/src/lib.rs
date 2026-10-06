use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};
use thiserror::Error;
use uuid::Uuid;

mod execution_protocol;
mod image_parameters;
mod parameter_mapping;
mod request_structure;
mod size_spec;
mod upload_media;
pub use execution_protocol::{
    AttemptStage, ExecutionStage, FencingToken, MAX_PROVIDER_IDENTIFIER_BYTES, ProviderTaskHandle,
    ProviderTaskState, ProviderTraceId, RECEIPT_CREDENTIAL_HEX_LEN, ReceiptCredential,
    is_bounded_provider_identifier,
};
pub use image_parameters::{
    ImageInputs, ImageParameterKind, contract_image_parameter_kind, declared_parameter_names,
    declared_reference_image_limit, declares_mask_parameter, declares_reference_image_parameter,
    image_inputs, image_parameter_kind, image_parameter_values, is_mask_parameter,
    is_reference_image_parameter, mask_value, place_image_inputs, platform_image_parameter,
    platform_image_parameters, take_contract_image_inputs, validate_image_inputs,
};
pub use parameter_mapping::{
    ParameterEnumMaps, ParameterRenames, SizeMapping, apply_enum_maps, apply_parameter_defaults,
    apply_parameter_renames, apply_size_mapping, carries_parameter, declared_defaults,
    declared_enum_maps, declared_field_names, declared_renames, declared_size_mapping,
    declares_parameter, is_used_parameter_value, literal_parameter_text, wire_parameter_name,
};
pub use request_structure::{
    REQUEST_JSON_LIMITS, REQUEST_JSON_MAX_DEPTH, REQUEST_JSON_MAX_NODES,
    REQUEST_JSON_MAX_OBJECT_FIELDS, REQUEST_JSON_MAX_STRING_BYTES, RequestJsonError,
    RequestJsonLimits, RequestParameters, RequestParametersBuilder, RequestStructureCounter,
    RequestStructureViolation, SUPPORTED_REQUEST_WIRE_BYTES,
};
pub use size_spec::{SizeForm, SizeProfile, SizeSpec, convert_size};
pub use upload_media::{
    MAX_UPLOAD_BYTES, OBJECT_KEY_PREFIX, UploadMediaType, UploadWriteFailure, new_object_key,
    object_key, within_single_file_limit,
};

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
    /// 有效计量证据；声明了成本的渠道可以没有 token 分项（ADR 0006 的放宽）。
    pub usage: Option<TokenUsage>,
}

/// 一次执行的**成本来源**：判据是"成本从哪来"，不是"金额对不对"。**三态**：
/// `Computed` 是渠道不给金额字段、平台按**实际用量**与该渠道**成本费率**自算；
/// `Declared` 是渠道终态**直接给了金额**（含渠道侧折扣，比自算权威）；`Unavailable` 是
/// 本该有金额却拿不到——**不得猜测**：不记 0、不用自算顶替、也不用上一次的值。
///
/// 这三个取值同时是 SDK 报告与库层约束的取值面：SDK 那一侧怎么映射过来只写一处
/// （`ProviderCost` 的 `From`），落库字符串只由 [`ProviderCostSource::as_str`] 给出，
/// 三处不再各写一份判据。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderCostSource {
    Computed,
    Declared,
    Unavailable,
}

impl ProviderCostSource {
    /// 落库用的稳定字符串（库层 CHECK 也认这一组）。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Computed => "computed",
            Self::Declared => "declared",
            Self::Unavailable => "unavailable",
        }
    }

    /// 从落库值还原。库层有 CHECK 保证取值；解析不到说明存储被绕过，按错误处理。
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "computed" => Some(Self::Computed),
            "declared" => Some(Self::Declared),
            "unavailable" => Some(Self::Unavailable),
            _ => None,
        }
    }
}

/// 一次执行留下的**成本事实**（成本平面：原币种原值 + 币种 + 折算后 CNY）。
///
/// 与 [`MeteringEvidence`] 并列，但**不是同一件事**：计量事实仍是上游给的分项 token，
/// 成本只进毛利口径——它**不改对客金额**，也不替代计量证据。币种按渠道声明，不假定 USD。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCostFact {
    pub source: ProviderCostSource,
    /// 原币种微单位金额；`unavailable` 时为 `None`（不猜）。
    pub amount_microusd: Option<u64>,
    /// 该渠道声明的成本币种；`unavailable` 时为 `None`。
    pub currency: Option<String>,
    /// 折算后 CNY 微单位（毛利用）。折算要用受理时冻结的汇率，所以由定价侧填；
    /// 汇率还没有落点时这一项是 `None`——不是"折算成了 0"。
    pub cny_microusd: Option<u64>,
}

/// 一条账目分录的类别。
///
/// 六种类别的**符号语义**在金额上：持有、扣费与成本是**负数**（钱被占住、真的扣掉或真的花掉），
/// 入账、释放与调整为**正数**。正负号因此是答案的一部分，读账目的人不能只看绝对值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LedgerEntryKind {
    /// 入账：建账户时的初始额度与之后的充值。它**不是**对某次执行的收费。
    Credit,
    /// 持有：受理时按保底额占住的预授权。**不是扣款**，钱还在账上只是不可用。
    Hold,
    /// 扣费：结算时的实收，金额为负。
    Capture,
    /// 释放：把占住的钱退回来（结算的差额、失败或对账退款），金额为正。
    Release,
    /// 调整：运营或对账对账目的改正。
    Adjustment,
    /// 成本：平台付给上游、自己承担的那笔费用，金额为负。
    ///
    /// 它记在**平台账户**上（`ledger.accounts.kind = 'platform'`，`migrations/0019_ledger_platform_cost.sql`
    /// 种下的那一行），与消费者的余额无关：上游已经扣了钱、而这次执行没有让消费者付费（判失败、
    /// 或对账退款结案）时，这笔钱由平台自己承担，账上必须看得见。成功那一次的成本留在执行事实上
    /// （`generation.attempts` 的成本四列），只进毛利口径，不落账本。
    Cost,
}

impl LedgerEntryKind {
    /// 落库取值，也是管理员面看到的 `kind`。
    ///
    /// 它与库里的 `CHECK` 约束、以及落库那几处字面量是同一份取值：改这里就是改存储取值。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Credit => "credit",
            Self::Hold => "hold",
            Self::Capture => "capture",
            Self::Release => "release",
            Self::Adjustment => "adjustment",
            Self::Cost => "cost",
        }
    }

    /// 认不出的取值返回 `None`：账本里出现本版本不认识的类别时，调用方要能按"读不出来"处理，
    /// 而不是猜一个方向（猜错的表现是把一笔占位读成扣款）。
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "credit" => Some(Self::Credit),
            "hold" => Some(Self::Hold),
            "capture" => Some(Self::Capture),
            "release" => Some(Self::Release),
            "adjustment" => Some(Self::Adjustment),
            "cost" => Some(Self::Cost),
            _ => None,
        }
    }
}

/// 账本上的一条分录（管理员流水的读模型）。
///
/// 它是账本行的**只读投影**：金额与余额的权威都是账本本身，流水不改写它们。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
    /// 账本行的标识。它是**同一时刻多条分录的定序键**（`created_at` 在事务内并列），因此也是历史
    /// 翻页游标定位的一部分；不进对客响应。
    pub id: Uuid,
    pub account_id: AccountId,
    pub kind: LedgerEntryKind,
    /// 分录金额（人民币微单位）：持有、扣费与成本为负，释放与调整为正。
    pub amount_microusd: i64,
    /// 归属的执行记录；建账户与充值这类不挂在执行上的分录没有它。
    pub job_id: Option<JobId>,
    /// 数据库盖章的写入时刻。**同一个事务里写的多条共用它**（`now()` 是事务时间），所以它
    /// 只用来分段，不用来定序同一笔事务内部的先后。
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceRates {
    pub currency: String,
    pub text_input_microusd_per_million: u64,
    pub image_input_microusd_per_million: u64,
    pub text_output_microusd_per_million: u64,
    pub image_output_microusd_per_million: u64,
}

/// **对客四档 CNY 费率向量**：随修订发布、受理时随 Job 快照冻结的售价依据。
///
/// 它是**按 token 计量量计价的候选**的对客价载体——运营按"成本单价 × 倍率 × 折算率"推导，
/// 也可以直接录入；渠道按张 / 按次计价或直接由上游给金额时没有这个载体，那几种形态的对客价由
/// 成本单价按同一条乘法在结算时算出来（见 [`PriceSnapshot::charge_microusd`]）。
///
/// 它与 [`PriceRates`] 是**两个载体、同一个算式**：前者是对客平面（只有 CNY），后者是成本
/// 平面（币种按渠道声明）。分开是因为两者的取值来源、发布路径与币种都不同——合成一个类型
/// 就得靠一个 `currency` 字段去猜"这份费率是对客的还是成本的"，而猜错的表现是把渠道成本
/// 当成对客售价卖出去。
///
/// 字段名只写 `micros`：这个载体的币种就是 CNY，带上 `usd` 会让读的人以为它是美元费率。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsumerRatesCny {
    pub text_input_micros_per_million: u64,
    pub image_input_micros_per_million: u64,
    pub text_output_micros_per_million: u64,
    pub image_output_micros_per_million: u64,
}

/// 结算时手上有的**执行事实**：对客实收只读它，加上受理时冻结的那份快照。
///
/// 三样各服务一种计价形态，形态决定读哪几样：按 token 计量量的候选读用量、按张的读产出张数、
/// 上游直接给金额的读上游这次声明的金额。把它们**一起**交进来，而不是按形态给不同的入参：
/// "这条供给按什么计价"只有快照自己知道，调用方先判一次形态、快照再判一次，判两遍就会漂移。
///
/// `declared_cost_microusd` 是**原币种**微单位，币种由该供给声明的成本币种决定（Driver 拿到的
/// 金额不带币种，它只能把快照里那份声明带回来）；上游没给就是 `None`——那是"没有对客计费基准"，
/// 不是"金额为 0"。
pub struct ChargeFacts<'a> {
    /// 本次实际用量（按 token 计量量的候选读它）。
    ///
    /// 声明了成本的渠道可以没有 token 分项（ADR 0006 的放宽）：那条通路只读
    /// `declared_cost_microusd`，`None` 不影响它。
    pub usage: Option<&'a TokenUsage>,
    /// 本次**产出的张数**（按张计价的候选读它）。
    pub images: usize,
    /// 上游这次声明的金额（上游直接给金额的候选读它）；没声明就是 `None`。
    pub declared_cost_microusd: Option<u64>,
}

/// 按 token 计价的算式要的那些 token：成功件没有分项就明确失败，不当 0。
fn evidence(usage: Option<&TokenUsage>) -> Result<&TokenUsage, DomainError> {
    usage.ok_or(DomainError::MissingMeteringEvidence)
}

/// 一次算钱要用的四档费率，**按名字**取数。
///
/// 四个费率都是 `u64`，按位置传进算式时把"文本输入"与"图像输入"写反了编译器一声不吭，
/// 算出来的钱要等对账才看得出来。算式因此只认这个结构体：两个载体（对客费率向量、渠道成本
/// 费率）各自把自己那一份填进来，谁也不会串到别人的档位上。
struct TokenRates {
    text_input: u64,
    image_input: u64,
    text_output: u64,
    image_output: u64,
}

impl From<&PriceRates> for TokenRates {
    fn from(rates: &PriceRates) -> Self {
        Self {
            text_input: rates.text_input_microusd_per_million,
            image_input: rates.image_input_microusd_per_million,
            text_output: rates.text_output_microusd_per_million,
            image_output: rates.image_output_microusd_per_million,
        }
    }
}

impl From<&ConsumerRatesCny> for TokenRates {
    fn from(rates: &ConsumerRatesCny) -> Self {
        Self {
            text_input: rates.text_input_micros_per_million,
            image_input: rates.image_input_micros_per_million,
            text_output: rates.text_output_micros_per_million,
            image_output: rates.image_output_micros_per_million,
        }
    }
}

/// 四档 token 费率 × 本次**实际用量**分项 → 微单位金额，向上取整。
///
/// **只有一个算式**：对客实收与渠道成本自算用的是同一套算术，读的费率各自一份（对客读对客
/// 费率向量、成本读该渠道的成本费率）。算式写两遍的代价是日后各自漂移，而漂移的表现是
/// "同一份用量在两条路上算出不同的钱"——那正是对账要查的东西。
fn token_amount_microusd(usage: &TokenUsage, rates: &TokenRates) -> Result<u64, DomainError> {
    usage.validate()?;
    let terms = [
        (usage.input_text_tokens, rates.text_input),
        (usage.input_image_tokens, rates.image_input),
        (usage.output_text_tokens, rates.text_output),
        (usage.output_image_tokens, rates.image_output),
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

impl ConsumerRatesCny {
    /// 本次实际用量 × 对客费率向量 → 实收（CNY 微单位）。
    pub fn amount_microusd(&self, usage: &TokenUsage) -> Result<u64, DomainError> {
        token_amount_microusd(usage, &TokenRates::from(self))
    }
}

impl PriceRates {
    /// 本次**实际用量** × 这组四档费率，向上取整到微单位。
    ///
    /// 对客实收与渠道成本自算**用的是同一个算式**，但读的是**各自那份费率**：对客读对客费率，
    /// 成本读该渠道的成本费率（今天 Price Plan 暂时兼作后者）。算式只有一份，免得两条路各写一遍、
    /// 日后各自漂移；而"读哪份费率"由各自的入口决定，不在这里判。
    pub fn amount_microusd(&self, usage: &TokenUsage) -> Result<u64, DomainError> {
        token_amount_microusd(usage, &TokenRates::from(self))
    }
}

/// 折算率的**定点分母**：折算率以百万分之一为单位表达。
///
/// 钱与汇率都不走浮点：`0.011354 × 1e6` 在浮点下会落在 11354.000000000002 这类值上，
/// 差一个微单位就是对不上账。分母写成常量而不是散落在各处，是为了让"1 单位该币种值多少
/// 人民币"这条口径只有一个说法。
pub const FX_RATE_DENOMINATOR: u64 = 1_000_000;

/// 一个币种折算成人民币的**折算率**（渠道币种 → CNY）。
///
/// 它是**外部事实**，按币种维护、由管理员录入，不属于某一份发布：同一时刻同一币种全平台
/// 必须是同一个数才对账得起来。受理时按候选的成本币种取"受理时刻生效的那一行"并**原值快照**
/// 进 Job，受理之后不再换算。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FxRate {
    pub currency: String,
    /// 1 单位该币种 = `rate_micros` / [`FX_RATE_DENOMINATOR`] 元人民币。
    pub rate_micros: u64,
    /// 这一行的生效时间（取值规则是"受理时刻生效的那一行"，快照里连同它一起留下）。
    pub effective_at: DateTime<Utc>,
}

impl FxRate {
    /// 把该币种的微单位金额折成人民币微单位，**向上取整**。
    ///
    /// 向上取整与费率算式同一个取向：平台记账宁可把成本记高一点，也不要因为抹掉零头而
    /// 让毛利看起来比实际好。
    pub fn to_cny_microusd(&self, amount_microusd: u64) -> Result<u64, DomainError> {
        let numerator = u128::from(amount_microusd)
            .checked_mul(u128::from(self.rate_micros))
            .ok_or(DomainError::ArithmeticOverflow)?;
        let rounded_up = numerator.div_ceil(u128::from(FX_RATE_DENOMINATOR));
        u64::try_from(rounded_up).map_err(|_| DomainError::ArithmeticOverflow)
    }
}

/// 预授权额的**来源**：事后可辨"这次为什么冻这么多"。
///
/// 取值与回落链一一对应（[`FloorTable::lookup`]）：供给档位查表 → `size = auto` 取默认档 `2K` →
/// 该供给的封顶保底值 → 平台兜底。它随快照冻结，因为保底表本身会随修订变——事后再查表
/// 得到的是"今天的表"，不是"受理时那张表"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldSource {
    /// 按这次请求归出来的档位在该供给的保底表里查到了（含像素型 `size` 按档位像素表或最长边归位）。
    Tier,
    /// `size = auto`（或 `size` 字段缺失）：取默认档 `2K`。
    AutoTier,
    /// 档位归不出来、或该档位在表里没有：回落到该供给的封顶保底值。
    SupplyCap,
    /// 连该供给的封顶保底值都没有：回落到平台级兜底数（今天的行为）。
    PlatformDefault,
}

impl HoldSource {
    /// 落库 / 进快照用的稳定字符串。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tier => "tier",
            Self::AutoTier => "auto_tier",
            Self::SupplyCap => "supply_cap",
            Self::PlatformDefault => "platform_default",
        }
    }

    /// 从落库值还原；解析不到说明存储被绕过，按错误处理。
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "tier" => Some(Self::Tier),
            "auto_tier" => Some(Self::AutoTier),
            "supply_cap" => Some(Self::SupplyCap),
            "platform_default" => Some(Self::PlatformDefault),
            _ => None,
        }
    }
}

/// `size = auto`（或调用方没给 `size` 这个字段）时用的**默认档**。
///
/// 用户口径是"按分辨率保底、通过 `size` 判断 1K/2K/4K"，而 `auto` 是"由模型自选"：取**中间档**
/// 兜底——既不是最小档（估太小会让结算频繁透支），也不是最大档（那会把每一次没钉尺寸的请求
/// 都按最贵的一档冻住）。
pub const DEFAULT_FLOOR_TIER: &str = "2K";

/// 这次请求的 `size` 归出来的**档位**，以及它是怎么来的。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTier {
    /// 规范化之后的档位（`1K` / `2K` / `4K`），直接就是保底表的键。
    pub tier: String,
    /// 这个档位是"查表得来的"还是"`auto` 的默认档"。
    pub source: HoldSource,
}

/// 把这次请求的 `size` 归到保底表的**档位**。
///
/// 保底表的键是档位，而调用方给的 `size` 常常是像素值（`1024x1024`）或比例，所以要有一条归位
/// 规则，顺序是：
/// ① 该供给**已发布的档位像素表**（它的尺寸档案，档位 → 比例 → 像素）反向查——各供给的档位
///    像素不同（同一个"1K"在不同模型上可能是 1024 或 1536），只有它自己声明的那张表才是它的
///    档位定义；
/// ② 表里没有这一格、或这条供给没发布尺寸档案 ⇒ 按**最长边**阈值兜底：≤1024 → `1K`、
///    ≤2048 → `2K`、>2048 → `4K`。四档的"K"本来就按长边命名（1K≈1024、2K≈2048、4K≈4096），
///    厂商文档里的档位像素表也都按长边分档；**这是兜底、不是精确口径**，所以只在没有该供给
///    自己的档位像素表时用；
/// ③ **`size` 字段缺失**、或字面就是 `auto` ⇒ [`DEFAULT_FLOOR_TIER`]。**空串不是"没给"**：
///    它是"给了个空"，照它的字面去归档位（归不出即回落），不替调用方当成没说话；
/// ④ 归不出来（比例型只说了形状、没说分辨率；空串、纯空白、认不出的取值）⇒ `None`，由调用方
///    回落到该供给的封顶保底值。
///
/// 取值一律**照字面**读：不 trim、不认别名（[`SizeSpec::parse`] 同一条口径）——写法是调用方
/// 的事，平台不替它收拾，也不把"写了但写歪了"悄悄换成默认档。
#[must_use]
pub fn resolve_size_tier(size: Option<&str>, profile: &SizeProfile) -> Option<ResolvedTier> {
    let Some(value) = size else {
        // **字段缺失**与 `auto` 同义：都是"调用方没钉尺寸、由模型自选"。
        return Some(auto_tier());
    };
    if value == "auto" {
        return Some(auto_tier());
    }
    match SizeSpec::parse(value) {
        // 调用方直接给了档位：就是它。
        Ok(SizeSpec::Tier { tier }) => Some(ResolvedTier {
            tier,
            source: HoldSource::Tier,
        }),
        Ok(SizeSpec::Pixels { width, height }) => {
            if let Some((_, tier)) = profile.lookup_reverse(width, height) {
                return Some(ResolvedTier {
                    tier,
                    source: HoldSource::Tier,
                });
            }
            let longest = width.max(height);
            let tier = if longest <= 1024 {
                "1K"
            } else if longest <= 2048 {
                "2K"
            } else {
                "4K"
            };
            Some(ResolvedTier {
                tier: tier.to_owned(),
                source: HoldSource::Tier,
            })
        }
        Ok(SizeSpec::Ratio { .. } | SizeSpec::Auto) | Err(_) => None,
    }
}

/// `size = auto` 的归位结果：默认档 + 它的来源。
fn auto_tier() -> ResolvedTier {
    ResolvedTier {
        tier: DEFAULT_FLOOR_TIER.to_owned(),
        source: HoldSource::AutoTier,
    }
}

/// 一条供给的**计价形态**：这个渠道的这个模型**按什么计价**（渠道事实，不是平台的定价选择）。
///
/// 它说明**平台与渠道怎么结算**，因此决定成本怎么算；对客卖多少钱与它无关（对客售价走
/// [`ConsumerRatesCny`]）。取值由渠道的计价方式决定，平台如实登记：按四分项 token 计量量、
/// 按产出张数、按调用次数，或**由上游直接给实扣金额**——最后这种平台没有自己的计价参数，
/// 金额由执行事实 [`ProviderCostSource::Declared`] 承接，不在这里编一个数。
///
/// 形态与它的参数一一配套（见发布期校验）：`token_rates` 要一份四档费率（原来挂在 Price Plan
/// 上的那一份），`per_image` / `per_call` 要一个单价，`upstream_declared` 什么参数都不要
/// ——"没有那份四档费率"因此是合法状态，`Price Plan` 也不再是每条供给的必填。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PricingFormula {
    /// 按四分项 token 计量量计价：参数是该渠道的四档费率（`PriceRates`）。
    TokenRates,
    /// 按产出的图片张数计价：参数是每张单价。
    PerImage,
    /// 按调用次数计价：参数是每次单价。
    PerCall,
    /// 上游直接给出实扣金额：平台没有计价参数，金额由执行事实承接；上游没给就是缺口。
    UpstreamDeclared,
}

impl PricingFormula {
    /// 落库 / 进快照 / 线上用的稳定字符串。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TokenRates => "token_rates",
            Self::PerImage => "per_image",
            Self::PerCall => "per_call",
            Self::UpstreamDeclared => "upstream_declared",
        }
    }

    /// 从落库值还原；解析不到说明存储被绕过，按错误处理。
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "token_rates" => Some(Self::TokenRates),
            "per_image" => Some(Self::PerImage),
            "per_call" => Some(Self::PerCall),
            "upstream_declared" => Some(Self::UpstreamDeclared),
            _ => None,
        }
    }

    /// 这种形态要不要一个单价（`per_image` / `per_call` 要，另外两种不要）。
    #[must_use]
    pub fn takes_unit_price(self) -> bool {
        matches!(self, Self::PerImage | Self::PerCall)
    }
}

/// 缺这个键的历史快照按 `token_rates` 读：那一天只有这一种计价形态。
fn legacy_pricing_formula() -> PricingFormula {
    PricingFormula::TokenRates
}

/// 按张 / 按次计费的一笔金额：**数量 × 单价**（成本平面，币种见该供给声明的成本币种）。
///
/// 与四档费率那个算式一样，钱只走整数：单价与数量都是微单位整数，乘积就是微单位金额，
/// 不引入浮点。溢出按错误处理，不截断。
pub fn unit_amount_microusd(units: u64, unit_price_microusd: u64) -> Result<u64, DomainError> {
    u128::from(units)
        .checked_mul(u128::from(unit_price_microusd))
        .and_then(|product| u64::try_from(product).ok())
        .ok_or(DomainError::ArithmeticOverflow)
}

/// 该候选的**成本来源口径**（按候选发布、随快照冻结）。
///
/// 取值面与执行事实上的 `provider_cost_source` 同源，但**不是同一个量**：这里是"这条候选按
/// 哪种来源记成本"的**发布侧口径**，那里是"这一笔实际按哪种来源取的"。快照冻结前者之后，
/// 事后能回答"这笔的成本本该按哪种来源算"，与实际取到的来源对不上时就是渠道行为变了。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostBasis {
    /// 渠道不给金额字段：成本由平台按实际用量 × 该渠道成本费率自算。
    Computed,
    /// 渠道终态直接给金额：直接取它，不自己算。
    Declared,
}

impl CostBasis {
    /// 落库 / 进快照用的稳定字符串。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Computed => "computed",
            Self::Declared => "declared",
        }
    }

    /// 从落库值还原；解析不到说明存储被绕过，按错误处理。
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "computed" => Some(Self::Computed),
            "declared" => Some(Self::Declared),
            _ => None,
        }
    }
}

/// 路由策略类型：在一批**合格候选**里"选哪一条"由运营配置的策略决定。
///
/// 零配置时的默认值是 [`RouteStrategy::PriorityFailover`]——它按 `routing_priority` 数字小的
/// 优先、该档不合格时依次降级、同档内按 `weight` 分摊，也就是策略层引入之前的行为。
///
/// 取值空间**只有合格候选**：承载面表达不了这次请求、或分支不被该候选允许的候选先被排除，
/// 任何策略都不得选中它们。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteStrategy {
    /// 按档位顺序挑：合格候选里 `routing_priority` 最小的那一档，档内按 `weight` 分摊。
    PriorityFailover,
    /// 不看档位：在**全部**合格候选里按 `weight` 分摊。
    WeightedRandom,
    /// 不看档位与权重：在合格候选里取**折后成本估算**最小的一条。
    LeastCost,
    /// 不看档位与权重：由**账户标签**经映射指定候选；映射指向的候选不合格时不选它。
    UserTag,
}

impl RouteStrategy {
    /// 落库与进缓存用的稳定字符串。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PriorityFailover => "priority_failover",
            Self::WeightedRandom => "weighted_random",
            Self::LeastCost => "least_cost",
            Self::UserTag => "user_tag",
        }
    }

    /// 从落库值还原；解析不到说明存储被绕过，按错误处理。
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "priority_failover" => Some(Self::PriorityFailover),
            "weighted_random" => Some(Self::WeightedRandom),
            "least_cost" => Some(Self::LeastCost),
            "user_tag" => Some(Self::UserTag),
            _ => None,
        }
    }
}

/// 一条生效的路由策略：作用域、类型、它自己的输入与版本标识。
///
/// `gateway_model` 为 `None` 表示**全局那条**；非空表示覆盖该网关模型（按模型取"有覆盖用
/// 覆盖、没有用全局"）。策略是**运行期配置**，不进不可变修订：改它即刻影响之后的受理，已经
/// 受理的 Job 早已把候选固定在快照里。`version` 每次写入都变，缓存拿它判断自己是不是旧的。
///
/// 两张输入表都**只被对应的策略消费**：`discount_rates` 只有 [`RouteStrategy::LeastCost`] 读，
/// `tag_channel_map` 只有 [`RouteStrategy::UserTag`] 读。没有生效的策略消费它们时，改这两张表
/// 不改变任何选路结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutePolicy {
    pub gateway_model: Option<String>,
    pub strategy: RouteStrategy,
    /// 折扣率表：**候选（`offering_id`）→ 万分比**，例如 `8000` 表示八折。
    ///
    /// 它只作 [`RouteStrategy::LeastCost`] 的比较输入：比的是**折后成本估算**，不是成本事实。
    /// 成本永远按实际扣费记，两者不一致时以实际扣费为准。
    pub discount_rates: BTreeMap<String, u32>,
    /// **标签 → 候选（`offering_id`）**的映射，供 [`RouteStrategy::UserTag`] 用。
    ///
    /// 标签落在账户上（`ledger.accounts.tag`），映射落在策略里：同一个标签在不同网关模型上可以
    /// 指向不同候选。映射指向的候选**仍要合格**——它承载不了这次请求时，策略也不能选它。
    pub tag_channel_map: BTreeMap<String, String>,
    pub version: String,
}

/// 一条供给的**保底表**（CNY）：受理时算预授权额的唯一来源。
///
/// 键是档位：`size` 本身，或 `size/quality`（**`quality` 留空即按 `size` 档**）。另有该供给的
/// **封顶保底值**：档位查不到时用它。表与封顶值都随修订发布、随 Job 快照冻结、**不编进代码**
/// ——档位结构与每档保底额都是随模型与渠道变的数据，换模型、渠道调价都不该改代码。
///
/// 它**只用于预授权**：档位价目表（`tier_prices`）降级为定价参考与展示，不参与这里，也不参与
/// 结算。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FloorTable {
    /// 档位 → 保底额（CNY 微单位）。键是 `size`，或 `size/quality`。
    #[serde(default)]
    amounts: BTreeMap<String, u64>,
    /// 该供给的封顶保底值：档位查不到时用它。
    #[serde(default)]
    cap_microusd: Option<u64>,
}

/// 一条保底表档位键拆开后的两维：档位值 + 可选的质量值。
struct FloorTier<'a> {
    /// 规范化之后的档位键（`size` 那一维）。
    size: &'a str,
    quality: Option<&'a str>,
}

impl FloorTable {
    /// 从发布的 JSON 读一张保底表。
    ///
    /// 档位键按尺寸取值**规范化**（`2k` 与 `2K` 是同一个档），所以管理员怎么写法都不影响查表；
    /// 规范化后撞到同一个档位就拒绝——同一档两个保底额，查出来的数就不确定了。
    ///
    /// 形状：`{"amounts": {"1K": 160000, "2K/high": 250000}, "cap_microusd": 300000}`。
    pub fn from_json(value: &Value) -> Result<Self, String> {
        let table: Self = serde_json::from_value(value.clone())
            .map_err(|error| format!("the floor table is malformed: {error}"))?;
        let mut normalized: BTreeMap<String, u64> = BTreeMap::new();
        for (key, amount) in &table.amounts {
            let tier = split_floor_key(key)?;
            let size = normalized_size(tier.size);
            let canonical = match tier.quality {
                Some(quality) => format!("{size}/{quality}"),
                None => size,
            };
            if normalized.insert(canonical.clone(), *amount).is_some() {
                return Err(format!("the floor table declares {canonical} twice"));
            }
        }
        Ok(Self {
            amounts: normalized,
            cap_microusd: table.cap_microusd,
        })
    }

    /// 表里有没有任何档位（只有封顶值、或什么都没有，都是合法形态）。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.amounts.is_empty() && self.cap_microusd.is_none()
    }

    /// 按**归好的档位**与这次请求的 `quality` 查保底额与它的来源。
    ///
    /// `tier` 由 [`resolve_size_tier`] 给出（像素型 `size` 已经归到档位、`auto` 已经取默认档）；
    /// 顺序即设计的回落链：
    /// ① 该档位查表（先精确到 `(档位, quality)`，再退到该档位"任意质量"的那一格——留空即按
    ///    `size` 档），来源取归位时带来的那个（查表得来 / `auto` 的默认档）；
    /// ② 档位归不出来、或该档位在表里没有 ⇒ 该供给的**封顶保底值**；
    /// ③ 连封顶值也没有 ⇒ `None`，由调用方回落到平台兜底数。
    ///
    /// `quality` 只影响同一档位内取哪一格：表里没为这个质量单列时，取该档位"任意质量"的那一格；
    /// 该档位连"任意质量"都没有，就算没查到这一档——不拿别的档位顶上。
    #[must_use]
    pub fn lookup(
        &self,
        tier: Option<&ResolvedTier>,
        quality: Option<&str>,
    ) -> Option<(u64, HoldSource)> {
        let quality = quality.map(str::trim).filter(|value| !value.is_empty());
        if let Some(resolved) = tier
            && let Some(amount) = self.amount_at(&resolved.tier, quality)
        {
            return Some((amount, resolved.source));
        }
        self.cap_microusd
            .map(|amount| (amount, HoldSource::SupplyCap))
    }

    /// 某个档位下的保底额：先精确到质量，再退到该档位"任意质量"的那一格。
    fn amount_at(&self, tier: &str, quality: Option<&str>) -> Option<u64> {
        if let Some(quality) = quality
            && let Some(amount) = self.amounts.get(&format!("{tier}/{quality}"))
        {
            return Some(*amount);
        }
        self.amounts.get(tier).copied()
    }
}

/// 拆开一个保底表键：`2K` → 档位 `2K`；`2K/high` → 档位 `2K` + 质量 `high`。
fn split_floor_key(key: &str) -> Result<FloorTier<'_>, String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("the floor table has an empty tier key".to_owned());
    }
    match key.split_once('/') {
        Some((size, quality)) => {
            let size = size.trim();
            let quality = quality.trim();
            if size.is_empty() || quality.is_empty() {
                return Err(format!(
                    "the floor table key {key} is not a size/quality pair"
                ));
            }
            Ok(FloorTier {
                size,
                quality: Some(quality),
            })
        }
        None => Ok(FloorTier {
            size: key,
            quality: None,
        }),
    }
}

/// 档位键的规范化：能解析成尺寸取值的按它的规范写法（`2k` → `2K`），否则原样保留。
///
/// 请求侧的 `size` 走同一条规范化（`resolve_size_tier` 归出来的档位就是规范写法），所以
/// "管理员怎么写"与"调用方怎么给"在查表这一步对齐。
fn normalized_size(value: &str) -> String {
    match SizeSpec::parse(value) {
        Ok(spec) => spec.to_string(),
        Err(_) => value.trim().to_owned(),
    }
}

/// 受理时选中并**随 Job 冻结**的那条候选：售价按它算。
///
/// 候选的定义本来就在 Job 上（`offering_id` / `channel_id` 两列），但"这一笔的售价是按哪条
/// 候选发布的费率算的"必须能在**快照里**读出来：快照是结算唯一读的东西，事后翻快照时不该
/// 再去 JOIN 一遍 Job 列才知道自己算的是谁。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HitCandidate {
    pub offering_id: OfferingId,
    pub channel_id: ChannelId,
    /// 渠道类别（例如 AIHubMix / APIMart）。
    pub provider_kind: String,
}

/// 受理时随 Job 冻结的**定价**（对客平面 CNY，外加折算用的汇率）。
///
/// 这些字段**一起有、一起没有**：全都随修订发布、受理时随快照冻结；都没有 = **旧口径**
/// （这次受理的修订没有定价，或历史 Job），对客扣费按已发布费率、预授权回落到平台兜底数，
/// 与今天逐位相同。
///
/// 其中 `hold_microusd` / `hold_source` / `fx_rate` 三项**依赖这次请求**（保底额按请求的
/// `(size, quality)` 查表、汇率按受理时刻取那一行），发布侧算不出来：仓库读候选时它们是空的，
/// 由受理用例算定后填上。它们从不到库里"半填"——写 Job 的只有受理这一条路，写之前已经填好。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceSnapshot {
    /// 这条供给的 Price Plan（**该渠道按 token 计量量计价时的那份四档费率**）。
    ///
    /// 渠道的计价形态不是 token 计量量时**没有这一项**：那份费率是 `token_rates` 这一形态的参数，
    /// 不是所有供给都必须有的东西。历史快照缺这个键时读成 `None`。
    #[serde(default)]
    pub price_plan_id: Option<PricePlanId>,
    /// 该渠道的**成本费率**（四档，币种见 [`PriceRates::currency`]）；没有 Price Plan 时为 `None`。
    ///
    /// 它**不是对客定价口径**：对客售价走 [`Self::consumer_rates_cny`]，两者是两套数据，
    /// 只是今天 Price Plan 暂时兼作渠道成本费率。所以成本自算一律经 [`Self::cost_rates`]
    /// 取费率，**不复用对客扣费算出来的那个金额**——那会让成本跟着售价漂移。
    #[serde(default)]
    pub rates: Option<PriceRates>,
    /// 这条供给的**计价形态**（渠道事实，随修订发布、随 Job 冻结）。
    ///
    /// 它决定上游没给金额时成本怎么算（见 [`Self::cost_unit_price_microusd`]）。
    /// 历史快照缺这个键时读成 `token_rates`：那一天只有这一种形态。
    #[serde(default = "legacy_pricing_formula")]
    pub formula: PricingFormula,
    /// `per_image` / `per_call` 的**单价**（成本平面微单位，币种见 [`Self::cost_currency`]）。
    ///
    /// 渠道按张 / 按次计价时它是成本自算唯一的参数；另外两种形态为 `None`。
    #[serde(default)]
    pub cost_unit_price_microusd: Option<u64>,
    /// 命中候选的**对客计价形态**（运营按候选选，随修订发布、随 Job 冻结）。
    ///
    /// 它与**成本**形态 [`Self::formula`] 相互独立，决定对客怎么收钱。历史快照缺这个键时读成
    /// **等于 `formula`**（等同旧口径）——取值经 [`Self::consumer_formula`]。
    #[serde(default)]
    pub consumer_formula: Option<PricingFormula>,
    pub captured_at: DateTime<Utc>,
    /// 受理时命中并冻结的那条候选（售价按它算）。
    #[serde(default)]
    pub hit_candidate: Option<HitCandidate>,
    /// 命中候选的**对客四档 CNY 费率向量**（对客选 `token_rates` 时的售价）：实收依据。
    ///
    /// 对客选 `upstream_declared` 时它是 `None`——那时对客价按上游声明金额 × 倍率 × 折算率算
    /// （见 [`Self::charge_microusd`]）。对客选 token 四档却没有它时走**旧口径**：Price Plan 的
    /// 那份费率兼作对客费率。
    #[serde(default)]
    pub consumer_rates_cny: Option<ConsumerRatesCny>,
    /// 档位价目表（CNY）：**只作定价参考与展示**，不参与预授权、也不参与结算。
    #[serde(default)]
    pub tier_prices: Option<Value>,
    /// 该供给的**保底表**（CNY，原值冻结）：受理时算预授权的查表依据。
    #[serde(default)]
    pub floor_amounts: Option<Value>,
    /// 本次请求的**保底额**（CNY 微单位）：受理时算定并冻结，**不由售价派生**。
    #[serde(default)]
    pub hold_microusd: Option<u64>,
    /// 保底额来源（事后可辨"这次为什么冻这么多"）。
    #[serde(default)]
    pub hold_source: Option<HoldSource>,
    /// 该候选的成本来源口径（两态，按候选发布）。
    #[serde(default)]
    pub cost_basis: Option<CostBasis>,
    /// 该候选的渠道成本（**原币种**微单位）：**只作定价参考，不是售价的被乘数**。
    #[serde(default)]
    pub reference_cost_microusd: Option<u64>,
    /// 该候选的成本币种（不假定 USD）。
    #[serde(default)]
    pub cost_currency: Option<String>,
    /// 加价系数（基点，每个网关模型一个）：参与设定该候选的对客费率向量。
    #[serde(default)]
    pub markup_bps: Option<i32>,
    /// 该币种 → CNY 的折算率：受理时取"受理时刻生效的那一行"并**原值快照**，受理后不再换算。
    #[serde(default)]
    pub fx_rate: Option<FxRate>,
}

impl PriceSnapshot {
    /// 这条渠道的**成本费率**（四档，币种见 [`PriceRates::currency`]）。
    ///
    /// 它**不是对客定价口径**：对客售价走对客自己的 CNY 费率向量，两者是两套数据，只是今天
    /// Price Plan 暂时兼作渠道成本费率。所以成本自算一律经这个访问点取费率，**不复用对客扣费
    /// 算出来的那个金额**——那会让成本跟着售价漂移，而上游成本与售价本来就是两个量。
    /// 没有 Price Plan（渠道不按 token 计量量计价）时为 `None`：那时成本的算法由
    /// [`Self::formula`] 决定，不从这里取费率。    #[must_use]
    pub fn cost_rates(&self) -> Option<&PriceRates> {
        self.rates.as_ref()
    }

    /// 这条渠道**声明的成本币种**。
    ///
    /// 成本平面记的是渠道自己的钱，所以币种按渠道声明取（不假定 USD）；对客平面只有 CNY，
    /// 不走这个访问点。受理时冻结的这份声明是成本侧币种**唯一**的取值点：Driver 拿到的金额
    /// 本身不带币种，它只能把这份声明带回来。声明与 Price Plan 的币种在实践中同源；两份都在时
    /// 发布期要求它们一致，所以这里取到的仍是一个答案。两样都没有 = 这条快照没带成本币种
    /// （旧修订受理出的历史 Job），由调用方按"说不清是哪个币种"处理。
    #[must_use]
    pub fn cost_currency(&self) -> Option<&str> {
        self.cost_currency
            .as_deref()
            .or_else(|| self.rates.as_ref().map(|rates| rates.currency.as_str()))
    }

    /// 这条快照的**对客计价形态**：运营选了就用它；历史快照缺这个键时读成**等于成本形态**
    /// [`Self::formula`]（旧口径）。
    #[must_use]
    pub fn consumer_formula(&self) -> PricingFormula {
        self.consumer_formula.unwrap_or(self.formula)
    }

    /// 对客实收（对客平面，CNY）。
    ///
    /// **对客价按对客形态取**：`token_rates` 读 [`Self::consumer_rates_cny`] × 实际用量；
    /// `upstream_declared` 按声明金额 × 倍率 × 折算率（[`Self::marked_up_cny_microusd`]）。**成本**
    /// 按 [`Self::formula`] 与 [`Self::cost_rates`] 另走一路，两者互不从属。
    ///
    /// 按 token 计量量的候选**不现算**：它的对客价是随修订发布的那份四档 CNY 向量
    /// （[`Self::consumer_rates_cny`]，运营按同一条乘法推导、也可以直接录入），受理时随快照冻结。
    /// 没有它时走**旧口径**——Price Plan 的那份费率兼作对客费率，历史 Job 与"迁移后仍生效但
    /// 没有定价的旧修订"都走这条。
    ///
    /// 算不出来时**不按 0 结算**（0 元等于白送，还会在账上留下一条"收过钱"的记录），也不拿别的
    /// 数顶替（成本只进毛利口径，把它当成对客金额就是把**成本当售价**卖出去）：返回错误，由调用方
    /// 按**平台侧故障**处置（今天那条路是"结算失败进对账"）。算不出来有几种：没有对客费率向量、
    /// 没有倍率、没有折算率、上游没声明金额。
    ///
    /// 成本自算走 [`Self::cost_rates`] 与 [`Self::formula`]，与这里分成两个入口。
    pub fn charge_microusd(&self, facts: ChargeFacts<'_>) -> Result<u64, DomainError> {
        match self.consumer_formula() {
            PricingFormula::TokenRates => match (&self.consumer_rates_cny, &self.rates) {
                (Some(rates), _) => rates.amount_microusd(evidence(facts.usage)?),
                (None, Some(rates)) => rates.amount_microusd(evidence(facts.usage)?),
                (None, None) => Err(DomainError::MissingConsumerRate),
            },
            PricingFormula::UpstreamDeclared => {
                let amount = facts
                    .declared_cost_microusd
                    .ok_or(DomainError::MissingConsumerRate)?;
                self.marked_up_cny_microusd(amount)
            }
            // 对客形态只有 token_rates / upstream_declared 两种；按张 / 按次是对客不提供的取值
            // （它们是成本侧的事实），发布期已拒。这条分支只兜住存量或被绕过的快照——那时这条候选
            // 没有对客计费基准，按平台侧故障处理。
            PricingFormula::PerImage | PricingFormula::PerCall => {
                Err(DomainError::MissingConsumerRate)
            }
        }
    }

    /// 成本金额 × 倍率 × 折算率 → **对客金额**（CNY 微单位），向上取整。
    ///
    /// **只有这一条乘法**：倍率是 `1 + markup_bps / 10000`（基点由运营按网关模型录入、随修订
    /// 发布、随快照冻结），折算率按该供给声明的成本币种取受理时冻结的那一行。同币种的那一行率恒为
    /// 1（例如 CNY → CNY），所以"同币种不产生折算"是这条乘法的结果，不在这里按币种名分叉——
    /// 把某个币种写死，接一个同币种的新渠道就得再改一次代码。
    ///
    /// 一次除、一次向上取整：分两步取整会让"同一个成本在两处算出不同的对客价"，而钱差一个微单位
    /// 就是对不上账。倍率与折算率都是发布数据/汇率表里的值，这里没有默认值——缺任何一样就是算不出
    /// 对客价。
    fn marked_up_cny_microusd(&self, amount_microusd: u64) -> Result<u64, DomainError> {
        let markup_bps = self.markup_bps.ok_or(DomainError::MissingConsumerRate)?;
        let fx_rate = self
            .fx_rate
            .as_ref()
            .ok_or(DomainError::MissingConsumerRate)?;
        // 负倍率是平台倒贴，发布期与库层都拦着；这里照"没有可用的倍率"处理，不把它算成一个数。
        let coefficient =
            u128::from(u32::try_from(markup_bps).map_err(|_| DomainError::MissingConsumerRate)?)
                + 10_000;
        let numerator = u128::from(amount_microusd)
            .checked_mul(coefficient)
            .and_then(|value| value.checked_mul(u128::from(fx_rate.rate_micros)))
            .ok_or(DomainError::ArithmeticOverflow)?;
        let denominator = 10_000 * u128::from(FX_RATE_DENOMINATOR);
        u64::try_from(numerator.div_ceil(denominator)).map_err(|_| DomainError::ArithmeticOverflow)
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
/// 与 [`PublishedOffering`] 的关系：字段完全一致，只多 `routing_priority` 与 `weight` 两个
/// **只在选路用**的发布字段。`PublishedOffering` 表示**受理时被选中并固化进 Job 的那一份**；
/// 本类型表示**发布物中的候选**。
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
    /// 选择顺序：数字小者优先。它**缺省等于候选在发布数组里的下标**（今天的老素材就是这个口径），
    /// 也可以由发布者显式给出——显式给值是为了让多条候选落在同一档。
    ///
    /// 它是**档位**：受理时按它升序找到第一个至少有一条合格候选的档，选中就在这一档里发生。
    pub routing_priority: i32,
    /// **档位内的分流比**：正整数，随候选发布，默认 `1`。
    ///
    /// 它只在**同一档内**起作用：同一档有多条合格候选时按权重分摊。它**不改变档位顺序**，
    /// 也不看价格、健康度或延迟——它是发布者给出的分流比，与 `routing_priority` 同为发布数据，
    /// 不是核心服务内置的择优规则。
    ///
    /// 反序列化缺省 `1`：这个字段是后加的，早于它落库的发布快照里没有这一项。快照今天只写不读，
    /// 但缺省值让"将来真要读旧快照"时不会因为少一个字段就整份读不出来——那时该读出的语义正是
    /// "权重 1"（今天唯一存在的取值）。
    #[serde(default = "default_routing_weight")]
    pub weight: u32,
}

/// 权重在**缺省**时的取值：`1`。`u32` 的 `Default` 是 0，而 0 不是合法的权重。
fn default_routing_weight() -> u32 {
    1
}

impl OfferingCandidate {
    /// 选中后固化进 Job 的形态（丢掉仅发布侧需要的 `routing_priority` 与 `weight`）。
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
    /// 模型类型（`image` / `video` / `chat`）：随发布引用的 Vendor Model 冻结，决定用量单位。
    pub model_type: String,
    /// 该模型的调用方合同：**发布的那一份**，客户端据此建表单。
    pub capability_schema: Value,
    /// 该模型当前文档的**不透明版本标识**：目录用它拼 `documentation_url`（Spec 0008 §2）。
    pub documentation_version: String,
    /// 该条目当前发布的生效时间：目录用它给标准字段 `created`（Spec 0009 §2）。
    pub published_at: DateTime<Utc>,
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

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DomainError {
    #[error("invalid job transition from {from} to {to}")]
    InvalidStateTransition { from: JobState, to: JobState },
    #[error("invalid execution stage transition from {from} to {to}")]
    InvalidExecutionStageTransition {
        from: ExecutionStage,
        to: ExecutionStage,
    },
    #[error("provider usage fields are inconsistent")]
    InconsistentUsage,
    #[error("arithmetic overflow")]
    ArithmeticOverflow,
    /// 这条供给**没有对客计费基准**：既没有对客费率向量，也算不出对客价（缺倍率、缺折算率、
    /// 上游没声明金额）；对客形态取了不提供的取值（按张 / 按次）时同样没有基准。
    /// **不按 0 结算**——0 元等于白送。
    #[error("no consumer rate basis to charge this supply with")]
    MissingConsumerRate,
    /// 按 token 计价的候选拿到一个没有 token 分项的成功件：算不出该收多少钱。
    /// **不按 0 结算**——不能拿缺证据当免费。
    #[error("this offering prices by token rates, but the result carries no metering evidence")]
    MissingMeteringEvidence,
}

#[cfg(test)]
mod tests;
