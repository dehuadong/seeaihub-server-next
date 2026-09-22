-- 路由策略表：在**已发布的合格候选**里"挑哪一条"由运营配置的策略决定。
--
-- 为什么它不是发布内容：策略回答的是"在合格候选里怎么选"，不是"卖什么"。因此它运行期可改、
-- 改完即时生效，不进不可变修订（runtime_revisions / runtime_entries）；已受理的 Job 早已把候选
-- 固定在快照里，不受后续改策略影响。
--
-- 作用域两档：gateway_model 为空 = 全局一条；非空 = 覆盖该网关模型（按模型取"有覆盖用覆盖、
-- 没有用全局"）。唯一索引建在 coalesce(gateway_model, '') 上：一个作用域只能有一行，
-- 否则"哪条生效"就取决于行序。
--
-- strategy 只放本层**已经实现**的取值：写进一个实现不了的策略，会让"配置没生效"伪装成
-- "配置生效了"，而选路正是靠它决定走哪家。P6b 加 least_cost / user_tag 时一并放宽这条约束。
--
-- version 每次写入都换：缓存拿它判断自己是不是旧的（与 route 缓存按 runtime_revision_id
-- 比对同构）。discount_rates / tag_channel_map 是后续策略的输入，先按设计把列留出来，
-- 本层不消费它们（写默认空对象）。
--
-- 沿用既有做法：以新迁移增量修改，不就地改 0001。

BEGIN;

CREATE SCHEMA IF NOT EXISTS routing;

CREATE TABLE routing.route_policies (
    id uuid PRIMARY KEY,
    gateway_model text,
    strategy text NOT NULL
        CHECK (strategy IN ('priority_failover', 'weighted_random')),
    discount_rates jsonb NOT NULL DEFAULT '{}'::jsonb,
    tag_channel_map jsonb NOT NULL DEFAULT '{}'::jsonb,
    version text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    updated_by text NOT NULL
);

COMMENT ON COLUMN routing.route_policies.gateway_model IS
    '作用域：NULL = 全局一条；非空 = 覆盖该网关模型';

CREATE UNIQUE INDEX route_policies_one_row_per_scope
    ON routing.route_policies ((coalesce(gateway_model, '')));

COMMIT;
