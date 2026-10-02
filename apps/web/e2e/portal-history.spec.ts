import { expect, test, type Page } from '@playwright/test';
import { accountIdOfEmail, seedLedgerPage } from './account-state';
import { nav, panel, pathnameOf, portalAt, registerCustomer, uniqueEmail } from './portal';

/// 客户历史浏览：**日期区间、类别与翻页**在浏览器里观测得到的那一层
/// （Spec C8–C10、V-C15、V-D13、V-D14；设计 `0014` §1–§3）。
///
/// 翻页本身由接口层用例在真库上验（多于一页的记录、游标不重不漏）；这里验页面：区间进地址、刷新与
/// 前进后退恢复、汇总与流水用同一个区间、两段用量用不同的视图取数、界面标出时区。
///
/// 挑时刻时用 UTC 正午：这样在任何本地时区（±12 小时以内）都落在同一天，断言不靠运行机器的时区。

/// 记下这一页发出的对客请求，供"用了哪个区间/哪个视图"的断言取用。
function recordCustomerCalls(page: Page): string[] {
  const calls: string[] = [];
  page.on('request', (request) => {
    const url = request.url();
    if (url.includes('/v1/customer/')) calls.push(url);
  });
  return calls;
}

function queryOf(url: string): URLSearchParams {
  return new URL(url).searchParams;
}

test('账单页把区间写进地址：汇总与流水按同一区间取数，刷新后仍在', async ({ page }) => {
  await registerCustomer(page, uniqueEmail('portal-history'));
  const calls = recordCustomerCalls(page);
  await nav(page, '账单与资金记录');
  await expect(panel(page, '账单汇总')).toBeVisible();

  // 默认区间**改写当前这条历史**（不留一条多余的返回），并进地址。
  await expect(page).toHaveURL(/\/billing\?since=[^&]+&until=[^&]+/);
  const params = new URL(page.url()).searchParams;
  const since = params.get('since');
  const until = params.get('until');
  expect(since, page.url()).toBeTruthy();
  expect(until, page.url()).toBeTruthy();

  // 区间标签必须标出时区：不写时区，客户看到的日期会与平台的归属差一天。
  await expect(page.getByTestId('portal-billing-range-label')).toContainText('UTC');

  // 汇总与流水**同一条区间**。
  const billing = calls.find((url) => url.includes('/v1/customer/billing'));
  const ledger = calls.find((url) => url.includes('/v1/customer/ledger'));
  expect(billing, calls.join('\n')).toBeTruthy();
  expect(ledger, calls.join('\n')).toBeTruthy();
  for (const call of [billing as string, ledger as string]) {
    expect(queryOf(call).get('since')).toBe(since);
    expect(queryOf(call).get('until')).toBe(until);
  }

  // 刷新：地址里的区间不变，页面照它取数。
  calls.length = 0;
  await page.reload();
  await expect(panel(page, '账单汇总')).toBeVisible();
  await expect(page.getByTestId('portal-billing-range-label')).toContainText('UTC');
  const refreshed = calls.find((url) => url.includes('/v1/customer/billing'));
  expect(refreshed, calls.join('\n')).toBeTruthy();
  expect(queryOf(refreshed as string).get('since')).toBe(since);
});

test('直接打开带区间的账单地址：页面按它取数，返回与前进恢复该区间', async ({ page }) => {
  await registerCustomer(page, uniqueEmail('portal-history-url'));
  const calls = recordCustomerCalls(page);
  const since = '2026-01-15T12:00:00.000Z';
  const until = '2026-02-15T12:00:00.000Z';
  await page.goto(
    portalAt(`/billing?since=${encodeURIComponent(since)}&until=${encodeURIComponent(until)}`),
  );
  await expect(panel(page, '账单汇总')).toBeVisible();
  await expect(page.getByTestId('portal-billing-range-label')).toContainText('2026-01-15');
  await expect(page.getByTestId('portal-billing-range-label')).toContainText('2026-02-15');

  const billing = calls.find((url) => url.includes('/v1/customer/billing'));
  expect(billing, calls.join('\n')).toBeTruthy();
  expect(queryOf(billing as string).get('since')).toBe(since);

  // 走开再后退：地址与页面都回到那个区间（V-D13 的"返回时仍在 URL 中"）。
  await nav(page, 'API Key');
  expect(pathnameOf(page)).toBe('/keys');
  await page.goBack();
  expect(pathnameOf(page)).toBe('/billing');
  await expect(page.getByTestId('portal-billing-range-label')).toContainText('2026-01-15');
  await expect(page).toHaveURL(/since=2026-01-15/);
});

test('调用记录页把处理中与已结束历史分开，两段用不同视图取数', async ({ page }) => {
  await registerCustomer(page, uniqueEmail('portal-history-usage'));
  const calls = recordCustomerCalls(page);
  await nav(page, '调用记录');
  await expect(panel(page, '调用记录')).toBeVisible();
  await expect(page.getByRole('heading', { name: '处理中' })).toBeVisible();
  await expect(page.getByRole('heading', { name: '已结束历史' })).toBeVisible();

  // 区间同样进地址，并标出时区。
  await expect(page).toHaveURL(/\/usage\?since=[^&]+&until=[^&]+/);
  await expect(page.getByTestId('portal-usage-range-label')).toContainText('UTC');

  // 处理中与已结束历史是两次读：前者 `view=active`（"现在还在跑"的清单，按受理时刻读、不带区间也不
  // 带游标），后者 `view=completed` 才吃地址里那条区间。
  const active = calls.find(
    (url) => url.includes('/v1/customer/usage') && url.includes('view=active'),
  );
  const completed = calls.find(
    (url) => url.includes('/v1/customer/usage') && url.includes('view=completed'),
  );
  expect(active, calls.join('\n')).toBeTruthy();
  expect(completed, calls.join('\n')).toBeTruthy();
  expect(queryOf(active as string).get('since')).toBeNull();
  expect(queryOf(active as string).get('cursor')).toBeNull();
  const address = new URL(page.url()).searchParams;
  expect(queryOf(completed as string).get('since')).toBe(address.get('since'));
  expect(queryOf(completed as string).get('until')).toBe(address.get('until'));

  // 没有下一页时不给"继续查看"入口（游标为空就是真的没有了）。
  await expect(page.getByTestId('portal-usage-more')).toHaveCount(0);
});

test('资金流水的类别筛选只重取流水，汇总不跟着换区间', async ({ page }) => {
  await registerCustomer(page, uniqueEmail('portal-history-kind'));
  await nav(page, '账单与资金记录');
  await expect(panel(page, '账单汇总')).toBeVisible();
  const calls = recordCustomerCalls(page);
  await page.getByTestId('portal-ledger-kind-credit').click();
  await expect
    .poll(() =>
      calls.some((url) => url.includes('/v1/customer/ledger') && url.includes('kind=credit')),
    )
    .toBe(true);
  // 汇总那条读不吃类别：它按整段区间全量算。
  expect(calls.some((url) => url.includes('/v1/customer/billing'))).toBe(false);
});

test('多于一页的资金流水：继续查看取到更早的那一页，汇总不跟着变', async ({ page }) => {
  const email = uniqueEmail('portal-history-ledger-page');
  await registerCustomer(page, email);
  // e2e 不起 Worker，多于一页的已结束历史只能直接摆进库（页大小是 20）。
  await seedLedgerPage(await accountIdOfEmail(email), 21);

  await nav(page, '账单与资金记录');
  await expect(panel(page, '账单汇总')).toBeVisible();
  // 等汇总真的读出来再记下读数：加载中只有标题，没有数字。
  await expect(page.getByTestId('portal-billing-requests')).toHaveText(/请求数\s*\d+/);
  await expect(page.getByTestId('portal-billing-net')).toHaveText(/净/);
  // 用 `textContent` 逐字比较：`toHaveText` 会归一化空白，拿它比 `innerText` 只会比出格式差异。
  const text = async (testId: string) => (await page.getByTestId(testId).textContent()) ?? '';
  const requests = await text('portal-billing-requests');
  const net = await text('portal-billing-net');

  const rows = page.getByTestId('portal-ledger-table').getByRole('row');
  await expect(rows).toHaveCount(21, { timeout: 10_000 }); // 表头 + 20 条
  await expect(page.getByTestId('portal-ledger-more')).toBeVisible();

  // 翻页用的仍然是**同一区间**：只多一个游标。
  const customerCalls: string[] = [];
  page.on('request', (request) => {
    const url = request.url();
    if (url.includes('/v1/customer/')) customerCalls.push(url);
  });
  const billingReads = customerCalls.filter((url) => url.includes('/v1/customer/billing')).length;
  const first = new URL(page.url()).searchParams;
  await page.getByTestId('portal-ledger-more').click();
  await expect(rows).toHaveCount(22, { timeout: 10_000 }); // 表头 + 21 条
  await expect(page.getByTestId('portal-ledger-more')).toHaveCount(0);

  const continued = customerCalls.find((url) => url.includes('cursor='));
  expect(continued, customerCalls.join('\n')).toBeTruthy();
  expect(queryOf(continued as string).get('since')).toBe(first.get('since'));
  expect(queryOf(continued as string).get('until')).toBe(first.get('until'));

  // 汇总按整段区间算：翻页不重取它，读数也不动。
  expect(
    customerCalls.filter((url) => url.includes('/v1/customer/billing')).length,
    customerCalls.join('\n'),
  ).toBe(billingReads);
  expect(await text('portal-billing-requests')).toBe(requests);
  expect(await text('portal-billing-net')).toBe(net);
});
