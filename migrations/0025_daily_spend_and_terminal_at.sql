-- 每日消费合计与 Job 终态时刻：把"今天花了多少"从逐次汇总历史流水改成每账户每 UTC 自然日一行，
-- 并让已完成请求按终态时刻归属（docs/design/0013-account-funds-and-reservations.md §4、§5）。
--
-- 合计以**正数记实收**，只由成功结算在写 `capture` 的同一事务累加；受理只读当天一行，不扫历史流水。
-- 本次不承担历史回填（`0013` §6）：既有 Job 的 `terminal_at` 留空，既有流水不折算成每日合计。
CREATE TABLE ledger.daily_spend (
    account_id uuid NOT NULL REFERENCES ledger.accounts(id),
    day date NOT NULL,
    settled_microusd bigint NOT NULL DEFAULT 0 CHECK (settled_microusd >= 0),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (account_id, day)
);

COMMENT ON TABLE ledger.daily_spend IS
    '每账户每 UTC 自然日的已完成实收合计（正数记实收）；受理只读当天一行（0013 §4）';
COMMENT ON COLUMN ledger.daily_spend.day IS
    'UTC 自然日：结算事务里由数据库时钟取 now() 折算，与 capture.created_at 同一时刻（0013 §4）';

-- Job 的终态时刻：成功结算与 capture.created_at 由同一事务确定，已完成请求按它归属区间（0013 §5）。
ALTER TABLE generation.jobs ADD COLUMN terminal_at timestamptz;
COMMENT ON COLUMN generation.jobs.terminal_at IS
    '终态时刻（succeeded / failed / canceled）；对账态不是终态，留空（0013 §5）';

-- 索引服务查询，不改变金额规则（0013 §5）。用量与账单的已完成分支按账户 + 终态时刻；
-- 处理中分支按账户 + 受理时刻；逐笔扣费按 Job 关联 capture。
CREATE INDEX jobs_account_terminal_at
    ON generation.jobs (account_id, terminal_at) WHERE terminal_at IS NOT NULL;
CREATE INDEX jobs_account_created_at
    ON generation.jobs (account_id, created_at);
CREATE INDEX entries_account_created_at
    ON ledger.entries (account_id, created_at);
CREATE INDEX entries_capture_job
    ON ledger.entries (job_id) WHERE kind = 'capture';
