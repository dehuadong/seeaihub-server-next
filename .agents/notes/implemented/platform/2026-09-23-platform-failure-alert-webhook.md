---
title: 平台故障告警出口
status: implemented
created: 2026-09-23
updated: 2026-09-23
approval: 用户 2026-09-23 授权实施平台故障告警出口
verification: cargo clippy --workspace --all-targets --all-features -- -D warnings 与 cargo test --workspace --all-features 通过；cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1 在本地空库上 95 条全通过（含 5 条告警出口用例），接收器是进程内本地监听，零计费、零外网
---

# Agent Note：平台故障告警出口

## 问题

平台在渠道侧欠费、凭证失效、某条候选连着一路失败，今天只有运营主动去读失败清单才看得见；对账案例新增也没有对外信号。要的是一条最小的外发出口：能定位到哪一条执行出了什么事，但不把仓库里的东西（凭证、提示词、图片）带出去；它还必须是**旁路**——出问题不许改 Job 的处置、结算与对客结果。合同见 [`docs/design/0009-operational-baseline.md`](../../../../docs/design/0009-operational-baseline.md) §6。

## 决定

出口是配置项 `PROVIDER_ALERT_WEBHOOK`：没配就没有出口——不构造端口、不读阈值、一条也不外发，也不内置默认地址。HTTP 实现单独成 crate `crates/alert-webhook`（POST JSON、有界超时与有界重试、地址不可用时构造即失败），用例层只留 `AlertSink` 端口与收口发送失败的 `PlatformAlerter`。

三个触发条件都在 Worker 的失败收尾里判定，位置在失败处置**提交之后**：那时 Job 的终态与那条失败已经落库，告警读的是已提交的事实，也不反过来改它。平台欠费 / 凭证类失败按**既有的** `ProviderFailureKind` 判定；对账案例新增看终态——写成 `reconciliation_required` 就是建案那条路径，建案本身在 `fail_job` 里；某候选连续失败 N 次从 `generation.jobs` 的终态现算（`failed` 与 `reconciliation_required` 算失败，`succeeded` 截断，未定终态的执行不参与），不新建表、也不落一份会漂移的计数。N 是配置项 `PROVIDER_ALERT_CONSECUTIVE_FAILURES`。

发送失败收口在 `PlatformAlerter::notify`：它**没有返回值**，只记日志与计数。调用点因此拿不到错误，也就没有机会让一次外发失败变成对主流程的影响。载荷是 `PlatformAlert` 的四个字段——job、渠道类别、失败类别、观测时刻：凭证与对客内容根本不在这个结构里，因此不可能被顺手带出去。

同一次失败**最多外发一条**：前两个条件成立时不再去数连续失败——同一件事发两条逐字相同的告警没有信息量，聚合与静默期是接入方的事，但这里连重复都不产生。

## 备选方案

**连续失败计数放内存。** 落选：进程重启就归零，而"这条候选一直失败"恰恰在崩溃重启之后最需要；现算一次只是读最近 N 行终态。

**新建一张告警表或计数表。** 落选：那是第二份事实，会与 Job 的终态漂移，而漂移出来的数正好用来决定"要不要告警"。

**把 HTTP 发送放进 `crates/application`。** 落选：用例层不该认识 HTTP；端口与实现分开之后，"什么时候发、发什么"不必联外网就能测。

**告警走独立进程或队列，或按触发条件各发一条。** 落选：前者是告警平台与聚合的范围；后者让同一次失败产生重复报文，而重复报文只会让接入方的静默期更难写。

**在受理路径上同步外发。** 落选：会拖慢受理，而且那时还没有结果可报。

## 后果

失败路径上多一次"最近 N 行终态"的读，且**只在配了出口时**才做。告警在 Worker 的那一轮里等待（超时 ×（1 + 重试次数）为上限）；Job 的处置在此之前已经提交，所以等待只推迟下一条 Job 的领取，不影响本次的处置与对客结果。

过期租约回收建出的对账案例**不在触发范围内**：那条路径由仓储的 `recover_expired_leases` 直接建案，应用层只拿到计数，没有 job 与渠道身份可报。要让它们进来，得让那次回收带上受影响 Job 的身份。

## 验证

`cargo clippy --workspace --all-targets --all-features -- -D warnings` 与 `cargo test --workspace --all-features` 通过。端到端 `cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1` 在本地空库上 95 条全通过，其中 5 条覆盖本出口：平台欠费失败按最小集外发、载荷里没有提示词与凭证值；对账案例新增（这次失败的类别是渠道限流，不在欠费/凭证那一类里）同样外发；某候选连续失败只在到达配置的阈值那一次外发；接收器回 500 时，对客响应、Job 终态与对客错误码、预授权处置、结果信封、余额与账本与不配出口时逐位相同；没配 `PROVIDER_ALERT_WEBHOOK` 时接收器什么都收不到。接收器是进程内本地监听，零计费、零外网。
