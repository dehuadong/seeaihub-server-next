---
title: 客户控制台已结算余额的浏览器验证
status: implemented
created: 2026-09-30
updated: 2026-09-30
approval: 用户授权按 Spec v3 §4/A7 与控制台 Spec v14 C7 实施客户控制台只显示已结算余额
verification: 见正文「验证」。本地通过 `cargo fmt --check`、`cargo test -p seeai-application -p seeai-persistence`、合同套件 `cases_public_surface`，以及 `apps/web` 的 `npx playwright test` 连跑两次（32 passed）。
---

# Agent Note：客户控制台已结算余额的浏览器验证

## 问题

[账户资金 Spec §4、A7](../../../../docs/specs/0002-account-funds-and-reservations.md) 要求客户控制台只显示已结算余额，预授权建立或释放都不改变这个读数，页面不出现可用额、持有中或单笔预授权金额；[控制台 Spec C7](../../../../docs/specs/0001-admin-and-customer-consoles.md) 同一条。这个行为只有真实浏览器能观测，而 `apps/web` 的 e2e 装置只起 API、**不起 Worker**——预授权与实收都要 Worker 参与才会发生。

## 决定

- 客户概览只渲染 `balance_microusd`，标题为“已结算余额”；移除“持有中”卡片，不渲染 `held_microusd` 或可用额。
- “预授权 30”用**真实受理**造：把 e2e 的 API 进程超时链压到 5 秒（`apps/web/e2e/start-api.mjs`），同步入口在窗口后回 504，请求停在持有中。生产缺省的基础超时是 180 秒、对客窗口 390 秒，浏览器用例等不起；四环（窗口 ≥ 上游上限 ≤ 租约）仍按 `crates/application/src/request_timeout.rs` 的校验自洽。
- “实收 20 显示 80”与“仅释放不改变读数”由 `apps/web/e2e/account-state.ts` 把事务**结果**摆进 e2e 库：结算改 `ledger.accounts` 的余额与占用、Hold 转 `captured` 并补一条指向该 Job 的 `capture`；释放只减占用、Hold 转 `released`。用 e2e 自己的 `postgres` 客户端（与 `ensure-database.mjs` 同一个依赖），不引入新装置。

## 备选方案

- **起 Worker + 假上游 vs 摆事务结果**：选摆结果。起 Worker 要给 e2e 加一个假上游 Node 服务、把 Worker 进程塞进 webServer 启动链，还要让夹具算出的实收恰好是 20 元；对一个只断言“页面读哪个数”的用例是过度装置。真结算事务由 `apps/api/tests/http_contract/cases_lifecycle.rs` 用真 Worker + 假上游覆盖。
- **把超时链压到秒级 vs 用生产缺省**：选压到秒级。缺省对客窗口 390 秒，用例等不到那次 504，也就造不出停在持有中的请求。
- **不覆盖实收与释放、只报告缺口 vs 摆结果补齐**：选摆结果。Spec A7 把“实收 20 后为 80、仅释放不改变读数”写成可判定结果，摆结果能验页面这一层，事务层另有合同用例。

## 后果

- e2e 的 API 进程与生产缺省超时不同；这只作用于 Playwright 拉起的测试进程，随 webServer 一起收。
- `account-state.ts` 的写入是测试夹具、不经过 API；它只摆合法的事务结果，不替代 `cases_lifecycle` 对真结算的验证。

## 验证

- `npx playwright test` 连跑两次，32 passed；`portal-settled-balance.spec.ts` 三条：预授权 30 后仍显示 100、仅释放仍显示 100、实收 20 后显示 80，且页面不出现可用额、持有中或预授权。
- `cargo fmt --check` 通过；`cargo test -p seeai-application -p seeai-persistence` 通过（application 141、persistence 7）；合同套件 `cases_public_surface` 4 passed。
