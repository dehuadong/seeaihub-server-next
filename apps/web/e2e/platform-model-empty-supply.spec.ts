import { expect, test } from '@playwright/test';
import { consoleUrl, settings } from './settings';

/// **一条供给都没有时，界面要指路**（#33 的零供给缺口）。
///
/// 供给来自工程师的发布素材导入（`SUPPLY_MATERIAL_DIR`）：库里没有供给时厂商下拉是空的。
/// 这一条用路由拦截把供给清单固定成空，验界面说的是"去找工程师配素材"，而不是让人对着一个
/// 没有选项的下拉和一句"先选厂商"发呆。
test('一条供给都没有时，发布抽屉说明供给从哪来', async ({ page }) => {
  await page.route('**/api/v1/offerings', (route) =>
    route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ offerings: [] }),
    }),
  );

  await page.goto(consoleUrl);
  await page.getByTestId('admin-email').fill(settings.adminEmail);
  await page.getByTestId('admin-password').fill(settings.adminPassword);
  await page.getByTestId('admin-sign-in').click();

  await page.locator('.ant-layout-sider').getByRole('menuitem', { name: '模型目录' }).click();
  await page.getByRole('button', { name: /发布新模型|上架新模型/ }).click();

  const empty = page.getByTestId('platform-supply-empty');
  await expect(empty).toBeVisible();
  await expect(empty).toContainText('工程师');
  await expect(page.getByText('先在上面选一个厂商，这里会列出它下面的供给。')).toHaveCount(0);
});
