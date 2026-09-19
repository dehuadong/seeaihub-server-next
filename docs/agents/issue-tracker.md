# Issue tracker: GitHub

本仓库的 Issue 与 Spec 以 GitHub Issues 为权威存放位置，所有操作使用 `gh` CLI。

## Conventions

仓库：`dehuadong/seeaihub-server-next`。本地 remote `origin` 已配置，`gh` 在仓库内会自动推断目标仓库，通常不需要 `-R`。

沙箱限制：本机沙箱内 SSH 传输不可用（`ssh.exe: couldn't create signal pipe`），因此 `git ls-remote`、`git fetch`、`git push` 在受限会话中会失败；`gh` 走 HTTPS + keyring，读写 Issue 不受影响。

- **创建 issue**：`gh issue create --title "..." --body "..."`，多行正文用 heredoc。
- **读取 issue**：`gh issue view <number> --comments`，用 `jq` 过滤评论，并同时取回标签。
- **列出 issue**：`gh issue list --state open --json number,title,body,labels,comments --jq '[.[] | {number, title, body, labels: [.labels[].name], comments: [.comments[].body]}]'`，按需加 `--label` / `--state` 过滤。
- **评论 issue**：`gh issue comment <number> --body "..."`。
- **增删标签**：`gh issue edit <number> --add-label "..."` / `--remove-label "..."`。
- **关闭**：`gh issue close <number> --comment "..."`。

跨仓库引用（例如历史提案）必须显式指定 `-R dehuadong/seeaihub <number>`。

## Proposal workflow

Proposal 是一个 GitHub issue，以 URL 或仓库限定编号（`dehuadong/seeaihub-server-next#<n>`）作为工作标识。同一工作复用已有 issue，不重复创建；独立的需求与设计工件留在其注册位置并链接，不复制正文。

工作状态用标签表示，不用正文状态字段：

| 工作状态 | 标签 |
| --- | --- |
| planning | `proposal:planning` |
| ready | `proposal:ready` |
| in-progress | `proposal:in-progress` |
| complete | `proposal:complete` |
| rejected | `proposal:rejected` |

这些标签目前尚未在远端创建；需要时用 `gh label create "<标签>" -R dehuadong/seeaihub-server-next` 建立，或改用 issue 正文中的状态行。本配置的建立不构成发布工作或创建远端标签的授权。

Plan Review 结论与批准证据写入该 issue 的决策/批准依据段，或链接的评审批次，并引用相关需求与设计。只有所需评审与批准齐备、且选定范围没有阻塞性决定或依赖时，才进入 `ready`；`in-progress` 仅在项目规则下取得执行授权后开始；交付并完成最终验证后进入 `complete`。就绪性丢失时，把受影响工作退回 `planning` 并记录阻塞原因；否决时记录决定。工作状态之外，批准与执行授权仍分别遵循项目指令（见仓库根 `AGENTS.md`）。

历史来源：上游总体提案 `dehuadong/seeaihub#674` 及其技术设计 v1–v5 由上游仓库保留，只作为冻结的产品与架构来源，不承担本仓库后续提案或进度。本仓库的承接总览为 `dehuadong/seeaihub-server-next#1`，实现映射登记在 `docs/design/0001-image-generation.md`，后续新工作全部进入本仓库跟踪器。

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
