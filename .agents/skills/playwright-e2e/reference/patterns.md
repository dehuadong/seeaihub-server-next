# spec 写法参考

目标只有一个：**同一条命令每次跑都走到同一个结果**。不确定的地方用断言等它确定，不用 sleep 赌它。

## 选择器

优先级从高到低：

1. `getByRole('button', { name: '提交订单' })` —— 顺带验证了可访问性
2. `getByLabel('邮箱')` —— 表单字段
3. `getByTestId('checkout-submit')` —— 上面两条都不稳时的稳定锚点，组件上补 `data-testid="checkout-submit"`
4. `getByText('...', { exact: true })` —— 文案断言；文案变了测试就该跟着改

`locator('div.card > button:nth-child(2)')` 这类只在没有别的办法时用，它会在下次改样式时红。

## 等待与断言

用 web-first 断言，Playwright 自己重试到超时：

```ts
await expect(page.getByRole('heading', { name: '工作台' })).toBeVisible()
await expect(page.getByTestId('total')).toHaveText('¥128.00')
await expect(page.getByRole('button', { name: '保存' })).toBeEnabled()
await expect(page).toHaveURL(/\/dashboard/)
```

`await page.waitForTimeout(2000)` 不出现在提交的代码里；替代品是断言、`expect.poll`、或 `await page.waitForResponse(...)`：

```ts
await expect.poll(async () => {
  const res = await page.request.get('/api/jobs/1')
  return (await res.json()).status
}, { timeout: 15_000 }).toBe('done')
```

## 网络

把不确定的外部依赖变成确定的：

```ts
await page.route('**/api/recommend', (route) =>
  route.fulfill({ status: 200, json: { items: [] } }))
```

- 只桩真正的外部、慢、不稳定的接口；自己的后端尽量跑真的，桩太多就测不到集成问题。
- 要断言请求确实发出去了：`const [req] = await Promise.all([page.waitForRequest('**/api/orders'), page.getByRole('button', { name: '提交' }).click()])`。
- 整段网络录制回放：`await context.routeFromHAR('fixtures/api.har', { update: !process.env.CI })`。

## 拆步骤

一个巨大的 test 红一次要看半小时；拆成并列的小 test，`test.step` 只用来在同一个用例里划分可读阶段：

```ts
test('结算流程', async ({ page }) => {
  await test.step('加入购物车', async () => { /* ... */ })
  await test.step('提交订单', async () => { /* ... */ })
})
```

每个 test 自建自己需要的数据，互不依赖执行顺序。

## 红的时候先看什么

| 症状 | 先查 |
| --- | --- |
| 元素找不到 | `--headed` 跑一次看是否根本没渲染；是不是 strict mode 命中多个选择器（用 `getByRole(..., { name })` 收窄） |
| 一会儿红一会儿绿 | 有没有裸 `waitForTimeout`；有没有未桩的网络；几个 test 是否抢同一份数据 |
| 卡在登录 | `storageState` 文件是否存在、setup project 是否真的跑了 |
| 本地绿 CI 红 | 时区 / 语言 / 字体、`CI=1` 下的 retries、`--with-deps` 装的浏览器是否对得上 |
| 失败时信息太少 | 临时 `trace: 'on'` 或 `--trace on`，再 `npx playwright show-trace test-results/**/trace.zip` |

## 调试开关

```bash
npx playwright test tests/x.spec.ts --headed --debug   # 单步
npx playwright test tests/x.spec.ts --trace on         # 每次都留 trace
npx playwright show-report                             # HTML 报告
npx playwright test --ui                               # 本地交互式跑
```

浏览器工具 / MCP 到这里为止：用来看 trace 和报告；驱动页面仍然交给 `npx playwright test`。
