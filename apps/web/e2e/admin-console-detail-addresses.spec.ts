import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import { adminApiUrl, consoleUrl, settings } from './settings';

/// **账户与客户的详情是独立地址**（Spec D6、V-D15；设计 `0011` §4.3–4.4.1）。
///
/// 判据只有浏览器观测得到：地址指向对象、刷新与前进后退回到同一页、返回列表保留查找条件、未登录时
/// 一个管理取数请求都不发、对象不存在时不残留上一个对象。接口层那两条按标识的读由
/// `cases_admin_surface` 覆盖，这里验的是"运营实际看到的地址与页面"。

async function signIn(page: Page): Promise<void> {
  await page.goto(consoleUrl);
  await page.getByTestId('admin-email').fill(settings.adminEmail);
  await page.getByTestId('admin-password').fill(settings.adminPassword);
  await page.getByTestId('admin-sign-in').click();
}

async function openAccount(request: APIRequestContext, tag: string): Promise<string> {
  const created = await request.post(`${adminApiUrl}/api/v1/accounts`, {
    headers: { authorization: `Bearer ${settings.adminToken}` },
    data: { initial_credit_microusd: 7_000_000 },
  });
  expect(created.ok(), await created.text()).toBeTruthy();
  const accountId = (await created.json()).account_id as string;
  const tagged = await request.put(`${adminApiUrl}/api/v1/accounts/${accountId}/tag`, {
    headers: { authorization: `Bearer ${settings.adminToken}` },
    data: { tag },
  });
  expect(tagged.ok(), await tagged.text()).toBeTruthy();
  return accountId;
}

test('账户详情直接打开与刷新都在同一个地址，返回列表保留查找条件，前进后退恢复页面', async ({
  page,
  request,
}) => {
  const tag = `addr-${Date.now()}`;
  const accountId = await openAccount(request, tag);

  await signIn(page);
  await page.locator('.ant-layout-sider').getByRole('menuitem', { name: '账户' }).click();

  // 按标签筛出来再点行：这是"返回列表要保留条件"里那个条件。
  await page.getByTestId('accounts-lookup-tag').fill(tag);
  await page.getByTestId('accounts-search').click();
  await expect(page.locator('.ant-table-tbody').getByRole('row')).toHaveCount(1);
  await page.locator('.ant-table-tbody').getByText(tag).click();

  // 地址指向这个账户；侧栏「账户」在详情里仍然是当前工作区（列表与详情共用同一个侧栏项）。
  await expect(page).toHaveURL(new RegExp(`#/accounts/${accountId}$`));
  // 侧栏把「账户」标成当前工作区（`aria-current` 由外壳写在导航项的标签上）。
  await expect(
    page.locator('.ant-layout-sider [aria-current="page"]'),
  ).toHaveText('账户');
  await expect(page.getByTestId('accounts-selected-id')).toHaveText(accountId);
  await expect(page.getByTestId('accounts-balance')).toContainText('7');
  await expect(page.getByTestId('accounts-lookup-tag')).toHaveCount(0);

  // **刷新**：详情自己按地址里的标识取数，不依赖列表那次查询。
  await page.reload();
  await expect(page).toHaveURL(new RegExp(`#/accounts/${accountId}$`));
  await expect(page.getByTestId('accounts-selected-id')).toHaveText(accountId);
  await expect(page.getByTestId('accounts-balance')).toContainText('7');

  // **浏览器后退**回列表：筛选条件还在，列表仍是筛过的那一行。
  await page.goBack();
  await expect(page).toHaveURL(/#\/accounts$/);
  await expect(page.getByTestId('accounts-lookup-tag')).toHaveValue(tag);
  await expect(page.locator('.ant-table-tbody').getByRole('row')).toHaveCount(1);

  // **前进**按浏览器历史回到详情。
  await page.goForward();
  await expect(page).toHaveURL(new RegExp(`#/accounts/${accountId}$`));
  await expect(page.getByTestId('accounts-selected-id')).toHaveText(accountId);

  // **页面上的「返回列表」**回同一个列表，条件同样是恢复的。
  await page.getByTestId('accounts-back-to-list').click();
  await expect(page).toHaveURL(/#\/accounts$/);
  await expect(page.getByTestId('accounts-lookup-tag')).toHaveValue(tag);
  await expect(page.locator('.ant-table-tbody').getByRole('row')).toHaveCount(1);
});

test('客户详情按地址直达，关联账户能进账户详情', async ({ page, request }) => {
  const email = `addr-${Date.now()}@example.com`;
  const opened = await request.post(`${adminApiUrl}/api/v1/customers`, {
    headers: { authorization: `Bearer ${settings.adminToken}` },
    data: { email, password: 'a-long-enough-password' },
  });
  expect(opened.status(), await opened.text()).toBe(201);
  const customer = await opened.json();
  const customerId = customer.customer_id as string;
  const accountId = customer.account_id as string;

  await signIn(page);
  // **直接打开**详情地址：不经列表、不经开户响应。
  await page.goto(`${consoleUrl}#/customers/${customerId}`);
  await expect(page.getByTestId('customers-detail-email')).toHaveText(email);
  await expect(page.getByTestId('customers-search-email')).toHaveCount(0);
  // 侧栏把「客户」标成当前工作区（`aria-current` 由外壳写在导航项的标签上）。
  await expect(
    page.locator('.ant-layout-sider [aria-current="page"]'),
  ).toHaveText('客户');

  // 关联账户是通往账户详情的入口：点一下就换到那个账户的地址。
  await page.getByTestId('customers-open-account').click();
  await expect(page).toHaveURL(new RegExp(`#/accounts/${accountId}$`));
  await expect(page.getByTestId('accounts-selected-id')).toHaveText(accountId);
  // 侧栏把「账户」标成当前工作区（`aria-current` 由外壳写在导航项的标签上）。
  await expect(
    page.locator('.ant-layout-sider [aria-current="page"]'),
  ).toHaveText('账户');
});

test('未登录直达详情只看到登录页，一个管理取数请求都不发；登录后回到原详情', async ({
  page,
  request,
}) => {
  const accountId = await openAccount(request, `unauth-${Date.now()}`);

  const management: string[] = [];
  page.on('request', (request) => {
    if (request.url().includes('/api/v1/')) management.push(request.url());
  });

  // 新开的标签页没有会话；地址直接指向账户详情。
  await page.goto(`${consoleUrl}#/accounts/${accountId}`);
  await expect(page.getByTestId('admin-sign-in')).toBeVisible();
  await expect(page.getByTestId('accounts-balance')).toHaveCount(0);
  expect(management, '未认证时不该向管理 API 发任何取数请求').toEqual([]);

  // 登录后地址不变，页面回到原来那个详情（Spec D6）。
  await page.getByTestId('admin-email').fill(settings.adminEmail);
  await page.getByTestId('admin-password').fill(settings.adminPassword);
  await page.getByTestId('admin-sign-in').click();
  await expect(page).toHaveURL(new RegExp(`#/accounts/${accountId}$`));
  await expect(page.getByTestId('accounts-selected-id')).toHaveText(accountId);
});

test('不存在的对象与未知地址显示找不到，不残留上一个对象的读数', async ({ page, request }) => {
  const accountId = await openAccount(request, `missing-${Date.now()}`);

  await signIn(page);
  await page.goto(`${consoleUrl}#/accounts/${accountId}`);
  await expect(page.getByTestId('accounts-balance')).toContainText('7');

  // 换成另一个不存在的标识：找不到，且上一个账户的读数与标签都不留在页面上。
  await page.goto(`${consoleUrl}#/accounts/00000000-0000-4000-8000-000000000000`);
  await expect(page.getByTestId('console-not-found')).toBeVisible();
  await expect(page.getByTestId('accounts-balance')).toHaveCount(0);
  await expect(page.getByTestId('accounts-selected-id')).toHaveCount(0);

  // 客户那条同理。
  await page.goto(`${consoleUrl}#/customers/00000000-0000-4000-8000-000000000000`);
  await expect(page.getByTestId('console-not-found')).toBeVisible();
  await expect(page.getByTestId('customers-detail-email')).toHaveCount(0);

  // 地址本身不是已知页面：也显示找不到，而不是悄悄落到模型目录。
  await page.goto(`${consoleUrl}#/no-such-page`);
  await expect(page.getByTestId('console-not-found')).toBeVisible();
});

test('会话被拒时回到登录页，并清掉上一名运营留下的列表筛选', async ({ page, request }) => {
  const tag = `expired-${Date.now()}`;
  await openAccount(request, tag);

  await signIn(page);
  await page.locator('.ant-layout-sider').getByRole('menuitem', { name: '账户' }).click();
  await page.getByTestId('accounts-lookup-tag').fill(tag);
  await page.getByTestId('accounts-search').click();
  await expect(page.locator('.ant-table-tbody').getByRole('row')).toHaveCount(1);

  // 会话在服务端被吊销或过期之后，管理读回 403。这里在浏览器层拦出同一个事实，免得为了造它去改
  // 管理员口令（那会牵动其他用例）。
  await page.route('**/api/v1/**', (route) =>
    route.fulfill({
      status: 403,
      contentType: 'application/json',
      body: JSON.stringify({ error: { code: 'forbidden', message: 'session is not accepted' } }),
    }),
  );

  // 刷新：页面重新挂载并发起管理读，收到 403 后回到登录页，管理数据不再显示。
  await page.reload();
  await expect(page.getByTestId('admin-sign-in')).toBeVisible();
  await expect(page.getByTestId('accounts-balance')).toHaveCount(0);

  // 会话结束也清掉了列表筛选：重新登录后，标签输入框是空的，不会看见上一名运营找过的条件。
  await page.unroute('**/api/v1/**');
  await page.getByTestId('admin-email').fill(settings.adminEmail);
  await page.getByTestId('admin-password').fill(settings.adminPassword);
  await page.getByTestId('admin-sign-in').click();
  await page.locator('.ant-layout-sider').getByRole('menuitem', { name: '账户' }).click();
  await expect(page.getByTestId('accounts-lookup-tag')).toHaveValue('');
});
