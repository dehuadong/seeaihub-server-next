-- 平台网关模型名与厂商原生名真正拆开：命名两列 + 运维开关表。
--
-- 1) publication.runtime_revisions 增两列：
--    - gateway_model：这次发布定义的是**哪个网关模型**（对客名）；
--    - vendor_model_id：它指向**哪一行**厂商模型合同。
--    这两样本来只能从这次发布写下的 runtime_entries 反推；落在修订上之后，"这次发布
--    定义的是哪个名字、挂的是哪份合同"一眼可读，不必反推。既有行按同一次发布写下的
--    条目回填（同一次发布的所有条目同值，取任意一条即可），再设 NOT NULL。
-- 2) publication.gateway_models：**只放运维开关**（这个名字现在开着吗、谁在什么时候改的）。
--    定义（候选集、合同、定价）不进这张表——定义只在不可变修订里，存第二份就等于造第二个权威。
--    按既有生效名字回填出对应行（enabled = true），使现有已发布数据在迁移后立刻可读、可停用。
--
-- 不做的两件事：
--   定价列（加价系数、渠道成本、对客费率、保底表等）不在本轮，它们随定价切片一起落；
--   历史修订的 runtime_entries.gateway_model **不回填**：已发布的名字等于当时的厂商原生名，
--   语义自洽（"平台型号名恰好等于厂商原生名"是合法取值），改历史行会破坏
--   "Job 固定受理时版本"——旧 Job 事后读到的必须与它受理时逐字相同。

ALTER TABLE publication.runtime_revisions ADD COLUMN gateway_model text;
ALTER TABLE publication.runtime_revisions ADD COLUMN vendor_model_id uuid;

-- 同一次发布写下的条目同值，所以按修订取一条即可。取哪一条要确定：先按优先级、
-- 再按 offering 排序，结果可复现。条目只被置 active = false、从不删除，因此每个修订
-- 都取得到；取不到说明库被人为改过，下面显式报错，不悄悄写一个假名字。
UPDATE publication.runtime_revisions rr
SET gateway_model = entries.gateway_model,
    vendor_model_id = entries.vendor_model_id
FROM (
    SELECT DISTINCT ON (re.runtime_revision_id)
        re.runtime_revision_id, re.gateway_model, re.vendor_model_id
    FROM publication.runtime_entries re
    ORDER BY re.runtime_revision_id, re.routing_priority ASC, re.offering_id ASC
) AS entries
WHERE entries.runtime_revision_id = rr.id;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM publication.runtime_revisions
        WHERE gateway_model IS NULL OR vendor_model_id IS NULL
    ) THEN
        RAISE EXCEPTION
            'runtime revision without entries cannot be attributed to a gateway model';
    END IF;
END $$;

ALTER TABLE publication.runtime_revisions ALTER COLUMN gateway_model SET NOT NULL;
ALTER TABLE publication.runtime_revisions ALTER COLUMN vendor_model_id SET NOT NULL;

-- 运维开关。`updated_by` 可空：迁移回填的行不是"某个人改的"，那时还没有这个字段，
-- 留 NULL 比编一个系统账号诚实；发布事务插入的行记发布者，PATCH 之后记改动人。
CREATE TABLE publication.gateway_models (
    gateway_model text PRIMARY KEY,
    enabled boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    updated_by text
);

INSERT INTO publication.gateway_models (gateway_model)
SELECT DISTINCT re.gateway_model
FROM publication.runtime_entries re
WHERE re.active;
