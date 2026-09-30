import { expect, test } from '@playwright/test';
import { consoleUrl, settings } from './settings';

/// **建账户要先填表单、确认才建**（`#39` 的 P3）。
///
/// 原来是点一下「建空账户」立刻建出余额 0、无邮箱无标签的账户——之后只能靠 UUID 找到。现在：
/// 点开是表单（标签 + 初始充值），取消不建、确认才建；填了标签的能按标签搜到。
test('建账户要先填表单：取消不建、确认才建且标签能搜到', async ({ page }) => {
  const tag = `e2e-tag-${Date.now()}`;

  await page.goto(consoleUrl);
  await page.getByTestId('admin-email').fill(settings.adminEmail);
  await page.getByTestId('admin-password').fill(settings.adminPassword);
  await page.getByTestId('admin-sign-in').click();
  await page.locator('.ant-layout-sider').getByRole('menuitem', { name: '账户' }).click();

  // 取消不建：表单出现，取消之后它消失，什么也没建。
  await page.getByTestId('accounts-create-open').click();
  await expect(page.getByTestId('accounts-create-tag')).toBeVisible();
  await expect(page.getByTestId('accounts-create-yuan')).toBeVisible();
  await page.getByTestId('accounts-create-cancel').click();
  await expect(page.getByTestId('accounts-create-tag')).toHaveCount(0);

  // 确认才建：填标签 + 初始充值 5 元。
  await page.getByTestId('accounts-create-open').click();
  await page.getByTestId('accounts-create-tag').fill(tag);
  await page.getByTestId('accounts-create-yuan').fill('5');
  await page.getByTestId('accounts-create-submit').click();

  // 建出来的账户按**标签**能搜到。用"账户列表"那张卡限定，免得与选中区那张卡混淆。
  await page.getByTestId('accounts-lookup-tag').fill(tag);
  await page.getByTestId('accounts-search').click();
  const list = page.locator('.ant-card', { hasText: '账户列表' });
  await expect(list.locator('.ant-table-tbody').getByText(tag)).toBeVisible();
  // 建完直接选中它：余额显示在账户那一行（刚填的初始充值 5 元）。
  await expect(page.getByTestId('accounts-balance')).toContainText('5');
});
