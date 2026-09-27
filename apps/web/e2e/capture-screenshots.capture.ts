import { test } from '@playwright/test';
import { adminApiUrl, consoleUrl, portalUrl, settings } from './settings';

/// 给两套界面的各个页面抓整页截图，落到 `apps/web/screenshots/`。
///
/// 它是**给人看的留档**：界面好不好看、间距对不对，是 spec 断言不了的（根 `AGENTS.md` 的
/// 「浏览器行为」把这一条划给人眼）。所以这里不做断言，只负责把"现在长什么样"固定下来。
///
/// 它与 e2e 分开配置（`playwright.capture.config.ts`）：`npm run e2e` 只跑断言，`npm run capture`
/// 只抓图。合成一份的话抓图会混进常规运行、白花时间还把用例总数搞乱。
const CONSOLE_PAGES = ['模型目录', '账户', '客户', '对账与诊断', '折算率', '路由策略'] as const;

test('运营后台六个页面各抓一张截图', async ({ page, request }) => {
  // 先造一点数据，免得每页都是空态：一个币种的折算率 + 一个带余额的账户。
  await request.put(`${adminApiUrl}/api/v1/fx-rates`, {
    headers: { authorization: `Bearer ${settings.adminToken}` },
    data: { currency: 'USD', rate_micros: 7_100_000 },
  });
  await request.post(`${adminApiUrl}/api/v1/accounts`, {
    headers: { authorization: `Bearer ${settings.adminToken}` },
    data: { initial_credit_microusd: 25_000_000 },
  });

  await page.goto(consoleUrl);
  await page.screenshot({ path: 'screenshots/00-运营后台-登录页.png', fullPage: true });
  await page.getByTestId('admin-email').fill(settings.adminEmail);
  await page.getByTestId('admin-password').fill(settings.adminPassword);
  await page.getByTestId('admin-sign-in').click();
  await page.locator('.ant-layout-sider').getByRole('menuitem', { name: '模型目录' }).waitFor();

  for (const [index, label] of CONSOLE_PAGES.entries()) {
    await page.locator('.ant-layout-sider').getByRole('menuitem', { name: label }).click();
    await page.waitForTimeout(700);
    await page.screenshot({
      path: `screenshots/0${index + 1}-运营后台-${label}.png`,
      fullPage: true,
    });
  }

  // 「上架新模型」的表单开在模型目录的抽屉里，它也是要给人看的一屏。
  await page.locator('.ant-layout-sider').getByRole('menuitem', { name: '模型目录' }).click();
  await page.getByTestId('models-add').click();
  await page.waitForTimeout(700);
  await page.screenshot({ path: 'screenshots/07-运营后台-上架新模型.png', fullPage: true });
});

test('客户控制台的登录页与控制台各抓一张截图', async ({ page }) => {
  await page.goto(portalUrl);
  await page.screenshot({ path: 'screenshots/10-客户控制台-登录页.png', fullPage: true });

  await page.getByTestId('portal-mode-register').click();
  await page.getByTestId('portal-email').fill(`shot-${Date.now()}@example.com`);
  await page.getByTestId('portal-password').fill('a-long-enough-password');
  await page.getByTestId('portal-submit').click();
  // 控制台渲染出来的标志是首屏那三个数（不依赖当前停在哪个标签页）。
  await page.locator('.ant-statistic').first().waitFor();
  await page.waitForTimeout(700);
  await page.screenshot({ path: 'screenshots/11-客户控制台-控制台.png', fullPage: true });
});
