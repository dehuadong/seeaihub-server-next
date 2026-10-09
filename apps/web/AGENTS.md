## 通用约定
- 不要把系统内部逻辑带到客户侧
- 文档属主、历史来源与未完成切换的范围读取仓库统一入口 [`docs/AGENTS.md`](../../docs/AGENTS.md#历史归属切换)。

## 浏览器行为：跑 `npx playwright test`

**判断标准一句话：这个改动的正确性，是不是只有真实浏览器才观测得到？** 答"是"才写 spec——路由守卫、表单校验与联动、
会话存续（cookie / `sessionStorage`）、构建产物级的静态资源与分发、全栈串联的主路径、重构后的回归护栏。
接口契约、状态码、鉴权规则归 `apps/api/tests/http_contract`；纯函数与后端逻辑归各自的单元测试；
"好不好看、间距对不对"归人眼，列成清单交给人验，不假装脚本能替人判断。

1. 配置与用例在 `apps/web`：`playwright.config.ts` 与 `e2e/*.spec.ts`。**一条命令自己拉起环境**——
   `ensure-database.mjs` 把库重置成空库、`npm run build` 出两份产物、`start-api.mjs` 起 API（启动时自己跑迁移），
   跑完即收。不需要先开终端准备任何东西。
2. 跑 `npm run e2e --prefix apps/web`（或在该目录下 `npx playwright test`）；只跑一条就 `npx playwright test e2e/xxx.spec.ts`。
3. 每个新的界面行为或界面 bugfix 都带一个能复现它的 spec，和业务代码一起提交。修 bug 先红后绿：先让 spec 复现问题，再改代码。
4. 选择器用 `getByTestId` / `getByRole`；缺稳定锚点就在组件上补 `data-testid`，不用脆弱的 CSS 层级或文案。
5. 不写界面单元测试（按钮渲染、className、快照）；界面行为由 spec 覆盖。
6. 红的处理顺序：先看产物（`npx playwright show-trace`、失败截图、`npx playwright show-report`）→ 给关键请求与状态加日志或把用例拆小 →
   改业务代码 → 重跑**同一条命令**。交付判据是同一条命令连跑两次都绿，不靠 retries 兜底。
7. 主机名用 `*.localhost`（`admin.localhost` 回运营后台、`app.localhost` 回客户控制台）：Chrome 把它们解析到回环，
   所以不用改 hosts，而 `admin.` 前缀正好命中生产的分发判据。**Node 的解析器不认 `.localhost`**——spec 里用
   `request` 这类走 Node 的调用要直连 `127.0.0.1`。
