# 运营后台（管理 API 的界面）

这个前端是**管理 API 的界面**：运营在这里加网关模型、定价与倍率、排候选顺序与权重、维护折算率与
路由策略、看账户与密钥、处理对账与成本缺口。它不新增后端能力——每个页面调用的端点都在
`apps/api/src/main.rs` 的路由表里，页面只是把"运营要做的事"组织成能点的东西。

对客那一侧（客户用自己的 API Key 调 `/v1/images/generations` 等）不在这个应用里：客户端是调用方
自己的程序，不需要我们的界面。

## 跑起来

需要一个在跑的 API 进程（并准备好数据库与折算率）：

```sh
# 1) 后端（另一个终端）。DATABASE_URL 指向你自己的库；ADMIN_TOKEN 是这个后台要填的令牌。
DATABASE_URL=postgres://seeai:seeai@127.0.0.1:54329/seeai_next \
API_BIND=127.0.0.1:8081 ADMIN_TOKEN=... cargo run -p seeai-api

# 2) 前端
cd apps/web
npm install
npm run dev          # http://localhost:5173
```

开发期由 Vite 把 `/api` 与 `/health` 代理到 `127.0.0.1:8081`（见 `vite.config.ts`），所以浏览器
不需要跨域、API 也不必开 CORS。后端不在 8081 时改代理目标即可。

打开页面后填 `ADMIN_TOKEN`：它只存在这个标签页的 `sessionStorage` 里，关掉标签页就没了，也不进
构建产物。

## 页面与它调用的端点

| 页面 | 端点 |
| --- | --- |
| 网关模型 | `GET /api/v1/gateway-models`；`PATCH /api/v1/gateway-models/{name}`（启停型号）；`PATCH /api/v1/offerings/{id}`（启停候选） |
| 发布修订 | `POST /api/v1/runtime-revisions` |
| 折算率 | `PUT /api/v1/fx-rates` |
| 路由策略 | `GET/PUT /api/v1/route-policies` |
| 账户与密钥 | `POST /api/v1/accounts`、`POST /api/v1/accounts/{id}/credits`、`PUT /api/v1/accounts/{id}/tag`、`GET /api/v1/accounts/{id}`、`GET /api/v1/accounts/{id}/entries`、`POST /api/v1/accounts/{id}/api-keys`、`DELETE /api/v1/api-keys/{id}` |
| 对账与诊断 | `GET /api/v1/reconciliation-cases`、`POST /api/v1/reconciliation-cases/{job_id}/refund`、`GET /api/v1/provider-failures`、`GET /api/v1/provider-cost-gaps` |

**发布修订**一页不做表单化改写：发布命令里的合同与候选是结构化数据，拆成几十个输入框会让人以为
平台在替它做决定。这一页只负责提交、把平台的校验原话显示出来，并指向"网关模型"页核对结果。

**账户一页没有"账户列表"**：管理 API 今天只有 `POST /api/v1/accounts`，没有"列出账户"这一条，
所以这一页按 id 粘贴查询，不假装能列出来。

## 构建与部署

```sh
npm run build        # tsc --noEmit && vite build -> dist/
```

产物是静态文件。生产建议与 API **同源**：由反代把 `/api` 与 `/health` 转给 API 进程、其余路径回
`dist/`（含一条"未知路径回 index.html"的规则，供 hash 路由之外的深链使用）。

## 边界

- 不带登录态：服务端今天只有 `ADMIN_TOKEN` 这一种管理员身份。真实登录（会话/口令）是一次独立的
  后端工作，与这个界面无关。
- 管理 API 今天没有"列账户""列密钥"这类读端点；界面如实反映这一点，不自己造数据。
- 金额一律以**微单位**传输与判断，只在展示层换算成元（`src/routes.ts` 的 `yuan`）。
