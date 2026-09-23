-- 计价形态落在供给上：Price Plan 从"每条供给必填"变成"按 token 计量量这一种形态的参数"。
--
-- 一件事：**计价形态是渠道事实**——这个渠道的这个模型按什么计价（四分项 token 计量量 / 按产出
-- 张数 / 按调用次数 / 由上游直接给实扣金额），平台如实登记，它决定**成本**怎么算（平台与渠道
-- 怎么结算）。渠道不按 token 计量量计价时，这条供给就没有那份四档费率表，所以：
--
--   1) `publication.runtime_entries.price_plan_id` 放开 NOT NULL：Price Plan 是 `token_rates`
--      这一种形态的参数，不是所有供给都有。放开的是"其它形态可以不发价目表"，**不是**
--      "`token_rates` 也可以不发"——那条由发布期校验拦着（缺四档费率直接 400）。
--   2) `supply.offerings` 增两列：`formula`（计价形态，落库值就是发布期校验过的那个取值，
--      库层白名单与领域取值同源）与 `cost_unit_price_microusd`（按张 / 按次时的单价）。
--
-- 既有行取默认值：它们都是按 token 计量量发布出来的供给（那时只有这一种形态），
-- `formula` 因此回填成 `token_rates`、单价留 NULL——与它们已经落库的那份价目表自洽，
-- 读回来的成本算法与今天逐位相同。
--
-- 不做的事：对客定价那一条维不动（对客费率向量、加价系数、汇率、保底表都不在这里）。

ALTER TABLE publication.runtime_entries
    ALTER COLUMN price_plan_id DROP NOT NULL;

ALTER TABLE supply.offerings
    ADD COLUMN formula text NOT NULL DEFAULT 'token_rates';

ALTER TABLE supply.offerings
    ADD COLUMN cost_unit_price_microusd bigint;

-- 取值面与领域 `PricingFormula` 的落库字符串**同源**：白名单只是把同一组取值钉在库层，
-- 不在这里另立一套判据。改取值面时两处一起改。
ALTER TABLE supply.offerings
    ADD CONSTRAINT offerings_formula_known
    CHECK (formula IN ('token_rates', 'per_image', 'per_call', 'upstream_declared'));

-- 单价只有按张 / 按次这两种形态才有：另外两种形态带着它说明发布者的意图与声明的形态对不上，
-- 而那个数永远不会被读。非负与"给了就必须是计价单位"一起钉住。
ALTER TABLE supply.offerings
    ADD CONSTRAINT offerings_cost_unit_price_shape
    CHECK (
        cost_unit_price_microusd IS NULL
        OR (cost_unit_price_microusd >= 0 AND formula IN ('per_image', 'per_call'))
    );

COMMENT ON COLUMN supply.offerings.formula IS
    '该供给的计价形态（渠道事实）：token_rates / per_image / per_call / upstream_declared';
COMMENT ON COLUMN supply.offerings.cost_unit_price_microusd IS
    '按张 / 按次计费的单价（微单位，币种见该候选声明的成本币种）；另外两种形态为 NULL';
