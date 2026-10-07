-- 对客参考价目的**归属移到模型（厂商）层**：它是"该 vendor／模型已知的价目"，一个模型一份，
-- 与勾哪条候选无关（用户 2026-10-07 裁定：厂商是这个价目）。
--
-- 先按模型把已有值收上去（同一模型下多条供给带不同值时取任意一条非空的：参考价目本来就只有一份，
-- #78 之后也只在一个模型下声明过），再删掉供给上那一列。
ALTER TABLE catalog.vendor_models ADD COLUMN consumer_reference_rates jsonb;

UPDATE catalog.vendor_models vm
SET consumer_reference_rates = source.reference
FROM (
    SELECT DISTINCT ON (o.vendor_model_id)
           o.vendor_model_id, o.consumer_reference_rates AS reference
    FROM supply.offerings o
    WHERE o.consumer_reference_rates IS NOT NULL
    ORDER BY o.vendor_model_id, o.id
) AS source
WHERE source.vendor_model_id = vm.id;

ALTER TABLE supply.offerings DROP COLUMN consumer_reference_rates;

COMMENT ON COLUMN catalog.vendor_models.consumer_reference_rates IS
    '该 vendor／模型声明的对客参考价目（渠道原币种四档）：只作对客 token 四档初始价的来源，不是成本参数、不进修订、不进 Job 快照。素材导入是它的属主。';
