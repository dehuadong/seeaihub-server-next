import { expect, type Locator, type Page } from '@playwright/test';
import { portalUrl } from './settings';

/// 客户控制台浏览器用例的共用装置：注册/登录、页面地址与面板定位。
///
/// 放在这里而不是各 spec 里复制：地址与选择器改一处漏一处是最常见的假绿来源。
export const PORTAL_PASSWORD = 'e2e-customer-password';

/// 客户入口下某条页面地址的完整 URL。
export function portalAt(path: string): string {
  return `${portalUrl.replace(/\/$/, '')}${path}`;
}

/// 当前地址的路径部分。断言地址时用它，避免端口与主机名写进用例。
export function pathnameOf(page: Page): string {
  return new URL(page.url()).pathname;
}

/// 一块面板。界面用的是 Ant Design 的 `Card`，它的标题渲染成 `div`（不是 heading），所以按
/// **卡片容器 + 标题文本**定位，而不是按 `getByRole('heading')`。
export function panel(page: Page, title: string): Locator {
  return page.locator('.ant-card').filter({ has: page.getByText(title, { exact: true }) });
}

/// 点固定导航进某一页。antd 的 `Menu` 把每项渲染成 `role=menuitem`。
export async function nav(page: Page, label: string): Promise<void> {
  await page.getByRole('menuitem', { name: label }).click();
}

/// 每个用例一个客户：库是共享的，写死的邮箱会在第二次运行时撞上"已注册"。
export function uniqueEmail(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 10_000)}@example.com`;
}

/// 注册一个全新客户并停在概览。
export async function registerCustomer(page: Page, email: string): Promise<void> {
  await page.goto(portalAt('/'));
  await expect(page.getByTestId('portal-submit')).toBeVisible();
  await page.getByTestId('portal-mode-register').click();
  await page.getByTestId('portal-email').fill(email);
  await page.getByTestId('portal-password').fill(PORTAL_PASSWORD);
  await page.getByTestId('portal-submit').click();
  await expect(page.getByTestId('portal-balance')).toBeVisible();
}

/// 在已经打开的登录/注册页上登录（不切换模式）。
///
/// 登录成功的标志用**固定导航**而不是概览余额：深链回跳会落在别的页，假设停在概览会误判。
export async function signInCustomer(page: Page, email: string): Promise<void> {
  await page.getByTestId('portal-email').fill(email);
  await page.getByTestId('portal-password').fill(PORTAL_PASSWORD);
  await page.getByTestId('portal-submit').click();
  await expect(page.getByRole('menuitem', { name: '概览' })).toBeVisible();
}
