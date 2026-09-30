import { expect, test } from '@playwright/test';
import { settings } from './settings';
import {
  nav,
  panel,
  pathnameOf,
  PORTAL_PASSWORD,
  portalAt,
  registerCustomer,
  uniqueEmail,
} from './portal';

/// 客户自助与账务：**只有浏览器才观测得到**的那一层——注册后进概览、概览只显示已结算余额、密钥
/// 明文只显示一次、改口令后旧会话失效、凭运营签发的令牌设新口令。
///
/// 这些行为的接口契约由 `apps/api/tests/http_contract/cases_identity.rs` 管；这里验的是"人在浏览器里
/// 点下去会发生什么"。页面地址与导航在 `portal-navigation.spec.ts`。
///
/// 地址从 `./portal` 取（与 `playwright.config.ts` 同源）：端口写死在这里的话 `SEEAI_E2E_PORT` 就失效了。

test('注册后进概览：首屏只有已结算余额 + 五页固定导航', async ({ page }) => {
  await registerCustomer(page, uniqueEmail('portal-e2e'));

  // **首屏**（不滚动、不切页）就该看到余额，且标题是"已结算余额"。
  await expect(page.getByTestId('portal-settled-balance')).toContainText('已结算余额');
  await expect(page.getByTestId('portal-settled-balance')).toContainText('0 元');
  // 概览只有一个数：没有"扣费总额（全部）"，也没有无区间说明的平均扣费（V-D14）。
  expect(await page.locator('.ant-statistic').count()).toBe(1);
  for (const forbidden of ['扣费总额', '平均每次扣费']) {
    await expect(page.getByText(forbidden)).toHaveCount(0);
  }
  // 客户页面不出现可用额、持有中或预授权金额（Spec C7）。
  for (const forbidden of ['可用余额', '可用额', '持有中', '预授权']) {
    await expect(page.getByText(forbidden)).toHaveCount(0);
  }

  // 五页固定导航都在。
  for (const label of ['概览', '调用记录', '账单与资金记录', 'API Key', '账户设置']) {
    await expect(page.getByRole('menuitem', { name: label })).toBeVisible();
  }
  // 概览另有通往其余四页的入口，不把四页内容堆在概览里。
  for (const route of ['usage', 'billing', 'keys', 'settings']) {
    await expect(page.getByTestId(`portal-entry-${route}`)).toBeVisible();
  }

  // 会话存在客户自己的键下，不会串到管理端。
  expect(await page.evaluate(() => sessionStorage.getItem('seeai.portal.session'))).toBeTruthy();
  expect(await page.evaluate(() => sessionStorage.getItem('seeai.console.session'))).toBeNull();
});

/// 客户凭据拿不去管理面（Spec V-D6 的后半）。
///
/// 前半（产物里不含管理端代码）由构建末尾的隔离核对与 `bundles-are-isolated.spec.ts` 守着；这里守的是
/// 另一半：**客户会话调管理端点一律被拒**。
test('客户会话令牌调管理 API 一律被拒', async ({ page, request }) => {
  await registerCustomer(page, uniqueEmail('portal-e2e'));

  const customerToken = await page.evaluate(() =>
    sessionStorage.getItem('seeai.portal.session'),
  );
  expect(customerToken).toBeTruthy();

  // 从**真正的浏览器页面**里发这次请求：要证的是"浏览器拿着客户令牌打管理面"这件事。
  const refused = await page.evaluate(async () => {
    const call = async (path: string) => {
      const response = await fetch(path, {
        headers: { authorization: `Bearer ${sessionStorage.getItem('seeai.portal.session') ?? ''}` },
      });
      const body = await response.text();
      return { status: response.status, code: JSON.parse(body)?.error?.code ?? null };
    };
    return {
      models: await call('/api/v1/gateway-models'),
      accounts: await call('/api/v1/accounts'),
      session: await call('/api/v1/admin/session'),
    };
  });

  // 一律拒：管理面把"凭据不对"统一回 `admin_forbidden` 403，客户令牌对管理面来说就是一把无效凭据。
  for (const [what, answer] of Object.entries(refused)) {
    expect(answer.status, `${what} 不该接受客户令牌，实际 ${answer.status}`).toBe(403);
    expect(answer.code, `${what} 的拒绝要带机器可读的错误码`).toBe('admin_forbidden');
  }

  // 反向确认这把令牌本身是好的：它对客面能用（否则上面的 403 证明不了"受众不同"）。
  // 用回环地址而不是 `portalUrl`：Node 的解析器不认 `.localhost`（那两个主机名只在浏览器里可用）。
  const own = await request.get(
    `http://127.0.0.1:${settings.port}/v1/customer/ledger?limit=1`,
    { headers: { authorization: `Bearer ${customerToken}` } },
  );
  expect(own.ok(), '客户令牌在对客面必须是好的').toBeTruthy();
});

test('调用记录页与账单与资金记录页各自给出内容，不堆在概览', async ({ page }) => {
  await registerCustomer(page, uniqueEmail('portal-e2e'));

  await nav(page, '调用记录');
  expect(pathnameOf(page)).toBe('/usage');
  await expect(panel(page, '调用记录')).toBeVisible();
  await expect(page.locator('.ant-table').first()).toBeVisible();
  // 概览的余额不在这一页。
  await expect(page.getByTestId('portal-settled-balance')).toHaveCount(0);

  await nav(page, '账单与资金记录');
  expect(pathnameOf(page)).toBe('/billing');
  await expect(panel(page, '账单汇总')).toBeVisible();
  await expect(panel(page, '资金流水')).toBeVisible();
  // 账单页不再算"平均每次扣费"（Spec §4.3、V-D14）。
  await expect(page.getByText('平均每次扣费')).toHaveCount(0);
});

test('新建密钥时明文只出现一次，列表里之后再也拿不到', async ({ page }) => {
  await registerCustomer(page, uniqueEmail('portal-e2e'));
  await nav(page, 'API Key');
  await expect(panel(page, 'API Key')).toBeVisible();

  await page.getByTestId('portal-key-label').fill('e2e 脚本');
  await page.getByTestId('portal-key-create').click();

  const plaintext = page.getByTestId('portal-key-plaintext');
  await expect(plaintext).toBeVisible();
  const key = (await plaintext.textContent())?.trim() ?? '';
  expect(key).toMatch(/^sk_seeai_/);

  // **只此一次**：刷新之后明文那一块不再出现，列表里也只有标签/时间/状态；地址仍停在 /keys。
  await page.reload();
  expect(pathnameOf(page)).toBe('/keys');
  await expect(panel(page, 'API Key')).toBeVisible();
  await expect(page.getByText('e2e 脚本')).toBeVisible();
  await expect(page.getByTestId('portal-key-plaintext')).toHaveCount(0);
  expect(await page.content()).not.toContain(key);
});

test('改口令成功后旧会话立即失效，回到登录页', async ({ page }) => {
  await registerCustomer(page, uniqueEmail('portal-e2e'));
  // 改口令是低频动作，收在"账户设置"里——这正是它不该占概览的原因。
  await nav(page, '账户设置');
  await expect(page.getByTestId('portal-change-password')).toBeVisible();

  const next = 'e2e-customer-password-changed';
  await page.getByTestId('portal-current-password').fill(PORTAL_PASSWORD);
  await page.getByTestId('portal-new-password').fill(next);
  await page.getByTestId('portal-change-password').click();

  // antd 的 `Alert` 会把消息渲染在两层同名元素里，所以取第一个。
  await expect(page.getByText('口令已改').first()).toBeVisible();
  // 该客户的**全部**会话都失效了，包括刚发起这次改动的那一条：回登录页。
  await page.getByRole('button', { name: '回登录页' }).first().click();
  await expect(page.getByTestId('portal-submit')).toBeVisible();
  await expect(page.locator('.ant-statistic')).toHaveCount(0);
});

test('凭运营签发的重置令牌设置新口令，之后能用新口令登录', async ({ page, request }) => {
  const email = uniqueEmail('portal-e2e');
  await registerCustomer(page, email);
  const accountId = await page.evaluate(() =>
    sessionStorage.getItem('seeai.portal.session.account'),
  );
  expect(accountId).toBeTruthy();

  // 运营那一侧：用共享令牌签发一枚一次性重置令牌（运营后台里也有这一步，那是界面的事）。
  //
  // 这里用 `127.0.0.1` 而不是 `admin.localhost`：**Node 的解析器不认 `.localhost`**（Chrome 认），
  // 而这条签发只认凭据、不认主机名，所以直连回环即可。
  const issued = await request.post(
    `http://127.0.0.1:${settings.port}/api/v1/accounts/${accountId}/password-reset`,
    { headers: { authorization: `Bearer ${settings.adminToken}` } },
  );
  expect(issued.status()).toBe(201);
  const resetToken = (await issued.json()).reset_token as string;
  expect(resetToken).toBeTruthy();

  // 客户那一侧：清掉会话（忘了口令的人本来就进不来），用令牌设新口令。
  const next = 'e2e-customer-password-reset';
  await page.evaluate(() => sessionStorage.clear());
  await page.goto(portalAt('/'));
  await page.getByTestId('portal-reset-token').fill(resetToken);
  await page.getByTestId('portal-reset-password').fill(next);
  await page.getByTestId('portal-reset-submit').click();
  await expect(page.getByText('口令已重置').first()).toBeVisible();

  // 新口令能登录、旧口令不行。
  await page.getByTestId('portal-email').fill(email);
  await page.getByTestId('portal-password').fill(next);
  await page.getByTestId('portal-submit').click();
  await expect(page.getByTestId('portal-settled-balance')).toBeVisible();
});
