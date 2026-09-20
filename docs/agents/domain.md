# Domain Docs

工程技能在探索本仓库代码库时应如何消费领域文档。

## Before exploring, read these

用项目指令引用的工件注册表（`docs/agents/artifacts.md`）确认当前与历史的决定属主。ADR 路径是本仓库已登记的决策位置；当具体决定登记在别处时，读取该处，不要另建竞争性的 ADR 目录。

从项目指令和既有注册确定的管理根目录（本仓库为仓库根目录）解析注册表与根路径，而不是当前子项目工作目录。多个上下文共用该注册表和根级 `.agents/notes/`；历史属主保持已登记状态。上下文映射只负责选择领域文档，不划分工程注册或记录分类。普通 Markdown 链接仍相对所在文件解析。

- **`CONTEXT.md`** — 仓库根目录，本仓库唯一的领域词汇表。
- **`CONTEXT-MAP.md`** — 本仓库不存在，即单上下文；出现该文件时它才指向各上下文的 `CONTEXT.md`，需读取与主题相关的每一个。
- **`docs/design/`** — 读取与即将工作的区域相关的独立技术设计 RFC（`NNNN-slug.md`）。这是技术设计的权威位置：`0001-image-generation.md` 登记实现映射，`0002-image-generation-tech-design.md` 是技术设计 v5 的权威副本。
- **`docs/adr/`** — 决策的权威位置，按 `0001-slug.md` 顺序编号，读取与即将工作的区域相关的 ADR；当前有 `0001`–`0017`，其中 6 篇是退役/合并**存根**（不算决策，见下「持久决定的准入与引用」）。

这些文件不存在时静默继续：不要标记缺失，也不要主动建议创建。`domain-modeling` 技能会在术语或决定真正敲定时惰性创建它们。

`out-reference/` 是外部参考资源（第一方协议调研、上游 Schema 快照、错误码与示例代码），不是工程工件，也不承载平台合同：探索时只作背景证据读取，运行中的服务只使用经审核发布的 Runtime Revision，不跟随该目录内容变化。登记见 `docs/agents/artifacts.md` 的「外部参考资源」。

## File structure

单上下文仓库：

```
/
├── CONTEXT.md
├── docs/
│   ├── agents/                ← 本技能的配置
│   ├── design/                ← 独立技术设计 RFC（权威位置）
│   │   ├── 0001-image-generation.md
│   │   └── 0002-image-generation-tech-design.md
│   └── adr/                   ← 决策（权威位置）
├── out-reference/             ← 外部参考资源（不是工程工件）
├── apps/
│   ├── api/
│   └── worker/
└── crates/
    ├── domain/
    ├── application/
    ├── persistence/
    ├── object-storage/
    ├── adapter-sdk/
    └── adapter-aihubmix/
```

本仓库是单上下文：没有 `CONTEXT-MAP.md`，且仓库根 `CONTEXT.md` 是唯一的词汇表，被 `apps/api` 与 `apps/worker` 共同使用。库分层（`crates/domain`、`crates/application`、基础设施 crate）是实现分层，不是领域边界。

若将来出现真正独立的领域边界，再在仓库根加入 `CONTEXT-MAP.md` 指向各上下文的 `CONTEXT.md`；注册表与 Agent Notes 安装位置保持在仓库根不变。

## Use the glossary's vocabulary

当输出命名某个领域概念时（issue 标题、重构提案、假设、测试名），使用 `CONTEXT.md` 中定义的说法；不要漂移到该文件明确标注 _Avoid_ 的同义词——例如用 **Vendor** 而不是「Provider」或「渠道」，用 **Vendor Model** 而不是「平台模型」，用 **Provider** 而不是「Vendor」，用 **Offering** 而不是「模型」或「渠道」，用 **Generation Job** 而不是「Provider Task」，用 **Metering Evidence** 而不是「费用」，用 **Reconciliation Case** 而不是「普通失败」。

若需要的概念还不在词汇表里，这是一个信号：要么你在发明项目不用的语言（重新考虑），要么存在真实的词汇缺口（记为缺口，交给 `domain-modeling`）。

## Flag conflicts

当输出与已交付的决定冲突时，显式指出而不是静默覆盖，并引用具体文档：

> _与 `docs/design/0001-image-generation.md` 的「不自动重提，创建 Reconciliation Case」冲突——但值得重开，因为……_

决策落位规则：决策的权威位置是 `docs/adr/`，ADR 拥有决策正文。`.agents/notes/` 记录工程变更、交付理由与验证（见 `docs/agents/artifacts.md`），引用相关 ADR 而不复制决策正文。同一决定只保留一个权威属主。

## 持久决定的准入与引用

`docs/adr/` **只收三条判据都成立的决定**：① **难反悔**；② **不看记录会奇怪**；③ **真权衡过**（有像样的备选，当时为具体理由选了一个）。

以下内容不进 ADR，各有去处：改数据或配置就能回去的东西（在售型号与目录状态、价格取值、路由优先级取值）→ 发布物；某阶段的实施范围与先后 → Proposal / 工作项；被实测回答掉的候选问题 → `docs/facts/` 或 `.agents/notes/rejected/`；当前实现状态与待办缺口 → Agent Notes 或工作项。

**退役与合并**：条目退役或并入他条时，**降级为一句话存根**（`status: deprecated`，写明为何不是决定、内容去了哪），**文件名与编号保留**以便旧链接可解析；**编号不复用**。存根不算决策。

**引用写法**：ADR 写 `ADR-0016`（或文件路径），工作项写 `issue #6`——本仓库的 ADR 编号与 Issue 编号会撞号（同一个"6"既可能是 ADR 也可能是工作项），混写必然产生歧义。
