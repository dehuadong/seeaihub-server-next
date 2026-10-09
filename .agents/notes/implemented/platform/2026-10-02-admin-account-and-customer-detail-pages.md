---
title: 后台账户与客户改成独立详情页
status: implemented
created: 2026-10-02
updated: 2026-10-02
approval: 用户于 2026-10-02 指出“列表下方追加详情”不是预期布局、纠正了旧工单把它归因为“用户已选择”的错误，并授权按选定范围实施
verification: 见正文「验证」。`cd apps/web && npm run typecheck` 通过；`cd apps/web && npx playwright test` 全量 50 passed；两条按标识读的管理端点由 `cases_admin_surface` 的真库用例覆盖。
---

# Agent Note：后台账户与客户改成独立详情页

## 问题

2026-09-30 的账户页把找账户列表、选中摘要与四个弹层入口放在同一页：点行后在列表下方追加“选中账户”。客户页同形——点“详情”在查找列表下方追加客户卡片。用户于 2026-10-02 指出两处都不是预期，并纠正了当时工单把该布局归因为“用户已选择”的错误。

本记录取代《管理员账户页改成按钮与弹层》（该文件随本次变更删除），接收其中仍然有效的理由：模块内容懒挂载、一次性明文只在签发那一次出现、按主体重挂而不是逐个手工清状态、金额直接渲染接口字段而不在页面重算。页面目标与金额口径不在本记录：页面组织由[管理端页面设计 §3.1、§4.3–4.4](../../../../docs/design/0011-console-information-architecture.md) 拥有，金额语义由[账户资金 Spec §4](../../../../docs/contracts/0002-account-funds-and-reservations.md) 拥有。

## 决定

- 列表与详情**分成两个地址**：`#/accounts`、`#/accounts/{account_id}`、`#/customers`、`#/customers/{customer_id}`。地址解析把同一工作区的列表与详情映射到同一个侧栏项，当前工作区由 `aria-current="page"` 标出。
- 详情**按地址里的标识独立取数**，不拿列表那一行当答案：新增两条只读管理端点 `GET /api/v1/accounts/{account_id}/summary`（既有 `AccountSummary`）与 `GET /api/v1/customers/{customer_id}`（既有 `CustomerView`），不存在返回 404；账户的三个金额仍读既有余额接口。
- 账户详情的四个模块（充值、充值记录、扣费记录、API Key）用页内按钮切换，**一次只挂载当前那一个**：切走即卸载，API Key 的签发明文因此不会在切回来时又出现。按钮与挂载内容来自同一张 `MODULES` 表。
- 充值的幂等键在模块内生成、成功后更换；模块卸载丢掉没提交的金额与错误。
- 两张列表的查找条件放在当前标签页的 `sessionStorage`，不进 URL；返回列表、刷新与前进后退时恢复，登录与退出时清掉。管理读回 403 时 `AdminClient` 把**这次用的那枚凭据**报给入口：只有它仍是当前会话才 `signOut`（回到登录页并顺带清掉上一名运营留下的条件），迟到的 403 不会踢掉换人之后的新会话。
- 详情读回 400/404 显示“找不到”并清屏；地址不是已知页面（含多出来的段）同样显示找不到，不回落到模型目录。

## 备选方案

- **沿用 hash 加标识段 vs 换成 History API**：选 hash。产物是静态文件，hash 不需要为深链配“所有路径回入口 HTML”的规则；客户控制台走 path 是因为那边本来就由 API 按主机名回退入口。
- **详情复用列表那一行 vs 按标识重读**：选重读。列表有条数上限，返回时筛选条件也可能已经变了，刷新一个较旧对象时列表里根本没有它。
- **保留弹窗／侧边栏 vs 页内切换**：选页内。模块与地址一起表达“这一个账户的哪一种操作”，弹层把“当前在哪”藏进叠层；页内切换还让“离开模块即销毁明文”有一条机械保证，而不是靠记得关闭。
- **筛选条件进 URL 查询串 vs 会话状态**：选会话状态。邮箱是客户资料，进 URL 就会留在浏览器历史与日志里（Spec D2、D6）；代价是换个标签页看不到上次条件，那可以接受。
- **建账户与写标签当成一步、标签失败就整体报错 vs 仍进详情**：选仍进详情。账户与初始充值这时已经成立，留在列表上等于让人手上多一个只能按标识找的账户；标签提示没写上，可以在详情里补。

## 后果

- 管理面多两条只读端点；`every_admin_endpoint_requires_credentials` 的端点矩阵随之加两行，未认证行为与其他管理端点一致。
- 未知 hash 不再回落到模型目录：以前敲错地址会看到模型目录，现在显示“找不到页面”。
- 管理端 403 现在会退出登录回到登录页（以前只显示一行错误）。管理面 403 只有“凭据不被接受”一个含义，所以这个收尾是确定的，没有别的分支要区分。
- 旧同页布局的浏览器断言随合同改写：`admin-account-detail-modules.spec.ts`（原 `admin-account-modules-in-layers.spec.ts`）覆盖模块挂载与一次性明文，新 `admin-console-detail-addresses.spec.ts` 覆盖地址、刷新、前进后退、筛选恢复、未登录直达、找不到与会话被拒清屏。

## 验证

- `cd apps/web && npm run typecheck` 通过。
- `cd apps/web && npx playwright test`（全量，含新写的 `admin-console-detail-addresses.spec.ts` 五条与改写后的账户／客户用例）**50 passed**；同一条命令连跑两次都绿。
- 两条按标识读的管理端点：`HTTP_CONTRACT_DATABASE_URL=postgres://seeai:seeai@127.0.0.1:5432/seeai_contract cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1 account_and_customer_details_are_readable_by_identifier` 通过（认证、存在、不存在、字段同形、不含凭据），`every_admin_endpoint_requires_credentials` 通过。
- `cargo test -p seeai-application -p seeai-persistence --all-features`：149 passed。`cargo fmt --check` 通过。
- `cargo clippy -p seeai-application -p seeai-persistence -p seeai-api -p seeai-worker --all-targets --all-features -- -D warnings` 在本机 rustc 1.99 上被**既有**的 `crates/adapter-aihubmix/src/lib.rs` `single_element_loop` 挡住（该文件本次未改）；放行这一条后本次改动的 crate 无告警。全量门禁由 CI 承担。
