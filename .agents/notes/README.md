# Agent Notes

本目录保存影响本仓库的工程提案与已交付决定，以及代码、测试和当前文档无法表达的选择理由、放弃方案与实际后果。已有 Spec、RFC 或 ADR 已经完整拥有相关内容时，Agent Note 只记录本次工程变更独有的理由与验证，不复制正文。

正文遵循 [`docs/AGENTS.md`](../../docs/AGENTS.md)，工件位置遵循 [`docs/agents/artifacts.md`](../../docs/agents/artifacts.md)。

## 布局与命名

路径为 `{lifecycle}/{category}/YYYY-MM-DD-topic-slug.md`：

- `proposed/`：实施前的提案，尚未交付或仅部分交付；
- `implemented/`：决定已经交付并完成验证，内容与当前交付保持同步；
- `rejected/`：提案被否决，仅在拒绝理由仍能避免重要错误时保留。

`category` 是决定种类，以 [`config.json`](./config.json) 为唯一来源。文件名日期是主题首次提出日期，无法确认时使用建档日期。文件名在所有生命周期和分类中唯一，移动生命周期时不改名。

Agent Note 之间使用相对 Markdown 链接。目录树就是清单，不生成集中式 `INDEX.md`。

## 何时需要写

- 重要工程选择的理由无法由代码、测试、Spec、RFC 或 ADR 完整表达时，创建或更新 Agent Note；
- 同一决定已有记录时更新原文件，不创建重复记录；
- 纯机械修改、局部文案、链接与格式修复不创建记录；
- Agent Note 不跟踪任务状态、分派或外部工作项。

完全取代已有决定时，新记录必须接收旧记录仍有价值的理由、备选方案、后果和验证，再删除旧记录并修复入站链接。部分取代时保留两份记录，用相对链接写清仍然有效的边界。

## 文件格式

### 元数据

文件以元数据块开头，只使用以下字段并保持顺序：

```markdown
---
title: 记录标题
status: proposed
created: YYYY-MM-DD
updated: YYYY-MM-DD
approval: 批准事实
verification: 验证证据
reason: 一句话否决理由
---
```

- `status` 与生命周期目录一致；
- `created` 与文件名日期一致，`updated` 不早于 `created`；
- `approval` 用一行写清实际决定权限；新建或实质改写的记录不引用外部工作项；
- `verification` 只用于 `implemented`，引用本地命令、用例或留档；
- `reason` 只用于 `rejected`。

元数据后第一个非空行必须是 `# Agent Note：<title>`，且标题与 `title` 一致。正文第一个章节统一为 `## 问题`。

### 生命周期骨架

| 生命周期 | 必需章节 | 含义 |
| --- | --- | --- |
| `proposed` | `## 提案`、`## 备选方案`、`## 验收条件`、`## 风险` | 写拟议变更、完成判据和有意承担的风险 |
| `implemented` | `## 决定`、`## 备选方案`、`## 后果`、`## 验证` | 用现在时写已交付事实、权衡和实际证据 |
| `rejected` | `## 备选方案`；保留原提案正文 | 在 `reason` 和正文中写清拒绝结论 |

真正独有的技术章节可以插在必需章节之间。`implemented` 不得保留 `## 提案`、`## 计划`、`## 迁移计划` 或 `## 验收条件`。

每份记录必须写真实考虑过的 `## 备选方案`，不得为满足格式编造。格式建立日（2026-09-22）之前创建且无法还原备选方案的记录，可以使用：

```markdown
<!-- agent-note-format: alternatives-not-recorded (pre-format Agent Note) -->
```

## 生命周期迁移

- `proposed → implemented`：移动文件并更新 `status`；把提案改写为现在时的决定，把验收和风险折入后果或验证，用实际交付替换计划；
- `proposed → rejected`：移动文件并更新 `status` 与 `reason`，保留提案正文；
- `implemented` 被部分取代：留在原处，写清失效范围并与新记录互相链接；完全取代按上文合并规则处理。

批准、交付和验证是不同事实。没有充分验证时不得进入 `implemented`。

## 维护

- `implemented` 中的路径、名称、结构、默认值等事实随交付在同一变更中更新，不借机改写原决定；
- 仍能指导未来选择的 `implemented` 保留；只记录机械修改且没有长期理由的记录可以删除；
- `proposed` 不归档，不再推进时转为 `rejected`；
- `rejected` 只在仍能阻止真实且容易重犯的错误时保留。

本仓库暂不设置 `archived/`。需要归档时，应先证明活跃记录已经妨碍发现当前权威，而不是按数量、篇幅或年龄设门槛。

## 检查

从仓库根运行：

```sh
node scripts/decisions/check.mjs
```

检查覆盖目录、分类、文件名、元数据、生命周期骨架、相对链接和文件名唯一性。脚本不判断批准是否属实、决定是否充分或备选方案是否真实，这些仍由审阅负责。
