> 归属状态（2026-10-09）：归属切换未完成；本文件适用且已接受的范围暂保留旧属主，原待评审／待接受内容不因此获批准。依赖它的新工作先核对有效内容及评审缺口，不得默认沿用。 全部映射与未决影响见[归属切换登记](../agents/document-ownership-transition.md)。以下原文与状态头保留其历史身份，不扩大本文权威。

---
status: accepted
---

# Vendor 与 Provider 身份不合并

Vendor 是定义模型产品与原生能力的厂商，Provider 是实际提供调用、任务与结果的服务方；两者在目录中始终是两个独立身份。因此同一个 Vendor Model 由多个 Provider 供应时，只新增 Offering / Channel / Price Plan，**不复制 Vendor Model**——合并会让同一模型在不同供给下的身份冲突，价格与渠道限制也无处安放。厂商原生名 `native_model_id` 与调用上游用的 `provider_model_id` 分开固化，Adapter 只用后者组装请求；对客看到的那个名字是 **Gateway Model**，它与这两个都不是同一个角色（见 `GLOSSARY.md` 与 [`0006`](../design/0006-gateway-models-and-consumer-surface.md) §1）。

**反悔成本**：改它要把 `catalog.vendor_models` 与 `supply.channels` / `supply.offerings` 的身份并成一份，并迁移已发布的 `native_model_id` / `provider_model_id` 固化值，以及引用它们的 Runtime Revision 与 Job 快照。

**当时没有比较过可选方案**：仓库里没有任何"把 Vendor 与 Provider 身份合并"被评估过的记录（`docs/design/`、`docs/facts/`、`.agents/notes/` 与其 `rejected/` 都没有），它是从技术设计里直接抽出来的身份划分。留在 ADR 是因为反悔成本真实存在且"两个角色何时该合并"脱离上下文会令人困惑；但**它没有经过权衡**，这一点照实记下来，以免后人以为漏写了备选。
