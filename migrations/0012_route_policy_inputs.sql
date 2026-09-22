-- 路由策略层第二批：`least_cost` 与 `user_tag` 两个策略，以及它们各自要吃的输入。
--
--   1) 放宽 route_policies.strategy 的取值面，加入 least_cost 与 user_tag；
--   2) route_policies 增折扣率表与标签映射两列（P6a 建表时按设计只放了策略类型）；
--   3) ledger.accounts 增 tag：标签落在**账户**上，映射落在**策略**里。
--
-- 折扣率只作 least_cost 的比较输入，**不是成本口径**：成本永远按实际扣费记（渠道声明多少就是
-- 多少、不乘折扣率），两者不一致时以实际扣费为准。标签是 user_tag 的输入：没有生效的 user_tag
-- 策略时，标签不影响任何选路结果。
--
-- 沿用既有做法：以新迁移增量修改，不就地改 0001–0011。

ALTER TABLE routing.route_policies
    DROP CONSTRAINT IF EXISTS route_policies_strategy_check;

ALTER TABLE routing.route_policies
    ADD CONSTRAINT route_policies_strategy_check
    CHECK (strategy IN ('priority_failover', 'weighted_random', 'least_cost', 'user_tag'));

ALTER TABLE routing.route_policies
    ADD COLUMN IF NOT EXISTS discount_rates jsonb NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN IF NOT EXISTS tag_channel_map jsonb NOT NULL DEFAULT '{}'::jsonb;

COMMENT ON COLUMN routing.route_policies.discount_rates IS
    '折扣率表：候选供给（offering_id）→ 万分比；只作 least_cost 的比较输入，不进成本';
COMMENT ON COLUMN routing.route_policies.tag_channel_map IS
    '标签 → 候选（offering_id）的映射，供 user_tag 用；映射指向的候选仍须合格';

ALTER TABLE ledger.accounts
    ADD COLUMN IF NOT EXISTS tag text;

COMMENT ON COLUMN ledger.accounts.tag IS
    '账户标签（运营设）：只有生效的 user_tag 策略消费它，没有该策略时不影响选路';
