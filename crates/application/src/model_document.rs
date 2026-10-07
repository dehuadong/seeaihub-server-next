//! 模型使用文档的渲染：由同版合同与文档素材生成最终 Markdown。
//!
//! 归属：Spec 0008 §3–§4 拥有正文内容边界与发布校验；本模块只做渲染，不读文件、不碰数据库。
//! 素材是**已解析**的形态——`{"narrative": "<Markdown 正文>", "fields": {...}}`，其中
//! `fields` 的键是从合同根算起的 JSON Pointer：属性用 `/properties/<名>`（嵌套继续往下），
//! 组合约束用 `/allOf/<下标>`。素材的路径解析发生在导入侧。

use serde_json::Value;

use crate::ApplicationError;
use seeai_domain::{declares_parameter, replace_contract_model_identity};

/// 最终正文上限：256 KiB。超限拒绝发布，不截断客户说明。
pub const MAX_MODEL_DOCUMENT_BYTES: usize = 262_144;

/// 公开使用文档的**名称**：渲染器转换链接与 API 提供资源共用这一份清单，加第四份只需改这里。
pub const PUBLIC_DOCUMENTS: [&str; 4] = [
    "README.md",
    "authentication.md",
    "uploads/images.md",
    "http-errors.md",
];

/// 素材里指代**平台对客基址**的占位符：正文与示例要写绝对地址时用它，发布时由配置代入。
pub const BASE_URL_PLACEHOLDER: &str = "{{SEE_BASEURL}}";

fn invalid(message: &str) -> ApplicationError {
    ApplicationError::Validation(message.to_owned())
}

/// 组合约束点错名字时的整句报错：判据与文案一处。
///
/// `subject` 是出问题的那份对象（"the contract for model X" / "offering A/B" / 素材的位置），由调用方
/// 按自己的语境给——同一条规则在三处使用，错误口径不能各写一份。
#[must_use]
pub fn undeclared_clause_message(subject: &str, schema: &Value) -> Option<String> {
    undeclared_clause_name(schema).map(|(pointer, name)| {
        format!(
            "{subject}: the schema names {name} at {pointer}, but it does not declare that field"
        )
    })
}

/// 组合约束里点到、但**同一层**没声明的字段名，附它在约束里的位置（JSON Pointer）。
///
/// 只判**封闭对象**（`additionalProperties: false`）：只有那一层里，"点到没声明的名字"才等于那条约束
/// 落不到任何值上——分支要么永远无法满足（`allOf`、`oneOf` 的死分支），要么永远是死条文（`not` 那种
/// 恒真的约束甚至放行了本想禁止的取值）；模型使用文档里那句结构描述还会教调用方填一个会被丢弃的字段。
/// 不封闭的那一层可以带额外属性，点到没声明的名字是合法的（由上游按自己的 schema 处置），不判。
///
/// 判据用 [`declares_parameter`]：`properties` 的键、`required` 里单列的名字与 `容器.成员` 都算声明
/// ——与参数过滤同一份判据，两边不一致就会出现"判它没声明、却把它的值留下"。
///
/// 每一层各判一次：顶层、每个 `properties` 子 schema、`items`。约束的落点覆盖 `allOf` / `oneOf` /
/// `anyOf` 的子句本身与它的 `if` / `then` / `else`，以及 `not`。
#[must_use]
pub fn undeclared_clause_name(schema: &Value) -> Option<(String, String)> {
    if let Some(found) = own_undeclared_clause_name(schema) {
        return Some(found);
    }
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (name, sub) in properties {
            if let Some((pointer, field)) = undeclared_clause_name(sub) {
                return Some((format!("/properties/{name}{pointer}"), field));
            }
        }
    }
    if let Some((pointer, field)) = schema.get("items").and_then(undeclared_clause_name) {
        return Some((format!("/items{pointer}"), field));
    }
    None
}

/// 只看这一层（且必须是封闭对象）的组合约束，不含子 schema。
fn own_undeclared_clause_name(schema: &Value) -> Option<(String, String)> {
    if schema.get("additionalProperties").and_then(Value::as_bool) != Some(false) {
        return None;
    }
    fn names(side: &Value, pointer: &str, out: &mut Vec<(String, String)>) {
        if let Some(required) = side.get("required").and_then(Value::as_array) {
            for (index, name) in required.iter().filter_map(Value::as_str).enumerate() {
                out.push((format!("{pointer}/required/{index}"), name.to_owned()));
            }
        }
        if let Some(properties) = side.get("properties").and_then(Value::as_object) {
            for name in properties.keys() {
                out.push((format!("{pointer}/properties/{name}"), name.clone()));
            }
        }
    }
    let mut candidates = Vec::new();
    for key in ["allOf", "oneOf", "anyOf"] {
        let Some(clauses) = schema.get(key).and_then(Value::as_array) else {
            continue;
        };
        for (index, clause) in clauses.iter().enumerate() {
            let pointer = format!("/{key}/{index}");
            names(clause, &pointer, &mut candidates);
            for keyword in ["if", "then", "else"] {
                if let Some(side) = clause.get(keyword) {
                    names(side, &format!("{pointer}/{keyword}"), &mut candidates);
                }
            }
        }
    }
    if let Some(negated) = schema.get("not") {
        names(negated, "/not", &mut candidates);
    }
    candidates
        .into_iter()
        .find(|(_, name)| !declares_parameter(schema, name))
}

/// 平台对客基址：必须是 http(s) 的源，不带结尾斜杠、查询或片段。
///
/// 它在**发布时**代入正文，所以必须是部署期就定好的对客地址；从请求主机取会把管理端主机写进
/// 不可变版本。
pub fn validate_base_url(base_url: &str) -> Result<(), ApplicationError> {
    if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
        return Err(invalid(&format!(
            "SEE_BASEURL must be an absolute http(s) origin, got {base_url}"
        )));
    }
    if base_url.ends_with('/') || base_url.contains('?') || base_url.contains('#') {
        return Err(invalid(&format!(
            "SEE_BASEURL must not carry a trailing slash, query or fragment, got {base_url}"
        )));
    }
    Ok(())
}

/// 一份公开使用文档的绝对地址。
pub fn public_document_url(base_url: &str, name: &str) -> String {
    format!("{base_url}/v1/docs/{name}")
}

/// 把正文里的本地链接统一成绝对地址：只允许指向公开使用文档，其余本地/内部路径一律拒绝
/// （Spec 0008 §1）。素材与公共文档各自按作者写的相对链接书写，这里统一代入平台对客基址；
/// 已经写死的 http(s) 链接、页内锚点与 `mailto:` 原样保留。
pub fn rewrite_public_doc_links(
    markdown: &str,
    base_url: &str,
) -> Result<String, ApplicationError> {
    let mut rendered = String::with_capacity(markdown.len());
    let mut rest = markdown;
    while let Some(start) = rest.find("](") {
        let after = &rest[start + 2..];
        let Some(end) = after.find(')') else {
            break;
        };
        rendered.push_str(&rest[..start + 2]);
        rendered.push_str(&public_doc_target(&after[..end], base_url)?);
        rest = &after[end..];
    }
    rendered.push_str(rest);
    Ok(rendered)
}

/// 把一份公开使用文档的源码变成对客正文：代入 `{{SEE_BASEURL}}`，再把本地链接统一成绝对地址。
///
/// 源码里写 `{{SEE_BASEURL}}/v1/docs/<名>`；没有占位符的本地链接也在这里统一成绝对地址。
pub fn render_public_document(source: &str, base_url: &str) -> Result<String, ApplicationError> {
    validate_base_url(base_url)?;
    let substituted = source.replace(BASE_URL_PLACEHOLDER, base_url);
    if let Some(start) = substituted.find("{{") {
        let tail = &substituted[start..];
        let end = tail.find("}}").map(|end| end + 2).unwrap_or(tail.len());
        return Err(invalid(&format!(
            "the public usage document leaves an unresolved placeholder {}",
            &tail[..end]
        )));
    }
    rewrite_public_doc_links(&substituted, base_url)
}

/// 由同版合同与素材渲染一份模型使用文档。
pub fn render_model_document(
    contract: &Value,
    platform_name: &str,
    vendor_id: &str,
    model_type: &str,
    revision: &str,
    base_url: &str,
    material: &Value,
) -> Result<String, ApplicationError> {
    validate_base_url(base_url)?;
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
    // 组合约束只能点合同自己声明的字段：点到别的名字时那条分支永远无法满足，渲染出来的结构描述
    // 还会教调用方填一个会被丢弃的字段（AIHubMix 改名那次就在顶层合同上留下过这种名字）。
    let subject = format!("the contract for model {platform_name}");
    if let Some(message) = undeclared_clause_message(&subject, contract) {
        return Err(invalid(&message));
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
        .replace("{{contract_revision}}", revision)
        .replace(BASE_URL_PLACEHOLDER, base_url);
    if let Some(start) = rendered.find("{{") {
        let tail = &rendered[start..];
        let end = tail.find("}}").map(|end| end + 2).unwrap_or(tail.len());
        return Err(invalid(&format!(
            "the model document leaves an unresolved placeholder {}",
            &tail[..end]
        )));
    }
    let rendered = rewrite_public_doc_links(&rendered, base_url)?;
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

fn public_doc_target(target: &str, base_url: &str) -> Result<String, ApplicationError> {
    if target.starts_with("http://")
        || target.starts_with("https://")
        || target.starts_with('#')
        || target.starts_with("mailto:")
    {
        return Ok(target.to_owned());
    }
    let normalized = target.trim_start_matches("./");
    if let Some(name) = PUBLIC_DOCUMENTS
        .iter()
        .find(|name| normalized == **name || normalized.ends_with(&format!("/{name}")))
    {
        return Ok(public_document_url(base_url, name));
    }
    Err(invalid(&format!(
        "the model document links to {target}; only the public usage documents may be linked"
    )))
}

#[cfg(test)]
mod tests;
