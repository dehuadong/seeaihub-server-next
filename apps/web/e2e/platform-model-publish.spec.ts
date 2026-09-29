import { expect, test } from '@playwright/test';
import { adminApiUrl, consoleUrl, settings } from './settings';

/// **运营发布平台模型：选厂商 → 勾供给 → 给价，不碰技术字段**（Spec V-D8、`#33` 的 P2/P4/P5）。
///
/// 这条要证明的是**界面事实**：运营那条路上，渠道地址、凭证变量名、驱动器、承载面、参数映射一次都不出现，
/// 而他能从一个厂商的供给里勾几条、给价、发布成功。接口层的能力由 `cases_publication` 覆盖（可选清单不含
/// 渠道部署事实、引用不存在的被拒、引用停用的被拒、条目快照冻结）；这里验的是运营实际看到与操作的路径。
///
/// **夹具为什么先用"内联发布"造一条供给**：e2e 环境是空库、也没有素材导入，所以一开始可选清单是空的——
/// 而供给本该由工程师随素材配好。这条 spec 用**旧的内联形状**（`offerings`）发一次，把供给造出来，
/// 顺带证明那条过渡路径**仍然可用**（`0012` §7：老形状在过渡期继续接受，但它不再是运营的路径）。
test('选厂商、勾供给、给价，发布一个平台模型', async ({ request, page }) => {
  const suffix = Date.now();
  const vendorModel = `e2e-supply-${suffix}`;
  const platformName = `e2e-platform-${suffix}`;

  // 折算率：渠道币种没录过的话，**发布期**就会拒（成本按渠道币种记原值，折算率是把它折成 CNY 的依据）。
  // 所以这一步要在夹具发布**之前**——放到后面会让夹具自己先失败，看起来像夹具坏了。
  const fx = await request.put(`${adminApiUrl}/api/v1/fx-rates`, {
    headers: { Authorization: `Bearer ${settings.adminToken}` },
    data: { currency: 'USD', rate_micros: 7_200_000 },
  });
  expect(fx.ok(), `折算率录入应当成功：${await fx.text()}`).toBeTruthy();

  // ── 夹具：用内联形状发布一次，造出一条带渠道与费率的供给 ──
  const seeded = await request.post(`${adminApiUrl}/api/v1/runtime-revisions`, {
    headers: { Authorization: `Bearer ${settings.adminToken}` },
    data: {
      vendor_id: 'OpenAI',
      native_model_id: vendorModel,
      gateway_model: `e2e-seed-model-${suffix}`,
      native_revision: 'e2e-1',
      actor: 'e2e',
      markup_bps: 2000,
      capability_schema: {
        type: 'object',
        additionalProperties: false,
        required: ['model', 'prompt'],
        properties: {
          model: { const: vendorModel },
          prompt: { type: 'string', minLength: 1 },
        },
      },
      offerings: [
        {
          provider_kind: 'E2EChannel',
          adapter_key: 'aihubmix-image-v1',
          provider_model_id: 'e2e-upstream-model',
          base_url: 'https://e2e-do-not-show.example.com',
          credential_env: 'E2E_DO_NOT_SHOW_KEY',
          restrictions: { allowed_branches: ['prompt_only'], max_reference_images: 0 },
          carrier_schema: {
            type: 'object',
            additionalProperties: false,
            required: ['model', 'prompt'],
            properties: {
              model: { const: vendorModel },
              prompt: { type: 'string', minLength: 1 },
            },
          },
          parameter_mapping: {},
          formula: 'token_rates',
          // 给了 `markup_bps` 就必须有一条候选**由它推导价**或**自带对客费率**——否则倍率是一次没人读的
          // 记录。这里给一份对客四档费率，夹具就完整了（并顺带证明"倍率是平台模型级一个"这件事：
          // 这一份费率是**按候选**给的，而倍率在顶层只出现一次）。
          consumer_rates_cny: {
            text_input_micros_per_million: 9_000_000,
            image_input_micros_per_million: 14_000_000,
            text_output_micros_per_million: 18_000_000,
            image_output_micros_per_million: 54_000_000,
          },
          price_plan: {
            currency: 'USD',
            text_input_microusd_per_million: 5_000_000,
            image_input_microusd_per_million: 8_000_000,
            text_output_microusd_per_million: 10_000_000,
            image_output_microusd_per_million: 30_000_000,
            source_url: 'https://e2e-price.example.com',
          },
          cost_currency: 'USD',
        },
      ],
    },
  });
  expect(seeded.ok(), `夹具发布应当成功：${await seeded.text()}`).toBeTruthy();

  // ── 运营的路：登录 → 发布平台模型 ──
  await page.goto(consoleUrl);
  await page.getByTestId('admin-email').fill(settings.adminEmail);
  await page.getByTestId('admin-password').fill(settings.adminPassword);
  await page.getByTestId('admin-sign-in').click();

  // **先看折算率页**，记下这一行——发布页显示的那个数要与它一致。两处不一致会让运营按一个不存在的
  // 汇率推价，而推出来的价会被发布期收下，所以这是断言，不是"好不好看"。
  await page.locator('.ant-layout-sider').getByRole('menuitem', { name: '折算率' }).click();
  const fxRow = page.locator('.ant-table-tbody').getByRole('row').filter({ hasText: 'USD' });
  await expect(fxRow).toContainText('7.2');

  await page.locator('.ant-layout-sider').getByRole('menuitem', { name: '模型目录' }).click();
  await page.getByRole('button', { name: /发布新模型|上架新模型/ }).click();

  await page.getByTestId('platform-name').fill(platformName);
  await page.getByTestId('platform-vendor').click();
  await page.getByTitle('OpenAI').click();

  // **P5：运营那条路上一次都不该出现渠道部署事实与驱动器**。断言的是**整页正文**，不是某个容器——
  // 只看一个容器会漏掉"从别处漏出来"。
  const body = await page.locator('body').innerText();
  for (const forbidden of [
    'e2e-do-not-show.example.com',
    'E2E_DO_NOT_SHOW_KEY',
    'base_url',
    'credential_env',
    'adapter_key',
    'carrier_schema',
    'parameter_mapping',
  ]) {
    expect(body, `运营界面不该出现 \`${forbidden}\``).not.toContain(forbidden);
  }

  // 勾一条该厂商下的供给（夹具那条），给它四档对客价。
  await page.getByTestId('platform-pick-E2EChannel-e2e-upstream-model').check();

  // 定价处显示的折算率与刚在折算率页看到的是同一个数。
  await expect(page.getByText(/折算率 USD → CNY：7\.2/)).toBeVisible();

  // 四档金额按**渠道原币种**（USD）填，默认取该渠道的费率表；这里改成 9/14/18/54。
  for (const [index, value] of [9, 14, 18, 54].entries()) {
    await page.getByTestId(`platform-amount-${index}`).fill(String(value));
  }

  await page.getByTestId('platform-publish').click();

  // 发布成功的答复里带着平台模型名，且**模型目录**里查得到它（对客目录按这个名字出牌）。
  await expect(page.getByText(platformName).first()).toBeVisible();
  const published = await request.get(`${adminApiUrl}/api/v1/gateway-models`, {
    headers: { Authorization: `Bearer ${settings.adminToken}` },
  });
  const catalog = (await published.json()) as {
    gateway_models: {
      gateway_model: string;
      vendor_id: string;
      candidates: { consumer_rates_cny: { text_input_micros_per_million: number } | null }[];
    }[];
  };
  const model = catalog.gateway_models.find((item) => item.gateway_model === platformName);
  expect(model, '发布出来的平台模型要能在目录里查到').toBeTruthy();
  expect(model?.vendor_id).toBe('OpenAI');
  // **P4：这一条候选带的是这次给的那份对客费率**（倍率是平台模型级一个，另外断言）。
  expect(model?.candidates[0]?.consumer_rates_cny?.text_input_micros_per_million).toBe(77_760_000);
});
