# Vendor 与 Provider 身份不合并

Vendor 是定义模型产品与原生能力的厂商（首期 `OpenAI`），Provider 是实际提供调用、任务和结果下载的服务方（首期 `AIHubMix`）。两者在目录中始终是两个独立身份：OpenAI `gpt-image-2` 作为 Vendor Model，AIHubMix 与其 Adapter、Channel、Offering 作为供给面。

因此以后其他 Provider 也供应 `gpt-image-2` 时，只新增 Offering / Channel / Price Plan，**不复制 Vendor Model**。合并会让同一模型在不同供给下的身份发生冲突，并使价格与渠道限制无处安放。

平台 `native_model_id`（厂商原生模型 ID）与调用上游所需的 `provider_model_id` 分开固化，Adapter 只使用后者组装供应商请求。

**来源**：技术设计 v4，v5 明确继续有效。
