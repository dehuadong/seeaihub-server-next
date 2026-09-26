---
name: playwright-e2e
description: 为一个项目配置 Playwright E2E——装依赖、写 config、把「验证只走 playwright test」落进 AGENTS.md、写出第一个 spec。
argument-hint: "项目路径，或要覆盖的功能 / bug"
---

# Playwright E2E 配置

分工是固定的：**Agent 写 spec，Playwright runner 跑 spec。** 提需求的这一轮里，Agent 不打开浏览器一格格点页面——那是它最贵、最慢、最不可回归的用法；它只写 Node.js + Playwright 脚本，然后跑命令。脚本一旦落盘，「这个功能对不对」就从一轮轮截图变成一条几秒钟、几乎不花 token、随时可重跑的断言。

这个 skill 一次跑完，产出三样**落盘**的东西：可跑的 `playwright.config.ts`、项目里长期生效的测试规则、覆盖目标行为的 spec。之后日常由项目里的规则接管，换项目、换技术栈、或规则要改的时候再调它。

## 闸门：这件事归 Playwright 吗

进来先分一次工，别把整个项目铺成 spec。判据一句话：**这个改动的正确性，是否只有通过真实浏览器才观测得到？** 答「是」的才写 spec；答「不是」的走下面右列那一层，更快也更准。

| 要验的东西 | 归谁 |
| --- | --- |
| UI 行为：路由守卫、表单校验与联动、弹窗抽屉、分页筛选、状态回填 | spec |
| 全栈串联：登录 → 下单 → 后台看到订单；不同权限看到不同菜单 | spec |
| 只有浏览器里才成立的：cookie / session / localStorage、CORS、重定向链、OAuth 回调落地、SSE / WebSocket 的界面表现 | spec |
| 浏览器专属能力：上传下载、拖拽、剪贴板、iframe、多标签页 | spec |
| 构建产物级 bug：只有 `build` 后才有的水合失败、静态资源 404 | spec（服务用 `command: 'pnpm build && pnpm preview'`） |
| 回归护栏：重构后功能仍在——它最值钱的一类 | spec，只覆盖关键路径 |
| 纯函数、算法、数据转换、后端业务逻辑 | 单元测试（vitest / jest / pytest） |
| 接口契约、状态码、鉴权规则 | 直接打 HTTP 的集成测试（supertest / pytest） |
| CLI、SDK、库 | 各自的单元 / 集成测试 |
| 原生移动端 App | Appium / Maestro |
| 性能压测、Lighthouse 审计 | k6 / Lighthouse |
| 好不好看、间距对不对 | 人眼：列成清单加 Todo 交给人验 |

三个边界要当场说清，否则后面会一直红或白配一套：

- 项目里根本没有浏览器可达的界面（纯 CLI、纯库、纯后端）：本技能到此为止，直接告诉用户该走哪一层。
- Electron 和浏览器扩展 Playwright 也支持，但配置形状不同（`_electron.launch()` / `--load-extension`），不是这份 config 的形状；要测就单独配。
- 第三方登录、验证码、短信、支付：不是不归它，是**先得有测试环境的后门或桩**（见 [reference/patterns.md](reference/patterns.md) 的网络桩），否则脚本永远红。

**完成判据**：这次要验的每一条都能说出归哪一层；判给 spec 的那些列出来，作为第 4 步的输入。

## 0. 从项目里读事实

先确定四件事，全部来自文件，不许凭印象填：

| 事实 | 从哪读 |
| --- | --- |
| 包管理器 | 锁文件：`pnpm-lock.yaml` / `yarn.lock` / `package-lock.json` / `bun.lockb` |
| 起开发服务的命令和端口 | `package.json` 的 `scripts`、`.env`、框架约定的默认端口 |
| 是否已有 Playwright | 依赖里的 `@playwright/test`、`playwright.config.*`、`tests/`、`e2e/` |
| 目标行为是否要登录态 | 路由和中间件、现有 spec 里的 `storageState` |

**完成判据**：四件事都能指到具体文件；下面每一步的命令都直接用这些事实拼出来，不再回头问用户。

## 1. 装 Playwright 与浏览器

Playwright **不用系统浏览器**：它按自己的版本号下载专用构建到系统缓存（Windows `%USERPROFILE%\AppData\Local\ms-playwright`，macOS `~/Library/Caches/ms-playwright`，Linux `~/.cache/ms-playwright`）。**Playwright 版本和浏览器构建是绑定的**，升级 `@playwright/test` 之后要重跑 `install`；机器上装没装 Chrome 与它无关。

**装在哪**：`@playwright/test` 装进项目的 `devDependencies`——它决定浏览器版本，必须和 CI 一致，所以不要全局装（本地没装时 `npx` 会去下载一份临时版本，静默换掉版本）。浏览器相反：只有一份，装在机器级缓存里被所有项目共享，不必也无法按项目隔离，更不要为了「干净」重下一次。

依赖里没有 `@playwright/test` 才装（已有就跳过），并装上这次要用的浏览器：

```bash
pnpm create playwright            # 或 npm init playwright@latest
npx playwright install chromium
npx playwright install --dry-run  # 只打印：装到哪、从哪个 URL 下载，不下载
```

`install` 幂等，已经有了会直接跳过，所以通常「直接装」比「先检测再装」更省事；要确认现状就用 `--dry-run` 和 `--list`。按情况选：

| 情况 | 命令 |
| --- | --- |
| 本地只跑无头 | `npx playwright install chromium --only-shell`（不装完整 Chromium：win64 上 205MB → 120MB） |
| CI | `npx playwright install --with-deps chromium`（`--with-deps` 一并装 Linux 系统依赖） |
| 要跨浏览器 | `npx playwright install chromium firefox webkit` |
| 国内下载慢 | `PLAYWRIGHT_DOWNLOAD_HOST=https://cdn.npmmirror.com/binaries/playwright`（已验证同步 cft 新路径，与官方同一份文件）；仍慢再加 `PLAYWRIGHT_DOWNLOAD_CONNECTION_TIMEOUT=120000` |
| 走公司代理 / 内网镜像 | `HTTPS_PROXY=...`、`PLAYWRIGHT_DOWNLOAD_HOST=<内网地址>`；证书报错加 `NODE_EXTRA_CA_CERTS` |
| 缓存位置要固定（容器、多版本共存） | `PLAYWRIGHT_BROWSERS_PATH=/path`；设成 `0` 则装进 `node_modules`，做 hermetic 安装 |

两个下载源互相独立，别搞混：`pnpm add -D @playwright/test` 走 npm registry（慢就换 `registry.npmmirror.com`），浏览器二进制走微软 CDN（要设 `PLAYWRIGHT_DOWNLOAD_HOST`）。配了一个不影响另一个。

**要不要改用机器上的 Chrome / Edge**：默认不要。需要对着用户实际在用的浏览器做回归、或涉及媒体编解码器差异时，给 project 加 `channel: 'chrome'` / `channel: 'msedge'`，运行时用的就是系统里装的品牌浏览器（注意 `npx playwright install msedge` 会装到操作系统的全局默认位置并覆盖现有安装）。Firefox 和 WebKit 没有这条路——Playwright 用的是自己打过补丁的构建，品牌版 Firefox / Safari 跑不了。

**完成判据**：`npx playwright install --list` 里能看到这次要用的浏览器，且 `npx playwright --version` 有输出。

## 2. 写 `playwright.config.ts`

配置的任务只有一个：**一条命令自己把服务拉起来、跑完、留下产物**。骨架（命令和端口换成第 0 步读到的事实）：

```ts
import { defineConfig, devices } from '@playwright/test'

export default defineConfig({
  testDir: './tests',
  use: {
    baseURL: 'http://localhost:3000',
    trace: 'on-first-retry',
    screenshot: 'only-on-failure',
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: {
    command: 'pnpm dev',
    url: 'http://localhost:3000',
    reuseExistingServer: !process.env.CI,
  },
})
```

服务已经在别处跑、monorepo 多服务、跨 spec 复用登录态、CI 的 retries / 分片 / 报告、GitHub Actions workflow——按分支读 [reference/config.md](reference/config.md)。

**完成判据**：`npx playwright test --list` 找得到测试目录，且不用人手先开一个终端——`webServer` 能自己把服务起起来。

## 3. 把规则落盘进项目

写进项目**已经在用**的那个 agent 指令文件（`AGENTS.md` 优先，其次 `CLAUDE.md`、`.cursorrules`）；都没有就新建 `AGENTS.md`。整块贴进去：

```md
## E2E 测试规则

1. UI 行为的验证一律跑 `npx playwright test`；只跑一个文件就 `npx playwright test tests/xxx.spec.ts`。
2. 每个新功能或 bugfix 都要带一个能复现它的 spec（`tests/**/*.spec.ts`），和业务代码一起提交。
3. 脚本失败时按这个顺序走：先让脚本稳定复现问题（红）→ 给关键请求、状态、时序加日志，把步骤拆小 → 改业务代码 → 重跑同一条脚本，直到绿。
4. 选择器用 `getByRole` / `getByLabel` / `getByTestId`；缺稳定锚点就在组件上补 `data-testid`，不用脆弱的 CSS 层级。
5. 不写 UI 单元测试（按钮渲染、className、快照）；UI 行为由 spec 覆盖。
6. 浏览器工具和 MCP 用来看产物——`npx playwright show-trace`、失败截图、`npx playwright show-report`；驱动页面的事务归 Playwright runner。
7. 需要人眼判断的视觉细节，列成清单加 Todo 交给人验，不假装脚本能替人判断好不好看。
8. 业务逻辑、纯函数、接口契约仍然由各自的单元 / 集成测试覆盖；spec 只负责只有浏览器能观测到的那部分。
```

**完成判据**：规则块出现在项目实际使用的那个文件里，`git diff` 里看得见。

## 4. 写第一个 spec

覆盖用户这次点名的功能或 bug。脚本里全是确定步骤，没有「试试看」：

```ts
import { test, expect } from '@playwright/test'

test('用户可以登录并进入工作台', async ({ page }) => {
  await page.goto('/login')

  await page.getByLabel('邮箱').fill('demo@example.com')
  await page.getByLabel('密码').fill('password123')
  await page.getByRole('button', { name: '登录' }).click()

  await expect(page).toHaveURL(/\/dashboard/)
  await expect(page.getByRole('heading', { name: '工作台' })).toBeVisible()
})
```

需要登录态、网络桩、文件上传、拆步骤、调试开关时读 [reference/patterns.md](reference/patterns.md)。

修 bug 时顺序是**先红后绿**：spec 先跑成失败，确认它复现的正是用户报的那个问题，再动业务代码。

**完成判据**：spec 覆盖闸门判给它的每一条行为；第一次运行的结果是绿，或是因为真实缺陷而红——没有因为「选择器还没定」而 `test.skip` 的用例。

## 5. 跑，红就修，直到连续两次绿

```bash
npx playwright test tests/xxx.spec.ts
```

红的处理顺序固定：

1. 读产物：`npx playwright show-trace test-results/<...>/trace.zip`、失败截图、`npx playwright show-report`。
2. 给关键请求、状态、时序加日志，或把大测试拆成更小步骤。
3. 改业务代码。
4. 重跑**同一条命令**。

**完成判据**：同一条命令连跑两次都绿，不靠 retries 兜底。

## 6. 接进 CI（项目有 CI 才做）

把 `npx playwright install --with-deps`、`npx playwright test`、`playwright-report/` 产物上传写进现有 workflow；失败重试、分片、报告配置见 [reference/config.md](reference/config.md)。

**完成判据**：`CI=1 npx playwright test` 在本地通过（PowerShell 用 `$env:CI=1; npx playwright test`），且 workflow 文件落盘。
