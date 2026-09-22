-- 一次执行留下的成本事实：渠道报出来的钱（成本平面）。
--
-- 为什么要落它：上游成本此前一家都没留痕——APIMart 的终态直接给 `cost`（含账号折扣，比自算
-- 更权威）却不采纳，AIHubMix 不给金额字段、金额要平台按实际用量自算也没存。没有这两列，
-- 毛利算不出来，事后也答不出"这一笔的成本是按哪种来源取的"。
--
-- 四列的分工：
--   provider_cost_source   成本从哪来（判据是"来源"，不是"金额对不对"）：
--                          computed    = 渠道不给金额字段，平台按实际分项 token × 该渠道成本费率自算
--                          declared    = 渠道终态直接给了金额，直接取它，不自己算
--                          unavailable = 本该有金额却拿不到（缺字段 / 负数 / 解析失败）——不猜
--   这三个取值与领域 `ProviderCostSource` 的落库字符串**同源**（SDK 的报告经一处映射转成它），
--   白名单只是把同一组取值钉在库层：改取值面时三处一起改，不在这里另立一套判据。
--   provider_cost_microusd 原币种微单位金额；`unavailable` 时为 NULL
--   provider_cost_currency 该渠道声明的币种（不假定 USD）；`unavailable` 时为 NULL
--   provider_cost_cny_microusd 折算后 CNY 微单位，毛利用
--
-- 为什么 source 可空：失败的执行没有终态金额，既没有渠道声明、也没有可自算的用量，
-- 四列一律留 NULL——把失败件写成 `unavailable` 会把"这条渠道本就不报金额"与
-- "报了我们没拿到"混成一件事，而这两件事的处置完全不同。
--
-- 为什么 CNY 折算列先落、值先留空：折算要用受理时冻结的汇率，而汇率表与快照里的汇率位
-- 属于定价切片（连同它的定点分母）。这一片只落列与落点，不自己发明一个分母。
--
-- 约束把"不猜"钉在库层：两种有金额的来源必须有金额与币种，`unavailable` 必须两者都为空。

ALTER TABLE generation.attempts
    ADD COLUMN provider_cost_microusd bigint,
    ADD COLUMN provider_cost_currency text,
    ADD COLUMN provider_cost_source text,
    ADD COLUMN provider_cost_cny_microusd bigint;

ALTER TABLE generation.attempts
    ADD CONSTRAINT attempts_provider_cost_non_negative
    CHECK (
        (provider_cost_microusd IS NULL OR provider_cost_microusd >= 0)
        AND (provider_cost_cny_microusd IS NULL OR provider_cost_cny_microusd >= 0)
    );

ALTER TABLE generation.attempts
    ADD CONSTRAINT attempts_provider_cost_source_known
    CHECK (
        provider_cost_source IS NULL
        OR provider_cost_source IN ('computed', 'declared', 'unavailable')
    );

-- 来源与金额/币种同形：有金额的来源两样都在，`unavailable` 两样都不在。
--
-- 来源为 NULL 的那一行是**失败的执行**：既没有渠道声明、也没有可自算的用量，四列一律留
-- NULL——只填一半会造出一条"有金额却说不出来源"的记录。`unavailable` 那一行同理不许有折算值：
-- 金额都没有，折算不出来，写了就是猜的。
--
-- **两支都要显式判 `provider_cost_source IS NOT NULL`**：`CHECK` 的表达式求值为 NULL 时算
-- **通过**（只有 FALSE 才拒绝），而来源为 NULL 时 `provider_cost_source IN (...)` 求值是 NULL，
-- 于是"来源 NULL 但金额非空"这种半填的行会从 `FALSE OR NULL` 里漏过去。加上这个非空判定，
-- 三支都是 TRUE/FALSE 的判定，半填的行落不下来。
ALTER TABLE generation.attempts
    ADD CONSTRAINT attempts_provider_cost_shape
    CHECK (
        (
            provider_cost_source IS NULL
            AND provider_cost_microusd IS NULL
            AND provider_cost_currency IS NULL
            AND provider_cost_cny_microusd IS NULL
        )
        OR (
            provider_cost_source IS NOT NULL
            AND (
                (
                    provider_cost_source IN ('computed', 'declared')
                    AND provider_cost_microusd IS NOT NULL
                    AND provider_cost_currency IS NOT NULL
                )
                OR (
                    provider_cost_source = 'unavailable'
                    AND provider_cost_microusd IS NULL
                    AND provider_cost_currency IS NULL
                    AND provider_cost_cny_microusd IS NULL
                )
            )
        )
    );

-- 成本缺口要"可发现"：按来源筛 `unavailable` 是运营查缺口的入口，给它一条部分索引。
CREATE INDEX attempts_provider_cost_unavailable
    ON generation.attempts (completed_at)
    WHERE provider_cost_source = 'unavailable';
