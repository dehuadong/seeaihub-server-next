use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
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
    declares_parameter, is_used_parameter_value, literal_parameter_text, wire_parameter_name,
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
    /// 成本读该渠道的成本费率（今天价格计划表暂时兼作后者）。算式只有一份，免得两条路各写一遍、
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
    pub price_plan_id: PricePlanId,
    /// 该渠道的**成本费率**（四档，币种见 [`PriceRates::currency`]）。
    ///
    /// 它**不是对客定价口径**：对客售价走 [`Self::consumer_rates_cny`]，两者是两套数据，
    /// 只是今天价格计划表暂时兼作渠道成本费率。所以成本自算一律经 [`Self::cost_rates`]
    /// 取费率，**不复用对客扣费算出来的那个金额**——那会让成本跟着售价漂移。
    pub rates: PriceRates,
    pub captured_at: DateTime<Utc>,
    /// 受理时命中并冻结的那条候选（售价按它算）。
    #[serde(default)]
    pub hit_candidate: Option<HitCandidate>,
    /// 命中候选的**对客四档 CNY 费率向量**：实收依据。缺它 = 旧口径。
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
    /// 价格计划表暂时兼作渠道成本费率。所以成本自算一律经这个访问点取费率，**不复用对客扣费
    /// 算出来的那个金额**——那会让成本跟着售价漂移，而上游成本与售价本来就是两个量。
    #[must_use]
    pub fn cost_rates(&self) -> &PriceRates {
        &self.rates
    }

    /// 这条渠道**声明的成本币种**。
    ///
    /// 成本平面记的是渠道自己的钱，所以币种按渠道声明取（不假定 USD）；对客平面只有 CNY，
    /// 不走这个访问点。受理时冻结的这份声明是成本侧币种**唯一**的取值点：Driver 拿到的金额
    /// 本身不带币种，它只能把这份声明带回来。
    #[must_use]
    pub fn cost_currency(&self) -> &str {
        &self.rates.currency
    }

    /// 对客实收（对客平面，CNY）。
    ///
    /// 有定价读**命中候选的对客费率向量**（售价按候选发布、随快照冻结）；没有定价读已发布
    /// 费率——那是旧口径，与今天逐位相同（历史 Job 与"迁移后仍生效但没有定价的旧修订"都走
    /// 这条路）。成本自算走 [`Self::cost_rates`]，与这里分成两个入口。
    pub fn charge_microusd(&self, usage: &TokenUsage) -> Result<u64, DomainError> {
        match &self.consumer_rates_cny {
            Some(rates) => rates.amount_microusd(usage),
            None => self.rates.amount_microusd(usage),
        }
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
        let snapshot = snapshot_with_rates("USD", 5_000_000, 8_000_000, 10_000_000, 30_000_000);
        assert_eq!(snapshot.charge_microusd(&usage()), Ok(14_207));
    }

    /// 成本自算与对客扣费是**两个口径**：算式同一个，读的费率各自一份。
    ///
    /// 今天价格计划表暂时兼作渠道成本费率，两份费率恰好是同一张表，所以两条路算出来的数
    /// 相同；用例把两份费率**人为设成不同的值**，钉住"成本读成本费率、扣费读对客费率"——
    /// 对客费率将来拆成自己的 CNY 向量时，成本不能跟着售价漂移。
    #[test]
    fn computed_cost_reads_the_channel_cost_rates_not_the_consumer_charge() {
        let cost_snapshot =
            snapshot_with_rates("USD", 5_000_000, 8_000_000, 10_000_000, 30_000_000);
        let consumer_snapshot =
            snapshot_with_rates("CNY", 7_000_000, 9_000_000, 11_000_000, 40_000_000);

        let cost = cost_snapshot.cost_rates().amount_microusd(&usage());
        let charge = consumer_snapshot.charge_microusd(&usage());
        // 成本：27 文本输入 × 5 + 1024 图像输入 × 8 + 196 图像输出 × 30（每 1M）。
        assert_eq!(cost, Ok(14_207));
        // 对客：同一份用量，换成对客那份费率，金额就不一样了。
        assert_eq!(charge, Ok(17_245));
        assert_ne!(
            cost, charge,
            "两份费率不同时，成本与对客扣费必须各自算各自的，不能互相顶替"
        );
        assert_eq!(
            cost_snapshot.cost_currency(),
            "USD",
            "成本币种取渠道声明的那一份，不假定 USD、也不跟着对客 CNY 走"
        );
    }

    /// 造一份价格快照：四档费率按参数给，其余字段取与用例无关的定值。
    ///
    /// `pricing` 留空 = 旧口径（已发布费率兼作对客费率、预授权回落平台兜底数）。
    fn snapshot_with_rates(
        currency: &str,
        text_input: u64,
        image_input: u64,
        text_output: u64,
        image_output: u64,
    ) -> PriceSnapshot {
        PriceSnapshot {
            price_plan_id: PricePlanId::new(),
            rates: PriceRates {
                currency: currency.to_owned(),
                text_input_microusd_per_million: text_input,
                image_input_microusd_per_million: image_input,
                text_output_microusd_per_million: text_output,
                image_output_microusd_per_million: image_output,
            },
            captured_at: Utc::now(),
            hit_candidate: None,
            consumer_rates_cny: None,
            tier_prices: None,
            floor_amounts: None,
            hold_microusd: None,
            hold_source: None,
            cost_basis: None,
            reference_cost_microusd: None,
            cost_currency: None,
            markup_bps: None,
            fx_rate: None,
        }
    }

    #[test]
    fn rejects_inconsistent_usage() {
        let mut invalid = usage();
        invalid.total_tokens = 1;
        assert_eq!(invalid.validate(), Err(DomainError::InconsistentUsage));
    }

    /// 成本来源的落库字符串是**库层 CHECK 的取值集合**，读写必须自洽：
    /// 落下去的值读不回来，等于把事实写成了一次性写入。
    #[test]
    fn provider_cost_sources_round_trip_through_their_stored_form() {
        for source in [
            ProviderCostSource::Computed,
            ProviderCostSource::Declared,
            ProviderCostSource::Unavailable,
        ] {
            assert_eq!(ProviderCostSource::parse(source.as_str()), Some(source));
        }
        assert_eq!(ProviderCostSource::parse("guessed"), None);
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

    /// 一份发布在**某条供给**下的保底表：只按 `size` 填（OpenAI 系当前形态）。
    fn openai_floor_table() -> FloorTable {
        FloorTable::from_json(&serde_json::json!({
            "amounts": {"1K": 160_000, "2K": 250_000, "4K": 300_000},
            "cap_microusd": 300_000,
        }))
        .expect("the published floor table parses")
    }

    /// 归位一次并查表：把"请求的 `size` → 档位 → 保底额"这条链走完。
    fn hold_for(
        table: &FloorTable,
        size: Option<&str>,
        quality: Option<&str>,
    ) -> Option<(u64, HoldSource)> {
        let tier = resolve_size_tier(size, &SizeProfile::default());
        table.lookup(tier.as_ref(), quality)
    }

    #[test]
    fn floor_lookup_takes_the_tier_the_request_asked_for() {
        let table = openai_floor_table();
        assert_eq!(
            hold_for(&table, Some("2K"), None),
            Some((250_000, HoldSource::Tier))
        );
        // `quality` 维留空即按 `size` 档：带任意质量都查到同一个 `size` 档保底额。
        for quality in ["low", "medium", "high", "xhigh", "max", "auto"] {
            assert_eq!(
                hold_for(&table, Some("2K"), Some(quality)),
                Some((250_000, HoldSource::Tier)),
                "只填了 size 维时，quality 不该改变查到的档位"
            );
        }
        // 档位写法的大小写不影响查表：调用方给 `2k` 与管理员的 `2K` 是同一个档。
        assert_eq!(
            hold_for(&table, Some("2k"), None),
            Some((250_000, HoldSource::Tier))
        );
    }

    /// **像素型 `size` 先归到档位再查表**：用户口径是"按分辨率保底、通过 `size` 判断 1K/2K/4K"。
    ///
    /// 这条供给没发布尺寸档案（OpenAI 系当前的形态），所以走**最长边**阈值兜底。
    #[test]
    fn pixel_sizes_resolve_to_a_tier_before_the_lookup() {
        let table = openai_floor_table();
        assert_eq!(
            hold_for(&table, Some("1024x1024"), None),
            Some((160_000, HoldSource::Tier)),
            "最长边 1024 ⇒ 1K 档"
        );
        assert_eq!(
            hold_for(&table, Some("2048x2048"), None),
            Some((250_000, HoldSource::Tier)),
            "最长边 2048 ⇒ 2K 档"
        );
        assert_eq!(
            hold_for(&table, Some("3840x2160"), None),
            Some((300_000, HoldSource::Tier)),
            "最长边 3840 ⇒ 4K 档"
        );
        // 阈值看的是**最长边**：短边小不改变档位。
        assert_eq!(
            hold_for(&table, Some("1024x2048"), None),
            Some((250_000, HoldSource::Tier))
        );
        // 质量维照旧只在同一档位内取格。
        assert_eq!(
            hold_for(&table, Some("1024x1024"), Some("high")),
            Some((160_000, HoldSource::Tier))
        );
    }

    /// **该供给发布的档位像素表优先**：同一个像素在别的供给上属于别的档位，只有它自己的表算数。
    #[test]
    fn the_supply_size_profile_wins_over_the_longest_edge_fallback() {
        let table = openai_floor_table();
        // 这条供给把 1024x1024 归在 2K 档（各供给的档位像素不同）。
        let profile = SizeProfile::from_json(&serde_json::json!({
            "2K": {"1:1": "1024x1024", "16:9": "2048x1152"},
        }))
        .expect("the published size profile parses");
        let tier =
            resolve_size_tier(Some("1024x1024"), &profile).expect("the profile knows this cell");
        assert_eq!(tier.tier, "2K");
        assert_eq!(
            table.lookup(Some(&tier), None),
            Some((250_000, HoldSource::Tier)),
            "按该供给自己的档位像素表归位，最长边兜底不参与"
        );
        // 表里没有这一格 ⇒ 回到最长边兜底（最长边 1024 ⇒ 1K）。
        let tier = resolve_size_tier(Some("1024x768"), &profile).expect("pixels always resolve");
        assert_eq!(tier.tier, "1K");
        assert_eq!(
            table.lookup(Some(&tier), None),
            Some((160_000, HoldSource::Tier))
        );
    }

    #[test]
    fn auto_and_a_missing_size_take_the_default_tier() {
        let table = openai_floor_table();
        // `size = auto` ⇒ 默认档 2K（中间档：既不是最小、也不是最大）。
        assert_eq!(
            hold_for(&table, Some("auto"), None),
            Some((250_000, HoldSource::AutoTier))
        );
        // 没给 size 与 `auto` 同义：都是"调用方没钉尺寸、由模型自选"。
        assert_eq!(
            hold_for(&table, None, None),
            Some((250_000, HoldSource::AutoTier))
        );
        // `auto` 也照旧受质量维影响。
        assert_eq!(
            hold_for(&table, Some("auto"), Some("high")),
            Some((250_000, HoldSource::AutoTier))
        );
    }

    /// **空串不是"没给"**：`size` 的字面量就是调用方说的那个尺寸，平台不替它 trim、也不把
    /// "给了个空"当成"没说话"再猜一个默认档——归不出就回落该供给的封顶保底值。
    #[test]
    fn an_empty_or_padded_size_is_given_and_is_not_the_missing_size() {
        let table = openai_floor_table();
        assert_eq!(
            resolve_size_tier(Some(""), &SizeProfile::default()),
            None,
            "空串归不出档位，不落到 `auto` 的默认档上"
        );
        assert_eq!(
            hold_for(&table, Some(""), None),
            Some((300_000, HoldSource::SupplyCap)),
            "空串与'没给这个字段'必须落到不同的保底额上"
        );
        assert_eq!(
            hold_for(&table, None, None),
            Some((250_000, HoldSource::AutoTier)),
            "只有字段缺失才走 `auto` 的默认档 2K"
        );
        // 纯空白同理：它也是一个字面量，不是"没给"。
        assert_eq!(
            hold_for(&table, Some("  "), None),
            Some((300_000, HoldSource::SupplyCap))
        );
        // `auto` 只有这一个写法：别名与带空格的写法都照字面读，认不出即回落。
        for value in ["AUTO", "Auto", " auto", "auto "] {
            assert_eq!(
                hold_for(&table, Some(value), None),
                Some((300_000, HoldSource::SupplyCap)),
                "`{value}` 不是那个字面量 `auto`"
            );
        }
    }

    #[test]
    fn floor_lookup_falls_back_to_the_supply_cap_when_the_tier_cannot_be_resolved() {
        let table = openai_floor_table();
        // 比例型只说了形状、没说分辨率：归不出档位 ⇒ 回落该供给的封顶保底值。
        assert_eq!(
            hold_for(&table, Some("16:9"), None),
            Some((300_000, HoldSource::SupplyCap))
        );
        // 认不出的取值同理。
        assert_eq!(
            hold_for(&table, Some("huge"), None),
            Some((300_000, HoldSource::SupplyCap))
        );
        // 归得出档位、但表里没有这一档 ⇒ 也是封顶值。
        let partial = FloorTable::from_json(&serde_json::json!({
            "amounts": {"1K": 160_000},
            "cap_microusd": 300_000,
        }))
        .expect("the published floor table parses");
        assert_eq!(
            hold_for(&partial, Some("4K"), None),
            Some((300_000, HoldSource::SupplyCap))
        );
    }

    #[test]
    fn floor_lookup_falls_back_to_the_platform_default_when_the_supply_declares_nothing() {
        let empty = FloorTable::from_json(&serde_json::json!({})).expect("an empty table is legal");
        assert!(empty.is_empty());
        // 连封顶保底值都没有 ⇒ 查不到，由调用方回落到平台兜底数。
        assert_eq!(hold_for(&empty, Some("2K"), None), None);
        assert_eq!(hold_for(&empty, Some("auto"), None), None);
        assert_eq!(hold_for(&empty, Some("1024x1024"), None), None);
    }

    #[test]
    fn floor_lookup_prefers_the_quality_specific_entry_then_the_size_entry() {
        let table = FloorTable::from_json(&serde_json::json!({
            "amounts": {"2K": 250_000, "2K/high": 900_000, "4K": 300_000},
        }))
        .expect("the published floor table parses");
        assert_eq!(
            hold_for(&table, Some("2K"), Some("high")),
            Some((900_000, HoldSource::Tier))
        );
        // 没为这个质量单列 ⇒ 取该档位"任意质量"的那一格，不拿别的档位顶上。
        assert_eq!(
            hold_for(&table, Some("2K"), Some("low")),
            Some((250_000, HoldSource::Tier))
        );
        // 该档位连"任意质量"都没有（只有 `2K/high`）⇒ 算没查到这一档。
        let quality_only = FloorTable::from_json(&serde_json::json!({
            "amounts": {"2K/high": 900_000},
            "cap_microusd": 300_000,
        }))
        .expect("the published floor table parses");
        assert_eq!(
            hold_for(&quality_only, Some("2K"), Some("low")),
            Some((300_000, HoldSource::SupplyCap))
        );
        // `auto` 取默认档 `2K`：它只有质量单列 ⇒ 质量对得上就用它。
        assert_eq!(
            hold_for(&quality_only, Some("auto"), Some("high")),
            Some((900_000, HoldSource::AutoTier))
        );
        // 默认档没有这个质量的那一格 ⇒ 回落封顶值，不拿别的档位顶上。
        assert_eq!(
            hold_for(&quality_only, Some("auto"), Some("low")),
            Some((300_000, HoldSource::SupplyCap))
        );
    }

    #[test]
    fn a_floor_table_that_declares_one_tier_twice_is_rejected() {
        // 规范化之后撞到同一个档位：同一档两个保底额，查出来的数就不确定了。
        let error = FloorTable::from_json(&serde_json::json!({
            "amounts": {"2K": 250_000, "2k": 260_000},
        }))
        .expect_err("the same tier twice is ambiguous");
        assert!(error.contains("2K"), "{error}");
        assert!(
            FloorTable::from_json(&serde_json::json!({"amounts": {"2K/": 1}})).is_err(),
            "半截的 size/quality 键不是档位"
        );
    }

    #[test]
    fn fx_conversion_is_fixed_point_and_rounds_up() {
        let rate = FxRate {
            currency: "USD".to_owned(),
            // 1 美元 = 7.1 元人民币。
            rate_micros: 7_100_000,
            effective_at: Utc::now(),
        };
        // 11354 微美元 × 7.1 = 80613.4 微元 ⇒ 向上取整 80614。
        assert_eq!(rate.to_cny_microusd(11_354), Ok(80_614));
        assert_eq!(rate.to_cny_microusd(0), Ok(0));
        // 折算率是定点整数：1:1 的币种（例如人民币自己）折出来逐位不变。
        let identity = FxRate {
            currency: "CNY".to_owned(),
            rate_micros: FX_RATE_DENOMINATOR,
            effective_at: Utc::now(),
        };
        assert_eq!(identity.to_cny_microusd(5950), Ok(5950));
    }

    /// 有定价时实收读**对客费率向量**，不是已发布费率；没有定价时才走旧口径。
    ///
    /// 两份费率**人为设成不同的值**：拿错一份就会算出另一个数，用例因此钉得住"实收按哪份费率"。
    #[test]
    fn the_charge_reads_the_consumer_vector_when_the_snapshot_carries_pricing() {
        let mut snapshot = snapshot_with_rates("USD", 5_000_000, 8_000_000, 10_000_000, 30_000_000);
        assert_eq!(snapshot.charge_microusd(&usage()), Ok(14_207), "旧口径");
        assert_eq!(snapshot.hold_microusd, None);
        snapshot.consumer_rates_cny = Some(ConsumerRatesCny {
            text_input_micros_per_million: 7_000_000,
            image_input_micros_per_million: 9_000_000,
            text_output_micros_per_million: 11_000_000,
            image_output_micros_per_million: 40_000_000,
        });
        snapshot.cost_basis = Some(CostBasis::Declared);
        snapshot.reference_cost_microusd = Some(11_354);
        snapshot.cost_currency = Some("USD".to_owned());
        snapshot.markup_bps = Some(2_000);
        snapshot.hold_microusd = Some(250_000);
        snapshot.hold_source = Some(HoldSource::Tier);
        snapshot.fx_rate = Some(FxRate {
            currency: "USD".to_owned(),
            rate_micros: 7_100_000,
            effective_at: Utc::now(),
        });
        assert_eq!(
            snapshot.charge_microusd(&usage()),
            Ok(17_245),
            "对客费率向量"
        );
        // 成本侧不受影响：它仍读该渠道的成本费率。
        assert_eq!(snapshot.cost_rates().amount_microusd(&usage()), Ok(14_207));
        assert_eq!(snapshot.hold_microusd, Some(250_000));
        assert_eq!(
            snapshot.fx_rate.as_ref().map(|rate| rate.rate_micros),
            Some(7_100_000)
        );
    }

    /// 历史快照（没有 `consumer_rates_cny` / `hold_microusd` 这些键）必须照样读得回来。
    ///
    /// 库里的 `price_snapshot` 是 jsonb：加字段这件事只有在**旧 JSON 仍能解析**时才不破坏
    /// 已受理 Job 的结算——解析失败会让历史 Job 直接读不出来。
    #[test]
    fn a_snapshot_without_the_pricing_keys_still_parses() {
        let legacy = serde_json::json!({
            "price_plan_id": PricePlanId::new(),
            "rates": {
                "currency": "USD",
                "text_input_microusd_per_million": 5_000_000,
                "image_input_microusd_per_million": 8_000_000,
                "text_output_microusd_per_million": 10_000_000,
                "image_output_microusd_per_million": 30_000_000,
            },
            "captured_at": Utc::now(),
        });
        let snapshot: PriceSnapshot =
            serde_json::from_value(legacy).expect("a legacy snapshot must still parse");
        assert_eq!(snapshot.consumer_rates_cny, None);
        assert_eq!(snapshot.hold_microusd, None);
        assert_eq!(snapshot.fx_rate, None);
        assert_eq!(snapshot.hit_candidate, None);
        assert_eq!(snapshot.charge_microusd(&usage()), Ok(14_207));
    }

    #[test]
    fn hold_sources_and_cost_bases_round_trip_through_their_stored_form() {
        for source in [
            HoldSource::Tier,
            HoldSource::AutoTier,
            HoldSource::SupplyCap,
            HoldSource::PlatformDefault,
        ] {
            assert_eq!(HoldSource::parse(source.as_str()), Some(source));
        }
        assert_eq!(HoldSource::parse("guessed"), None);
        for basis in [CostBasis::Computed, CostBasis::Declared] {
            assert_eq!(CostBasis::parse(basis.as_str()), Some(basis));
        }
        assert_eq!(CostBasis::parse("unavailable"), None);
    }
}
