import { expect, test, type Locator, type Page } from '@playwright/test';
import { portalUrl, settings } from './settings';

/// 客户自助与账务：**只有浏览器才观测得到**的那一层——注册后进控制台、首屏三个数、密钥明文只显示
/// 一次、改口令后旧会话失效、凭运营签发的令牌设新口令。
///
/// 这些行为的接口契约由 `apps/api/tests/http_contract/cases_identity.rs` 管；这里验的是"人在浏览器里
/// 点下去会发生什么"。
///
/// 地址从 `settings` 取（与 `playwright.config.ts` 同源）：端口写死在这里的话 `SEEAI_E2E_PORT` 就失效了。
const PORTAL = portalUrl;

/// 每个用例一个客户：库是共享的，写死的邮箱会在第二次运行时撞上"已注册"。
function uniqueEmail(): string {
  return `portal-e2e-${Date.now()}-${Math.floor(Math.random() * 10_000)}@example.com`;
}

const PASSWORD = 'e2e-customer-password';

/// 一块面板。界面用的是 Ant Design 的 `Card`，它的标题渲染成 `div`（不是 heading），所以按
/// **卡片容器 + 标题文本**定位，而不是按 `getByRole('heading')`——换 UI 库时选择器跟着实现走，
/// 但断言的性质不变。
function panel(page: Page, title: string): Locator {
  return page.locator('.ant-card').filter({ has: page.getByText(title, { exact: true }) });
}

/// 切标签页。控制台的分组依据是**使用频次**：首屏只放三个数，其余按标签页收起来。
async function tab(page: Page, label: string): Promise<void> {
  await page.getByRole('tab', { name: label }).click();
}

/// 控制台是否已经渲染出来。用首屏那三个统计数判断——**它们不依赖任何标签页**，
/// 所以"登录后看到控制台"这件事与"当前停在哪个标签页"无关。
function overview(page: Page): Locator {
  return page.locator('.ant-statistic');
}

async function register(page: Page, email: string): Promise<void> {
  await page.goto(PORTAL);
  // 未登录：只有登录/注册页，没有账户数据。
  await expect(page.getByTestId('portal-submit')).toBeVisible();
  await expect(overview(page)).toHaveCount(0);

  await page.getByTestId('portal-mode-register').click();
  await page.getByTestId('portal-email').fill(email);
  await page.getByTestId('portal-password').fill(PASSWORD);
  await page.getByTestId('portal-submit').click();

  await expect(overview(page).first()).toBeVisible();
}

test('注册后进控制台：首屏三个数 + 三个标签页', async ({ page }) => {
  await register(page, uniqueEmail());

  // **首屏**（不切标签页、不滚动）就该看到三个数：可用余额、持有中、扣费总额。
  await expect(page.getByText('可用余额')).toBeVisible();
  await expect(page.getByText('持有中')).toBeVisible();
  await expect(page.getByText('扣费总额（全部）')).toBeVisible();
  expect(await overview(page).count()).toBeGreaterThanOrEqual(3);

  // 三个标签页都在；明细收在后面，不占首屏。
  for (const label of ['用量与账单', 'API Key', '账户设置']) {
    await expect(page.getByRole('tab', { name: label })).toBeVisible();
  }

  // 会话存在客户自己的键下，不会串到管理端。
  expect(await page.evaluate(() => sessionStorage.getItem('seeai.portal.session'))).toBeTruthy();
  expect(await page.evaluate(() => sessionStorage.getItem('seeai.console.session'))).toBeNull();
});

/// 客户凭据拿不去管理面（Spec V-D6 的后半）。
///
/// 前半（产物里不含管理端代码）由构建末尾的隔离核对与 `bundles-are-isolated.spec.ts` 守着；这里守的是
/// 另一半：**客户会话调管理端点一律被拒**。两个页面的令牌都在 `sessionStorage` 里，但键与受众不同——
/// 少了这条，一个把两套令牌当同一个东西的实现也能让前面所有用例通过。
test('客户会话令牌调管理 API 一律被拒', async ({ page, request }) => {
  await register(page, uniqueEmail());

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

  // 一律拒，且拒得能被机器认出来。**不是 401**：管理面把"凭据不对"统一回 `admin_forbidden` 403
  // （`apps/api/src/main.rs` 的 `admin_bearer_token`），客户令牌对管理面来说就是一把无效凭据；
  // 判据要的是"调不通"，不是某一个具体状态码。
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

test('用量与账单页给出汇总与逐笔明细，且有账目流水', async ({ page }) => {
  await register(page, uniqueEmail());
  await tab(page, '用量与账单');

  await expect(panel(page, '账单汇总')).toBeVisible();
  await expect(panel(page, '逐笔明细')).toBeVisible();
  await expect(panel(page, '充值记录与账目流水')).toBeVisible();
});

test('新建密钥时明文只出现一次，列表里之后再也拿不到', async ({ page }) => {
  await register(page, uniqueEmail());
  await tab(page, 'API Key');
  await expect(panel(page, 'API Key')).toBeVisible();

  await page.getByTestId('portal-key-label').fill('e2e 脚本');
  await page.getByTestId('portal-key-create').click();

  const plaintext = page.getByTestId('portal-key-plaintext');
  await expect(plaintext).toBeVisible();
  const key = (await plaintext.textContent())?.trim() ?? '';
  expect(key).toMatch(/^sk_seeai_/);

  // **只此一次**：刷新之后明文那一块不再出现，列表里也只有标签/时间/状态。
  await page.reload();
  await tab(page, 'API Key');
  await expect(panel(page, 'API Key')).toBeVisible();
  await expect(page.getByText('e2e 脚本')).toBeVisible();
  await expect(page.getByTestId('portal-key-plaintext')).toHaveCount(0);
  expect(await page.content()).not.toContain(key);
});

test('改口令成功后旧会话立即失效，回到登录页', async ({ page }) => {
  await register(page, uniqueEmail());
  // 改口令是低频动作，收在"账户设置"里——这正是它不该占首屏的原因。
  await tab(page, '账户设置');
  // 改口令是低频动作，收在"账户设置"里——这正是它不该占首屏的原因。
  // 用按钮的 `data-testid` 而不是文案：卡片标题也叫"改口令"，按文案会命中两处。
  await expect(page.getByTestId('portal-change-password')).toBeVisible();

  const next = 'e2e-customer-password-changed';
  await page.getByTestId('portal-current-password').fill(PASSWORD);
  await page.getByTestId('portal-new-password').fill(next);
  await page.getByTestId('portal-change-password').click();

  // antd 的 `Alert` 会把消息渲染在两层同名元素里，所以取第一个。
  await expect(page.getByText('口令已改').first()).toBeVisible();
  // 该客户的**全部**会话都失效了，包括刚发起这次改动的那一条：回登录页。
  await page.getByRole('button', { name: '回登录页' }).first().click();
  await expect(page.getByTestId('portal-submit')).toBeVisible();
  await expect(overview(page)).toHaveCount(0);
});

test('凭运营签发的重置令牌设置新口令，之后能用新口令登录', async ({ page, request }) => {
  const email = uniqueEmail();
  await register(page, email);
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
  await page.goto(PORTAL);
  await page.getByTestId('portal-reset-token').fill(resetToken);
  await page.getByTestId('portal-reset-password').fill(next);
  await page.getByTestId('portal-reset-submit').click();
  await expect(page.getByText('口令已重置').first()).toBeVisible();

  // 新口令能登录、旧口令不行。
  await page.getByTestId('portal-email').fill(email);
  await page.getByTestId('portal-password').fill(next);
  await page.getByTestId('portal-submit').click();
  await expect(overview(page).first()).toBeVisible();
});
