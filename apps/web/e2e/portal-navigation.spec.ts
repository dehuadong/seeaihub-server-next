import { expect, test, type Page } from '@playwright/test';
import {
  nav,
  panel,
  pathnameOf,
  portalAt,
  registerCustomer,
  signInCustomer,
  uniqueEmail,
} from './portal';

/// 客户控制台的页面地址与导航：**只有真实浏览器才观测得到**的那一层——未登录直达回登录、登录后
/// 回跳、五条地址刷新与前进后退、凭据不进 URL、退出清屏（控制台 Spec D5、V-D13；设计 0014 §1、§4）。
///
/// 页面内容与自助流程在 `portal-self-service.spec.ts`；这里只验"地址与导航"。

const ROUTES = ['/', '/usage', '/billing', '/keys', '/settings'];

/// 某条地址上的页面标志：概览按余额锚点，其余按那一页独有的卡片标题。
async function expectRoute(page: Page, path: string): Promise<void> {
  expect(pathnameOf(page), `地址应当停在 ${path}`).toBe(path);
  const card = {
    '/usage': '调用记录',
    '/billing': '账单汇总',
    '/keys': 'API Key',
    '/settings': '账号',
  }[path];
  if (card) {
    await expect(panel(page, card)).toBeVisible();
  } else {
    await expect(page.getByTestId('portal-settled-balance')).toBeVisible();
  }
}

test('未登录直达任一页都只显示登录，且不取账户数据', async ({ page }) => {
  for (const path of ROUTES) {
    await page.goto(portalAt(path));
    // 地址保留，但页面只有登录/注册。
    expect(pathnameOf(page), `地址应当停在 ${path}`).toBe(path);
    await expect(page.getByTestId('portal-submit')).toBeVisible();
    await expect(page.locator('.ant-statistic')).toHaveCount(0);
    await expect(page.getByRole('menuitem', { name: '概览' })).toHaveCount(0);
  }

  // 全程没有向对客账户端点取过数：路由守卫在登录前不挂载任何取数组件（Spec C11、D5）。
  const accountCalls = await page.evaluate(() =>
    performance
      .getEntriesByType('resource')
      .map((entry) => entry.name)
      .filter((name) => name.includes('/v1/customer/')),
  );
  expect(accountCalls).toEqual([]);
});

test('未登录直达 /billing，登录后回到 /billing', async ({ page }) => {
  const email = uniqueEmail('portal-nav-return');
  await registerCustomer(page, email);

  // 清掉会话，模拟"未登录时从外部打开深链"。
  await page.evaluate(() => sessionStorage.clear());
  await page.goto(portalAt('/billing'));
  await expect(page.getByTestId('portal-submit')).toBeVisible();
  expect(pathnameOf(page)).toBe('/billing');

  await signInCustomer(page, email);
  expect(pathnameOf(page), '登录后应当回到原定页面').toBe('/billing');
  await expect(panel(page, '账单汇总')).toBeVisible();
});

test('五条地址直接打开与刷新都落在对应页', async ({ page }) => {
  await registerCustomer(page, uniqueEmail('portal-nav-refresh'));

  for (const path of ROUTES) {
    await page.goto(portalAt(path));
    await expectRoute(page, path);
    await page.reload();
    await expectRoute(page, path);
  }
});

test('浏览器前进后退按地址恢复页面', async ({ page }) => {
  await registerCustomer(page, uniqueEmail('portal-nav-history'));

  await nav(page, '调用记录');
  await expectRoute(page, '/usage');
  await nav(page, '账单与资金记录');
  await expectRoute(page, '/billing');

  await page.goBack();
  await expectRoute(page, '/usage');
  await page.goBack();
  await expectRoute(page, '/');

  await page.goForward();
  await expectRoute(page, '/usage');
  await page.goForward();
  await expectRoute(page, '/billing');
});

test('会话凭据与密钥明文都不进 URL', async ({ page }) => {
  await registerCustomer(page, uniqueEmail('portal-nav-url'));
  const token = await page.evaluate(() => sessionStorage.getItem('seeai.portal.session'));
  expect(token).toBeTruthy();

  for (const label of ['调用记录', '账单与资金记录', 'API Key', '账户设置', '概览']) {
    await nav(page, label);
    const url = page.url();
    expect(url, `${label} 页的地址不该带凭据`).not.toContain(token as string);
    expect(url).not.toContain('token');
    expect(url).not.toContain('session=');
    expect(url).not.toContain('api_key');
  }
});

test('退出登录清掉会话与页面数据', async ({ page }) => {
  await registerCustomer(page, uniqueEmail('portal-nav-signout'));
  await nav(page, '账户设置');
  await page.getByRole('button', { name: '退出登录' }).first().click();

  await expect(page.getByTestId('portal-submit')).toBeVisible();
  await expect(page.locator('.ant-statistic')).toHaveCount(0);
  await expect(page.getByRole('menuitem', { name: '概览' })).toHaveCount(0);
  expect(pathnameOf(page)).toBe('/');
  expect(await page.evaluate(() => sessionStorage.getItem('seeai.portal.session'))).toBeNull();
});

test('未知客户路径显示客户侧 404，不回落到概览', async ({ page }) => {
  await registerCustomer(page, uniqueEmail('portal-nav-404'));
  await page.goto(portalAt('/no-such-page'));

  await expect(page.getByTestId('portal-not-found')).toBeVisible();
  await expect(page.getByTestId('portal-settled-balance')).toHaveCount(0);
  await expect(page.getByRole('menuitem', { name: '概览' })).toBeVisible();
});
