import { expect, test } from '@playwright/test';
import { consoleUrl, portalUrl, settings } from './settings';

/// **替客户开户不需要先抄账户标识**（Spec V-D11）。
///
/// 运营在客户区用一个邮箱开户（新建账户），随后能在同一处为该客户签发重置令牌。这条全程不碰账户
/// 标识：邮箱是入口，令牌是出口。
///
/// 为什么只有浏览器才验得出来：判据说的是"界面上不需要抄标识"，那是流程事实。接口层的能力由
/// `cases_identity` 与 `cases_admin_surface` 覆盖，这里验的是运营实际操作的路径。
test('开户并签发重置令牌，客户能用它设新口令', async ({ page }) => {
  const email = `open-${Date.now()}@example.com`;
  const initial = 'a-long-enough-password';
  const reset = 'a-long-enough-password-reset';

  await page.goto(consoleUrl);
  await page.getByTestId('admin-email').fill(settings.adminEmail);
  await page.getByTestId('admin-password').fill(settings.adminPassword);
  await page.getByTestId('admin-sign-in').click();
  await page.locator('.ant-layout-sider').getByRole('menuitem', { name: '客户' }).click();

  // **开户**：邮箱 + 初始口令，不填账户标识（留空即新建一个空账户）。
  await page.getByTestId('customers-open-email').fill(email);
  await page.getByTestId('customers-open-password').fill(initial);
  await page.getByTestId('customers-open-submit').click();

  // 列表里查得到（邮箱精确匹配）——开户即选中这个客户，详情卡片的标题带邮箱。
  await page.getByTestId('customers-search-email').fill(email);
  await page.getByTestId('customers-search-submit').click();
  await expect(page.locator('.ant-table-tbody').getByRole('row')).toHaveCount(1);

  const detail = page.locator('.ant-card').filter({ hasText: `客户 ${email}` });
  await expect(detail).toBeVisible();
  await expect(detail.getByText('账户', { exact: true })).toBeVisible();

  // **签发重置令牌**：明文只显示这一次。
  await page.getByTestId('customers-issue-reset').click();
  const tokenText = page.getByTestId('customers-reset-token');
  await expect(tokenText).toBeVisible();
  const token = (await tokenText.textContent())?.trim() ?? '';
  expect(token.length).toBeGreaterThan(20);
  // 界面上写明它只活一次、平台不发邮件——运营要当场转交。
  await expect(page.getByText('只显示这一次，当场转交客户')).toBeVisible();

  // **令牌真的能用**：客户凭它在客户端设新口令，然后用新口令登录。
  await page.goto(portalUrl);
  await page.getByTestId('portal-reset-token').fill(token);
  await page.getByTestId('portal-reset-password').fill(reset);
  await page.getByTestId('portal-reset-submit').click();
  await expect(page.getByText('口令已重置').first()).toBeVisible();

  await page.getByTestId('portal-email').fill(email);
  await page.getByTestId('portal-password').fill(reset);
  await page.getByTestId('portal-submit').click();
  // 进了控制台：首屏那三个统计数在。
  await expect(page.locator('.ant-statistic').first()).toBeVisible();
});
