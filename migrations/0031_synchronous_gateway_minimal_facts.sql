-- 同步网关最小事实与容量（Spec 0005 §2–§6，RFC 0017 §3/§5/§7）。
--
-- 这是**扩展**一步：旧协议路径（API 建 Job、Worker 领取执行、正文与结果信封落库）原样保留，
-- 只把同一批执行/财务记录扩展出协议版本与最小事实列，并让旧正文列对新协议可为空。
-- 新协议记录不写业务载荷；旧正文列要到切换切片（S5）才删除。
--
-- 新协议 Job 阶段 admitted → executing → succeeded/failed/reconciliation_required；
-- Attempt 阶段 prepared/submitting/accepted/terminal/unknown。

-- 1) 协议版本
ALTER TABLE generation.jobs ADD COLUMN execution_protocol text NOT NULL DEFAULT 'legacy';
ALTER TABLE generation.jobs ADD CONSTRAINT jobs_execution_protocol_known
    CHECK (execution_protocol IN ('legacy', 'v1'));
COMMENT ON COLUMN generation.jobs.execution_protocol IS
    '执行协议版本：legacy = 切换前 Worker 队列，v1 = API 直接执行（RFC 0017 §5）';

-- 2) 新协议阶段进入同一 state 列；旧取值不变，旧可领取索引只认 accepted
ALTER TABLE generation.jobs DROP CONSTRAINT jobs_state_check;
ALTER TABLE generation.jobs ADD CONSTRAINT jobs_state_check CHECK (state IN (
    'accepted', 'leased', 'submitting', 'succeeded', 'failed',
    'reconciliation_required', 'canceled', 'admitted', 'executing'));
ALTER TABLE generation.attempts DROP CONSTRAINT attempts_state_check;
ALTER TABLE generation.attempts ADD CONSTRAINT attempts_state_check CHECK (state IN (
    'submitting', 'succeeded', 'failed', 'reconciliation_required',
    'prepared', 'accepted', 'terminal', 'unknown'));

-- 3) 幂等键与请求指纹摘要：新协议不再保存明文键；lookup 摘要跨协议共用，请求指纹带版本
ALTER TABLE generation.jobs ADD COLUMN idempotency_key_digest text;
ALTER TABLE generation.jobs ADD COLUMN idempotency_lookup_key_version smallint;
ALTER TABLE generation.jobs ADD COLUMN request_digest text;
ALTER TABLE generation.jobs ADD COLUMN request_digest_key_version smallint;
CREATE UNIQUE INDEX jobs_idempotency_digest_key
    ON generation.jobs (account_id, idempotency_key_digest)
    WHERE idempotency_key_digest IS NOT NULL;
COMMENT ON COLUMN generation.jobs.idempotency_key_digest IS
    '幂等键的不可逆标识（稳定 lookup 密钥版本）；旧协议记录此列为空，明文键仍供其使用';

-- 4) 执行所有权与 fencing：提交与收尾核验，接管时在数据库里比较并交换
ALTER TABLE generation.jobs ADD COLUMN execution_owner text;
ALTER TABLE generation.jobs ADD COLUMN fencing_token bigint NOT NULL DEFAULT 0;
ALTER TABLE generation.jobs ADD COLUMN provider_task_handle text;
COMMENT ON COLUMN generation.jobs.provider_task_handle IS
    '上游任务句柄（如 APIMart task id）；只为对账查询保留，不含结果访问地址';

-- 5) 旧正文列对新协议可为空；旧协议记录一律仍有值
ALTER TABLE generation.jobs ALTER COLUMN idempotency_key DROP NOT NULL;
ALTER TABLE generation.jobs ALTER COLUMN request_hash DROP NOT NULL;
ALTER TABLE generation.jobs ALTER COLUMN native_parameters DROP NOT NULL;
ALTER TABLE generation.jobs ALTER COLUMN carrier_schema DROP NOT NULL;

-- 6) 渠道全局未决任务容量事实：唯一关联 Job，与受理同事务获取，获取与释放幂等。
--    租约过期不证明上游结束；未知已提交任务保留槽位，直到确定终态或可信人工处置（RFC 0017 §6）。
CREATE TABLE generation.execution_capacity (
    id uuid PRIMARY KEY,
    job_id uuid NOT NULL REFERENCES generation.jobs(id),
    channel_id uuid NOT NULL REFERENCES supply.channels(id),
    state text NOT NULL DEFAULT 'held',
    acquired_at timestamptz NOT NULL DEFAULT now(),
    released_at timestamptz,
    UNIQUE (job_id),
    CONSTRAINT execution_capacity_state_known CHECK (state IN ('held', 'released')),
    CONSTRAINT execution_capacity_released_shape CHECK (
        (state = 'held' AND released_at IS NULL)
        OR (state = 'released' AND released_at IS NOT NULL))
);
CREATE INDEX execution_capacity_channel_held
    ON generation.execution_capacity (channel_id) WHERE state = 'held';
COMMENT ON TABLE generation.execution_capacity IS
    '渠道全局未决任务槽位：账户执行名额沿用既有在飞计数，不在此表另记一行';
