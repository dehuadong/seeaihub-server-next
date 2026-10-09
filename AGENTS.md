# Agent Rules
> 使用 ASD-STE100 格式以及使用中文简体表述输出汇报结果。
> 表达简明扼要，使用白话，不要抛技术选择题，严禁黑话与虚浮套话;
> 新造词必须和用户解释或按照约定的术语与定义;有产品空洞先自己核实代码/文档再上报。
> 具体业务任务，多实测少猜测，基于验证而非空想推进任务。

文档遵循 [`docs/AGENTS.md`](docs/AGENTS.md)：文档分层、位置、职责与写法。

## 工程工作流 

工程任务从 Discuss 开始。

### Discuss

使用 Discuss 理解问题，结合现有代码、文档入口登记的有效合同与设计及项目约束形成解决方案，并消除足够的歧义，以判断下一阶段。

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

若可以判断讨论已经足够成熟，并说明已解决事项、剩余未决事项和建议的下一阶段，但不能仅凭自身判断离开 Discuss。

何时从 Discuss 进入 Planning 或 Implementation Gate，由用户决定。若用户此前的请求已经明确授权推进，则复用该授权。

讨论后推进、直接收到阶段请求或恢复工程工作时，读取并按 `docs/agents/GATES.md` 的 Steps、授权与返回规则执行；具体工作方法由对应技能负责。

文档和仓库治理工作可以绕过该工程工作流，除非它改变了重要的产品、技术、架构或其他工程契约。

## 工程边界

- `apps/api`：HTTP 控制面与图片生成的同步直接执行入口；执行事实与账务入 PostgreSQL，业务载荷只在内存。
- `apps/worker`：异常对账与清理；不领取生成任务、不重发生成请求。
- `crates/domain`：稳定领域类型与状态规则，不依赖数据库、HTTP 或 Provider。
- `crates/application`：用例与端口，编排领域对象。
- `crates/persistence`、`crates/adapter-*`：基础设施实现。
- 模块通过公开接口通信；基础设施不得反向拥有目录、Job 或账本规则。

## 事实与安全

- PostgreSQL 是业务事实权威；缓存不是事实来源。
- 图片不落盘：请求里的图就是参数值（只收公网 URL；本地文件先经 `POST /v1/uploads/images` 换成公网 URL），结果按渠道原形回（`url` 或 `b64_json`）。合同归属与有效范围见[文档入口](docs/AGENTS.md#历史归属切换)。
- Provider 凭证只从环境变量读取，不写入配置、日志、响应或测试 fixture。
- Provider 创建请求状态不确定时进入 `reconciliation_required`，不得自动重提。
- 模型 Schema、Offering、Channel、Price Plan 经不可变 Runtime Revision 发布；请求与 Job 固定受理时版本。


## 验证

本地按改动范围运行能发现本次回归的最小必要检查。检查范围、运行时机与推送前核对要求见 [`docs/agents/git.md`](docs/agents/git.md)。

Rust 全量检查默认由 CI 执行。以下命令在仓库根运行，不要求每次在本地执行：

```sh
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

本地全量检查仅用于用户明确要求、排查 CI 失败，或改动确实横跨整个仓库。

这三条与 CI 的前三步逐字一致，但它们**只覆盖进程内的单元用例**：需要真实 PostgreSQL、真实
Redis 或独立子进程的用例都带 `#[ignore]`（全仓 348 条），第三条命令会编译它们却一条都不跑
（输出里那些 `0 passed` 的测试二进制就是它们）。CI 额外跑两层：

```sh
cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1   # 契约层（真库真进程）
cargo test -p seeai-persistence -- --ignored --test-threads=1                # 端口层（真库）
```

两条都要 `HTTP_CONTRACT_DATABASE_URL`（本机指 `seeai_contract` 库）且**串行**跑。所以"三条全绿"
只说明编译与单元层没问题，不等于验收成立；要验行为，按改动面挑上面两层的过滤词跑。

本机只编"这次要验的目标"：反馈用 `cargo check -p <crate>`；用例用 `cargo test -p <crate> --lib`，或
`cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1 <过滤词>`。
`--workspace --all-targets` 会把每个 crate 的每个测试二进制都编一遍——那是 CI 的事（开发 profile 已去掉
依赖的调试信息，但 `target/debug` 仍会随每次改动增长）。

`target/` 变大时**不要**顺手 `cargo clean`（它连增量缓存一起清掉，下一次全量重编很慢）：先删
`target/debug/incremental`（可再生成），或 `cargo clean -p <crate>` 只清一个 crate；要按时间清可装
`cargo-sweep` 跑 `cargo sweep --time 30`。

真实 Provider 测试必须显式启用并限制调用次数；普通测试不得产生外部费用。

文档、Agent Note 与技能按改动范围在本地检查。已通过且仍适用的检查，不因提交或推送重复运行。只报告实际跑过的命令。


### 浏览器行为：跑 `npx playwright test`

playwright test e2e 参阅 `apps/web/AGENTS.md`

## 通用约定
- 未经批准不发起任何计费调用
- 未经批准不自主选择渠道模型
- 新的持久架构决定必须获得用户确认，记录属主按 `docs/AGENTS.md` 选择
- Rust 全量门禁交给 CI，不在本地重跑：本地只按改动面跑能挡住这次回归的最小证据，文档、Agent Note 与技能按各自约定检查；没跑的检查就说没跑。跑哪些、什么时候跑、推之前核对什么见 [`docs/agents/git.md`](docs/agents/git.md)
- 测试不写在源码文件里：源码文件只留 `#[cfg(test)] mod tests;`，用例放同级的 `tests.rs`（`src/lib.rs`、`src/main.rs` 用 `src/tests.rs`，`src/<模块>.rs` 用 `src/<模块>/tests.rs`），多了就在对应 `tests/` 子目录里按主题分文件；生产代码的可见性不为测试放宽，夹具可放宽到测试模块内部；只有需要真实数据库、真实 Redis 或独立进程的端到端用例才进 crate 的 `tests/`（测试夹具自身的检查随夹具同处），`#[ignore]` 必须写明前置条件

## 文档与约定

每个会话需要的规则从这里找；每份文档只写自己的职责，位置、生命周期与写法不在别处重复。

| 文档 | 职责 |
| --- | --- |
| `docs/agents/GATES.md` | 工程流程的阶段编排、门禁与返回路径 |
| `docs/agents/issue-tracker.md` | Proposal 与工单：GitHub Issues 为准、工作状态标签、PR 分诊 |
| `docs/agents/domain.md` | 领域工程判断：读什么、术语纪律、冲突标注、独立架构记录准入与引用 |
| `docs/AGENTS.md` | 文档标准：文档分层、位置、职责、正文与注释的写法、slop 清单 |
| `docs/agents/git.md` | 提交与推送：提交粒度与信息、推送前跑哪些证据、历史改写 |
| `.agents/notes/` | Agent Note：记录范围、生命周期、文件骨架与检查（`README.md` 与 `AGENTS.md`） |

本仓库是单上下文：仓库根 `GLOSSARY.md` 是唯一词汇表，不存在 `GLOSSARY-MAP.md`。

重大工程变更与重要提案遵循 `.agents/notes/README.md`：改动时在同一变更里维护相关记录，并运行文档化检查。
