import { test } from '@playwright/test';
import { adminApiUrl, consoleUrl, settings } from './settings';

/// 给六个管理页面各抓一张整页截图，落到 `apps/web/screenshots/`。
///
/// 它的用途是**留档与给人看**：界面好不好看、间距对不对，是 spec 断言不了的（`AGENTS.md` 的
/// 「浏览器行为」把这一条划给人眼）。所以这里不做断言，只负责把"现在长什么样"固定下来。
/// 库里是空的，所以大多数页面显示的是空态——那也是一种要留档的状态。
const PAGES = ['网关模型', '发布修订', '折算率', '路由策略', '账户与密钥', '对账与诊断'] as const;

test('六个管理页面各抓一张截图', async ({ page, request }) => {
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
  await page.getByTestId('admin-email').fill(settings.adminEmail);
  await page.getByTestId('admin-password').fill(settings.adminPassword);
  await page.getByTestId('admin-sign-in').click();
  await page.locator('.ant-layout-sider').getByRole('menuitem', { name: '网关模型' }).waitFor();

  for (const [index, label] of PAGES.entries()) {
    await page.locator('.ant-layout-sider').getByRole('menuitem', { name: label }).click();
    await page.waitForTimeout(700);
    const name = `0${index + 1}-${label}`;
    await page.screenshot({ path: `screenshots/${name}.png`, fullPage: true });
  }
});
