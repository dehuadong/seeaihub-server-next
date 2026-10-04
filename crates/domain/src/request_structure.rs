//! 调用方请求结构的平台上限与**有界解析**（RFC 0018 §2.2）。
//!
//! 请求正文的字节数上限（[`SUPPORTED_REQUEST_WIRE_BYTES`]）管的是 wire 长度，它**不等于**解析
//! 结果占用的内存：`serde_json::Value` 的节点比它的 wire 表示贵得多。实测两种节点密集的已支持
//! 输入形态，在 16 MiB wire 内能把解析结构抬到远超解析预留：
//!
//! | 形态 | wire | 解析结构峰值 |
//! | --- | --- | --- |
//! | `[0,0,…]`（标量节点） | 16 MiB | 526036 KiB（约 514 MiB） |
//! | `[{"a":0},…]`（单字段对象节点） | 16 MiB | 2065756 KiB（约 2.0 GiB） |
//!
//! 因此解析必须**边解析边计数**，在超限时立刻失败——不能先把整份 `Value` 建好再统计节点数，
//! 那样峰值已经发生。本模块提供这一条有界入口：
//!
//! - 计数器（[`RequestStructureCounter`]）在构造之前/构造过程中记账：容器层数、节点总数、
//!   单个对象的字段数、累计字符串字节；
//! - [`RequestParameters`] 是"已经计过数的顶层对象"：wire 入口只能通过
//!   [`RequestParameters::parse`] 拿到它，进程内调用方（multipart 逐项构造、测试夹具）通过
//!   [`RequestParameters::builder`] 或 [`RequestParameters::try_from_value`] 拿到它。
//!
//! # 上限怎么从已支持的输入范围推导
//!
//! 输入范围是 [`SUPPORTED_REQUEST_WIRE_BYTES`]（16 MiB，与 API 的正文上限同一口径）。解析结构
//! 的预算是 `GATEWAY_REQUEST_PARSE_BYTES`（18 MiB，实测，见 `crates/adapter-sdk`）：
//!
//! ```text
//! 结构上界 = 累计字符串字节 + 节点数 × 每节点结构开销
//!          <= 16 MiB            + 2048 × 1 KiB
//!          =  16 MiB            + 2 MiB
//!          =  18 MiB = 解析预留
//! ```
//!
//! - **累计字符串字节 = 16 MiB**：JSON 里每个解码出的字符串字节至少吃掉一个 wire 字节
//!   （转义序列只会更多：`\n` 2:1、`\uXXXX` 6:1 到 6:3），所以解码总量恒 ≤ wire 总量。取
//!   wire 上限就是**不缩小任何已支持输入**的最大值；一份贴住 16 MiB 的 data URL 图片请求因此
//!   照旧解析通过。
//! - **节点数 = 2048，每节点按 1 KiB 计**：1 KiB 是实测的最贵形态——单字段对象节点
//!   （2065756 KiB ÷ 约 2097147 个节点 ≈ 1008 B/节点），它同时覆盖了 `Map`/`Vec` 的预分配与
//!   扩容峰值（标量数组实测 64 B/节点，其中一半是 `Vec` 的几何扩容；1 KiB 把它也收在里面）。
//!   2048 × 1 KiB = 2 MiB，正好是 18 MiB 解析预算里除掉字符串那一份之后的余量。
//! - **单个对象字段数 = 256**：`serde_json::Map`（BTreeMap）每个字段约 58 B 槽位，256 个字段约
//!   15 KiB，远在它的那些值节点的计费之内。正常请求的顶层字段在十几个量级。
//! - **容器层数 = 64**：已声明参数面里最深的合法形状是"顶层对象 → 嵌套容器 → 字段 → 值"四层，
//!   64 是它的十几倍。`serde_json` 自带的递归上限是 128，这里取更小的一条，是为了让"深度"这条
//!   上限由平台自己判定、而不是继承库的内部常量；比 64 更深的请求本来也没有承载面。
//!
//! 这四条是**编译期常数**，不是部署旋钮：放宽任何一条都会让上面的推导不再成立，必须连同
//! `GATEWAY_REQUEST_PARSE_BYTES` 一起重新实测。收紧（例如运维想给某类部署更小的结构上限）走
//! `GENERATION_REQUEST_JSON_*` 配置，只允许 ≤ 这里的默认值。

use serde::de::{
    self, DeserializeSeed, Deserializer as SerdeDeserializer, MapAccess, SeqAccess, Visitor,
};
use serde_json::{Map, Number, Value};
use std::fmt;

/// 平台支持的请求正文 wire 上限：API 的正文限制、内存预算的 `I` 段与请求结构计数上限都从它推导。
pub const SUPPORTED_REQUEST_WIRE_BYTES: usize = 16 * 1024 * 1024;

/// 容器嵌套层数上限：已声明参数面最深四层（顶层 → 容器 → 字段 → 值），64 是它的十几倍，
/// 同时小于 `serde_json` 自带的 128 递归上限，让这条上限由平台自己判定（见模块注释）。
pub const REQUEST_JSON_MAX_DEPTH: usize = 64;
/// JSON 值总数上限（含根对象；见模块注释的 1 KiB/节点计费）。
pub const REQUEST_JSON_MAX_NODES: usize = 2048;
/// 单个对象的字段数上限。
pub const REQUEST_JSON_MAX_OBJECT_FIELDS: usize = 256;
/// 累计字符串字节上限（解码后字节数；等于 wire 上限，见模块注释）。
pub const REQUEST_JSON_MAX_STRING_BYTES: usize = SUPPORTED_REQUEST_WIRE_BYTES;

/// 请求结构计数的四条上限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestJsonLimits {
    pub max_depth: usize,
    pub max_nodes: usize,
    pub max_object_fields: usize,
    pub max_string_bytes: usize,
}

/// 缺省上限：全部从 [`SUPPORTED_REQUEST_WIRE_BYTES`] 推导（见模块注释）。
pub const REQUEST_JSON_LIMITS: RequestJsonLimits = RequestJsonLimits {
    max_depth: REQUEST_JSON_MAX_DEPTH,
    max_nodes: REQUEST_JSON_MAX_NODES,
    max_object_fields: REQUEST_JSON_MAX_OBJECT_FIELDS,
    max_string_bytes: REQUEST_JSON_MAX_STRING_BYTES,
};

/// 计数过程中被突破的那一条上限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestStructureViolation {
    Depth { limit: usize },
    Nodes { limit: usize },
    ObjectFields { limit: usize },
    StringBytes { limit: usize },
}

impl fmt::Display for RequestStructureViolation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Depth { limit } => write!(
                formatter,
                "the request nests containers more than {limit} levels deep"
            ),
            Self::Nodes { limit } => {
                write!(
                    formatter,
                    "the request contains more than {limit} JSON values"
                )
            }
            Self::ObjectFields { limit } => write!(
                formatter,
                "a request object declares more than {limit} fields"
            ),
            Self::StringBytes { limit } => write!(
                formatter,
                "the request carries more than {limit} bytes of string data"
            ),
        }
    }
}

impl std::error::Error for RequestStructureViolation {}

/// 有界解析的失败：语法/形状问题，或某一条结构上限被突破。
///
/// 对客处置两类一样（受理前的参数错误，不建记录、不取 Hold），分开是为了日志与测试能说出**哪一条**
/// 上限先被撞到。
#[derive(Debug, thiserror::Error)]
pub enum RequestJsonError {
    /// 正文不是合法 JSON（含尾随数据）。
    #[error("the request body is not valid JSON: {0}")]
    Syntax(#[source] serde_json::Error),
    /// 正文是合法 JSON，但顶层不是对象；请求参数面要求对象。
    #[error("the request body must be a JSON object")]
    NotAnObject,
    /// 某一条结构上限被突破。
    #[error(transparent)]
    Limit(RequestStructureViolation),
}

/// 请求结构的计数器：构造**之前/过程中**记账，不做事后遍历。
///
/// 每一次记账都在分配之前检查：节点数、字符串字节数在写入集合之前核对，容器层数在进入之前核对。
/// 记录下来的第一条越界（[`Self::violation`]）用于把 serde 的字符串化错误还原成结构化结论。
#[derive(Debug, Clone)]
pub struct RequestStructureCounter {
    limits: RequestJsonLimits,
    depth: usize,
    nodes: usize,
    string_bytes: usize,
    violation: Option<RequestStructureViolation>,
}

impl RequestStructureCounter {
    #[must_use]
    pub fn new(limits: RequestJsonLimits) -> Self {
        Self {
            limits,
            depth: 0,
            nodes: 0,
            string_bytes: 0,
            violation: None,
        }
    }

    #[must_use]
    pub fn limits(&self) -> RequestJsonLimits {
        self.limits
    }

    /// 第一条被突破的上限；没有越界时为 `None`。
    #[must_use]
    pub fn violation(&self) -> Option<RequestStructureViolation> {
        self.violation
    }

    /// 记一个 JSON 值节点。
    pub fn count_node(&mut self) -> Result<(), RequestStructureViolation> {
        self.nodes = self.nodes.saturating_add(1);
        if self.nodes > self.limits.max_nodes {
            return Err(self.record(RequestStructureViolation::Nodes {
                limit: self.limits.max_nodes,
            }));
        }
        Ok(())
    }

    /// 进入一层容器（对象或数组）。
    pub fn enter_container(&mut self) -> Result<(), RequestStructureViolation> {
        self.depth = self.depth.saturating_add(1);
        if self.depth > self.limits.max_depth {
            return Err(self.record(RequestStructureViolation::Depth {
                limit: self.limits.max_depth,
            }));
        }
        Ok(())
    }

    /// 离开一层容器。
    pub fn leave_container(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    /// 累计一个字符串的解码后字节数（对象键也算）。
    pub fn count_string(&mut self, bytes: usize) -> Result<(), RequestStructureViolation> {
        self.string_bytes = self.string_bytes.saturating_add(bytes);
        if self.string_bytes > self.limits.max_string_bytes {
            return Err(self.record(RequestStructureViolation::StringBytes {
                limit: self.limits.max_string_bytes,
            }));
        }
        Ok(())
    }

    /// 核对一个对象的字段数（`fields` 是该对象当前的字段个数）。
    pub fn count_object_fields(&mut self, fields: usize) -> Result<(), RequestStructureViolation> {
        if fields > self.limits.max_object_fields {
            return Err(self.record(RequestStructureViolation::ObjectFields {
                limit: self.limits.max_object_fields,
            }));
        }
        Ok(())
    }

    /// 递归计数一个**已经在内存里**的值。
    ///
    /// 只给进程内调用方：multipart 逐项插入的值、测试夹具、以及 `try_from_value`。wire 入口必须走
    /// [`RequestParameters::parse`] 的有界 visitor——先建整份 `Value` 再调这里，峰值已经发生了。
    pub fn observe_value(&mut self, value: &Value) -> Result<(), RequestStructureViolation> {
        self.count_node()?;
        match value {
            Value::String(text) => self.count_string(text.len())?,
            Value::Array(items) => {
                self.enter_container()?;
                for item in items {
                    self.observe_value(item)?;
                }
                self.leave_container();
            }
            Value::Object(object) => {
                self.enter_container()?;
                for (name, item) in object {
                    self.count_string(name.len())?;
                    self.observe_value(item)?;
                }
                self.count_object_fields(object.len())?;
                self.leave_container();
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
        Ok(())
    }

    fn record(&mut self, violation: RequestStructureViolation) -> RequestStructureViolation {
        if self.violation.is_none() {
            self.violation = Some(violation);
        }
        violation
    }
}

/// 已经计过数的请求参数面：顶层是 JSON 对象，其结构在**构造时**就按 [`RequestJsonLimits`] 核对过。
///
/// 应用层因此只能收到计过数的请求参数（[`crate::take_contract_image_inputs`] 也只要这种类型），
/// 不存在"先建一份无界 `Value` 再交给受理路径"的口子。
#[derive(Debug, Clone)]
pub struct RequestParameters {
    limits: RequestJsonLimits,
    /// 不变量：永远是 [`Value::Object`]，且其结构已按 `limits` 计数。
    value: Value,
}

impl RequestParameters {
    /// wire 入口：有界 visitor 边解析边计数，超限立刻失败，不等整份 `Value` 建好。
    pub fn parse(body: &[u8]) -> Result<Self, RequestJsonError> {
        Self::parse_with_limits(body, REQUEST_JSON_LIMITS)
    }

    /// 同 [`Self::parse`]，但用给定的上限（配置收紧、或测试里拿小上限验边界）。
    pub fn parse_with_limits(
        body: &[u8],
        limits: RequestJsonLimits,
    ) -> Result<Self, RequestJsonError> {
        let mut counter = RequestStructureCounter::new(limits);
        let mut deserializer = serde_json::Deserializer::from_slice(body);
        let parsed = CountedValue {
            counter: &mut counter,
        }
        .deserialize(&mut deserializer);
        let value = match parsed {
            Ok(value) => value,
            // serde 会把自定义错误字符串化；第一条越界记在计数器里，用它还原结构化结论。
            Err(error) => {
                return Err(counter
                    .violation()
                    .map_or(RequestJsonError::Syntax(error), RequestJsonError::Limit));
            }
        };
        if let Err(error) = deserializer.end() {
            return Err(RequestJsonError::Syntax(error));
        }
        match value {
            Value::Object(_) => Ok(Self { limits, value }),
            _ => Err(RequestJsonError::NotAnObject),
        }
    }

    /// 逐项构造非 JSON 编码的请求参数面（例如 `multipart/form-data` 的文本部件）。
    #[must_use]
    pub fn builder() -> RequestParametersBuilder {
        RequestParametersBuilder::new(REQUEST_JSON_LIMITS)
    }

    /// 同 [`Self::builder`]，但用给定的上限。
    #[must_use]
    pub fn builder_with_limits(limits: RequestJsonLimits) -> RequestParametersBuilder {
        RequestParametersBuilder::new(limits)
    }

    /// 计数一个**已经在内存里**的对象。
    ///
    /// 进程内调用方与测试夹具用；wire 入口必须走 [`Self::parse`]（先建后数是本末倒置，见模块注释）。
    pub fn try_from_value(
        value: Value,
        limits: RequestJsonLimits,
    ) -> Result<Self, RequestJsonError> {
        let mut counter = RequestStructureCounter::new(limits);
        if let Err(violation) = counter.observe_value(&value) {
            return Err(RequestJsonError::Limit(violation));
        }
        match value {
            Value::Object(_) => Ok(Self { limits, value }),
            _ => Err(RequestJsonError::NotAnObject),
        }
    }

    /// 同 [`Self::try_from_value`]，输入是对象。
    pub fn try_from_object(
        object: Map<String, Value>,
        limits: RequestJsonLimits,
    ) -> Result<Self, RequestJsonError> {
        Self::try_from_value(Value::Object(object), limits)
    }

    #[must_use]
    pub fn limits(&self) -> RequestJsonLimits {
        self.limits
    }

    /// 参数面本身。
    ///
    /// `parse` 与 `try_from_value` 都只会在顶层是对象时构造成功，所以这里是不可达分支；用
    /// `expect` 而不是静默返回空对象，是为了让"不变量被绕过"立刻暴露，而不是把它读成"没有参数"。
    #[must_use]
    pub fn as_object(&self) -> &Map<String, Value> {
        self.value
            .as_object()
            .expect("RequestParameters is only constructed from a JSON object")
    }

    /// 参数值（进指纹、冻价与映射都借它，不深拷贝）。
    #[must_use]
    pub fn as_value(&self) -> &Value {
        &self.value
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.as_object().get(name)
    }

    /// 摘掉一个参数（图片字段与 `model` 都是这么离开参数面的）。
    pub fn remove(&mut self, name: &str) -> Option<Value> {
        match &mut self.value {
            Value::Object(object) => object.remove(name),
            _ => None,
        }
    }

    /// 消费成对象。
    #[must_use]
    pub fn into_object(self) -> Map<String, Value> {
        match self.value {
            Value::Object(object) => object,
            _ => Map::new(),
        }
    }
}

impl TryFrom<Value> for RequestParameters {
    type Error = RequestJsonError;

    /// 测试夹具与进程内调用方的入口：按缺省上限计数，超限拒绝。
    fn try_from(value: Value) -> Result<Self, Self::Error> {
        Self::try_from_value(value, REQUEST_JSON_LIMITS)
    }
}

/// 逐项构造 [`RequestParameters`] 的计数器（非 JSON 编码的入口用）。
#[derive(Debug)]
pub struct RequestParametersBuilder {
    limits: RequestJsonLimits,
    counter: RequestStructureCounter,
    object: Map<String, Value>,
}

impl RequestParametersBuilder {
    #[must_use]
    pub fn new(limits: RequestJsonLimits) -> Self {
        let mut counter = RequestStructureCounter::new(limits);
        // 顶层对象本身是一层容器。上限里的 `max_depth` 至少是 1（启动校验只允许收紧、不允许 0），
        // 所以这里必然成功；真失败了也只会让第一次 `insert` 的计数立刻报出同一条越界。
        let entered = counter.enter_container();
        debug_assert!(
            entered.is_ok(),
            "a request object is at least one level deep"
        );
        Self {
            limits,
            counter,
            object: Map::new(),
        }
    }

    /// 插入一个字段：先按当前字段数、键的字节数与值的结构计数，全部通过才写进对象。
    pub fn insert(&mut self, name: String, value: Value) -> Result<(), RequestStructureViolation> {
        self.counter.count_string(name.len())?;
        self.counter.count_object_fields(self.object.len() + 1)?;
        self.counter.observe_value(&value)?;
        self.object.insert(name, value);
        Ok(())
    }

    /// 收口成已计数的请求参数面。
    #[must_use]
    pub fn finish(mut self) -> RequestParameters {
        self.counter.leave_container();
        RequestParameters {
            limits: self.limits,
            value: Value::Object(self.object),
        }
    }
}

/// 有界反序列化种子：先记一个节点，再把具体形状交给 [`CountedVisitor`]。
struct CountedValue<'a> {
    counter: &'a mut RequestStructureCounter,
}

impl<'de> DeserializeSeed<'de> for CountedValue<'_> {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: SerdeDeserializer<'de>,
    {
        self.counter.count_node().map_err(de::Error::custom)?;
        deserializer.deserialize_any(CountedVisitor {
            counter: self.counter,
        })
    }
}

/// 边构造 [`Value`] 边计数：每一项都在分配进集合之前核对上限。
struct CountedVisitor<'a> {
    counter: &'a mut RequestStructureCounter,
}

impl<'de> Visitor<'de> for CountedVisitor<'_> {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value within the platform request structure limits")
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
        Ok(Value::Number(Number::from(value)))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        Ok(Value::Number(Number::from(value)))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
        Ok(Number::from_f64(value).map_or(Value::Null, Value::Number))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        // 先核对上限再分配：超限时连这一份拷贝都不做。
        self.counter.count_string(value.len()).map_err(E::custom)?;
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
        self.counter.count_string(value.len()).map_err(E::custom)?;
        Ok(Value::String(value))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: SerdeDeserializer<'de>,
    {
        CountedValue {
            counter: self.counter,
        }
        .deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let counter = self.counter;
        counter.enter_container().map_err(de::Error::custom)?;
        let mut array: Vec<Value> = Vec::new();
        while let Some(element) = sequence.next_element_seed(CountedValue {
            counter: &mut *counter,
        })? {
            array.push(element);
        }
        counter.leave_container();
        Ok(Value::Array(array))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let counter = self.counter;
        counter.enter_container().map_err(de::Error::custom)?;
        let mut object: Map<String, Value> = Map::new();
        let mut fields = 0_usize;
        while let Some(name) = map.next_key::<String>()? {
            counter
                .count_string(name.len())
                .map_err(de::Error::custom)?;
            fields += 1;
            counter
                .count_object_fields(fields)
                .map_err(de::Error::custom)?;
            let value = map.next_value_seed(CountedValue {
                counter: &mut *counter,
            })?;
            object.insert(name, value);
        }
        counter.leave_container();
        Ok(Value::Object(object))
    }
}

#[cfg(test)]
mod tests;
