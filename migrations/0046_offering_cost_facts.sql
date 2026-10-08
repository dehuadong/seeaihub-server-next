-- 供给声明的**保底表**与**渠道参考成本**：两者都是渠道侧的成本事实（素材里写着的 `floor_amounts`
-- 与 `reference_cost_microusd`），不是运营的定价。
--
-- 它们此前只有发布命令一个来源，而运营表单里不出现成本侧字段（Spec 0001 V-D12），控制台的引用式
-- 发布于是永远填不出它们：`upstream_declared` 候选的保底表进不了修订，受理只能回落平台兜底额
-- （工作项 #89）。落点与 `cost_currency`（迁移 `0044`）同一条判断：运营在界面上无从补录的东西，
-- 必须有素材 → Offering 行的落点。
ALTER TABLE supply.offerings
    ADD COLUMN floor_amounts jsonb,
    ADD COLUMN reference_cost_microusd bigint;

COMMENT ON COLUMN supply.offerings.floor_amounts IS
    '渠道事实：这条供给的保底表（CNY/张，按 (size, quality) 与该供给每张封顶值）。受理时算预授权额的查表依据，随发布物冻结进修订。';

COMMENT ON COLUMN supply.offerings.reference_cost_microusd IS
    '渠道事实：这条供给的渠道参考成本（**原币种**微单位，币种取 cost_currency）。只作定价参考，随发布物冻结进修订。';
