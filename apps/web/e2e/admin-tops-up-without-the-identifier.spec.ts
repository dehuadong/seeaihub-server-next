import { expect, test } from '@playwright/test';
import { adminApiUrl, consoleUrl, settings } from './settings';

/// **充值不需要先知道账户标识**（Spec V-D10）。
///
/// 运营手上没有 UUID，他们有的是客户邮箱或自己设的标签。这条从造一个带邮箱的客户开始，全程**不把
/// 账户标识抄来抄去**：用邮箱搜到它、在列表里确认是它，再打开详情充值。
///
/// 为什么只有浏览器才验得出来：这条判据说的是"界面上不需要抄标识"，那是流程事实，接口层看不见
/// （`cases_admin_surface` 验的是那条读本身能用，`admin-reads-fixture-data` 验的是页面读出库里的数）。
test('用邮箱搜到账户并充值，不需要先知道账户标识', async ({ page, request }) => {
  const email = `topup-${Date.now()}@example.com`;

  // 先造一个带登录身份的账户（运营开户那条路径由「客户」页的用例覆盖）。
  const opened = await request.post(`${adminApiUrl}/api/v1/customers`, {
    headers: { authorization: `Bearer ${settings.adminToken}` },
    data: { email, password: 'a-long-enough-password' },
  });
  expect(opened.status(), await opened.text()).toBe(201);
  const accountId = ((await opened.json()) as { account_id: string }).account_id;

  await page.goto(consoleUrl);
  await page.getByTestId('admin-email').fill(settings.adminEmail);
  await page.getByTestId('admin-password').fill(settings.adminPassword);
  await page.getByTestId('admin-sign-in').click();
  await page.locator('.ant-layout-sider').getByRole('menuitem', { name: '账户' }).click();

  // **按邮箱搜**：筛选生效时列表只剩这一行，而且能看见邮箱——搜出来要能确认是谁。
  await page.getByTestId('accounts-lookup-email').fill(email);
  await page.getByTestId('accounts-search').click();
  const body = page.locator('.ant-table-tbody');
  await expect(body.getByRole('row')).toHaveCount(1);
  await expect(body).toContainText(email);

  // 打开详情（用"按标识直达"那一格）。
  await page.getByTestId('accounts-lookup-id').fill(accountId);
  await page.getByTestId('accounts-open-by-id').click();

  // 详情里充值：按**元**填，界面换算成微单位。
  await page.getByTestId('accounts-credit-yuan').fill('12.34');
  await page.getByTestId('accounts-credit-key').fill(`topup-${Date.now()}`);
  // 抽屉是滚动容器，按钮可能在视口之外；显式滚过去再点。
  const submit = page.getByTestId('accounts-credit-submit');
  await submit.scrollIntoViewIfNeeded();
  await submit.click();

  // 余额与流水都反映这次充值（不是只弹了个提示）。
  await expect(page.locator('.ant-drawer').getByText('12.34 元').first()).toBeVisible({
    timeout: 10_000,
  });
  await expect(page.locator('.ant-drawer').getByText('credit').first()).toBeVisible();
});
