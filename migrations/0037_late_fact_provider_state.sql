-- 晚到账务事实必须带 Provider 终态（Spec 0005 §5、RFC 0018 §7）：收件只能说明"上游给过什么"，
-- 不能凭"有 usage"推断成功。没有成功终态就没有向消费者收费的依据——保留占用并进对账。
ALTER TABLE generation.late_facts
    ADD COLUMN provider_state text;

ALTER TABLE generation.late_facts
    ADD CONSTRAINT late_facts_provider_state_known CHECK (
        provider_state IS NULL
        OR provider_state IN ('pending', 'succeeded', 'failed', 'cancelled', 'unknown'));

COMMENT ON COLUMN generation.late_facts.provider_state IS
    '上游任务的终态快照；只有 succeeded 允许按证据结算，缺失或未知一律保留占用转对账';
