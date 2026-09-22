-- 定价、保底与结算：汇率表、修订上的定价列，以及透支所需的三处约束放宽。
--
-- 三件事：
--   1) pricing.fx_rates：**按币种维护**的折算率表（渠道币种 → CNY），带生效时间。
--      为什么不在价格计划里固定汇率：汇率是外部事实，同一时刻同一币种全平台必须是同一个数
--      才对账得起来；放进每份发布里，改一次汇率要重发所有型号。取值规则是"受理时刻生效的
--      那一行"，所以同一币种同一生效时刻只允许一行——两行同时刻就没有唯一答案。
--      **不预置任何数值**：汇率是外部事实，由管理员录入；没有折算率的币种在发布期被拒。
--   2) publication.runtime_revisions 增七列定价：修订级 markup_bps + **按候选键**的六个 jsonb
--      映射（参考成本 / 成本币种 / 对客费率向量 / 成本来源 / 档位价目表 / 保底表）。
--      为什么按候选键：售价与保底都是**按供给**定的，同一个网关模型的不同候选价格不同；
--      落在修订上（而不是可变的开关表）才能随不可变修订发布、随 Job 快照冻结。
--      **全部可空、不回填**：旧修订没有定价，它们受理出来的快照不带对客费率与保底额，
--      结算与预授权走旧口径、与今天逐位相同。
--   3) 放宽三处 CHECK：预授权**只是保底**，结算按实际扣、超出部分把余额扣成负数（透支发生
--      在结算，不在受理），而保底额本身可以是 0（连该供给的封顶保底值都没有时由平台兜底，
--      兜底数也可能是 0）。库层拦着负余额 / 零保底额时，透支与"保底额可为 0"在库层面直接报错。
--      这里只放宽，不收紧：`ledger.accounts.balance_microusd` 去掉非负约束（余额可为负），
--      另两处由 `> 0` 改 `>= 0`。约束名是 0001 内联 CHECK 的自动命名，显式写出来是为了
--      让"放宽的到底是哪一条"可读、可查。

CREATE TABLE pricing.fx_rates (
    id uuid PRIMARY KEY,
    -- 渠道声明的币种（不假定 USD）：成本平面按它记原值，折算成对客的 CNY。
    currency text NOT NULL,
    -- 定点整数，分母 1_000_000：1 单位该币种 = rate_micros / 1_000_000 元人民币。
    -- 不使用浮点：钱差一个微单位就是对不上账。
    rate_micros bigint NOT NULL CHECK (rate_micros > 0),
    -- 生效时间：取值规则是"受理时刻生效的那一行"（受理时刻之前已生效、其中最新的一行）。
    effective_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    created_by text NOT NULL
);

-- 同一币种同一生效时刻只允许一行：取值规则必须只有一个答案。
CREATE UNIQUE INDEX fx_rates_one_per_currency_and_instant
    ON pricing.fx_rates (currency, effective_at);

-- 取值查询的支撑索引：按币种取"生效时间不晚于某时刻"的最新一行。
CREATE INDEX fx_rates_effective
    ON pricing.fx_rates (currency, effective_at DESC);

ALTER TABLE publication.runtime_revisions
    ADD COLUMN markup_bps integer,
    ADD COLUMN reference_cost_microusd jsonb,
    ADD COLUMN cost_currency jsonb,
    ADD COLUMN consumer_rates_cny jsonb,
    ADD COLUMN cost_basis jsonb,
    ADD COLUMN tier_prices jsonb,
    ADD COLUMN floor_amounts jsonb;

-- 加价系数是基点，可以为 0（不加价），但不可以为负（负加价等于平台倒贴，不是定价）。
ALTER TABLE publication.runtime_revisions
    ADD CONSTRAINT runtime_revisions_markup_non_negative
    CHECK (markup_bps IS NULL OR markup_bps >= 0);

ALTER TABLE ledger.accounts DROP CONSTRAINT IF EXISTS accounts_balance_microusd_check;
ALTER TABLE ledger.holds DROP CONSTRAINT IF EXISTS holds_amount_microusd_check;
ALTER TABLE generation.jobs DROP CONSTRAINT IF EXISTS jobs_max_cost_microusd_check;

ALTER TABLE ledger.holds
    ADD CONSTRAINT holds_amount_microusd_non_negative
    CHECK (amount_microusd >= 0);

ALTER TABLE generation.jobs
    ADD CONSTRAINT jobs_max_cost_microusd_non_negative
    CHECK (max_cost_microusd >= 0);
