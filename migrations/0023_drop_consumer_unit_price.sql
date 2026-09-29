-- 对客计价只保留两种形态：按 token 四档 / 上游声明金额 × 倍率。对客侧不再有"每张 / 每次单价"
-- 这个载体，所以 0022 加的那一列在这里删掉。
--
-- 按张 / 按次仍是**成本**形态（渠道事实，落在 supply.offerings.formula 与它的 cost_unit_price_microusd
-- 上），与对客侧无关。

ALTER TABLE publication.runtime_revisions
    DROP COLUMN IF EXISTS consumer_unit_price_cny_microusd;
