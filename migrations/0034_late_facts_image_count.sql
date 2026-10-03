-- 晚到账务事实上的产出图片张数（RFC 0017 §3）：按张计价的成本/实收靠它才算得出。
--
-- 它随收件行一起落，属于 Spec 0005 §2 允许保存的最小事实（产出张数）。领取者按它补算；
-- 收件形态不带它（或调用方拿不到）时列留 NULL，按张计价一律落成本缺口，不拿 token 数或
-- 请求的 n 顶替。收件行的形状约束（late_facts_shape）不变：张数不是单独成案的依据。
ALTER TABLE generation.late_facts ADD COLUMN image_count integer;

ALTER TABLE generation.late_facts
    ADD CONSTRAINT late_facts_image_count_non_negative
    CHECK (image_count IS NULL OR image_count >= 0);

COMMENT ON COLUMN generation.late_facts.image_count IS
    '上游实际产出的图片张数；收件形态不带它或调用方拿不到时为 NULL';
