-- 平台侧失败类别 + 对客错误码白名单
--
-- 两个目的：
--   1) 消费者在 Job 上看到的 error_code 只能是平台自己的三个码之一；渠道的 HTTP 状态码、
--      错误码与原文只留在 attempts 与管理端视图里（渠道原始码在 attempts 上完整保留）。
--   2) failure_kind 记录平台侧失败类别，供运营发现"平台在渠道侧欠费/凭证配置问题"这类事件。

ALTER TABLE generation.jobs ADD COLUMN failure_kind text;

-- 历史行归一：老值可能是渠道码或平台内部码，按当时的状态收敛到对客白名单。
-- 状态是不明结果的一律 outcome_unknown；其余按平台侧故障。渠道原始码不丢——它仍在
-- attempts.provider_error_code 上。
UPDATE generation.jobs
SET error_code = CASE
        WHEN state = 'reconciliation_required' THEN 'outcome_unknown'
        ELSE 'platform_unavailable'
    END
WHERE error_code IS NOT NULL;

ALTER TABLE generation.jobs
    ADD CONSTRAINT jobs_error_code_is_public
    CHECK (
        error_code IS NULL
        OR error_code IN ('platform_unavailable', 'outcome_unknown', 'content_rejected')
    );

-- NULL 表示历史行未记录类别；新写入的每个失败路径都必须给值。
ALTER TABLE generation.jobs
    ADD CONSTRAINT jobs_failure_kind_known
    CHECK (
        failure_kind IS NULL
        OR failure_kind IN (
            'platform_funding',
            'platform_credential',
            'platform_internal',
            'upstream_rejected',
            'upstream_unavailable',
            'upstream_rate_limited',
            'consumer_content',
            'unknown'
        )
    );

CREATE INDEX jobs_platform_failures
    ON generation.jobs (failure_kind, updated_at DESC)
    WHERE failure_kind IS NOT NULL;
