-- 第二阶段：同一 Vendor Model 可以有多个 active Offering
--
-- 依据 docs/adr/0009-multiple-active-offerings-and-routing.md 与 #2 规划 §3.1：
--   1) runtime_entries 增加 routing_priority（优先级来自发布顺序，数字小者优先）；
--   2) 唯一索引从「每个型号只能有一个 active 条目」改为「每个型号的每个优先级只能有一个」；
--   3) 新增 generation.routing_decisions：记录受理时用过哪些请求侧事实、判成了什么。
--
-- 说明：0001 以「空库建表」方式被应用；本轮以新迁移增量修改，不就地改 0001——
-- 已应用过 0001 的库不会因改动 0001 而更新。

-- 1) 优先级列。默认 0，故既有行自动落在优先级 0，且不破坏「每个型号只有一个 active」的既有语义。
ALTER TABLE publication.runtime_entries
    ADD COLUMN routing_priority integer NOT NULL DEFAULT 0;

-- 2) 先删旧唯一索引，再建新的：否则同型号多 active 条目仍被禁止。
DROP INDEX publication.one_active_runtime_entry_per_model;

CREATE UNIQUE INDEX one_active_entry_per_model_and_priority
    ON publication.runtime_entries (native_model_id, routing_priority) WHERE active;

-- 3) 判定记录：每次受理一条，记录被考虑过的候选与各自的取舍原因。
--    属主边界（规划 §3.4）：候选集与顺序的权威是发布物（runtime_revisions.snapshot），
--    本表只记「受理时用哪些请求侧事实判成了什么」，不复制 base_url / credential_env 等发布字段。
--    job_id 用 UNIQUE（非 PK）：与 Job 一一对应，但不占用主键——本表是追加型日志，用 bigserial 作主键。
CREATE TABLE generation.routing_decisions (
    id bigserial PRIMARY KEY,
    job_id uuid NOT NULL UNIQUE REFERENCES generation.jobs(id),
    runtime_revision_id uuid NOT NULL REFERENCES publication.runtime_revisions(id),
    chosen_offering_id uuid NOT NULL REFERENCES supply.offerings(id),
    considered jsonb NOT NULL,
    decided_at timestamptz NOT NULL DEFAULT now()
);
