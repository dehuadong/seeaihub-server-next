---
title: worker 排空逻辑改为可确定验证的循环
status: implemented
created: 2026-09-24
updated: 2026-09-24
approval: 工单 #24 的验收条件要求"给 worker 发终止信号后 Job 不滞留提交中"；原实现已排空，但无法确定验证。本次按该验收把停机输入改为参数化并补齐用例。
verification: `cargo test -p seeai-worker`（4 passed：排空请求与终止信号都不打断在飞那一轮、已在排空态不领新任务、没有停机请求时不退出）；`cargo clippy -p seeai-worker --all-targets --all-features -- -D warnings` 通过；`cargo fmt --all -- --check` 通过。
---

# Agent Note：worker 排空逻辑改为可确定验证的循环

## 问题

工单 #24 的"给 worker 发终止信号后 Job 不滞留提交中"一直挂着 BLOCKED：实现（信号只置位、在飞那一轮跑完再退）在，但**判据没法确定地验**——夹具只起 API 进程，而 Windows 上没有可编程的 Ctrl+C；用 `GenerateConsoleCtrlEvent` 需要 `unsafe`，workspace 又是 `unsafe_code = "forbid"`（`allow` 盖不住 `forbid`）。

原实现另有一个说不清的地方：停机信号与"手上这一轮"写在同一个 `select!` 里，哪一支先就绪取决于运行期乱序，因此"信号会不会打断在飞那一轮"这件事**读代码也读不出确定结论**。

## 决定

- 停机输入收进一个结构（排空开关 `watch` + 终止信号），当**参数**传给主循环：主循环在**领下一轮之前**看停机条件，手上那一轮跑完再看一次。停机因此从不与在飞调用竞争，判据不再依赖 `select!` 的乱序。
- 终止信号是"立即不再领"，排空开关是将来"排水"接口的入口——今天只有测试会置位它，还没有对外端点。
- 跑完先判停机、再退避：退避是"没活干、也没人要求停"时的事（运维取值可能是分钟级），排在停机判定之前会让一次停机白等一个退避周期。
- 真信号（`ctrl_c`）与排空开关都从 `main` 装上；用例用可控的同类输入验同一段循环。

## 备选方案

- **在端到端装置里给 worker 子进程投一个真 Ctrl+C**（`GenerateConsoleCtrlEvent` + 新进程组）：落选。要为测试给 workspace 开 `unsafe` 的口子（`forbid` 不允许 `allow` 局部放宽），而得到的好处只是"信号确实能被操作系统送进去"——那与仓库自己的循环逻辑无关。
- **只走人工验证**（在能发信号的部署里手工验一次）：落选。它把一条会回归的合同压在"记得手工跑"上；改成参数化之后，同一段循环可以在进程内确定地验。
- **保留原来的 `select!` 形状，只补注释**：落选。那样"信号不打断在飞调用"仍然依赖运行期乱序，用例也没法写成确定的断言。

## 后果

- 停机判定与在飞调用彻底分开：`run_once` 一旦开始就一定跑完，排空与终止都只在它前后生效。
- 终止信号是**一次性**的（Ctrl+C 就绪之后一直就绪）：主循环用它时不再重新注册监听，因此不会反复唤醒。
- 两条新用例把合同钉死：停机请求投在"这一轮已经在飞"时，返回前那一轮必须跑完；已经在排空态时一轮都不领；没有停机请求时循环不退出。
- 停机的**操作系统侧**（SIGTERM/Ctrl+C 真的送到进程）仍由部署验证：本件验的是"送到之后循环怎么做"，不是信号投递本身。

## 验证

| 行为 | 证据 |
| --- | --- |
| 排空请求不打断在飞那一轮（请求投在那一轮已经在飞时，返回前它必须跑完） | `a_drain_request_waits_for_the_in_flight_iteration`（`apps/worker/src/worker_loop_tests.rs`） |
| 终止信号同样等手上那一轮跑完 | `an_interrupt_waits_for_the_in_flight_iteration`（同文件） |
| 已经在排空态时一轮都不领（重复的停机请求不会各领一轮） | `an_already_draining_worker_does_not_claim_another_iteration`（同文件） |
| 没有停机请求时循环不退出 | `the_loop_keeps_working_while_no_stop_is_requested`（同文件） |
| 门禁（本改动面） | `cargo fmt --all -- --check`、`cargo clippy -p seeai-worker --all-targets --all-features -- -D warnings`、`cargo test -p seeai-worker` |

## 依据与关联

- 机制设计见 [`docs/design/0009`](../../../../docs/design/0009-operational-baseline.md) §2（排空、时限由部署给、强杀之后不丢事实）；本件只把该节的"谁来验、怎么验"补上，不改它的决定。
- 强杀之后的事实由既有的过期租约回收兜住，不自动重提——见 [`docs/adr/0011`](../../../../docs/adr/0011-safe-before-acceptance-does-not-retry-yet.md)。
