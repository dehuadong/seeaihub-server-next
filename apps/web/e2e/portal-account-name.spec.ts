import { expect, test } from '@playwright/test';
import { nav, registerCustomer, uniqueEmail } from './portal';

/// 客户在「账户设置」里看与改自己的账户名称（账户名称 Spec `0003` U6、V6、V9）。
///
/// 只有浏览器才观测得到的那一层：注册后账户设置里显示的是服务端生成的名称、能改成自己的名字、刷新后
/// 仍是新值、清空被当场拦下且输入框回到已保存的值。生成规则与两侧读数一致由接口用例在真库上验。

test('注册后账户设置显示生成的名称，改名成功并在刷新后保持', async ({ page }) => {
  const email = uniqueEmail('portal-name');
  await registerCustomer(page, email);
  await nav(page, '账户设置');

  // 自助注册不给名称输入：这里看到的是服务端按登录邮箱生成的名称（`<本地部分>_<id 前 4 位>`）。
  const input = page.getByTestId('portal-account-name');
  await expect(input).toBeVisible();
  await expect(input).toHaveValue(new RegExp(`^${email.split('@')[0]}_[0-9a-f]{4}$`));
  // 文案只说这个字段是干什么的，不解释名称从哪来。
  await expect(page.getByText('账户名称用于识别与账单，可随时修改。')).toBeVisible();

  // 改成自己的名字。
  const name = `我的工作室-${Date.now()}`;
  await input.fill(name);
  await page.getByTestId('portal-account-name-save').click();
  await expect(page.getByText('账户名称已保存')).toBeVisible();
  await expect(input).toHaveValue(name);

  // 客户侧不解释名称从哪来：整页文字里不该出现"自动生成"这类内部机制的说法。
  expect(await page.locator('body').innerText()).not.toContain('自动生成');

  // 刷新后仍是新值：改的是账户资料，不是页面状态。
  await page.reload();
  await nav(page, '账户设置');
  await expect(page.getByTestId('portal-account-name')).toHaveValue(name);

  // 清空被拦下：输入框回到已保存的值，服务端那份也没被改掉。
  await page.getByTestId('portal-account-name').fill('   ');
  await page.getByTestId('portal-account-name-save').click();
  await expect(page.getByText('账户名称不能为空')).toBeVisible();
  await page.reload();
  await nav(page, '账户设置');
  await expect(page.getByTestId('portal-account-name')).toHaveValue(name);
});
