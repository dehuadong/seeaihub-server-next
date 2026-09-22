---
title: 提交与推送的落地纪律：按改动面选证据、只允许带 lease 的历史改写
status: implemented
created: 2026-09-22
updated: 2026-09-22
approval: 用户在本会话中要求参考上游 deepseek-harness 的文档治理与 git 工作流改进本仓库约定，并指出此前没有参考其技能与脚本；属仓库文档治理，不启动工程流程
verification: 规则内容逐条对着仓库实际做法核过——提交信息形状取自 `git log`（`feat` 17 / `docs` 15 / `fix` 4 / `test` 2 / `chore` 1），分支模型取自 `git branch -a`（只有 `main`），沙箱内 `git push` 不可用取自 `docs/agents/issue-tracker.md` 的既有记录；`node scripts/decisions/check.mjs` 通过；本次只改 Markdown，未跑 `cargo` 门禁
---

# Agent Note：提交与推送的落地纪律：按改动面选证据、只允许带 lease 的历史改写

## 问题

本仓库此前没有提交、推送与历史改写的约定。根 [`AGENTS.md`](../../../../AGENTS.md) 的「验证」只列了三条命令，没说什么时候跑、要不要为了提交再跑一遍、推送前核对什么；实际做法只能从 `git log` 里反推（`feat:` / `docs:` 标题、密集正文、最后一段「验证：」），而改写历史、推送失败、本机 CRLF 提示这些情况完全没有规则。

上游 deepseek-harness 的做法正好对着这几个洞：它的 [`dsh-pre-push-checks`](https://github.com/deepseek-ai/deepseek-harness/blob/master/.agents/skills/dsh-pre-push-checks/SKILL.md) 技能要求按改动面选**最小**证据、明确禁止「因为要提交/推送就重复一遍已经通过的检查」、只报告实际跑过的命令；根指令要求历史改写带 lease、改写后重新核对评审与检查；`docs/development.md` 写明钩子只做窄的、能当场修的事。这些约束在本仓库一条都没有。

## 决定

- 提交粒度与提交信息、证据选择、推送与历史改写的约定在 [`docs/agents/git.md`](../../../../docs/agents/git.md)；根 `AGENTS.md` 的「验证」与「文档与约定」表各有一个入口。
- **按改动面选最小证据**，并写明「要提交或推送，不构成把已经通过的检查再跑一遍的理由」。CI（[`.github/workflows/ci.yml`](../../../../.github/workflows/ci.yml)）拥有穷尽覆盖：全量 `cargo test --workspace --all-features`、`--ignored` 端到端整跑与平台矩阵都在那里；本地跑全量只用于用户明确要求、排查 CI 失败，或改动确实横跨整个仓库。
- **只报告实际跑过的命令**：没跑就写没跑，不用「应该没问题」代替证据。
- 提交信息沿用 `git log` 的既有写法：标题 `<type>: <一句话>`，正文说清改了什么、为什么以及与已批准决定的偏差，最后一段用 `验证：` 列实跑证据。
- 历史改写只允许 `--force-with-lease=<branch>:<观察到的远端 OID>`，禁止裸 `--force`；lease 不成立就中止并重新取远端状态；改写之后重新取远端 head，重新核对评审线程、批准与检查结论。
- 文件尾只留一个换行，提交前 `git diff --cached --check`；本机 `core.autocrlf` 报的 LF→CRLF 提示是本机配置的提示，不是文件错误。

## 备选方案

- **把上游的 `dsh-pre-push-checks` 技能整份搬过来**：不采纳。那份技能一百多行，价值在覆盖上游几十条测试通道的选面矩阵（Vitest 过滤、覆盖率 `--coverage.include`、快照放置、Remote mock）；本仓库的通道只有 cargo 三条命令加一个决策记录检查，「选最小证据」一句话就够，整份搬来是一份没有对应对象的流程。
- **搬 `gh stack` 那套堆叠 PR 流程**：不采纳。本仓库只有 `main` 一条分支、改动直接落主干（`git log` 的既有做法），没有「堆叠 PR」这个对象；其中可迁移的只有「改写必须带 lease」「改写后重新核对」两条，已经写进 `git.md`。
- **本地装 Git 钩子代替纪律**（上游用 lefthook：`pre-commit` 只做暂存区 lint 与空白，`pre-push` 只做增量 typecheck）：不采纳。本仓库没有钩子，`.agents/notes/README.md` 也写明脚本不默认安装钩子；钩子一宽就会有人用 `--no-verify` 绕过，等于没有。真要装，规则已经写在 `git.md` 里：只装窄的、能当场修的事。
- **搬上游的 `change-scope` 脚本**（对已验证的 base 出一份改动面报告）：不采纳。它解决的是「本地与 CI 通道极多、要自动选出该跑哪些」的问题；本仓库用 `git diff --stat <base>...HEAD` 与 `git status --short` 就够，多一个脚本就多一处要维护的解析。

## 后果

- 本地不再默认把整仓测试跑一遍。交付门槛与 CI 门槛不受影响，但「选哪些证据」靠人判断，选窄了就是漏证据——所以规则同时要求只报告实际跑过的命令：漏没漏，在提交信息里看得见。
- 提交信息有了固定形状（标题 + 正文 + `验证：`），`git log` 里那条内容只有一个「1」的提交就是这条规则要挡的东西。
- 历史改写多了一道前置动作（先取远端、记下 OID），换来的是不会覆盖别人推进过的远端分支。
- 宽窄交给纪律而不是钩子：代价是纪律只能靠审阅与 `git log` 检验，好处是没有人能 `--no-verify` 绕过它。

## 验证

- 规则内容逐条对着仓库实际做法核过，不是照抄上游：提交信息形状取自 `git log`（`feat` 17 / `docs` 15 / `fix` 4 / `test` 2 / `chore` 1），分支模型取自 `git branch -a`（只有 `main` 与 `origin/main`），沙箱内 `git push` / `git fetch` 不可用取自 [`docs/agents/issue-tracker.md`](../../../../docs/agents/issue-tracker.md) 的既有记录。
- `node scripts/decisions/check.mjs` 通过（本条记录与文件格式门禁一致）。
- 本次只改 Markdown 与新增约定文档，未触碰 Rust 代码，未跑 `cargo` 门禁。

## 依据与关联

- 上游：[`dsh-pre-push-checks`](https://github.com/deepseek-ai/deepseek-harness/blob/master/.agents/skills/dsh-pre-push-checks/SKILL.md)、[`dsh-merging-stacked-prs`](https://github.com/deepseek-ai/deepseek-harness/blob/master/.agents/skills/dsh-merging-stacked-prs/SKILL.md)、上游根 `AGENTS.md` 的「Run relevant checks locally」与「Choose PR history deliberately」、上游 `docs/development.md` 的钩子范围。
- 生效约定：[`docs/agents/git.md`](../../../../docs/agents/git.md)；入口在根 [`AGENTS.md`](../../../../AGENTS.md) 的「验证」与「文档与约定」表。
- 相邻记录：[统一 Agent Note 的生命周期与文件格式](./2026-09-22-uniform-agent-note-format.md)（同一轮治理改动，含写作标准）、[取消生成式 Agent Notes 索引](./2026-09-22-remove-generated-agent-note-index.md)。
