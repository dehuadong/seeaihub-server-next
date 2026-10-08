-- 删掉每日消费合计：它是"每日扣费上限"这条受理判定的判据，而那条判定已整段删除。
--
-- 按日汇总不丢事实：同一区间的已完成实收可以按 `ledger.entries` 的 `capture` 与 Job 的终态时刻
-- 重建（docs/design/0013-account-funds-and-reservations.md §4）。
DROP TABLE ledger.daily_spend;
