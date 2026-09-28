-- 对客计价形态与对客单价：运营按候选选，与成本计价形态（supply.offerings.formula）独立。
-- 两列与 consumer_rates_cny 同处：publication.runtime_revisions 上**按候选键**的 jsonb，可空。
-- 旧修订留 NULL ⇒ 受理与结算按"对客形态等于成本形态"的旧口径，逐位不变。

ALTER TABLE publication.runtime_revisions
    ADD COLUMN consumer_formula jsonb,
    ADD COLUMN consumer_unit_price_cny_microusd jsonb;

COMMENT ON COLUMN publication.runtime_revisions.consumer_formula IS
    '按候选键：该候选的对客计价形态（运营按候选选）；缺这个键的历史修订按等于成本形态解释';
COMMENT ON COLUMN publication.runtime_revisions.consumer_unit_price_cny_microusd IS
    '按候选键：对客选 per_image / per_call 时的每张 / 每次对客单价（CNY 微单位），运营给';
