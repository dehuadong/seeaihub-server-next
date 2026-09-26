import { expect, test, type Page } from '@playwright/test';
import { adminApiUrl, consoleUrl, settings } from './settings';

/// 管理页面读的是**库里的真实数据**，不是写死的样例（Spec V-D2 的"读出该夹具的数据、可重复"）。
///
/// 做法：先用管理 API 造事实（一个币种的折算率、一个带初始余额的账户），再让页面去读，断言页面上出现
/// 的正是**刚造的那一组值**。整套跑在每次重建的空库上，所以"读到旧库的状态"不可能蒙对。
///
/// 为什么不用端到端夹具（`apps/api/tests/http_contract`）那一套真跑一笔生成：那要起 Worker、发布修订、
/// 造 Provider 桩，而它已经由 `cases_billing` 与 `cases_identity` 在接口层覆盖了。这里要证的只是
/// **页面把库里的数读出来了**，用管理 API 造事实足够，也快得多。
test.describe('管理页面读的是库里的数据', () => {
  async function signIn(page: Page): Promise<void> {
    await page.goto(consoleUrl);
    await page.getByTestId('admin-email').fill(settings.adminEmail);
    await page.getByTestId('admin-password').fill(settings.adminPassword);
    await page.getByTestId('admin-sign-in').click();
    // 侧栏出现即已进后台；限定在侧栏里，页头另有一份"当前在哪一页"的指示。
    await expect(sidebar(page, '网关模型')).toBeVisible();
  }

  /// 切到某一页。导航项不是 button，是按角色的 menuitem；限定在侧栏内避免与页头重复。
  async function goTo(page: Page, label: string): Promise<void> {
    await sidebar(page, label).click();
  }

  function sidebar(page: Page, label: string) {
    return page.locator('.ant-layout-sider').getByRole('menuitem', { name: label });
  }

  test('折算率页显示刚录入的那一行', async ({ page, request }) => {
    // `*.localhost` 只有浏览器认，Node 的解析器不认——所以 API 调用直连回环。
    const currency = `ZZ${Math.floor(Math.random() * 90 + 10)}`;
    const written = await request.put(`${adminApiUrl}/api/v1/fx-rates`, {
      headers: { authorization: `Bearer ${settings.adminToken}` },
      data: { currency, rate_micros: 7_150_000 },
    });
    expect(written.status()).toBe(204);

    await signIn(page);
    await goTo(page, '折算率');

    // 页面上出现的必须是**这一行**：币种对得上，数值也对得上（7.15）。
    const row = page.getByRole('row', { name: new RegExp(currency) });
    await expect(row).toBeVisible();
    await expect(row).toContainText('7.15');
  });

  test('账户与密钥页显示刚建的账户与刚充的余额', async ({ page, request }) => {
    const opened = await request.post(`${adminApiUrl}/api/v1/accounts`, {
      headers: { authorization: `Bearer ${settings.adminToken}` },
      data: { initial_credit_microusd: 12_340_000 },
    });
    expect(opened.ok()).toBeTruthy();
    const accountId = (await opened.json()).account_id as string;
    expect(accountId).toBeTruthy();

    await signIn(page);
    await goTo(page, '账户与密钥');

    // 在这一页上按刚建的账户标识去读余额：读出来的必须正是那个数（12.34 元）。
    await page.getByTestId('accounts-lookup-id').fill(accountId);
    await page.getByRole('button', { name: '读余额与流水' }).click();

    // 余额与微单位分两行显示，所以按**那一块描述列表**断言，不用裸文本（会命中两处）。
    // antd 会把文本切成多个节点，`getByText('12.34 元')` 匹配不到——按容器 + 归一化文本断言。
    const balanceBlock = page.locator('.ant-descriptions').first();
    await expect(balanceBlock).toContainText('12.34');
    await expect(balanceBlock).toContainText('12340000');
    await expect(balanceBlock).toContainText('微单位');
  });
});
