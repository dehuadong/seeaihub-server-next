-- 权重与路由日志：候选上的分流比，以及"同档可以有多条候选"这件事的库层前提。
--
-- 两件事：
--   1) publication.runtime_entries 增 weight：**档位内的分流比**（正整数，默认 1）。
--      它与 routing_priority 是两个量：routing_priority 是**档位**（数字小者优先），
--      weight 只在**同一档内**按比例分摊。默认 1 让既有行与老素材自动落在"权重 1"，
--      也就是"档内只有一条候选"时行为完全不变。0 不是权重的取值——想不参与分流就不发这条
--      候选；写成 0 只会让"这条候选为什么从来分不到"变成一个要读代码才能回答的问题，所以在
--      库层直接拒掉（应用层先拒一次，为了给出说得清的错误）。
--   2) 唯一索引 one_active_entry_per_model_and_priority 换成 one_active_entry_per_model_and_offering。
--      旧索引按 (native_model_id, routing_priority) 唯一（0002 建的；0004 把该列改名为
--      gateway_model，索引定义跟着改名后的列），等价于"同一档只能有一条候选"，
--      与"档内按权重分流"直接冲突——档内分流要有第二条候选，旧索引先把这条路堵死了。
--      换过之后索引只防"同一个网关模型下同一条供给出现两行"（同一次发布里给同一条供给
--      配两条候选没有意义，两条候选会各写一份权重）。
--      "同一个名字的生效条目来自同一次发布"这条性质**不靠索引**：发布事务先
--      `UPDATE ... SET active = false WHERE active AND gateway_model = $1` 原子替换，
--      仓库层再对"active 候选跨修订并存"报错；同一名字的并发发布由发布事务里那把
--      按名字取的事务级咨询锁串行化（否则两边的替换会交错，active 条目跨修订并存，
--      而那只在读的时候才暴露）。
--
-- 说明：判定记录新增的权重依据与分流落点落在 considered jsonb 里，不加列。
-- 沿用既有做法：以新迁移增量修改，不就地改 0001/0002。

ALTER TABLE publication.runtime_entries
    ADD COLUMN weight integer NOT NULL DEFAULT 1;

ALTER TABLE publication.runtime_entries
    ADD CONSTRAINT runtime_entries_weight_positive
    CHECK (weight > 0);

DROP INDEX publication.one_active_entry_per_model_and_priority;

CREATE UNIQUE INDEX one_active_entry_per_model_and_offering
    ON publication.runtime_entries (gateway_model, offering_id) WHERE active;
