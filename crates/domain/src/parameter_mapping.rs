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

/// 一份 schema 是否声明了这个字段名。
///
/// 名字里带点表示它落在**容器字段**里：`extra.quality` 判的是 `extra` 自己的 `properties` 或
/// `required` 里的 `quality`。容器本身必须在顶层被声明（不声明就是没有这个容器），判据与
/// [`declared_field_names`] 对顶层那一层是同一份。
#[must_use]
pub fn declares_parameter(schema: &Value, name: &str) -> bool {
    match name.split_once('.') {
        None => declared_field_names(schema).contains(&name),
        Some((container, field)) => schema
            .get("properties")
            .and_then(|properties| properties.get(container))
            .is_some_and(|inner| declared_field_names(inner).contains(&field)),
    }
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

/// 参数面里一个字段的**字面文本**：字段缺失、`null`、或不是字符串 ⇒ `None`。
///
/// 它回答的是"这个字段写的是什么"，不是"调用方有没有用到这个字段"——后者是
/// [`is_used_parameter_value`] 的判据，两者只在一处不同：**空串在这里算给了**（"给了个空"也是
/// 一个字面量），在那个判据里算"没给"。要字面量的地方（例如按 `size` 的取值归档位）必须用这
/// 一个：调用方写的字面量就是它说的那个东西，认不出由领域回落，不替它当成没说话再猜一个默认值。
///
/// 不 trim、不认别名：写法是调用方的事，平台不替它收拾。
#[must_use]
pub fn literal_parameter_text<'a>(parameters: &'a Value, name: &str) -> Option<&'a str> {
    parameters.get(name).and_then(Value::as_str)
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
mod tests;
