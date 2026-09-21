//! 图像尺寸参数的四型，以及它们之间的换算。
//!
//! 同一个尺寸在不同厂商、不同渠道上写着三种具体的东西：**像素**（`1024x1024`）、**宽高比**
//! （`16:9`）、**分辨率档位**（`2K`）；此外还有第四型 `auto`——它不是一个尺寸，而是"由模型按
//! 提示词自己决定最佳比例"。于是同一个字段名（`size`）在几个模型上可以有好几义，而客户端提交的
//! 是**合同**声明的那一义、渠道要的可能是另一义。这一层只做两件事：把一个取值认成四型中的一型，
//! 并按一份**发布数据**（比例 × 档位 → 像素的档案）在它们之间换算。
//!
//! 四条边界：
//!
//! 1. **档案是数据，不是代码**：任何厂商的具体档位表都不写在这里。`SizeProfile` 只把随发布
//!    携带的那张表解析成一个可查的结构；接一个新厂商只发一份新档案，不改代码。
//! 2. **判型看取值本身**：`2K` 是档位、`16:9` 是比例、`1024x1024` 是像素、`auto` 是"让模型
//!    自己决定"，靠字符串的形状判定，与它出现在哪个字段名上无关（`size`、`resolution`、
//!    `aspect_ratio` 都可能是任意一型）。因此"这个字段是哪一型"不必在合同里再声明一遍——合同
//!    本来就只声明字段与类型，而调用方提交的是取值。
//! 3. **换算不出就明确失败**：档案里没有那一格，返回错误（受理侧据此判这条候选不合格），
//!    绝不退回一个近似值、也绝不把调用方已经说过的那一型悄悄丢掉。
//! 4. **`auto` 只透传、永不换算**：它说的是"让模型自己挑"，平台替它算一个比例就是把模型的
//!    决定权拿走了，而算出来的那个尺寸与调用方要的东西毫无关系。承载面收得了它的渠道原样发
//!    出去，收不了的渠道明确失败、这条候选因此不合格。
//!
//! 可换算的三型之间只做一种换算：**（比例 + 档位）↔ 像素**。单独一个比例、单独一个档位都换算
//! 不出像素（同一档位下有很多比例，同一个比例下有很多档位），那种组合一律失败而不是被猜一个值。

use serde_json::Value;
use std::{
    collections::BTreeMap,
    fmt::{Display, Formatter},
};

/// 尺寸取值属于哪一型。映射声明里写的就是这四型的名字。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeForm {
    Pixels,
    Ratio,
    Tier,
    /// 由模型自己决定：它不是一个具体尺寸，因此没有可换算的形态。
    Auto,
}

impl SizeForm {
    /// 四型的稳定名字：声明里写 `pixels` / `ratio` / `tier` / `auto`。
    pub const ALL: [Self; 4] = [Self::Pixels, Self::Ratio, Self::Tier, Self::Auto];

    /// 声明里的名字还原成型别；不是这四型的名字则为 `None`。
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|form| form.as_str() == value)
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pixels => "pixels",
            Self::Ratio => "ratio",
            Self::Tier => "tier",
            Self::Auto => "auto",
        }
    }
}

/// 一个尺寸取值：四型中的一型。
///
/// 取值本身是字符串（`1024x1024` / `16:9` / `2K` / `auto`），`Display` 就是它的线上形态；
/// 比例与档位在这里**已经规范化**（比例约分、档位统一大写 K），因此同一个尺寸的不同写法
/// （`32:18` 与 `16:9`、`2k` 与 `2K`）在这一层之后是同一个值。`auto` 没有可规范化的内容——
/// 它本来就不是一个尺寸，线上原样就是 `auto`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SizeSpec {
    Pixels {
        width: u32,
        height: u32,
    },
    Ratio {
        ratio: String,
    },
    Tier {
        tier: String,
    },
    /// 由模型自己决定最佳比例：只原样透传，永不参与换算。
    Auto,
}

impl SizeSpec {
    /// 按取值的形状判型：`宽x高` 是像素、`a:b` 是比例、`数字K` 是档位、`auto` 是"让模型自己
    /// 决定"，其余一律报错。
    ///
    /// 宽、高、比例的两边、档位数字都必须**是正整数**：`0x1024`、`16:0`、`0K` 都不是一个尺寸。
    /// 不做 trim、不认别名：调用方写的字面量就是它说的那个尺寸，平台不替它收拾写法。
    pub fn parse(value: &str) -> Result<Self, String> {
        if value.is_empty() {
            return Err("a size must not be empty".to_owned());
        }
        if value == "auto" {
            // `auto` 是唯一一个"不是一个具体尺寸"的取值：它说的是"由模型自己决定最佳比例"，
            // 因此没有宽高、比例或档位可以规范化。
            return Ok(Self::Auto);
        }
        if let Some((width, height)) = split_pair(value, 'x').or_else(|| split_pair(value, 'X')) {
            return Ok(Self::Pixels {
                width: dimension(width, value)?,
                height: dimension(height, value)?,
            });
        }
        if let Some((width, height)) = split_pair(value, ':') {
            let (width, height) = (dimension(width, value)?, dimension(height, value)?);
            let divisor = greatest_common_divisor(width, height);
            return Ok(Self::Ratio {
                ratio: format!("{}:{}", width / divisor, height / divisor),
            });
        }
        if let Some(digits) = value.strip_suffix('K').or_else(|| value.strip_suffix('k')) {
            return Ok(Self::Tier {
                tier: format!("{}K", dimension(digits, value)?),
            });
        }
        Err(not_a_size(value))
    }

    /// 这个取值属于哪一型。
    #[must_use]
    pub fn form(&self) -> SizeForm {
        match self {
            Self::Pixels { .. } => SizeForm::Pixels,
            Self::Ratio { .. } => SizeForm::Ratio,
            Self::Tier { .. } => SizeForm::Tier,
            Self::Auto => SizeForm::Auto,
        }
    }
}

impl Display for SizeSpec {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pixels { width, height } => write!(formatter, "{width}x{height}"),
            Self::Ratio { ratio } => formatter.write_str(ratio),
            Self::Tier { tier } => formatter.write_str(tier),
            Self::Auto => formatter.write_str("auto"),
        }
    }
}

/// 比例 × 档位 → 像素的档案：随发布携带的那张表，领域只负责查它。
///
/// 形状是 `{"2K": {"1:1": "2048x2048", ...}, "3K": {...}}`，正好对应厂商文档里
/// "分辨率档位 × 宽高比 → 宽高像素值"的表格。两层键在解析时都规范化，所以档案里写 `2k`、
/// 调用方给 `2K` 也查得到同一格。
///
/// 两张表都从同一份档案走：正向查（比例 + 档位 → 像素）与反向查（像素 → 比例 + 档位）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SizeProfile {
    /// 档位 → 比例 → 像素。两层都用规范化后的键，顺序由键序决定（可复现）。
    rows: BTreeMap<String, BTreeMap<String, (u32, u32)>>,
}

impl SizeProfile {
    /// 从发布数据里解析这张表；形状不对、键不是档位/比例、值不是像素，都在这里报错。
    ///
    /// 发布期据此拒绝一份写歪的档案：档案是**声明**，声明得不成形状就不该被发布出去——
    /// 否则它只会在受理时变成"这条候选换算不出"，让调用方看到一个说不清来由的平台故障。
    pub fn from_json(value: &Value) -> Result<Self, String> {
        let rows = value.as_object().ok_or_else(|| {
            "the size profile must be an object of tier → ratio → pixels".to_owned()
        })?;
        let mut profile = Self::default();
        for (tier, row) in rows {
            let tier = profile_key(tier, SizeForm::Tier, |spec| match spec {
                SizeSpec::Tier { tier } => Some(tier.clone()),
                _ => None,
            })?;
            let row = row.as_object().ok_or_else(|| {
                format!("the size profile entry for {tier} must be an object of ratio → pixels")
            })?;
            for (ratio, pixels) in row {
                let ratio = profile_key(ratio, SizeForm::Ratio, |spec| match spec {
                    SizeSpec::Ratio { ratio } => Some(ratio.clone()),
                    _ => None,
                })?;
                let pixels = pixels.as_str().ok_or_else(|| {
                    format!("the size profile entry for {ratio} at {tier} must be a pixel string")
                })?;
                let (width, height) = match SizeSpec::parse(pixels)
                    .map_err(|reason| format!("size profile entry `{pixels}`: {reason}"))?
                {
                    SizeSpec::Pixels { width, height } => (width, height),
                    other => {
                        return Err(format!(
                            "the size profile entry for {ratio} at {tier} must be pixels, got {other}"
                        ));
                    }
                };
                if profile
                    .rows
                    .entry(tier.clone())
                    .or_default()
                    .insert(ratio.clone(), (width, height))
                    .is_some()
                {
                    return Err(format!("the size profile declares {ratio} at {tier} twice"));
                }
            }
        }
        Ok(profile)
    }

    /// 这张表是不是一格都没有。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.values().all(BTreeMap::is_empty)
    }

    /// 比例 + 档位 → 像素；档案里没有这一格则为 `None`（调用方据此判候选不合格）。
    #[must_use]
    pub fn lookup(&self, ratio: &str, tier: &str) -> Option<(u32, u32)> {
        self.rows.get(tier)?.get(ratio).copied()
    }

    /// 像素 → 比例 + 档位；档案里没有这一组像素则为 `None`。
    ///
    /// 同一组像素在表里出现多次时取**键序最靠前**的那一组：反向查必须有一个确定答案，
    /// 否则同一份请求会因档案内部的书写顺序而落成不同的比例。
    #[must_use]
    pub fn lookup_reverse(&self, width: u32, height: u32) -> Option<(String, String)> {
        for (tier, row) in &self.rows {
            for (ratio, pixels) in row {
                if *pixels == (width, height) {
                    return Some((ratio.clone(), tier.clone()));
                }
            }
        }
        None
    }
}

/// 把合同侧给出的尺寸取值换算成这条供给要的那一型。
///
/// `values` 是这次请求**实际用到**的尺寸字段解析出来的取值（按映射声明的顺序）。只有两种情形
/// 成立：
///
/// - **目标型别已经在取值里** → 原样用它（不需要档案）；但取值里多出别的型别时失败：
///   多出来的那一型会被无声丢掉，等于平台替调用方决定了尺寸。
/// - **目标型别没给** → 只有（比例 + 档位）↔ 像素这一种换算，且必须能在档案里查到那一格。
///
/// `auto` 不参与任何换算：目标形态本身就是 `auto` 时原样通过（它只透传），其余情形一律明确失败
/// ——平台不替模型算一个比例，也不把调用方说的 `auto` 丢掉。
///
/// 其余组合一律失败，理由与"档案缺那一格"相同：猜一个值会让调用方拿到的图与它要的尺寸无关，
/// 而这种错在图上完全看不出来。
pub fn convert_size(
    form: SizeForm,
    profile: &SizeProfile,
    values: &[SizeSpec],
) -> Result<SizeSpec, String> {
    let (pixels, ratio, tier, auto) = collect_size_values(values)?;
    if auto {
        // `auto` 的语义是"由模型按提示词自己决定最佳比例"，平台不替它算一个比例：它只原样透传、
        // 永不换算。目标形态本身就是 `auto` 时原样通过；否则这条候选收不了它——把 `auto` 换算成
        // 别的形态，等于平台替模型做了这个决定。请求同时给了别的尺寸取值时同样失败：那些取值会
        // 被无声丢掉，与"多出来的那一型会被悄悄丢掉"是同一条理由。
        if form == SizeForm::Auto && pixels.is_none() && ratio.is_none() && tier.is_none() {
            return Ok(SizeSpec::Auto);
        }
        return Err(auto_is_pass_through_only(
            form,
            &given_forms(pixels.is_some(), ratio.is_some(), tier.is_some()),
        ));
    }
    match (form, pixels, ratio, tier) {
        // 已经是供给要的那一型：直接用，不查档案。
        (SizeForm::Pixels, Some(SizeSpec::Pixels { width, height }), None, None) => {
            Ok(SizeSpec::Pixels { width, height })
        }
        (SizeForm::Ratio, None, Some(ratio), None) => Ok(SizeSpec::Ratio {
            ratio: ratio.to_owned(),
        }),
        (SizeForm::Tier, None, None, Some(tier)) => Ok(SizeSpec::Tier {
            tier: tier.to_owned(),
        }),
        // 供给要像素、调用方给的是比例 + 档位：查档案。
        (SizeForm::Pixels, None, Some(ratio), Some(tier)) => {
            let (width, height) = profile.lookup(ratio, tier).ok_or_else(|| {
                format!("the size profile has no entry for ratio {ratio} at tier {tier}")
            })?;
            Ok(SizeSpec::Pixels { width, height })
        }
        // 供给要比例/档位、调用方给的是像素：反查档案。
        (SizeForm::Ratio, Some(SizeSpec::Pixels { width, height }), None, None) => {
            let (ratio, _) = profile.lookup_reverse(width, height).ok_or_else(|| {
                format!("the size profile has no entry for the pixel size {width}x{height}")
            })?;
            Ok(SizeSpec::Ratio { ratio })
        }
        (SizeForm::Tier, Some(SizeSpec::Pixels { width, height }), None, None) => {
            let (_, tier) = profile.lookup_reverse(width, height).ok_or_else(|| {
                format!("the size profile has no entry for the pixel size {width}x{height}")
            })?;
            Ok(SizeSpec::Tier { tier })
        }
        (form, pixels, ratio, tier) => Err(unusable_size(
            form,
            pixels.is_some(),
            ratio.is_some(),
            tier.is_some(),
        )),
    }
}

/// 按型别归位后的尺寸取值：像素、比例、档位各自最多一个（同型给出两个不同的值是含糊），
/// 外加"这次请求给的是不是 `auto`"。
type GivenSize<'a> = (Option<SizeSpec>, Option<&'a str>, Option<&'a str>, bool);

/// 取值按型别归位：同一型给出两个不同的值（例如两个档位）是**含糊**，直接报错。
fn collect_size_values(values: &[SizeSpec]) -> Result<GivenSize<'_>, String> {
    let mut pixels = None;
    let mut ratio = None;
    let mut tier = None;
    let mut auto = false;
    for value in values {
        match value {
            SizeSpec::Pixels { width, height } => {
                set_once(
                    &mut pixels,
                    SizeSpec::Pixels {
                        width: *width,
                        height: *height,
                    },
                    "pixel size",
                )?;
            }
            SizeSpec::Ratio { ratio: given } => set_once(&mut ratio, given.as_str(), "ratio")?,
            SizeSpec::Tier { tier: given } => set_once(&mut tier, given.as_str(), "tier")?,
            // `auto` 没有可归位的型别：它给几次都是同一个意思，因此只记"给过"。
            SizeSpec::Auto => auto = true,
        }
    }
    Ok((pixels, ratio, tier, auto))
}

fn set_once<T: PartialEq + Display>(
    slot: &mut Option<T>,
    value: T,
    what: &str,
) -> Result<(), String> {
    match slot {
        Some(existing) if *existing != value => Err(format!(
            "the request gives two different {what} values: {existing} and {value}"
        )),
        _ => {
            *slot = Some(value);
            Ok(())
        }
    }
}

/// 这一组取值里的 `auto` 收不了：说清"它只能原样透传、不能换算"，让判定记录里看得懂。
///
/// 两种情形都落到这里，但理由不同：目标形态不是 `auto` 时，平台不能替模型算一个比例；目标形态
/// 就是 `auto` 而请求还给了别的尺寸取值时，那些取值会被无声丢掉。两者都是"平台不许替调用方
/// 决定尺寸"。
fn auto_is_pass_through_only(form: SizeForm, others: &[&str]) -> String {
    if form == SizeForm::Auto {
        format!(
            "this offering passes `auto` through as it is and cannot convert the request's other size values ({}) into it",
            others.join(" + ")
        )
    } else {
        format!(
            "`auto` lets the model decide the size itself, so it can only be passed through as it is and cannot be converted to {}",
            form.as_str()
        )
    }
}

/// 这一组取值没法变成供给要的那一型：说清"要哪一型、给的是哪几型"，让判定记录里看得懂。
fn unusable_size(form: SizeForm, pixels: bool, ratio: bool, tier: bool) -> String {
    let given = given_forms(pixels, ratio, tier);
    if given.is_empty() {
        return "the request gives no size value".to_owned();
    }
    if form == SizeForm::Auto {
        // 这条供给只把 `auto` 原样透传：把调用方明确给的尺寸换成"让模型自己挑"同样是替它决定尺寸。
        return format!(
            "this offering only passes `auto` through and cannot convert the request's size ({}) into it",
            given.join(" + ")
        );
    }
    format!(
        "this offering sizes images in {} and cannot use the request's size ({})",
        form.as_str(),
        given.join(" + ")
    )
}

/// 这一组取值给了哪几型。
fn given_forms(pixels: bool, ratio: bool, tier: bool) -> Vec<&'static str> {
    let mut given = Vec::new();
    if pixels {
        given.push("pixels");
    }
    if ratio {
        given.push("a ratio");
    }
    if tier {
        given.push("a tier");
    }
    given
}

/// 档案里的一个键：按期望的型别解析并取规范化后的形态。
fn profile_key(
    key: &str,
    expected: SizeForm,
    take: impl Fn(&SizeSpec) -> Option<String>,
) -> Result<String, String> {
    let spec =
        SizeSpec::parse(key).map_err(|reason| format!("size profile key `{key}`: {reason}"))?;
    take(&spec).ok_or_else(|| {
        format!(
            "size profile key `{key}` must be a {}, got {}",
            expected.as_str(),
            spec.form().as_str()
        )
    })
}

/// 按分隔符切成两半：必须恰好出现一次、两边都非空。
fn split_pair(value: &str, separator: char) -> Option<(&str, &str)> {
    let mut parts = value.split(separator);
    let first = parts.next()?;
    let second = parts.next()?;
    if parts.next().is_some() || first.is_empty() || second.is_empty() {
        return None;
    }
    Some((first, second))
}

/// 一个维度/档位数字：必须是正整数。`0`、负数、小数、溢出、带空格都不是。
fn dimension(text: &str, whole: &str) -> Result<u32, String> {
    text.parse::<u32>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| not_a_size(whole))
}

fn not_a_size(value: &str) -> String {
    format!(
        "`{value}` is not a size: expected pixels (`1024x1024`), a ratio (`16:9`), a tier (`2K`) or `auto`"
    )
}

fn greatest_common_divisor(left: u32, right: u32) -> u32 {
    if right == 0 {
        left
    } else {
        greatest_common_divisor(right, left % right)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Seedream 5.0 lite 的 2K 档映射表（一手来源是厂商文档里的"分辨率 × 宽高比 → 宽高像素值"）。
    /// 它出现在这里只是**测试夹具**：领域代码里没有任何厂商的档位表。
    fn lite_2k() -> Value {
        serde_json::json!({
            "2K": {
                "1:1": "2048x2048",
                "4:3": "2304x1728",
                "3:4": "1728x2304",
                "16:9": "2848x1600",
                "9:16": "1600x2848",
                "3:2": "2496x1664",
                "2:3": "1664x2496",
                "21:9": "3136x1344"
            }
        })
    }

    /// Seedream 5.0 pro 的表：与 lite 在同一个档位、同一个比例上给出**不同的像素**。
    /// 两份档案因此能证明"同一格查出来的值来自档案，而不是来自代码"。
    fn pro_1k_and_2k() -> Value {
        serde_json::json!({
            "1K": {
                "1:1": "1024x1024",
                "4:3": "1152x864",
                "16:9": "1424x800",
                "2:3": "832x1248",
                "21:9": "1568x672"
            },
            "2K": {
                "1:1": "2048x2048",
                "4:3": "2368x1776",
                "16:9": "2816x1584",
                "2:3": "1664x2496",
                "21:9": "3136x1344"
            }
        })
    }

    fn profile(value: Value) -> SizeProfile {
        SizeProfile::from_json(&value).expect("the fixture profile parses")
    }

    #[test]
    fn parses_the_four_forms_and_normalises_them() {
        for (value, expected) in [
            (
                "1024x1024",
                SizeSpec::Pixels {
                    width: 1024,
                    height: 1024,
                },
            ),
            (
                "2048X2048",
                SizeSpec::Pixels {
                    width: 2048,
                    height: 2048,
                },
            ),
            (
                "1664x2496",
                SizeSpec::Pixels {
                    width: 1664,
                    height: 2496,
                },
            ),
            (
                "16:9",
                SizeSpec::Ratio {
                    ratio: "16:9".to_owned(),
                },
            ),
            // 比例约分：同一个宽高比的不同写法在解析后是同一个值。
            (
                "32:18",
                SizeSpec::Ratio {
                    ratio: "16:9".to_owned(),
                },
            ),
            (
                "4:6",
                SizeSpec::Ratio {
                    ratio: "2:3".to_owned(),
                },
            ),
            (
                "2K",
                SizeSpec::Tier {
                    tier: "2K".to_owned(),
                },
            ),
            (
                "2k",
                SizeSpec::Tier {
                    tier: "2K".to_owned(),
                },
            ),
            (
                "04K",
                SizeSpec::Tier {
                    tier: "4K".to_owned(),
                },
            ),
            // 第四型：不是一个具体尺寸，线上原样就是 `auto`。
            ("auto", SizeSpec::Auto),
        ] {
            let parsed =
                SizeSpec::parse(value).unwrap_or_else(|error| panic!("`{value}`: {error}"));
            assert_eq!(parsed, expected, "`{value}`");
            // 序列化回字符串：线上就是这一份形态。
            assert_eq!(parsed.to_string(), expected.to_string(), "`{value}`");
        }
        assert_eq!(
            SizeSpec::parse("1024x1024").expect("pixels").form(),
            SizeForm::Pixels
        );
        assert_eq!(
            SizeSpec::parse("16:9").expect("ratio").form(),
            SizeForm::Ratio
        );
        assert_eq!(SizeSpec::parse("2K").expect("tier").form(), SizeForm::Tier);
        assert_eq!(
            SizeSpec::parse("auto").expect("auto").form(),
            SizeForm::Auto
        );
        // 型别的名字就是这四个。
        assert_eq!(SizeForm::parse("pixels"), Some(SizeForm::Pixels));
        assert_eq!(SizeForm::parse("ratio"), Some(SizeForm::Ratio));
        assert_eq!(SizeForm::parse("tier"), Some(SizeForm::Tier));
        assert_eq!(SizeForm::parse("auto"), Some(SizeForm::Auto));
        assert_eq!(SizeForm::parse("pixel"), None);
        assert_eq!(SizeForm::Pixels.as_str(), "pixels");
        assert_eq!(SizeForm::Auto.as_str(), "auto");
    }

    #[test]
    fn rejects_anything_that_is_not_one_of_the_four_forms() {
        for value in [
            "",
            "1024",
            "1024x",
            "x1024",
            "1024x1024x1024",
            "0x1024",
            "1024x0",
            "0x0",
            "-1024x1024",
            "1e3x1e3",
            "1024.5x1024",
            " 1024x1024",
            "1024 x 1024",
            "16:",
            ":9",
            "16:9:1",
            "0:9",
            "16:0",
            "1.5:1",
            "K",
            "k",
            "0K",
            "2M",
            "2 K",
            "K2",
            "2KB",
            "99999999999999K",
            "1024x99999999999999",
            // `auto` 只有这一个写法：它不是"随便什么写法都能认"的别名。
            "AUTO",
            "Auto",
            " auto",
            "auto ",
        ] {
            let error = SizeSpec::parse(value)
                .err()
                .unwrap_or_else(|| panic!("`{value}` 不是一个尺寸"));
            assert!(
                error.contains("size"),
                "报错要说清这不是一个尺寸：`{value}` → {error}"
            );
        }
    }

    /// `auto` 的语义是"由模型按提示词自己决定最佳比例"：它只原样透传，永不换算。
    #[test]
    fn auto_is_only_passed_through_and_never_converted() {
        let lite = profile(lite_2k());
        let auto = [SizeSpec::parse("auto").expect("auto")];
        // 目标形态本身就是 `auto`：原样通过，档案用不上。
        assert_eq!(
            convert_size(SizeForm::Auto, &SizeProfile::default(), &auto)
                .expect("auto passes through"),
            SizeSpec::Auto
        );
        // 目标形态是别的形态：明确失败，错误信息说清"只能原样透传、不能换算"。
        for form in [SizeForm::Pixels, SizeForm::Ratio, SizeForm::Tier] {
            let error =
                convert_size(form, &lite, &auto).expect_err("auto is not convertible at all");
            assert!(
                error.contains("auto")
                    && error.contains("passed through")
                    && error.contains("cannot be converted"),
                "报错要说清 `auto` 只能原样透传、不能换算：{error}"
            );
        }
        // 目标形态就是 `auto`，但请求还给了别的尺寸取值：那些取值会被无声丢掉，因此失败。
        let error = convert_size(
            SizeForm::Auto,
            &lite,
            &[SizeSpec::Auto, SizeSpec::parse("2K").expect("tier")],
        )
        .expect_err("the tier would be silently dropped");
        assert!(
            error.contains("a tier") && error.contains("cannot convert"),
            "{error}"
        );
        // 目标形态是 `auto`、请求给的是像素：不能把调用方说的尺寸换成"让模型自己挑"。
        let error = convert_size(
            SizeForm::Auto,
            &lite,
            &[SizeSpec::parse("1024x1024").expect("pixels")],
        )
        .expect_err("pixels cannot become auto");
        assert!(
            error.contains("`auto`") && error.contains("cannot convert"),
            "{error}"
        );
        // 同一个 `auto` 给两次不是含糊：它就是同一个值。
        assert_eq!(
            convert_size(
                SizeForm::Auto,
                &lite,
                &[SizeSpec::Auto, SizeSpec::parse("auto").expect("auto")]
            )
            .expect("auto twice is the same value"),
            SizeSpec::Auto
        );
    }

    #[test]
    fn converts_a_ratio_and_a_tier_into_pixels_with_the_published_profile() {
        let lite = profile(lite_2k());
        for (ratio, expected) in [
            ("1:1", (2048, 2048)),
            ("4:3", (2304, 1728)),
            ("16:9", (2848, 1600)),
            ("3:2", (2496, 1664)),
            // 设计里点名的那一格：2:3 + 2K。
            ("2:3", (1664, 2496)),
            ("21:9", (3136, 1344)),
        ] {
            let values = [
                SizeSpec::parse(ratio).expect("ratio"),
                SizeSpec::parse("2K").expect("tier"),
            ];
            let converted = convert_size(SizeForm::Pixels, &lite, &values)
                .unwrap_or_else(|error| panic!("{ratio} + 2K: {error}"));
            assert_eq!(
                converted,
                SizeSpec::Pixels {
                    width: expected.0,
                    height: expected.1
                },
                "{ratio} + 2K"
            );
            assert_eq!(
                converted.to_string(),
                format!("{}x{}", expected.0, expected.1)
            );
        }
        // 同一个档位、同一个比例，在两份档案上换算出的像素不同：值来自档案。
        let pro = profile(pro_1k_and_2k());
        let values = [
            SizeSpec::parse("16:9").expect("ratio"),
            SizeSpec::parse("2K").expect("tier"),
        ];
        assert_eq!(
            convert_size(SizeForm::Pixels, &pro, &values).expect("pro 2K 16:9"),
            SizeSpec::Pixels {
                width: 2816,
                height: 1584
            }
        );
        assert_eq!(
            convert_size(SizeForm::Pixels, &lite, &values).expect("lite 2K 16:9"),
            SizeSpec::Pixels {
                width: 2848,
                height: 1600
            }
        );
        // 调用方直接给像素、供给也要像素：不需要档案，空档案也照样通过。
        let direct = [SizeSpec::parse("3750x1250").expect("pixels")];
        assert_eq!(
            convert_size(SizeForm::Pixels, &SizeProfile::default(), &direct)
                .expect("pixels pass through"),
            SizeSpec::Pixels {
                width: 3750,
                height: 1250
            }
        );
    }

    #[test]
    fn converts_pixels_back_into_a_ratio_and_a_tier() {
        let pro = profile(pro_1k_and_2k());
        let pixels = [SizeSpec::parse("1024x1024").expect("pixels")];
        assert_eq!(
            convert_size(SizeForm::Ratio, &pro, &pixels).expect("ratio"),
            SizeSpec::Ratio {
                ratio: "1:1".to_owned()
            }
        );
        assert_eq!(
            convert_size(SizeForm::Tier, &pro, &pixels).expect("tier"),
            SizeSpec::Tier {
                tier: "1K".to_owned()
            }
        );
        // 同一份档案里另一档的像素值反查回另一档。
        let pixels = [SizeSpec::parse("2048x2048").expect("pixels")];
        assert_eq!(
            convert_size(SizeForm::Tier, &pro, &pixels).expect("tier"),
            SizeSpec::Tier {
                tier: "2K".to_owned()
            }
        );
        // 档案里没有的像素值：反查不出，明确失败。
        let pixels = [SizeSpec::parse("1500x1500").expect("pixels")];
        let error = convert_size(SizeForm::Tier, &pro, &pixels).expect_err("no such entry");
        assert!(error.contains("1500x1500"), "{error}");
        // 比例/档位本来就是要的那一型：原样用，不查档案。
        assert_eq!(
            convert_size(
                SizeForm::Ratio,
                &SizeProfile::default(),
                &[SizeSpec::parse("32:18").expect("ratio")]
            )
            .expect("ratio pass through"),
            SizeSpec::Ratio {
                ratio: "16:9".to_owned()
            }
        );
    }

    #[test]
    fn refuses_to_guess_when_the_profile_has_no_such_combination() {
        let lite = profile(lite_2k());
        // lite 的档案里只有 2K：3K 那一格缺了。
        let values = [
            SizeSpec::parse("2:3").expect("ratio"),
            SizeSpec::parse("3K").expect("tier"),
        ];
        let error = convert_size(SizeForm::Pixels, &lite, &values).expect_err("3K is not in lite");
        assert!(error.contains("2:3") && error.contains("3K"), "{error}");
        // 比例缺了同样失败：档案里没有 5:4。
        let values = [
            SizeSpec::parse("5:4").expect("ratio"),
            SizeSpec::parse("2K").expect("tier"),
        ];
        let error = convert_size(SizeForm::Pixels, &lite, &values).expect_err("5:4 is not in lite");
        assert!(error.contains("5:4") && error.contains("2K"), "{error}");
    }

    #[test]
    fn refuses_size_values_it_cannot_turn_into_the_form_the_offering_wants() {
        let lite = profile(lite_2k());
        // 只有一个比例、没有档位：查不到像素，也不许猜一个档位。
        let error = convert_size(
            SizeForm::Pixels,
            &lite,
            &[SizeSpec::parse("16:9").expect("ratio")],
        )
        .expect_err("a ratio alone cannot become pixels");
        assert!(
            error.contains("pixels") && error.contains("a ratio"),
            "{error}"
        );
        // 只有一个档位：同理。
        let error = convert_size(
            SizeForm::Pixels,
            &lite,
            &[SizeSpec::parse("2K").expect("tier")],
        )
        .expect_err("a tier alone cannot become pixels");
        assert!(error.contains("a tier"), "{error}");
        // 供给只要比例，调用方却同时给了比例与档位：档位会被无声丢掉，因此失败。
        let error = convert_size(
            SizeForm::Ratio,
            &lite,
            &[
                SizeSpec::parse("16:9").expect("ratio"),
                SizeSpec::parse("2K").expect("tier"),
            ],
        )
        .expect_err("the tier would be silently dropped");
        assert!(error.contains("ratio") && error.contains("tier"), "{error}");
        // 一次请求给出两个档位：含糊，失败。
        let error = convert_size(
            SizeForm::Pixels,
            &lite,
            &[
                SizeSpec::parse("2:3").expect("ratio"),
                SizeSpec::parse("2K").expect("tier"),
                SizeSpec::parse("3K").expect("tier"),
            ],
        )
        .expect_err("two tiers are ambiguous");
        assert!(error.contains("two different tier"), "{error}");
        // 一个尺寸都没给：也说清楚。
        let error = convert_size(SizeForm::Pixels, &lite, &[]).expect_err("no size at all");
        assert!(error.contains("no size value"), "{error}");
    }

    /// 反向查必须有确定答案：同一组像素在档案里出现两次时按键序取第一组。
    #[test]
    fn the_reverse_lookup_is_deterministic() {
        let profile = profile(serde_json::json!({
            "2K": {"1:1": "2048x2048", "4:3": "2048x2048"},
            "1K": {"1:1": "2048x2048"}
        }));
        assert_eq!(
            profile.lookup_reverse(2048, 2048),
            Some(("1:1".to_owned(), "1K".to_owned())),
            "键序最靠前的那一组"
        );
        assert_eq!(profile.lookup_reverse(1024, 1024), None);
        assert_eq!(profile.lookup("1:1", "2K"), Some((2048, 2048)));
        assert_eq!(profile.lookup("1:1", "4K"), None);
    }

    #[test]
    fn the_profile_keys_are_normalised_when_parsed() {
        // 档案里写小写档位、没约分的比例，查的时候用规范化后的形态照样命中。
        let profile = profile(serde_json::json!({
            "2k": {"32:18": "2848x1600"}
        }));
        assert_eq!(profile.lookup("16:9", "2K"), Some((2848, 1600)));
        assert!(!profile.is_empty());
        assert!(SizeProfile::default().is_empty());
    }

    #[test]
    fn a_profile_that_is_not_a_table_is_rejected() {
        for (value, needle) in [
            (serde_json::json!([]), "must be an object"),
            (serde_json::json!("2K"), "must be an object"),
            (serde_json::json!({"2K": "2048x2048"}), "must be an object"),
            (
                serde_json::json!({"2K": {"1:1": 2048}}),
                "must be a pixel string",
            ),
            (serde_json::json!({"2K": {"1:1": "16:9"}}), "must be pixels"),
            (
                serde_json::json!({"1:1": {"2K": "2048x2048"}}),
                "must be a tier",
            ),
            (
                serde_json::json!({"2K": {"2K": "2048x2048"}}),
                "must be a ratio",
            ),
            (serde_json::json!({"2K": {"1:1": "0x0"}}), "is not a size"),
            // 规范化之后撞在同一格上：档案自己含糊，拒绝。
            (
                serde_json::json!({"2K": {"1:1": "2048x2048", "2:2": "2048x2048"}}),
                "twice",
            ),
        ] {
            let error = SizeProfile::from_json(&value)
                .err()
                .unwrap_or_else(|| panic!("{value} 不是一张表"));
            assert!(error.contains(needle), "{value} → {error}");
        }
    }
}
