# 工件管理

## 范围

本约定登记工程工件的**查找位置与边界**：放哪、不放什么。不要求生成全部工件，也不承载各工件自己的规则——生命周期、批准、写法分别由它们各自的约定拥有。已有登记保持归属，合并迁移需单独授权。

## 工件归属

> 变更、历史与状态**不写进本文件**：变更归提交历史与 Agent Notes，历史位置归下方「历史工件与新旧衔接」表。本表只回答"放哪里、边界在哪"。

| 工件或信息 | 位置与边界 |
| --- | --- |
| Proposal / 总体及阶段提案 | GitHub Issues，以仓库限定编号作标识；负责选定交付范围、规划状态与相关工件链接，配置与工作状态见 [`issue-tracker.md`](issue-tracker.md) |
| 既有工作项 / 实施 Ticket | 按 [`issue-tracker.md`](issue-tracker.md) 配置；已有工作项能明确范围与验收时直接复用，不另建 Proposal |
| Agent Notes / 工程变更与交付记录 | `.agents/notes/{生命周期}/{分类}/`；路径、生命周期、文件骨架与检查见 [`.agents/notes/README.md`](../../.agents/notes/README.md) |
| 领域词汇表 | 仓库根 `CONTEXT.md`：只放术语与定义；读取与命名约定见 [`domain.md`](domain.md) |
| 持久决定 / ADR | **决策的权威位置**：`docs/adr/`，按 `0001-slug.md` 编号；准入、退役与引用写法见 [`domain.md`](domain.md) |
| RFC 文件 / Spec 与技术设计 | `docs/design/`；Spec 与技术设计分节，复杂能力可拆为 RFC 集合；沿用 `NNNN-slug.md` 编号和既有 `主题` / `当前修订` / `状态` 头格式；**不混入评审记录** |
| 代码结构图 / 落点索引 | `docs/architecture.md`：**只做索引**——职责与规则归 `docs/design/0004-layered-architecture.md`，持久决定归 `docs/adr/`，冲突时以后两者为准；新增或移动文件、增删路由与表时在**同一变更**里同步 |
| 受控验证清单 | `docs/verification/`，按 `阶段-slug.md` 命名；**停止条件以来源规划为准**，清单只复述与执行；真实计费调用的留档固定在 `docs/verification/paid-provider-calls.md`（授权依据、次数、花费、样本位置），不写进渠道事实台账 |
| 调查与探索存档 | `docs/research/`：结论 + 事实/推论/待确认分开 + 来源；已归纳定稿的渠道事实放 `docs/facts/`；结论被推翻时保留更正记录，不静默改写 |
| 汇总事实登记 | `docs/facts/`：**只放结论**，引用而不复述；含凭证类内容时只记**变量名**；原始形状、逐字样本与调用流水分别归 `out-reference/<provider>/` 与 `docs/verification/paid-provider-calls.md` |
| Agent 技能 | `.agents/skills/`；由仓库根 `AGENTS.md` 的阶段交接点名调用；产品与运行时合同归 `docs/` 或源码 |
| 外部参考资源 | `out-reference/`，**只作参考，不是工程工件**；入库范围见下 |
| 受控采集脚本 | `scripts/probe/response-shapes.ps1`：**唯一**会发真实计费调用的入口（开关与凭证读法见脚本头部注释）；脱敏结果落 `out-reference/<provider>/`，登记补进该渠道的 `response-shapes.md` |

文件有真实内容时才创建。Spec 是内容，不是单独工件；需要在仓库内长期保存时由 RFC 文件承载，小改动可以留在工作项。同一决定只保留一个权威属主：已写入 ADR 的决定，Agent Notes 只引用并链接，不另写一份决策正文。RFC 文件与 ADR 不随变更记录移动——它们的审阅、批准与替代约定写在各自文件里，记录只引用它们。

## 历史工件与新旧衔接

| 历史位置 / 适用工作 | 当前权威性 | 新工作位置 | 已有工作更新方式 | 迁移状态与依据 |
| --- | --- | --- | --- | --- |
| `docs/design/` 各文件的 `当前修订` / `状态` 头 | 仍有效 | 与 Agent Notes 的 `proposed / implemented / rejected` 并存，映射方式见下 | 原地更新 | 未迁移；不新增竞争状态字段 |
| `docs/design/0006-gateway-models-pricing-and-admin-console.md` | 文件名已停用，内容已迁走；三份切片设计为权威位置 | 网关模型与对客面归 `docs/design/0006-gateway-models-and-consumer-surface.md`；定价、保底与结算归 `docs/design/0007-pricing-floor-and-settlement.md`；路由策略与缓存归 `docs/design/0008-routing-strategy-and-caching.md` | 新工作更新对应切片的那一份，不回到旧文件 | 2026-09-22 按用户授权按切片拆分；过程记录（评审记录与逐条处置）与实施清单移到本地暂存 `.data/design-0006-review-log.md`、`.data/design-0006-slice-tickets.md`（`.data/` 不入版本控制，属本地工作副本） |

RFC 状态头（`主题` / `当前修订` / `状态`）表示设计评审状态，不表示交付生命周期，交付证据在 Agent Notes 的 `implemented/` 记录中引用。设计中的持久决策已抽到 `docs/adr/`，设计文档与 ADR 互相引用而不复制决策正文。

优先按用户给定工件和已有工作关联定位，再查适用的新旧位置。已有工作默认更新原属主，不因注册位置变化生成第二份合同。新主题进入新位置；更新注册表不代表文件已迁移。

## 外部参考资源

位置：仓库根目录 `out-reference/`，按 Provider 分目录，存放第一方协议调研、上游 Schema 快照、错误码表与官方示例代码。它只作背景证据，不是工程工件，也不承载平台合同；**怎么用**（不得作为运行时数据源、进合同前走哪条更新流程）由 [`domain.md`](domain.md) 与 [`docs/design/0004`](../../docs/design/0004-layered-architecture.md) 拥有。

**入库范围**：调研笔记、Schema 快照、错误码表与计量证据样本入版本控制，使 `docs/adr/`、`docs/design/` 中的引用在 clone 后可解析；可运行的第三方示例代码排除（`.gitignore` 忽略 `out-reference/doubao/touch_edit_demo/`）。因此 clone 后缺该目录属预期行为，需要时回上游来源重新获取。

## 维护

重新 setup 默认检查和补齐配置，已有差异保留并报告。明确选择新位置时更新本注册表与项目指令；只有授权迁移时才移动历史工件、转换状态并修复链接。注册不新增执行权限。
