---
title: 客户历史游标翻页与密钥确认
status: implemented
created: 2026-10-02
updated: 2026-10-02
approval: 用户于 2026-10-02 明确授权执行实现；两条工作项的范围与验收在实施前按 Plan Review 修订并记录在各自工作项
verification: 见正文「验证」。`cd apps/web && npx playwright test` 全量两次各 55 passed；`cargo test -p seeai-api --test http_contract -- --ignored` 167 passed；`cargo test -p seeai-application` 149 passed。
---

# Agent Note：客户历史游标翻页与密钥确认

## 问题

客户控制台的调用记录与资金流水只给“最近 50 条”：没有日期区间、没有类别筛选、也没有继续翻页的入口；接口层的两个读只认 `since`/`until`/`limit`，`truncated` 还只是“本页是否满”的猜测。另外，仍需对账的请求（`reconciliation_required`）在对客用量里被映射成“未产出”，而合同要求它一直显示“处理中”；API Key 的吊销没有确认，一次性明文也没有关闭入口。

页面与历史查询的设计不在本记录：[客户控制台设计 §2–§3](../../../../docs/design/0014-customer-console-navigation-and-history.md) 拥有参数、分组与翻页；金额与终态时刻归属由[账户资金 Spec §5](../../../../docs/contracts/0002-account-funds-and-reservations.md) 拥有。

## 决定

- **游标是不透明的加密载荷**：`base64url(nonce ‖ AES-256-GCM(载荷))`，密钥只从环境变量 `CUSTOMER_HISTORY_CURSOR_KEY` 读（32 字节的 base64），缺失或格式无效时**进程启动失败并点名配置**。载荷带流（用量已结束 / 流水）、账户、区间、类别与排序位置 `(时刻, 定序键)`；解码失败或与本次请求的账户、区间、类别不符一律参数错误，不静默从首页重查。
- **位置带定序键**：同一事务写入的多条流水共用 `created_at`，只按时刻翻页会在并列处重复或漏项，所以位置是 `(created_at, id)`、`(terminal_at, Job id)`。索引按同样的键重建（新迁移 DROP + CREATE：`(account_id, created_at, id)`、`(account_id, terminal_at, id)`、`(account_id, created_at, id)`），核对过实际执行计划。
- **分流按“结果定了没有”**：`view=active` 是 `accepted`/`leased`/`submitting`/`reconciliation_required`，`view=completed` 是三种终态；`active` 不接受游标、用既有 `truncated` 说明还有更多，`completed` 才按 `(terminal_at, id)` 翻页。`customer_usage_status` 相应把 `ReconciliationRequired` 映射成 `Pending`。
- **对客流水的区间与账单汇总同为半开** `[since, until)`：边界那一笔两边都算或都不算，逐笔求和才等于汇总。管理员那条流水读保持它自己的开区间增量语义（上一次拉到的位置不重复计入）。
- **区间与翻页是一件事**：页面首次算出的 `since`/`until` 就是这一段历史的边界，翻页原样复用、不重算“现在”；账单把区间写进地址（缺省时用 `replaceState` 补写，不新增历史），刷新与前进后退都落回同一区间。
- **对客流水只认真实收支**：这条读**无条件**收窄到 `credit` / `capture` / `adjustment`，`kind` 只能在其内部再筛。账本上还可能有的第四类是 `cost`（平台成本，记在平台账户名下），它不是客户的事实（C8、V-C15）。预授权与释放不在 `ledger.entries` 里（迁移 `0024` 已把这两类清掉并把约束收窄），这条读保证的是它们回来时也不会漏出去。管理员的流水读不受影响。
- **API Key**：吊销先弹确认（带标签），一次性明文多一个显式关闭入口，关闭即从页面状态清掉。

## 备选方案

- **服务端持久游标表 vs 客户端回传加密游标**：选后者。不引入新的表与清理任务；代价是部署要提供一份稳定密钥、轮换会让旧游标失效（页面重新查询即可）。
- **offset 翻页 vs `(时刻, 定序键)` 游标**：选游标。新记录插到首页时 offset 会把后续页整体推移，翻页会漏项。
- **只签名（HMAC）vs 加密 + 认证**：选加密。设计要求“内部 Job 标识不以可解码的文本出现在响应里”，签名拦得住伪造但拦不住解码。
- **游标里带筛选条件摘要 vs 带原值**：带原值并逐项比对。语义等价，少一层摘要算法，也不会因为摘要碰撞把两条不同的筛选面当成同一条。
- **改写 0025 的索引 vs 新迁移 DROP + CREATE**：选新迁移。已应用的迁移不改写（`0013` §6），就地改会留下两份同用途索引。
- **对客流水沿用管理员的开区间 vs 改半开**：选半开。与账单汇总同口径，否则边界那一笔进汇总、不进流水，V-C8 的“逐笔与汇总对得上”在边界上不成立。
- **按 `terminal_at` 是否为空分流 vs 按状态分流**：选状态。合同说“结果尚不确定的显示处理中”，而对账态正是结果未定；按字段空不空会把对账中的请求算进已结束历史。

## 后果

- 部署多一个**必填**环境变量：不配进程起不来（这是刻意的——没有密钥就给不出也解不开下一页）。轮换密钥会让已发出的游标全部失效。
- 对客资金流水的下界从“不含 `since`”变成“含 `since`”，与管理员的增量读**故意不同**：两条读的用途不同，各自的口径写在自己的文档与注释里。
- 管理端的账户用量读仍然读合并视图（处理中 + 已结束），不受对客分流影响。
- 前端多一个直接依赖 `dayjs`（Ant Design 的日期区间控件要它）。
- 客户历史页面的两次读都带 `limit` 和区间：一页放不下时给出“继续查看”，而不是把全量历史拉到浏览器再切。
- 不带 `view` 的旧调用仍然给合并视图，但它的 `truncated` 从“本页是否满”改成准确的“还有没有下一页”：条数正好等于 `limit` 时旧行为会谎报还有更多。
- 0025 之前落库的终态行没有 `terminal_at`：两个新视图都看不到它们（按状态分流 + 按终态时刻归属），只有不带 `view` 的合并视图看得到。仓库未上线、迁移按 `0013` §6 不回填，所以这是已知且可接受的边界。
- 游标一旦发出就与那次查询的筛选面绑死；密钥轮换或区间/类别改变都必须从首页重查（页面已按此实现：改筛选条件即丢弃续页）。

## 验证

- `cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1`：**167 passed**（含 `cases_customer_history` 八条：游标翻页不重不漏、按同一区间翻页、类别筛选与错误游标、跨账户/跨筛选面拒绝、区间下界逐笔与汇总一致、终态时刻跨 UTC 日归属、平台成本不出现在对客流水、游标密钥缺失/格式无效时启动失败并点名配置）。
- `cargo test -p seeai-application --all-features`：**149 passed**（含 `history_cursor` 六条：往返、载荷不可解码、nonce 每次不同、篡改/换密钥/垃圾串按参数错误拒、游标只认自己的查询、由筛选面编出的游标认得出自己；`customer_usage` 一条：每个内部状态的对客取值）。
- `cd apps/web && npx playwright test`：全量**两次各 55 passed**（含 `portal-history.spec.ts` 五条：区间进地址与刷新、直接打开带区间的地址与前进后退、两段用量用不同视图取数、类别筛选只重取流水、多于一页时继续查看且汇总不跟着变；`portal-self-service.spec.ts` 补了吊销确认与明文关闭）。
- 索引：`entries_account_created_at` 在 5000 行流水上 `EXPLAIN (ANALYZE)` 走 `Index Scan Backward`（无 Sort）；两条 Job 索引在 `enable_seqscan=off` 下分别服务两种排序（无 Sort）。
- `cargo fmt --check` 通过；`cargo clippy -p seeai-application -p seeai-persistence -p seeai-api -p seeai-worker --all-targets --all-features -- -D warnings` 在本机 rustc 1.99 仍被**既有**的 `crates/adapter-aihubmix` `single_element_loop` 挡住（该文件未改），放行该条后本次改动的 crate 无告警。
