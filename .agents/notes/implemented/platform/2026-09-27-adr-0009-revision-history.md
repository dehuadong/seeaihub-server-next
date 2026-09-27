---
title: ADR-0009 的修订史：选中顺序改归路由策略，预授权降为保底
status: implemented
created: 2026-09-27
updated: 2026-09-27
approval: 用户 2026-09-27 授权整理 ADR-0009 正文：现行条款留在 ADR，被取代的结论与依据搬进本记录
verification: `node scripts/decisions/check.mjs` 通过；引用的提交 `54e25de`、`1a0d688`、`a031cd6` 与迁移 `0009`／`0010` 已逐条用 `git show --stat` 与本机文件核对
---

# Agent Note：ADR-0009 的修订史：选中顺序改归路由策略，预授权降为保底

## 问题

[`ADR-0009`](../../../../docs/adr/0009-multiple-active-offerings-and-routing.md) 的正文里积了三段修订叙述（2026-09-20 部分被取代、2026-09-22 预授权口径、2026-09-24 成本护栏边界）与一段批准缺口说明。按 [`docs/AGENTS.md`](../../../../docs/AGENTS.md) 的写作规则，正文写当前仍然生效的内容，变更过程归提交与 Agent Note；这些叙述也让读者得自己判断哪半句还生效。搬进本记录之后，ADR 只剩现行条款。

## 决定

ADR-0009 正文重写为现行条款，被取代的结论搬进本记录：

- **"选中顺序完全由发布决定"这半条由 [`ADR-0020`](../../../../docs/adr/0020-routing-strategy-layer-configured-by-operations.md) 取代**（提交 `54e25de`）：在一批合格候选里挑哪一条改由运营配置的路由策略决定，未配置时默认 `priority_failover`。旧表述里"`routing_priority` 数字小者优先**且同一型号内唯一**"的后半不再成立——迁移 `0010` 把唯一索引从 `(gateway_model, routing_priority)` 换成 `(gateway_model, offering_id)`，同一档位内可以有多个候选、按 `weight` 分流。仍然成立的是候选集来自同一次发布、合格是合取判据、不合格在调用上游前失败、`submitting` 后禁止改选。
- **预授权由"调用方给的 `max_cost_microusd`"改为"按供给维度的保底表查得"**（提交 `1a0d688`，迁移 `0009`）：旧条款是"预授权金额与计价口径是两个量，不得互相推导：预授权由调用方给出的 `max_cost_microusd` 决定……被选中候选的最小可能费用已超上限时在受理前拒绝"。它不成立的原因是**被判的那个量在受理时算不准**——按 token 计费的渠道受理时不知道真实用量，据此拒绝会把正常请求挡掉。改为保底额后，估小了由结算透支吸收、估大了释放差额，硬拒绝只剩"余额 < 保底额"（402 `insufficient_balance`）。
- **成本护栏不是"受理上限"**（提交 `a031cd6`）：删掉的是"拿调用方给的 `max_cost_microusd` 与被选中候选的最小可能费用比"这条规则；后加的单次请求成本上限判的是**发布物与折算率给出的单次成本上界**，与客户余额无关，超限对客是 503 平台侧故障。
- **批准状态没有变化**：本条的规划评审通过记录不在本仓库、不可复核，2026-09-20 核对时登记为"批准待用户事后确认"；本次只改写正文，不据此推定已获批准。

## 备选方案

- **把修订叙述继续留在 ADR 正文就地标注（沿用 2026-09-20／22／24 的做法）**：落选。正文会继续积累已经失效的结论，读者要自己判断哪半句还生效——这正是 `docs/AGENTS.md` slop 清单里"历史写进了不该写历史的层"。
- **把这三段并进同主题的既有记录**（[`2026-09-22-routing-weight-and-decisions`](./2026-09-22-routing-weight-and-decisions.md)、[`2026-09-22-pricing-floor-and-settlement`](./2026-09-22-pricing-floor-and-settlement.md)、[`2026-09-24-request-cost-ceiling`](./2026-09-24-request-cost-ceiling.md)）：落选。那三条各自记录一次变更的理由与验证，把"ADR 正文积压的修订叙述"塞进去会让它们的 `## 决定` 变成别人的历史；本条只做一件事，并与它们互相链接。

## 后果

- ADR-0009 正文只留现行条款；被取代的旧结论、失效原因与依据提交都在本记录里。
- 三条既有记录里指向"ADR-0009 就地标注的修订段"的句子已改为指向本记录。
- 本记录不复制设计正文：保底表口径见 [`docs/design/0007`](../../../../docs/design/0007-pricing-floor-and-settlement.md) §6，成本护栏见 [`docs/design/0009`](../../../../docs/design/0009-operational-baseline.md) §7，策略层见 [`docs/design/0008`](../../../../docs/design/0008-routing-strategy-and-caching.md) §6。

## 验证

- `node scripts/decisions/check.mjs` 通过（本次改动只有 `docs/adr/` 与 `.agents/notes/`，未动代码，因此没有重跑 crate 测试）。
- 逐条引用的提交用 `git show --stat` 核对：`54e25de`（ADR-0020 落地与 0009 的标注）、`1a0d688`（ADR-0006／0009 就地修订）、`a031cd6`（成本护栏与 ADR-0009 的边界句）；迁移事实用 `migrations/0009_pricing_floor_and_settlement.sql` 与 `migrations/0010_routing_weight_and_decisions.sql` 核对。
