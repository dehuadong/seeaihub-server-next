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
    await expect(page.getByTestId('portal-balance')).toBeVisible();
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

    // 这一页没有向对客账户端点取过数：路由守卫在登录前不挂载任何取数组件（Spec C11、D5）。
    // 断言必须落在**每次 `goto` 之后**——`resource timing` 只属于当前文档，循环外查只看得到
    // 最后一次导航那一页，前面几页发了请求也抓不到。
    const accountCalls = await page.evaluate(() =>
      performance
        .getEntriesByType('resource')
        .map((entry) => entry.name)
        .filter((name) => name.includes('/v1/customer/')),
    );
    expect(accountCalls, `${path} 不该向对客账户端点取数`).toEqual([]);
  }
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
  await expect(page.getByTestId('portal-balance')).toHaveCount(0);
  await expect(page.getByRole('menuitem', { name: '概览' })).toBeVisible();
});

test('账单读失败时不把缺失值画成零', async ({ page }) => {
  await registerCustomer(page, uniqueEmail('portal-nav-billing-failure'));

  // 让账单汇总那条读失败：缺失值要显示 `—` 并给出错误与重取，**不能**画成 0——把"没读到"
  // 画成"零"是错的读数（V-D14、设计 0014 §3）。
  await page.route('**/v1/customer/billing*', (route) =>
    route.fulfill({ status: 500, contentType: 'application/json', body: '{"error":"boom"}' }),
  );
  await page.goto(portalAt('/billing'));

  const summary = panel(page, '账单汇总');
  await expect(summary.getByRole('alert')).toBeVisible();
  await expect(summary.getByText('—').first()).toBeVisible();
});

test('对客请求回 401 时清掉页面数据并回到登录', async ({ page }) => {
  await registerCustomer(page, uniqueEmail('portal-nav-401'));

  // 先制造可见的密钥数据，清屏才有东西可"消失"。
  await nav(page, 'API Key');
  await page.getByTestId('portal-key-label').fill('401 前可见');
  await page.getByTestId('portal-key-create').click();
  // 明文弹窗挡着页面，先按「取消」关掉它，再继续（关闭即清，Spec C5）。
  await expect(page.getByRole('dialog')).toBeVisible();
  await page.getByTestId('portal-key-plaintext-close').click();
  await expect(page.getByTestId('portal-key-plaintext')).toHaveCount(0);
  await expect(page.getByText('401 前可见')).toBeVisible();

  // 服务端提前吊销会话：之后的对客取数一律回 401。
  await page.route('**/v1/customer/**', (route) =>
    route.fulfill({
      status: 401,
      contentType: 'application/json',
      body: '{"error":{"code":"unauthorized","message":"unauthorized"}}',
    }),
  );

  // 同一页面上触发一次重取——401 由客户端集中清屏，而不是把旧数据留在可见页面（设计 0014 §4）。
  await page.getByTestId('portal-keys-reload').click();

  await expect(page.getByTestId('portal-submit')).toBeVisible();
  await expect(page.getByText('401 前可见')).toHaveCount(0);
  await expect(page.getByTestId('portal-key-create')).toHaveCount(0);
  await expect(page.getByRole('menuitem', { name: '概览' })).toHaveCount(0);
  expect(await page.evaluate(() => sessionStorage.getItem('seeai.portal.session'))).toBeNull();
});

test('会话 expires_at 已过期时刷新回到登录并清屏', async ({ page }) => {
  await registerCustomer(page, uniqueEmail('portal-nav-expired'));

  await nav(page, 'API Key');
  await page.getByTestId('portal-key-label').fill('过期前可见');
  await page.getByTestId('portal-key-create').click();
  await expect(page.getByText('过期前可见')).toBeVisible();

  // 只把本地会话的到期时刻改成过去：服务端那条会话可能仍然有效，页面也必须按到期时刻自行清屏。
  await page.evaluate(() => {
    sessionStorage.setItem(
      'seeai.portal.session.expires',
      new Date(Date.now() - 60_000).toISOString(),
    );
  });
  await page.reload();

  await expect(page.getByTestId('portal-submit')).toBeVisible();
  await expect(page.getByText('过期前可见')).toHaveCount(0);
  await expect(page.getByTestId('portal-key-create')).toHaveCount(0);
  await expect(page.getByRole('menuitem', { name: '概览' })).toHaveCount(0);
  expect(await page.evaluate(() => sessionStorage.getItem('seeai.portal.session'))).toBeNull();
});
