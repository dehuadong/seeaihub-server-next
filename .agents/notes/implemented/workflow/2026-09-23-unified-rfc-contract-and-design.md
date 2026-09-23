---
title: RFC 文件合并 Spec 与技术设计
status: implemented
created: 2026-09-23
updated: 2026-09-23
approval: 用户确认 Spec 与 RFC 合并、RFC 可以拆成多份，并要求执行及同步关联文档
verification: 技能结构、Agent Note、内容与文件边界断言、相对链接、历史 RFC 无差异及 diff 格式检查通过；Implementation Review 两轴 PASS
---

# Agent Note：RFC 文件合并 Spec 与技术设计

## 问题

仓库一直没有独立 Spec 文件，但产品范围与验收这些 Spec 内容始终存在，通常写在 Proposal 或 RFC 中。再增加一类文件会扩大文档数量和交叉引用，也不符合本仓库现状。PRD 属于用户按需选择的产品定义活动，不应成为每次 Planning 的自动分支；仓库工件也不能依赖外部工作系统才能读懂。现有 Agent Note 检查已经覆盖本仓库需要，复制上游脚本只会形成第二套维护对象。

## 决定

- Spec 作为产品合同内容保留，不创建独立 Spec 文件；RFC 文件分节承载 Spec 与技术设计。
- 复杂能力可以拆成 RFC 集合：牵头 RFC 的 Spec 管共享范围、共同验收与切片地图，子 RFC 的 Spec 管各自切片；每条验收只有一个属主。
- 没有共享 Spec 的切片不强制创建牵头 RFC；没有 RFC 文件的小改动由工作项直接承载 Spec 与必要技术决定。
- PRD 仍由用户显式调用，只作为产品背景；ADR 仍只记录需要长期保留的重要决定。
- 新增或实质改写的仓库工件不反向引用 GitHub Issue 或 PR；既有历史引用不专项清理。
- PRD 的显式调用继续由 `agents/openai.yaml` 的 `policy.allow_implicit_invocation: false` 保证。
- Agent Note 继续使用 `proposed`、`implemented`、`rejected` 三种职责和现有检查脚本，不增加归档目录或复制上游脚本。
- 既有 RFC 不迁移、不因本次规则调整而专项改写。

## 备选方案

- **独立维护 Spec 文件**：不采纳，会增加文件与同步成本，且与既有仓库实践不符。
- **全部放进 Proposal**：不采纳，本地 RFC 会失去完整合同，阅读技术设计仍需跳到工作系统。
- **所有能力只允许一份 RFC**：不采纳，复杂能力需要按可独立评审和实施的切片拆分。
- **Planning 自动调用 PRD**：不采纳，会把按需的产品定义变成所有工程规划的固定前置。
- **复制上游检查脚本或增加归档目录**：不采纳，现有检查与三种职责已经够用，暂时没有重复维护或归档的必要。

## 后果

- Planning、Proposal、PRD、工程流程、工件注册表和 Issue 约定使用同一模型：Spec 是内容，RFC 是承载它与技术设计的文件，不登记 `docs/specs/`。
- 牵头 RFC 的 Spec 只保留共享合同与切片地图；子 RFC 的验收不得重复归属。
- 旧 RFC 结构可以保持原样；新规则只约束新增或实质改写的内容。
- PRD 显式调用、仓库工件单向引用和 Agent Note 三种职责保持有效。

## 验证

- 系统技能校验以 UTF-8 模式检查 Planning 与 PRD，结果均为 `Skill is valid!`。
- `node scripts/decisions/check.mjs` 通过，共检查 22 条记录。
- 变更 Markdown 的相对链接全部可解析；`docs/specs/` 不存在，Planning 保留 `spec.md` 说明 Spec 内容边界，现行规则不创建独立 Spec 文件。
- `docs/design/` 没有差异，旧记录没有入站引用；`git diff --check` 通过。
- Implementation Review 的 Standards 与 Spec 两轴均 PASS。
