-- 收口：API 直接执行已是唯一图片路径，删掉旧协议（API 建 Job、Worker 领取执行、正文与结果信封
-- 落库）的载荷列、领取列与协议列（Spec 0005 §2/§6，RFC 0019 §4）。
--
-- 先守卫：库里还留着旧协议的未决执行或已收尾尝试时，**整条迁移失败**，由人决定重建开发库或先手工
-- 处置。守卫不改任何行——`submitting` 与 `reconciliation_required` 表示上游可能已受理，自动标失败
-- 等于平台单方认赔，而且旧路径删除后没有任何现存路径能收尾它们。
--
-- 发布侧的 `supply.offerings.carrier_schema` / `parameter_mapping` 是另一回事，不在这里删。

DO $$
DECLARE
    legacy_in_flight bigint;
    jobs_out_of_scope bigint;
    attempts_out_of_scope bigint;
    jobs_without_digest bigint;
BEGIN
    SELECT count(*) INTO legacy_in_flight
    FROM generation.jobs
    WHERE execution_protocol = 'legacy'
      AND state IN ('accepted', 'leased', 'submitting', 'reconciliation_required');

    -- 收窄后的 CHECK 只认 v1 五个阶段：任何其它取值（含终态的 canceled）都会让
    -- ADD CONSTRAINT 以裸错失败，这里先拦下来给一条能读懂的报错。
    SELECT count(*) INTO jobs_out_of_scope
    FROM generation.jobs
    WHERE state NOT IN ('admitted', 'executing', 'succeeded', 'failed', 'reconciliation_required');

    SELECT count(*) INTO attempts_out_of_scope
    FROM generation.attempts
    WHERE state NOT IN ('prepared', 'submitting', 'accepted', 'terminal', 'unknown');

    -- 收口后 idempotency_key_digest 是唯一的防重依据且非空：没有它的旧行留着就收不了口。
    SELECT count(*) INTO jobs_without_digest
    FROM generation.jobs
    WHERE idempotency_key_digest IS NULL;

    IF legacy_in_flight > 0 THEN
        RAISE EXCEPTION
            'migration 0038 refuses to run: % legacy job(s) are still in flight (accepted/leased/submitting/reconciliation_required); rebuild the development database or dispose of them by hand',
            legacy_in_flight;
    END IF;
    IF jobs_out_of_scope > 0 THEN
        RAISE EXCEPTION
            'migration 0038 refuses to run: % job(s) carry a state outside the v1 stages (admitted/executing/succeeded/failed/reconciliation_required); rebuild the development database or dispose of them by hand',
            jobs_out_of_scope;
    END IF;
    IF attempts_out_of_scope > 0 THEN
        RAISE EXCEPTION
            'migration 0038 refuses to run: % attempt(s) carry a state outside the v1 stages (prepared/submitting/accepted/terminal/unknown); rebuild the development database or dispose of them by hand',
            attempts_out_of_scope;
    END IF;
    IF jobs_without_digest > 0 THEN
        RAISE EXCEPTION
            'migration 0038 refuses to run: % job(s) have no idempotency_key_digest, which becomes the only dedup fact and is NOT NULL after this migration',
            jobs_without_digest;
    END IF;
END $$;

-- 产出张数的最小落点（设计 0019 §5 第 3 条）：结算时写入，用量明细与账单汇总读它。拿不到张数时留
-- NULL，读取按 0，不用请求的 n 或 token 数顶替。
ALTER TABLE generation.jobs ADD COLUMN image_count integer;

COMMENT ON COLUMN generation.jobs.image_count IS
    '本次执行实际产出的图片张数；结算时按内存里的实际张数写入，缺失为 NULL、读取按 0';

-- 旧协议的载荷与领取列：不再有任何写者或读者。
-- lease_expires_at 保留：执行所有权续约写它、接管候选按它筛选。
ALTER TABLE generation.jobs
    DROP COLUMN native_parameters,
    DROP COLUMN result_images,
    DROP COLUMN carrier_schema,
    DROP COLUMN parameter_mapping,
    DROP COLUMN idempotency_key,
    DROP COLUMN request_hash,
    DROP COLUMN lease_owner,
    DROP COLUMN next_attempt_at,
    DROP COLUMN version,
    DROP COLUMN execution_protocol;

-- 旧 Worker 的领取索引随 next_attempt_at 一起不可用；显式删除让这一步在迁移里看得见。
DROP INDEX IF EXISTS generation.jobs_claimable;

-- 两条收口索引去掉协议条件后保留：删列会连带删掉它们，这里按新条件重建。
CREATE INDEX jobs_v1_takeover
    ON generation.jobs (lease_expires_at)
    WHERE state IN ('executing', 'reconciliation_required');

CREATE INDEX jobs_v1_admitted
    ON generation.jobs (created_at)
    WHERE state = 'admitted';

-- 明文幂等键删除后，摘要就是唯一的防重依据，必须无条件必填。
ALTER TABLE generation.jobs ALTER COLUMN idempotency_key_digest SET NOT NULL;

COMMENT ON COLUMN generation.jobs.idempotency_key_digest IS
    '幂等键的不可逆标识（无密钥 SHA-256，固定领域前缀 || 幂等键）：受理必写，是唯一的防重依据';

-- 阶段面收窄到存活路径真正写的取值：Job 是 admitted → executing → 终态；
-- Attempt 是 prepared/submitting/accepted/terminal/unknown。
ALTER TABLE generation.jobs DROP CONSTRAINT jobs_state_check;
ALTER TABLE generation.jobs ADD CONSTRAINT jobs_state_check CHECK (state IN (
    'admitted', 'executing', 'succeeded', 'failed', 'reconciliation_required'));

ALTER TABLE generation.attempts DROP CONSTRAINT attempts_state_check;
ALTER TABLE generation.attempts ADD CONSTRAINT attempts_state_check CHECK (state IN (
    'prepared', 'submitting', 'accepted', 'terminal', 'unknown'));
