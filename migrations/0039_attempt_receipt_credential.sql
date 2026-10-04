-- Attempt 收件凭据（Spec 0005 §2、§5；RFC 0018 §5.2）。
--
-- `begin_submission` 在写下提交声明的事务里原子生成与该 Attempt 绑定的随机收件凭据，数据库只保存
-- 它的摘要：执行上下文持有原值，晚到事实投递时用它证明"这次投递来自创建该 Attempt 的原提交者"，
-- 因此允许原 fencing token 已经过期。它只决定收不收件，不授权正式结算、不改所有权、不重开终态。
--
-- 升级前已在飞的 Attempt 此列为 NULL：没有凭据就不收它名下的晚到事实，由既有对账处置，
-- 不伪造身份。凭据不是环境变量，也不进日志。
ALTER TABLE generation.attempts ADD COLUMN receipt_credential_digest text;

ALTER TABLE generation.attempts
    ADD CONSTRAINT attempts_receipt_credential_digest_shape CHECK (
        receipt_credential_digest IS NULL
        OR receipt_credential_digest ~ '^[0-9a-f]{64}$');

COMMENT ON COLUMN generation.attempts.receipt_credential_digest IS
    '收件凭据的无密钥 SHA-256 摘要（十六进制，绑定 Attempt/Job 渠道）；只用于晚到事实收件校验，原值不落库';
