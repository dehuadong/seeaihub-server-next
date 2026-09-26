import { expect, test } from '@playwright/test';

/// 客户自助与账务：**只有浏览器才观测得到**的那一层——注册后进控制台、四块数据都渲染出来、
/// 密钥明文只显示一次、改口令后旧会话失效、凭运营签发的令牌设新口令。
///
/// 这些行为的接口契约由 `apps/api/tests/http_contract/cases_identity.rs` 管；这里验的是"人在浏览器里
/// 点下去会发生什么"。
const PORTAL = 'http://app.localhost:8090/';

/// 每个用例一个客户：库是共享的，写死的邮箱会在第二次运行时撞上"已注册"。
function uniqueEmail(): string {
  return `portal-e2e-${Date.now()}-${Math.floor(Math.random() * 10_000)}@example.com`;
}

const PASSWORD = 'e2e-customer-password';

test('注册后进控制台，四块数据都渲染出来', async ({ page }) => {
  await page.goto(PORTAL);

  // 未登录：只有登录/注册页，没有账户数据。
  await expect(page.getByTestId('portal-submit')).toBeVisible();
  await expect(page.getByRole('heading', { name: '余额与持有' })).toHaveCount(0);

  await page.getByTestId('portal-mode-register').click();
  await page.getByTestId('portal-email').fill(uniqueEmail());
  await page.getByTestId('portal-password').fill(PASSWORD);
  await page.getByTestId('portal-submit').click();

  await expect(page.getByRole('heading', { name: '余额与持有' })).toBeVisible();
  await expect(page.getByRole('heading', { name: 'API Key' })).toBeVisible();
  await expect(page.getByRole('heading', { name: '用量与账单' })).toBeVisible();
  await expect(page.getByRole('heading', { name: '改口令' })).toBeVisible();

  // 余额与持有必须**分开**给（合成"总资产"会说清不了一笔钱扣没扣）。
  await expect(page.getByText('可用余额')).toBeVisible();
  await expect(page.getByText('持有中（已预授权、还没结算）')).toBeVisible();

  // 会话存在客户自己的键下，不会串到管理端。
  expect(await page.evaluate(() => sessionStorage.getItem('seeai.portal.session'))).toBeTruthy();
  expect(await page.evaluate(() => sessionStorage.getItem('seeai.console.session'))).toBeNull();
});

test('新建密钥时明文只出现一次，列表里之后再也拿不到', async ({ page }) => {
  await page.goto(PORTAL);
  await page.getByTestId('portal-mode-register').click();
  await page.getByTestId('portal-email').fill(uniqueEmail());
  await page.getByTestId('portal-password').fill(PASSWORD);
  await page.getByTestId('portal-submit').click();
  await expect(page.getByRole('heading', { name: 'API Key' })).toBeVisible();

  await page.getByTestId('portal-key-label').fill('e2e 脚本');
  await page.getByTestId('portal-key-create').click();

  const plaintext = page.getByTestId('portal-key-plaintext');
  await expect(plaintext).toBeVisible();
  const key = (await plaintext.textContent())?.trim() ?? '';
  expect(key).toMatch(/^sk_seeai_/);

  // **只此一次**：刷新之后明文那一块不再出现，列表里也只有标签/时间/状态。
  await page.reload();
  await expect(page.getByRole('heading', { name: 'API Key' })).toBeVisible();
  await expect(page.getByText('e2e 脚本')).toBeVisible();
  await expect(page.getByTestId('portal-key-plaintext')).toHaveCount(0);
  expect(await page.content()).not.toContain(key);
});

test('改口令成功后旧会话立即失效，回到登录页', async ({ page }) => {
  await page.goto(PORTAL);
  await page.getByTestId('portal-mode-register').click();
  await page.getByTestId('portal-email').fill(uniqueEmail());
  await page.getByTestId('portal-password').fill(PASSWORD);
  await page.getByTestId('portal-submit').click();
  await expect(page.getByRole('heading', { name: '改口令' })).toBeVisible();

  const next = 'e2e-customer-password-changed';
  await page.getByTestId('portal-current-password').fill(PASSWORD);
  await page.getByTestId('portal-new-password').fill(next);
  await page.getByTestId('portal-change-password').click();

  await expect(page.getByText('口令已改')).toBeVisible();
  // 该客户的**全部**会话都失效了，包括刚发起这次改动的那一条：回登录页。
  await page.getByRole('button', { name: '回登录页' }).click();
  await expect(page.getByTestId('portal-submit')).toBeVisible();
  await expect(page.getByRole('heading', { name: '余额与持有' })).toHaveCount(0);
});

test('凭运营签发的重置令牌设置新口令，之后能用新口令登录', async ({ page, request }) => {
  const email = uniqueEmail();
  await page.goto(PORTAL);
  await page.getByTestId('portal-mode-register').click();
  await page.getByTestId('portal-email').fill(email);
  await page.getByTestId('portal-password').fill(PASSWORD);
  await page.getByTestId('portal-submit').click();
  await expect(page.getByRole('heading', { name: '余额与持有' })).toBeVisible();
  const accountId = await page.evaluate(() =>
    sessionStorage.getItem('seeai.portal.session.account'),
  );
  expect(accountId).toBeTruthy();

  // 运营那一侧：用共享令牌签发一枚一次性重置令牌（运营后台里也有这一步，那是界面的事）。
  //
  // 这里用 `127.0.0.1` 而不是 `admin.localhost`：**Node 的解析器不认 `.localhost`**（Chrome 认），
  // 而这条签发只认凭据、不认主机名，所以直连回环即可。
  const issued = await request.post(
    `http://127.0.0.1:8090/api/v1/accounts/${accountId}/password-reset`,
    { headers: { authorization: `Bearer ${process.env.SEEAI_E2E_ADMIN_TOKEN ?? 'e2e-shared-token'}` } },
  );
  expect(issued.status()).toBe(201);
  const resetToken = (await issued.json()).reset_token as string;
  expect(resetToken).toBeTruthy();

  // 客户那一侧：清掉会话（忘了口令的人本来就进不来），用令牌设新口令。
  const next = 'e2e-customer-password-reset';
  await page.evaluate(() => sessionStorage.clear());
  await page.goto(PORTAL);
  await page.getByTestId('portal-reset-token').fill(resetToken);
  await page.getByTestId('portal-reset-password').fill(next);
  await page.getByTestId('portal-reset-submit').click();
  await expect(page.getByText('口令已重置')).toBeVisible();

  // 新口令能登录、旧口令不行。
  await page.getByTestId('portal-email').fill(email);
  await page.getByTestId('portal-password').fill(next);
  await page.getByTestId('portal-submit').click();
  await expect(page.getByRole('heading', { name: '余额与持有' })).toBeVisible();
});
