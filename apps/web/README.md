# 前端：一个工程、两个入口

这个工程产出**两个入口产物**，服务两拨人：

| 入口 | 源码 | 产物 | 给谁 |
| --- | --- | --- | --- |
| `console.html` | `src/console/` | `dist/console.html` | 运营后台（管理员） |
| `portal.html` | `src/portal/` | `dist/portal.html` | 客户控制台（客户） |

`src/shared/` 放两边共用的骨架：HTTP 调用与错误解析、金额与时间格式、加载三态与通用展示组件。
**会话的存放不在共享层**：管理端用 `seeai.console.session`、客户端用 `seeai.portal.session`，各写各的，
同一个浏览器同时开两个控制台不会串。

## 跑起来

需要一个在跑的 API 进程（并准备好数据库）：

```sh
# 后端（另一个终端）。ADMIN_EMAIL/ADMIN_PASSWORD 给的是**引导**用的初始账号：只在账号不存在时写入。
DATABASE_URL=postgres://seeai:seeai@127.0.0.1:54329/seeai_next \
API_BIND=127.0.0.1:8081 ADMIN_TOKEN=... \
ADMIN_EMAIL=ops@example.com ADMIN_PASSWORD=... cargo run -p seeai-api

# 前端
cd apps/web
npm install
npm run dev
```

开发期 Vite 把 `/api`、`/v1` 与 `/health` 代理到 `127.0.0.1:8081`，所以浏览器不需要跨域、API 也不必
开 CORS。两个入口在同一个 dev 服务上：`/console.html` 与 `/portal.html`。

## 构建

```sh
npm run build     # tsc --noEmit && vite build -> dist/ 下的两份产物
```

产物是静态文件，由 API 按**主机名**分发：管理主机回 `console.html` 那一份、客户主机回 `portal.html`
那一份，未命中任何 API 路径的请求回各自的入口 HTML（深链）。**未注册的 `/api/...` 与 `/v1/...` 路径
仍然回既有的 JSON 404**——兜底不吃 API 的 404。

两份产物互不引用：`portal.html` 只引客户那一份脚本，管理端的代码不在里面（反之亦然）。这条是
Spec D4 要的性质，改完 `vite.config.ts` 的入口或共享层之后值得用下面两条再核一次：

```sh
# 客户产物里不该出现管理端的端点名
grep -c 'gateway-models' dist/assets/portal-*.js      # 期望 0
# 管理产物里不该出现对客自助的路径
grep -c '/v1/customer/' dist/assets/console-*.js      # 期望 0
```

## 边界

- 管理端只调 `/api/v1/*`；客户端只调 `/v1/customer/*`。服务端按凭据判权，前端分包只解决"不该送到
  浏览器的代码不送过去"。
- 未登录时两个控制台都**只**渲染登录/注册页：账户与管理的取数组件在那之前不挂载，因此不会发出任何
  取数请求。
- 平台**没有**在线支付：充值由运营在后台完成，客户控制台只展示余额与充值记录。客户也**不能**自助
  重置口令——由运营签发一次性重置令牌后转交。
- 金额一律以**微单位**传输与判断，只在展示层换算成元（`src/shared/routes.ts` 的 `yuan`）。
