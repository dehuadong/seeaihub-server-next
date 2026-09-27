# Agent Rules
> 使用中文简体回复用户的问题，包括GitHub Issues文档产物。
> 表达简明扼要，讲人话，使用大白话；先给结论，用通俗标准语言，严禁黑话与虚浮套话;新造词必须和用户解释或按照约定的术语与定义。
> 输出直接呈现核心事实与动作，不打无意义流水账。
> 具体业务任务，多实测少猜测，基于验证而非空想推进任务。

这个仓库是独立的新服务端，不引用或修改旧 SeeAI Hub 仓库的内部代码、数据库和缓存。

文档遵循 [`docs/AGENTS.md`](docs/AGENTS.md)：文档分层、位置、职责与写法。

## 工程边界

- `apps/api`：HTTP 控制面与图片生成入口。
- `apps/worker`：领取持久 Job、调用 Provider、落账与结算。
- `crates/domain`：稳定领域类型与状态规则，不依赖数据库、HTTP 或 Provider。
- `crates/application`：用例与端口，编排领域对象。
- `crates/persistence`、`crates/adapter-*`：基础设施实现。
- 模块通过公开接口通信；基础设施不得反向拥有目录、Job 或账本规则。

## 事实与安全

- PostgreSQL 是业务事实权威；缓存不是事实来源。
- 图片不落盘：请求里的图就是参数值（公网 URL 或 data URL），结果按渠道原形回（`url` 或 `b64_json`）。
- Provider 凭证只从环境变量读取，不写入配置、日志、响应或测试 fixture。
- Provider 创建请求状态不确定时进入 `reconciliation_required`，不得自动重提。
- 模型 Schema、Offering、Channel、Price Plan 经不可变 Runtime Revision 发布；请求与 Job 固定受理时版本。

## 验证

在仓库根执行：

```sh
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

真实 Provider 测试必须显式启用并限制调用次数；普通测试不得产生外部费用。

按改动面选能挡住这次回归的**最小**证据；要提交或推送不构成把已经通过的检查再跑一遍的理由。Rust 全量门禁由 CI 承担，文档、Agent Note 与技能按改动面在本地检查。只报告实际跑过的命令。提交、推送与历史改写见 `docs/agents/git.md`。

### 浏览器行为：跑 `npx playwright test`

playwright test e2e 参阅 `apps/web/AGENTS.md`

## 通用约定
- 未经批准不发起任何计费调用
- 未经批准不自主选择渠道模型
- 当需要记录持久的ADR架构设计决策时必须获得用户确认
- Rust 全量门禁交给 CI，不在本地重跑：本地只按改动面跑能挡住这次回归的最小证据，文档、Agent Note 与技能按各自约定检查；没跑的检查就说没跑。跑哪些、什么时候跑、推之前核对什么见 [`docs/agents/git.md`](docs/agents/git.md)
- 测试不写在源码文件里：源码文件只留 `#[cfg(test)] mod tests;`，用例放同级的 `tests.rs`（`src/lib.rs`、`src/main.rs` 用 `src/tests.rs`，`src/<模块>.rs` 用 `src/<模块>/tests.rs`），多了就在对应 `tests/` 子目录里按主题分文件；生产代码的可见性不为测试放宽，夹具可放宽到测试模块内部；只有需要真实数据库、真实 Redis 或独立进程的端到端用例才进 crate 的 `tests/`（测试夹具自身的检查随夹具同处），`#[ignore]` 必须写明前置条件

## 工程工作流

> 文档和仓库治理工作可以绕过该工程工作流，除非它改变了重要的产品、技术、架构或其他工程契约。

工程任务从 Discuss 开始。
进入后续阶段时，读取并按 `docs/agents/engineering.md` 执行：
`Discuss → Planning → Implementation Gate → Implement → Verify`

阶段技能交接（锚在**可判定的动作**上，而不是"进入了某个阶段"）：

* 用户授权推进、且存在待固化的合同决策（进入 Planning）→ `planning`
* Implementation Gate 通过、**写第一行实现代码之前** → `implement`
* 实现完成、**报告完成或提交之前** → `code-review`（Implementation Review）
* 实现期采用测试优先 → `tdd`
* 审查通过、**声称交付（PASS/完成）之前** → `verify`

上列动作发生前必须加载对应技能。加载技能不等于满足该阶段的完成条件，也不替代该阶段要求的审查。
Discuss 及阶段推进权限在此定义；Planning 之后的阶段编排、门禁、审查收敛和返回路径由 `docs/agents/engineering.md` 定义。


### Discuss

理解问题，结合现有代码、文档、历史决策、已接受的 ADR/RFC 和项目约束形成解决方案，并消除足够的歧义，以判断下一阶段。
讨论不是被动访谈。优先自行分析已有项目上下文，而不是把可判断的问题交给用户。
讨论过程中：

- 明确实际问题、约束和相关既有决策
- 提出有实质差异的可行方案，并在依据充分时给出推荐
- 说明关键权衡、风险和影响
- 发现假设与现有事实或决策冲突时，明确指出
- 仅在缺少必要信息，或涉及未决的产品、业务、范围、兼容性、成本、风险及其他价值判断时，请求用户裁决

不要要求用户重复已有信息，也不要求在 Discuss 中确定所有实现细节。

在以下情况下继续停留在 Discuss：

- 工作仍处于探索阶段
- 仍在比较重要的备选方案
- 用户当前只是寻求理解，而不是准备推进
- 目标或选定范围尚不足以形成实施合同

不要仅因为正在讨论产品、技术或架构决策，就创建规划产物。
模型可以判断讨论已经足够成熟，并说明已解决事项、剩余未决事项和建议的下一阶段，但不能仅凭自身判断离开 Discuss。
何时从 Discuss 进入 Planning 或 Implementation Gate，由用户决定。若用户此前的请求已经明确授权推进，则复用该授权。
当用户已授权推进时：
- 若仍有重要合同决策需要补全或正式固化，则进入 Planning
- 否则按照 `docs/agents/engineering.md` 中的 Implementation Gate 继续

授权进入 Planning 不等于授权实施。

 
### 执行授权

当当前工作已具备实施条件但尚未获得执行授权时，需要用户明确输入“执行实现”。

“确认”“可以”“同意”等仅表示审批，不构成执行授权。

执行授权仅适用于**当前已确认范围的实施工作**，并覆盖该工作内部连续发生的：

- 实施
- Implementation Review
- 范围内修正
- Verify

这些阶段之间不需要重复请求执行授权。

以下情况不继承原有执行授权：

- 开始新的工作项
- 扩大或改变已授权范围
- 返回 Planning 后形成了实质变化的合同或方案
- 出现需要用户重新裁决的重大决策

执行授权不会因为处于同一会话而自动延续到其他工作。

端到端请求在获得当前工作项的执行授权并通过相应门禁后，持续推进至验证完成。

限定阶段的请求，在该阶段及其要求的审查完成后结束。

### 开工前读合同

工程任务的范围由**工作项**拥有，不在对话里。开工前先取回它，不能靠代码与对话猜：

1. `gh issue view <n>` 读选定范围、实施步骤表与**明确排除项**（约定见 [`docs/agents/issue-tracker.md`](docs/agents/issue-tracker.md)）；
2. **沿引用链往上读到提案、往下读到承接这条需求的实施工单**（"没有提案"要作为明确事实记下，不能当成没看过）。要能说出：我这次做的那条需求**写在哪**、**由哪个工单承接**、**那个工单是否已关闭**；
3. 读适用 Spec 的验收条件（`docs/specs/`）与 RFC（`docs/design/`）；
4. 列本轮清单，逐条标注依据（issue 的哪一步、Spec 的哪一条）。**清单之外的事不做**。

**需求本体没被设计过的东西不做。** 只读到"某个工作项说了要它"不够：提案 `#13` 范围里的"**选择** vendor → **选择**渠道模型"，被承接它的切片工单窄化成了"管理员用整份发布定义它"；提案随后按"切片全部关闭"结案，而那半条需求从未交付，最后是靠用户复述才发现。判断方法是第 2 步那三个问题；答不上来，或答案是"工单没覆盖这一条"，就**停下来把它写进工作项**，别用实现去补。

需求只能来自工作项或 Spec。自己认为"应该有"的东西写进工作项等裁决，**不直接实现**——那是范围外的设计，做得再多也是要返工的。

范围有缺口或合同要改，回 `planning`（见 `docs/agents/engineering.md` §1），评审收敛后再动手；不在实现里补合同。改动 Spec 或 RFC 要同步工作项，不能改完即算生效。

工作状态写回工作项（标签见 [`docs/agents/issue-tracker.md`](docs/agents/issue-tracker.md)），实现进度与范围外的改动都记在那里，不留在对话里。


## 文档与约定

每个会话需要的规则从这里找；每份文档只写自己的职责，位置、生命周期与写法不在别处重复。

| 文档 | 职责 |
| --- | --- |
| `docs/agents/engineering.md` | 工程流程的阶段编排、门禁与返回路径 |
| `docs/agents/issue-tracker.md` | Proposal 与工单：GitHub Issues 为准、工作状态标签、PR 分诊 |
| `docs/agents/domain.md` | 领域工程判断：读什么、术语纪律、冲突标注、ADR 准入与引用 |
| `docs/AGENTS.md` | 文档标准：文档分层、位置、职责、正文与注释的写法、slop 清单 |
| `docs/agents/git.md` | 提交与推送：提交粒度与信息、推送前跑哪些证据、历史改写 |
| `.agents/notes/` | Agent Note：记录范围、生命周期、文件骨架与检查（`README.md` 与 `AGENTS.md`） |

本仓库是单上下文：仓库根 `CONTEXT.md` 是唯一词汇表，不存在 `CONTEXT-MAP.md`。

重大工程变更与重要提案遵循 `.agents/notes/README.md`：改动时在同一变更里维护相关记录，并运行文档化检查。
