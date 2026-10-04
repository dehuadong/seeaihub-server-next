---
title: 账户余额缓存：带版本的快照与数据库最终判定
status: implemented
created: 2026-09-30
updated: 2026-10-04
approval: 用户授权按 Spec v3 §4 与 RFC v3 §3 实施余额快照的版本闸门与预检口径调整
verification: 见正文「验证」。本地通过 `cargo fmt --check`、`cargo test -p seeai-application -p seeai-persistence`，以及合同套件 `HTTP_CONTRACT_DATABASE_URL=postgres://seeai:seeai@127.0.0.1:5432/seeai_contract cargo test -p seeai-api --test http_contract -- --ignored cases_cache`（本地 PG + 进程内假 Redis）。
---

# Agent Note：账户余额缓存：带版本的快照与数据库最终判定

## 问题

账户当前值改为 `ledger.accounts` 的 `balance_microusd` / `held_microusd` / 单调递增 `version` 之后，`user_balance` 缓存里的旧值只有余额、写入时间与来源三项。两个后果：缓存表达不了"可用额 = 余额 − 占用"，预检只能拿已结算余额与保底额比；两个事务提交后的异步写回可能乱序到达，旧快照覆盖新快照。RFC v3 §3（[`0013` §3](../../../../docs/design/0013-account-funds-and-reservations.md)）要求缓存值带版本、写回拒绝倒序，且**缓存不足不得单独产生 402**——受理闸门只能是数据库那条条件更新。

本记录覆盖余额快照的版本闸门与"缓存不决定资金结果"的口径；route 条目清理、写穿时机、对账与降级由 [2026-09-22 的加速层记录](./2026-09-22-redis-acceleration-layer.md) 承接，两条互相链接。

## 决定

- **快照带版本**：`user_balance:<account_id>` 的值是 `{balance_microusd, held_microusd, available_microusd, version, written_at, source}`。三个金额与版本同属一次账户读取，`available = balance − held` 在缓存里同样成立。
- **版本闸门**：`write_balance` 先读缓存当前版本，只有版本**不低于**它的快照才写。数据库对同一账户的金额更新串行，版本随每次变更递增；两个提交后的异步写回乱序到达时，旧快照被挡下。读不到当前值（键不存在、缓存不可用或值不可读）时照写——数据库是权威；写失败只记日志，不回滚数据库。
- **缓存不决定资金结果**：余额判定一律由受理时的数据库条件更新（`balance − held ≥ 保底额` 且 `kind = consumer`）确认；缓存快照只在写穿与对账之间流转，不参与 402。
- **对账按版本校正**：`reconcile_once` 读数据库当前行；缓存版本高于这次读数时不动它（那次读发生在新提交之前），版本与三个金额都相同才算同一个快照，版本相同而金额不同按缓存被改坏覆盖并写 `cache.balance_corrected` 审计。

## 备选方案

- **写回闸门用 Redis 原子 CAS（Lua 脚本）vs 应用层读-比较-写**：用读-比较-写。`CacheStore` 只有 `GET` / `SET` / `DEL` 三条命令，缓存语义（值长什么样）留在用例层；让实现方解析 JSON 里的 `version` 会把"余额"带进缓存实现。代价是检查与写入不是一条原子命令：挡得住"提交更早、写回更晚"的倒序，挡不住两条写回真正同时执行且都读到同一旧版本的窄窗口；后者由数据库串行提交与下一轮对账兜底。
- **保留"新鲜且不足即拒" vs 交给数据库条件更新**：交给数据库条件更新。让缓存单独决定资金结果会把"账实不符"读成"账实相符"；Spec v3 §4 与 RFC v3 §3 明确 402 必须由数据库确认。
- **对账无条件覆盖 vs 按版本校正**：按版本校正。对账读到的行可能早于缓存里刚写回的新快照，无条件覆盖就是另一种倒序。

## 后果

- 受理路径没有"凭缓存拒绝"的审计；余额判定一律由数据库条件更新确认。
- 每次写回多一次缓存读；单次缓存操作超时上界不变。
- 缓存里一条被伪造成高版本的快照不会被对账降级，直到 TTL 到期或下一次版本更高的写回。

## 验证

- `cargo fmt --check` 通过；`cargo test -p seeai-application -p seeai-persistence` 通过（application 141、persistence 7）。
- 合同套件 `HTTP_CONTRACT_DATABASE_URL=postgres://seeai:seeai@127.0.0.1:5432/seeai_contract cargo test -p seeai-api --test http_contract -- --ignored cases_cache`（本地 PG + 进程内假 Redis）覆盖：写穿后缓存带三个金额与版本；新鲜缓存不足仍由数据库受理并结算；缓存显示充足而数据库不足时由数据库回 402；缓存版本更高时低版本写回被挡下；以及缓存不可用、过期、对账校正与重放的既有用例。
