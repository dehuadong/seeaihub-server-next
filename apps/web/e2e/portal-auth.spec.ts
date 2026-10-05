import { expect, test, type Page } from '@playwright/test';
import { settings } from './settings';
import {
  pathnameOf,
  PORTAL_PASSWORD,
  portalAt,
  registerCustomer,
  signInCustomer,
  uniqueEmail,
} from './portal';

/// 客户公开认证页面：**只有真实浏览器才观测得到**的那一层——三个任务各自独立成页、公开地址直达与
/// 刷新、登录回跳、重置表单与结果语义、有会话访问边界（认证 Spec 0004 A1–A9、设计 0016 §1–§3）。
///
/// 注册/登录能力与账务主路径仍在 `portal-self-service.spec.ts`；页面地址与受保护页历史在
/// `portal-navigation.spec.ts`。接口层结果码由 `apps/api/tests/http_contract` 覆盖。

const RESET_ENDPOINT = '**/v1/customer/password-resets/redeem';

async function fillReset(page: Page, code: string, password: string): Promise<void> {
  await page.getByTestId('portal-reset-token').fill(code);
  await page.getByTestId('portal-reset-password').fill(password);
  await page.getByTestId('portal-reset-confirm').fill(password);
}

test('登录页只做登录/注册：没有重置输入、服务探活与支付说明', async ({ page }) => {
  await page.goto(portalAt('/login'));
  await expect(page.getByTestId('portal-auth-title')).toHaveText('登录');
  await expect(page.getByTestId('portal-email')).toBeVisible();
  await expect(page.getByTestId('portal-password')).toBeVisible();
  // 登录页不承载重置、探活或充值说明（Spec §2、A1）。
  await expect(page.getByTestId('portal-reset-token')).toHaveCount(0);
  await expect(page.getByTestId('portal-reset-password')).toHaveCount(0);
  const loginHtml = await page.content();
  expect(loginHtml).not.toContain('服务探活');
  expect(loginHtml).not.toContain('在线支付');

  // 同一地址切到注册模式：仍是既有注册能力，同样没有重置输入。
  await page.getByTestId('portal-mode-register').click();
  await expect(page.getByTestId('portal-auth-title')).toHaveText('注册');
  await expect(page.getByTestId('portal-submit')).toHaveText('注册并进入');
  await expect(page.getByTestId('portal-reset-token')).toHaveCount(0);

  await page.getByTestId('portal-email').fill(uniqueEmail('portal-auth-register'));
  await page.getByTestId('portal-password').fill(PORTAL_PASSWORD);
  await page.getByTestId('portal-submit').click();
  await expect(page.getByTestId('portal-balance')).toBeVisible();
});

test('忘记密码页只指导联系客服，不发任何签发请求', async ({ page }) => {
  const issued: string[] = [];
  page.on('request', (request) => {
    if (request.method() === 'POST' && request.url().includes('password-reset')) {
      issued.push(request.url());
    }
  });

  await page.goto(portalAt('/forgot-password'));
  await expect(page.getByTestId('portal-auth-title')).toHaveText('忘记密码');
  await expect(page.getByTestId('portal-forgot-hint')).toContainText('请联系平台客服获取重置码');
  await expect(page.getByTestId('portal-email')).toHaveCount(0);
  expect(issued, '找回页不该发起任何重置码签发请求').toEqual([]);

  await page.getByTestId('portal-forgot-have-code').click();
  expect(pathnameOf(page)).toBe('/reset-password');
  await expect(page.getByTestId('portal-reset-token')).toBeVisible();
});

test('三个认证地址直接打开、刷新与前进后退', async ({ page }) => {
  const pages: [string, string][] = [
    ['/login', '登录'],
    ['/forgot-password', '忘记密码'],
    ['/reset-password', '重置密码'],
  ];
  for (const [path, title] of pages) {
    await page.goto(portalAt(path));
    expect(pathnameOf(page), `直接打开 ${path}`).toBe(path);
    await expect(page.getByTestId('portal-auth-title')).toHaveText(title);
    await page.reload();
    expect(pathnameOf(page), `刷新 ${path}`).toBe(path);
    await expect(page.getByTestId('portal-auth-title')).toHaveText(title);
    // 尾斜杠形式按归一化规则落回同一页：开发回退与客户端归一化都必须认它。
    await page.goto(portalAt(`${path}/`));
    await expect(page.getByTestId('portal-auth-title')).toHaveText(title);
  }

  await page.goto(portalAt('/login'));
  await page.goto(portalAt('/forgot-password'));
  await page.goBack();
  expect(pathnameOf(page)).toBe('/login');
  await page.goForward();
  expect(pathnameOf(page)).toBe('/forgot-password');
});

test('未登录直达带日期区间的账单页，经找回、刷新、返回登录后回到原区间', async ({ page }) => {
  const email = uniqueEmail('portal-auth-return');
  await registerCustomer(page, email);
  await page.evaluate(() => sessionStorage.clear());

  const since = '2026-01-01T00:00:00.000Z';
  const until = '2026-02-01T00:00:00.000Z';
  await page.goto(
    portalAt(`/billing?since=${encodeURIComponent(since)}&until=${encodeURIComponent(until)}`),
  );
  // 未登录只显示登录，地址保留，不取账户数据。
  await expect(page.getByTestId('portal-submit')).toBeVisible();
  expect(pathnameOf(page)).toBe('/billing');

  // 从受保护地址进找回，刷新找回页、经重置页与返回登录都保留目标。
  await page.getByTestId('portal-forgot-link').click();
  expect(pathnameOf(page)).toBe('/forgot-password');
  await page.reload();
  await expect(page.getByTestId('portal-auth-title')).toHaveText('忘记密码');
  await page.getByTestId('portal-forgot-have-code').click();
  expect(pathnameOf(page)).toBe('/reset-password');
  await page.reload();
  await expect(page.getByTestId('portal-reset-token')).toBeVisible();
  await page.getByTestId('portal-reset-back').click();
  expect(pathnameOf(page)).toBe('/login');

  await signInCustomer(page, email);
  const url = new URL(page.url());
  expect(url.pathname).toBe('/billing');
  expect(url.searchParams.get('since')).toBe(since);
  expect(url.searchParams.get('until')).toBe(until);
});

test('含凭据的回跳候选整体拒绝，回概览', async ({ page }) => {
  const email = uniqueEmail('portal-auth-credential-return');
  await registerCustomer(page, email);
  await page.evaluate(() => sessionStorage.clear());

  // 日期区间合法也救不回它：查询参数里出现会话凭据时整条目标作废（设计 0016 §2）。
  await page.goto(
    portalAt(
      '/billing?since=2026-01-01T00:00:00.000Z&until=2026-02-01T00:00:00.000Z&session=abc',
    ),
  );
  await expect(page.getByTestId('portal-submit')).toBeVisible();
  await page.getByTestId('portal-forgot-link').click();
  await page.getByTestId('portal-forgot-back').click();
  await signInCustomer(page, email);
  expect(pathnameOf(page)).toBe('/');
});

test('同源存储里的非法回跳目标在读取时被重新校验', async ({ page }) => {
  const email = uniqueEmail('portal-auth-stored-return');
  await registerCustomer(page, email);

  for (const raw of ['https://evil.example.com/steal', '{"path":"/api/v1/admin"}', 'not json']) {
    await page.evaluate(() => sessionStorage.clear());
    await page.goto(portalAt('/login'));
    await page.evaluate((value) => sessionStorage.setItem('seeai.portal.returnTo', value), raw);
    await page.getByTestId('portal-email').fill(email);
    await page.getByTestId('portal-password').fill(PORTAL_PASSWORD);
    await page.getByTestId('portal-submit').click();
    await expect(page.getByTestId('portal-balance')).toBeVisible();
    expect(pathnameOf(page), `非法目标 ${raw} 应回概览`).toBe('/');
  }
});

test('重置表单校验不通过不发请求，有效输入只提交一次且确认密码不传送', async ({ page }) => {
  await page.goto(portalAt('/reset-password'));
  let calls = 0;
  await page.route(RESET_ENDPOINT, (route) => {
    calls += 1;
    return route.fulfill({ status: 204 });
  });

  // 缺重置码。
  await page.getByTestId('portal-reset-submit').click();
  await expect(page.getByTestId('portal-reset-token-field')).toContainText('请输入重置码');
  expect(calls).toBe(0);

  // 新密码过短。
  await page.getByTestId('portal-reset-token').fill('code-123');
  await page.getByTestId('portal-reset-password').fill('short');
  await page.getByTestId('portal-reset-confirm').fill('short');
  await page.getByTestId('portal-reset-submit').click();
  await expect(page.getByTestId('portal-reset-password-field')).toContainText('新密码至少 8 个字符');
  expect(calls).toBe(0);

  // 两次输入不一致。
  await page.getByTestId('portal-reset-password').fill('a-long-enough-password');
  await page.getByTestId('portal-reset-confirm').fill('a-different-password');
  await page.getByTestId('portal-reset-submit').click();
  await expect(page.getByTestId('portal-reset-confirm-field')).toContainText('两次输入的密码不一致');
  expect(calls).toBe(0);

  // 有效：重置码去首尾空白、密码保持原值、确认密码不进入请求体、只发一次。
  await page.getByTestId('portal-reset-token').fill('  code-123  ');
  await page.getByTestId('portal-reset-confirm').fill('a-long-enough-password');
  const [request] = await Promise.all([
    page.waitForRequest(RESET_ENDPOINT),
    page.getByTestId('portal-reset-submit').click(),
  ]);
  const body = request.postDataJSON();
  expect(body).toEqual({ reset_token: 'code-123', new_password: 'a-long-enough-password' });
  expect(request.postData(), '确认密码只用于浏览器校验').not.toContain('confirm');
  await expect(page.getByTestId('portal-reset-done')).toBeVisible();
  expect(calls).toBe(1);
});

test('重置结果语义：400 可改再试，429 显示等待时长，404 与未知结果终止本次表单', async ({ page }) => {
  await page.goto(portalAt('/reset-password'));

  await page.route(RESET_ENDPOINT, (route) =>
    route.fulfill({
      status: 400,
      contentType: 'application/json',
      body: JSON.stringify({ error: { code: 'invalid_parameter', message: 'invalid' } }),
    }),
  );
  await fillReset(page, 'bad-code', 'a-long-enough-password');
  await page.getByTestId('portal-reset-submit').click();
  await expect(page.getByTestId('portal-reset-error')).toContainText('重置码无效或已过期');
  await expect(page.getByTestId('portal-reset-token')).toHaveValue('bad-code');
  await expect(page.getByTestId('portal-reset-submit')).toBeVisible();

  await page.unroute(RESET_ENDPOINT);
  await page.route(RESET_ENDPOINT, (route) =>
    route.fulfill({
      status: 429,
      headers: { 'Retry-After': '30' },
      contentType: 'application/json',
      body: JSON.stringify({ error: { code: 'rate_limit_exceeded', message: 'too many' } }),
    }),
  );
  await page.getByTestId('portal-reset-submit').click();
  await expect(page.getByTestId('portal-reset-error')).toContainText('尝试过于频繁');
  await expect(page.getByTestId('portal-reset-error')).toContainText('30');
  await expect(page.getByTestId('portal-reset-token')).toHaveValue('bad-code');
  await expect(page.getByTestId('portal-reset-submit')).toBeVisible();

  // 5xx 保守归为结果未知：不显示成功、不自动重试、终止本次表单。
  await page.unroute(RESET_ENDPOINT);
  await page.route(RESET_ENDPOINT, (route) =>
    route.fulfill({
      status: 500,
      contentType: 'application/json',
      body: JSON.stringify({ error: { code: 'internal_error', message: 'boom' } }),
    }),
  );
  await page.getByTestId('portal-reset-submit').click();
  await expect(page.getByTestId('portal-reset-error')).toContainText('暂时无法确认密码是否已更新');
  await expect(page.getByTestId('portal-reset-done')).toHaveCount(0);
  await expect(page.getByTestId('portal-reset-submit')).toHaveCount(0);

  // 重新打开后 404 终止本次表单，保留登录与找回入口。
  await page.goto(portalAt('/reset-password'));
  await page.unroute(RESET_ENDPOINT);
  await page.route(RESET_ENDPOINT, (route) =>
    route.fulfill({
      status: 404,
      contentType: 'application/json',
      body: JSON.stringify({ error: { code: 'not_found', message: 'no customer' } }),
    }),
  );
  await fillReset(page, 'bad-code', 'a-long-enough-password');
  await page.getByTestId('portal-reset-submit').click();
  await expect(page.getByTestId('portal-reset-error')).toContainText('无法完成重置，请联系平台客服');
  await expect(page.getByTestId('portal-reset-submit')).toHaveCount(0);
  await expect(page.getByTestId('portal-reset-back')).toBeVisible();
  await expect(page.getByTestId('portal-reset-again')).toBeVisible();

  // 重新打开重置页：空白编辑态，不恢复、不重放上一次请求。
  await page.goto(portalAt('/reset-password'));
  await expect(page.getByTestId('portal-reset-token')).toHaveValue('');
});

test('非约定的 2xx 成功响应归为结果未知，不显示成功', async ({ page }) => {
  await page.goto(portalAt('/reset-password'));
  const answers = [
    { status: 200 },
    { status: 200, contentType: 'application/json', body: '{}' },
    { status: 202, contentType: 'application/json', body: '{}' },
  ];
  for (const answer of answers) {
    await page.unroute(RESET_ENDPOINT);
    await page.route(RESET_ENDPOINT, (route) => route.fulfill(answer));
    await fillReset(page, 'code-123', 'a-long-enough-password');
    await page.getByTestId('portal-reset-submit').click();
    await expect(page.getByTestId('portal-reset-error')).toContainText('暂时无法确认密码是否已更新');
    await expect(page.getByTestId('portal-reset-done')).toHaveCount(0);
    await page.goto(portalAt('/reset-password'));
  }
});

test('有会话访问登录或忘记密码进概览，重置成功与返回登录都清本地会话', async ({ page, request }) => {
  const email = uniqueEmail('portal-auth-session');
  await registerCustomer(page, email);
  const accountId = await page.evaluate(() =>
    sessionStorage.getItem('seeai.portal.session.account'),
  );
  expect(accountId).toBeTruthy();

  // 有会话访问公开登录/找回地址：进概览。地址改写发生在挂载之后，所以先等外壳出现再断言地址。
  await page.goto(portalAt('/login'));
  await expect(page.getByTestId('portal-balance')).toBeVisible();
  expect(pathnameOf(page)).toBe('/');
  await page.goto(portalAt('/forgot-password'));
  await expect(page.getByTestId('portal-balance')).toBeVisible();
  expect(pathnameOf(page)).toBe('/');

  // 有会话可直接打开重置页，且不展示账户数据。
  await page.goto(portalAt('/reset-password'));
  await expect(page.getByTestId('portal-reset-token')).toBeVisible();
  await expect(page.getByTestId('portal-balance')).toHaveCount(0);

  const issued = await request.post(
    `http://127.0.0.1:${settings.port}/api/v1/accounts/${accountId}/password-reset`,
    { headers: { authorization: `Bearer ${settings.adminToken}` } },
  );
  expect(issued.status()).toBe(201);
  const code = (await issued.json()).reset_token as string;

  await fillReset(page, code, 'a-fresh-enough-password');
  await page.getByTestId('portal-reset-submit').click();
  await expect(page.getByTestId('portal-reset-done')).toBeVisible();
  // 确定成功后清除本地客户会话，即使重置码属于当前登录的客户。
  expect(await page.evaluate(() => sessionStorage.getItem('seeai.portal.session'))).toBeNull();

  // 返回登录能显示登录表单。
  await page.getByTestId('portal-reset-back').click();
  expect(pathnameOf(page)).toBe('/login');
  await expect(page.getByTestId('portal-submit')).toBeVisible();
});

test('登录被尝试限制拒绝时显示等待提示，不写成密码错误', async ({ page }) => {
  await page.goto(portalAt('/login'));
  await page.route('**/v1/customer/sessions', (route) =>
    route.fulfill({
      status: 429,
      headers: { 'Retry-After': '42' },
      contentType: 'application/json',
      body: JSON.stringify({ error: { code: 'rate_limit_exceeded', message: 'too many' } }),
    }),
  );
  await page.getByTestId('portal-email').fill('someone@example.com');
  await page.getByTestId('portal-password').fill(PORTAL_PASSWORD);
  await page.getByTestId('portal-submit').click();

  const alert = page.getByTestId('portal-auth-error');
  await expect(alert).toContainText('尝试过于频繁');
  await expect(alert).toContainText('42');
  await expect(alert).not.toContainText('邮箱或密码不正确');
  // 已填输入保留，等待结束后可再次提交。
  await expect(page.getByTestId('portal-password')).toHaveValue(PORTAL_PASSWORD);
  await expect(page.getByTestId('portal-submit')).toBeEnabled();
});

test('重复日期参数的回跳候选只保留页面路径', async ({ page }) => {
  const email = uniqueEmail('portal-auth-duplicate-dates');
  await registerCustomer(page, email);
  await page.evaluate(() => sessionStorage.clear());

  // 用不写缺省区间的受保护页：登录目标是否带查询参数只取决于回跳校验，不被页面补写干扰。
  await page.goto(
    portalAt(
      '/keys?since=2026-01-01T00:00:00.000Z&since=2026-02-01T00:00:00.000Z&until=2026-03-01T00:00:00.000Z',
    ),
  );
  await expect(page.getByTestId('portal-submit')).toBeVisible();
  await page.getByTestId('portal-forgot-link').click();
  await page.getByTestId('portal-forgot-back').click();
  await signInCustomer(page, email);

  const url = new URL(page.url());
  expect(url.pathname).toBe('/keys');
  expect(url.search, '重复日期参数不保留为区间').toBe('');
});

test('晚到的登录响应不污染已离开的页面，也不替它建会话', async ({ page }) => {
  const email = uniqueEmail('portal-auth-late');
  await registerCustomer(page, email);
  await page.evaluate(() => sessionStorage.clear());

  await page.goto(portalAt('/login'));
  await page.route('**/v1/customer/sessions', async (route) => {
    await new Promise((resolve) => setTimeout(resolve, 1_500));
    await route.continue();
  });
  await page.getByTestId('portal-email').fill(email);
  await page.getByTestId('portal-password').fill(PORTAL_PASSWORD);
  await page.getByTestId('portal-submit').click();

  // 请求还没回来就离开登录页；晚到的响应不得替新页面建会话或跳转。
  await page.getByTestId('portal-forgot-link').click();
  await expect(page.getByTestId('portal-auth-title')).toHaveText('忘记密码');
  await page.waitForTimeout(2_000);
  expect(pathnameOf(page), '晚到响应不该把页面带回控制台').toBe('/forgot-password');
  expect(await page.evaluate(() => sessionStorage.getItem('seeai.portal.session'))).toBeNull();
});

test('响应丢失保守归为结果未知，不自动重试', async ({ page }) => {
  await page.goto(portalAt('/reset-password'));
  let calls = 0;
  await page.route(RESET_ENDPOINT, (route) => {
    calls += 1;
    return route.abort();
  });
  await fillReset(page, 'code-123', 'a-long-enough-password');
  await page.getByTestId('portal-reset-submit').click();
  await expect(page.getByTestId('portal-reset-error')).toContainText('暂时无法确认密码是否已更新');
  await expect(page.getByTestId('portal-reset-done')).toHaveCount(0);
  expect(calls, '未知结果不自动重试').toBe(1);
});

test('窄屏下三个认证页无横向溢出，字段有可访问标签', async ({ page }) => {
  await page.setViewportSize({ width: 360, height: 740 });
  const pages: [string, string][] = [
    ['/login', '登录'],
    ['/forgot-password', '忘记密码'],
    ['/reset-password', '重置密码'],
  ];
  for (const [path, title] of pages) {
    await page.goto(portalAt(path));
    await expect(page.getByTestId('portal-auth-title')).toHaveText(title);
    const overflow = await page.evaluate(
      () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
    );
    expect(overflow, `${path} 在窄屏下不应横向溢出`).toBeLessThanOrEqual(1);
  }

  // 输入框有可见标签（可访问名）。
  await page.goto(portalAt('/login'));
  await expect(page.getByLabel('邮箱')).toBeVisible();
  await expect(page.getByLabel('密码（至少 8 个字符）')).toBeVisible();
  await page.goto(portalAt('/reset-password'));
  for (const label of ['重置码', '新密码（至少 8 个字符）', '确认新密码']) {
    await expect(page.getByLabel(label)).toBeVisible();
  }
});
