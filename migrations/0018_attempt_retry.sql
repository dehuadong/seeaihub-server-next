-- 一个 Job 可以有多次上游调用：**只在可证明上游没有受理**时才会多出第二次。
--
-- 为什么必须去掉 `UNIQUE (job_id)`：那条唯一约束把"一个 Job 只有一次上游调用"钉在了库层，
-- 于是"这一次没被受理、重投一次"在数据模型里根本表达不出来（多出来的一行插不进去）。
-- 一次**用户请求**仍然只对应一次上游调用；重投是同一台 Job 内的一次新执行，不是把用户的
-- `n` 张拆成多次请求。
--
-- 为什么是 `attempt_no`（同一 Job 内从 1 递增）而不是只靠 `started_at` 排序：
--   1. "这是第几次执行"要被**唯一**确定下来——同一毫秒的两行不能有两种排序，重投次数
--      （`attempt_no` 与上限比较）必须是可判定的事实，不能由排序的稳定性决定；
--   2. 结算与成本逐次归到"第几次"，运营看一次 Job 的花费时不必再去猜行的先后；
--   3. 原来那条约束的反面（同一 Job 内不许有两次同号执行）由 `UNIQUE (job_id, attempt_no)`
--      接住：既有唯一性没有被放松，放松的只是"一个 Job 只能有一行"。

ALTER TABLE generation.attempts DROP CONSTRAINT attempts_job_id_key;
-- 默认值 `1` 是给**不是重投产生的**那些行留的：`UNIQUE (job_id)` 生效期间每个 Job 恰好一行，
-- 所以"没写号"的行按定义就是它那个 Job 的第一次执行（历史行、以及运维/夹具直接插的行）。
-- 写号的那条路径（Worker 的 `begin_attempt`）**总是**显式给出算好的号，不受这个默认值影响。
--
-- 也不用 `bigserial`：号只在**同一台 Job 内**有意义，全局序列会给出 `1, 2, 3...` 这种跨 Job 的
-- 连续号，读起来像"平台第几次调用上游"，而它其实只是"这台 Job 的第几次"。
ALTER TABLE generation.attempts ADD COLUMN attempt_no integer DEFAULT 1;
-- 历史行的回填：`UNIQUE (job_id)` 生效期间每个 Job 至多一行，所以每一行都是它那个 Job 的
-- 第 1 次执行。不按 `started_at` 重新编号——那样得到的是同一批 `1`，还要额外处理并列，
-- 而"第几个"这个事实在旧数据里本来就是确定的：一行即第一次。
UPDATE generation.attempts SET attempt_no = 1 WHERE attempt_no IS NULL;
ALTER TABLE generation.attempts ALTER COLUMN attempt_no SET NOT NULL;
ALTER TABLE generation.attempts
    ADD CONSTRAINT attempts_attempt_no_positive CHECK (attempt_no > 0);
ALTER TABLE generation.attempts
    ADD CONSTRAINT attempts_job_attempt_no_key UNIQUE (job_id, attempt_no);
