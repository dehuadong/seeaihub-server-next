import { expect, test } from '@playwright/test';
import { adminApiUrl, consoleUrl, settings } from './settings';

/// **充值不需要先知道账户标识**（Spec V-D10、`#39`）。
///
/// 运营手上没有 UUID，他们有的是客户邮箱或自己设的标签。这条从造一个带邮箱的客户开始，全程**不出现
/// 账户标识**：用邮箱搜到它，点那一行选中，页面直接给出充值 / 账目流水 / API Key 三块。
///
/// 这条判据的要害在"不出现标识"上，所以用例**不能**为了省事把接口返回的 `account_id` 填回界面——
/// 那样验的是"拿到标识之后能用"，正是判据排除的那条路。
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

  // 点那一行选中它：页面直接给出三块，**没有「打开」、没有抽屉**。
  await body.getByText(email).click();
  await expect(page.getByTestId('accounts-credit-yuan')).toBeVisible();
  await expect(page.getByText('账目流水')).toBeVisible();
  await expect(page.getByText('API Key')).toBeVisible();
  await expect(page.locator('.ant-drawer')).toHaveCount(0);

  // 充值**只填金额**——幂等键由平台生成，不用运营抄一个。
  await page.getByTestId('accounts-credit-yuan').fill('12.34');
  await page.getByTestId('accounts-credit-submit').click();

  // 余额与流水都反映这次充值（不是只弹了个提示）。
  await expect(page.getByTestId('accounts-balance')).toContainText('12.34', { timeout: 10_000 });
  await expect(page.getByText('credit').first()).toBeVisible();

  // **全程没有出现过账户标识**：这页上没有把它填进过任何输入框。
  await expect(page.getByTestId('accounts-lookup-id')).toHaveValue('');
});
