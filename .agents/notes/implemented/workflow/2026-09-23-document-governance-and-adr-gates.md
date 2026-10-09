---
title: 统一文档归属、Spec 合同与 ADR 准入
status: implemented
created: 2026-09-23
updated: 2026-09-23
approval: 用户同意统一文档归属、恢复独立 Spec，并收紧 ADR 准入与模板
verification: `node scripts/decisions/check.mjs`、五个受影响技能的 `quick_validate.py`、`git diff --check` 与本地链接检查通过
---

# Agent Note：统一文档归属、Spec 合同与 ADR 准入

## 问题

旧的工件注册表、领域文档和文档标准重复解释位置与职责，改一处容易漏另一处。旧注册表检查曾因 `rootDir` 指向仓库外且读取失败被吞掉而空跑；当时已用变异实测修复，但再维护一份位置表仍会造成重复。原 Spec 定义偏窄，安全策略、协议和运维不变量容易误入 ADR 或 RFC；ADR 的边界决策示例又削弱了难以逆转、真实权衡的准入条件。

## 决定

> 失效范围（2026-10-09）：本记录关于新工作固定使用独立 Spec/RFC/ADR、Note 只记录单次理由的归属规则，由[新归属决定](2026-10-09-work-items-and-notes-own-new-work.md)取代。其余历史理由、职责分离与验证保留；当前有效范围从[文档入口](../../../../docs/AGENTS.md#历史归属切换)读取。

- [`docs/AGENTS.md`](../../../../docs/AGENTS.md) 统一拥有文档分层、位置、职责与写法；根 [`AGENTS.md`](../../../../AGENTS.md) 只作约定入口。[`docs/agents/artifacts.md`](../../../../docs/agents/artifacts.md) 保留为旧链接的短兼容入口，不再维护第二份位置表。
- [`docs/agents/domain.md`](../../../../docs/agents/domain.md) 只管领域阅读、术语、冲突标注、ADR 准入与引用。生命周期、工作状态、授权和工具用法分别由 Agent Note 指南、工作项指南、根指令与工具文档拥有；本地文档引用可点回归属文档，编号注明类型，合同在使用处写全，理由只链接其归属文档。
- Spec 与 RFC 分文件：Spec 修订经评审接受后成为可验收的行为合同，涵盖标准、协议、安全、兼容及运维不变量；RFC 说明如何实现，并可拆成多份。合同修订只记变化和生效情况，不在 Spec 存决策史。
- ADR 只记录难以逆转、脱离上下文令人困惑且存在真实权衡的架构选择。`adr` 技能要求说明反悔成本、备选方案和选择理由；新 ADR 先为 `proposed`，获批准后才为 `accepted`。本次不改已有 ADR 文件。
- Agent Note 检查只负责 Agent Note 格式，不再检查兼容入口的文案；历史注册表检查的 `rootDir` 故障及变异实测保留在本记录与 Git 历史中，不声称该检查仍在运行。

## 备选方案

- **继续让注册表拥有位置规则**：曾经采用并修复过检查，但文档分层与位置仍需跨文件同步，因此被这次决定取代。
- **删除兼容入口**：会使旧链接失效，故保留短指针。
- **把完整位置表或领域目录树留在领域指南**：会再次混淆“放在哪”与“如何判断”，故不采用。
- **把 Spec 与 RFC 合为一个文件**：减少文件数，但合同修订与技术设计演进互相牵连，故仍分文件。

## 后果

- 新规则只需在所属文档修改；旧 `artifacts.md` 链接仍可解析，原注册表决定与检查记录可由 Git 历史追溯。
- 经接受的 Spec 修订是实现和验收依据；一个 Spec 可由多个 RFC 承接。机制或普通边界结论不因名字像“决策”就自动成为 ADR。
- 13 份现行历史 ADR 不在本次调整范围内；不安排迁移或降级。

## 验证

- `node scripts/decisions/check.mjs` 通过。
- 系统 `skill-creator` 的 `quick_validate.py <skill-dir>` 对 `planning`、`prd`、`implement`、`domain-modeling` 和 `adr` 通过。
- `git diff --check` 与本地 Markdown 相对链接检查通过；`docs/adr/` 无改动。
