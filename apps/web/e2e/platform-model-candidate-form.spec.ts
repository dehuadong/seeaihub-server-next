import { expect, test } from '@playwright/test';
import { adminApiUrl, consoleUrl, settings } from './settings';

/// **展开候选能看到"对客计价形态"**（Spec `0001` §5.2 M1）。
///
/// 一个平台模型两条候选：一条对客按 token 四档（带四档 CNY 费率），一条对客按上游声明金额 × 倍率
/// （按定义没有向量）。判据是这两条在目录里被展开时各显示自己的形态，而不显示别人的费率。
test('展开候选显示对客计价形态：按 token 四档与上游声明金额各归各', async ({ page, request }) => {
  const suffix = Date.now();
  const vendorModel = 'e2e-candidate-form-supply-' + suffix;
  const platformName = 'e2e-candidate-form-' + suffix;

  const fx = await request.put(adminApiUrl + '/api/v1/fx-rates', {
    headers: { Authorization: 'Bearer ' + settings.adminToken },
    data: { currency: 'USD', rate_micros: 7_200_000 },
  });
  expect(fx.ok(), '折算率录入应当成功：' + (await fx.text())).toBeTruthy();

  const capability = {
    type: 'object',
    additionalProperties: false,
    required: ['model', 'prompt'],
    properties: {
      model: { const: vendorModel },
      prompt: { type: 'string', minLength: 1 },
    },
  };
  const seeded = await request.post(adminApiUrl + '/api/v1/runtime-revisions', {
    headers: { Authorization: 'Bearer ' + settings.adminToken },
    data: {
      vendor_id: 'OpenAI',
      native_model_id: vendorModel,
      gateway_model: platformName,
      native_revision: 'e2e-candidate-form-1',
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
          provider_kind: 'E2EToken',
          adapter_key: 'aihubmix-image-v1',
          provider_model_id: 'e2e-candidate-token',
          base_url: 'https://e2e-token.example.com',
          credential_env: 'E2E_TOKEN_KEY',
          restrictions: { allowed_branches: ['prompt_only'], max_reference_images: 0 },
          carrier_schema: capability,
          parameter_mapping: {},
          formula: 'token_rates',
          consumer_formula: 'token_rates',
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
        {
          provider_kind: 'APIMart',
          adapter_key: 'apimart-image-v1',
          provider_model_id: 'e2e-candidate-declared',
          base_url: 'https://e2e-declared.example.com',
          credential_env: 'E2E_DECLARED_KEY',
          restrictions: { allowed_branches: ['prompt_only'], max_reference_images: 0 },
          carrier_schema: capability,
          parameter_mapping: {},
          formula: 'upstream_declared',
          consumer_formula: 'upstream_declared',
          cost_currency: 'USD',
        },
      ],
    },
  });
  expect(seeded.ok(), '夹具发布应当成功：' + (await seeded.text())).toBeTruthy();

  await page.goto(consoleUrl);
  await page.getByTestId('admin-email').fill(settings.adminEmail);
  await page.getByTestId('admin-password').fill(settings.adminPassword);
  await page.getByTestId('admin-sign-in').click();
  await page.locator('.ant-layout-sider').getByRole('menuitem', { name: '模型目录' }).click();

  const panel = page.locator('.ant-card').filter({ hasText: platformName }).first();
  await panel.getByRole('button', { name: /候选（/ }).click();

  // 两条候选各自显示自己的对客形态；按上游金额那条没有向量，如实说明。
  await expect(panel.getByText('按 token 四档', { exact: true })).toBeVisible();
  await expect(panel.getByText('上游声明金额 × 倍率', { exact: true })).toBeVisible();
  await expect(panel.getByText('无（按上游声明金额 × 倍率）')).toBeVisible();
});
