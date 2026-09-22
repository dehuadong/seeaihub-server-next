---
title: 取消生成式 Agent Notes 索引：目录树即清单，只留校验
status: implemented
created: 2026-09-22
updated: 2026-09-22
approval: 用户在本会话中直接指示取消生命周期 INDEX.md 及其关联文件（`.agents/notes/README.md` 改造保留、`.agents/notes/INDEX.md` 与 `scripts/decisions/update-index.mjs` 删除），并指定以上游 Agent Note「无需生成索引即可发现 Agent Note」的设计理由为准；属仓库文档治理，不启动工程流程
verification: 删除后运行 `node scripts/decisions/check.mjs` 通过（15 条记录，含本记录；无配置/目录/元数据/链接错误）；守卫的检出能力用实测确认（临时放回 `INDEX.md` → 检查 exit 1 并报错，删除后 exit 0）；改动为文档与本地脚本，未触碰 Rust 代码，`cargo` 门禁不受影响
---

# Agent Note：取消生成式 Agent Notes 索引：目录树即清单，只留校验

## 问题

`.agents/notes/INDEX.md` 是按生命周期和分类渲染出来的集中清单，由 `scripts/decisions/update-index.mjs` 生成，`check.mjs` 再逐字比对它是否新鲜。这份清单记录的事实——文件处在哪个生命周期/分类目录、文件名里的日期、正文 H1 的标题——**文件名和目录路径本身已经编码了一遍**，清单只是重复。

代价有两处：

- **共享文件争用**：任何分支只要新增、移动或重命名一条彼此无关的记录，都会重写同一个 `INDEX.md`，它是可预见的合并冲突热点；冲突本身能靠重新生成机械解决，但"无关改动撞同一文件"这件事解决不了，评审噪音也不减。
- **维护负担**：渲染器、生成命令和索引新鲜度检查都要跟着目录规则一起维护，而它们提供的发现价值有限——浏览 `{生命周期}/{分类}/` 目录树或搜索仓库同样能找到记录。

本次要把生成索引这一层整个取消，让目录树成为清单，只保留校验。

## 决定

- **删除** `.agents/notes/INDEX.md` 与渲染器 `scripts/decisions/update-index.mjs`；工具只剩 `scripts/decisions/{lib,check}.mjs`。
- **`check.mjs` 去掉索引新鲜度检查**，保留其余全部校验（配置、生命周期目录与分类目录一致性、文件名与 `created` 一致、必要元数据、日期、文件名唯一性、内联本地链接目标），以及本仓库本地新增的「`docs/agents/artifacts.md` 的『新工件归属』表不得写变更信息」那条。
- **`lib.mjs` 删掉 `renderIndex()` 与索引相关的特判**（`ignoreIndex` 参数、根目录 `INDEX.md` 的豁免），并**新增一条反向守卫**：根目录若又出现 `INDEX.md`，直接报错。这条守卫替代了原来的"新鲜度检查"，防止索引生成那套被顺手重新引入。
- **`.agents/notes/README.md` 保留**，作为人工维护的入口和约定；只改掉与索引有关的表述：开头不再指向索引、类别只用于校验而非"生成类别视图"、状态迁移后"再运行检查"而不是"再生成索引"、原「索引与检查」一节改为「检查」并写明不设索引的理由与 `INDEX.md` 会被拒。
- **同步引用**：`AGENTS.md` 的 Agent Notes 一段去掉"重新生成索引"；工具"只校验、不渲染"的说明落在 [`scripts/decisions/README.md`](../../../../scripts/decisions/README.md)（工具自己的家）；`docs/architecture.md` 的文件表把 `scripts/decisions/*.mjs` 的描述改为"检查（不生成索引）"。

发现路径随之改变：读者不再有单一的时间顺序页面，改用生命周期/分类目录树或仓库搜索。

## 备选方案

- **保留已提交的生成索引，靠重新生成解决冲突**：不采纳。重新生成能让冲突解决过程机械化，但拦不住无关分支修改同一产物，评审噪音照旧。
- **保留一个不提交到仓库的按需索引命令**：不采纳。已提交文件的冲突是没了，但渲染器和命令仍要维护，而目录树浏览与仓库搜索已经覆盖这条发现路径。
- **改成人工维护的索引**：不采纳。它同样造成共享文件争用，还会重新引入生成机制本来能避免的完整性（漏记、错记）与排序错误。
- **连 `README.md` 一起删**：不采纳。约定本身（记录范围、分类、生命周期、更新责任）仍需要一个人工入口，仓库根 `AGENTS.md` 与 `docs/agents/artifacts.md` 也指向它；取消的只是生成索引这一层，不是记录制度。
- **只删文件、不留下"拒绝 `INDEX.md`"的守卫**：不采纳。删除是一次性动作，守卫才是让这件事不再回来的那部分，成本只有一行判断。

## 验证

- `node scripts/decisions/check.mjs` 在删除后通过：15 条记录（含本记录），无配置、目录、元数据、文件名与本地链接错误；同时因为 `INDEX.md` 已删除，新加的守卫不触发。
- 守卫本身的检出能力用实测确认：把 `INDEX.md` 临时放回根目录后检查报错，删除后恢复通过（不是靠"跑一次过了"推断）。
- 全仓库检索确认没有残留的索引生成引用：`INDEX.md`、`update-index`、`scripts/decisions` 的提及只出现在已同步的文档里。
- 未改任何 Rust 代码，`cargo fmt` / `clippy` / `test` 门禁不受本次改动影响。

## 后果

- **集中清单消失后，"最近改了什么"没有单一页面**：改用目录树或仓库搜索，并按记录正文里的 `updated` 字段判断新鲜度。若日后确实需要一份时间顺序视图，应按需生成、不提交到仓库，并明确它不属于校验范围。
- **守卫只认根目录的 `INDEX.md`**：其他位置的同名文件不在拒绝范围内——集中清单只可能生成在根目录，够用；若把索引挪到别处，这条守卫不会拦住。
- **`README.md` 的索引相关表述若在升级 setup bundle 时被覆盖**：[`scripts/decisions/README.md`](../../../../scripts/decisions/README.md) 已写明"bundle 里若带索引渲染器与索引新鲜度检查，不随升级引入"。

## 依据与关联

- 设计理由：上游 Agent Note「无需生成索引即可发现 Agent Note」（[deepseek-harness 仓库](https://github.com/deepseek-ai/deepseek-harness/blob/master/.agents/notes/implemented/process/2026-07-19-remove-generated-agent-note-index.zh.md)）——同一取舍（目录树即清单、README 保留、门禁只校验并拒绝 `INDEX.md`）。
- 约定：[`.agents/notes/README.md`](../../README.md) 的「检查」一节（记录范围、分类、生命周期、更新责任均不变）。
- 登记：[`scripts/decisions/README.md`](../../../../scripts/decisions/README.md)；仓库指令 [`AGENTS.md`](../../../../AGENTS.md) 的 Agent Notes 一段。
- 落点索引：[`docs/architecture.md`](../../../../docs/architecture.md) 的文件与迁移表。
- 工具：[`scripts/decisions/check.mjs`](../../../../scripts/decisions/check.mjs) 与 [`scripts/decisions/lib.mjs`](../../../../scripts/decisions/lib.mjs)。
