-- 供给声明的对客参考价目（Spec 0001 §5.3 V-D12；设计 0007 §2）。
--
-- 它是**渠道原币种的四档 token 价目**，只作对客 token 四档的初始价来源：运营在发布页看到的那份
-- 预填值就是它，改了按改后的发、随 Job 快照冻结。它**不是成本参数**——成本费率是 `price_plan`
-- （`token_rates` 的成本参数），因此任何成本形态的供给都可以声明参考价目，发布期也不因缺它而拒。
--
-- 列可空：没有它时对客 token 四档的初始价由运营按报价填（设计 0007 §2）。
ALTER TABLE supply.offerings ADD COLUMN consumer_reference_rates jsonb;

COMMENT ON COLUMN supply.offerings.consumer_reference_rates IS
    '对客 token 四档的初始价参考价目：{currency, 四档 microusd_per_million, source_url}；只作初始值来源，不进成本口径、不参与结算';
