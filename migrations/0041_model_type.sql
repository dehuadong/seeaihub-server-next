-- Vendor Model 的模型类型（模型类型 Spec 0006 §1、§4.4；模型类型设计 0020 §1、§7）。
--
-- 类型是模型自身的事实，与 `capability_schema` 同层同生命周期，随发布素材声明、随发布物落库：
-- 一个网关模型由一次发布引用一个 Vendor Model，类型由被引用的那一行决定。它不参与受理、路由与
-- 目录可见性，也不是计费口径。
--
-- 到本次改动为止，仓库的驱动、承载面与计价形态只覆盖图片模型，所以存量行回填 `image` 是事实而
-- 不是默认值；列不留默认值，之后的插入必须显式声明。
ALTER TABLE catalog.vendor_models ADD COLUMN model_type text;

UPDATE catalog.vendor_models SET model_type = 'image' WHERE model_type IS NULL;

ALTER TABLE catalog.vendor_models ALTER COLUMN model_type SET NOT NULL;

ALTER TABLE catalog.vendor_models
    ADD CONSTRAINT vendor_models_model_type_known
    CHECK (model_type IN ('image', 'video', 'chat'));

COMMENT ON COLUMN catalog.vendor_models.model_type IS
    '模型类型：image / video / chat；随发布素材声明，按 (vendor_id, native_model_id, native_revision) 不可变';
