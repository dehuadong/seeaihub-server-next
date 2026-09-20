---
status: accepted
---

# Vendor 与 Provider 身份不合并

Vendor 是定义模型产品与原生能力的厂商，Provider 是实际提供调用、任务与结果的服务方；两者在目录中始终是两个独立身份。因此同一个 Vendor Model 由多个 Provider 供应时，只新增 Offering / Channel / Price Plan，**不复制 Vendor Model**——合并会让同一模型在不同供给下的身份冲突，价格与渠道限制也无处安放。对外身份 `native_model_id` 与调用上游用的 `provider_model_id` 分开固化，Adapter 只用后者组装请求。
