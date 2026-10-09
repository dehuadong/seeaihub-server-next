---
title: AIHubMix 改走 /ai/v1：公网 URL 参考图与声明金额计量
status: implemented
created: 2026-10-06
updated: 2026-10-09
approval: 用户 2026-10-06 逐条裁定：两条对客入口都保留、上游改走 `POST /ai/v1/images/generations`、参考图与遮罩传公网 URL、同步取内联 `b64_json`（平台不下载结果）、成本取上游声明的 `usage.cost`、超时沿用现行链且不做找回；同日批准"声明了成本的渠道，成功件允许没有 token 分项"与"对客计价按上游声明成本加价"，并给出执行授权
verification: `cargo test -p seeai-adapter-aihubmix --lib`（22 条，含端点、媒体引用、成本三态、失败任务带回已读到的金额）、`cargo test -p seeai-domain --lib`（含按 token 计价缺证据就明确失败）、`cargo test -p seeai-adapter-sdk --lib`、`cargo test -p seeai-api --test http_contract -- --ignored cases_aihubmix`（3 条端到端：URL 参考图逐字进请求体、平台不取结果）、`cases_direct_execution`（21 条）、`cases_cache`（12 条）、`cases_cost_facts`（10 条）；真实付费调用（2026-10-06）确认 `/ai/v1` 收 URL 参考图、同步响应带内联 `b64_json` 与 `usage.cost`
---

# Agent Note：AIHubMix 改走 /ai/v1：公网 URL 参考图与声明金额计量

## 问题

对客合同要求参考图与遮罩只收公网 URL、平台不下载（[历史同步图片网关 Spec](../../../../docs/specs/0005-synchronous-image-gateway.md) §1、§3）。而 AIHubMix 原来的编辑端点 `POST /v1/images/edits` **只收文件部件**：单值 `image` 与数组 `image[]` 传字符串都被上游拒（2026-10-06 实测，`invalid_type`：`expected one of an array of files or file, but got a string instead`）。所以那条路上要么平台把 URL 取成字节（内存过一手、并占对客同步窗口），要么换端点。

同一渠道的 `/ai/v1/images/generations` 把参考图声明成**媒体引用**（公网 URL 字符串或 `{url}`），一个端点同时覆盖文生图与图生图；它的同步响应是完成态任务对象，带内联 `b64_json`、上游任务 id 与 `usage.cost`。本记录完整拥有当前技术设计，[设计 0022](../../../../docs/design/0022-aihubmix-ai-v1-execution-path.md)只保留历史来源；归属接受结果见[切换登记](../../../../docs/agents/document-ownership-transition.md)。

## 决定

AIHubMix 一族的图片执行全部改走 `POST /ai/v1/images/generations`，**同步**形态：

- 端点只有一个，分支由请求里有没有图片字段决定；参考图与遮罩按冻结的图片位写**公网 URL 字符串或字符串数组**，Driver 不改写名字，只按承载面声明的形态写数组还是单值。
- 模型专属字段（`quality` / `background` / `moderation` / `output_compression` / `user`）在那个端点里收在 `extra` 里。参数映射因此支持**容器目标**（`{"rename": {"quality": "extra.quality"}}`），承载面声明容器、发布期按容器成员判合同可达性。
- 结果取响应里 `output[].b64_json`；`content_url` 要平台凭据（实测无凭据 401），既不交给调用方也不由平台下载。
- 成本取 `usage.cost`（USD，可为 `null`）：有值记 `declared`，缺席或读不出记**成本缺口**。`declares_cost` 因此从 `false` 改成 `true`，该渠道的供给按 `upstream_declared` 重新发布。
- 任务 id 落到 Attempt 的 `provider_trace_id` 供审计；超时或中断按受理状态不确定进对账，**不做列表匹配之类的找回**，也不给这条通路加按句柄查询计量的能力。
- 超时链不变：单次上游调用取 `min(适配器配置, 本次执行期限剩余)`，全程受 `GENERATION_SYNC_WAIT_SECONDS` 约束。

同一次变更把结算闸门按 ADR 0006 的原文对齐：那条 ADR 早已写明"计量形态跟着渠道的计费方式走——直接声明金额就用那句金额"，而代码此前要求成功件必须有 token 分项。现在**声明了成本的渠道允许成功件没有 token 分项**，计量依据就是那句声明金额；按 token 计价的算式拿到没有 evidence 的成功件时明确失败（`DomainError::MissingMeteringEvidence`），不按 0 结算。

配套的发布期把关落在驱动器能力上（`AdapterDescriptor::provides_token_usage`）：这条通路只声明金额、给不出四分项用量，对客选 `token_rates` 的候选发布期就拒并点名，不等到每一笔请求都落进对账。同一处也守另一条：声明 `upstream_declared` 的候选要求通路真的会把金额交回来。这两条能力随运营的可选清单一起回给界面（`SelectableOfferingView` 的 `declares_cost` 与 `provides_token_usage`），发布页的对客形态下拉据此只摆这条通路算得出的那一种。

## 技术设计

### 请求构造

Adapter 使用 JSON 同步提交，不设置 `async`，不使用 Webhook。厂商合同字段仍是调用方输入，Offering 的容器映射将 `quality`、`background`、`moderation`、`output_compression`、`user` 写入 `extra.<名>`；承载面声明容器成员，发布期验证成员可从合同到达。对这条通路，`extra` 与顶层未声明字段不会凭空扩展，上游字段面来源归[渠道事实](../../../../docs/facts/channel-facts.md)。上游 Schema 能收 data URI 或裸 base64 不扩大平台输入合同；平台只把公网 URL 写入冻结图片位。

### 同步结果与身份

响应本身是终态任务对象。只解析 `output[].b64_json` 的内联图片，不取 `content_url`，不下载、不转存结果；任务对象的 `id` 写到 Attempt 的 `provider_trace_id`，用于审计。异步任务只有需要平台凭据的结果 URL，不能满足本路径的结果边界。没有等待前就取得的句柄，也没有按任务列表、轮询、Webhook 或启发式匹配恢复丢失响应的机制。

成功件内存事实经平台结算确认后交给 API 响应投影；渠道任务 id 不替代平台调用 id。对客信封与积分的历史行为见[历史同步网关合同](../../../../docs/specs/0005-synchronous-image-gateway.md)；技术理由见[响应信封 Note](2026-10-07-consumer-response-envelope-and-points.md)，新工作的响应行为与验收归对应 Issue，渠道形态不决定平台信封。

### 证据、能力与失败

`usage.cost` 为 USD 声明金额，有值时记 `ProviderCost::Declared`；缺失、null 或不可解析时记 `ProviderCost::Unavailable`，保留成本缺口，不猜零。成功件没有 token 四分项，驱动器声明 `declares_cost = true`、`provides_token_usage = false`。发布归一据描述子拒绝不具备证据能力的对客计价形态：token 计价缺 token 能力不能发布，声明金额计价要求真实声明金额能力。费率、倍率与折算率随受理快照冻结，已有发布需按渠道事实重新发布。

失败终态解析 `error.code` 与 `message`，未解析上游可选的 `upstream_detail`，带回已经读到的金额和任务 id，不把成本事实当成功证明。同步提交之后的超时、断开或未知结果进入异常对账，不重新提交；单次调用上限由 `external_call_timeout` 取 Adapter 配置与本次执行剩余期限的较小值，总期限变量为 `GENERATION_SYNC_WAIT_SECONDS`。

## 备选方案

- **保留 `/v1/images/edits`，由平台把 URL 取成字节再发文件部件。** 这是改动前的形状，也是本次先落地又撤掉的一版。撤回的理由：它把字节搬回对客同步窗口，且与"参考图只收公网 URL、平台不下载"的既有取向相反；上游本来就有收 URL 的端点。
- **异步提交 + 请求内轮询。** 好处是任务 id 在任何长等待之前就拿到，超时后能按 id 精确查回。否决的理由：异步任务终态只给 `content_url`（`b64_json` 为空），而那个地址要平台凭据 —— 等于要求平台下载结果字节。
- **同步提交 + 响应丢失时按 `GET /ai/v1/images` 列表找回任务。** 上游文档明列这个用途，实测同步任务也确实在列表里。用户裁定不要：同步就是"行就行、不行就不行"，找回是启发式，不做。
- **保留 token 计量、只把金额当参考。** 不成立：这条通路的响应里没有任何 token 分项（全文检索 `token` 0 次）。

## 后果

- 请求仍只收公网 URL；当前成功信封由[响应信封 Note](2026-10-07-consumer-response-envelope-and-points.md)承接 2026-10-07 的后续变化，本记录不拥有信封格式。
- 这条渠道的**用量记录不再有 token 分项**，只有上游声明的金额；对客计价相应改成"声明金额 × 冻结倍率 × 冻结折算率"。
- 承运面的形状多了一层容器：合同的 `quality` 等顶层字段落到承载面的 `extra.<名>`。
- 撤销了一版实现（参考图当 URL 文本部件发给 `/v1/images/edits`）：那条路径对这条渠道不可用（实测 502，上游原话为"要文件"）。
- 旧记录"金额型计量证据被否决"的前提在这一条渠道上不再成立（它的响应没有 token），边界见[那份记录](../../rejected/domain/2026-09-19-provider-declared-charge-as-metering-evidence.md)。
- 端到端夹具里"渠道不给金额 ⇒ 平台按实际用量自算成本"那条用例随渠道事实一起撤销：这条渠道的执行面总会声明金额，平台自算成本的算式由 `crates/application/src/tests/cost_facts.rs` 的用例覆盖。

## 验证

- `cargo test -p seeai-adapter-aihubmix --lib`：22 条，含端点唯一、媒体引用写进请求体、成本三态、结果只认内联 base64；结果缺失时失败件仍带着已经读到的金额回去。
- `cargo test -p seeai-domain --lib`：按 token 计价的候选拿到没有 token 分项的成功件时返回 `DomainError::MissingMeteringEvidence`，不按 0 结算。
- `cargo test -p seeai-api --test http_contract -- --ignored cases_aihubmix`：3 条端到端，含"两张参考图都逐字进请求体""平台不发结果下载请求""内联 base64 原样交回"；`cases_direct_execution` 21 条、`cases_cache` 12 条、`cases_cost_facts` 10 条同时通过（声明的金额计价形态下结算、持仓与用量都对得上）。
- 真实付费调用（2026-10-06）：`/ai/v1` 收两张公网 URL 参考图、同步 200、响应带 `output[0].b64_json` 与 `usage.cost = 0.026436`；`/v1/images/edits` 传 URL 字符串被 400 拒（两次探针）。
