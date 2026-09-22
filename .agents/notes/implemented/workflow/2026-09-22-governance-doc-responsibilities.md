---
title: 治理文档各管一件事：注册表登记位置，索引只留一处
status: implemented
created: 2026-09-22
updated: 2026-09-22
approval: 用户在本会话中要求审视 `docs/agents/artifacts.md`、`docs/agents/domain.md` 与 `docs/AGENTS.md` 的职责、消除重复；属仓库文档治理，不启动工程流程
verification: `node scripts/decisions/check.mjs` 通过；注册表检查用变异实测确认生效——在「范围」里临时塞入一行「注册模式：保留现有；生效日期：2026-09-22，已更新注册表。」，检查报出「具体日期」与「变更说明」两条，恢复后文件逐字节回到原状；`rootDir` 用 `node -e` 打印确认指向仓库根（修前是仓库外一层，读不到注册表）；全仓本地 Markdown 链接 305 条、断链 0
---

# Agent Note：治理文档各管一件事：注册表登记位置，索引只留一处

## 问题

`docs/agents/artifacts.md`（工件注册表）、`docs/agents/domain.md`（领域文档阅读指南）与 `docs/AGENTS.md`（写作标准）本该各管一件事，实际互相抄，改一处要记得改几处：

- 注册表自己开头摆着 `注册模式：保留现有` 与 setup 的生效日期，而它的表头写着「变更、历史与状态不写进本文件」；它还写了一整节「生命周期与批准」，把变更记录的生命周期含义、Proposal 的工作状态、独立 RFC 与 ADR 的批准约定都收在自己名下——这些规则的家分别是 `.agents/notes/README.md`、`docs/agents/issue-tracker.md`、`docs/agents/domain.md`。
- 「约定文档住在哪」有三处索引：根 `AGENTS.md` 的 Agent skills 小节、注册表的「工程约定文档」行、`docs/AGENTS.md` 开头的三条指针。`domain.md` 又把自己那份位置说明与注册表重了一遍（全篇 9 处提到注册表）。
- 同一件事在几个文件里各说一遍（按话题统计出现次数）：单上下文 / `CONTEXT-MAP` 在注册表与 `domain.md` 共 10 处；`out-reference` 的性质在两处各一份；「推论与待确认必须标明」在注册表行里与 `docs/AGENTS.md` 的写作规则各一份。
- `domain.md` 还手抄了会腐烂的清单：「当前有 `0001`–`0020`，其中 7 篇是退役/合并存根」。
- 那条本该拦住注册表里这些内容的检查**一直是空跑**：`scripts/decisions/lib.mjs` 的 `rootDir` 写成 `../../../`，从 `scripts/decisions/` 上跳三级落到仓库外一层，`readFile` 失败又被 `.catch(() => null)` 吞掉，于是"注册表里不写变更信息"这条检查从来没跑过——文档却写着"靠这条检查而不是靠自觉"。
- 根 `AGENTS.md` 还有一条「引用要带所有者」，把两件事塞进一句话：编号不能裸写（ADR 编号与 Issue 编号撞号），以及引用要能点得回去。前者的家本来就在 `docs/agents/domain.md` 的「引用写法」；后者在我把 `docs/AGENTS.md` 里那条重复副本删掉时**连"相对链接"的要求一起删了**，于是这半条规则在仓库里没有任何家，只剩一句口号和不可点、不可机检的例子（`#6 差距 G5`、`ADR-0017`）。

## 决定

- **注册表只写位置与边界**：删掉注册模式与生效日期两行、删掉「生命周期与批准」整节、去掉开头那句与根 `AGENTS.md` 重复的治理流程说明；列名从「位置与规则」改成「位置与边界」。
- **各家的规则归各家**：变更记录的生命周期与批准归 `.agents/notes/README.md`；Proposal 与 Ticket 的工作状态归 `docs/agents/issue-tracker.md`；ADR 的准入、退役与引用归 `docs/agents/domain.md`；批准与执行授权的区别归仓库根 `AGENTS.md`；正文与注释的写法归 `docs/AGENTS.md`。注册表只在表后留一句边界事实：独立 RFC、Spec 与 ADR 不随变更记录移动。
- **索引只留一处**：根 `AGENTS.md` 新增「文档与约定」表，一个约定文档一行（职责 + 位置）；注册表删掉「工程约定文档」行；`docs/AGENTS.md` 只保留开头三条边界指针；`domain.md` 不再复述位置权威性。
- **`domain.md` 回到自己的活**：读哪些领域文档、术语纪律、冲突标注、ADR 准入与引用。读什么保留路径，"某某是权威位置"改为指向注册表；目录树只留领域文档那几行（`apps/`、`crates/` 归 `docs/architecture.md`）；删掉 ADR 数量与存根计数。
- **补登记**：注册表补「领域词汇表」（`CONTEXT.md`）与「Agent 技能」（`.agents/skills/`）两行；约定文档不再登记在注册表里，入口在根 `AGENTS.md` 的表。
- **检查修好并扩到整个注册表**：`rootDir` 改为 `../../`；除「历史工件与新旧衔接」一节外，注册表里出现具体日期、提交号或变更说明即报错。
- **引用规则拆成两半、各归其家**：根 `AGENTS.md` 删掉「引用要带所有者」。`docs/AGENTS.md` 承接的是**「合同就地写，理由链接走」**：读者在用到的地方要能看到完整合同，**为什么这么定**只链接到拥有它的那一层（ADR、设计或 Agent Note），正文不重述理由。`docs/agents/domain.md` 的「引用写法」只保留一条**条件**规则——引用了编号就写清是谁的编号（ADR 编号与 Issue 编号撞号），不要求"每个编号都带所有者"。`docs/agents/git.md` 里指向根指令的那句改指向这两处。
- **工具自述归工具自己**：新增 [`scripts/decisions/README.md`](../../../../scripts/decisions/README.md)（用法、三处本地修补、升级 setup bundle 的提醒），注册表删掉「决策记录工具」一节与登记行，指向它的四处引用同步改掉。
- **注册表瘦身**（56 → 49 行）：删掉「范围」里与 `domain.md`、`.agents/notes/README.md` 重复的三件事（管理根目录解析、多上下文、Agent Notes 位置），指向别家的登记压成一行指针，合并「既有工作项」与「实施 Ticket」两行。它只回答"放哪、不放什么"。

## 备选方案

- **索引放 `docs/agents/README.md`**：不采纳。根 `AGENTS.md` 是每个会话必读的文件，索引在那里不用多一跳；另开一份索引文件等于多一处要同步的家。
- **注册表继续登记约定文档**：不采纳。`domain.md` 全篇都在讲注册表，根指令也在讲同一批文档；三处索引必然互相过期。注册表只登记工件、词汇表、技能与工具这些"东西放在哪"。
- **把「生命周期与批准」压缩成一张转发表留在注册表**：不采纳。转发表就是第二份索引，改规则时还要记得回来改表；留一句"谁拥有什么"的边界指向就够。
- **目录树整段删掉、只指向 `docs/architecture.md`**：不采纳。`domain.md` 是领域技能的就地配置，读者需要在这里看到领域文档各自的用途；只把 `apps/`、`crates/` 那份（`architecture.md` 的地盘）去掉。
- **让 `domain.md` 保留完整的位置权威性说明**：不采纳。那是注册表已经拥有的结论；技能里保留"读什么、怎么用"这类能直接执行的就地合同就够。
- **给注册表每一行加「不放什么」列**（上游层级表是「职责 / 不该放什么」两列）：不采纳。真正要写明的边界只有几条，现都写在对应行里；为几条边界把整张表改成三列，宽度换不来多少信息。
- **把根指令那条「引用要带所有者」原样留着**：不采纳。它把"不裸写编号"与"引用要能点回去"混成一句，两个家都不在这里；而且 `#6 差距 G5`、`ADR-0017` 这种裸文本自身不可点、不可机检，与"点得回去"自相矛盾。上游在这件事上只有"当前文件用相对链接、历史引用用 tag 或 PR 号"两条，没有"带所有者"这种约定。
- **整条不要，连「不得裸写编号」也去掉**：不采纳。本仓库的 ADR 编号与 Issue 编号共用数字，裸写 `6` 必然有歧义；这条理由 `docs/agents/domain.md` 早就写着。
- **把引用规则写成"必须引用工作项"**：不采纳。工作项只拥有这次工作的**范围与验收**，引用它是"引用属主"；**理由**（为什么这么定）归 `docs/adr/`、`docs/design/` 与 Agent Notes。以工作项为例会把重点从"链接理由"挪到"引用 issue"，本轮一度就是这么写偏的。
- **把位置表并进 `docs/AGENTS.md`、删掉注册表**（上游的形状：位置表就在写作标准里）：不采纳。位置表里有 9 行没有别处可问，而且它还登记非文档类的东西（GitHub Issues、`out-reference/`、脚本、技能）；并进写作标准会让标准从 64 行长到 100+ 行，还要动 `check.mjs` 读的文件与 setup 部署的项目副本。保留但瘦身。
- **只删重复、不动工具段**：不采纳。工具被四个地方写到（`notes/AGENTS.md`、`notes/README.md`、`docs/AGENTS.md`，加注册表），前三处各讲自己那部分，只有注册表那处是重复；工具该自己讲自己。

## 后果

- 三份文档各自只有一个主题，改规则时不用再担心漏改另一处。
- 索引只有根 `AGENTS.md` 一处：新增一份约定文档时改那一张表。
- 注册表检查真的会拦人了（变异实测：塞一行日期与"已更新"进去当场报两条）。它只认模式，判断不了"这句话算不算边界"。
- `rootDir` 那处修补写进了 [`scripts/decisions/README.md`](../../../../scripts/decisions/README.md)（工具自己的家），升级 setup bundle 时不会被覆盖回去；它同时是这一整类故障的样本——读不到文件被 `.catch` 吞掉，检查会安静地通过。
- 注册表不再登记约定文档：找"规则文档住在哪"要看根 `AGENTS.md`。这是有意的单一入口，也是本轮唯一新增的"必须记住去看"的地方。

## 验证

- `node scripts/decisions/check.mjs` 通过，零报错。
- **变异实测**：在注册表「范围」里临时塞入 `注册模式：保留现有；生效日期：2026-09-22，已更新注册表。`，检查报出「具体日期」与「变更说明」两条；恢复后逐字节回到原状。
- **`rootDir` 实测**：`node -e` 打印修前为 `E:\workspace\`（仓库外一层，`path.join(rootDir, 'docs/agents/artifacts.md')` 不存在），修后为仓库根。
- 全仓检索：删掉的内容没有悬空引用（「保留现有」「生效日期与依据」无处出现；「权威属主」只一处；约定文档索引只在根 `AGENTS.md`）。
- 全仓 305 条本地 Markdown 链接、0 断链。
- 只改 Markdown 与 `scripts/decisions/*.mjs`，未触碰 Rust 代码。

## 依据与关联

- 上游：`docs/AGENTS.md` 的层级表（一个事实一个家、「职责 / 不该放什么」两列）与写作规则的「历史归提交、PR 与 Agent Notes」。
- 生效：根 [`AGENTS.md`](../../../../AGENTS.md) 的「文档与约定」表、[`docs/agents/artifacts.md`](../../../../docs/agents/artifacts.md)、[`docs/agents/domain.md`](../../../../docs/agents/domain.md)、[`docs/AGENTS.md`](../../../../docs/AGENTS.md)。
- 检查：[`scripts/decisions/check.mjs`](../../../../scripts/decisions/check.mjs) 与 [`scripts/decisions/lib.mjs`](../../../../scripts/decisions/lib.mjs)。
- 相邻记录：[统一 Agent Note 的生命周期与文件格式](./2026-09-22-uniform-agent-note-format.md)、[提交与推送的落地纪律](./2026-09-22-commit-and-push-discipline.md)、[取消生成式 Agent Notes 索引](./2026-09-22-remove-generated-agent-note-index.md)。
