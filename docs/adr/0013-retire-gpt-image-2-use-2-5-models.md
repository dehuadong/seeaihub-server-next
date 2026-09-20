# 运营决策：退役 `gpt-image-2`，改用 `gpt-image-2.5-flare` 与 `gpt-image-2.5-sunburst`

**决策**：作为项目运营方，**决定不再把 `gpt-image-2` 作为在售模型**，后续使用 `gpt-image-2.5-flare` 与 `gpt-image-2.5-sunburst` 两款。同时，`gpt-image-2-official` 与 `gpt-image-2` **视为同一模型**，二者的差异只在命名与所提供的参数能力。

**这是运营/产品决定，不是从上游公告推导出的事实。** 记录此点很重要：本仓库没有、也不需要「`gpt-image-2` 已被上游退役」的平台侧证据（实测见 `docs/research/gpt-image-2.5-schema-research.md` §4：AIHubMix 退役清单未含该模型）。**决定的下游后果与上游是否公告无关**——目录里卖什么由运营方定，平台的职责是让「退役旧供给、发布新供给」这件事可执行、可审计、可回滚。

## 后果

1. **第一阶段的正式计费 Offering（AIHubMix → `gpt-image-2`）进入退役流程**：不再作为在售供给，不再受理新 Job。**已受理的 Job 不受影响**——它们固定了受理时的 Vendor Model Revision、Offering 与 Price Snapshot（[0003](./0003-postgresql-is-source-of-truth.md)）。退役是「停止新受理」，不是「改写已完成的事实」。
2. **替换目标为两款 2.5**：`gpt-image-2.5-flare`（速度优先，适合日常高质量出图与批量）与 `gpt-image-2.5-sunburst`（编辑精度优先）。两款在 AIHubMix 的机器 Schema 中**端点与顶层参数集合完全一致**，差异集中在 `extra.quality` 与 `extra.moderation`（见下）。
3. **`quality` 档位是两者的主要能力差异**：`gpt-image-2` 为 `low|medium|high`；2.5 两款为 `low|medium|high|xhigh|max|auto`（默认 `auto`，新增 `moderation`）。（APIMart 侧另有一条关于 `xhigh`/`max` 传给 `gpt-image-2` 会返回 400 的边界事实——那是**另一个渠道**的事实，见 `docs/facts/channel-facts.md` §3；本 ADR 不据它做结论。）
4. **`gpt-image-2-official` 与 `gpt-image-2` 同源这一判断，其用途是把两个接入名归一为同一 Vendor Model**，从而使「同一 Vendor Model 由多个 Provider 供应」可用而不必把 official 侧当作另一个模型。**本 ADR 记录的是运营方的归一判断**；平台侧的支撑是：official 侧带分项 token `usage`，与第一阶段计量维度一致，因此归一后**不需要**引入新的证据类型。

## 对第二阶段规划的影响（要重做的那部分）

第二阶段原设计引入的是**第二个 Vendor 的 Vendor Model**（ByteDance 的 `doubao-seedream-5-0-260128`，由字节自己的托管服务火山方舟供应），动因是「需要引入不同计量维度」（按张计费 ⇒ 结算模型泛化 ⇒ 需要 `MeteredUsage` 联合类型）。

**该选择从一开始就无法满足第二阶段的核心命题，这一点当时被我写混了**：火山方舟供应的是 **ByteDance 自有的 Seedream**，它**不供应 `gpt-image-*`，也不供应 `gemini-image`**——它和 AIHubMix 根本不在同一个 Vendor Model 上竞争。按 `CONTEXT.md` 的词汇，Seedream 的 Vendor 是 ByteDance、Provider 是火山方舟；而 AIHubMix 供应的是 Vendor OpenAI 的模型。**拿它们比「谁供应同一个模型」是范畴错误**：二者供应的是**不同 Vendor 的不同模型**，因此「同 Provider/不同 Vendor」不构成「同一 Vendor Model 由多个 Provider 供应」的样本。

改用 2.5 之后，这一命题才有了真实样本（**同一 Vendor = OpenAI，两个 Provider = AIHubMix 与 APIMart**）：

- **AIHubMix 与 APIMart 都供应 `gpt-image-2.5-flare` 与 `-sunburst`**（两者在各自文档中作为可用的 `model` 取值出现）。因此「**同一 Vendor Model 由两个不同 Provider 供应**」这一第二阶段的核心命题**有了真实样本**，不再需要以「同 Provider 两个 Offering」代替；
- 但两家的**响应里返回什么计量事实尚未结清**：AIHubMix 的 OpenAI 兼容端点在第一阶段样本中返回分项 token；APIMart 的任务响应文档示例**只有金额**（`cost`/`credits_cost`），其分项 `usage` 是否在场**未实测**。**这决定证据形态是两条还是一条**——若 APIMart 也返回分项 token，则只需一条路径；若确实只有金额，则需要 [0012](./0012-provider-declared-charge-as-evidence.md)。
  > **事实更正（2026-09-19 同日稍后，实测结清）**：APIMart 的任务完成响应**含四分项 `usage`**（`input_tokens_details` 区分 text/image）。因此**证据形态是一条**，本 ADR 上面那句"未实测"及其两种分支**已作废**；[0012](./0012-provider-declared-charge-as-evidence.md) 随之继续保持**作废**（不是"待批准"）。依据：`docs/facts/channel-facts.md` §3.3（响应形状）、§3.9（计费与成本口径）、`out-reference/apimart/controlled-probe-2026-09-19.json`（原始样本）。
- 因此**第二阶段需要重新规划**（已完成，见工作项 #2 的规划正文），而新规划把「APIMart 的实际响应形状」列为必须由受控验证结清的事实。

## 与既有决策的关系

- 不推翻任何已有 ADR：`0001`–`0011` 与本次退役无关。
- [0012](./0012-provider-declared-charge-as-evidence.md)（Provider 声明的扣费金额作为**第二类**计量证据）：**已因上面的事实更正而作废**——两家渠道都返回四分项 token，不需要金额型证据。（原文写的是"是否仍需要取决于实测结果，因此保持待批准"；该状态已被事实更正取代。）
- 与 [`0004`](./0004-vendor-and-provider-identities-stay-separate.md) 一致：换模型只影响 Vendor Model 与 Offering/Channel/Price Plan，不合并 Vendor 与 Provider 身份。

**来源**：用户（项目运营方）于 2026-09-19 在第二阶段讨论中的明确决定。平台侧只读核对见 `docs/research/gpt-image-2.5-schema-research.md`（含 AIHubMix 两款的机器 Schema 对比、`quality` 位置随端点族变化、退役清单未含 `gpt-image-2` 的实测）。**APIMart 侧的核对不在那份文件里**（一个渠道的调研不夹带另一个渠道的结论，见 `docs/design/0004` R1）；它的模型供应、参数边界与计量事实见 `out-reference/apimart/apimart-image-api-research.md` 与 `docs/facts/channel-facts.md` §3。
