-- 供给声明的**成本币种**：这是渠道事实（素材里写着的 `cost_currency`），发布期按候选取它。
--
-- 它此前只存在于 Price Plan 的币种或已发布修订的冻结值里。没有价目表、又还没发布过的供给于是没有
-- 任何来源：引用式发布只能撞上"必须声明成本币种"的拒绝，运营在界面上也无从补录（工作项 #81）。
-- 素材导入是它的事实属主，照实写回；发布命令给了就用命令的。
ALTER TABLE supply.offerings ADD COLUMN cost_currency text;

COMMENT ON COLUMN supply.offerings.cost_currency IS
    '渠道事实：这条供给声明的成本币种（原币种代码）。缺省取它的 Price Plan 币种；都没有时发布期按候选要求显式给出。';
