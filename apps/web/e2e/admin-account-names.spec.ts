import { expect, test } from '@playwright/test';
import { consoleUrl, settings } from './settings';

/// 账户名称在运营台上的行为（账户名称 Spec `0003` V1/V2/V6/V9）。
///
/// 只有浏览器才观测得到的那一层：表单留空也提交得出去、详情显示**生成出来的**名称、改名之后列表与
/// 详情读的都是新值、名称筛选能按子串找。生成规则本身由接口用例在真库上验。

async function signIn(page: import('@playwright/test').Page): Promise<void> {
  await page.goto(consoleUrl);
  await page.getByTestId('admin-email').fill(settings.adminEmail);
  await page.getByTestId('admin-password').fill(settings.adminPassword);
  await page.getByTestId('admin-sign-in').click();
  await page.locator('.ant-layout-sider').getByRole('menuitem', { name: '账户' }).click();
}

test('建账户留空名称也会生成一个，改名之后列表与详情都读新值', async ({ page }) => {
  const name = `星尘工作室-${Date.now()}`;
  await signIn(page);

  // 名称是必填？不是：留空即生成。表单里给出提示，但不拦提交。
  await page.getByTestId('accounts-create-open').click();
  await expect(page.getByTestId('accounts-create-name')).toBeVisible();
  // §3.1 的说明文字与「路由标签」这个名字都是合同固定的界面文案。
  await expect(
    page.getByText('创建账户，暂不开通邮箱登录；可先充值、签发 API Key，之后再开通登录。'),
  ).toBeVisible();
  await expect(page.getByText('路由标签（可选）')).toBeVisible();
  await page.getByTestId('accounts-create-submit').click();

  // 建完直接进详情：留空的那一次显示的是**服务端生成**的名称（形状固定，具体值由 id 决定）。
  await expect(page).toHaveURL(/#\/accounts\/[0-9a-f-]{36}$/);
  await expect(page.getByTestId('accounts-detail-name')).toHaveText(/^账户_[0-9a-f]{8}$/);

  // 改名：保存后详情立刻显示新值。
  await page.getByTestId('accounts-name-input').fill(name);
  await page.getByTestId('accounts-name-submit').click();
  await expect(page.getByTestId('accounts-detail-name')).toHaveText(name);

  // 名称是主要识别文字：回列表按名称子串就能找到它。
  await page.getByTestId('accounts-back-to-list').click();
  await page.getByTestId('accounts-lookup-name').fill(name.slice(0, 4));
  await page.getByTestId('accounts-search').click();
  await expect(page.locator('.ant-table-tbody').getByText(name)).toBeVisible();
});

test('建账户时填的名称被采用，标签与名称一次提交', async ({ page }) => {
  const name = `指定名称-${Date.now()}`;
  const tag = `e2e-name-tag-${Date.now()}`;
  await signIn(page);

  await page.getByTestId('accounts-create-open').click();
  // 名称规则的前端同款预检：控制字符在本地就被拦下，连请求都不发。
  await page.getByTestId('accounts-create-name').fill('星尘\t工作室');
  await page.getByTestId('accounts-create-submit').click();
  await expect(page.getByText('账户名称不能包含控制字符或格式字符').first()).toBeVisible();

  await page.getByTestId('accounts-create-name').fill(name);
  await page.getByTestId('accounts-create-tag').fill(tag);
  await page.getByTestId('accounts-create-submit').click();

  await expect(page).toHaveURL(/#\/accounts\/[0-9a-f-]{36}$/);
  await expect(page.getByTestId('accounts-detail-name')).toHaveText(name);
  // 名称与标签都是这一次创建带上的：详情里两项都能读到刚填的值。
  await expect(page.getByTestId('accounts-tag-input')).toHaveValue(tag);
});

test('绑定已有账户要预览确认：不给预览不能提交，换了标识也不能用旧预览提交', async ({ page }) => {
  const name = `可绑定的账户-${Date.now()}`;
  const otherName = `另一个账户-${Date.now()}`;
  await signIn(page);

  // 先建两个带名称的账户：一个用来预览，另一个用来验"换了标识就不能拿旧预览提交"。
  const createAccount = async (accountName: string) => {
    await page.getByTestId('accounts-create-open').click();
    await page.getByTestId('accounts-create-name').fill(accountName);
    await page.getByTestId('accounts-create-submit').click();
    await expect(page).toHaveURL(/#\/accounts\/[0-9a-f-]{36}$/);
    const id = page.url().split('/').pop() ?? '';
    expect(id).toMatch(/^[0-9a-f-]{36}$/);
    return id;
  };
  const accountId = await createAccount(name);
  // 详情替换了列表，建第二个账户前要先回列表（否则找不到建账户入口）。
  await page.getByTestId('accounts-back-to-list').click();
  const otherId = await createAccount(otherName);

  await page.locator('.ant-layout-sider').getByRole('menuitem', { name: '客户登录' }).click();
  await page.getByTestId('customers-open-mode').getByText('为已有账户开通邮箱登录').click();
  await expect(page.getByTestId('customers-open-account-id')).toBeVisible();
  await page.getByTestId('customers-open-email').fill(`bind-${Date.now()}@example.com`);

  // 填了标识但没预览就提交：被拦下，不产生任何写入。
  const strangerId = crypto.randomUUID();
  await page.getByTestId('customers-open-account-id').fill(strangerId);
  await page.getByTestId('customers-open-submit').click();
  await expect(page.getByText('请先确认这个账户的预览').first()).toBeVisible();

  // 预览一个不存在的账户：预览自己就报错，提交仍然被拦。
  await page.getByTestId('customers-open-preview').click();
  await expect(page.getByTestId('customers-open-preview-result')).toContainText(strangerId);
  await expect(page.getByTestId('customers-open-preview-result')).not.toContainText(name);
  await page.getByTestId('customers-open-submit').click();
  await expect(page.getByText('请先确认这个账户的预览').first()).toBeVisible();

  // 预览到的是这个账户：显示名称与「未开通」。
  await page.getByTestId('customers-open-account-id').fill(accountId);
  await page.getByTestId('customers-open-preview').click();
  await expect(page.getByTestId('customers-open-preview-result')).toContainText(name);
  await expect(page.getByTestId('customers-open-preview-result')).toContainText('未开通');

  // 换成另一个标识：旧预览**立刻**从页面上消失（不能让上一个对象的信息停在换了目标的表单上）。
  await page.getByTestId('customers-open-account-id').fill(otherId);
  await expect(page.getByTestId('customers-open-preview-result')).not.toContainText(name);
  // 没重新预览就提交：旧预览不作数，提交被拦（**提交旧预览要阻止**）。
  await page.getByTestId('customers-open-submit').click();
  await expect(page.getByText('请先确认这个账户的预览').first()).toBeVisible();

  // 重新预览这个新标识之后才提交成功，并进入客户详情。
  await page.getByTestId('customers-open-preview').click();
  await expect(page.getByTestId('customers-open-preview-result')).toContainText(otherName);
  await page.getByTestId('customers-open-submit').click();
  await expect(page).toHaveURL(/#\/customers\/[0-9a-f-]{36}$/);
  // 绑定不改名：客户详情上看到的仍是那个账户原来的名称。
  await expect(page.getByTestId('customers-detail-account-name')).toHaveText(otherName);
});

test('名称筛选不进地址，返回列表时仍然生效', async ({ page }) => {
  const name = `筛选恢复-${Date.now()}`;
  await signIn(page);

  await page.getByTestId('accounts-create-open').click();
  await page.getByTestId('accounts-create-name').fill(name);
  await page.getByTestId('accounts-create-submit').click();
  await expect(page).toHaveURL(/#\/accounts\/[0-9a-f-]{36}$/);
  const accountId = page.url().split('/').pop() ?? '';

  await page.getByTestId('accounts-back-to-list').click();
  await page.getByTestId('accounts-lookup-name').fill(name);
  await page.getByTestId('accounts-search').click();
  // 名称是**非敏感筛选**，但它不进地址：地址里只有查询串以外的部分。
  expect(page.url()).not.toContain('name=');
  await expect(page.locator('.ant-table-tbody').getByText(name)).toBeVisible();

  // 进详情再返回：筛选条件仍在（会话级恢复），输入框里也是那个条件。
  await page.locator('.ant-table-tbody').getByText(accountId).click();
  await expect(page).toHaveURL(/#\/accounts\/[0-9a-f-]{36}$/);
  await page.getByTestId('accounts-back-to-list').click();
  await expect(page.getByTestId('accounts-lookup-name')).toHaveValue(name);
  await expect(page.locator('.ant-table-tbody').getByText(name)).toBeVisible();
});

test('提交中重复点击只发一次创建请求', async ({ page }) => {
  const name = `只发一次-${Date.now()}`;
  await signIn(page);

  // 把创建请求压慢，给第二次点击留出窗口。
  const creates: string[] = [];
  await page.route('**/api/v1/accounts', async (route) => {
    if (route.request().method() === 'POST') {
      creates.push(route.request().url());
      await new Promise((resolve) => setTimeout(resolve, 800));
    }
    await route.continue();
  });

  await page.getByTestId('accounts-create-open').click();
  await page.getByTestId('accounts-create-name').fill(name);
  const submit = page.getByTestId('accounts-create-submit');
  await submit.click();
  // 第二次点击落在按钮被禁用/加载中的窗口里：它不该再发一次。
  await submit.click({ force: true, timeout: 2_000 }).catch(() => {});

  await expect(page).toHaveURL(/#\/accounts\/[0-9a-f-]{36}$/);
  expect(creates).toHaveLength(1);
});

test('创建请求结果未知时提示先按名称查找，不引导重建', async ({ page }) => {
  const name = `未知结果-${Date.now()}`;
  await signIn(page);

  // 让创建请求直接失败在传输层（网络中断/超时的形态）：这是"结果未知"，不是"创建失败"。
  await page.route('**/api/v1/accounts', (route) =>
    route.request().method() === 'POST' ? route.abort() : route.continue(),
  );
  await page.getByTestId('accounts-create-open').click();
  await page.getByTestId('accounts-create-name').fill(name);
  await page.getByTestId('accounts-create-submit').click();

  await expect(page.getByText('创建结果暂未确认，请先按账户名称查找确认')).toBeVisible();
  // 入口给到位：名称筛选已经按刚填的名称带上，运营可以立刻确认建没建成。
  await expect(page.getByTestId('accounts-lookup-name')).toHaveValue(name);
});

test('已经绑过登录身份的账户：预览显示已绑定，且不能提交', async ({ page }) => {
  const email = `bound-once-${Date.now()}@example.com`;
  const name = `已绑定的账户-${Date.now()}`;
  await signIn(page);

  // 建一个账户，然后**用绑定模式**把身份真的绑上去（新建模式会另建一个账户，测不出这条）。
  await page.getByTestId('accounts-create-open').click();
  await page.getByTestId('accounts-create-name').fill(name);
  await page.getByTestId('accounts-create-submit').click();
  await expect(page).toHaveURL(/#\/accounts\/[0-9a-f-]{36}$/);
  const accountId = page.url().split('/').pop() ?? '';

  await page.locator('.ant-layout-sider').getByRole('menuitem', { name: '客户登录' }).click();
  await page.getByTestId('customers-open-mode').getByText('为已有账户开通邮箱登录').click();
  await page.getByTestId('customers-open-email').fill(email);
  await page.getByTestId('customers-open-account-id').fill(accountId);
  await page.getByTestId('customers-open-preview').click();
  await expect(page.getByTestId('customers-open-preview-result')).toContainText(name);
  await page.getByTestId('customers-open-submit').click();
  await expect(page).toHaveURL(/#\/customers\/[0-9a-f-]{36}$/);

  // 再拿同一个账户走绑定：预览如实说它已经绑过，提交被拦。
  await page.getByTestId('customers-back-to-list').click();
  await page.getByTestId('customers-open-mode').getByText('为已有账户开通邮箱登录').click();
  await page.getByTestId('customers-open-email').fill(`another-${Date.now()}@example.com`);
  await page.getByTestId('customers-open-account-id').fill(accountId);
  await page.getByTestId('customers-open-preview').click();
  await expect(page.getByTestId('customers-open-preview-result')).toContainText(email);
  await page.getByTestId('customers-open-submit').click();
  await expect(page.getByText('这个账户已经有登录身份了').first()).toBeVisible();
  // 还留在列表页：没有产生任何新身份。
  await expect(page).toHaveURL(/#\/customers$/);
});

test('建账户撞名：完全相同才被拒，只差大小写的名称可以并存', async ({ page }) => {
  const taken = `NameTaken${Date.now()}`;
  await signIn(page);

  await page.getByTestId('accounts-create-open').click();
  await page.getByTestId('accounts-create-name').fill(taken);
  await page.getByTestId('accounts-create-submit').click();
  await expect(page).toHaveURL(/#\/accounts\/[0-9a-f-]{36}$/);

  // 再建一个**完全相同**的：地址不进详情，提示“已被占用”，表单还在、输入值保留。
  await page.getByTestId('accounts-back-to-list').click();
  await page.getByTestId('accounts-create-open').click();
  await page.getByTestId('accounts-create-name').fill(taken);
  await page.getByTestId('accounts-create-submit').click();
  await expect(page.getByTestId('accounts-create-error')).toHaveText('这个名称已被占用，请换一个');
  await expect(page).toHaveURL(/#\/accounts$/);
  await expect(page.getByTestId('accounts-create-name')).toHaveValue(taken);

  // **只差大小写是另一个名称**：它能建成，详情显示的就是填进去的那个大小写。
  await page.getByTestId('accounts-create-name').fill(taken.toUpperCase());
  await page.getByTestId('accounts-create-submit').click();
  await expect(page).toHaveURL(/#\/accounts\/[0-9a-f-]{36}$/);
  await expect(page.getByTestId('accounts-detail-name')).toHaveText(taken.toUpperCase());

  // 换成另一个名称同样能建成：冲突不把人卡死。
  await page.getByTestId('accounts-back-to-list').click();
  await page.getByTestId('accounts-create-open').click();
  await page.getByTestId('accounts-create-name').fill(`${taken}-另一个`);
  await page.getByTestId('accounts-create-submit').click();
  await expect(page).toHaveURL(/#\/accounts\/[0-9a-f-]{36}$/);
});

test('改名撞名：完全相同被拒并保留原值；只差大小写是另一个名称', async ({ page }) => {
  const taken = `Rename${Date.now()}`;
  const mine = `${taken}Mine`;
  await signIn(page);

  await page.getByTestId('accounts-create-open').click();
  await page.getByTestId('accounts-create-name').fill(taken);
  await page.getByTestId('accounts-create-submit').click();
  await expect(page).toHaveURL(/#\/accounts\/[0-9a-f-]{36}$/);

  await page.getByTestId('accounts-back-to-list').click();
  await page.getByTestId('accounts-create-open').click();
  await page.getByTestId('accounts-create-name').fill(mine);
  await page.getByTestId('accounts-create-submit').click();
  await expect(page).toHaveURL(/#\/accounts\/[0-9a-f-]{36}$/);
  await expect(page.getByTestId('accounts-detail-name')).toHaveText(mine);

  // 改成第一个账户的**完全相同**名称：冲突提示出现，详情上的名称仍是自己的。
  await page.getByTestId('accounts-name-input').fill(taken);
  await page.getByTestId('accounts-name-submit').click();
  await expect(page.getByTestId('accounts-name-error')).toHaveText('这个名称已被占用，请换一个');
  await expect(page.getByTestId('accounts-detail-name')).toHaveText(mine);

  // 只差大小写是另一个名称：改名成功，详情显示新值。
  await page.getByTestId('accounts-name-input').fill(taken.toUpperCase());
  await page.getByTestId('accounts-name-submit').click();
  await expect(page.getByTestId('accounts-detail-name')).toHaveText(taken.toUpperCase());
});

test('客户登录列表显示关联账户名称', async ({ page }) => {
  const email = `named-list-${Date.now()}@example.com`;
  const name = `列表可见的名称-${Date.now()}`;
  await signIn(page);

  await page.locator('.ant-layout-sider').getByRole('menuitem', { name: '客户登录' }).click();
  await page.getByTestId('customers-open-email').fill(email);
  await page.getByTestId('customers-open-name').fill(name);
  await page.getByTestId('customers-open-password').fill('a-long-enough-password');
  await page.getByTestId('customers-open-submit').click();
  await expect(page).toHaveURL(/#\/customers\/[0-9a-f-]{36}$/);

  await page.getByTestId('customers-back-to-list').click();
  await page.getByTestId('customers-search-email').fill(email);
  await page.getByTestId('customers-search-submit').click();
  const rows = page.locator('.ant-table-tbody').getByRole('row');
  await expect(rows).toHaveCount(1);
  await expect(rows).toContainText(name);
});
