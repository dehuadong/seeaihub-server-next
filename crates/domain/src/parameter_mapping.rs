//! 供给的参数映射：把**合同值**转成**渠道包装**的声明，目前有四块——显式默认值、改名、
//! 字段取值映射与尺寸换算。四块都只回答"这次请求在这条供给上长什么样"，谁也不是通用映射引擎，
//! 只是这份声明里已经用到的那几个键。
//!
//! **显式默认值**：同一个字段在各家渠道各有自己的一套默认（上游把水印默认打开就是例子）。
//! 调用方没给这个字段时，平台若原样不发，最终效果就由**渠道的默认值**决定——同一份请求在两个
//! 渠道上得到不同的东西，而这个差异调用方既看不见也管不了。映射里声明的默认值让"调用方没给"
//! 这件事由平台定下来，渠道那套默认值就再也用不上。
//!
//! **改名**：同一个字段在合同里叫一个名字，某条供给的线上却叫另一个名字。承载面声明的是
//! **线上字段名**（它写的就是线上要发的东西），于是"合同里的 `size`"在这条供给上根本没有对应的
//! 声明——按名字直判会把它当成承载不了，而它其实只是换了个名字。改名表就是这两套名字之间的桥。
//!
//! **字段取值映射**：同一个字段的取值在两套体系里各有一套说法（合同写 `high`、线上要 `xhigh`）。
//! 映射表把合同取值翻成线上取值；表里没有的取值**明确失败**，既不猜近似值、也不把合同取值原样
//! 透传——透传出去的那个取值对这条渠道没有意义，而调用方以为平台已经按声明映射过了。
//!
//! **尺寸换算**：同一个字段名（`size`）在不同模型上是三种具体的东西（像素 / 比例 / 档位），客户端
//! 按合同提交一型、渠道可能要另一型。映射声明这条供给要哪一型、由合同里的哪几个字段喂它、
//! 写进线上哪个字段，以及（需要查表时）随发布携带的那份**比例 × 档位 → 像素**档案。
//! 档案放在这里而不是合同里：换算表是**这条供给**为了把合同形态变成自己要的形态才需要的，
//! 同一个模型的像素面渠道与比例+档位面渠道各要各的一份；合同是模型级唯一一份、落库后不再改，
//! 把渠道差异抬进合同会让"换一个渠道"变成"改模型合同、发新修订"。
//!
//! 尺寸取值还有第四型 `auto`（"由模型自己决定"）：它只原样透传、永不换算，所以声明了尺寸换算的
//! 供给收不了它（除非它声明的目标形态本身就是 `auto`）。
//!
//! 注入默认值的判据是**合同 + 这条供给承载得了**：两边都成立才注入。合同没声明，说明调用方根本
//! 提交不了它，注入等于平台凭空多出一个调用方看不见的参数；承载不了（承载面没声明、改名也没把它
//! 落到一个声明的名字上），说明这条供给发不出去它，注入只会得到上游自己的一套解释。两种情况都
//! **不注入、不报错**——一次请求不该为一份写歪的映射失败。发布期另有一条校验把这些"声明了却
//! 做不到"的键拦下来（见应用层的参数映射校验），受理期因此不必为它报错。
//!
//! 改名与取值映射不一样：它们要么落到线上、要么明确失败。声明写歪（改名的源不在合同里、目标不在
//! 承载面里、表不成形状）在**发布期**就被拒绝——映射是发布数据，写歪了不该被发布出去；而"取值
//! 映射表里没有这个取值"是受理期的事实，那时这条候选不合格（换下一条候选，或明确报平台侧故障）。
//!
//! 尺寸换算同理：要么换算出一个值、要么明确失败；发布期拦"声明写歪"，受理期判"档案里缺这一格"。

use crate::size_spec::{SizeForm, SizeProfile, SizeSpec, convert_size};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// 映射里声明**显式默认值**的键：形如 `{"defaults": {"watermark": false}}`。
pub const PARAMETER_DEFAULTS_KEY: &str = "defaults";

/// 这份映射声明的显式默认值；没有声明（或声明得不成形状）时是 `None`。
#[must_use]
pub fn declared_defaults(mapping: &Value) -> Option<&Map<String, Value>> {
    mapping.get(PARAMETER_DEFAULTS_KEY)?.as_object()
}

/// 一份 schema 声明的**顶层字段名**清单：`properties` 的键，加上 `required` 里列出的名字。
///
/// 两处都算：只认 `properties` 会漏掉"只写进 `required`"的名字——它在 JSON Schema 里是合法的
/// 声明（那个键必须出现），漏掉它会让校验看起来通过了、实际却把调用方给的一个声明字段丢掉。
#[must_use]
pub fn declared_field_names(schema: &Value) -> Vec<&str> {
    let mut names: Vec<&str> = schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| properties.keys().map(String::as_str).collect())
        .unwrap_or_default();
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for name in required.iter().filter_map(Value::as_str) {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names
}

/// 一份 schema 是否声明了这个**顶层字段名**（判据与 [`declared_field_names`] 是同一份）。
#[must_use]
pub fn declares_parameter(schema: &Value, name: &str) -> bool {
    declared_field_names(schema).contains(&name)
}

/// 这个取值算不算"调用方**真的用了**这个字段"。
///
/// `null` 与空串是"这一处没给"（全平台共用的空值约定），空数组是"一个都没给"（与图片列表同一套）。
/// 空对象**算给了**：平台不猜它的内部结构，调用方写了一个对象就是写了。
///
/// 判据不看取值的类型对不对——取值本身从来不校验，这里只回答"有没有"。
#[must_use]
pub fn is_used_parameter_value(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Bool(_) | Value::Number(_) | Value::Object(_) => true,
    }
}

/// 把映射声明的显式默认值注入参数面：**调用方没给、合同声明了、且这条供给承载得了**该字段才注入。
///
/// "调用方没给"含空值：`null` 与空串同样按"没给"处理，否则调用方写一个空位就能把平台的默认值
/// 顶掉，而那个空位到上游那里又什么都不表示。
///
/// 已经给了的字段**一个字都不改**：默认值只在调用方没说话的地方替它说话。
///
/// 传进来的参数面此时还在**合同名字**上（改名在注入之后才做），所以这里按合同字段名找"给了没有"。
pub fn apply_parameter_defaults(
    contract: &Value,
    carrier: &Value,
    renames: Option<&ParameterRenames>,
    defaults: Option<&Map<String, Value>>,
    parameters: &mut Map<String, Value>,
) {
    let Some(defaults) = defaults else {
        return;
    };
    for (name, value) in defaults {
        if !declares_parameter(contract, name) || !carries_parameter(carrier, renames, name) {
            continue;
        }
        if parameters
            .get(name)
            .is_none_or(|given| !is_used_parameter_value(given))
        {
            parameters.insert(name.clone(), value.clone());
        }
    }
}

// ── 改名：合同字段名 → 线上字段名 ────────────────────────────────────────────

/// 映射里声明**改名**的键：形如 `{"rename": {"size": "resolution"}}`（合同字段名 → 线上字段名）。
pub const PARAMETER_RENAME_KEY: &str = "rename";

/// 一条供给的**改名表**：合同字段名 → 线上字段名。
pub type ParameterRenames = BTreeMap<String, String>;

/// 一条供给的**取值映射表**：合同字段名 → （合同取值 → 线上取值）。
pub type ParameterEnumMaps = BTreeMap<String, Map<String, Value>>;

/// 这份映射声明的**改名表**；没有声明（或显式写 `null`）时是 `None`。
///
/// 声明得不成形状一律报错而不是按"没有声明"处理：那会把一份写歪的声明变成"字段名原样上行"，
/// 而调用方与运营都以为改名发生了。
pub fn declared_renames(mapping: &Value) -> Result<Option<ParameterRenames>, String> {
    let Some(declared) = mapping.get(PARAMETER_RENAME_KEY) else {
        return Ok(None);
    };
    if declared.is_null() {
        return Ok(None);
    }
    let declared = declared
        .as_object()
        .ok_or_else(|| format!("{PARAMETER_RENAME_KEY} must be an object"))?;
    let mut renames = BTreeMap::new();
    for (contract_name, wire_name) in declared {
        if contract_name.is_empty() {
            return Err(format!(
                "{PARAMETER_RENAME_KEY} keys must be non-empty field names"
            ));
        }
        let wire_name = wire_name
            .as_str()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| {
                format!("{PARAMETER_RENAME_KEY}.{contract_name} must be a non-empty field name")
            })?;
        renames.insert(contract_name.clone(), wire_name.to_owned());
    }
    Ok(Some(renames))
}

/// 这条供给线上写这个合同字段时用的名字。
///
/// 承载面自己声明了这个名字就用它；否则看改名表把它映射到哪个名字，**那个名字必须被承载面
/// 声明**——映射到一个承载面都没声明的名字，等于把一个发不出去的字段名当作能承载。
///
/// 承载面的声明优先：供给说了自己线上就叫这个名字时，改名表对这个字段不生效（它本来就没打算改名）。
/// 因此"这条供给承载不了这个字段"的答案只有一种：承载面没声明它，改名也没把它落到一个声明上。
#[must_use]
pub fn wire_parameter_name(
    carrier: &Value,
    renames: Option<&ParameterRenames>,
    name: &str,
) -> Option<String> {
    if declares_parameter(carrier, name) {
        return Some(name.to_owned());
    }
    let wire = renames?.get(name)?;
    declares_parameter(carrier, wire).then(|| wire.clone())
}

/// 这条供给承载得了这个合同字段吗：承载面直接声明了它，或改名把它落到一个声明上。
#[must_use]
pub fn carries_parameter(carrier: &Value, renames: Option<&ParameterRenames>, name: &str) -> bool {
    wire_parameter_name(carrier, renames, name).is_some()
}

/// 把参数面里的合同字段名换成这条供给线上要发的名字（就地改键，取值一字不动）。
///
/// 只改**承载面没声明**的名字，与 [`wire_parameter_name`] 是同一条规则。两个合同字段落到同一个
/// 线上名字（目标上已经有值）时**报错**而不是挑一个：平台不猜调用方想留哪一个，这条候选因此不合格。
pub fn apply_parameter_renames(
    carrier: &Value,
    renames: Option<&ParameterRenames>,
    parameters: &mut Map<String, Value>,
) -> Result<(), String> {
    let Some(renames) = renames else {
        return Ok(());
    };
    // 先收集要改的名字，不边遍历边改。
    let moves: Vec<(String, String)> = parameters
        .keys()
        .filter(|name| !declares_parameter(carrier, name))
        .filter_map(|name| {
            let wire = renames.get(name)?;
            (wire != name && declares_parameter(carrier, wire))
                .then(|| (name.clone(), wire.clone()))
        })
        .collect();
    for (name, wire) in moves {
        if parameters.contains_key(&wire) {
            return Err(format!(
                "this offering writes both {name} and {wire} on the wire; it cannot carry this request"
            ));
        }
        if let Some(value) = parameters.remove(&name) {
            parameters.insert(wire, value);
        }
    }
    Ok(())
}

// ── 字段取值映射：合同值 → 线上值 ────────────────────────────────────────────

/// 映射里声明**字段取值映射**的键：形如
/// `{"enum_map": {"quality": {"high": "xhigh", "low": "low"}}}`。
pub const PARAMETER_ENUM_MAP_KEY: &str = "enum_map";

/// 这份映射声明的**取值映射表**：合同字段名 → （合同取值 → 线上取值）；没有声明时是 `None`。
///
/// 表的形状：外层键是合同字段名，内层键是调用方会提交的那个取值，内层值是线上要发的取值。
/// 内层值不限于字符串——线上那个字段未必是字符串字段。
pub fn declared_enum_maps(mapping: &Value) -> Result<Option<ParameterEnumMaps>, String> {
    let Some(declared) = mapping.get(PARAMETER_ENUM_MAP_KEY) else {
        return Ok(None);
    };
    if declared.is_null() {
        return Ok(None);
    }
    let declared = declared
        .as_object()
        .ok_or_else(|| format!("{PARAMETER_ENUM_MAP_KEY} must be an object"))?;
    let mut tables = BTreeMap::new();
    for (field, table) in declared {
        if field.is_empty() {
            return Err(format!(
                "{PARAMETER_ENUM_MAP_KEY} keys must be non-empty field names"
            ));
        }
        let table = table.as_object().ok_or_else(|| {
            format!("{PARAMETER_ENUM_MAP_KEY}.{field} must be an object of value mappings")
        })?;
        tables.insert(field.clone(), table.clone());
    }
    Ok(Some(tables))
}

/// 按取值映射表把参数面上的取值换成线上取值。
///
/// 只处理**表里声明的字段**，且只处理参数面上真的带着取值的那些（调用方没给、也没被默认值补上的
/// 字段无事可做）。字段在参数面上的名字按 [`wire_parameter_name`] 找——参数面此时已经改过名。
///
/// **表里没有的取值 → 明确失败**（这条候选不合格）：既不猜一个近似值，也不把合同取值原样透传——
/// 透传出去的那个取值对这条渠道没有意义，上游要么拒、要么按自己的一套解释，而调用方以为平台
/// 已经按声明映射过了。取值不是字符串时同样算"表里没有"：表的内层键只能是字符串，它匹配不上。
pub fn apply_enum_maps(
    carrier: &Value,
    renames: Option<&ParameterRenames>,
    enum_maps: &ParameterEnumMaps,
    parameters: &mut Map<String, Value>,
) -> Result<(), String> {
    for (name, table) in enum_maps {
        let Some(wire) = wire_parameter_name(carrier, renames, name) else {
            continue;
        };
        let Some(value) = parameters.get(&wire) else {
            continue;
        };
        if !is_used_parameter_value(value) {
            continue;
        }
        let mapped = value
            .as_str()
            .and_then(|value| table.get(value))
            .ok_or_else(|| {
                format!("this offering declares no wire value for {name}={value}, so it cannot carry this request")
            })?;
        parameters.insert(wire, mapped.clone());
    }
    Ok(())
}

/// 映射里声明**尺寸换算**的键：形如
/// `{"size": {"source": ["size", "resolution"], "target": "size", "form": "pixels", "profile": {...}}}`。
pub const PARAMETER_SIZE_KEY: &str = "size";

/// 一条供给的尺寸换算声明：它要哪一型、由合同里的哪几个字段喂它、写进线上哪个字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SizeMapping {
    /// 合同里参与尺寸的字段名，按声明序。这些字段是换算的**输入**，不再原样上行。
    pub source: Vec<String>,
    /// 换算结果写进线上的哪个字段名；它必须被这条供给的承载面声明。
    pub target: String,
    /// 这条供给要的那一型。
    pub form: SizeForm,
    /// 比例 × 档位 → 像素的档案；只在需要查表（像素 ↔ 比例+档位）时用得上。
    pub profile: SizeProfile,
}

impl SizeMapping {
    /// 这个合同字段是不是被尺寸换算**消耗**掉的（它不再原样上行）。
    ///
    /// 承载校验据此放行：请求用到了 `resolution`，但这条供给把它当作换算的输入而不是要发的
    /// 字段——供给表达得了它，只是表达成另一个样子。渠道线上写不出这个名字（例如像素面渠道
    /// 根本没有 `resolution`）时，这一点尤其重要。
    #[must_use]
    pub fn consumes(&self, name: &str) -> bool {
        self.source.iter().any(|source| source == name)
    }
}

/// 这份映射声明的尺寸换算；没有声明（或声明得不成形状）时分别返回 `None` 与 `Err`。
///
/// 声明得不成形状一律报错而不是按"没有声明"处理：那会把一份写歪的声明变成"尺寸原样上行"，
/// 而调用方与运营都以为换算发生了。
pub fn declared_size_mapping(mapping: &Value) -> Result<Option<SizeMapping>, String> {
    let Some(declared) = mapping.get(PARAMETER_SIZE_KEY) else {
        return Ok(None);
    };
    if declared.is_null() {
        return Ok(None);
    }
    let declared = declared
        .as_object()
        .ok_or_else(|| format!("{PARAMETER_SIZE_KEY} must be an object"))?;
    let source = declared
        .get("source")
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .map(|name| {
                    name.as_str()
                        .filter(|name| !name.is_empty())
                        .map(str::to_owned)
                        .ok_or_else(|| "size.source must contain non-empty field names".to_owned())
                })
                .collect::<Result<Vec<String>, String>>()
        })
        .transpose()?
        .filter(|names| !names.is_empty())
        .ok_or_else(|| "size.source is required and must list the contract fields".to_owned())?;
    let target = declared
        .get("target")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "size.target is required".to_owned())?
        .to_owned();
    let form = declared
        .get("form")
        .and_then(Value::as_str)
        .and_then(SizeForm::parse)
        .ok_or_else(|| {
            format!(
                "size.form is required and must be one of {}",
                SizeForm::ALL.map(SizeForm::as_str).join(", ")
            )
        })?;
    let profile = match declared.get("profile") {
        None | Some(Value::Null) => SizeProfile::default(),
        Some(profile) => SizeProfile::from_json(profile)?,
    };
    Ok(Some(SizeMapping {
        source,
        target,
        form,
        profile,
    }))
}

/// 按尺寸声明把参数面换算成这条供给要的形态。
///
/// 输入取值优先从**合同面**读（`contract_parameters`），不在那里时退到过滤后的参数面
/// （`parameters`）：承载面没声明的源字段在过滤时就没了，而换算恰恰要用它们；反过来，平台在
/// 承载面上补的**显式默认值**（例如"调用方没给档位时按 2K 算"）只在后者里，它也是一个声明出来
/// 的值、不是平台猜的，所以照样参与换算。
///
/// 结果写进 `parameters` 的 `target` 上，并把**所有**被消耗的源字段从中摘掉——它们的取值已经变成
/// 目标字段的一部分，再发一遍等于把同一个尺寸说两次（默认值补进来的也在内）。
///
/// 这次请求**没用到**任何源字段时什么都不做：没有尺寸要换算，也不该凭档案替调用方造一个。
pub fn apply_size_mapping(
    mapping: &SizeMapping,
    contract_parameters: &Map<String, Value>,
    parameters: &mut Map<String, Value>,
) -> Result<(), String> {
    let mut values = Vec::new();
    for name in &mapping.source {
        let value = contract_parameters
            .get(name)
            .filter(|value| is_used_parameter_value(value))
            .or_else(|| {
                parameters
                    .get(name)
                    .filter(|value| is_used_parameter_value(value))
            });
        let Some(value) = value else {
            continue;
        };
        let text = value
            .as_str()
            .ok_or_else(|| format!("size parameter {name} must be a string, got {value}"))?;
        values.push(
            SizeSpec::parse(text).map_err(|reason| format!("size parameter {name}: {reason}"))?,
        );
    }
    if values.is_empty() {
        return Ok(());
    }
    let converted = convert_size(mapping.form, &mapping.profile, &values)?;
    for name in &mapping.source {
        if *name != mapping.target {
            parameters.remove(name);
        }
    }
    parameters.insert(mapping.target.clone(), Value::String(converted.to_string()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(properties: Value) -> Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": properties
        })
    }

    #[test]
    fn a_declared_field_counts_from_properties_or_required() {
        let with_required_only = serde_json::json!({
            "type": "object",
            "required": ["model"],
            "properties": {"prompt": {"type": "string"}}
        });
        assert!(declares_parameter(&with_required_only, "model"));
        assert!(declares_parameter(&with_required_only, "prompt"));
        assert!(!declares_parameter(&with_required_only, "quality"));
        // 清单与判据是同一份：`properties` 的键在前，只写进 `required` 的名字在后。
        assert_eq!(
            declared_field_names(&with_required_only),
            vec!["prompt", "model"]
        );
        // 连 properties 都没有：一个名字都不算声明。
        assert!(!declares_parameter(
            &serde_json::json!({"type": "object"}),
            "model"
        ));
        assert!(declared_field_names(&serde_json::json!({"type": "object"})).is_empty());
    }

    #[test]
    fn only_values_that_carry_something_count_as_used() {
        for empty in [
            serde_json::json!(null),
            serde_json::json!(""),
            serde_json::json!([]),
        ] {
            assert!(!is_used_parameter_value(&empty), "`{empty}` 是没给");
        }
        for used in [
            serde_json::json!("low"),
            serde_json::json!(0),
            serde_json::json!(false),
            serde_json::json!([1]),
            // 空对象算给了：平台不猜它的内部结构。
            serde_json::json!({}),
        ] {
            assert!(is_used_parameter_value(&used), "`{used}` 是给了");
        }
    }

    #[test]
    fn defaults_fill_only_the_fields_the_caller_left_empty() {
        let contract = schema(serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"},
            "watermark": {"type": "boolean"}
        }));
        let carrier = contract.clone();
        let mapping = serde_json::json!({"defaults": {"watermark": false}});
        let renames = declared_renames(&mapping).expect("no rename declared");
        let defaults = declared_defaults(&mapping);

        // 调用方没给：注入默认值。
        let mut parameters = Map::from_iter([("prompt".to_owned(), serde_json::json!("x"))]);
        apply_parameter_defaults(
            &contract,
            &carrier,
            renames.as_ref(),
            defaults,
            &mut parameters,
        );
        assert_eq!(parameters.get("watermark"), Some(&serde_json::json!(false)));

        // 调用方给了：用调用方的值，一个字都不改。
        let mut parameters = Map::from_iter([
            ("prompt".to_owned(), serde_json::json!("x")),
            ("watermark".to_owned(), serde_json::json!(true)),
        ]);
        apply_parameter_defaults(
            &contract,
            &carrier,
            renames.as_ref(),
            defaults,
            &mut parameters,
        );
        assert_eq!(parameters.get("watermark"), Some(&serde_json::json!(true)));

        // 调用方写了个空位：那也是"没给"，默认值照旧生效。
        for empty in [serde_json::json!(null), serde_json::json!("")] {
            let mut parameters = Map::from_iter([
                ("prompt".to_owned(), serde_json::json!("x")),
                ("watermark".to_owned(), empty.clone()),
            ]);
            apply_parameter_defaults(
                &contract,
                &carrier,
                renames.as_ref(),
                defaults,
                &mut parameters,
            );
            assert_eq!(
                parameters.get("watermark"),
                Some(&serde_json::json!(false)),
                "`{empty}` 是空位，默认值该生效"
            );
        }
    }

    /// 合同与承载面**都**声明了才注入：只有一边声明时，注入出去的东西平台自己都说不清。
    #[test]
    fn defaults_are_not_injected_outside_the_contract_and_the_carrier() {
        let contract = schema(serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"},
            "watermark": {"type": "boolean"}
        }));
        // 承载面收窄：它承载不了 `watermark`。
        let carrier = schema(serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"}
        }));
        let mapping = serde_json::json!({"defaults": {"watermark": false, "seed": 7}});
        let renames = declared_renames(&mapping).expect("no rename declared");
        let defaults = declared_defaults(&mapping);
        let mut parameters = Map::from_iter([("prompt".to_owned(), serde_json::json!("x"))]);
        apply_parameter_defaults(
            &contract,
            &carrier,
            renames.as_ref(),
            defaults,
            &mut parameters,
        );
        assert!(
            parameters.get("watermark").is_none(),
            "承载面没声明就不注入：{parameters:?}"
        );
        assert!(
            parameters.get("seed").is_none(),
            "合同与承载面都没声明就更不该注入：{parameters:?}"
        );

        // 合同没声明（承载面也不可能声明，承载面是合同的子集）：同样不注入。
        let mut parameters = Map::from_iter([("prompt".to_owned(), serde_json::json!("x"))]);
        apply_parameter_defaults(
            &carrier,
            &carrier,
            renames.as_ref(),
            defaults,
            &mut parameters,
        );
        assert!(parameters.get("watermark").is_none());
    }

    /// 默认值的键经**改名**落到承载面声明的名字上时照旧注入：那条供给承载得了它，只是线上叫
    /// 另一个名字。注入仍然发生在合同名字上，改名在注入之后才做。
    #[test]
    fn a_default_for_a_field_the_offering_carries_under_another_name_is_injected() {
        let contract = schema(serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"},
            "quality": {"type": "string"}
        }));
        // 承载面写的是**线上字段名**：它线上叫 `xquality`。
        let carrier = schema(serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"},
            "xquality": {"type": "string"}
        }));
        let mapping =
            serde_json::json!({"defaults": {"quality": "low"}, "rename": {"quality": "xquality"}});
        let renames = declared_renames(&mapping).expect("a rename is declared");
        let defaults = declared_defaults(&mapping);
        let mut parameters = Map::from_iter([("prompt".to_owned(), serde_json::json!("x"))]);
        apply_parameter_defaults(
            &contract,
            &carrier,
            renames.as_ref(),
            defaults,
            &mut parameters,
        );
        assert_eq!(
            parameters.get("quality"),
            Some(&serde_json::json!("low")),
            "注入按合同名字落：{parameters:?}"
        );
        apply_parameter_renames(&carrier, renames.as_ref(), &mut parameters).expect("renamed");
        assert_eq!(parameters.get("xquality"), Some(&serde_json::json!("low")));
        assert!(parameters.get("quality").is_none());
    }

    #[test]
    fn a_mapping_without_defaults_changes_nothing() {
        let contract = schema(serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"}
        }));
        let mut parameters = Map::from_iter([("prompt".to_owned(), serde_json::json!("x"))]);
        let before = parameters.clone();
        for mapping in [
            serde_json::json!({}),
            // 声明得不成形状（不是对象）：按"没有声明"处理，不报错。
            serde_json::json!({"defaults": ["watermark"]}),
            serde_json::json!({"renames": {"watermark": "wm"}}),
        ] {
            let renames = declared_renames(&mapping).expect("no rename declared");
            let defaults = declared_defaults(&mapping);
            apply_parameter_defaults(
                &contract,
                &contract,
                renames.as_ref(),
                defaults,
                &mut parameters,
            );
            assert_eq!(parameters, before, "映射 {mapping} 不该改动参数面");
        }
        assert!(declared_defaults(&serde_json::json!({})).is_none());
    }

    // ── 改名与取值映射 ──────────────────────────────────────────────────────

    /// 承载判据：承载面声明了这个名字就用它；没声明时看改名表能不能把它落到一个声明的名字上。
    #[test]
    fn a_rename_bridges_the_contract_name_to_the_name_written_on_the_wire() {
        let carrier = schema(serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"},
            "resolution": {"type": "string"}
        }));
        let renames = declared_renames(&serde_json::json!({
            "rename": {"size": "resolution", "prompt": "prompt"}
        }))
        .expect("the declaration parses")
        .expect("a rename is declared");

        // 承载面直接声明的名字：原样用。
        assert_eq!(
            wire_parameter_name(&carrier, Some(&renames), "resolution"),
            Some("resolution".to_owned())
        );
        // 承载面没声明、但改名落到了一个声明的名字上：承载得了，线上用那个名字。
        assert_eq!(
            wire_parameter_name(&carrier, Some(&renames), "size"),
            Some("resolution".to_owned())
        );
        assert!(carries_parameter(&carrier, Some(&renames), "size"));
        // 两边都没有：承载不了。
        assert_eq!(
            wire_parameter_name(&carrier, Some(&renames), "quality"),
            None
        );
        assert!(!carries_parameter(&carrier, Some(&renames), "quality"));
        // 没有改名表时只有承载面声明过的名字算数。
        assert_eq!(wire_parameter_name(&carrier, None, "size"), None);
        // 改名落到一个承载面都没声明的名字上：等于发不出去，不算承载。
        let dangling = declared_renames(&serde_json::json!({"rename": {"size": "aspect_ratio"}}))
            .expect("the declaration parses")
            .expect("a rename is declared");
        assert_eq!(wire_parameter_name(&carrier, Some(&dangling), "size"), None);
    }

    /// 改名只改**承载面没声明**的名字，取值一字不动；两个合同字段落到同一个线上名字时明确失败。
    #[test]
    fn renaming_moves_only_the_names_the_carrier_does_not_declare() {
        let carrier = schema(serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"},
            "resolution": {"type": "string"}
        }));
        let renames = declared_renames(&serde_json::json!({
            "rename": {"size": "resolution", "quality": "resolution"}
        }))
        .expect("the declaration parses")
        .expect("a rename is declared");

        let mut parameters = Map::from_iter([
            ("model".to_owned(), serde_json::json!("m")),
            ("prompt".to_owned(), serde_json::json!("x")),
            ("size".to_owned(), serde_json::json!("2:3")),
        ]);
        apply_parameter_renames(&carrier, Some(&renames), &mut parameters).expect("renamed");
        assert_eq!(
            parameters.get("resolution"),
            Some(&serde_json::json!("2:3"))
        );
        assert!(!parameters.contains_key("size"));
        assert_eq!(parameters.get("prompt"), Some(&serde_json::json!("x")));

        // 承载面自己声明了 `resolution`：改名表对那个名字不生效，它原样留着。
        let mut parameters = Map::from_iter([
            ("resolution".to_owned(), serde_json::json!("1k")),
            ("prompt".to_owned(), serde_json::json!("x")),
        ]);
        apply_parameter_renames(&carrier, Some(&renames), &mut parameters).expect("renamed");
        assert_eq!(
            parameters.get("resolution"),
            Some(&serde_json::json!("1k")),
            "承载面声明的名字优先：{parameters:?}"
        );

        // 两个合同字段落到同一个线上名字：平台不猜留哪一个，明确失败。
        let mut parameters = Map::from_iter([
            ("size".to_owned(), serde_json::json!("2:3")),
            ("quality".to_owned(), serde_json::json!("high")),
        ]);
        let error = apply_parameter_renames(&carrier, Some(&renames), &mut parameters)
            .expect_err("two contract fields cannot share one wire name");
        assert!(error.contains("resolution"), "{error}");

        // 没有改名表：一个名字都不动。
        let mut untouched = Map::from_iter([("size".to_owned(), serde_json::json!("2:3"))]);
        let before = untouched.clone();
        apply_parameter_renames(&carrier, None, &mut untouched).expect("nothing to rename");
        assert_eq!(untouched, before);
    }

    #[test]
    fn a_rename_declaration_that_is_not_shaped_like_one_is_rejected() {
        for (mapping, needle) in [
            (serde_json::json!({"rename": "size"}), "must be an object"),
            (
                serde_json::json!({"rename": {"size": 7}}),
                "must be a non-empty field name",
            ),
            (
                serde_json::json!({"rename": {"size": ""}}),
                "must be a non-empty field name",
            ),
        ] {
            let error = declared_renames(&mapping)
                .err()
                .unwrap_or_else(|| panic!("{mapping} 不是一份改名声明"));
            assert!(error.contains(needle), "{mapping} → {error}");
        }
        // 没有声明（键缺失或显式写 null）＝ 这条供给不改名。
        assert_eq!(
            declared_renames(&serde_json::json!({"defaults": {"watermark": false}})),
            Ok(None)
        );
        assert_eq!(
            declared_renames(&serde_json::json!({"rename": null})),
            Ok(None)
        );
        // 改名成同一个名字是合法的空操作（它不改变任何东西）。
        assert_eq!(
            declared_renames(&serde_json::json!({"rename": {"size": "size"}})),
            Ok(Some(BTreeMap::from_iter([(
                "size".to_owned(),
                "size".to_owned()
            )])))
        );
    }

    /// 取值映射：合同取值换成线上取值；表里没有的取值**明确失败**，不猜也不透传原值。
    #[test]
    fn an_enum_map_replaces_the_value_and_a_missing_one_is_an_error() {
        let carrier = schema(serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"},
            "xquality": {"type": "string"}
        }));
        let mapping = serde_json::json!({
            "rename": {"quality": "xquality"},
            "enum_map": {"quality": {"high": "xhigh", "low": "low"}}
        });
        let renames = declared_renames(&mapping).expect("a rename is declared");
        let enum_maps = declared_enum_maps(&mapping)
            .expect("the declaration parses")
            .expect("an enum map is declared");

        // 合同名字上是 `high`，线上那个名字上是映射后的 `xhigh`。
        let mut parameters = Map::from_iter([("xquality".to_owned(), serde_json::json!("high"))]);
        apply_enum_maps(&carrier, renames.as_ref(), &enum_maps, &mut parameters)
            .expect("high is in the table");
        assert_eq!(
            parameters.get("xquality"),
            Some(&serde_json::json!("xhigh")),
            "{parameters:?}"
        );

        // 表里没有这个取值：明确失败，参数面不许留下一个半映射的值。
        for value in [serde_json::json!("medium"), serde_json::json!(7)] {
            let mut parameters = Map::from_iter([("xquality".to_owned(), value.clone())]);
            let error = apply_enum_maps(&carrier, renames.as_ref(), &enum_maps, &mut parameters)
                .expect_err("a value outside the table must fail");
            assert!(error.contains("quality"), "{value} → {error}");
            assert_eq!(parameters.get("xquality"), Some(&value));
        }

        // 字段没给（不在参数面上）或给了个空位：无事可做。
        let mut parameters = Map::from_iter([("prompt".to_owned(), serde_json::json!("x"))]);
        let before = parameters.clone();
        apply_enum_maps(&carrier, renames.as_ref(), &enum_maps, &mut parameters)
            .expect("nothing to map");
        assert_eq!(parameters, before);
        let mut parameters = Map::from_iter([("xquality".to_owned(), serde_json::json!(""))]);
        let before = parameters.clone();
        apply_enum_maps(&carrier, renames.as_ref(), &enum_maps, &mut parameters)
            .expect("an empty value is not a value");
        assert_eq!(parameters, before);

        // 承载面承载不了的字段：这张表对它无事可做（请求用到它时早就判过不合格了）。
        let narrow = schema(serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"}
        }));
        let mut parameters = Map::from_iter([("quality".to_owned(), serde_json::json!("high"))]);
        let before = parameters.clone();
        apply_enum_maps(&narrow, renames.as_ref(), &enum_maps, &mut parameters)
            .expect("the offering cannot carry quality at all");
        assert_eq!(parameters, before);
    }

    #[test]
    fn an_enum_map_declaration_that_is_not_shaped_like_one_is_rejected() {
        for (mapping, needle) in [
            (
                serde_json::json!({"enum_map": "quality"}),
                "must be an object",
            ),
            (
                serde_json::json!({"enum_map": {"quality": ["high"]}}),
                "must be an object of value mappings",
            ),
        ] {
            let error = declared_enum_maps(&mapping)
                .err()
                .unwrap_or_else(|| panic!("{mapping} 不是一份取值映射声明"));
            assert!(error.contains(needle), "{mapping} → {error}");
        }
        assert_eq!(declared_enum_maps(&serde_json::json!({})), Ok(None));
        assert_eq!(
            declared_enum_maps(&serde_json::json!({"enum_map": null})),
            Ok(None)
        );
    }

    /// 尺寸换算：调用方给"比例 + 档位"，这条供给要像素——组装出来的参数面里是查表得到的像素，
    /// 而那两个源字段不再原样上行。
    #[test]
    fn the_size_conversion_writes_the_target_field_and_consumes_its_sources() {
        let mapping = declared_size_mapping(&serde_json::json!({
            "size": {
                "source": ["size", "resolution"],
                "target": "size",
                "form": "pixels",
                "profile": {"2K": {"1:1": "2048x2048", "2:3": "1664x2496"}}
            }
        }))
        .expect("the declaration parses")
        .expect("a size declaration is present");
        assert_eq!(mapping.form, SizeForm::Pixels);
        assert!(mapping.consumes("resolution"));
        assert!(mapping.consumes("size"));
        assert!(!mapping.consumes("prompt"));

        // 合同面（调用方提交的字段）：比例 + 档位。
        let contract_face = Map::from_iter([
            ("model".to_owned(), serde_json::json!("m")),
            ("prompt".to_owned(), serde_json::json!("x")),
            ("size".to_owned(), serde_json::json!("2:3")),
            ("resolution".to_owned(), serde_json::json!("2K")),
        ]);
        // 承载面过滤后的参数面：`resolution` 没被承载面声明，所以本来就不在里面。
        let mut parameters = Map::from_iter([
            ("model".to_owned(), serde_json::json!("m")),
            ("prompt".to_owned(), serde_json::json!("x")),
            ("size".to_owned(), serde_json::json!("2:3")),
        ]);
        apply_size_mapping(&mapping, &contract_face, &mut parameters)
            .expect("2:3 at 2K is in the profile");
        assert_eq!(
            parameters.get("size"),
            Some(&serde_json::json!("1664x2496")),
            "线上那个字段是换算后的像素：{parameters:?}"
        );
        assert!(
            !parameters.contains_key("resolution"),
            "源字段被换算消耗，不再原样上行：{parameters:?}"
        );
        assert_eq!(parameters.get("prompt"), Some(&serde_json::json!("x")));

        // 调用方这次没用到尺寸：一个字段都不动。
        let untouched = Map::from_iter([("prompt".to_owned(), serde_json::json!("x"))]);
        let mut parameters = untouched.clone();
        apply_size_mapping(&mapping, &untouched, &mut parameters).expect("nothing to convert");
        assert_eq!(parameters, untouched);
        // 源字段在场但是空值：同样按"没给"处理。
        let blank = Map::from_iter([
            ("prompt".to_owned(), serde_json::json!("x")),
            ("size".to_owned(), serde_json::json!("")),
            ("resolution".to_owned(), serde_json::json!(null)),
        ]);
        let mut parameters = blank.clone();
        apply_size_mapping(&mapping, &blank, &mut parameters).expect("blank is not a size");
        assert_eq!(parameters, blank);
    }

    /// 档案里缺那一格：明确失败，绝不退回调用方给的原值、也不猜一个近似值。
    #[test]
    fn a_combination_the_profile_does_not_have_is_an_error() {
        let mapping = declared_size_mapping(&serde_json::json!({
            "size": {
                "source": ["size", "resolution"],
                "target": "size",
                "form": "pixels",
                "profile": {"2K": {"2:3": "1664x2496"}}
            }
        }))
        .expect("the declaration parses")
        .expect("a size declaration is present");
        let contract_face = Map::from_iter([
            ("size".to_owned(), serde_json::json!("2:3")),
            ("resolution".to_owned(), serde_json::json!("3K")),
        ]);
        let mut parameters = Map::from_iter([("size".to_owned(), serde_json::json!("2:3"))]);
        let error = apply_size_mapping(&mapping, &contract_face, &mut parameters)
            .expect_err("3K is not in the profile");
        assert!(error.contains("2:3") && error.contains("3K"), "{error}");
        assert_eq!(
            parameters.get("size"),
            Some(&serde_json::json!("2:3")),
            "失败时不许留下一个半换算的参数面：{parameters:?}"
        );
        // 取值不是字符串（合同只声明类型，取值本身从不校验）：换算不了，明确失败。
        let contract_face = Map::from_iter([("size".to_owned(), serde_json::json!(1024))]);
        let mut parameters = Map::new();
        let error = apply_size_mapping(&mapping, &contract_face, &mut parameters)
            .expect_err("a number is not a size");
        assert!(error.contains("must be a string"), "{error}");
    }

    #[test]
    fn a_size_declaration_that_is_not_shaped_like_one_is_rejected() {
        for (mapping, needle) in [
            (serde_json::json!({"size": "pixels"}), "must be an object"),
            (
                serde_json::json!({"size": {"target": "size", "form": "pixels"}}),
                "size.source is required",
            ),
            (
                serde_json::json!({"size": {"source": [], "target": "size", "form": "pixels"}}),
                "size.source is required",
            ),
            (
                serde_json::json!({"size": {"source": ["size"], "form": "pixels"}}),
                "size.target is required",
            ),
            (
                serde_json::json!({"size": {"source": ["size"], "target": "size"}}),
                "size.form is required",
            ),
            (
                serde_json::json!({"size": {"source": ["size"], "target": "size", "form": "pixel"}}),
                "size.form is required",
            ),
            (
                serde_json::json!({
                    "size": {"source": ["size"], "target": "size", "form": "pixels", "profile": []}
                }),
                "must be an object",
            ),
        ] {
            let error = declared_size_mapping(&mapping)
                .err()
                .unwrap_or_else(|| panic!("{mapping} 不是一份尺寸声明"));
            assert!(error.contains(needle), "{mapping} → {error}");
        }
        // 没有声明（键缺失或显式写 null）＝ 这条供给不做尺寸换算。
        assert_eq!(
            declared_size_mapping(&serde_json::json!({"defaults": {"watermark": false}})),
            Ok(None)
        );
        assert_eq!(
            declared_size_mapping(&serde_json::json!({"size": null})),
            Ok(None)
        );
    }

    /// 没声明尺寸换算的供给（纯透传）根本不经过换算函数：`auto` 照常原样上行。
    ///
    /// 换算只在映射声明了 `size` 时才发生（[`declared_size_mapping`] 为 `None` 就没有这一步），
    /// 因此"承载面收得了 `auto`"的渠道不会被平台拿去算一个比例。
    #[test]
    fn a_supply_without_a_size_declaration_passes_auto_through_untouched() {
        let mapping = serde_json::json!({"rename": {"size": "resolution"}});
        assert_eq!(
            declared_size_mapping(&mapping).expect("no size declaration is not an error"),
            None,
            "没有尺寸声明就没有换算这一步，`auto` 不会经过 convert_size"
        );
        let renames = declared_renames(&mapping).expect("a rename is declared");
        let mut parameters = Map::from_iter([
            ("prompt".to_owned(), serde_json::json!("x")),
            ("size".to_owned(), serde_json::json!("auto")),
        ]);
        apply_parameter_renames(
            &schema(serde_json::json!({
                "prompt": {"type": "string"},
                "resolution": {"type": "string"}
            })),
            renames.as_ref(),
            &mut parameters,
        )
        .expect("the wire name is declared");
        assert_eq!(
            parameters.get("resolution"),
            Some(&serde_json::json!("auto")),
            "纯透传供给把 `auto` 原样带到线上：{parameters:?}"
        );
        assert!(!parameters.contains_key("size"), "{parameters:?}");
    }

    /// 目标字段与源字段同名时（`size` 换算成 `size`），换算结果就写回同一个名字。
    #[test]
    fn a_single_source_field_can_be_converted_in_place() {
        let mapping = declared_size_mapping(&serde_json::json!({
            "size": {
                "source": ["size"],
                "target": "size",
                "form": "ratio",
                "profile": {"2K": {"16:9": "2848x1600"}}
            }
        }))
        .expect("the declaration parses")
        .expect("a size declaration is present");
        let contract_face = Map::from_iter([("size".to_owned(), serde_json::json!("2848x1600"))]);
        let mut parameters = contract_face.clone();
        apply_size_mapping(&mapping, &contract_face, &mut parameters).expect("reverse lookup");
        assert_eq!(parameters.get("size"), Some(&serde_json::json!("16:9")));
    }

    /// 平台补的**显式默认值**也参与换算：调用方没给档位、映射声明了默认档位时，
    /// 那条默认值就是换算的输入，而不是被当成一个孤零零的字段发上去。
    #[test]
    fn an_injected_default_can_feed_the_conversion() {
        let contract = schema(serde_json::json!({
            "model": {"const": "m"},
            "prompt": {"type": "string"},
            "size": {"type": "string"},
            "resolution": {"type": "string"}
        }));
        let carrier = contract.clone();
        let mapping = serde_json::json!({
            "defaults": {"resolution": "2K"},
            "size": {
                "source": ["size", "resolution"],
                "target": "size",
                "form": "pixels",
                "profile": {"2K": {"2:3": "1664x2496"}}
            }
        });
        let declared = declared_size_mapping(&mapping)
            .expect("the declaration parses")
            .expect("a size declaration is present");
        // 调用方只给了比例，档位由映射的显式默认值补上。
        let contract_face = Map::from_iter([
            ("model".to_owned(), serde_json::json!("m")),
            ("size".to_owned(), serde_json::json!("2:3")),
        ]);
        let mut parameters = contract_face.clone();
        apply_parameter_defaults(
            &contract,
            &carrier,
            None,
            declared_defaults(&mapping),
            &mut parameters,
        );
        assert_eq!(
            parameters.get("resolution"),
            Some(&serde_json::json!("2K")),
            "默认值先注入：{parameters:?}"
        );
        apply_size_mapping(&declared, &contract_face, &mut parameters)
            .expect("the default tier completes the conversion");
        assert_eq!(
            parameters.get("size"),
            Some(&serde_json::json!("1664x2496"))
        );
        assert!(
            !parameters.contains_key("resolution"),
            "被消耗的源字段（哪怕是默认值补进来的）不许再上行：{parameters:?}"
        );
    }
}
