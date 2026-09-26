import { defineConfig, devices } from '@playwright/test';

/// 端到端测试配置：一条命令自己把**空库 → 迁移 → API → 两份产物**都准备好，跑完即收。
///
/// 为什么跑**构建产物**而不是 dev server：这个前端有两个入口，产物怎么被分发、哪个入口取到哪份脚本，
/// 只有构建之后才成立（`docs/design/0010` §4.5 与 U7）。dev server 下两个入口都是从源码现编的，
/// 验不到分发那一层。
///
/// 主机名用 `admin.localhost`：Chrome 把 `*.localhost` 都解析到回环，所以**不用改 hosts**，而
/// `admin.` 前缀正好命中生产那条分发判据（`apps/api/src/main.rs` 的 `StaticSpa::serve`）。
/// 测试因此不依赖任何只给测试用的分支。
const port = Number(process.env.SEEAI_E2E_PORT ?? 8090);
const baseURL = process.env.SEEAI_E2E_BASE_URL ?? `http://admin.localhost:${port}`;

export default defineConfig({
  testDir: './e2e',
  testMatch: '**/*.spec.ts',
  // 每个用例都会建客户、跑请求；共用一份库时彼此会看见对方的数据，串行跑最省事也最诚实。
  workers: 1,
  fullyParallel: false,
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
    url: `http://127.0.0.1:${port}/health`,
    reuseExistingServer: !process.env.CI,
    timeout: 300_000,
    stdout: 'pipe',
    stderr: 'pipe',
    env: {
      SEEAI_E2E_API_BIND: `0.0.0.0:${port}`,
      SEEAI_E2E_ADMIN_EMAIL: 'ops@example.com',
      SEEAI_E2E_ADMIN_PASSWORD: 'e2e-admin-password',
    },
  },
});
