-- 并发名额改成必填：进程配置不再是名额来源（工作项 #99）。
--
-- 存量行可能是 NULL（0048 的口径是"NULL＝用部署缺省"），先回填**平台固定值 1**，再把列设成
-- NOT NULL，最后**重写列注释**——0048 的注释写着"NULL＝用部署缺省"，与新状态矛盾，而 0048 已应用、
-- 按校验和规则不许就地改。
--
-- 切换顺序见 `docs/operations/deployment.md` §3.3：要保留更大并发的部署，先在升级**之前**用
-- `PATCH /api/v1/gateway-models/{model}` 把现值逐个钉到模型上，再升级；这条迁移一跑，回填对所有
-- 副本当场生效，没有"升级后再设回去"的窗口。
UPDATE publication.gateway_models
SET max_concurrent_jobs = 1
WHERE max_concurrent_jobs IS NULL;

ALTER TABLE publication.gateway_models
    ALTER COLUMN max_concurrent_jobs SET NOT NULL;

COMMENT ON COLUMN publication.gateway_models.max_concurrent_jobs IS
    '每个账户在该网关模型上同时在跑的生成任务上限；必填，由运营在发布或编辑时给（发布命令没给时新建的模型按平台固定值 1）';
