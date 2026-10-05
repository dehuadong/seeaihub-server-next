---
title: 公开鉴权端点的失败尝试限制与来源信任边界
status: implemented
created: 2026-10-05
updated: 2026-10-05
approval: 用户接受 Spec 0004 v1 并授权执行实现（2026-10-05）；来源维采信哪个头由本次实现决定，写在本记录
verification: HTTP_CONTRACT_DATABASE_URL=... cargo test -p seeai-api --test http_contract -- --ignored --test-threads=1 cases_auth_attempts（8 passed）；cargo clippy -p seeai-api -p seeai-application --all-targets --all-features -- -D warnings；cargo fmt --check
---

# Agent Note：公开鉴权端点的失败尝试限制与来源信任边界

## 问题

客户注册、登录与重置码兑换是三个不需要凭据的公开端点。没有失败计数时，脚本可以无界地试口令或猜重置码；既有的加速层只有"每把密钥的请求速率"，计的是请求数、键是密钥标识，覆盖不到这三个端点。

## 决定

行为由[客户认证 Spec](../../../../docs/specs/0004-customer-authentication-pages.md) §1 S5 与 §5 A11 拥有；计数的键形、判定与写入落点、降级规则由[认证页面设计](../../../../docs/design/0016-customer-authentication-pages.md) §3 拥有，本记录不重复。

本记录保存**来源维的信任边界选择**。[连接层](../../../../apps/api/src/supervisor/connection.rs)给的连接对端地址不可伪造；生产 API 在 nginx 之后时对端是代理本身，因此由运维显式配置采信哪个受信头（`AUTH_SOURCE_HEADER`，例如 `x-real-ip`），未配置时退回对端地址。不采信 `X-Forwarded-For` 的最左值，因为它由客户端自带。

采信受信头的前提是 **API 不能被绕过代理直连**：能直连的调用方可以自带同名头伪造来源。这是部署期边界（网络、防火墙或只监听回环），代码兜不住，写在运维文档由部署守住。

Spec 的“连续失败”按固定窗口内的失败次数实现（设计 §3 选定的固定窗口）：窗口内失败—成功—再失败仍会累计到上限，成功只是不写。

## 备选方案

- 只用连接对端地址：不可伪造，但在代理之后所有客户端共用一个来源，来源维会很快锁住整个平台。
- 采信 `X-Forwarded-For` 最左值：无需额外配置，但最左值由客户端自带，来源维可被伪造。
- 把失败计数做进数据库：计数是保护机制而不是业务事实，写库会把"请求很多"这种形态本身变成数据库写压力。
- 精确的第 N 次原子计数：`CacheStore` 只有读写两条命令、没有 `INCR`；要挡的是数量级异常，不是精确的第 N 次。
- 三端点共用一对计数桶：键更短，但一次登录失败会占用同邮箱注册的额度，也做不到按端点分别调上限与窗口，与 Spec 的“各自”不符，未采用。

## 后果

来源头可被伪造，但只在 API 可被直连绕过代理时成立，靠部署边界消除，本记录明确写下这条前提。管理员重置码兑换端点（`POST /api/v1/admin/password-resets/redeem`）不在 S5 范围内，仍没有失败计数——它属于控制台 Spec 0001 的 A8，需要时由独立工作项承接。无缓存部署不满足 A11，这是 Spec 明确接受的约束。计数不是严格原子（读改写），同一窗口实际能过去的尝试可能略多于上限。

## 验证

[API 合同用例](../../../../apps/api/tests/http_contract/cases_auth_attempts.rs)覆盖：注册、登录与重置码兑换各自计数并返回带 `Retry-After` 的 429；来源与身份两维各自触发；三个端点互不占用额度；等待期内正确凭据同样被拒、窗口过后恢复；成功尝试不累计；注册冲突与重置码兑换同样受限；存在/不存在邮箱与未用/已用/不存在重置码的拒绝一致；无缓存部署按既有无缓存行为放行。浏览器侧由 `apps/web/e2e/portal-auth.spec.ts` 断言页面把 429 显示为可等待的提示。
