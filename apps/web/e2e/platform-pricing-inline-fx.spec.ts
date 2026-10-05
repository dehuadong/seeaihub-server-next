import { expect, test } from '@playwright/test';
import { adminApiUrl, consoleUrl, settings } from './settings';

/// **缺折算率就在定价处当场录**（Spec M2/V-D12、`#38` 的 P2）。
///
/// 折算率是「这个币种 → CNY」的全局事实，缺它就算不出人民币对客价。界面不让运营跳去别的页面，
/// 而是就地给一个输入 + 记录按钮；录完立刻出人民币结果。
///
/// 夹具：e2e 是空库，用内联形状发一条 USD 的供给（种子发布本身要先有折算率，所以先用 API 录一行，
/// 再把**页面**看到的折算率表拦成空的——验的就是「页面看到缺」时的那条路）。
test('缺折算率时在定价处当场录，录完立刻出人民币价', async ({ page, request }) => {
  const suffix = Date.now();
  const vendorModel = 'e2e-inline-fx-supply-' + suffix;

  const fx = await request.put(adminApiUrl + '/api/v1/fx-rates', {
    headers: { Authorization: 'Bearer ' + settings.adminToken },
    data: { currency: 'USD', rate_micros: 7_100_000 },
  });
  expect(fx.ok(), '种子折算率录入应当成功：' + (await fx.text())).toBeTruthy();

  const capability = {
    type: 'object',
    additionalProperties: false,
    required: ['model', 'prompt'],
    properties: { model: { const: vendorModel }, prompt: { type: 'string', minLength: 1 } },
  };
  const seeded = await request.post(adminApiUrl + '/api/v1/runtime-revisions', {
    headers: { Authorization: 'Bearer ' + settings.adminToken },
    data: {
      vendor_id: 'OpenAI',
      native_model_id: vendorModel,
      gateway_model: 'e2e-inline-fx-seed-' + suffix,
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
          provider_kind: 'E2EInlineFx',
          adapter_key: 'aihubmix-image-v1',
          provider_model_id: 'e2e-inline-fx-upstream',
          base_url: 'https://e2e-inline-fx.example.com',
          credential_env: 'E2E_INLINE_FX_KEY',
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
            source_url: 'https://e2e-price.example.com',
          },
          cost_currency: 'USD',
        },
      ],
    },
  });
  expect(seeded.ok(), '夹具发布应当成功：' + (await seeded.text())).toBeTruthy();

  let recorded: { currency: string; rate_micros: number; effective_at: string } | null = null;
  await page.route('**/api/v1/fx-rates', async (route) => {
    if (route.request().method() === 'PUT') {
      recorded = { currency: 'USD', rate_micros: 7_200_000, effective_at: new Date().toISOString() };
      await route.continue();
      return;
    }
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ rates: recorded ? [recorded] : [] }),
    });
  });

  await page.goto(consoleUrl);
  await page.getByTestId('admin-email').fill(settings.adminEmail);
  await page.getByTestId('admin-password').fill(settings.adminPassword);
  await page.getByTestId('admin-sign-in').click();
  await page.locator('.ant-layout-sider').getByRole('menuitem', { name: '模型目录' }).click();
  await page.getByRole('button', { name: /发布新模型|上架新模型/ }).click();
  await page.getByTestId('platform-vendor').click();
  await page.getByTitle('OpenAI').click();
  await page.getByTestId('platform-pick-E2EInlineFx-e2e-inline-fx-upstream').check();

  await expect(page.getByText('还没有 USD → CNY 的折算率')).toBeVisible();
  await expect(page.getByTestId('platform-amount-0')).toHaveValue('5');
  await expect(page.getByText('还缺折算率，人民币对客价算不出来。')).toBeVisible();

  await page.getByTestId('platform-fx-rate').fill('7.2');
  await page.getByTestId('platform-fx-record').click();

  await expect(page.getByText(/折算率 USD → CNY：7\.2/)).toBeVisible();
  await expect(page.getByText(/文入 ¥43\.20/)).toBeVisible();
});
