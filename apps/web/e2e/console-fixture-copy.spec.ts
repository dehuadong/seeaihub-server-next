import { expect, test } from '@playwright/test';
import { consoleUrl, settings } from './settings';

/// **介绍发布素材的地方必须说明它是夹具、不是生产价**（Spec `0001` §6）。
///
/// 那条约束原由"工程师贴整份定义"那条路的用例守着；那条入口已删（渠道与供给由工程师随素材配置），
/// 现在仓库里唯一介绍 `config/bootstrap/*.json` 的界面是模型目录的空态。用路由拦截把模型清单固定成
/// 空来验那句话还在。
test('模型目录空态介绍发布素材时说明它是夹具、不是生产价', async ({ page }) => {
  await page.route('**/api/v1/gateway-models', (route) =>
    route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ gateway_models: [] }),
    }),
  );
  await page.goto(consoleUrl);
  await page.getByTestId('admin-email').fill(settings.adminEmail);
  await page.getByTestId('admin-password').fill(settings.adminPassword);
  await page.getByTestId('admin-sign-in').click();
  await page.locator('.ant-layout-sider').getByRole('menuitem', { name: '模型目录' }).click();
  await expect(page.getByText(/发布夹具/)).toBeVisible();
  await expect(page.getByText(/不是生产价/)).toBeVisible();
});
