//! 图片输入在"调用方给的字段"与"候选自己声明的字段"之间怎么走。
//!
//! 全平台只有这一处讲图片参数的名字与空值，但它回答的是**三个不同的问题**，三套约定不能混：
//!
//! 1. **平台契约面**（受理侧）：平台只认三个字段名——`image` 与 `image_urls`（同义，二选一）
//!    装参考图，`mask` 装遮罩，**精确匹配键名**。这让"哪些字段由平台自己管"成为一份封闭、
//!    可以逐字背下来的清单：其余任何名字（哪怕叫 `image_with_roles`、`mask_url`、`images`）
//!    都不是平台的事，留在请求参数里等下一步按合同与承载面处置。
//! 2. **声明面**（过滤与装载）：一份 schema 声明了哪些顶层字段名，就是"这次请求能带哪些参数名"
//!    的判据。它有两个用处，判据同一份、处置不同：**合同**决定字段归属——合同没声明的名字在
//!    受理时直接丢掉（不报错）；**承载面**决定这条供给承载得了什么——请求里真正用到的字段若
//!    不在承载面里，那是**该候选不合格**（换下一条候选，或明确报"无可用供给"），绝不静默丢参。
//!    装载图片也用承载面：把调用方的图落到它声明的参数名上（`image`、`image_urls`、`mask_url`……）。
//!    这一面的名字由渠道的 Profile 决定、无法预先枚举，所以装图时按名字的形状认：以 `image`
//!    开头的是参考图、名字里含 `mask` 的是遮罩。
//! 3. **归属**（取回）：一个参数名到底是不是"平台自己装好的"，由**显式名单**决定，与它的取值
//!    长什么形状无关。名单就是 [`platform_image_parameters`] 按候选声明面与分支算出的那几个名字
//!    （在请求受理时冻结进 Job）。Driver 只认名单：名单里的名字按候选声明面分清哪一个是参考图、
//!    哪一个是遮罩。名单里的名字来自候选声明面，过滤一定留得下它们。
//!
//! "像不像图片参数"是第 2 面的判据（[`image_parameter_kind`] 按**名字的形状**分角色，与取值
//! 无关）；第 1 面的判据是"键名是不是那三个"，第 3 面的判据是"名字在不在名单里"。
//! 三面都不看**取值的形状**——这正是这一批约定要守住的边界。
//!
//! 为什么归属不能看取值形状：调用方也可以给一个叫 `images` 的参数（渠道文档里的原生名），
//! 取值同样是字符串数组。按形状判就会把它误当成平台装载的图——在取图时被读走，或者在换算图片
//! 时被改成上游 URL，两种都是对调用方参数的改写。归属由名单决定之后，
//! "平台装载的图"与"调用方恰好给了一个像图的名字"再也不会混。
//!
//! 为什么合同外的参数是丢掉而不是拒掉、也不是透传：合同就是客户端那一侧的参数面，把没声明的
//! 名字发过去，上游要么拒、要么按自己的一套解释成一个平台根本没打算传的东西；而按"一律透传"
//! 放行，等于平台替调用方承诺了一份它没有核对过的上游戏约。丢掉是唯一不留后患的处置，
//! 而且它必须发生在受理期——Driver 拿到的参数面因此**天然是声明面内的子集**。
//! 注意丢掉只适用于"合同都没声明"的名字：合同声明了、这条供给承载不了的名字**不丢**，
//! 那说明这条候选表达不了这次请求，要么换候选、要么明确失败。
//!
//! 空值约定三面共用：`null` 与空串都表示"这一处没有图"，一律当作没给。对**坏取值**的处置不同：
//! 第 1 面里解释不成一张图的取值（数字、布尔、对象、数组里混进的非字符串条目）直接报错——那是
//! 调用方说的话，平台不替它猜；第 3 面里的取值是平台自己装载进去的，只可能是字符串或字符串数组。

use crate::ImageBranch;
use serde_json::{Map, Value};

/// 调用方传进来的图片输入，已经从原生参数里取出来。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageInputs {
    pub reference_images: Vec<String>,
    pub mask: Option<String>,
}

/// 一个图片参数名扮演的角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageParameterKind {
    /// 参考图：要拿去生成 / 编辑的那张图。
    Reference,
    /// 遮罩：编辑范围。
    Mask,
}

// ── 平台契约面：只有这三个字段名 ──────────────────────────────────────────────

/// 契约里装参考图的字段名：同义，二选一。
const CONTRACT_REFERENCE_IMAGE_PARAMETERS: [&str; 2] = ["image", "image_urls"];
/// 契约里装遮罩的字段名。
const CONTRACT_MASK_PARAMETER: &str = "mask";

/// 契约字段名扮演的角色：**精确匹配键名**，不做前缀或包含判断。
///
/// 受理侧的 multipart 路径用它判断"这个部件是不是平台自己管的图片字段"；JSON 路径整批判，
/// 见 [`take_contract_image_inputs`]。
#[must_use]
pub fn contract_image_parameter_kind(name: &str) -> Option<ImageParameterKind> {
    if name == CONTRACT_MASK_PARAMETER {
        Some(ImageParameterKind::Mask)
    } else if CONTRACT_REFERENCE_IMAGE_PARAMETERS.contains(&name) {
        Some(ImageParameterKind::Reference)
    } else {
        None
    }
}

/// 受理侧入口：按契约字段名取出参考图与遮罩，并把这三个名字从参数里**摘掉**。
///
/// 摘掉之后它们不再当普通参数：图片有自己的去处（选路后落到候选声明的字段上）。
/// `image` 与 `image_urls` 是同义字段，**两边都给出非空值**才算含糊，宁可报错也不替调用方挑
/// 一个；`null`、空串、空数组都是"这一处没有图"，不参与这个判定——否则
/// `{"image": null, "image_urls": ["u"]}` 会因为一个空位被拒，与空值约定自相矛盾。
///
/// 只摘这三个名字：其余字段（含渠道文档里的 `image_with_roles`、`mask_url`、`images`）
/// 留在这里不动，由选路后的 [`declared_parameter_names`] 按候选声明面处置。
pub fn take_contract_image_inputs(
    parameters: &mut Map<String, Value>,
) -> Result<ImageInputs, String> {
    let mut reference_images = Vec::new();
    let mut given: Vec<&str> = Vec::new();
    for name in CONTRACT_REFERENCE_IMAGE_PARAMETERS {
        let Some(value) = parameters.get(name) else {
            continue;
        };
        let values = image_parameter_values(name, value)?;
        if values.is_empty() {
            continue;
        }
        given.push(name);
        if given.len() > 1 {
            return Err(synonym_error(&given));
        }
        reference_images = values.into_iter().map(str::to_owned).collect();
    }
    let mask = match parameters.get(CONTRACT_MASK_PARAMETER) {
        Some(value) => mask_value(CONTRACT_MASK_PARAMETER, value)?.map(str::to_owned),
        None => None,
    };
    for name in CONTRACT_REFERENCE_IMAGE_PARAMETERS {
        parameters.remove(name);
    }
    parameters.remove(CONTRACT_MASK_PARAMETER);
    Ok(ImageInputs {
        reference_images,
        mask,
    })
}

// ── 候选声明面：名字的形状来自 Profile ────────────────────────────────────────

/// 参数名是否表示"这是参考图"：以 `image` 开头（`image`、`images`、`image_urls`）。
#[must_use]
pub fn is_reference_image_parameter(name: &str) -> bool {
    name.starts_with("image")
}

/// 参数名是否表示"这是遮罩"：名字里含 `mask`（`mask`、`mask_url`）。
#[must_use]
pub fn is_mask_parameter(name: &str) -> bool {
    name.contains("mask")
}

/// 这个参数名属于哪一类图片输入；不是图片参数则为 `None`。
///
/// 两类都像的名字（例如 `image_mask`）**算遮罩**：遮罩只划定编辑范围，含义比参考图更窄更
/// 具体；把这类名字当参考图，会把一块编辑范围当成一张要拿去生成的图，错得更远。一个名字只给
/// 一种解释，否则同一个字段读出来的和写进去的可能是两类东西。
#[must_use]
pub fn image_parameter_kind(name: &str) -> Option<ImageParameterKind> {
    if is_mask_parameter(name) {
        Some(ImageParameterKind::Mask)
    } else if is_reference_image_parameter(name) {
        Some(ImageParameterKind::Reference)
    } else {
        None
    }
}

/// 候选的参数面里装参考图的参数名，取名字序里的第一个。
fn reference_image_parameter(schema: &Value) -> Option<(&str, &Value)> {
    schema
        .get("properties")?
        .as_object()?
        .iter()
        .find(|(name, _)| image_parameter_kind(name) == Some(ImageParameterKind::Reference))
        .map(|(name, field)| (name.as_str(), field))
}

/// 候选的参数面里装遮罩的参数名。
fn mask_parameter(schema: &Value) -> Option<(&str, &Value)> {
    schema
        .get("properties")?
        .as_object()?
        .iter()
        .find(|(name, _)| image_parameter_kind(name) == Some(ImageParameterKind::Mask))
        .map(|(name, field)| (name.as_str(), field))
}

/// 候选是否声明了装参考图的参数。
#[must_use]
pub fn declares_reference_image_parameter(schema: &Value) -> bool {
    reference_image_parameter(schema).is_some()
}

/// 候选是否声明了遮罩参数。
#[must_use]
pub fn declares_mask_parameter(schema: &Value) -> bool {
    mask_parameter(schema).is_some()
}

/// Profile 对参考图的**声明上限**：数组取 `maxItems`，单值按 1。
///
/// 数组形式声明了却没写 `maxItems` 视为**没有承诺上限**（返回 `None`）——发布期据此拒绝
/// "限制允许的图片数超过 Profile 承诺"的情形：限制只能收窄，不能凭空放宽。
#[must_use]
pub fn declared_reference_image_limit(schema: &Value) -> Option<u64> {
    let mut scalar = false;
    let mut maximum = 0_u64;
    let properties = schema.get("properties").and_then(Value::as_object)?;
    for (name, field) in properties {
        if image_parameter_kind(name) != Some(ImageParameterKind::Reference) {
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
        Some(maximum)
    } else if scalar {
        Some(1)
    } else {
        None
    }
}

/// 把调用方的参考图与遮罩写进该候选**自己声明的参数名**上（数组赋值 / 标量赋值）。
///
/// 找不到装图片的参数、或者图片张数超过该参数装得下的数量，都返回 `Err`：这类候选表达不了
/// 这次请求，选路据此判它不合格——不静默丢掉一张图。
pub fn place_image_inputs(
    schema: &Value,
    object: &mut Map<String, Value>,
    reference_images: &[String],
    mask: Option<&str>,
) -> Result<(), String> {
    if !reference_images.is_empty() {
        let (name, field) = reference_image_parameter(schema)
            .ok_or_else(|| "this offering declares no reference image parameter".to_owned())?;
        let is_array = field.get("type").and_then(Value::as_str) == Some("array")
            || field.get("items").is_some();
        if is_array {
            let capacity = field
                .get("maxItems")
                .and_then(Value::as_u64)
                .unwrap_or(u64::MAX);
            if u64::try_from(reference_images.len()).unwrap_or(u64::MAX) > capacity {
                return Err(format!(
                    "this offering accepts at most {capacity} reference image(s)"
                ));
            }
            object.insert(
                name.to_owned(),
                Value::Array(
                    reference_images
                        .iter()
                        .map(|value| Value::String(value.clone()))
                        .collect(),
                ),
            );
        } else {
            if reference_images.len() > 1 {
                return Err(format!(
                    "this offering takes a single reference image in {name}"
                ));
            }
            object.insert(name.to_owned(), Value::String(reference_images[0].clone()));
        }
    }
    if let Some(mask) = mask {
        let (name, _) = mask_parameter(schema)
            .ok_or_else(|| "this offering declares no mask parameter".to_owned())?;
        object.insert(name.to_owned(), Value::String(mask.to_owned()));
    }
    Ok(())
}

/// 按这份**声明面**留下参数：只保留它声明过的顶层字段名（`properties` 的键，或 `required` 里
/// 列出的名字）。
///
/// 调用方给的其他名字——渠道文档里有文档的一手参数（`image_with_roles`）、平台没声明的普通参数
/// （`seed`）、纯属多余的 `foo`——在这里**直接丢掉**：既不报错（多写一个参数不该让整次请求失败），
/// 也不会跟着请求走去上游（上游没声明过它，发过去只会得到上游自己的一套解释）。
///
/// 传进来的 schema 决定这次过滤问的是哪个问题：传**合同**问的是"这个字段是调用方该提交的吗"
/// （归属，丢掉不报错）；传**承载面**是在组装参数面时把空位清干净（空值不携带信息，而承载面没
/// 声明的名字不该发给上游）。**用到的字段承载不了**不在这里处置：那是"这条候选表达不了这次请求"，
/// 由受理侧判它不合格——绝不靠过滤把它悄悄抹掉。
///
/// 只对比**顶层字段名**：声明面就是"这个字段名叫什么"的一份清单，平台不去猜它的内部结构——
/// 候选声明了 `extra`，`extra` 这个整体就留下，里面有什么由上游按自己的 schema 处置。
///
/// 顺序无关，也**不涉及图片归属**：平台装载的图片参数名就是用这份声明面选出来的
/// （见 [`platform_image_parameters`]），过滤一定留得下它们。所以本函数与 [`place_image_inputs`]
/// 谁先谁后都对；受理侧按"先按声明面过滤、再装载图片"的顺序用，是让落库的参数面一开始就干净。
///
/// 未声明的参数被丢掉，因此**不会**出现"透传未声明参数"这回事；也正因如此，Driver 拿到的
/// 参数面天然是声明面内的子集，它不需要、也没有机会再判一次"这个参数认不认识"。
#[must_use]
pub fn declared_parameter_names(schema: &Value, names: &Map<String, Value>) -> Map<String, Value> {
    names
        .iter()
        .filter(|(name, _)| crate::declares_parameter(schema, name))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

/// **平台自己装好的参数名**：由候选声明的参数名与分支共同算出。这就是"归属"的唯一判据。
///
/// - `prompt_only`：平台不装载任何图，名单为空；
/// - `image_conditioned`：候选声明的那个参考图参数名；
/// - `masked`：参考图参数名，再加上候选声明的遮罩参数名。
///
/// 它是**确定性的**：候选面（[`place_image_inputs`] 写进去的那些名字）与分支都在 Job 里冻结了，
/// 与调用方这一次恰好给了什么取值无关。因此它可以在受理时算一次、随请求交给 Driver。
///
/// 名单里的名字按候选声明面判定角色（见 [`image_parameter_kind`]）就够：能进这份名单的名字
/// 都是 [`reference_image_parameter`] / [`mask_parameter`] 选出来的，角色本来唯一。
///
/// 名字必在声明面内：它就是从这里选出来的，所以 [`declared_parameter_names`] 一定留得下它们。
#[must_use]
pub fn platform_image_parameters(schema: &Value, branch: ImageBranch) -> Vec<String> {
    if branch == ImageBranch::PromptOnly {
        return Vec::new();
    }
    let mut names = Vec::new();
    if let Some((name, _)) = reference_image_parameter(schema) {
        names.push(name.to_owned());
    }
    if branch == ImageBranch::Masked
        && let Some((name, _)) = mask_parameter(schema)
    {
        names.push(name.to_owned());
    }
    names
}

/// 名单里扮演某个角色的那个参数名：回答"平台的这张图该写在哪个参数名（哪个 multipart 部件）上"。
///
/// 名字**取自名单**（受理时按候选声明面与分支算好、随请求冻结），不写死：Profile 把参考图参数
/// 声明成 `image_urls`，这里就返回 `image_urls`，线上跟着它走——平台不改写渠道参数名。
/// 角色仍按候选声明面判（见 [`image_parameter_kind`]），不看取值的形状。
///
/// 名单里找不到这个角色说明该候选表达不了这次请求：调用方据此**明确失败**，绝不退回一个写死的
/// 名字——那会把图塞进上游根本没声明过的字段，而且错得无声无息。
#[must_use]
pub fn platform_image_parameter(
    platform_parameters: &[String],
    kind: ImageParameterKind,
) -> Option<&str> {
    platform_parameters
        .iter()
        .find(|name| image_parameter_kind(name) == Some(kind))
        .map(String::as_str)
}

/// 某个参数名下的图片取值：字符串、或字符串数组。
///
/// `null` 与空串都表示"这一处没有图"，跳过；数组里混进 `null` 或空串同样跳过。
/// 其余条目解释不成一张图，返回 `Err`——调用方据此拒绝这次请求，不猜。
pub fn image_parameter_values<'a>(name: &str, value: &'a Value) -> Result<Vec<&'a str>, String> {
    let mut values = Vec::new();
    match value {
        Value::Null => {}
        Value::String(text) => push_present(&mut values, text),
        Value::Array(items) => {
            for item in items {
                match item {
                    Value::Null => {}
                    Value::String(text) => push_present(&mut values, text),
                    other => {
                        return Err(format!(
                            "image entries in {name} must be strings, got {other}"
                        ));
                    }
                }
            }
        }
        other => {
            return Err(format!(
                "image parameter {name} must be a string or an array of strings, got {other}"
            ));
        }
    }
    Ok(values)
}

/// 遮罩参数下的取值：只有一个字符串，`null` 与空串是"没有遮罩"。
///
/// 遮罩没有数组形态（它就是一块编辑范围），所以数组与其他形状一样按形状不对待。
pub fn mask_value<'a>(name: &str, value: &'a Value) -> Result<Option<&'a str>, String> {
    match value {
        Value::Null => Ok(None),
        Value::String(text) if text.is_empty() => Ok(None),
        Value::String(text) => Ok(Some(text)),
        other => Err(format!(
            "mask parameter {name} must be a string, got {other}"
        )),
    }
}

fn push_present<'a>(values: &mut Vec<&'a str>, text: &'a str) {
    if !text.is_empty() {
        values.push(text);
    }
}

/// 从原生参数里把**平台装载的**参考图与遮罩取回来（Driver 用；与 [`place_image_inputs`] 用
/// 同一份候选声明面约定）。
///
/// 只读**名单里的名字**（见 [`platform_image_parameters`]），一个都不多读：名单外的参数哪怕
/// 叫 `images`、取值就是一张张图，也只是普通的声明参数，这里既不拿它当图片，
/// 也不因为它们让整次请求失败。
pub fn image_inputs(
    parameters: &Value,
    platform_parameters: &[String],
) -> Result<ImageInputs, String> {
    let object = parameters
        .as_object()
        .ok_or_else(|| "native parameters must be an object".to_owned())?;
    let mut inputs = ImageInputs::default();
    for name in platform_parameters {
        let Some(value) = object.get(name) else {
            continue;
        };
        match image_parameter_kind(name) {
            Some(ImageParameterKind::Reference) => {
                inputs.reference_images.extend(
                    image_parameter_values(name, value)?
                        .into_iter()
                        .map(str::to_owned),
                );
            }
            Some(ImageParameterKind::Mask) => {
                inputs.mask = mask_value(name, value)?.map(str::to_owned);
            }
            None => {}
        }
    }
    Ok(inputs)
}

fn synonym_error(names: &[&str]) -> String {
    format!(
        "{} are synonyms; pass only one of them",
        names.join(" and ")
    )
}

#[cfg(test)]
mod tests;
