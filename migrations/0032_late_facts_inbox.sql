-- 晚到事实收件箱（RFC 0017 §3）：原提交者在执行 token 失效后仍可能收到上游的 task handle 或
-- 计量/成本事实。它不得改所有权、重开终态或直接结算，只把有界事实落成收件行，由当前收尾者
-- 领取后按现有端口处理。只保存 Spec 0005 §2 允许的最小事实，不含渠道正文、结果图片或请求参数。
CREATE TABLE generation.late_facts (
    id uuid PRIMARY KEY,
    job_id uuid NOT NULL REFERENCES generation.jobs(id),
    attempt_id uuid NOT NULL REFERENCES generation.attempts(id),
    kind text NOT NULL,
    content_digest text NOT NULL,
    provider_task_handle text,
    provider_trace_id text,
    metering_evidence jsonb,
    provider_cost_microusd bigint,
    provider_cost_currency text,
    provider_cost_source text,
    provider_cost_cny_microusd bigint,
    received_at timestamptz NOT NULL DEFAULT now(),
    consumed_at timestamptz,
    CONSTRAINT late_facts_kind_known CHECK (kind IN ('task_handle', 'accounting')),
    CONSTRAINT late_facts_shape CHECK (
        (kind = 'task_handle' AND provider_task_handle IS NOT NULL)
        OR (kind = 'accounting' AND (metering_evidence IS NOT NULL OR provider_cost_source IS NOT NULL))),
    CONSTRAINT late_facts_cost_source_known CHECK (
        provider_cost_source IS NULL
        OR provider_cost_source IN ('computed', 'declared', 'unavailable'))
);
CREATE UNIQUE INDEX late_facts_attempt_kind_digest
    ON generation.late_facts (attempt_id, kind, content_digest);
CREATE INDEX late_facts_unconsumed
    ON generation.late_facts (received_at) WHERE consumed_at IS NULL;
COMMENT ON TABLE generation.late_facts IS
    '晚到事实收件箱：只写最小收件行；当前收尾者领取后按 settle/fail_or_reconcile 处理（RFC 0017 §3）';
