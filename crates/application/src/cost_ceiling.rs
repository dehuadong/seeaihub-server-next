//! 成本护栏：**单次请求**可能产生的上游成本上限。
//!
//! 它守的不是"客户余额够不够"——那是受理唯一的对客闸门（余额 < 保底额即 402 `insufficient_balance`），
//! 也不是调用方自报的预算，而是**平台自己的运营护栏**：一条供给按发布数据算下来，一次请求最多可能让
//! 平台付给上游多少钱。它挡的是配错的单价、合同允许的档位异常大，以及折算率变差之后已经发布出去的
//! 供给。
//!
//! **两处判，一处防误配、一处兜运行期**：
//!
//! - **发布期**（主）：一份修订里任何一条候选的**最大单次成本**超过上限，整份发布被拒，并点名
//!   是哪条候选、可能花多少。误配因此在生效之前就被挡住——那时还没有任何 Job 受影响；
//! - **受理期**（兜底）：受理时按**这一次请求**再判一次，超过即拒（平台侧故障）。它兜的是
//!   "上限被调小、或折算率变差之后，**已经发布出去的**供给"：发布期的判据是当时那个上限与当时
//!   那一份折算率，改小之后已经生效的候选不会自己重判。
//!
//! **判据是 `>`，不是 `>=`**：恰好等于上限算通过。上限说的是"最多允许多少"，等于它就没有超过它。
//!
//! 一次请求的最大成本按计价形态取：
//!
//! | 计价形态 | 单次上限 |
//! | --- | --- |
//! | `per_image` | 单价 × 张数 |
//! | `per_call` | 单价 |
//! | `token_rates` / `upstream_declared` | 发布者给的**参考成本**（`reference_cost_microusd`）——这两种形态没有"每张单价"，可用的声明数就是它 |
//!
//! 张数在发布期取**合同声明的最大 `n`**（`capability_schema.properties.n.maximum`），在受理期取
//! **本次请求的 `n`**：发布期问的是"这条供给最坏能花多少"，受理期问的是"这一次要花多少"。
//!
//! **每种形态判得到什么**（判据的边界写在这里，别把它读成"平台不会亏"）：只有 `per_image` 真的算得出
//! "最坏一次"——单价乘合同允许的最大张数；`per_call` 与请求无关，就是单价。`token_rates` 与
//! `upstream_declared` 没有每张单价，取的是发布者给的**参考成本**：它是发布者按"费率 × 参考用量"
//! 给出的**单次成本声明**，不是一个被证明的上界——真实用量更大的请求可能超过它，上游临时涨价更不会
//! 经过它（那两种形态的金额到执行时才由上游给出）。这两种形态的护栏因此只挡"参考成本本身配得离谱"。
//!
//! **成本平面与币种**：按计价形态算出来的是**该候选成本币种**的金额，判之前用那一份折算率折成
//! 人民币——上限是运营按人民币设的一个数。**算不出成本就不判**（没有参考成本、没有成本币种、
//! 该币种没有生效折算率）：当作"超了"会让旧形状的素材发不出去，当作 0 又等于静默放行。

use std::env;

use seeai_domain::{FxRate, PricingFormula};

use crate::ApplicationError;

/// 上限的默认值（人民币微单位）：10 元。
///
/// 它是**运营取值**的默认，不是产品档位：单张图的上游成本离它很远（当前发布面是 0.08 元左右），
/// 撞上它基本意味着单价配错、档位异常或上游涨价。要改的是部署期的环境变量。
pub const DEFAULT_MAX_REQUEST_COST_MICROUSD: u64 = 10_000_000;

/// 单次请求成本上限的运维取值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestCostCeiling {
    max_request_cost_microusd: u64,
}

impl RequestCostCeiling {
    /// 上限必须是正数：0 等于"一次请求都不许花"，那不是上限、是关停，而关停该走供给与渠道那条路，
    /// 不是让每个请求都撞在一个说不清的错误上。
    pub fn new(max_request_cost_microusd: u64) -> Result<Self, ApplicationError> {
        if max_request_cost_microusd == 0 {
            return Err(ApplicationError::Configuration(
                "the request cost ceiling must be positive".to_owned(),
            ));
        }
        Ok(Self {
            max_request_cost_microusd,
        })
    }

    /// 运维默认值：单次请求 10 元。
    #[must_use]
    pub fn default_ceiling() -> Self {
        Self {
            max_request_cost_microusd: DEFAULT_MAX_REQUEST_COST_MICROUSD,
        }
    }

    #[must_use]
    pub fn max_request_cost_microusd(&self) -> u64 {
        self.max_request_cost_microusd
    }

    /// 这次请求（或这条候选的最大单次成本）超上限了吗。
    ///
    /// `>` 而不是 `>=`：恰好落在上限上算通过。上限是"最多允许多少"，等于它不算超过。
    #[must_use]
    pub fn exceeded_by(&self, cost_cny_microusd: u64) -> bool {
        cost_cny_microusd > self.max_request_cost_microusd
    }

    /// 从环境变量读运维取值：`GENERATION_MAX_REQUEST_COST_MICROUSD`（人民币微单位）；没给或给空用
    /// 默认值。
    ///
    /// 与 `GENERATION_MAX_COST_MICROUSD` **不是同一个量**：那个是"连供给封顶保底值都查不到时"的
    /// 兜底**保底额**（预授权），这个是单次请求可能花掉的**成本**上限。两个名字都在部署面出现，
    /// 所以各管各的、互不推导。
    pub fn from_env() -> Result<Self, ApplicationError> {
        match env::var("GENERATION_MAX_REQUEST_COST_MICROUSD") {
            Ok(value) if !value.trim().is_empty() => {
                Self::new(value.trim().parse::<u64>().map_err(|_| {
                    ApplicationError::Configuration(
                        "GENERATION_MAX_REQUEST_COST_MICROUSD must be an integer number of \
                         microusd"
                            .to_owned(),
                    )
                })?)
            }
            _ => Ok(Self::default_ceiling()),
        }
    }
}

/// 一次请求最多可能花掉的上游成本（**该候选的成本币种**微单位）。
///
/// 两种"没有每张单价"的形态取发布者给的参考成本；`per_image` 按张数乘，`per_call` 与张数无关。
/// 参数缺失（形态与参数不配套）时返回 `None`：那是"算不出一次要花多少"，不是"花 0 元"。
#[must_use]
pub fn single_request_cost_native(
    formula: PricingFormula,
    cost_unit_price_microusd: Option<u64>,
    reference_cost_microusd: Option<u64>,
    images: u64,
) -> Option<u64> {
    match formula {
        // 饱和乘：真溢出说明这个数已经大到没有任何上限挡不住它，按最大可能值处理。
        PricingFormula::PerImage => Some(cost_unit_price_microusd?.saturating_mul(images)),
        PricingFormula::PerCall => cost_unit_price_microusd,
        PricingFormula::TokenRates | PricingFormula::UpstreamDeclared => reference_cost_microusd,
    }
}

/// 同 [`single_request_cost_native`]，再用该候选那一份折算率折成人民币微单位。
///
/// 折算率与币种对不上、或没有折算率时返回 `None`：护栏判的是人民币这个数，拿不到就判不出来。
#[must_use]
pub fn single_request_cost_cny(
    formula: PricingFormula,
    cost_unit_price_microusd: Option<u64>,
    reference_cost_microusd: Option<u64>,
    cost_currency: Option<&str>,
    fx_rate: Option<&FxRate>,
    images: u64,
) -> Option<u64> {
    let native = single_request_cost_native(
        formula,
        cost_unit_price_microusd,
        reference_cost_microusd,
        images,
    )?;
    let rate = fx_rate.filter(|rate| Some(rate.currency.as_str()) == cost_currency)?;
    rate.to_cny_microusd(native).ok()
}

#[cfg(test)]
mod tests;
