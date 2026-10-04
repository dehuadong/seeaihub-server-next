-- 删除查找摘要的密钥版本列（RFC 0017 §2 修订，Spec 0005 §2/§4）。
--
-- 幂等键查找摘要改为无密钥 SHA-256（固定领域前缀 || 幂等键）：它只要求稳定、不可逆、
-- 跨 API 副本一致，不依赖任何密钥，因此没有版本可记。请求指纹密钥仍按版本轮换，
-- request_digest_key_version 保留。旧协议记录本来就不写这一列，删除不影响旧路径。

ALTER TABLE generation.jobs DROP COLUMN idempotency_lookup_key_version;
COMMENT ON COLUMN generation.jobs.idempotency_key_digest IS
    '幂等键的不可逆标识（无密钥 SHA-256，固定领域前缀 || 幂等键）；旧协议记录此列为空，明文键仍供其使用';
