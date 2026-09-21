//! 供给的参数映射：把**合同值**转成**渠道包装**的声明，目前只有第一块——显式默认值。
//!
//! 为什么要有显式默认值：同一个字段在各家渠道各有自己的一套默认（上游把水印默认打开就是例子）。
//! 调用方没给这个字段时，平台若原样不发，最终效果就由**渠道的默认值**决定——同一份请求在两个
//! 渠道上得到不同的东西，而这个差异调用方既看不见也管不了。映射里声明的默认值让"调用方没给"
//! 这件事由平台定下来，渠道那套默认值就再也用不上。
//!
//! 只做这一件事：改名与枚举映射（合同字段名与渠道字段名不同时怎么落）是另一块，本模块不碰；
//! 它也不是"通用映射引擎"，只是这份声明里已经用到的那几个键。
//!
//! 注入的判据是**合同 + 承载面**：两边都声明了这个字段才注入。合同没声明，说明调用方根本提交
//! 不了它，注入等于平台凭空多出一个调用方看不见的参数；承载面没声明，说明这条供给发不出去它，
//! 注入只会得到上游自己的一套解释。两种情况都**不注入、不报错**——一次请求不该为一份写歪的映射
//! 失败。注意这意味着映射写错（键名拼错、声明的字段不在承载面里）时默认值只是**不生效**，
//! 当前没有发布期校验会把它拦下来。

use serde_json::{Map, Value};

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

/// 把映射声明的显式默认值注入参数面：**调用方没给、且合同与承载面都声明了**该字段才注入。
///
/// "调用方没给"含空值：`null` 与空串同样按"没给"处理，否则调用方写一个空位就能把平台的默认值
/// 顶掉，而那个空位到上游那里又什么都不表示。
///
/// 已经给了的字段**一个字都不改**：默认值只在调用方没说话的地方替它说话。
pub fn apply_parameter_defaults(
    contract: &Value,
    carrier: &Value,
    mapping: &Value,
    parameters: &mut Map<String, Value>,
) {
    let Some(defaults) = declared_defaults(mapping) else {
        return;
    };
    for (name, value) in defaults {
        if !declares_parameter(contract, name) || !declares_parameter(carrier, name) {
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

        // 调用方没给：注入默认值。
        let mut parameters = Map::from_iter([("prompt".to_owned(), serde_json::json!("x"))]);
        apply_parameter_defaults(&contract, &carrier, &mapping, &mut parameters);
        assert_eq!(parameters.get("watermark"), Some(&serde_json::json!(false)));

        // 调用方给了：用调用方的值，一个字都不改。
        let mut parameters = Map::from_iter([
            ("prompt".to_owned(), serde_json::json!("x")),
            ("watermark".to_owned(), serde_json::json!(true)),
        ]);
        apply_parameter_defaults(&contract, &carrier, &mapping, &mut parameters);
        assert_eq!(parameters.get("watermark"), Some(&serde_json::json!(true)));

        // 调用方写了个空位：那也是"没给"，默认值照旧生效。
        for empty in [serde_json::json!(null), serde_json::json!("")] {
            let mut parameters = Map::from_iter([
                ("prompt".to_owned(), serde_json::json!("x")),
                ("watermark".to_owned(), empty.clone()),
            ]);
            apply_parameter_defaults(&contract, &carrier, &mapping, &mut parameters);
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
        let mut parameters = Map::from_iter([("prompt".to_owned(), serde_json::json!("x"))]);
        apply_parameter_defaults(&contract, &carrier, &mapping, &mut parameters);
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
        apply_parameter_defaults(&carrier, &carrier, &mapping, &mut parameters);
        assert!(parameters.get("watermark").is_none());
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
            apply_parameter_defaults(&contract, &contract, &mapping, &mut parameters);
            assert_eq!(parameters, before, "映射 {mapping} 不该改动参数面");
        }
        assert!(declared_defaults(&serde_json::json!({})).is_none());
    }
}
