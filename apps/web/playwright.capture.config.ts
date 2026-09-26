import { defineConfig, devices } from '@playwright/test';
import { settings } from './e2e/settings';

/// **抓图用的配置**，与 `playwright.config.ts`（跑 e2e）分开。
///
/// 为什么不合成一份：`testMatch` 一收 `.capture.ts`，抓图脚本就会混进 `npx playwright test` 的常规
/// 运行里——它不是断言，跑起来白花时间还会把"用例总数"搞乱。分开之后：
///
/// - `npm run e2e` —— 只跑断言（`*.spec.ts`）；
/// - `npm run capture` —— 只抓图（`*.capture.ts`），产物落 `screenshots/`，供人眼看界面。
///
/// `AGENTS.md` 的「浏览器行为」把"好不好看、间距对不对"划给人眼，这一份就是给那一类判断准备材料。
export default defineConfig({
  testDir: './e2e',
  testMatch: '**/*.capture.ts',
  workers: 1,
  timeout: 120_000,
  reporter: [['list']],
  use: {
    baseURL: `http://admin.localhost:${settings.port}`,
    viewport: { width: 1440, height: 900 },
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: {
    command: 'node e2e/ensure-database.mjs && npm run build && node e2e/start-api.mjs',
    url: `http://127.0.0.1:${settings.port}/health`,
    reuseExistingServer: !process.env.CI,
    timeout: 300_000,
    stdout: 'pipe',
    stderr: 'pipe',
    env: {
      SEEAI_E2E_DATABASE: settings.database,
      SEEAI_E2E_API_BIND: `0.0.0.0:${settings.port}`,
      SEEAI_E2E_ADMIN_EMAIL: settings.adminEmail,
      SEEAI_E2E_ADMIN_PASSWORD: settings.adminPassword,
      SEEAI_E2E_ADMIN_TOKEN: settings.adminToken,
    },
  },
});
