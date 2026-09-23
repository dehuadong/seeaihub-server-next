---
title: Spec 与 RFC 往返收敛，PRD 按需独立调用
status: implemented
created: 2026-09-22
updated: 2026-09-23
approval: 用户先确认 Spec 与 RFC 往返收敛方案，后因合并文件仍有文档治理问题，明确要求恢复 Spec 与 RFC 分离并执行
verification: Planning 与 PRD 技能结构、Agent Note、相对链接和 Spec/RFC 分离断言通过；git diff --check 通过；Implementation Review 两轴 PASS
---

# Agent Note：Spec 与 RFC 往返收敛，PRD 按需独立调用

## 问题

Planning 原先把目标、范围与验收主要交给 Proposal 或工作项，Spec 只是可选工件，PRD 又与 Spec 同时承担范围、非目标和验收，导致产品合同没有稳定属主。RFC 虽然负责技术设计，却没有明确区分“用于反馈产品范围的探索草案”和“可以指导实施的已接受设计”。仓库对 Spec 的存放位置也存在冲突：工件注册表登记 `docs/specs/`，Issue 配置却称 Spec 位于 GitHub Issues。

曾尝试把 Spec 内容合并进 RFC 文件以减少文件数量，但产品合同与技术设计共用文件后，属主、拆分、引用和生命周期仍然互相牵连。用户据此决定恢复独立 Spec 与 RFC 文件，由引用关系连接两者。

## 决定

- Planning 以 Proposal 或既有工作项作为本次交付的根节点，以 Spec 或直接承载产品合同的小型工作项拥有产品范围与验收，以 RFC 拥有技术设计；存在独立 RFC 时必须有 Spec。
- Spec 与非权威 RFC 探索草案可以往返收敛。技术可行性、成本、性能、兼容性和安全约束反馈给产品合同属主；产品合同稳定后，RFC 才能接受并成为实施依据。
- 一份 Spec 可以由多份 RFC 承接。每份 RFC 只引用适用的 Spec 修订与章节。
- PRD 是默认禁止隐式调用的独立技能，负责用户问题、产品目标、价值与产品级边界，不自动创建 Proposal、Spec 或 RFC。
- Proposal 负责选定交付范围、规划状态、批准证据和工件链接；存在独立 Spec 时不复制详细产品范围与验收。
- 独立 Spec 位于 `docs/specs/`，GitHub Issue 只保存工作状态、批准证据与工件链接；独立 RFC 位于 `docs/design/`。
- 新增或实质改写的本地工件不反向引用 GitHub Issue 或 PR；既有历史引用不做专项清洗。
- Agent Note 以 `proposed`、`implemented`、`rejected` 三种职责组织；暂不增加 `archived/`。现有检查脚本已经覆盖上游格式检查的核心能力，因此保留并精简入口说明，不另复制一套脚本。
- 显式技能使用 `agents/openai.yaml` 的 `policy.allow_implicit_invocation: false`。[`SKILL-MECHANICS.md`](../../../skills/writing-for-agents/SKILL-MECHANICS.md) 的旧前置字段说明与当前校验器不兼容，现与实际机制一致。

## 备选方案

- **严格要求先完成 Spec，再允许创建 RFC**：不采纳。上游能力、成本和兼容性等事实可能决定产品边界，禁止前置探索会迫使产品合同在没有可行性证据时定稿。
- **先接受 RFC，再从技术方案反推 Spec**：不采纳。这样会让技术方案替产品决定范围与验收，形成解决方案先行的倒置依赖。
- **继续把 PRD 留在 Planning 内自动使用**：不采纳。PRD 是用户按需选择的产品定义活动，不应成为每次工程规划的自动分支；其范围也与 Spec 重叠。
- **把 Spec 内容合并进 RFC 文件**：不采纳。文件数量减少，但产品合同与技术设计的属主、拆分和生命周期仍然耦合，增加后续治理成本。

## 后果

- [`planning`](../../../skills/planning/SKILL.md) 可以在 Spec 稳定前开展 RFC 可行性探索，但探索结果不能直接成为实施依据。
- [`spec.md`](../../../skills/planning/spec.md)、[`rfc.md`](../../../skills/planning/rfc.md) 与 [`review.md`](../../../skills/planning/review.md) 分别拥有产品合同、技术设计和评审规则。
- 用户需要产品方向文档时显式调用 [`prd`](../../../skills/prd/SKILL.md)；普通 Planning 不自动创建 PRD。
- GitHub 工作项单向链接本地工件；既有历史引用保持原样。
- Agent Note 只保留三种生命周期。现有检查脚本继续使用，不新增归档目录。

## 验证

- Planning 与 PRD 分别通过技能结构校验。
- `node scripts/decisions/check.mjs` 通过，共检查 22 条记录。
- 变更 Markdown 的本地链接、文件结构、PRD 显式调用策略和历史文档无差异断言通过。
- `git diff --check` 通过；输出仅有 Git 的 LF/CRLF 工作区提示。
- Implementation Review 的 Standards 与 Spec 两轴均 PASS。
- `git show 39f34f0` 确认回退只反转合并提交的 16 个治理文件，并保留其后的主分支提交。
