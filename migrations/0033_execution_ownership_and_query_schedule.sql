-- v1 所有权续约/接管与对账查询调度（RFC 0017 §3、§5）。
--
-- 这是**扩展**一步：旧协议路径（Worker 领取、renew_lease/recover_expired_leases）原样保留，
-- 只给新协议（execution_protocol = 'v1'）加两条只认 v1 的部分索引，并给晚到事实收件与对账案例
-- 补上领取/调度列。所有新 v1 SQL 都显式过滤 execution_protocol = 'v1'，索引条件与过滤条件同形。

-- 1) 过期所有权扫描：只认 v1 的 executing/reconciliation_required。租约为空表示没有有效所有权，
--    与已过期同样可被接管；索引条件与扫描的硬过滤一致。
CREATE INDEX jobs_v1_takeover
    ON generation.jobs (lease_expires_at)
    WHERE execution_protocol = 'v1'
      AND state IN ('executing', 'reconciliation_required');

-- 2) 未提交孤儿回收：只认 v1 的 admitted，按受理年龄界领取。
CREATE INDEX jobs_v1_admitted
    ON generation.jobs (created_at)
    WHERE execution_protocol = 'v1' AND state = 'admitted';

-- 3) 晚到事实领取：claimed_by/claimed_at 只是领取标记，领取超时后可被另一领取者覆盖；只有消费成功
--    才写 consumed_at。已有的 late_facts_unconsumed（received_at，consumed_at IS NULL）继续支撑领取顺序。
ALTER TABLE generation.late_facts ADD COLUMN claimed_by text;
ALTER TABLE generation.late_facts ADD COLUMN claimed_at timestamptz;
COMMENT ON COLUMN generation.late_facts.claimed_by IS
    '当前领取者；领取超时后另一领取者可覆盖它，不表示事实已被消费';
COMMENT ON COLUMN generation.late_facts.claimed_at IS
    '本次领取时刻；未消费且领取超时的事实可被重新领取，消费成功才写 consumed_at';

-- 4) 对账查询调度：next_query_at 是下次允许只读查询的时刻，attempts 是已发起的查询次数
--    （退避与重试上限据此判定）。两者只服务异常对账 Worker，不参与任何对客金额。
ALTER TABLE operations.reconciliation_cases ADD COLUMN next_query_at timestamptz;
ALTER TABLE operations.reconciliation_cases ADD COLUMN attempts integer NOT NULL DEFAULT 0;
COMMENT ON COLUMN operations.reconciliation_cases.next_query_at IS
    '下次允许只读查询的时刻；为空表示还没排期';
COMMENT ON COLUMN operations.reconciliation_cases.attempts IS
    '已发起的只读查询次数；退避与重试上限据此判定';
