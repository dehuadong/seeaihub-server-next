import { expect, test } from '@playwright/test';
import { adminApiUrl, consoleUrl, settings } from './settings';

/// **充值不需要先知道账户标识**（Spec V-D10、`#39`）。
///
/// 运营手上没有 UUID，他们有的是客户邮箱或自己设的标签。这条从造一个带邮箱的客户开始，全程**不出现
/// 账户标识**：用邮箱搜到它、点那一行进入它的详情页，充值就填个金额。详情页的地址里会有账户标识，
/// 但那是平台拼出来的，不是运营抄进去的——这正是这条判据与"拿到标识之后能用"的区别。
///
/// 充值的**幂等键由平台生成**：运营只填金额。
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

  // 点那一行：进入这个账户**自己的地址**，列表让位；页面上没有需要运营填标识的地方。
  await body.getByText(email).click();
  await expect(page).toHaveURL(/#\/accounts\/[0-9a-f-]{36}$/);
  await expect(page.getByTestId('accounts-lookup-id')).toHaveCount(0);

  // 充值**只填金额**——幂等键由平台生成，不用运营抄一个。
  await page.getByTestId('accounts-credit-yuan').fill('12.34');
  await page.getByTestId('accounts-credit-submit').click();

  // 余额与**充值记录**都反映这次充值（不是只弹了个提示）。
  await expect(page.getByTestId('accounts-balance')).toContainText('12.34', { timeout: 10_000 });
  await page.getByTestId('accounts-module-credits').click();
  await expect(page.getByTestId('accounts-credits-reload')).toBeVisible();
  await expect(page.locator('.ant-table-tbody')).toContainText('12.34');

  // 回列表：这次查找条件**还在**（还是那一行），而标识输入框仍然是空的——全程没人填过它；
  // 地址里也没有邮箱。
  await page.getByTestId('accounts-back-to-list').click();
  await expect(page.getByTestId('accounts-lookup-id')).toHaveValue('');
  await expect(page.getByTestId('accounts-lookup-email')).toHaveValue(email);
  await expect(body.getByRole('row')).toHaveCount(1);
  expect(page.url()).not.toContain(encodeURIComponent(email));
  expect(page.url()).not.toContain(email);
});
