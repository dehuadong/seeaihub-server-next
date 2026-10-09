> 归属状态（2026-10-09）：本文件是原地保留的归档资料，原修订与状态头只说明历史，不拥有新工作的行为与验收，也不授予实施授权。未来依赖工作先在对应 Proposal/Issue 确认适用范围和验收；具体承接尚未完成，见[归属切换登记](../agents/document-ownership-transition.md#历史产品合同)。

主题: OpenAI 兼容的模型列表
当前修订: v1
生效修订: v1
状态: 已接受
依赖: [控制台 Spec](0001-admin-and-customer-consoles.md) v19（`GET /v1/models` 既有字段的冻结）、[模型类型 Spec](0006-model-type-and-usage-records.md) v1（只增字段的先例）、[模型使用文档 Spec](0008-model-usage-documentation.md) v2（同一条目的 `documentation_url`）

# OpenAI 兼容的模型列表

本 Spec 拥有 `GET /v1/models` 的 OpenAI 兼容响应形状：列表信封与每条模型的标准字段。目录的可见性判据与既有字段的来源归[控制台 Spec](0001-admin-and-customer-consoles.md)、[模型类型 Spec](0006-model-type-and-usage-records.md) 与[模型使用文档 Spec](0008-model-usage-documentation.md)；标准字段是既有字段的投射，不引入第二份事实。术语沿用 [GLOSSARY.md](../../GLOSSARY.md)。

## 1. 目的

`GET /v1/models` 是 OpenAI-compatible API 体系的标准端点：客户端用它验证 API Key、确认模型可用、取模型标识、排查配置。平台现有形状是自有字段，标准客户端与现成工具读不到模型标识。本 Spec 在只增字段的前提下让该端点返回标准列表形状。

## 2. 合同

`GET /v1/models` 无需凭据，成功返回 `200`。对客响应：

- 顶层除既有 `data` 外增加 `object`，取值固定 `"list"`。
- `data` 的每条除既有 `name`、`vendor_id`、`revision`、`type`、`contract`、`documentation_url` 外增加：
  - `id`：与同条 `name` 同值，客户端把它填进请求的 `model`。
  - `object`：固定 `"model"`。
  - `created`：Unix 秒，取该条**当前 Runtime Revision 的发布时间**。
  - `owned_by`：与同条 `vendor_id` 同值。
- 既有字段的名字与取值不变；新增字段排在既有字段之前，既有字段之间的相对顺序不变；可见模型集合与顺序不变；不新增端点，错误响应形状不变。

## 3. 兼容边界

- 只增字段：只读既有字段的客户端不受影响。
- 标准客户端按 `data[].id` 取模型标识；平台文档与示例继续以 `name` 为准，两者同值。
- `created` 是平台这条目录条目当前发布的生效时间，不承诺等于厂商侧的模型创建时间。

## 4. 验收

| 编号 | 验收条件 |
| --- | --- |
| A1 | 顶层含 `object: "list"`；每条含 `id`、`object: "model"`、`created`（整数 Unix 秒）、`owned_by`。 |
| A2 | `id` 与 `name`、`owned_by` 与 `vendor_id` 同值；`created` 等于该条当前 Runtime Revision 的发布时间。 |
| A3 | 既有六个字段的名字与取值不变，它们之间的相对顺序不变；可见模型集合与顺序不变。 |
| A4 | 只读标准字段的客户端（按 `data[].id` 取模型标识）能解析出全部可见模型。 |

## 5. 修订记录

| 修订 | 章节 | 合同变化摘要 | 生效 |
| --- | --- | --- | --- |
| v1 | 全文 | 目录响应只增 OpenAI 模型列表的标准字段：顶层 `object`，每条 `id` / `object` / `created` / `owned_by`；既有字段不变。 | 已接受／v1 |
