//! 模型使用文档的渲染：由同版合同与文档素材生成最终 Markdown。
//!
//! 归属：Spec 0008 §3–§4 拥有正文内容边界与发布校验；本模块只做渲染，不读文件、不碰数据库。
//! 素材是**已解析**的形态——`{"narrative": "<Markdown 正文>", "fields": {...}}`，其中
//! `fields` 的键是从合同根算起的 JSON Pointer：属性用 `/properties/<名>`（嵌套继续往下），
//! 组合约束用 `/allOf/<下标>`。素材的路径解析发生在导入侧。

use serde_json::Value;

use crate::ApplicationError;
use seeai_domain::replace_contract_model_identity;

/// 最终正文上限：256 KiB。超限拒绝发布，不截断客户说明。
pub const MAX_MODEL_DOCUMENT_BYTES: usize = 262_144;

/// 公开使用文档的**名称与公开地址**：渲染器转换链接与 API 提供资源共用这一份清单，
/// 加第四份只需改这里（Spec 0008 §2）。
pub const PUBLIC_DOCUMENTS: [(&str, &str); 3] = [
    ("authentication.md", "/v1/docs/authentication.md"),
    ("uploads/images.md", "/v1/docs/uploads/images.md"),
    ("http-errors.md", "/v1/docs/http-errors.md"),
];

fn invalid(message: &str) -> ApplicationError {
    ApplicationError::Validation(message.to_owned())
}

/// 由同版合同与素材渲染一份模型使用文档。
pub fn render_model_document(
    contract: &Value,
    platform_name: &str,
    vendor_id: &str,
    model_type: &str,
    revision: &str,
    material: &Value,
) -> Result<String, ApplicationError> {
    let narrative = material
        .get("narrative")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("the model document material declares no narrative"))?;
    let fields = material
        .get("fields")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("the model document material declares no field definitions"))?;
    if !narrative.contains("{{parameter_table}}") {
        return Err(invalid(
            "the model document narrative does not place {{parameter_table}}",
        ));
    }

    // 参数表里的型号身份是**平台对客名**：合同里冻结的是厂商原生名，只在交给调用方时替换这一个
    // 字段（与目录的对客投射同一条规则，Spec 0008 §3）。
    let mut projected = contract.clone();
    replace_contract_model_identity(&mut projected, platform_name);
    let contract = &projected;

    let rows = property_rows(contract)?;
    for row in &rows {
        if !fields.contains_key(&row.pointer) {
            return Err(invalid(&format!(
                "the model document material does not define contract field {}",
                row.pointer
            )));
        }
    }
    let constraint_pointers: Vec<String> = contract
        .get("allOf")
        .and_then(Value::as_array)
        .map(|clauses| {
            (0..clauses.len())
                .map(|index| format!("/allOf/{index}"))
                .collect()
        })
        .unwrap_or_default();
    for key in fields.keys() {
        if !rows.iter().any(|row| row.pointer == *key) && !constraint_pointers.contains(key) {
            return Err(invalid(&format!(
                "the model document material defines {key}, which the contract does not declare"
            )));
        }
    }

    let mut table =
        String::from("| 字段 | 必填 | 类型与限制 | 释义 |\n| --- | --- | --- | --- |\n");
    for row in &rows {
        let definition = fields[&row.pointer].as_str().unwrap_or_default();
        table.push_str(&format!(
            "| `{}` | {} | {} | {} |\n",
            row.name,
            if row.required { "是" } else { "否" },
            limits(row),
            definition
        ));
    }
    if !constraint_pointers.is_empty() {
        table.push_str("\n组合约束：\n\n");
        for (index, pointer) in constraint_pointers.iter().enumerate() {
            table.push_str(&format!("- {}", clause_text(contract, index)?));
            if let Some(text) = fields.get(pointer).and_then(Value::as_str) {
                table.push(' ');
                table.push_str(text);
            }
            table.push('\n');
        }
    }

    let rendered = narrative
        .replace("{{parameter_table}}", table.trim_end())
        .replace("{{platform_name}}", platform_name)
        .replace("{{vendor_id}}", vendor_id)
        .replace("{{model_type}}", model_type)
        .replace("{{contract_revision}}", revision);
    if let Some(start) = rendered.find("{{") {
        let tail = &rendered[start..];
        let end = tail.find("}}").map(|end| end + 2).unwrap_or(tail.len());
        return Err(invalid(&format!(
            "the model document leaves an unresolved placeholder {}",
            &tail[..end]
        )));
    }
    let rendered = public_doc_links(&rendered)?;
    if rendered.len() > MAX_MODEL_DOCUMENT_BYTES {
        return Err(invalid(&format!(
            "the rendered model document is {} bytes, over the {} byte limit",
            rendered.len(),
            MAX_MODEL_DOCUMENT_BYTES
        )));
    }
    Ok(rendered)
}

struct PropertyRow {
    pointer: String,
    name: String,
    schema: Value,
    required: bool,
}

/// 递归收集合同里的属性：每个属性一条，嵌套属性继续往下。
fn property_rows(contract: &Value) -> Result<Vec<PropertyRow>, ApplicationError> {
    let mut rows = Vec::new();
    collect("", contract, &mut rows)?;
    Ok(rows)
}

fn collect(
    prefix: &str,
    schema: &Value,
    rows: &mut Vec<PropertyRow>,
) -> Result<(), ApplicationError> {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return Ok(());
    };
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    for (name, property) in properties {
        let pointer = format!("{prefix}/properties/{name}");
        rows.push(PropertyRow {
            pointer: pointer.clone(),
            name: name.clone(),
            schema: property.clone(),
            required: required.contains(&name.as_str()),
        });
        collect(&pointer, property, rows)?;
    }
    Ok(())
}

/// 一行参数的「类型与限制」：类型、固定值/枚举、数值与长度/数量上下界、格式、默认声明。
fn limits(row: &PropertyRow) -> String {
    let mut parts = Vec::new();
    if let Some(kind) = row.schema.get("type").and_then(Value::as_str) {
        parts.push(kind.to_owned());
    }
    if let Some(value) = row.schema.get("const") {
        parts.push(format!("固定 {}", display(value)));
    }
    if let Some(values) = row.schema.get("enum").and_then(Value::as_array) {
        let values: Vec<String> = values.iter().map(display).collect();
        parts.push(format!("可选 {}", values.join(" / ")));
    }
    if let Some(items) = row.schema.get("items") {
        parts.push(format!("元素 {}", schema_summary(items)));
    }
    // 合同里的组合取值（`anyOf` / `oneOf`）与自然语言描述都要能读到，不能只认白名单键。
    for (key, label) in [("anyOf", "至少满足一条"), ("oneOf", "恰好满足一条")] {
        if let Some(options) = row.schema.get(key).and_then(Value::as_array) {
            let rendered: Vec<String> = options.iter().map(schema_summary).collect();
            parts.push(format!("{label}：{}", rendered.join(" / ")));
        }
    }
    for (key, label) in [
        ("minimum", "最小"),
        ("maximum", "最大"),
        ("minLength", "最短"),
        ("maxLength", "最长"),
        ("minItems", "最少"),
        ("maxItems", "最多"),
    ] {
        if let Some(value) = row.schema.get(key) {
            parts.push(format!("{label} {}", display(value)));
        }
    }
    if let Some(pattern) = row.schema.get("pattern").and_then(Value::as_str) {
        parts.push(format!("格式 {pattern}"));
    }
    if row.schema.get("default").is_some() {
        parts.push(format!("默认声明 {}", display(&row.schema["default"])));
    }
    if let Some(description) = row.schema.get("description").and_then(Value::as_str) {
        parts.push(description.to_owned());
    }
    parts.join("，")
}

/// 一层子约束的短描述：`anyOf` / `oneOf` 的每个分支与数组 `items` 用它。
fn schema_summary(schema: &Value) -> String {
    if let Some(value) = schema.get("const") {
        return format!("固定 {}", display(value));
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        let values: Vec<String> = values.iter().map(display).collect();
        return format!("可选 {}", values.join(" / "));
    }
    if let Some(pattern) = schema.get("pattern").and_then(Value::as_str) {
        return format!("格式 {pattern}");
    }
    if let Some(kind) = schema.get("type").and_then(Value::as_str) {
        return kind.to_owned();
    }
    "约束".to_owned()
}

fn display(value: &Value) -> String {
    match value {
        Value::String(text) => format!("`{text}`"),
        other => other.to_string(),
    }
}

/// 一条组合约束的中文结构描述：`allOf[i]` 的 `if` 条件成立时，`then` 要求什么。
fn clause_text(contract: &Value, index: usize) -> Result<String, ApplicationError> {
    let clause = contract
        .get("allOf")
        .and_then(Value::as_array)
        .and_then(|clauses| clauses.get(index))
        .ok_or_else(|| invalid("the model document contract has no such combination clause"))?;
    let condition = requirement(clause.get("if"));
    let consequence = requirement(clause.get("then"));
    Ok(format!("当{condition}时，{consequence}。"))
}

fn requirement(schema: Option<&Value>) -> String {
    let Some(schema) = schema else {
        return "无".to_owned();
    };
    let mut parts = Vec::new();
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for name in required.iter().filter_map(Value::as_str) {
            parts.push(format!("提供 `{name}`"));
        }
    }
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (name, rule) in properties {
            if let Some(value) = rule.get("const") {
                parts.push(format!("`{name}` 为 {}", display(value)));
            }
            if let Some(values) = rule.get("enum").and_then(Value::as_array) {
                let values: Vec<String> = values.iter().map(display).collect();
                parts.push(format!("`{name}` 取 {}", values.join(" / ")));
            }
        }
    }
    if parts.is_empty() {
        return "无".to_owned();
    }
    parts.join("、")
}

/// 正文里的本地链接只允许指向三份公共文档；其余本地/内部路径一律拒绝（Spec 0008 §1）。
fn public_doc_links(narrative: &str) -> Result<String, ApplicationError> {
    let mut rendered = String::with_capacity(narrative.len());
    let mut rest = narrative;
    while let Some(start) = rest.find("](") {
        let after = &rest[start + 2..];
        let Some(end) = after.find(')') else {
            break;
        };
        rendered.push_str(&rest[..start + 2]);
        let target = &after[..end];
        rendered.push_str(&public_doc_target(target)?);
        rest = &after[end..];
    }
    rendered.push_str(rest);
    Ok(rendered)
}

fn public_doc_target(target: &str) -> Result<String, ApplicationError> {
    if target.starts_with("http://")
        || target.starts_with("https://")
        || target.starts_with('#')
        || target.starts_with("mailto:")
    {
        return Ok(target.to_owned());
    }
    let normalized = target.trim_start_matches("./");
    if let Some((_, url)) = PUBLIC_DOCUMENTS
        .iter()
        .find(|(path, _)| normalized == *path || normalized.ends_with(&format!("/{path}")))
    {
        return Ok((*url).to_owned());
    }
    Err(invalid(&format!(
        "the model document links to {target}; only the public usage documents may be linked"
    )))
}

#[cfg(test)]
mod tests;
