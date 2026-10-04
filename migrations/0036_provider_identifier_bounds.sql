-- Provider 标识的存储层约束（Spec 0005 §2、RFC 0018 §6）。写入侧已经有类型与边界校验：
-- 任务句柄是只能经校验构造的有界类型，各写入路径也过滤 trace。这里把同一条不变量落到数据库，
-- 兜住任何绕过应用的写入。历史非法值先清成 NULL——句柄清空会让相关执行按"没有可查询句柄"进对账，
-- 而不是拿一段正文或 URL 去拼上游查询。
--
-- 只清字段，不删行：最小幂等、账本与执行记录本身全部保留。

DO $$
DECLARE
    jobs_cleared bigint;
    traces_cleared bigint;
    facts_cleared bigint;
BEGIN
    UPDATE generation.jobs
    SET provider_task_handle = NULL
    WHERE provider_task_handle IS NOT NULL
      AND (length(provider_task_handle) NOT BETWEEN 1 AND 128
           OR provider_task_handle !~ '^[A-Za-z0-9._:-]+$');
    GET DIAGNOSTICS jobs_cleared = ROW_COUNT;

    UPDATE generation.attempts
    SET provider_trace_id = NULL
    WHERE provider_trace_id IS NOT NULL
      AND (length(provider_trace_id) NOT BETWEEN 1 AND 128
           OR provider_trace_id !~ '^[A-Za-z0-9._:-]+$');
    GET DIAGNOSTICS traces_cleared = ROW_COUNT;

    -- 收件箱里带句柄的行：句柄非法时整行都不可用（形状约束要求 task_handle 行必须有句柄），
    -- 删除这些行；带非法 trace 的行只清 trace。
    DELETE FROM generation.late_facts
    WHERE kind = 'task_handle'
      AND (length(provider_task_handle) NOT BETWEEN 1 AND 128
           OR provider_task_handle !~ '^[A-Za-z0-9._:-]+$');
    GET DIAGNOSTICS facts_cleared = ROW_COUNT;

    UPDATE generation.late_facts
    SET provider_trace_id = NULL
    WHERE provider_trace_id IS NOT NULL
      AND (length(provider_trace_id) NOT BETWEEN 1 AND 128
           OR provider_trace_id !~ '^[A-Za-z0-9._:-]+$');

    RAISE NOTICE 'provider identifier cleanup: % job handle(s), % attempt trace(s), % late fact row(s)',
        jobs_cleared, traces_cleared, facts_cleared;
END $$;

ALTER TABLE generation.jobs
    ADD CONSTRAINT jobs_provider_task_handle_bounded CHECK (
        provider_task_handle IS NULL
        OR (length(provider_task_handle) BETWEEN 1 AND 128
            AND provider_task_handle ~ '^[A-Za-z0-9._:-]+$'));

ALTER TABLE generation.attempts
    ADD CONSTRAINT attempts_provider_trace_bounded CHECK (
        provider_trace_id IS NULL
        OR (length(provider_trace_id) BETWEEN 1 AND 128
            AND provider_trace_id ~ '^[A-Za-z0-9._:-]+$'));

ALTER TABLE generation.late_facts
    ADD CONSTRAINT late_facts_provider_task_handle_bounded CHECK (
        provider_task_handle IS NULL
        OR (length(provider_task_handle) BETWEEN 1 AND 128
            AND provider_task_handle ~ '^[A-Za-z0-9._:-]+$'));

ALTER TABLE generation.late_facts
    ADD CONSTRAINT late_facts_provider_trace_bounded CHECK (
        provider_trace_id IS NULL
        OR (length(provider_trace_id) BETWEEN 1 AND 128
            AND provider_trace_id ~ '^[A-Za-z0-9._:-]+$'));
