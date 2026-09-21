-- 调用方合同与供给承载面拆开：合同是模型级唯一一份，承载面属于每条供给。
--
-- 1) supply.offerings 新增两列：
--    - carrier_schema：这条供给**能承载**合同里的哪些字段（它自己声明的面）；
--    - parameter_mapping：把合同值转成渠道包装的声明。本步只落列，映射内容由后续步骤补。
--    老数据就地补上承载面：老形状里 offering 级那份声明面**就是**这条供给能承载的面，
--    补上之后老供给照常可用，不必等重新发布。
-- 2) generation.jobs 新增同样的两列：受理时随 Job 冻结，此后不因发布物变化而变——
--    旧 Job 事后读到的仍是它受理时的那份承载面。
-- 3) catalog.vendor_models 的唯一键去掉 schema_hash：合同是模型级唯一一份，同一个
--    (vendor_id, native_model_id, native_revision) 不能再按内容分叉成两行。
--    同一模型已存在的多行就地合并：保留 created_at 最新的一行（并列时取 id 较大者，
--    让结果确定、可复现）；指向被删行的 offering / runtime_entry / job 改挂到保留行上
--    （供给与 Job 的身份不变，只是它们的合同归属改成那一份唯一的合同）；再删掉多余的行。
--    老 Job 的承载面在第 2 步已按它自己当时那一行补好，因此合并不改变它读到的面。
--    schema_hash 列随之删除：它原本只为"同一修订下多份内容各占一行"服务，那个用法没有了。
--
-- 说明：早期迁移以「建表」方式被应用过，改它们不会更新已建好的库，因此这里增量修改。
-- 已发布的修订不回填成新形状，新发布必须给出承载面（发布期校验会拒绝缺它的发布）。

ALTER TABLE supply.offerings ADD COLUMN carrier_schema jsonb;
ALTER TABLE supply.offerings ADD COLUMN parameter_mapping jsonb NOT NULL DEFAULT '{}'::jsonb;

UPDATE supply.offerings o
SET carrier_schema = vm.capability_schema
FROM catalog.vendor_models vm
WHERE vm.id = o.vendor_model_id AND o.carrier_schema IS NULL;

ALTER TABLE supply.offerings ALTER COLUMN carrier_schema SET NOT NULL;

ALTER TABLE generation.jobs ADD COLUMN carrier_schema jsonb;
ALTER TABLE generation.jobs ADD COLUMN parameter_mapping jsonb NOT NULL DEFAULT '{}'::jsonb;

UPDATE generation.jobs j
SET carrier_schema = vm.capability_schema
FROM catalog.vendor_models vm
WHERE vm.id = j.vendor_model_id AND j.carrier_schema IS NULL;

ALTER TABLE generation.jobs ALTER COLUMN carrier_schema SET NOT NULL;

-- 合并同一模型的多行：先算出"每一行该并到哪一行"，再逐个改挂外键，最后删多余行。
CREATE TEMP TABLE vendor_model_merge AS
SELECT id,
       first_value(id) OVER (
           PARTITION BY vendor_id, native_model_id, native_revision
           ORDER BY created_at DESC, id DESC
       ) AS keep_id
FROM catalog.vendor_models;

UPDATE supply.offerings o
SET vendor_model_id = m.keep_id
FROM vendor_model_merge m
WHERE o.vendor_model_id = m.id AND m.id <> m.keep_id;

UPDATE publication.runtime_entries re
SET vendor_model_id = m.keep_id
FROM vendor_model_merge m
WHERE re.vendor_model_id = m.id AND m.id <> m.keep_id;

UPDATE generation.jobs j
SET vendor_model_id = m.keep_id
FROM vendor_model_merge m
WHERE j.vendor_model_id = m.id AND m.id <> m.keep_id;

DELETE FROM catalog.vendor_models vm
USING vendor_model_merge m
WHERE vm.id = m.id AND m.id <> m.keep_id;

DROP TABLE vendor_model_merge;

-- 旧唯一约束的名字由 PostgreSQL 自动生成（可能被截断），按目录查出来再删，不写死名字。
DO $$
DECLARE existing text;
BEGIN
    SELECT con.conname INTO existing
    FROM pg_constraint con
    JOIN pg_class rel ON rel.oid = con.conrelid
    JOIN pg_namespace nsp ON nsp.oid = rel.relnamespace
    WHERE nsp.nspname = 'catalog' AND rel.relname = 'vendor_models' AND con.contype = 'u';
    IF existing IS NOT NULL THEN
        EXECUTE format('ALTER TABLE catalog.vendor_models DROP CONSTRAINT %I', existing);
    END IF;
END $$;

ALTER TABLE catalog.vendor_models DROP COLUMN schema_hash;

ALTER TABLE catalog.vendor_models
    ADD CONSTRAINT vendor_models_vendor_id_native_model_id_native_revision_key
    UNIQUE (vendor_id, native_model_id, native_revision);
