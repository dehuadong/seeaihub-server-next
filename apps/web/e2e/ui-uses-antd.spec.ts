import { expect, test, type Page } from '@playwright/test';
import { consoleUrl, portalUrl, settings } from './settings';

/// 两套界面用的都是 **Ant Design**（用户明确要求："用 Ant Design 设计组件"）。
///
/// 为什么要一条断言把这件事钉住：界面"看起来是 antd"很容易在后续改动里被悄悄换回裸 HTML——那时
/// 页面还能用、接口还通、截图也看不出坏，只有人眼会发现"又变简陋了"。这条按 **antd 的渲染产物**
/// （布局与组件的类名、`Menu` 的无障碍角色）判断，不是按源码里有没有 import。
///
/// 它不判断"好不好看"——那一类判断归人眼（见根 `AGENTS.md` 的「浏览器行为」第 7 条与
/// `apps/web/screenshots/` 的留档）。
async function signInConsole(page: Page): Promise<void> {
  await page.goto(consoleUrl);
  await page.getByTestId('admin-email').fill(settings.adminEmail);
  await page.getByTestId('admin-password').fill(settings.adminPassword);
  await page.getByTestId('admin-sign-in').click();
  await expect(page.locator('.ant-layout-sider')).toBeVisible();
}

test('运营后台的外壳与六个页面都渲染 Ant Design 组件', async ({ page }) => {
  await page.goto(consoleUrl);

  // 登录页本身也是 antd：一张 `Card` + 表单控件。
  await expect(page.locator('.ant-card')).toBeVisible();
  await expect(page.locator('.ant-input').first()).toBeVisible();

  await signInConsole(page);

  // 外壳：`Layout.Sider` + `Menu` + `Layout.Header`。
  // 页面上有三个 `Menu`（侧栏、收起按钮、右上角用户菜单），所以取第一个而不是要求唯一。
  await expect(page.locator('.ant-layout-header')).toBeVisible();
  await expect(page.locator('.ant-menu').first()).toBeVisible();

  const sidebar = page.locator('.ant-layout-sider');
  const pages: { label: string; markers: string[] }[] = [
    { label: '网关模型', markers: ['.ant-card', '.ant-alert'] },
    { label: '发布修订', markers: ['.ant-card', '.ant-input', '.ant-btn'] },
    { label: '折算率', markers: ['.ant-card', '.ant-table', '.ant-form'] },
    { label: '路由策略', markers: ['.ant-card', '.ant-table', '.ant-select'] },
    { label: '账户与密钥', markers: ['.ant-card', '.ant-input'] },
    { label: '对账与诊断', markers: ['.ant-card', '.ant-tabs'] },
  ];

  for (const { label, markers } of pages) {
    await sidebar.getByRole('menuitem', { name: label }).click();
    // 每一页至少要能看到本页特征性的 antd 组件——空库时表格之类的可能不渲染，
    // 所以只要求"命中其中至少一种"，但 `Card` 是所有页面共有的外框，一定在。
    await expect(page.locator('.ant-card').first()).toBeVisible();
    const found = await Promise.all(
      markers.map(async (selector) => (await page.locator(selector).count()) > 0),
    );
    expect(found.some(Boolean), `${label} 页应当至少有 antd 组件之一：${markers}`).toBe(true);
  }
});

test('客户控制台的登录页与控制台都渲染 Ant Design 组件', async ({ page }) => {
  await page.goto(portalUrl);

  // 登录/注册页：一张 `Card`、一个 `Segmented`（登录/注册切换）与表单控件。
  await expect(page.locator('.ant-card').first()).toBeVisible();
  await expect(page.locator('.ant-segmented')).toBeVisible();
  await expect(page.locator('.ant-input').first()).toBeVisible();

  // 注册一个客户，看控制台那几块面板。
  await page.getByTestId('portal-mode-register').click();
  await page.getByTestId('portal-email').fill(`antd-${Date.now()}@example.com`);
  await page.getByTestId('portal-password').fill('e2e-customer-password');
  await page.getByTestId('portal-submit').click();

  for (const title of ['余额与持有', '改口令', 'API Key', '用量与账单']) {
    const card = page.locator('.ant-card').filter({ has: page.getByText(title, { exact: true }) });
    await expect(card).toBeVisible();
  }
  // 控制台里有统计卡与表格（空库时表格也在，只是显示空态）。
  await expect(page.locator('.ant-statistic').first()).toBeVisible();
  await expect(page.locator('.ant-table').first()).toBeVisible();
});
