---
title: 客户控制台独立页面与路径路由
status: implemented
created: 2026-09-30
updated: 2026-09-30
approval: 用户授权实施客户控制台独立页面与导航（控制台 Spec v14 C15、D5、V-D13–V-D14；设计 0014 §1–§3）
verification: 见正文「验证」。`apps/web` 的 `npm run typecheck`、`npm run build`（含产物隔离核对）通过，`npx playwright test` 连跑两次各 42 passed。
---

# Agent Note：客户控制台独立页面与路径路由

## 问题

[历史控制台 Spec v14 C15](../../../../docs/specs/0001-admin-and-customer-consoles.md) 要求客户控制台提供五个独立页面与固定导航，D5 要求未登录直达内部地址只显示登录、登录后回到原页面，且凭据不进 URL；[设计 0014 §1](../../../../docs/design/0014-customer-console-navigation-and-history.md) 给出五条地址。原来的客户入口是单页加标签页（`src/portal/Dashboard.tsx`），概览与明细堆在一起，地址不能标识页面，刷新与前进后退都落回同一页。

## 决定

- **用 path + History API 自己做路由，不引 react-router。** 五条地址是产品固定的页面标识（`/`、`/usage`、`/billing`、`/keys`、`/settings`），路由只需要解析地址、监听 `popstate` 与 `pushState`；这里没有嵌套与参数，路由库带进来的匹配与嵌套概念用不上。管理端那套 hash 路由不动——它承担的是另一份入口的旧地址兼容。
- **路由守卫只控制渲染，不动地址。** 没有会话时只挂载 `AuthPage`，账户、密钥与账务组件不挂载、不发取数请求；地址保留，登录成功后按当前地址渲染对应页面，这就是"登录后回跳"。
- **会话清屏有两条触发。** 服务端回 401（退出、被吊销、过期）时 `CustomerClient` 集中回调清掉本地会话；登录响应里的 `expires_at` 到点也清。清屏即回登录页，旧账务与密钥不留可见页面。
- **五个页面各自取数。** 概览只请求 `/v1/customer/account`；调用记录、账单与资金记录、API Key、账户设置各请求自己需要的端点，互不加载对方的数据（C15）。
- **概览只放一个「余额」与入口。** 余额的口径后由[客户侧余额改为「现在能用的钱」](../domain/2026-10-02-customer-balance-is-available.md)改成"客户现在能用的钱"；移除原来的"扣费总额（全部）"与账单请求；账单页移除"平均每次扣费"，把有符号净额解释为净支出/净返还/收支相抵（Spec §4.3、V-D14）。
- **开发服务补同一条深链回退。** 生产由 API 静态兜底把无扩展名路径回 `portal.html`；`vite.config.ts` 的 `portal-dev-deep-links` 在本机做同一件事，两条落回同一组地址（设计 0014 §5）。

## 备选方案

- **react-router**：落选。五条固定地址、没有嵌套与参数，路由库的匹配与嵌套概念用不上；自写路由约 60 行，行为只在 `routes.ts` 一处。
- **继续用 hash（与管理端一致）**：落选。设计 0014 §1 给的是 `/usage` 这类路径，V-D13 要"直接打开内部地址"；hash 地址在分享与直接打开时不落在同一条路径上。
- **五个页面做成同一地址下的条件渲染、不换 URL**：落选。刷新与前进后退要按地址恢复页面（D5、V-D13），地址必须标识页面。
- **只靠服务端 401 清屏，或只靠 `expires_at` 倒计时**：两条都做。401 覆盖"已被提前吊销"，倒计时覆盖"闲置到期"，各管一半。
- **开发服务不做深链回退**：落选。设计 0014 §5 要求开发与生产都能直接打开五个地址；只在 API 托管下成立会让本机开发只能从概览点进去。

## 后果

- 客户入口产物多出一层 `Layout` 外壳与导航；概览不再请求账单，首次进入只发一条账户请求。
- 路由以 path 为准，静态托管必须继续把无扩展名路径回 `portal.html`，且 `/api`、`/v1` 的 JSON 404 语义不能被兜底吃掉——这条由 `apps/api` 既有实现与用例守着。
- `sessionStorage` 多存一个 `expires` 键；退出与到期都会清掉它。
- 日期筛选与连续翻页、API Key 吊销确认与一次性明文不在本次决定内，仍按设计 0014 §2–§4 的后续切片做。

## 验证

- `npm run typecheck` 通过；`npm run build`（含 `check-bundle-isolation.mjs`）通过，两份产物互不引用。
- `npx playwright test` 连跑两次，各 **42 passed**。新增/改写的用例：
  - `portal-navigation.spec.ts`：未登录直达五条地址只显示登录且不取账户数据、未登录直达 `/billing` 登录后回 `/billing`、五条地址直接打开与刷新都落在对应页、前进后退按地址恢复、凭据不进 URL、退出清屏、未知路径显示客户侧 404。
  - `portal-self-service.spec.ts`：概览首屏只有一个「余额」与五页导航、页面拆分各自取数、密钥明文只出现一次、改口令与重置令牌。
  - `ui-uses-antd.spec.ts`：客户外壳与五个页面都渲染 Ant Design 组件。
- 未改客户业务 API，未跑 Rust 合同套件；Rust 全量门禁交 CI。
