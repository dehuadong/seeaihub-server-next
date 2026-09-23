# Proposal

Proposal 在没有合适既有工作项时作为本次规划的根工作项。它负责为什么要改、这次选定哪部分、决定和就绪状态，并指向适用的 RFC 或 RFC 集合。

## 配置与存放

创建或更新 Proposal 前，读取项目指令指向的 Issue 配置，通常是 `docs/agents/issue-tracker.md`。配置必须说明 Proposal 放在哪里，以及工作状态、评审和批准证据如何维护。

配置缺失或无法确定 Proposal 的处理方式时，在对话中继续讨论和起草，但不要自行选择存放位置、发布 Proposal 或改变远端状态。

复用合适的既有 Proposal。一个工作只保留一个规划根节点，不同时创建远端 Issue 与重复的本地 Proposal。

## 内容

遵循项目既有格式。项目没有格式时使用 [proposal-template.md](./proposal-template.md)。内容与工作规模相称：

- **目标与选定范围**：为什么做、本次交付哪一段以及重要排除项；
- **Spec**：引用适用 RFC 文件的 Spec 章节；没有 RFC 文件的小改动直接写清产品行为与验收；
- **决定**：重要选择、未解决问题、Plan Review 与批准证据；
- **引用**：相关 PRD、Spec、RFC 技术设计、ADR、Agent Note 和实施工作项；Spec 与 RFC 技术设计可以链接同一 RFC 文件的对应章节。

Proposal 不复制 RFC 正文。存在 RFC 文件时，Spec 与技术设计由该文件拥有；Proposal 只说明本次选择哪些 RFC 或切片。没有 RFC 文件的小改动，才由 Proposal 或既有工作项直接拥有 Spec 与必要技术决定。

## 状态更新

只使用配置好的工作状态与证据约定。Plan Review 结果记录在 Proposal 或既有工作项；只有规划内容通过、所需批准齐备且没有阻塞时，才更新为就绪。

评审通过、批准和执行授权是三件不同的事。

Spec、技术设计和实施进度分别留在自己的属主中。
