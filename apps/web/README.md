# 前端：一个工程、两个入口

这个工程产出**两个入口产物**，服务两拨人：

| 入口 | 源码 | 产物 | 给谁 |
| --- | --- | --- | --- |
| `console.html` | `src/console/` | `dist/console.html` | 运营后台（管理员） |
| `portal.html` | `src/portal/` | `dist/portal.html` | 客户控制台（客户） |

`src/shared/` 放两边共用的**与界面无关**的东西：HTTP 调用与错误解析（`api.ts`）、金额与时间格式
（`format.ts`）、取数的加载三态（`ui.tsx` 的 `useLoadable`）。**会话的存放不在共享层**：管理端用
`seeai.console.session`、客户端用 `seeai.portal.session`，各写各的，同一个浏览器同时开两个控制台不会串。

## 界面

两套界面都用 **Ant Design**（`antd` 6）。约定：

- 组件、主题、操作反馈统一走 antd：颜色与圆角由各入口的 `ConfigProvider` 一处配，提示走 `App` 提供的
  `message` 上下文；`src/shared/styles.css` 只留基础重置（body 边距、底色、根容器高度），**不写**
  `input`/`button`/`table` 的规则——那是另一套设计系统，会和 antd 冲突。
- 每页的外框用 `console/ui.tsx` 的 `ConsolePage` 与 `Panel`（管理端）或 `Card`（客户端），
  表格用 `Table`、表单用 `Form`、状态用 `Tag`/`Switch`、空态用 `Alert` 写清下一步该做什么。
- `e2e/ui-uses-antd.spec.ts` 按**渲染产物**（`.ant-layout-sider`、`.ant-menu`、`.ant-card` 等）
  断言这件事——不是按源码里有没有 import，否则被换回裸 HTML 时页面还能用、接口还通，只有人眼会发现。

`npm run capture` 给两套界面的各个页面抓整页截图到 `screenshots/`（它用单独的
`playwright.capture.config.ts`，不混进 e2e）。"好不好看、间距对不对"归人眼，这份就是给那类判断准备材料。

## 跑起来

需要一个在跑的 API 进程（并准备好数据库）：

```sh
# 后端（另一个终端）。ADMIN_EMAIL/ADMIN_PASSWORD 给的是**引导**用的初始账号：只在账号不存在时写入。
DATABASE_URL=postgres://seeai:seeai@127.0.0.1:5432/seeai_next \
API_BIND=127.0.0.1:8081 ADMIN_TOKEN=... \
ADMIN_EMAIL=ops@example.com ADMIN_PASSWORD=... cargo run -p seeai-api

# 前端
cd apps/web
npm install
npm run dev
```

开发期 Vite 把 `/api`、`/v1` 与 `/health` 代理到 `127.0.0.1:8081`，所以浏览器不需要跨域、API 也不必
开 CORS。两个入口在同一个 dev 服务上：`/console.html` 与 `/portal.html`。

### 不走 dev 服务时怎么看管理端

生产由 API **按主机名**分发：`admin.<domain>` 回运营后台，其余主机回客户控制台。本机往往没有域名可指
（写 hosts 要管理员权限，容器里更没有），那时设一个显式出口：

```sh
CONSOLE_DEV_HOST=localhost cargo run -p seeai-api   # 于是 http://localhost:8080/ 回管理端
```

缺省不设 —— 不设时分发只看主机名，`localhost` 拿到的是客户入口。**生产不要设它**。

## 构建

```sh
npm run build     # tsc --noEmit && vite build -> dist/ 下的两份产物
```

产物是静态文件，由 API 按**主机名**分发：管理主机回 `console.html` 那一份、客户主机回 `portal.html`
那一份，未命中任何 API 路径的请求回各自的入口 HTML（深链）。**未注册的 `/api/...` 与 `/v1/...` 路径
仍然回既有的 JSON 404**——兜底不吃 API 的 404。

两份产物互不引用：`portal.html` 只引客户那一份脚本，管理端的代码不在里面（反之亦然）。这条是
Spec D4 要的性质，改完 `vite.config.ts` 的入口或共享层之后值得再核一次。**按带引号的精确路径搜，
别用裸路径**——`/v1/customers` 是管理端自己的 `/api/v1/customers` 的子串，用裸路径搜会得到假阳性：

```sh
# 客户产物里不该出现任何管理面端点，也不该有控制台的会话键
grep -c '"\/api\/v1\/' dist/assets/portal-*.js          # 期望 0
grep -c 'seeai.console.session' dist/assets/portal-*.js # 期望 0
# 管理产物里不该出现对客自助端点
grep -c '"\/v1\/customer\/' dist/assets/console-*.js    # 期望 0
```

## 边界

- 管理端只调 `/api/v1/*`；客户端只调 `/v1/customer/*`。服务端按凭据判权，前端分包只解决"不该送到
  浏览器的代码不送过去"。
- 未登录时两个控制台都**只**渲染登录/注册页：账户与管理的取数组件在那之前不挂载，因此不会发出任何
  取数请求。
- 平台**没有**在线支付：充值由运营在后台完成，客户控制台只展示余额与充值记录。客户也**不能**自助
  重置口令——由运营签发一次性重置令牌后转交。
- 金额一律以**微单位**传输与判断，只在展示层换算成元（`src/shared/routes.ts` 的 `yuan`）。
