# Issue tracker: GitHub

本仓库的 Proposal 与实施工作项以 GitHub Issues 为权威存放位置，所有 Issue 操作使用 `gh` CLI。工作项拥有本次范围、产品行为与验收、评审和批准证据，并链接长期技术设计的 Agent Note。跨工作项合同按需引用登记的独立产品合同及适用修订，不在 Issue 复制正文。位置与历史例外统一读取 [`docs/AGENTS.md`](../AGENTS.md#历史归属切换)，新工作不再新建 RFC 或 ADR。

## Conventions

仓库：`dehuadong/seeaihub-server-next`。本地 remote `origin` 已配置，`gh` 在仓库内会自动推断目标仓库，通常不需要 `-R`。

- **创建 issue**：`gh issue create --title "..." --body "..."`，多行正文用 heredoc。
- **读取 issue**：`gh issue view <number> --comments`，用 `jq` 过滤评论，并同时取回标签。
- **列出 issue**：`gh issue list --state open --json number,title,body,labels,comments --jq '[.[] | {number, title, body, labels: [.labels[].name], comments: [.comments[].body]}]'`，按需加 `--label` / `--state` 过滤。
- **评论 issue**：`gh issue comment <number> --body "..."`。
- **增删标签**：`gh issue edit <number> --add-label "..."` / `--remove-label "..."`。
- **关闭**：`gh issue close <number> --comment "..."`。


## Proposal workflow

Proposal 是一个 GitHub issue，以 URL 或仓库限定编号（`dehuadong/seeaihub-server-next#<n>`）作为工作标识。同一工作复用已有 issue，不重复创建。Proposal 写全本次范围、产品行为与验收、规划状态、评审和批准证据；持续合同及技术设计只链接其登记属主，明确本次适用修订与范围。规模大小不决定是否另建 Spec，只有跨工作项持续合同才需要独立属主。

工作状态用标签表示，不用正文状态字段：

| 工作状态 | 标签 |
| --- | --- |
| planning | `proposal:planning` |
| ready | `proposal:ready` |
| in-progress | `proposal:in-progress` |
| complete | `proposal:complete` |
| rejected | `proposal:rejected` |

Plan Review 结论与批准证据写入该 issue 的决策/批准依据段，或链接的评审批次，并引用相关需求与设计。只有所需评审与批准齐备、且选定范围没有阻塞性决定或依赖时，才进入 `ready`；`in-progress` 仅在项目规则下取得执行授权后开始；交付并完成最终验证后进入 `complete`。就绪性丢失时，把受影响工作退回 `planning` 并记录阻塞原因；否决时记录决定。工作状态之外，批准与执行授权仍分别遵循项目指令（见仓库根 `AGENTS.md`）。

历史来源：上游总体提案 `dehuadong/seeaihub#674` 及其技术设计 v1–v5 由上游仓库保留，只作为冻结的产品与架构来源，不承担本仓库后续提案或进度。本仓库的承接总览为 `dehuadong/seeaihub-server-next#1`，初期实现映射在冻结的历史设计中保留；有效属主从 [`docs/AGENTS.md`](../AGENTS.md#历史归属切换) 读取。后续新工作全部进入本仓库跟踪器。

### 提案的每条范围条目都要有归宿

提案是范围的最大范围；它结案时，**每条范围条目**必须落进三种归宿之一，并写在结案评论里：

| 归宿 | 要写明 |
| --- | --- |
| **已交付** | 交付它的工作项编号，或直接证据（提交、用例） |
| **明确不做** | 裁决记录（谁在什么时候定的、理由） |
| **转成新工作项** | 新工作项编号 |

**禁止用"切片全部关闭"代替逐条核对。** 实施工单常常把提案里的一条**窄化**成交付得出来的那一半，剩下那半没人承接——`#13` 就是这样结案的：范围第 2 条的"**选择** vendor → **选择**渠道模型"被 `#14` 收窄成"管理员用整份发布定义它"，提案按"切片全部关闭"关闭，那半条需求悬着，直到用户复述才发现。

工作项本身同样声明它**覆盖**提案里的哪几条、以及同一提案里它**不覆盖**哪几条。一条需求在提案里有、在所有工作项的范围里都没有，就是**缺口**：当场指出，写进工作项，**不许默默实现**。

## Pull requests as a triage surface

**PRs as a request surface: no.**（若本仓库把外部 PR 当作功能请求，改为 `yes`；`/triage` 读取此标记。）

设为 `yes` 时，PR 走与 issue 相同的标签和状态，使用对应的 `gh pr` 命令：

- **读取 PR**：`gh pr view <number> --comments` 与 `gh pr diff <number>`。
- **列出待分诊的外部 PR**：`gh pr list --state open --json number,title,body,labels,author,authorAssociation,comments`，只保留 `authorAssociation` 为 `CONTRIBUTOR`、`FIRST_TIME_CONTRIBUTOR`、`NONE` 的条目。
- **评论/打标签/关闭**：`gh pr comment`、`gh pr edit --add-label` / `--remove-label`、`gh pr close`。

GitHub 的 issue 与 PR 共用一套编号空间，裸 `#42` 可能是其中之一——用 `gh pr view 42` 判定，再回退到 `gh issue view 42`。

## When a skill says "publish to the issue tracker"

创建一个 GitHub issue。

## When a skill says "fetch the relevant ticket"

执行 `gh issue view <number> --comments`。

## Wayfinding operations

供 `/wayfinder` 使用。**地图**是一个 issue，**子工单**是它的子 issue。

- **地图**：单个带 `wayfinder:map` 标签的 issue，正文承载 Notes / Decisions-so-far / Fog。`gh issue create --label wayfinder:map`。
- **子工单**：作为 GitHub sub-issue 关联到地图（对 sub-issues 端点调用 `gh api`）。子 issue 不可用时，把子项加入地图正文的任务列表，并在子项正文顶部写 `Part of #<map>`。标签为 `wayfinder:<type>`（`research`/`prototype`/`grilling`/`task`）。认领后把工单指派给推进者。
- **阻塞**：使用 GitHub 原生 issue 依赖（UI 可见的权威表示）。用 `gh api --method POST repos/<owner>/<repo>/issues/<child>/dependencies/blocked_by -F issue_id=<blocker-db-id>` 建边，其中 `<blocker-db-id>` 是阻塞者的数字**数据库 id**（`gh api repos/<owner>/<repo>/issues/<n> --jq .id`，不是 `#number` 或 `node_id`）。GitHub 通过 `issue_dependencies_summary.blocked_by` 报告当前阻塞（仅未关闭者，是实时闸门）。依赖功能不可用时，回退为子项正文顶部的 `Blocked by: #<n>, #<n>`。所有阻塞者关闭后，该工单即解除阻塞。
- **前沿查询**：列出地图的未关闭子项（`gh issue list --state open`，限定在地图的 sub-issues / 任务列表内），剔除存在未关闭阻塞（`issue_dependencies_summary.blocked_by > 0`，或 `Blocked by` 行中仍有未关闭 issue）或已有指派者的条目；按地图顺序取第一个。
- **认领**：`gh issue edit <n> --add-assignee @me` —— 本次会话的第一次写入。
- **解决**：`gh issue comment <n> --body "<答案>"`，然后 `gh issue close <n>`，再把上下文指针（要点 + 链接）追加到地图的 Decisions-so-far。
