import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import { adminApiUrl, consoleUrl, settings } from './settings';

/// **账户页选中后只给四个按钮，模块内容开在弹层里**（`#41` 的 P1/P2/P3）。
///
/// 一页只有"找账户 + 选中那一行"：点列表行即选中、行内没有功能按钮；充值开**弹窗**，
/// 充值记录 / 扣费记录（调用明细）/ API Key 各开一个**侧边栏**。模块内容在打开前不存在；
/// 切换选中账户会关掉当前弹层。
test.describe('账户页的四个模块开在弹层里', () => {
  async function signIn(page: Page): Promise<void> {
    await page.goto(consoleUrl);
    await page.getByTestId('admin-email').fill(settings.adminEmail);
    await page.getByTestId('admin-password').fill(settings.adminPassword);
    await page.getByTestId('admin-sign-in').click();
    await page.locator('.ant-layout-sider').getByRole('menuitem', { name: '账户' }).click();
  }

  /// 关掉当前侧边栏。侧边栏里的关闭按钮是 antd 的 `Close`；页面里的输入框清空按钮也叫
  /// `close-circle`，所以限定在 `.ant-drawer` 内、并要求名字精确匹配。
  async function closeDrawer(page: Page): Promise<void> {
    await page.locator('.ant-drawer').getByRole('button', { name: 'Close', exact: true }).click();
  }

  async function openAccount(request: APIRequestContext, initialMicrousd: number): Promise<string> {
    const opened = await request.post(`${adminApiUrl}/api/v1/accounts`, {
      headers: { authorization: `Bearer ${settings.adminToken}` },
      data: { initial_credit_microusd: initialMicrousd },
    });
    expect(opened.ok(), await opened.text()).toBeTruthy();
    return (await opened.json()).account_id as string;
  }

  test('选中后只有四个按钮，各模块在弹窗或侧边栏里打开', async ({ page, request }) => {
    const accountId = await openAccount(request, 20_000_000);

    await signIn(page);
    await page.getByTestId('accounts-lookup-id').fill(accountId);
    await page.getByTestId('accounts-open-by-id').click();

    // 选中区只给四个按钮；充值输入框与任何侧边栏在打开前都不存在。
    for (const id of [
      'accounts-credit-open',
      'accounts-credits-open',
      'accounts-usage-open',
      'accounts-keys-open',
    ]) {
      await expect(page.getByTestId(id)).toBeVisible();
    }
    await expect(page.getByTestId('accounts-credit-yuan')).toHaveCount(0);
    await expect(page.locator('.ant-drawer')).toHaveCount(0);

    // 三个金额分别显示：刚建的账户已结算余额 20、持有中 0、可用额 20（账户资金 Spec §4）。
    await expect(page.getByTestId('accounts-balance')).toContainText('20');
    await expect(page.getByTestId('accounts-held')).toContainText('0');
    await expect(page.getByTestId('accounts-available')).toContainText('20');

    // 充值：弹窗，输入框这时才出现；取消不充。
    await page.getByTestId('accounts-credit-open').click();
    await expect(page.getByTestId('accounts-credit-yuan')).toBeVisible();
    await page.getByTestId('accounts-credit-cancel').click();
    await expect(page.getByTestId('accounts-credit-yuan')).toHaveCount(0);

    // 充值记录：侧边栏。
    await page.getByTestId('accounts-credits-open').click();
    await expect(page.getByTestId('accounts-credits-reload')).toBeVisible();
    await closeDrawer(page);
    await expect(page.locator('.ant-drawer')).toHaveCount(0);

    // 扣费记录（调用明细）：另一个侧边栏。
    await page.getByTestId('accounts-usage-open').click();
    await expect(page.getByTestId('accounts-usage-reload')).toBeVisible();
    await closeDrawer(page);
    await expect(page.locator('.ant-drawer')).toHaveCount(0);

    // API Key：第三个侧边栏，可签发与吊销。
    await page.getByTestId('accounts-keys-open').click();
    await expect(page.getByTestId('accounts-issue-key')).toBeVisible();
    await expect(page.getByTestId('accounts-revoke-key')).toBeVisible();
  });

  test('API Key 明文只在签发时出现一次，吊销要确认', async ({ page, request }) => {
    const accountId = await openAccount(request, 0);

    await signIn(page);
    await page.getByTestId('accounts-lookup-id').fill(accountId);
    await page.getByTestId('accounts-open-by-id').click();

    await page.getByTestId('accounts-keys-open').click();
    await page.getByTestId('accounts-issue-key').click();
    await expect(page.getByTestId('accounts-issued-key')).toBeVisible();

    // 关掉侧边栏再打开：明文不再出现（界面承诺"关掉这个侧边栏就没了"）。
    await closeDrawer(page);
    await expect(page.getByTestId('accounts-issued-key')).toHaveCount(0);
    await page.getByTestId('accounts-keys-open').click();
    await expect(page.getByTestId('accounts-issued-key')).toHaveCount(0);

    // **切到别的模块再切回来**也不该再现：签发那一次过去就该丢。这条路径与"关掉再打开"不同
    // （侧边栏不挡页面，模块按钮一直可点），漏了它明文会二次显示。
    await page.getByTestId('accounts-credits-open').click();
    await expect(page.getByTestId('accounts-issued-key')).toHaveCount(0);
    await page.getByTestId('accounts-keys-open').click();
    await expect(page.getByTestId('accounts-issued-key')).toHaveCount(0);

    // 吊销要二次确认：点"吊销"先出确认按钮，不直接发请求。
    await page.getByTestId('accounts-revoke-key').fill('00000000-0000-4000-8000-000000000000');
    await page.getByTestId('accounts-revoke-open').click();
    await expect(page.getByTestId('accounts-revoke-confirm')).toBeVisible();
  });

  test('切换选中账户会关掉当前弹层', async ({ page, request }) => {
    const first = await openAccount(request, 10_000_000);
    const second = await openAccount(request, 30_000_000);

    await signIn(page);
    await page.getByTestId('accounts-lookup-id').fill(first);
    await page.getByTestId('accounts-open-by-id').click();
    await expect(page.getByTestId('accounts-balance')).toContainText('10');

    await page.getByTestId('accounts-credits-open').click();
    await expect(page.getByTestId('accounts-credits-reload')).toBeVisible();

    // 换一个账户（点列表另一行）：当前侧边栏关掉，读数换成新账户的。
    await page.locator('.ant-table-tbody').getByText(second).click();
    await expect(page.getByTestId('accounts-credits-reload')).toHaveCount(0);
    await expect(page.locator('.ant-drawer')).toHaveCount(0);
    await expect(page.getByTestId('accounts-selected-id')).toHaveText(second);
    await expect(page.getByTestId('accounts-balance')).toContainText('30');
  });
});
