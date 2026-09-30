---
title: 每日消费合计与跨天账单归属
status: implemented
created: 2026-09-30
updated: 2026-09-30
approval: 用户授权按 Spec v3 §5 与 RFC v4 §4–§5 实施每日消费合计、Job 终态时刻与跨天用量账单归属
verification: 见正文「验证」。本地通过 `cargo fmt --check`、`cargo test -p seeai-application -p seeai-persistence`，以及合同套件 `HTTP_CONTRACT_DATABASE_URL=postgres://seeai:seeai@127.0.0.1:5432/seeai_contract cargo test -p seeai-api --test http_contract -- --ignored cases_cost_facts cases_billing cases_public_surface`（本地 PG + 进程内假 Redis）。
---

# Agent Note：每日消费合计与跨天账单归属

## 问题

受理路径此前每次按账户和当天汇总 `ledger.entries` 的 `capture`：请求耗时随历史流水条数增长，与"受理只读当天一行"的合同（[`0013` §4](../../../../docs/design/0013-account-funds-and-reservations.md)）不符。用量与账单都按 Job 的受理时刻归属，跨 UTC 日结算的扣费被投进受理日；逐笔扣费又按流水入账时刻过滤，与账单的扣费口径分叉。

## 决定

合同（合计记什么、归属规则、限额语义）归 [`0013` §4–§5](../../../../docs/design/0013-account-funds-and-reservations.md)；这里只记本次实现选定的机制。

- `ledger.daily_spend` 主键 `(account_id, day)`，以正数记已完成实收；结算在写 `capture` 的同一事务按 `(now() AT TIME ZONE 'UTC')::date` upsert 累加，零实收不写；受理只读当天一行、缺行视为 0。
- `generation.jobs.terminal_at` 的落点：`succeeded` 在 `complete_job`、`failed` 在 `fail_job`、人工解除对账在 `refund_reconciliation`，各与自己那次状态变更同事务；`canceled` 当前没有写入方，`reconciliation_required` 不是领域终态（`JobState::is_terminal`），留空。
- 用量按 `terminal_at IS NOT NULL` 与否分两支（不用 `COALESCE`，否则两个索引都用不上）；逐笔扣费改成该 Job 的 `capture` 标量子查询，不再带流水时间过滤；对客与管理员用量响应都新增 `terminal_at`。
- 账单的请求数与张数按 `terminal_at` 归属，`charged_microusd` 仍按 `capture` / `adjustment` 的入账时刻求和。
- 索引：`jobs (account_id, terminal_at) WHERE terminal_at IS NOT NULL`、`jobs (account_id, created_at)`、`entries (account_id, created_at)`、`entries (job_id) WHERE kind = 'capture'`。

## 备选方案

- **继续按历史流水求和（加账户索引）vs 每账户每日合计表**：选每日合计表。求和把受理成本绑在流水条数上，也与"只读当天一行、不扫历史流水"的合同不符。
- **用量过滤写成 `COALESCE(terminal_at, created_at)` 单表达式 vs 两个分支的 OR**：选 OR。执行计划实测两个分支各自走 `jobs_account_terminal_at` 与 `jobs_account_created_at` 的 BitmapOr；`COALESCE` 会让两个索引都用不上。
- **逐笔扣费保留按流水入账时刻过滤的 JOIN vs 改成该 Job 的标量子查询**：选标量子查询。跨天结算的扣费只由 Job 的终态时刻归属，再用流水时间筛同一笔会把已归属的扣费筛掉。
- **`terminal_at` 也对 `reconciliation_required` 落时刻 vs 只对领域终态落**：选只对领域终态落。对账态不是终态，它转成 `failed` 时才落终态时刻；对账中那笔在用量里仍按受理时刻显示，也不计账单请求数。

## 后果

- 本次不做历史回填（[`0002` §6](../../../../docs/specs/0002-account-funds-and-reservations.md)）：既有 Job 的 `terminal_at` 为空、既有 `capture` 不进 `daily_spend`。这些账户当天的限额从 0 重新计起，直到新的结算累加；历史 Job 在用量里按受理时刻显示，扣费仍来自它自己的 `capture`。
- 每日合计只由结算写入，受理与余额读取不碰它；账实核查仍按账户核对 `balance = SUM(entries)` 与 `held = SUM(active holds)`，与每日合计无关。
- 新增一张表与四个索引；受理路径从"按流水聚合"改为一次主键点查。

## 验证

- `cargo fmt --check` 通过。
- `cargo test -p seeai-application -p seeai-persistence`：application 141、persistence 7，全通过。
- 合同套件（本地 PG `seeai_contract` + 进程内假 Redis）：`cases_cost_facts`、`cases_billing`、`cases_public_surface` 共 23 passed / 0 failed。改写 `a_reached_daily_spend_cap_rejects_new_requests_with_its_own_code`，补一笔只存在于历史流水的当天扣费后仍受理、把当天合计顶到额度才 429；新增 `a_settlement_after_midnight_is_billed_on_the_day_it_settled` 钉住成功结算的 `terminal_at` 与 `capture.created_at` 同事务、跨 UTC 日按结算日归属用量与账单；新增 `usage_charges_match_the_capture_entries_and_adjustments_stay_separate` 钉住已完成用量逐笔扣费等于 `capture` 求和、正式调整单独列示后账单净额等于扣费加调整。`cases_identity`、`cases_pricing` 共 29 passed / 0 failed。
- 索引按实际执行计划核定：在临时库 `seeai_explain` 上应用完整迁移链并灌入 2 万 Job、1 万 capture、1.2 万日合计行后 `EXPLAIN (ANALYZE)`——用量走两个 jobs 索引的 BitmapOr 与 `entries_capture_job`，账单请求数走 `jobs_account_terminal_at` Index Only Scan、扣费走 `entries_account_created_at`，当日合计走 `daily_spend_pkey`。
- 全量 Rust 门禁与 e2e 未在本地重跑：前者按仓库约定交 CI，后者本次不动前端。
