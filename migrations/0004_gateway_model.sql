-- 平台型号名的列名收口：jobs 与 runtime_entries 里存的一直是**平台型号名**（运营发布时用的
-- 型号标识），却叫 native_model_id（"原生"）——那个名字是**厂商原生名**的语义，属
-- catalog.vendor_models.native_model_id。两处改名成 gateway_model，对外接口一直就叫 model。
--
-- 今天两者同值（发布时用厂商原生名当平台型号名），改名只把角色写清，不改变任何取值。

ALTER TABLE generation.jobs RENAME COLUMN native_model_id TO gateway_model;
ALTER TABLE publication.runtime_entries RENAME COLUMN native_model_id TO gateway_model;
