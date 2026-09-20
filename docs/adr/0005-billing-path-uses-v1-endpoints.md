---
status: accepted
---

# AIHubMix 的正式计费路径走 OpenAI 兼容的 `/v1`；它的 `/ai/v1` 只列为已验证能力

**本条只讲 AIHubMix。** 该渠道的首期生产 Offering 走 `/v1/images/generations` 与 `/v1/images/edits`：它们返回完整的分项 token，能形成可核验的 Metering Evidence。它的原生异步 `/ai/v1` 创建与详情都**没有 `usage`**，因此保留为 Adapter 的已验证能力，不作为正式计费的执行路径。Adapter 按原生图片参数分流：无参考图走 generations，有则走 edits，遮罩可选。

**不适用于其它渠道**：APIMart 也有一个叫 `/v1/images/generations` 的路径，但它是**任务式**、`/v1` 上没有同步结果面，其事实来源与成本口径见 [0016](./0016-cost-basis-per-channel.md)。

**代价与切换条件**：代价是放弃 AIHubMix `/ai/v1` 的原生异步与统一入口——endpoint 与 JSON/multipart 的差异只存在于 Adapter，应用层仍是一个命令、一个 Job、一套结算流程。`/ai/v1` 在满足任一条件后可通过新 Runtime Revision 发布：任务详情返回可核验 usage；存在可按 task ID 关联的账单 API；或产品另行明确一种不依赖 Provider usage 的计价合同。
