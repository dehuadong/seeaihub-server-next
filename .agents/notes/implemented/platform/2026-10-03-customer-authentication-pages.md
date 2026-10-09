---
title: 客户认证页面拆分与一次性重置结果反馈
status: implemented
created: 2026-10-03
updated: 2026-10-05
approval: 用户接受 Spec 0004 v1 与 RFC 0016 v1，并明确授权执行实现（2026-10-05）
verification: npx playwright test e2e/portal-auth.spec.ts e2e/portal-self-service.spec.ts e2e/admin-opens-a-customer.spec.ts e2e/portal-navigation.spec.ts（32 passed）；npm run typecheck/build --prefix apps/web；vite dev 深链 curl
---

# Agent Note：客户认证页面拆分与一次性重置结果反馈

## 问题

改造前的认证页是单页 `Auth.tsx`（现拆为 [auth/](../../../../apps/web/src/portal/auth/)），同时展示登录/注册、凭重置码设置密码及充值、探活说明，找回路径没有独立入口。用户需要每个页面只完成当前认证任务；基础身份能力仅支持客服转交一次性码，不能把页面拆分包装成邮件自助找回。

## 决定

历史行为依据见[历史客户认证 Spec](../../../../docs/specs/0004-customer-authentication-pages.md)，组件、路由和请求处理由[认证页面 RFC](../../../../docs/design/0016-customer-authentication-pages.md)拥有。本记录保存选择理由、备选与后果，不重复合同。

三个独立地址让刷新与前进后退恢复当前任务；注册保留为登录页的局部切换，不扩成注册流程改版。回跳目标存当前标签页的受保护地址与恰好一对日期区间（`since`/`until`），目标不进入认证地址。重置兑换只认 204，其余 2xx 归为结果未知；共享传输新增可选 `expectedStatus`，默认行为不变。

地址订阅改为模块级共享快照：`pushState`/`replaceState` 不触发 `popstate`，各组件各持局部状态会让公开登录页替换到回跳目标后外壳仍停在登录地址，守卫再把页面抢回概览。

[现有重置实现](../../../../crates/application/src/lib.rs)先消费重置码再写密码，且一次性接口没有查询完成结果能力。响应丢失或服务端写入失败后重提同一码不能证明密码是否更新，界面提示尝试新密码登录或联系客服取得新码；本次未改后端事务。

## 备选方案

- 登录页折叠重置表单或用弹窗：改动少，但没有独立刷新与历史定位，也继续让登录页承载重置任务。
- 只拆登录与重置两页：少一次点击，但没有专门解释如何取得重置码的地方，与选定的忘记密码引导页面不一致。
- 增加邮件申请重置：符合常见产品习惯，但当前没有邮件发送、邮箱验证与对应安全合同，需要另行设计，不能由界面假装已具备。
- 使用认证地址查询参数保存回跳：易于复制共享，但需持续暴露日期区间且更容易误收外部地址；当前仅需同一标签页内恢复，选用带校验的标签页存储。
- 各组件各持 `usePortalRoute` 局部状态：改动小，但导航不跨组件生效；改用共享快照。

## 后果

没有具体客服联系方式时，页面只能指导使用客户已有的联系渠道；配置客服入口需另行确定。主动刷新仍会失去秘密字段，需要重新输入。跨标签页不传递回跳目标；有会话重置另一个客户的密码后会清除当前标签页客户会话，带来一次重新登录。后端局部失败可能使码已消耗但密码未更新，客服签发新码是恢复路径，前端不能保证旧码可重试。公开鉴权端点的有界尝试限制（S5）不在本次交付内，由独立工单承接。

## 验证

浏览器用例 `apps/web/e2e/portal-auth.spec.ts` 覆盖三个页面分离与文案、公开地址直达/刷新/尾斜杠/前进后退、登录回跳与非法目标拒绝、重复日期参数、表单校验与单次提交、确认密码不传送、400/404/429/5xx/响应丢失/非约定 2xx 的结果语义、晚到响应隔离、有会话访问边界、窄屏无横向溢出与字段可访问名；`portal-self-service.spec.ts` 覆盖真实重置码兑换、旧密码失效与改密码错误文案；`admin-opens-a-customer.spec.ts` 与 `portal-navigation.spec.ts` 覆盖运营开户与受保护页导航回归。静态与构建证据是 `npm run typecheck --prefix apps/web` 与 `npm run build --prefix apps/web`（含产物隔离核对）；开发深链在 vite dev 上对 `/login`、`/login/`、`/forgot-password/`、`/reset-password/`、`/usage/` 均回 200 且含 `#root`。
