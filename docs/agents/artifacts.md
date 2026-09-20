# 工件管理

## 生效范围

本约定管理工程工件的查找、创建、更新和生命周期，不要求生成全部工件。日常文档治理直接处理并检查；实质改变工程合同的修改按工程流程处理。

注册模式：**保留现有**。
生效日期与依据：2026-09-19，用户在 setup 评审中确认保留现有工件位置，并纠正 `docs/design/` 为独立技术设计 RFC 的权威位置（不使用 `docs/rfcs/`）。
仓库管理根目录：`E:\workspace\seeaihub-server-next`（Git 顶层，依据为 `docs/design/0001-image-generation.md` 声明本仓库是该服务端的实现映射）。本注册表是该范围的统一入口，表中位置均相对此根目录解析，不随子项目工作目录改变。普通 Markdown 链接仍相对所在文件解析。

本仓库为单上下文：根目录 `CONTEXT.md` 是唯一词汇表，不存在 `CONTEXT-MAP.md`。多上下文只分发领域资料；根目录 `CONTEXT-MAP.md` 可指向各项目的 `CONTEXT.md`，本注册表可登记各上下文的工件位置。新建 Agent Notes 统一部署在根目录 `.agents/notes/`，子项目不另建注册表或记录系统。既有分散位置保留其登记归属，合并迁移需单独授权。

## 新工件归属

| 工件或信息 | 位置与规则 |
| --- | --- |
| Proposal / 总体及阶段提案 | 按 `docs/agents/issue-tracker.md` 配置的 GitHub Issues 管理，以仓库限定编号作标识；作为该规划工作的主入口，引用独立需求与设计 |
| Agent Notes / 工程变更与交付记录 | `.agents/notes/proposed/<分类>/YYYY-MM-DD-主题.md`，交付并验证后移至 `implemented/`，否决时移至 `rejected/`；记录工程变更、交付理由及其验证，可引用 Proposal 与 ADR，不复制提案正文与决策正文 |
| 独立技术设计 RFC | `docs/design/`；需要独立评审、复用或演进时拆出，由提案引用，是技术设计的权威位置。沿用 `NNNN-slug.md` 顺序编号和既有 `主题` / `当前修订` / `状态` 头格式。第二阶段不另立 RFC，其技术设计与验收条件由工作项 `#2` 的规划正文承载 |
| 代码结构图 / 落点索引 | `docs/architecture.md`；回答"哪个 crate、文件、表负责什么"。它**只做索引**：分层的职责与规则归 `docs/design/0004-layered-architecture.md`，持久决定归 `docs/adr/`，冲突时以后两者为准。新增或移动文件、增删路由与表时在**同一变更**里同步 |
| 调查与探索存档 | `docs/research/`；存放**本仓库 Agent 自己做的**调查与探索记录（结论 + 事实/推论/待确认 + 来源），写法与边界见其 `README.md`。**上游或第三方的原始材料仍放 `out-reference/`**（那里只作证据、服务不读取）；已归纳定稿的渠道事实放 `docs/facts/`。结论被推翻时保留"更正记录"，不静默改写 |
| 独立行为合同 Spec | `docs/specs/`；明确需要时创建，优先更新同一工作已有 Spec |
| 受控验证清单 | `docs/verification/`，按 `阶段-slug.md` 命名；记录受控验证的步骤、停止条件与留档要求。**停止条件以来源规划为准**，清单只复述与执行 |
| 汇总事实登记 | `docs/facts/`；把散在原始证据里的渠道事实归纳成单一出处，引用而不复述。含凭证类内容时只记**变量名** |
| 持久决定 / ADR | **决策的权威位置**：`docs/adr/`，按 `0001-slug.md` 顺序编号，ADR 拥有决策正文。变更记录、RFC、Proposal 只引用 ADR，不复制决策正文 |
| 既有工作项 / 阶段进度 | 按 `docs/agents/issue-tracker.md` 配置；既有工作项能明确本次范围、验收及决定时可直接复用，不另建 Proposal |
| 实施 Ticket | 按跟踪器配置存储与跟踪，关联所属工作项 |
| 其他已有工件 | 承接总览与未来进度归 `dehuadong/seeaihub-server-next#1`；上游 `dehuadong/seeaihub#674` 只作冻结的历史来源，不再承接新提案或进度 |
| 外部参考资源 | `out-reference/`，按 Provider 分目录（`aihubmix/`、`apimart/`、`doubao/`、`openai/`、`openrouter/`）。**只作参考，不是工程工件**，定位见下文「外部参考资源」一节 |
| 决策记录工具 | `scripts/decisions/{lib,update-index,check}.mjs`，随决策记录系统部署的项目副本。已相对 setup 技能自带的原始版本修补链接解析（见下文「决策记录工具」） |
| 受控采集脚本 | `scripts/probe/response-shapes.ps1`：**唯一**会发真实计费调用的入口（默认演练，必须显式加 `-ConfirmPaidCalls`）。用途是把各渠道的真实响应结构落成脱敏文件到 `out-reference/<provider>/`，并把登记补进该渠道的 `response-shapes.md`。凭证只从环境变量读 |

文件有真实内容时才创建。Agent Notes 分类读取 `.agents/notes/config.json`，当前分类为 `domain`、`platform`、`workflow`，分别覆盖领域语义与状态规则、平台基础设施与运行时保证、仓库级工程约定与 CI 工具。Agent Notes 的正文格式参考 `.agents/notes/templates/record.md`。

工件职责边界：`docs/design/` 是独立技术设计 RFC 的权威位置，拥有设计正文；`docs/adr/` 是决策的权威位置，拥有决策正文；`.agents/notes/` 承载工程变更、交付理由及其验证的记录，引用而不复制决策正文。同一决定只保留一个权威属主：已写入 ADR 的决定，Agent Notes 只引用并链接。

## 历史工件与新旧衔接

| 历史位置 / 适用工作 | 当前权威性 | 新工作位置 | 已有工作更新方式 | 迁移状态与依据 |
| --- | --- | --- | --- | --- |
| `docs/design/0001-image-generation.md` | 仍有效，原先兼任实现映射与设计决定的载体 | 实现映射留在原文件；设计正文进 `docs/design/0002-image-generation-tech-design.md`；持久决定进 `docs/adr/0001`–`0008` | 原地更新正文与指针，保留 `主题` / `当前修订` / `状态` 头 | 已按用户授权移动归类；原文件仍为实现映射属主 |
| `docs/design/0002-image-generation-tech-design.md` | 仍有效，技术设计 v5 在本仓库的权威副本 | 设计修订继续在此文件演进，同步更新 `当前修订` 头 | 原地更新 | 2026-09-19 新落盘；内容取自上游 comment 5732545152 并合并 v3/v4 中继续有效的规则 |
| 上游总体提案 `dehuadong/seeaihub#674` | 冻结的历史产品与架构来源；v1–v5 逐版设计全文归上游仓库所有，不再承担本仓库未来提案或进度 | 承接总览为 `dehuadong/seeaihub-server-next#1`；不复制旧 issue 正文 | 上游保持不动；本仓库后续状态、评审和工作项只更新新跟踪器 | 已完成权威接管；历史来源不物理迁移 |
| 上游技术设计 v5 评论（comment 5732545152） | 仍有效，作为来源留档 | 本仓库权威副本已落盘为 `docs/design/0002-image-generation-tech-design.md` | 上游原地保留，不修改 | 已完成落盘；**这是复制而非移动**，上游原文仍在 |
| `docs/design/` 各文件的 `当前修订` / `状态` 头 | 仍有效 | 与 Agent Notes 的 `proposed / implemented / rejected` 并存，映射方式见下 | 原地更新 | 未迁移；不新增竞争状态字段 |

一个文件兼任提案与记录时，沿用其登记归属与状态映射，分别识别工作进展、批准与交付生命周期。设计文档同时承担「历史提案在本仓库的落地依据」与「已评审通过的技术设计」两种角色：#674 只提供历史批准与设计来源，本仓库工作进展、后续评审和批准依据由 `seeaihub-server-next#1` 及其后续工作项维护，不复制旧提案正文；RFC 状态头（`主题` / `当前修订` / `状态`）表示设计评审状态，不表示交付生命周期，交付证据在 Agent Notes 的 `implemented/` 记录中引用。设计中的持久决策已抽到 `docs/adr/`，设计文档与 ADR 互相引用而不复制决策正文。

优先按用户给定工件和已有工作关联定位，再查适用的新旧位置。已有工作默认更新原属主，不因注册位置变化生成第二份合同。新主题进入新位置；更新注册表不代表文件已迁移。

## 生命周期与批准

变更记录遵循 `.agents/notes/README.md`：`proposed` 表示尚未或部分交付，`implemented` 表示所记范围已交付并验证，`rejected` 表示否决。批准依据和执行授权分别遵循项目约定（见仓库根 `AGENTS.md`）。

Proposal 的工作状态由 `docs/agents/issue-tracker.md` 拥有的 GitHub 标签表示；Plan Review 结果写回本次 Proposal 或既有工作项，取得所需批准后才能更新准备实施状态。记录目录及状态不替代评审、批准或执行授权。

独立 RFC、Spec、ADR 不随变更记录移动；在此登记它们已有的审阅、批准和替代约定，未规定时不从文件生成推断批准。持久决定的批准依据在 `docs/adr/` 的 ADR 中记录，Agent Notes 只关联涉及本决定的评审与批准证据，不复制决策正文。Issue / Ticket 状态由跟踪器配置管理。

总体提案链接阶段工作项，各工程单元按需运行工程循环。阶段完成还需集成与阶段验收；覆盖总体范围的变更记录在全部交付前保持 proposed，提案进度按跟踪器约定维护。

## 外部参考资源

位置：仓库根目录 `out-reference/`，按 Provider 分目录（`aihubmix/`、`apimart/`、`doubao/`、`openai/`、`openrouter/`），存放第一方协议调研、上游 Schema 快照、错误码表与官方示例代码。

目录名表示「外部参考资源」：材料来自本仓库之外的上游或第三方，**不是工程工件，也不表达平台的对外接口合同**，只作证据使用（见仓库根 `AGENTS.md` 的「工程边界」）。顶层只放按 Provider 命名的目录，不放平铺文件；汇总登记类不放这里（见 `docs/facts/`）。

使用规则：

- 引用与追踪用代码格式的根路径措辞，不作为构件依赖，也不从代码或配置里读取；
- **不得**作为运行时数据源：运行中的服务只使用经审核发布的 Runtime Revision，不跟随远端文档或本目录内容变化；
- 进入平台合同前必须走「抓取候选 → 差异检查 → 审核 → 发布新 Runtime Revision」流程，平台合同以发布后的不可变修订为准；
- 新增或更新资料时按来源与证据等级标注（事实 / 推论 / 待确认分开），保留来源 URL、抓取时间与版本信息；
- 第三方示例代码不参与本仓库构建，不纳入 `cargo fmt` / `clippy` / `test` 的验证范围；
- **凭证纪律**：只保存脱敏后的记录，真实密钥、Bearer token、task ID 与短期 URL 一律不入库（见仓库根 `AGENTS.md` 的「事实与安全」）。

**入库范围**：调研笔记、Schema 快照、错误码表与计量证据样本入版本控制，使 `docs/adr/`、`docs/design/` 中的引用在 clone 后可解析；可运行的第三方示例代码排除（`.gitignore` 忽略 `out-reference/doubao/touch_edit_demo/`）。因此 clone 后缺该目录属预期行为，需要时回上游来源重新获取。

## 决策记录工具

`scripts/decisions/lib.mjs` 是本地修补过的项目副本：`resolveTarget()` 用显式栈逐段解析相对链接，不使用 `path.resolve` / `path.normalize` / `path.join`——原版在本机 Node v24.10.0（Windows）上会把正确的跨目录相对链接误报为断链。

升级 setup 技能自带的 bundle 时：先逐文件比对，保留本处定制，不要用原始 `lib.mjs` 覆盖；`update-index.mjs`、`check.mjs` 未修改。

## 维护

重新 setup 默认检查和补齐配置，已有差异保留并报告。明确选择新位置时更新本注册表与项目指令；只有授权迁移时才移动历史工件、转换状态并修复链接。注册不新增执行权限。
