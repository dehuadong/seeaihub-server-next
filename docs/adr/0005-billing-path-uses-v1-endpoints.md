# 正式计费路径走 OpenAI 兼容的 `/v1`，`/ai/v1` 暂列为已验证能力

AIHubMix 为 `gpt-image-2` 公开三条路径，实测结论决定了职责划分：`/ai/v1/images/generations` 是原生异步、返回短期受保护 URL，但创建与详情都**没有** `usage`；`/v1/images/generations`（JSON）与 `/v1/images/edits`（multipart）是上游同步、返回 Base64 PNG，且带完整文本输入 / 图片输入 / 图片输出 token。

因此首期生产 Offering 的执行路径是 `/v1`：Adapter 按原生图片参数分流——无 `image`/`images` 调 `generations`，有则调 `edits`（mask 可选）。`/ai/v1` 保留为 Adapter Descriptor 中的已验证能力，不作为正式计费的执行路径，原因是任务对象没有 usage，无法形成精确的最终 Evidence。

只有满足以下任一条件后，`/ai/v1` 才可通过新 Runtime Revision 发布：任务详情返回可核验 usage；存在可通过 task ID 关联的账单 API；产品另行明确一种不依赖 Provider usage 的计价合同。切换不需要修改应用层或公开协议，只发布新的 Adapter 执行策略 / Offering 修订。

**代价**：放弃 `/ai/v1` 的原生异步与统一入口。endpoint 与 JSON/multipart 的差异只存在于 Adapter，平台应用层仍只有一个 Command、一个 Job 和一个结算流程；Provider 同步也不等于平台同步——调用方先拿到持久 Job，Worker 在后台等待并维护 lease/heartbeat。

**来源**：技术设计 v5 取代 v4 的「生产路径优先使用 `/ai/v1`」结论。
