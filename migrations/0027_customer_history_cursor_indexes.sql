-- 客户历史翻页的索引：按 (账户, 时刻, id) 覆盖游标定位的排序键
-- （docs/design/0014-customer-console-navigation-and-history.md §3）。
--
-- 0025 建的三条索引都少了定序键 `id`：同一时刻并列时（同一个事务写入的多条共用 `now()`）翻页要在
-- id 上继续比较，缺了它数据库只能在内存里补排序，页越深越贵。已应用的迁移不改写（0013 §6），所以这里
-- DROP + CREATE，免得留下两份同用途索引。
DROP INDEX IF EXISTS generation.jobs_account_terminal_at;
CREATE INDEX jobs_account_terminal_at
    ON generation.jobs (account_id, terminal_at, id)
    WHERE terminal_at IS NOT NULL;

DROP INDEX IF EXISTS generation.jobs_account_created_at;
CREATE INDEX jobs_account_created_at
    ON generation.jobs (account_id, created_at, id);

DROP INDEX IF EXISTS ledger.entries_account_created_at;
CREATE INDEX entries_account_created_at
    ON ledger.entries (account_id, created_at, id);
