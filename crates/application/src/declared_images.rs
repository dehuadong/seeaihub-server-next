//! 声明面里的**输出张数**上限：`properties.n.maximum`（`n` 在承载面上叫别的线上名时，用那个名字）。
//!
//! 这个数有两处用处，都要有一个**来源**，不能写死在代码里（合同改了没人知道），也不能拿输入参考图
//! 上限顶替（`restrictions.max_reference_images` 说的是**一次能带几张图进去**，与**一次能要几张
//! 图出来**无关）：
//!
//! - **超时链的上限**（启动时算）：来源是**合同**自己给 `n` 声明的取值面。一次发布的合同对全平台
//!   生效，所以取的是**所有在效合同里最大的那个 `n`**；一条都没发布时取
//!   [`crate::NO_CONTRACT_MAX_OUTPUT_IMAGES`]，那只是让"还没发布过"的部署也能跑起来，不是取值来源。
//! - **受理时发往上游的张数**：来源是**这条候选承载面**为它声明的上界——各候选不同，所以它是逐次
//!   判的（见 `crate::plan_candidate`）。

use serde_json::Value;

/// 从声明面里读 `n`（输出张数）的**上限**：`properties.<名字>.maximum`，读不出就是 `None`。
///
/// 缺 `properties`、缺这个名字、不是对象、没写 `maximum`，都算"没有声明"。名字由调用方给：合同
/// 那一侧是字段名 `n`，候选承载面那一侧是改名表把它落到的**线上名**（与承载校验用同一条规则）。
///
/// 这个读法**不带任何取值语义**，`maximum: 0` 会原样读出来：两条消费路径对 0 的解释不同（见
/// [`declared_output_images`] 与 `crate::declared_n_maximum`），所以过滤留给各自那一侧。
#[must_use]
pub fn declared_output_image_maximum(schema: &Value, name: &str) -> Option<u64> {
    schema
        .get("properties")
        .and_then(|properties| properties.get(name))
        .and_then(|n| n.get("maximum"))
        .and_then(Value::as_u64)
}

/// 某一条合同声明的输出张数上限：`(网关模型名, 上限)`。名字用来在启动日志里说清这个数哪来的。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredOutputImages {
    pub gateway_model: String,
    pub maximum: u64,
}

/// 从合同的能力 Schema 里读 `n`（输出张数）的上限。
///
/// 声明成 `0` 按没声明算——这是**超时链**的口径："要 0 张"不是一次生成，按 0 比会让那条上限恒真。
#[must_use]
pub fn declared_output_images(
    gateway_model: &str,
    capability_schema: &Value,
) -> Option<DeclaredOutputImages> {
    let maximum =
        declared_output_image_maximum(capability_schema, "n").filter(|declared| *declared > 0)?;
    Some(DeclaredOutputImages {
        gateway_model: gateway_model.to_owned(),
        maximum,
    })
}

#[cfg(test)]
mod tests;
