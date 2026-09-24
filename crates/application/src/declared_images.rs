//! 合同声明的**输出张数**上限：`capability_schema.properties.n.maximum`。
//!
//! 超时链上那条"按最大输出张数算出来的上限"要有一个**来源**：不能写死在代码里（合同改了没人
//! 知道），也不能拿输入参考图上限顶替（`restrictions.max_reference_images` 说的是**一次能带几张图进去**，
//! 与**一次能要几张图出来**无关）。来源就是合同自己给 `n` 声明的取值面：合同说能要到 `n` 张，
//! 这一档请求就必须落在超时链内。
//!
//! 一次发布的合同对全平台生效，所以启动时取的是**所有在效合同里最大的那个 `n`**；一条都没发布
//! 时取 [`crate::NO_CONTRACT_MAX_OUTPUT_IMAGES`]，那只是让"还没发布过"的部署也能跑起来，不是取值
//! 来源。

use serde_json::Value;

/// 某一条合同声明的输出张数上限：`(网关模型名, 上限)`。名字用来在启动日志里说清这个数哪来的。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredOutputImages {
    pub gateway_model: String,
    pub maximum: u64,
}

/// 从合同的能力 Schema 里读 `n`（输出张数）的上限。
///
/// 合同缺 `properties.n`、`n` 不是对象、或没写 `maximum`，都算"没有声明"：那种模型收不到 `n`
/// 这个参数，一次请求只生成一张。`maximum` 是 0 也按没声明算——要 0 张不是一次生成。
#[must_use]
pub fn declared_output_images(
    gateway_model: &str,
    capability_schema: &Value,
) -> Option<DeclaredOutputImages> {
    let maximum = capability_schema
        .get("properties")
        .and_then(|properties| properties.get("n"))
        .and_then(|n| n.get("maximum"))
        .and_then(Value::as_u64)
        .filter(|maximum| *maximum > 0)?;
    Some(DeclaredOutputImages {
        gateway_model: gateway_model.to_owned(),
        maximum,
    })
}

#[cfg(test)]
mod tests;
