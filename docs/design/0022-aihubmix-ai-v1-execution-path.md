主题: AIHubMix 执行路径改走 /ai/v1：URL 参考图、任务标识与成本口径
当前修订: v1
状态: 待接受
承接: [同步图片网关 Spec](../specs/0005-synchronous-image-gateway.md) §1–§6；渠道事实见 [`docs/facts/channel-facts.md`](../facts/channel-facts.md) §2
依赖: [同步网关设计](0017-synchronous-image-gateway.md)、[ADR 0006 计量证据](../adr/0006-metering-evidence.md)、[ADR 0007 对账](../adr/0007-reconciliation-instead-of-automatic-retry.md)

# AIHubMix 执行路径改走 `/ai/v1`

本稿拥有 AIHubMix 一族的执行端点选择、请求与响应形状、任务标识与对账取回方式、成本与计量的口径。对客行为合同仍由[同步图片网关 Spec](../specs/0005-synchronous-image-gateway.md)拥有：请求只收公网 URL，成功回 `{created, data:[{url|b64_json}]}`，每项只保留一种形式。

## 1. 现状与问题

平台的编辑分支现在走 `/v1/images/edits`（OpenAI 兼容、`multipart/form-data`）。该端点**只收文件部件**：单值 `image` 与数组 `image[]` 都拒绝字符串。真实上游原话（2026-10-06 实测）：

```text
Invalid type for 'image': expected one of an array of files or file, but got a string instead.
Invalid type for 'image[]': expected a file, but got a string instead.
```

因此"参考图只收公网 URL、平台不下载"这条对客合同无法在该端点上成立：要么平台把 URL 取成字节（内存过一手、并占对客同步窗口），要么换端点。本稿选换端点。

换端点的理由只有一条：媒体引用要收公网 URL。平台不把这条通路与查找历史任务挂钩——同步调用没有等待前的 id，也不为丢失的响应设计找回路径。

## 2. 目标端点与形状

`POST /ai/v1/images/generations`（同一个 Schema 覆盖 flare 与 sunburst，`schema_version` 1.2）：

- 参考图与遮罩是**媒体引用**：`image`（单张，别名 `images[0]`）、`images`（数组，≤16）、`mask`。取值是**公网 URL 字符串**或 `{url}` 对象，也接受 data URI 与裸 base64。
- 其余可声明的字段：`model`、`prompt`、`n`、`size`、`output_format`、`extra`、`webhook_url`、`webhook_events_filter`、`async`。
- `additionalProperties: false`：未声明字段是硬拒绝（实测 `response_format` 被拒：`Unknown request parameter`）。**以此 Schema 为准**，文档列出的 `response_format`、`aspect_ratio`、`seed`、`negative_prompt` 不在其中。
- `content_type` 是 `application/json`。
- **模型专属字段必须放进 `extra`**：`quality` / `background` / `moderation` / `output_compression` / `user` 在 `extra` 里，且 `extra` 自己也是 `additionalProperties: false`。顶层放这些字段是硬拒绝（实测 `Unknown request parameter`）。
- 承载面的落位因此需要一层容器：发布映射要能把合同字段指到 `extra.<名>` 上，请求体按点号嵌套写出。这是映射机制内的扩展，对客合同不变。描述子里 `supported_extra_parameters` 已经存在且发布期会校验，加上这条落位它才真正可用。

## 3. 执行流程：同步提交

用**同步**（不传 `async`）：响应本身就是完成态任务对象，**带 `b64_json`**。异步轮询回来的 `b64_json` 是空的，只有 `content_url`；而平台不下载结果字节，所以只能走同步。

1. **提交**：`POST /ai/v1/images/generations`，不传 `async`。生成完成后直接回完成态任务对象（实测 50.4 秒，`status: "completed"`）。
2. **结果**：`output[]` 每项的 `b64_json` 非空（实测 2,160,484 字符）；对客逐项回 `b64_json`，保持 Spec 0005 的"每项只保留一种形式"。平台不下载、不转存。
3. **标识**：响应里的 `id` 是上游任务 id，落到 `attempts.provider_trace_id`。
4. **超时与丢失**：等待期内连接中断或超时就是拿不到结果，平台不按列表或其他手段去找回这次任务：按 ADR 0007 进人工对账，与今天 `/v1` 的处境相同。

不轮询、不用 Webhook、不做列表匹配；平台也不给这条通路加按句柄查询计量的能力。`content_url` 不交给调用方：它需要平台凭据（实测无凭据 401、带凭据 200），且上游列为有下载次数上限、过期返回 `410 artifact_expired` 的资源。

超时沿用现行链，不改：单次上游调用取 `min(适配器配置, 本次执行期限剩余)`（[`external_call_timeout`](../../crates/adapter-sdk/src/gateway.rs)），全程受 `GENERATION_SYNC_WAIT_SECONDS` 约束，`PROVIDER_TIMEOUT_SECONDS` 与 `base + per_image × 图片数` 的算法不变（[`RequestTimeoutPolicy`](../../crates/application/src/request_timeout.rs)）。到点拿不到结果就按受理状态不确定处置。

## 4. 成本与计量口径

终态任务对象带 `usage.cost`（USD）——异步创建的任务也一样（实测完成态 `0.01422`）。它与 APIMart 的 `cost` 同性质：**上游声明的实际扣费**，取它，不用公开费率反算。

两处与现状不同：

- `cost` 可能为 `null`（历史任务里有）。空值按**成本缺口**记（`ProviderCost::Unavailable`），不猜金额。
- **没有任何 token 分项**：`usage` 只有 `cost` 一个字段。这条通路的成功件因此没有四分项计量证据。

## 5. 未决的合同决定

以下三项超出本稿权限，需在你这一层定：

1. **成功件必须有计量证据这条规则要改。** 现行规则是"成功件 `usage` 必须为 `Some`"（[ADR 0006](../adr/0006-metering-evidence.md)）。本通路给不出 token，只能 `None`。改成"声明了成本的渠道，成功件允许没有 token 分项"。
2. **对客计价形态要对齐。** 现行供给用 `token_rates` 按 token 计价，本通路没有 token。可选项是按上游声明成本加价（`upstream_declared` + `markup_bps`）——与 APIMart 同一形态。
3. **`prompt_only` 分支是否一起搬。** 搬＝整条渠道一个形状、`declares_cost` 单一取值、成本取上游实际扣费；不搬＝保留 `/v1` 的 token 分项，但同一 adapter 内两种成本形态并存，`declares_cost` 无法用一个布尔表达。

## 6. 失败与对账

- `failed` / `cancelled`：任务对象的 `error` 给 `code`、`message`、可选的 `upstream_detail`。
- **已受理之后的中断**（同步等待期内超时、连接断开）都是受理状态不确定：按 ADR 0007 进 `reconciliation_required`，不自动重提。
- 与今天的差别：响应到手时里面有上游任务 id，落到 Attempt 的 `provider_trace_id` 供审计；失败或中断进对账后由人工处置，路径不变。

## 7. 影响面

| 面 | 改动 |
| --- | --- |
| `crates/adapter-aihubmix` | 端点、请求体（`image`/`images`/`mask` 传 URL 字符串）、结果取响应里的 `b64_json`、成本取 `usage.cost`；`declares_cost` 改成 `true`；按任务 id 查计量 |
| Runtime Revision | 该渠道的供给要重新发布（计价形态与承载面） |
| `crates/domain` 的参数映射与 `crates/application` 的发布校验 | 允许把合同字段落进承载面的容器字段（`extra.<名>`），请求体嵌套写出 |
| `docs/facts/channel-facts.md` §2 | 端点表、字段面、成本与计量、结果取回、任务查询 |
| `docs/architecture.md` | AIHubMix 一族的端点与结果描述 |
| 用例 | 断言上游收到的是 `/ai/v1` 与 URL 字符串；断言结果取响应里的 `b64_json` 且平台不发结果下载请求；断言成本取 `usage.cost`、空值进缺口 |

## 8. 验证

受控实测（2026-10-06，真实付费调用）已经取证：同步提交 URL 参考图 HTTP 200、响应 `status: "completed"` 且 `output[0].b64_json` 非空（2,160,484 字符）、`usage.cost` 有值；异步形态的完成态 `b64_json` 为空、只有需凭据的 `content_url`（实测无凭据 401），且异步完成态同样带 `usage.cost`。实施后的验证按 Spec 0005 的验收项跑，并补一条"结果不下载、直接取响应 `b64_json`"的用例。
