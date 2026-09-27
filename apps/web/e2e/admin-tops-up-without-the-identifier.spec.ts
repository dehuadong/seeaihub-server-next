import { expect, test } from '@playwright/test';
import { adminApiUrl, consoleUrl, settings } from './settings';

/// **充值不需要先知道账户标识**（Spec V-D10）。
///
/// 运营手上没有 UUID，他们有的是客户邮箱或自己设的标签。这条从造一个带邮箱的客户开始，全程**不出现
/// 账户标识**：用邮箱搜到它，在列表那一行直接打开详情充值。
///
/// 这条判据的要害在"不出现标识"上，所以用例**不能**为了省事把接口返回的 `account_id` 填回界面——
/// 那样验的是"拿到标识之后能用"，正是判据排除的那条路。列表行上那个「打开」就是为此存在的入口。
///
/// 为什么只有浏览器才验得出来：这是流程事实，接口层看不见（`cases_admin_surface` 验的是那条读本身）。
test('用邮箱搜到账户并充值，全程不用账户标识', async ({ page, request }) => {
  const email = `topup-${Date.now()}@example.com`;

  // 先造一个带登录身份的账户（运营开户那条路径由「客户」页的用例覆盖）。
  const opened = await request.post(`${adminApiUrl}/api/v1/customers`, {
    headers: { authorization: `Bearer ${settings.adminToken}` },
    data: { email, password: 'a-long-enough-password' },
  });
  expect(opened.status(), await opened.text()).toBe(201);

  await page.goto(consoleUrl);
  await page.getByTestId('admin-email').fill(settings.adminEmail);
  await page.getByTestId('admin-password').fill(settings.adminPassword);
  await page.getByTestId('admin-sign-in').click();
  await page.locator('.ant-layout-sider').getByRole('menuitem', { name: '账户' }).click();

  // **只按邮箱搜**：筛选生效时列表只剩这一行，而且能看见邮箱——搜出来要能确认是谁。
  await page.getByTestId('accounts-lookup-email').fill(email);
  await page.getByTestId('accounts-search').click();
  const body = page.locator('.ant-table-tbody');
  await expect(body.getByRole('row')).toHaveCount(1);
  await expect(body).toContainText(email);

  // 在**那一行**上打开详情，不碰"按标识直达"。
  const openRow = body.getByTestId('accounts-open-row');
  await openRow.scrollIntoViewIfNeeded();
  await openRow.click();
  const drawer = page.locator('.ant-drawer');
  await expect(drawer.getByText('余额')).toBeVisible();

  // 在详情里充值：按**元**填，界面换算成微单位。
  await page.getByTestId('accounts-credit-yuan').fill('12.34');
  await page.getByTestId('accounts-credit-key').fill(`topup-${Date.now()}`);
  // 抽屉是滚动容器，底部按钮可能落在视口之外；显式滚过去再点。
  const submit = page.getByTestId('accounts-credit-submit');
  await submit.scrollIntoViewIfNeeded();
  await submit.click();

  // 余额与流水都反映这次充值（不是只弹了个提示）。
  await expect(drawer.getByText('12.34 元').first()).toBeVisible({ timeout: 10_000 });
  await expect(drawer.getByText('credit').first()).toBeVisible();

  // **全程没有出现过账户标识**：这页上没有把它填进过任何输入框。
  await expect(page.getByTestId('accounts-lookup-id')).toHaveValue('');
});
