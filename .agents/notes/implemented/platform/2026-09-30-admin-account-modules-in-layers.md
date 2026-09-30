---
title: 管理员账户页改成按钮与弹层
status: implemented
created: 2026-09-30
updated: 2026-09-30
approval: 用户选定账户页布局——一页只放找账户与选中行，充值为弹窗、其余三个模块为侧边栏
verification: 见正文「验证」。`cd apps/web && npm run typecheck` 通过；`cd apps/web && npx playwright test` 连跑两次，35 passed。
---

# Agent Note：管理员账户页改成按钮与弹层

## 问题

[信息架构设计 §4.3](../../../../docs/design/0011-console-information-architecture.md) 要求账户页只保留“找账户 + 选中行 + 四个入口”，各模块在弹层里打开；既有页面把充值表单、充值记录、调用明细与 API Key 四块直接摊在选中区下方。金额语义由[账户资金 Spec §4](../../../../docs/specs/0002-account-funds-and-reservations.md) 拥有：管理员可以分别显示已结算余额、持有中与可用额。

## 决定

- 选中区只给四个按钮：充值、充值记录、扣费记录（调用明细）、API Key；四块内容都按一个 `layer` 状态懒挂载，充值输入框在弹窗打开前不存在。
- 充值走 `Modal`；其余三个各走一个 `Drawer`，且 `mask={false}`——侧边栏不挡主页面，用户可以直接点列表换账户。
- 切换账户用选中区组件上的 `key={selected}` 重挂：金额读数、标签表单与当前弹层一起回到初始状态，不必逐个手工关。
- 充值幂等键在打开弹窗与每次成功入账时重生成；充值表单 `preserve={false}`，取消后重开是空的。
- 金额那行分别显示已结算余额、持有中与可用额，直接渲染账户读接口的三个字段，不在页面里重算。

## 备选方案

- **四个按钮共用一个侧边栏（含充值）vs 充值单独走弹窗**：选弹窗。用户已定“充值走弹窗、其余侧边栏”；充值只有一个金额输入，是一次独立动作，不与其他三个只读或管理面板挤在同一容器。
- **侧边栏带遮罩 vs `mask={false}`**：选不挡。带遮罩时用户点不到列表，“切换账户会关闭当前弹层”这条要求没有可达路径；充值弹窗保留遮罩，因为它是一次必须完成或取消的动作。
- **手工 `setLayer(null)` 关弹层 vs `key` 重挂**：选重挂。换账户意味着选中区整体换主体，重挂顺带清掉三个金额、标签表单与弹层；手工关要枚举四个状态，漏一个就会对着旧账户操作。

## 后果

- 弹层内容默认不挂载（`destroyOnHidden`），页面上不会先出现这些容器；但**三个读（余额、充值记录、扣费记录）在选中账户时就取数**——懒挂载省的是 DOM，不是请求。切换账户时旧账户的读数与弹层一起消失（选中区按 `key` 重挂）。
- 侧边栏不挡页面，打开侧边栏时仍可点列表换账户；充值弹窗仍要求先取消或提交。
- 三条既有浏览器用例改成按弹层断言，另加 `admin-account-modules-in-layers.spec.ts` 覆盖四个入口、内容不预挂载、切换账户关弹层与三个金额。

## 验证

- `cd apps/web && npm run typecheck` 通过。
- `cd apps/web && npx playwright test` 连跑两次，35 passed；新增用例 `admin-account-modules-in-layers.spec.ts` 三条（四个入口与内容不预挂载、切换账户关弹层、密钥明文只在签发时出现一次且吊销要确认），改写的 `admin-tops-up-without-the-identifier.spec.ts`、`admin-creates-an-account.spec.ts`、`admin-reads-fixture-data.spec.ts` 覆盖充值弹窗、建账户与三个金额读数。
