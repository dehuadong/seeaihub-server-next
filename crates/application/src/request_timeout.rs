//! 一次上游生成调用的超时，以及它上下两条链的取值口径。
//!
//! **为什么超时按本次请求算**：对客是同步接口，而 AIHubMix 这类渠道**只有同步路才给计量**
//! （异步任务面不回 token、不回金额），所以一次请求里 `n` 张图就是**一次上游调用**，耗时随
//! `n` 增长。固定超时在 `n` 大时会把还在生成的上游调用掐断：上游照样算钱，我们拿不到结果——
//! 消费者拿到 504、钱照扣。超时因此按本次请求要在上游生成的张数算：
//!
//! ```text
//! 上游超时 = 基础 + max(0, n − 基础含张数) × 每张预算
//! ```
//!
//! 基础那一段本身就含**前面几张**：出图不是纯线性，头几张的排队、建连与参考图上传都摊在基础里，
//! 所以 1 到基础含张数这几档是同一个值。三项都是**运维取值**（基础、基础含张数、每张预算），没有
//! 一项是产品档位：同一个 `n` 在不同模型上的出图耗时差得远，写死在代码里就只能靠改代码调。
//!
//! **两条链**：按**合同声明允许的最大 `n`** 算出来的上限，要被上下两条各夹一次，两条都要覆盖得
//! 住它，否则它被掐断时那一边还在等一个永远不会来的结果。
//!
//! ```text
//! 上游超时上限 ≤ 对客同步等待窗口
//! ```
//!
//! **对客同步等待窗口 ≥ 上限**：窗口先到期，消费者拿到 504，而上游还在生成、照样计费——就是上面
//! 那个最坏结果。比较的是**上限**，即"允许的最大 `n` 下算出来的那个值"：只保证某个小 `n`
//! 够用不算够，能发出的最大请求必须落在链内。
//!
//! 最大输出张数 **来自合同自己声明的取值面**（`capability_schema.properties.n.maximum`），不是代码
//! 里写死的数，也不是参考图上限（`restrictions.max_images` 是**输入**参考图的张数，是另一回事）：
//! 合同说能要到 `n` 张，超时链就得覆盖 `n` 张。一次发布的合同对全平台生效，所以启动时取的是
//! **所有在效合同里最大的那个 `n`**。

use std::{env, time::Duration};

use serde_json::Value;

use crate::ApplicationError;

/// 上游调用的**基础超时**（秒）：含基础张数在内、与张数无关的那一段（建连、参考图上传、上游
/// 排队与结算尾）。运维取值。
pub const DEFAULT_BASE_SECONDS: u64 = 180;

/// 基础那一段**含几张**：1 到这几张都按基础超时算。运维取值。
pub const DEFAULT_INCLUDED_IMAGES: u64 = 4;

/// **每张预算**（秒）：超过基础含张数之后，每多要一张加多少。运维取值；它按张摊，不是总时长。
pub const DEFAULT_PER_IMAGE_SECONDS: u64 = 30;

/// 对客窗口要比上游超时多出来的那一点（秒）：受理、轮询间隔与结算尾。
pub const SYNC_WAIT_OVERHEAD_SECONDS: u64 = 30;

/// 一条在效合同都没有时认的输出张数上限。
///
/// 这是**兜底**，不是取值来源：有在效合同时一律用合同声明的最大值（见
/// [`crate::declared_output_images`]）。兜底取 10，是因为当前发布面里 `n` 声明的上限就是 10
/// （`config/bootstrap` 的 `capability_schema.properties.n.maximum`）；等第一个合同发布进来，
/// 这个数就不再参与。
pub const NO_CONTRACT_MAX_OUTPUT_IMAGES: u64 = 10;

/// 一次请求要在上游生成的张数：取冻结在 Job 上的那份 `n`。
///
/// 缺 `n`、不是整数、小于 `1` 都按 1 算——上游的 `n` 默认就是 1，这些情形这一次请求都只生成
/// 一张，超时按一张算才是对的（按更大的数算只是把窗口放长，反而掩盖"参数没落下去"）。
#[must_use]
pub fn requested_image_count(native_parameters: &Value) -> u64 {
    native_parameters
        .get("n")
        .and_then(Value::as_u64)
        .filter(|count| *count > 0)
        .unwrap_or(1)
}

/// 按本次请求的张数算上游超时：`基础 + max(0, n − 基础含张数) × 每张预算`。
#[must_use]
pub fn upstream_timeout(
    base: Duration,
    included_images: u64,
    per_image: Duration,
    images: u64,
) -> Duration {
    let extra = images.saturating_sub(included_images);
    base + per_image.saturating_mul(u32::try_from(extra).unwrap_or(u32::MAX))
}

/// 一次上游调用的超时取值，以及它上下两条链的取值。
///
/// `provider_timeout` 是**上限**：算出来的值再大也不越过它。启动时校验它不小于按最大 `n` 算出来
/// 的值（见 [`Self::validate`]）——不然上限自己就落在"最大 `n` 会超时"的位置上。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestTimeoutPolicy {
    /// 基础超时（秒）。
    pub base: Duration,
    /// 基础含几张。
    pub included_images: u64,
    /// 每张预算（秒）。
    pub per_image: Duration,
    /// 上游调用的上限（秒）：算出来的值封顶在这里。
    pub provider_timeout: Duration,
    /// 对客同步等待窗口：必须覆盖上游调用。
    pub sync_wait: Duration,
    /// **输出张数**上限：对客参数 `n` 在合同里声明的最大值。
    ///
    /// 它是**输出**张数的上限，与 `restrictions.max_images`（**输入参考图**上限）无关：后者说的是
    /// 一次能带几张图进去，前者说的一次能要几张图出来。超时随输出张数走，所以链上比的是这个。
    pub max_output_images: u64,
}

impl RequestTimeoutPolicy {
    /// 允许的最大**输出张数**下算出来的上游超时。两条链校验比的就是它。
    #[must_use]
    pub fn max_upstream_timeout(&self) -> Duration {
        upstream_timeout(
            self.base,
            self.included_images,
            self.per_image,
            self.max_output_images,
        )
    }

    /// 本次请求要用的上游超时：按张数算，再封顶在上限。
    ///
    /// 封顶而不是"超了就报错"：上限就是给运维挡最坏情况的那个数，超过它的请求宁可等它到上限
    /// 再由超时按"结果不明"进对账，也不要在这里改请求的形状。
    #[must_use]
    pub fn upstream_timeout_for(&self, images: u64) -> Duration {
        upstream_timeout(self.base, self.included_images, self.per_image, images)
            .min(self.provider_timeout)
    }

    /// 按上限（最大 `n`）算出来的对客窗口取值。
    ///
    /// 窗口默认就等于上限加 [`SYNC_WAIT_OVERHEAD_SECONDS`]：窗口比上游超时短一刻，上游那一次
    /// 调用就可能在生成中途被放弃——那正是要避免的最坏结果。`GENERATION_SYNC_WAIT_SECONDS`
    /// 配了别的值时以配置为准。
    #[must_use]
    pub fn default_sync_wait(provider_timeout: Duration) -> Duration {
        provider_timeout + Duration::from_secs(SYNC_WAIT_OVERHEAD_SECONDS)
    }

    /// 校验超时链。任何一条不满足都返回点名到具体那条链与两边当前值的配置错误。
    ///
    /// 输出张数上限在合同里，环境变量读不到它，所以这条约束只能在连库之后、进程起来之前判——
    /// 判不过就不启动。比较的先后顺序就是链自下而上的顺序，报错先报最下面那条：先修根因，
    /// 一次改一层；上限比"最大输出张数下的按请求上限"小是根因，上限一旦成立窗口那条比较才有
    /// 意义。
    pub fn validate(&self) -> Result<(), ApplicationError> {
        let ceiling = self.provider_timeout.as_secs();
        let max_upstream = self.max_upstream_timeout().as_secs();
        if ceiling < max_upstream {
            return Err(ApplicationError::Configuration(format!(
                "PROVIDER_TIMEOUT_SECONDS ({ceiling}s) is below the upstream timeout for the \
                 largest output image count declared by an active contract (n = {}: base {}s + \
                 max(0, n - {}) x per-image {}s = {max_upstream}s): a request at that n would be \
                 cut off mid-generation, which still costs money at the upstream while the caller \
                 gets a timeout",
                self.max_output_images,
                self.base.as_secs(),
                self.included_images,
                self.per_image.as_secs()
            )));
        }
        let sync_wait = self.sync_wait.as_secs();
        if self.sync_wait < self.provider_timeout {
            return Err(ApplicationError::Configuration(format!(
                "GENERATION_SYNC_WAIT_SECONDS ({sync_wait}s) is below PROVIDER_TIMEOUT_SECONDS \
                 ({ceiling}s): the caller would get a timeout while the upstream call is still \
                 generating, and that call is billed whether or not its result is collected"
            )));
        }
        Ok(())
    }

    /// 从环境变量读整条链，**输出张数**上限取合同声明里的那份（见
    /// [`crate::declared_output_images`]）。API 与对账 Worker 读的是同一组变量：**窗口在上限
    /// 之上**这条链横跨两个进程，各读各的、各校验各的，两边才看得到对方的值。
    ///
    /// 五个变量，都是运维取值：
    /// - `PROVIDER_TIMEOUT_BASE_SECONDS`（默认 [`DEFAULT_BASE_SECONDS`]）
    /// - `PROVIDER_TIMEOUT_INCLUDED_IMAGES`（默认 [`DEFAULT_INCLUDED_IMAGES`]）
    /// - `PROVIDER_TIMEOUT_PER_IMAGE_SECONDS`（默认 [`DEFAULT_PER_IMAGE_SECONDS`]）
    /// - `PROVIDER_TIMEOUT_SECONDS`（默认按最大输出张数算出来的值——默认值下最大请求正好不超时）
    /// - `GENERATION_SYNC_WAIT_SECONDS`（默认 `PROVIDER_TIMEOUT_SECONDS +`
    ///   [`SYNC_WAIT_OVERHEAD_SECONDS`]）
    pub fn from_env(max_output_images: u64) -> Result<Self, ApplicationError> {
        let base = seconds_env("PROVIDER_TIMEOUT_BASE_SECONDS", DEFAULT_BASE_SECONDS)?;
        let included_images =
            seconds_env("PROVIDER_TIMEOUT_INCLUDED_IMAGES", DEFAULT_INCLUDED_IMAGES)?.as_secs();
        let per_image = seconds_env(
            "PROVIDER_TIMEOUT_PER_IMAGE_SECONDS",
            DEFAULT_PER_IMAGE_SECONDS,
        )?;
        let computed = upstream_timeout(base, included_images, per_image, max_output_images);
        let provider_timeout = seconds_env("PROVIDER_TIMEOUT_SECONDS", computed.as_secs())?;
        let default_sync_wait = Self::default_sync_wait(provider_timeout).as_secs();
        let sync_wait = seconds_env("GENERATION_SYNC_WAIT_SECONDS", default_sync_wait)?;
        Ok(Self {
            base,
            included_images,
            per_image,
            provider_timeout,
            sync_wait,
            max_output_images,
        })
    }
}

fn seconds_env(name: &str, default: u64) -> Result<Duration, ApplicationError> {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => value
            .trim()
            .parse::<u64>()
            .map(Duration::from_secs)
            .map_err(|_| {
                ApplicationError::Configuration(format!(
                    "{name} must be an integer number of seconds"
                ))
            }),
        _ => Ok(Duration::from_secs(default)),
    }
}

#[cfg(test)]
mod tests;
