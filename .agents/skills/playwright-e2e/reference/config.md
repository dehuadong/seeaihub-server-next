# Playwright 配置参考

按项目形状读对应分支。骨架在 [SKILL.md](../SKILL.md) 第 2 步。

## 先判断服务怎么起

| 项目形状 | config 怎么写 |
| --- | --- |
| 一条命令能起服务（`pnpm dev` / `npm run dev`） | `webServer.command` 交给 Playwright 自己拉起来 |
| 开发机上服务已经常驻 | `reuseExistingServer: true`，`url` 指过去；CI 上仍写成能自己起 |
| 服务由 docker compose 起 | `command: 'docker compose up web'`，`timeout: 120_000`，`url` 用容器映射出的宿主端口 |
| 前后端两个服务 | `webServer` 写数组，每个服务一项 |
| 只测构建产物 | `command: 'pnpm build && pnpm preview'` |

```ts
webServer: {
  command: 'pnpm dev',
  cwd: '.',                       // monorepo 里指到子包
  url: 'http://localhost:3000',   // 比 port 准：等到真的能响应
  timeout: 120_000,               // 冷启动慢的框架给足
  reuseExistingServer: !process.env.CI,
  stdout: 'pipe',                 // 服务起不来时能看到它的日志
  env: { NODE_ENV: 'test' },
}
```

端口从项目里读（`package.json`、`.env`、框架默认值），不要照抄示例的 3000。

## 完整 config 模板

```ts
import { defineConfig, devices } from '@playwright/test'

export default defineConfig({
  testDir: './tests',
  outputDir: './test-results',        // trace / 截图 / 视频落盘处，加进 .gitignore
  fullyParallel: true,
  forbidOnly: !!process.env.CI,       // CI 上留着 test.only 直接失败
  retries: process.env.CI ? 2 : 0,
  workers: process.env.CI ? 1 : undefined,   // CI 官方建议串行换稳定；要并行就上分片
  reporter: process.env.CI
    ? [['blob'], ['github']]          // blob 供分片后 merge-reports
    : [['list'], ['html', { open: 'never' }]],

  timeout: 30_000,
  expect: { timeout: 5_000 },         // 断言级超时，比到处 sleep 靠谱
  globalTimeout: process.env.CI ? 60 * 60 * 1000 : undefined,
                                      // CI 上必须有上限：否则挂死的 run 被 runner 杀掉，连报告都留不下

  use: {
    baseURL: process.env.E2E_BASE_URL ?? 'http://localhost:3000',
    trace: 'on-first-retry',
    screenshot: 'only-on-failure',
    video: 'retain-on-failure',
    locale: 'zh-CN',
    timezoneId: 'Asia/Shanghai',
  },

  projects: [
    { name: 'setup', testMatch: /auth\.setup\.ts/ },
    {
      name: 'chromium',
      use: { ...devices['Desktop Chrome'], storageState: 'playwright/.auth/user.json' },
      dependencies: ['setup'],
    },
    // 需要移动端 / 多浏览器时再加：
    // { name: 'mobile', use: { ...devices['Pixel 7'] } },
  ],

  webServer: {
    command: 'pnpm dev',
    url: 'http://localhost:3000',
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
  },
})
```

用不到的项删掉——一份没人读得完的 config 会在第一次报错时耽误时间。Playwright 不自动读 `.env`，需要就 `pnpm add -D dotenv` 并在 config 顶部 `import 'dotenv/config'`。

## 跨 spec 复用登录态

`tests/auth.setup.ts`：

```ts
import { test as setup, expect } from '@playwright/test'

setup('登录并保存 storageState', async ({ page }) => {
  await page.goto('/login')
  await page.getByLabel('邮箱').fill(process.env.E2E_USER!)
  await page.getByLabel('密码').fill(process.env.E2E_PASSWORD!)
  await page.getByRole('button', { name: '登录' }).click()
  await expect(page).toHaveURL(/\/dashboard/)
  await page.context().storageState({ path: 'playwright/.auth/user.json' })
})
```

`playwright/.auth/` 加进 `.gitignore`；凭据走环境变量，不写进 spec。

## CI

GitHub Actions：

```yaml
jobs:
  e2e:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
      - uses: actions/setup-node@v6
        with: { node-version: 22, cache: pnpm }
      - run: pnpm install --frozen-lockfile
      - run: npx playwright install --with-deps chromium
      - run: pnpm exec playwright test
      - uses: actions/upload-artifact@v5
        if: ${{ !cancelled() }}
        with:
          name: playwright-report
          path: playwright-report/
          retention-days: 7
```

- 分片：`npx playwright test --shard=${{ matrix.shard }}/4`，矩阵开 4 份 job，各上传 `blob-report/`，最后一份 job 里 `npx playwright merge-reports --reporter=html ./all-blob-reports`。
- 本地预演 CI：`CI=1 npx playwright test`（PowerShell：`$env:CI=1; npx playwright test`），重点看 retries 关掉后还绿不绿。
- 只在该跑的时候跑：用 `paths` 过滤，或给 spec 打 tag，PR 上 `--grep @smoke`，合并后再跑全量。
- 不要缓存浏览器二进制：官方明确不推荐——还原缓存的时间和重新下载差不多，而且 Linux 的系统依赖本来就缓存不了。真要缓存，key 里必须带 Playwright 版本号。
- 不要给 workflow 设比 `globalTimeout` 更短的 `timeout-minutes`：要让 Playwright 自己先停，报告才留得下来。
