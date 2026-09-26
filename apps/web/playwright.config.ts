import { defineConfig, devices } from '@playwright/test';
import { settings } from './e2e/settings';

/// 端到端测试配置：一条命令自己把**空库 → 迁移 → API → 两份产物**都准备好，跑完即收。
///
/// 为什么跑**构建产物**而不是 dev server：这个前端有两个入口，产物怎么被分发、哪个入口取到哪份脚本，
/// 只有构建之后才成立（`docs/design/0010` §4.5 与 U7）。dev server 下两个入口都是从源码现编的，
/// 验不到分发那一层。
///
/// 主机名与凭据由 `e2e/settings.ts` 统一决定：spec 跑在 worker 进程里，**读不到**这里写进 `process.env`
/// 的东西，所以两边必须从同一个模块取值（踩过：口令取岔了，界面报"邮箱或口令不对"）。
const baseURL = `http://admin.localhost:${settings.port}`;

export default defineConfig({
  testDir: './e2e',
  // 只收 `*.spec.ts`。`.capture.ts` 是**抓图脚本**（给界面留档用，供人眼看），它不是断言，
  // 跑 e2e 时不该被执行——需要时单独跑 `npx playwright test e2e/capture-screenshots.capture.ts`。
  testMatch: '**/*.spec.ts',
  // 每个用例都会建客户、跑请求；共用一份库时彼此会看见对方的数据，串行跑最省事也最诚实。
  workers: 1,
  fullyParallel: false,
  // CI 上留一次重试：真失败会重试后仍失败，而偶发的时序抖动不会把整条流水线染红。本地不重试——
  // 本地要的是"同一条命令连跑两次都绿"，重试会把这件事藏起来。
  retries: process.env.CI ? 1 : 0,
  timeout: 60_000,
  expect: { timeout: 10_000 },
  reporter: process.env.CI ? [['github'], ['html', { open: 'never' }]] : [['list']],
  use: {
    baseURL,
    trace: 'on-first-retry',
    screenshot: 'only-on-failure',
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: {
    // 顺序是刻意的：先把库重置成**空库**（模板里可能有历史行），再构建两份产物，最后起 API——
    // API 启动路径上自己跑迁移（`HubRepository::migrate`）。
    command: 'node e2e/ensure-database.mjs && npm run build && node e2e/start-api.mjs',
    url: `http://127.0.0.1:${settings.port}/health`,
    reuseExistingServer: !process.env.CI,
    timeout: 300_000,
    stdout: 'pipe',
    stderr: 'pipe',
    env: {
      // 数据连接与凭据都由这里**显式**交给 webServer 起的那些进程：spec 从 `e2e/settings.ts` 取同一组值，
      // 两边不会取岔。`SEEAI_E2E_DATABASE` 也一并给，`ensure-database.mjs` 与 `start-api.mjs` 都读它。
      SEEAI_E2E_DATABASE: settings.database,
      SEEAI_E2E_API_BIND: `0.0.0.0:${settings.port}`,
      SEEAI_E2E_ADMIN_EMAIL: settings.adminEmail,
      SEEAI_E2E_ADMIN_PASSWORD: settings.adminPassword,
      SEEAI_E2E_ADMIN_TOKEN: settings.adminToken,
    },
  },
});
