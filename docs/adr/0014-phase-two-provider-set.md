# 第二阶段的 Provider 集合：OpenAI 2.5 两款由 AIHubMix 与 APIMart 供应，直连 OpenAI 与火山方舟移出本阶段

**决策**（2026-09-19，第二阶段的规划范围）：

> **实施顺序说明（2026-09-19 追加，不改本决策）**：本 ADR 定的是**候选集合**这一决策，**不定实施顺序**。第二阶段的**规划层范围收口**（`#2` 规划 §7.5）决定**先落 AIHubMix**，APIMart **连同其 Driver 一并推迟**，原因是 `APIMART_API_KEY` 在三级环境中均不存在、其验证无法推进。当时由此推出一条约束：**在 APIMart 具备凭证并通过受控验证之前，不得开始其 Driver 实现**。
> **事实更正（2026-09-19 同日稍后）**：上述两条事实前提**都已不成立**——`APIMART_API_KEY` 在 User 与 Machine 级**都存在**（当时只看了进程环境，见 `docs/facts/channel-facts.md` §1.1）；APIMart Driver **已实现并完成受控验证**（文生图 + 参考图/遮罩，见同文件 §5.3/§5.6），两条分支已开放。因此那条"不得开始实现"的约束**已随事实消失**。
> **没有变的两条**（用户 2026-09-19 再次确认）：**直连 OpenAI** 与**火山方舟 / Seedream** 暂不做，先把这两家渠道跑通——与下面第 3、4 条一致。

1. **Vendor 固定为 `OpenAI`**，本阶段纳入两款 Vendor Model：`gpt-image-2.5-flare` 与 `gpt-image-2.5-sunburst`（退役 `gpt-image-2` 见 [0013](./0013-retire-gpt-image-2-use-2-5-models.md)）。
2. **首批候选 Provider 为两家：AIHubMix 与 APIMart**。两者在各自目录里都提供同名的上述两款模型，因此构成「**同一 Vendor Model 由两个不同 Provider 供应**」的样本——这正是工作项 [seeaihub-server-next#2](https://github.com/dehuadong/seeaihub-server-next/issues/2) 的核心命题。
   > **身份依据的更正（2026-09-20 收口）**：本项原先写"两者供应**同名**的模型，**因此**构成同一 Vendor Model"。**同名不是依据**——按 [0004](./0004-vendor-and-provider-identities-stay-separate.md) 与复审要求，Provider 不决定 Vendor，同名模型不自动合并。实际依据是**运营方的显式配置决定**：发布命令里声明 `vendor_id: OpenAI` 与同一个 `native_model_id`，两个 Offering 挂在同一 Vendor Model 下（`config/bootstrap/*.json`）。渠道侧提供不了这个证据——APIMart 的目录接口对**所有**模型都返回 `owned_by: custom`（含 `gemini-*`），**既不能证明也不能否证**归属（`docs/facts/channel-facts.md` §3）。因此本项是**运营决定 + 显式配置**，不是"由名称或渠道字段推导出的事实"。
3. **直连 OpenAI 不纳入首批**：它是**合法**的第二个 Provider（Vendor 与模型名相同，仅调用与账单来源不同，符合 [0004](./0004-vendor-and-provider-identities-stay-separate.md) 的「只新增 Offering/Channel/Price Plan」），但会引入第三组凭证、第三套计量与限额语义。本阶段的四项目标（同一 Vendor Model 多 Provider 路由、Provider 限制收窄、计价选择、安全降级）由两家已可完整验证。直连 OpenAI 保留为后续工作项，其价值是验证「厂商自营 vs 聚合」这一**供应形态差异**，而不是增加候选数量。
4. **火山方舟 / ByteDance Seedream 移出本阶段**：它是**另一个 Vendor 的另一个 Vendor Model**，与「同一 Vendor Model 多 Provider」正交。保留为「引入新 Vendor Model」的独立工作项；`docs/design/0003-doubao-ark-image-adapter.md` 随之转为该工作项的设计草案，**不废弃**。

## 为什么两款都纳入，但执行上分先后

`gpt-image-2.5-sunburst` 与 `-flare` 在 AIHubMix 的机器 Schema 中**端点集合与顶层参数完全一致**，差异集中在 `extra.quality`（`low|medium|high|xhigh|max|auto`）与 `extra.moderation`。因此「两个 Vendor Model × 两个 Provider」的候选矩阵边际成本很小；而真正的增量验证点是 **AIHubMix 的 `gpt-image-2.5-flare` 与 APIMart 的 `gpt-image-2.5-flare` 是同一 Vendor Model、不同 Provider**。执行顺序上先打通一款、再扩展到第二款，避免首期同时压两套真实计量。

## 与既有决策的关系

- **不推翻任何 ADR**：[0004](./0004-vendor-and-provider-identities-stay-separate.md) 要求 Vendor 与 Provider 身份在目录中分开固化，并预告「其他 Provider 也供应同一模型时只新增 Offering/Channel/Price Plan」——本决策正是该预告的落地。
- **[0009](./0009-multiple-active-offerings-and-routing.md) 的路由规则首次有了真实对象**：此前只能以「同一 Provider 的两个 SKU」充当多候选，那验证不了 Provider 侧差异；现在两个候选来自**不同 Provider**。
- **[0012](./0012-provider-declared-charge-as-evidence.md) 已作废**（2026-09-20 更正）：两家的计价维度已由受控实测结清——都返回四分项 token，不需要金额型证据。此前写"继续维持待批准"已过时。

## 未决的关键事实（决定本阶段范围大小）

> **状态：已结清（2026-09-20 更正）。** 本节起草时把"两家的计价维度口径不一致"列为实施期必须结清的受控验证项。实测结果：**两家都返回四分项 token `usage`**（AIHubMix 见 `docs/facts/channel-facts.md` §2.6；APIMart 见同文件 §3.3、§3.9）。因此上文"若两家都按 token 计量则 0010 的联合类型与 0012 在本阶段都不需要"这一分支**成立**：[0012](./0012-provider-declared-charge-as-evidence.md) **作废**，[0010](./0010-metering-evidence-is-unit-bearing.md) 的原则继续有效，**不需要**金额型证据。下面保留原文，仅作历史理由。

两家对同款模型的**计价维度口径不一致**，需要以受控验证结清（**不在本决策内**，且不得在未获批准时发起计费调用）：

- AIHubMix 的 2.5 摘要写 `Pricing: per-generation`；
- APIMart 的 2.5 文档写「按实际 **token** 用量计费」并给出含 `input_tokens`/`output_tokens`/`total_tokens` 的响应示例，而其**价格页按「张 × 分辨率档」**列 6 档（`flare@1K/2K/4K`、`sunburst@1K/2K/4K`）。

若两家都按 token 计量，则 [0010](./0010-metering-evidence-is-unit-bearing.md) 的联合类型与 [0012](./0012-provider-declared-charge-as-evidence.md) 在本阶段都不需要，范围显著缩小；若 APIMart 侧只有张数或只有金额，则两者都需要。**该事实以只读取证无法结清，属于实施期的显式受控验证项。**

**来源**：用户（项目运营方）于 2026-09-19 授权由本 Agent 确定第二阶段范围。只读实测证据见 `docs/research/gpt-image-2.5-schema-research.md`（AIHubMix 两款 2.5 的机器 Schema 对比、与 `gpt-image-2` 的 `quality` 差异）与 `out-reference/apimart/apimart-image-api-research.md`（APIMart 的供应声明、价格页 6 档、`quality` 边界与计价口径张力）。
