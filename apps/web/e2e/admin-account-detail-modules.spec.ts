import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import { adminApiUrl, consoleUrl, settings } from './settings';

/// **账户详情页：四个模块用页内导航切换，一次只挂载当前那一个**（Spec M5、V-D15；设计 `0011` §4.3）。
///
/// 地址是详情本身的一部分：点行、按标识直达或建账户成功都进入 `#/accounts/{account_id}`，列表离开
/// 主内容区。模块不再开弹窗／侧边栏——切走即卸载，所以 API Key 的签发明文不会在切回来时又出现。
test.describe('账户详情页的四个模块', () => {
  async function signIn(page: Page): Promise<void> {
    await page.goto(consoleUrl);
    await page.getByTestId('admin-email').fill(settings.adminEmail);
    await page.getByTestId('admin-password').fill(settings.adminPassword);
    await page.getByTestId('admin-sign-in').click();
    await page.locator('.ant-layout-sider').getByRole('menuitem', { name: '账户' }).click();
  }

  async function openAccount(request: APIRequestContext, initialMicrousd: number): Promise<string> {
    const opened = await request.post(`${adminApiUrl}/api/v1/accounts`, {
      headers: { authorization: `Bearer ${settings.adminToken}` },
      data: { initial_credit_microusd: initialMicrousd },
    });
    expect(opened.ok(), await opened.text()).toBeTruthy();
    return (await opened.json()).account_id as string;
  }

  async function openAccountDetail(page: Page, accountId: string): Promise<void> {
    await page.getByTestId('accounts-lookup-id').fill(accountId);
    await page.getByTestId('accounts-open-by-id').click();
    await expect(page).toHaveURL(new RegExp(`#/accounts/${accountId}$`));
  }

  test('进入详情后列表让位；四个模块一次只挂载当前那一个', async ({ page, request }) => {
    const accountId = await openAccount(request, 20_000_000);

    await signIn(page);
    await openAccountDetail(page, accountId);

    // 列表离开主内容区：找账户的输入框与列表都不在详情页上。
    await expect(page.getByTestId('accounts-lookup-email')).toHaveCount(0);
    await expect(page.getByTestId('accounts-selected-id')).toHaveText(accountId);

    // 三个金额分别显示：刚建的账户已结算余额 20、持有中 0、可用额 20（账户资金 Spec §4）。
    await expect(page.getByTestId('accounts-balance')).toContainText('20');
    await expect(page.getByTestId('accounts-held')).toContainText('0');
    await expect(page.getByTestId('accounts-available')).toContainText('20');

    // 默认停在「充值」：只有它的内容挂着；另外三个模块的内容在切过去之前不存在。
    await expect(page.getByTestId('accounts-credit-yuan')).toBeVisible();
    await expect(page.getByTestId('accounts-credits-reload')).toHaveCount(0);
    await expect(page.getByTestId('accounts-usage-reload')).toHaveCount(0);
    await expect(page.getByTestId('accounts-issue-key')).toHaveCount(0);

    // 充值记录：充值那块的输入框随之卸载。
    await page.getByTestId('accounts-module-credits').click();
    await expect(page.getByTestId('accounts-credits-reload')).toBeVisible();
    await expect(page.getByTestId('accounts-credit-yuan')).toHaveCount(0);

    // 扣费记录（调用明细）：又是另一个模块。
    await page.getByTestId('accounts-module-usage').click();
    await expect(page.getByTestId('accounts-usage-reload')).toBeVisible();
    await expect(page.getByTestId('accounts-credits-reload')).toHaveCount(0);

    // API Key：可签发与吊销。
    await page.getByTestId('accounts-module-keys').click();
    await expect(page.getByTestId('accounts-issue-key')).toBeVisible();
    await expect(page.getByTestId('accounts-revoke-key')).toBeVisible();
    await expect(page.getByTestId('accounts-usage-reload')).toHaveCount(0);
  });

  test('API Key 明文只在签发那一次出现，吊销要确认', async ({ page, request }) => {
    const accountId = await openAccount(request, 0);

    await signIn(page);
    await openAccountDetail(page, accountId);
    await page.getByTestId('accounts-module-keys').click();
    await page.getByTestId('accounts-issue-key').click();
    await expect(page.getByTestId('accounts-issued-key')).toBeVisible();

    // 切到别的模块：那一块整个卸载，明文随之消失。
    await page.getByTestId('accounts-module-credits').click();
    await expect(page.getByTestId('accounts-issued-key')).toHaveCount(0);

    // **切回 API Key 也不再出现**：签发那一次过去就该丢，模块重挂不该把明文带回来。
    await page.getByTestId('accounts-module-keys').click();
    await expect(page.getByTestId('accounts-issued-key')).toHaveCount(0);

    // 吊销要二次确认：点"吊销"先出确认按钮，不直接发请求。
    await page.getByTestId('accounts-revoke-key').fill('00000000-0000-4000-8000-000000000000');
    await page.getByTestId('accounts-revoke-open').click();
    await expect(page.getByTestId('accounts-revoke-confirm')).toBeVisible();
  });

  test('换一个账户时模块与一次性明文都不跟过来', async ({ page, request }) => {
    const first = await openAccount(request, 10_000_000);
    const second = await openAccount(request, 30_000_000);

    await signIn(page);
    await openAccountDetail(page, first);
    await expect(page.getByTestId('accounts-balance')).toContainText('10');

    await page.getByTestId('accounts-module-keys').click();
    await page.getByTestId('accounts-issue-key').click();
    await expect(page.getByTestId('accounts-issued-key')).toBeVisible();

    // 回列表再点另一个账户：地址换成第二个账户，读数跟着换，密钥明文不出现。
    await page.getByTestId('accounts-back-to-list').click();
    await expect(page).toHaveURL(/#\/accounts$/);
    await page.locator('.ant-table-tbody').getByText(second).click();
    await expect(page).toHaveURL(new RegExp(`#/accounts/${second}$`));
    await expect(page.getByTestId('accounts-selected-id')).toHaveText(second);
    await expect(page.getByTestId('accounts-balance')).toContainText('30');
    await expect(page.getByTestId('accounts-issued-key')).toHaveCount(0);
    // 新账户的详情默认停在「充值」，密钥模块没挂载。
    await expect(page.getByTestId('accounts-credit-yuan')).toBeVisible();
  });
});
