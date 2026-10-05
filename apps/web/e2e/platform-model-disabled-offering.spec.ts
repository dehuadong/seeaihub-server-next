import { expect, test } from '@playwright/test';
import { adminApiUrl, consoleUrl, settings } from './settings';

/// **停用的供给列出来并标明"不能选"，而不是消失**（Spec V-D8 与 `#33` 的 P1 后半）。
///
/// 为什么这条要真浏览器：判据是**界面上看得见"为什么它选不了"**。把它从清单里删掉实现起来更省事，
/// 但那样运营看到的是"这条供给不存在"，于是他不会去问"谁把它停了"——他会去重新配一条一模一样的。
///
/// 为什么需要夹具：e2e 环境是空库、没有素材导入，所以先用内联形状发布一次把供给造出来，再把它停掉。
test('停用的供给在清单里标明不能选，而不是消失', async ({ request, page }) => {
  const suffix = Date.now();
  const vendorModel = `e2e-disabled-${suffix}`;
  const platformName = `e2e-disabled-platform-${suffix}`;
  const channel = `E2EDisabledChannel${suffix}`;

  const fx = await request.put(`${adminApiUrl}/api/v1/fx-rates`, {
    headers: { Authorization: `Bearer ${settings.adminToken}` },
    data: { currency: 'USD', rate_micros: 7_200_000 },
  });
  expect(fx.ok(), `折算率录入应当成功：${await fx.text()}`).toBeTruthy();

  const capability = {
    type: 'object',
    additionalProperties: false,
    required: ['model', 'prompt'],
    properties: {
      model: { const: vendorModel },
      prompt: { type: 'string', minLength: 1 },
    },
  };
  const seeded = await request.post(`${adminApiUrl}/api/v1/runtime-revisions`, {
    headers: { Authorization: `Bearer ${settings.adminToken}` },
    data: {
      vendor_id: 'OpenAI',
      native_model_id: vendorModel,
      gateway_model: `e2e-disabled-seed-${suffix}`,
      native_revision: 'e2e-1',
      type: 'image',
      actor: 'e2e',
      markup_bps: 2000,
      capability_schema: capability,
      documentation: {
        narrative: '# {{platform_name}}\n\n{{parameter_table}}\n',
        fields: {
          '/properties/model': '目录返回的模型名。',
          '/properties/prompt': '提示词。',
        },
      },
      offerings: [
        {
          provider_kind: channel,
          adapter_key: 'aihubmix-image-v1',
          provider_model_id: 'e2e-disabled-upstream',
          base_url: 'https://e2e-disabled.example.com',
          credential_env: 'E2E_DISABLED_KEY',
          restrictions: { allowed_branches: ['prompt_only'], max_reference_images: 0 },
          carrier_schema: capability,
          parameter_mapping: {},
          formula: 'token_rates',
          price_plan: {
            currency: 'USD',
            text_input_microusd_per_million: 5_000_000,
            image_input_microusd_per_million: 8_000_000,
            text_output_microusd_per_million: 10_000_000,
            image_output_microusd_per_million: 30_000_000,
            source_url: 'https://e2e-disabled-price.example.com',
          },
          consumer_rates_cny: {
            text_input_micros_per_million: 9_000_000,
            image_input_micros_per_million: 14_000_000,
            text_output_micros_per_million: 18_000_000,
            image_output_micros_per_million: 54_000_000,
          },
          cost_currency: 'USD',
        },
      ],
    },
  });
  expect(seeded.ok(), `夹具发布应当成功：${await seeded.text()}`).toBeTruthy();

  // 把这条供给停掉（运营在供给清单上的动作）。
  const listed = await request.get(`${adminApiUrl}/api/v1/offerings`, {
    headers: { Authorization: `Bearer ${settings.adminToken}` },
  });
  const catalog = (await listed.json()) as {
    offerings: { offering_id: string; native_model_id: string }[];
  };
  const target = catalog.offerings.find((item) => item.native_model_id === vendorModel);
  expect(target, '夹具那条供给要能在可选清单里找到').toBeTruthy();
  const disabled = await request.patch(
    `${adminApiUrl}/api/v1/offerings/${target?.offering_id}`,
    {
      headers: { Authorization: `Bearer ${settings.adminToken}` },
      data: { enabled: false },
    },
  );
  expect(disabled.ok(), `停用应当成功：${await disabled.text()}`).toBeTruthy();

  // ── 运营的路 ──
  await page.goto(consoleUrl);
  await page.getByTestId('admin-email').fill(settings.adminEmail);
  await page.getByTestId('admin-password').fill(settings.adminPassword);
  await page.getByTestId('admin-sign-in').click();
  await page.locator('.ant-layout-sider').getByRole('menuitem', { name: '模型目录' }).click();
  await page.getByRole('button', { name: /发布新模型|上架新模型/ }).click();
  await page.getByTestId('platform-name').fill(platformName);
  await page.getByTestId('platform-vendor').click();
  await page.getByTitle('OpenAI').click();

  // 它**还在清单里**，且标明不能选。
  const pick = page.getByTestId(`platform-pick-${channel}-e2e-disabled-upstream`);
  await expect(pick, '停用的供给要列出来，不能悄悄消失').toBeVisible();
  await expect(pick).toBeDisabled();
  await expect(page.getByText('已停用，不能选')).toBeVisible();
});
