import { expect, test } from '@playwright/test';
import { consoleUrl, portalUrl, settings } from './settings';

/// 管理端会话：**只有浏览器才观测得到**的那一层——路由守卫在登录前不取数、登录后进后台、
/// 刷新之后会话还在（会话存在 `sessionStorage` 而不是 cookie 里）。
///
/// 接口契约、状态码与鉴权规则由 `apps/api/tests/http_contract` 的用例管，不在这里重验。
const ADMIN_EMAIL = settings.adminEmail;
const ADMIN_PASSWORD = settings.adminPassword;

const CONSOLE = consoleUrl;
const PORTAL = portalUrl;

const NAV_LABELS = ['网关模型', '发布修订', '折算率', '路由策略', '账户与密钥', '对账与诊断'];

/// 侧栏导航项。Ant Design 的 `Menu` 把每项渲染成 `role=menuitem`，不是 button——换 UI 库时
/// 选择器要跟着实现走，但**断言的性质不变**（那六项在 / 不在）。
///
/// 限定在 `.ant-layout-sider` 里：页头也有一份"当前在哪一页"的指示，按角色找会同时命中两处。
function navItem(page: import('@playwright/test').Page, label: string) {
  return page.locator('.ant-layout-sider').getByRole('menuitem', { name: label });
}

/// 未登录时**一个管理取数请求都不发**。这条是路由守卫的实质：把这些请求记下来，过滤出管理面的。
async function consoleApiCalls(page: import('@playwright/test').Page): Promise<string[]> {
  return page.evaluate(() =>
    performance
      .getEntriesByType('resource')
      .map((entry) => entry.name)
      .filter((name) => name.includes('/api/v1/')),
  );
}

test('未登录时只渲染登录页，且不向管理 API 发取数请求', async ({ page }) => {
  await page.goto(CONSOLE);

  await expect(page.getByTestId('admin-email')).toBeVisible();
  await expect(page.getByTestId('admin-sign-in')).toBeVisible();
  // 后台的导航项一个都不该在。
  for (const label of NAV_LABELS) {
    await expect(navItem(page, label)).toHaveCount(0);
  }
  // 守卫生效的硬证据：没有任何管理面请求发出去过。
  expect(await consoleApiCalls(page)).toEqual([]);
});

test('登录后进后台，刷新之后仍是登录态', async ({ page }) => {
  await page.goto(CONSOLE);

  await page.getByTestId('admin-email').fill(ADMIN_EMAIL);
  await page.getByTestId('admin-password').fill(ADMIN_PASSWORD);
  await page.getByTestId('admin-sign-in').click();

  // 六个导航项就是"进到后台了"。
  for (const label of NAV_LABELS) {
    await expect(navItem(page, label)).toBeVisible();
  }
  // 会话令牌在 sessionStorage 里（不是 cookie）：这是刷新后仍登录的原因。
  const stored = await page.evaluate(() => sessionStorage.getItem('seeai.console.session'));
  expect(stored).toBeTruthy();
  expect(await page.evaluate(() => document.cookie)).not.toContain('seeai.console.session');

  // **URL 里不出现会话凭据**（V-D3）：路由用的是 hash，别哪天顺手把令牌拼进去。
  const url = page.url();
  expect(url).not.toContain(stored as string);
  expect(url).not.toContain('token');
  expect(url).not.toContain('session=');

  await page.reload();
  for (const label of NAV_LABELS) {
    await expect(navItem(page, label)).toBeVisible();
  }
  // 刷新之后 URL 仍然干净。
  expect(page.url()).not.toContain(stored as string);
});

test('口令不对时留在登录页并说明原因', async ({ page }) => {
  await page.goto(CONSOLE);

  await page.getByTestId('admin-email').fill(ADMIN_EMAIL);
  await page.getByTestId('admin-password').fill('definitely-not-the-password');
  await page.getByTestId('admin-sign-in').click();

  // 管理面所有凭据失败都是同一个答复，所以文案不区分"邮箱不存在"与"口令不对"。
  // 提示现在是 Ant Design 的 `Alert`；它的 role 有嵌套（同名元素两处），登录页上另有一条
  // "API 探活"的提示，所以先按文案筛出错误那一条，再取第一个。
  await expect(page.getByRole('alert').filter({ hasText: '邮箱或口令' }).first()).toBeVisible();
  await expect(navItem(page, '网关模型')).toHaveCount(0);
});

test('客户入口在任何主机上都能按文件名直达，且拿到的是客户那一份', async ({ page }) => {
  // `/portal.html` 是静态文件，按名字直达，不受主机名分发影响——这条保证开发出口开着时两个入口
  // 各自仍有一条明确地址（实际踩到过：只认主机名时 `/portal.html` 被顶成了管理端）。
  await page.goto(PORTAL);
  await expect(page).toHaveTitle(/seeai 控制台/);
  await expect(page.getByTestId('portal-submit')).toBeVisible();
  // 客户入口里不该有管理端的东西。
  await expect(page.getByTestId('admin-sign-in')).toHaveCount(0);
});
