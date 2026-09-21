//! 图片输入在"调用方给的字段"与"候选自己声明的字段"之间怎么走。
//!
//! 全平台只有这一处讲图片参数的名字与空值，但它回答的是**三个不同的问题**，三套约定不能混：
//!
//! 1. **平台契约面**（受理侧）：平台只认三个字段名——`image` 与 `image_urls`（同义，二选一）
//!    装参考图，`mask` 装遮罩，**精确匹配键名**。这让"哪些字段由平台自己管"成为一份封闭、
//!    可以逐字背下来的清单：其余任何名字（哪怕叫 `image_with_roles`、`mask_url`、`images`）
//!    都不是平台的事，留在请求参数里等下一步按候选声明面处置。
//! 2. **候选声明面**（装载与过滤）：被选中候选的 `capability_schema.properties` 是**这次请求
//!    能带哪些参数名的唯一判据**。装图用它（把调用方的图落到它声明的参数名上：`image`、
//!    `image_urls`、`mask_url`……），过滤也用它：调用方发了但候选没声明的参数名，
//!    **在受理时直接丢掉**——既不报错，也不会出现在上游请求体里。这一面的名字由渠道的 Profile
//!    决定、无法预先枚举，所以装图时按名字的形状认：以 `image` 开头的是参考图、名字里含 `mask`
//!    的是遮罩。
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
//! 为什么未声明的参数是丢掉而不是拒掉、也不是透传：候选的声明面就是这条渠道的参数面，把没声明的
//! 名字发过去，上游要么拒、要么按自己的一套解释成一个平台根本没打算传的东西；而按"一律透传"
//! 放行，等于平台替调用方承诺了一份它没有核对过的上游戏约。丢掉是唯一不留后患的处置，
//! 而且它必须发生在受理期——Driver 拿到的参数面因此**天然是声明面内的子集**。
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

/// 按候选的**声明面**留下参数：只保留该候选 `capability_schema.properties` 里声明过的参数名。
///
/// 这是"发给上游的参数以候选声明面为准"的落地。调用方给的其他名字——渠道文档里有文档的一手参数
/// （`image_with_roles`）、平台没声明的普通参数（`seed`）、纯属多余的 `foo`——在这里**直接丢掉**：
/// 既不报错（多写一个参数不该让整次请求失败），也不会跟着请求走去上游（上游没声明过它，
/// 发过去只会得到上游自己的一套解释）。
///
/// 只对比**顶层属性名**：声明面就是"这个字段名叫什么"的一份清单，平台不去猜它的内部结构——
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
    let declared = schema.get("properties").and_then(Value::as_object);
    names
        .iter()
        .filter(|(name, _)| declared.is_some_and(|properties| properties.contains_key(*name)))
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
mod tests {
    use super::*;

    fn schema(properties: Value) -> Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["model", "prompt"],
            "properties": properties
        })
    }

    #[test]
    fn places_reference_images_on_the_candidates_own_parameter() {
        let vendor = schema(serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"},
            "image_urls": {"type": "array", "items": {"type": "string"}, "maxItems": 4},
            "mask_url": {"type": "string"}
        }));
        let mut parameters = Map::new();
        parameters.insert("prompt".to_owned(), Value::String("x".to_owned()));
        place_image_inputs(
            &vendor,
            &mut parameters,
            &[
                "https://example.invalid/a.png".to_owned(),
                "data:image/png;base64,AAAA".to_owned(),
            ],
            Some("data:image/png;base64,BBBB"),
        )
        .expect("the candidate can express both inputs");
        assert_eq!(
            parameters.get("image_urls"),
            Some(&serde_json::json!([
                "https://example.invalid/a.png",
                "data:image/png;base64,AAAA"
            ]))
        );
        assert_eq!(
            parameters.get("mask_url"),
            Some(&Value::String("data:image/png;base64,BBBB".to_owned()))
        );
    }

    #[test]
    fn a_candidate_that_cannot_express_the_input_is_rejected() {
        let text_only = schema(serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"}
        }));
        let mut parameters = Map::new();
        let error = place_image_inputs(
            &text_only,
            &mut parameters,
            &["https://example.invalid/a.png".to_owned()],
            None,
        )
        .expect_err("a text-only offering cannot take a reference image");
        assert!(error.contains("reference image"), "{error}");
        // 只给遮罩、没有参考图字段时也一样拒绝。
        let error = place_image_inputs(&text_only, &mut parameters, &[], Some("data:,"))
            .expect_err("a text-only offering cannot take a mask");
        assert!(error.contains("mask"), "{error}");
        // 单值字段装不下两张。
        let single = schema(serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"},
            "image": {"type": "string"}
        }));
        assert!(
            place_image_inputs(
                &single,
                &mut Map::new(),
                &["a".to_owned(), "b".to_owned()],
                None
            )
            .is_err()
        );
    }

    #[test]
    fn declared_reference_image_limit_needs_a_promise_from_the_profile() {
        let bounded = schema(serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"},
            "image_urls": {"type": "array", "items": {"type": "string"}, "maxItems": 3}
        }));
        assert_eq!(declared_reference_image_limit(&bounded), Some(3));
        // 数组没写 maxItems ＝ 没有承诺上限，不能据它接受任何收图限制。
        let unbounded = schema(serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"},
            "image_urls": {"type": "array", "items": {"type": "string"}}
        }));
        assert_eq!(declared_reference_image_limit(&unbounded), None);
        // 单值参数按一张算。
        let scalar = schema(serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"},
            "image": {"type": "string"}
        }));
        assert_eq!(declared_reference_image_limit(&scalar), Some(1));
        assert!(!declares_mask_parameter(&scalar));
        assert!(declares_reference_image_parameter(&scalar));
    }

    #[test]
    fn reads_back_what_placement_wrote() {
        let vendor = schema(serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"},
            "image_urls": {"type": "array", "items": {"type": "string"}},
            "mask_url": {"type": "string"}
        }));
        let names = platform_image_parameters(&vendor, crate::ImageBranch::Masked);
        assert_eq!(
            names,
            vec!["image_urls".to_owned(), "mask_url".to_owned()],
            "名单就是候选声明面里那几个名字"
        );
        let parameters = serde_json::json!({
            "model": "m",
            "prompt": "x",
            "image_urls": ["https://example.invalid/a.png", "data:image/png;base64,AAAA"],
            "mask_url": "data:image/png;base64,BBBB"
        });
        let inputs = image_inputs(&parameters, &names).expect("inputs");
        assert_eq!(
            inputs.reference_images,
            vec![
                "https://example.invalid/a.png".to_owned(),
                "data:image/png;base64,AAAA".to_owned()
            ]
        );
        assert_eq!(inputs.mask.as_deref(), Some("data:image/png;base64,BBBB"));
        // 名单里的名字同时也是"平台的图该写到哪儿"的答案：按角色取名字，同样只有名单说了算。
        assert_eq!(
            platform_image_parameter(&names, ImageParameterKind::Reference),
            Some("image_urls")
        );
        assert_eq!(
            platform_image_parameter(&names, ImageParameterKind::Mask),
            Some("mask_url")
        );
        // 名单里没有这个角色：没有名字可用，调用方据此明确失败（不退回写死的名字）。
        assert_eq!(
            platform_image_parameter(&[], ImageParameterKind::Reference),
            None
        );
        // 单值形态同样读得出来。
        let scalar = serde_json::json!({"image": "https://example.invalid/a.png"});
        assert_eq!(
            image_inputs(&scalar, &["image".to_owned()])
                .expect("scalar")
                .reference_images,
            vec!["https://example.invalid/a.png".to_owned()]
        );
        // 名单里没有的名字：不是平台装载的东西，一个都不读。
        let not_ours = serde_json::json!({
            "image_with_roles": [{"role": "reference", "url": "https://example.invalid/a.png"}],
            "images": ["https://example.invalid/b.png"]
        });
        assert_eq!(
            image_inputs(&not_ours, &names).expect("no image is not an error"),
            ImageInputs::default()
        );
    }

    /// 声明面过滤：没声明的参数名一律留下不来，声明的参数（含取值）一字不改。
    #[test]
    fn only_declared_parameter_names_survive_the_filter() {
        let vendor = schema(serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"},
            "quality": {"enum": ["low", "high"]},
            "n": {"type": "integer"},
            "images": {"type": "array", "items": {"type": "string"}}
        }));
        let supplied = Map::from_iter([
            ("model".to_owned(), serde_json::json!("m")),
            ("prompt".to_owned(), serde_json::json!("x")),
            // 声明过的：原样留下，取值不做任何校验或改写。
            ("quality".to_owned(), serde_json::json!("high")),
            ("n".to_owned(), serde_json::json!("not-a-number")),
            // 没声明的：直接丢掉，既不报错也不出现在结果里。
            ("seed".to_owned(), serde_json::json!(7)),
            ("foo".to_owned(), serde_json::json!({"a": 1})),
            (
                "image_with_roles".to_owned(),
                serde_json::json!([{"role": "reference", "url": "https://example.invalid/a.png"}]),
            ),
        ]);
        let kept = declared_parameter_names(&vendor, &supplied);
        assert_eq!(
            kept.keys().collect::<Vec<_>>(),
            vec!["model", "n", "prompt", "quality"],
            "只留声明过的名字"
        );
        assert_eq!(kept.get("quality"), supplied.get("quality"));
        assert_eq!(kept.get("n"), supplied.get("n"));
        assert!(
            supplied.contains_key("seed") && supplied.contains_key("image_with_roles"),
            "过滤不改动入参：平台只是不把它们交给上游"
        );
        // 候选一个属性都没声明（连 properties 都没有）：什么都不留。
        assert_eq!(
            declared_parameter_names(&serde_json::json!({"type": "object"}), &supplied),
            Map::new()
        );
    }

    /// 过滤不会把平台装载的图丢掉：装载用的名字就是从同一份声明面选出来的。
    ///
    /// 顺序也在这里钉住：先按声明面过滤、再装载图片（受理侧就是这么用的），
    /// 图片照样落在候选声明的名字上；反过来先装再滤也是一样结果。
    #[test]
    fn filtering_keeps_the_images_the_platform_placed() {
        let vendor = schema(serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"},
            "image_urls": {"type": "array", "items": {"type": "string"}},
            "mask_url": {"type": "string"}
        }));
        let supplied = Map::from_iter([
            ("prompt".to_owned(), serde_json::json!("x")),
            ("image_with_roles".to_owned(), serde_json::json!([])),
        ]);
        let mut filtered = declared_parameter_names(&vendor, &supplied);
        place_image_inputs(
            &vendor,
            &mut filtered,
            &["data:image/png;base64,AAAA".to_owned()],
            Some("data:image/png;base64,BBBB"),
        )
        .expect("the candidate declares both image parameters");
        assert_eq!(
            filtered.get("image_urls"),
            Some(&serde_json::json!(["data:image/png;base64,AAAA"]))
        );
        assert_eq!(
            filtered.get("mask_url"),
            Some(&serde_json::json!("data:image/png;base64,BBBB"))
        );
        assert!(!filtered.contains_key("image_with_roles"));
        // 名单里的名字必然都在声明面内，过滤前后都留得下。
        for name in platform_image_parameters(&vendor, crate::ImageBranch::Masked) {
            assert!(
                declared_parameter_names(&vendor, &filtered).contains_key(&name),
                "装载的 `{name}` 不会被过滤掉"
            );
        }
        // 先装载再过滤：同一份声明面下结果一致。
        let mut placed = supplied.clone();
        place_image_inputs(
            &vendor,
            &mut placed,
            &["data:image/png;base64,AAAA".to_owned()],
            Some("data:image/png;base64,BBBB"),
        )
        .expect("the candidate declares both image parameters");
        assert_eq!(declared_parameter_names(&vendor, &placed), filtered);
    }

    /// 空值与形状约定：三处读写图片的地方共用这一份，行为必须一模一样。
    #[test]
    fn null_and_empty_values_mean_there_is_no_image() {
        let reference = ["image".to_owned()];
        let with_mask = ["image".to_owned(), "mask".to_owned()];
        for value in [
            serde_json::json!(null),
            serde_json::json!(""),
            serde_json::json!([]),
            serde_json::json!([null]),
            serde_json::json!([""]),
            serde_json::json!(["", null]),
        ] {
            assert_eq!(
                image_inputs(&serde_json::json!({"image": value}), &reference)
                    .expect("no image is not an error"),
                ImageInputs::default(),
                "`{value}` 就是「没有图」"
            );
        }
        // 标量与数组两种形态都读得出来，数组按调用方给的顺序。
        assert_eq!(
            image_inputs(&serde_json::json!({"image": "a"}), &reference).expect("scalar"),
            ImageInputs {
                reference_images: vec!["a".to_owned()],
                mask: None,
            }
        );
        assert_eq!(
            image_inputs(&serde_json::json!({"image": ["a", "b"]}), &reference).expect("array"),
            ImageInputs {
                reference_images: vec!["a".to_owned(), "b".to_owned()],
                mask: None,
            }
        );
        // 数组里的空值只是"这一格没给"，不占位。
        assert_eq!(
            image_inputs(
                &serde_json::json!({"image": ["a", null, "", "b"]}),
                &reference
            )
            .expect("holes"),
            ImageInputs {
                reference_images: vec!["a".to_owned(), "b".to_owned()],
                mask: None,
            }
        );
        // 遮罩同一套约定：空值＝没给。
        assert_eq!(
            image_inputs(&serde_json::json!({"image": "a", "mask": ""}), &with_mask)
                .expect("empty mask"),
            ImageInputs {
                reference_images: vec!["a".to_owned()],
                mask: None,
            }
        );
    }

    /// 归属只看名单：**取值长得再像图**，名字不在名单里就不是平台的图片参数。
    ///
    /// 这正是按取值形状判归属会出错的地方——调用方给渠道的原生名 `images`（字符串数组、
    /// 值就是公网 URL），看起来与平台装载的参考图一模一样。
    #[test]
    fn ownership_comes_from_the_name_list_not_from_the_shape_of_the_value() {
        let vendor = schema(serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"},
            "image_urls": {"type": "array", "items": {"type": "string"}},
            "mask_url": {"type": "string"}
        }));
        // 文生图分支：平台一张图都没装载，名单为空——哪怕是候选声明过的名字也不认领。
        assert!(
            platform_image_parameters(&vendor, crate::ImageBranch::PromptOnly).is_empty(),
            "prompt_only 分支平台不装载任何图"
        );
        // 图生图分支：只有参考图那个名字，遮罩还没进名单。
        assert_eq!(
            platform_image_parameters(&vendor, crate::ImageBranch::ImageConditioned),
            vec!["image_urls".to_owned()]
        );
        // 名单为空时，调用方给的图名参数（`images`）同样不许被当成图片。
        let caller_supplied = serde_json::json!({
            "prompt": "x",
            "images": ["https://example.invalid/b.png"],
            "image_with_roles": [{"role": "reference", "url": "https://example.invalid/a.png"}]
        });
        assert_eq!(
            image_inputs(
                &caller_supplied,
                &platform_image_parameters(&vendor, crate::ImageBranch::PromptOnly)
            )
            .expect("caller supplied parameters are not images"),
            ImageInputs::default()
        );
        // 同一个名字在名单里才被读走，取值形状（标量/数组）不影响归属。
        let placed = serde_json::json!({
            "image_urls": ["https://example.invalid/a.png"],
            "images": ["https://example.invalid/b.png"]
        });
        let inputs = image_inputs(
            &placed,
            &platform_image_parameters(&vendor, crate::ImageBranch::ImageConditioned),
        )
        .expect("inputs");
        assert_eq!(
            inputs.reference_images,
            vec!["https://example.invalid/a.png".to_owned()],
            "名单外的同名参数不参与读图"
        );
    }

    /// 受理侧只认契约字段名：取值解释不成一张图的直接报错，其余名字一个字都不碰。
    #[test]
    fn the_contract_side_reads_exactly_three_names() {
        let mut parameters = serde_json::Map::from_iter([
            ("model".to_owned(), serde_json::json!("m")),
            ("prompt".to_owned(), serde_json::json!("x")),
            ("image_urls".to_owned(), serde_json::json!(["a", "", "b"])),
            (
                "mask".to_owned(),
                serde_json::json!("data:image/png;base64,BBBB"),
            ),
            // 渠道文档里的一手参数：名字以 image 开头、含 mask，但不是平台契约字段。
            (
                "image_with_roles".to_owned(),
                serde_json::json!([{"role": "reference", "url": "https://example.invalid/a.png"}]),
            ),
            (
                "mask_url".to_owned(),
                serde_json::json!("https://example.invalid/m.png"),
            ),
            (
                "images".to_owned(),
                serde_json::json!(["https://example.invalid/b.png"]),
            ),
        ]);
        let inputs = take_contract_image_inputs(&mut parameters).expect("inputs");
        assert_eq!(
            inputs.reference_images,
            vec!["a".to_owned(), "b".to_owned()]
        );
        assert_eq!(inputs.mask.as_deref(), Some("data:image/png;base64,BBBB"));
        // 只摘契约里的三个名字：其余字段留在参数面里，等选路后按候选声明面处置。
        assert_eq!(
            parameters.keys().collect::<Vec<_>>(),
            vec!["image_with_roles", "images", "mask_url", "model", "prompt"]
        );
        // 契约字段的取值解释不成一张图：拒绝，不猜。
        for value in [
            serde_json::json!(7),
            serde_json::json!(true),
            serde_json::json!({"url": "a"}),
            serde_json::json!([7]),
            serde_json::json!(["a", false]),
        ] {
            let mut wrong = serde_json::Map::from_iter([
                ("model".to_owned(), serde_json::json!("m")),
                ("image".to_owned(), value.clone()),
            ]);
            assert!(
                take_contract_image_inputs(&mut wrong).is_err(),
                "`{value}` 不是一张图"
            );
        }
        // 遮罩没有数组形态：给一个数组（哪怕只有一项）也是形状不对。
        for value in [serde_json::json!(["m1"]), serde_json::json!(["m1", "m2"])] {
            let mut wrong = serde_json::Map::from_iter([
                ("image".to_owned(), serde_json::json!("a")),
                ("mask".to_owned(), value),
            ]);
            assert!(take_contract_image_inputs(&mut wrong).is_err());
        }
        // 空值是"没给"，不是错误；空值也不参与同义判定。
        let mut blank = serde_json::Map::from_iter([
            ("image".to_owned(), serde_json::json!("")),
            ("mask".to_owned(), serde_json::json!(null)),
        ]);
        assert_eq!(
            take_contract_image_inputs(&mut blank).expect("blank is not an error"),
            ImageInputs::default()
        );
        assert!(blank.is_empty(), "摘干净的契约字段不再留在参数面里");
    }

    /// `image` 与 `image_urls` 同义：只有两边都给出非空值才算含糊。
    #[test]
    fn synonymous_fields_conflict_only_when_both_carry_a_value() {
        let conflict = |image: Value, image_urls: Value| {
            let mut parameters = serde_json::Map::from_iter([
                ("model".to_owned(), serde_json::json!("m")),
                ("image".to_owned(), image),
                ("image_urls".to_owned(), image_urls),
            ]);
            take_contract_image_inputs(&mut parameters)
        };
        // 两边都有图：含糊，拒绝。
        let error = conflict(
            serde_json::json!("https://example.invalid/a.png"),
            serde_json::json!(["https://example.invalid/b.png"]),
        )
        .expect_err("two synonyms with values");
        assert!(error.contains("synonyms"), "{error}");
        // 一边是空位：另一边说了算，不是冲突。
        for empty in [
            serde_json::json!(null),
            serde_json::json!(""),
            serde_json::json!([]),
            serde_json::json!([null]),
            serde_json::json!([""]),
        ] {
            let inputs = conflict(
                empty.clone(),
                serde_json::json!(["https://example.invalid/b.png"]),
            )
            .unwrap_or_else(|error| panic!("`{empty}` 是空位，不该冲突：{error}"));
            assert_eq!(
                inputs.reference_images,
                vec!["https://example.invalid/b.png".to_owned()]
            );
            let inputs = conflict(
                serde_json::json!("https://example.invalid/a.png"),
                empty.clone(),
            )
            .unwrap_or_else(|error| panic!("`{empty}` 是空位，不该冲突：{error}"));
            assert_eq!(
                inputs.reference_images,
                vec!["https://example.invalid/a.png".to_owned()]
            );
        }
        // 两边都是空位：就是"没有图"。
        assert_eq!(
            conflict(serde_json::json!(null), serde_json::json!([])).expect("no image"),
            ImageInputs::default()
        );
    }

    #[test]
    fn a_name_has_exactly_one_meaning() {
        assert_eq!(
            image_parameter_kind("image_urls"),
            Some(ImageParameterKind::Reference)
        );
        assert_eq!(
            image_parameter_kind("mask_url"),
            Some(ImageParameterKind::Mask)
        );
        assert_eq!(image_parameter_kind("prompt"), None);
        // 两类都像的名字按遮罩归类：遮罩的含义更窄，优先。
        assert_eq!(
            image_parameter_kind("image_mask"),
            Some(ImageParameterKind::Mask)
        );
        assert!(is_reference_image_parameter("image_mask"));
        assert!(is_mask_parameter("image_mask"));
        // 归类变了，候选面据此选到的字段也跟着变。
        let both_names = schema(serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"},
            "image_mask": {"type": "string"}
        }));
        assert!(declares_mask_parameter(&both_names));
        assert!(!declares_reference_image_parameter(&both_names));
        let mut parameters = Map::new();
        place_image_inputs(&both_names, &mut parameters, &[], Some("data:,"))
            .expect("image_mask 是遮罩字段，装得下这块编辑范围");
        assert_eq!(
            parameters.get("image_mask"),
            Some(&serde_json::json!("data:,"))
        );
    }

    /// 契约面与候选面是两件事：同一个名字在两面里的归属可以不同。
    #[test]
    fn the_contract_side_does_not_use_the_shape_of_the_name() {
        assert_eq!(
            contract_image_parameter_kind("image"),
            Some(ImageParameterKind::Reference)
        );
        assert_eq!(
            contract_image_parameter_kind("image_urls"),
            Some(ImageParameterKind::Reference)
        );
        assert_eq!(
            contract_image_parameter_kind("mask"),
            Some(ImageParameterKind::Mask)
        );
        for name in ["image_with_roles", "mask_url", "images", "image_mask"] {
            assert_eq!(contract_image_parameter_kind(name), None, "{name}");
            // 但它们在候选面里仍然按名字形状认。
            assert!(image_parameter_kind(name).is_some(), "{name}");
        }
    }
}
