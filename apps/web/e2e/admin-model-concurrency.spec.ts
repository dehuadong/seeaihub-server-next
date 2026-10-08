import { expect, test } from '@playwright/test';
import { adminApiUrl, consoleUrl, settings } from './settings';

/// **并发名额由运营在模型上设，留空＝用部署缺省**（Spec V-D17，工作项 #94）。
///
/// 夹具发一个平台模型，再从「模型目录」那张卡上设名额、清名额：
/// 设 2 之后重新载入页面看到 2；清空之后看到的是"用部署缺省（N）"而不是一个空白框——
/// 运营要能直接看出"没设的时候实际是多少"。
test('并发名额：能设、能清，未设时显示部署缺省', async ({ page, request }) => {
  const suffix = Date.now();
  const vendorModel = 'e2e-quota-supply-' + suffix;
  const model = 'e2e-quota-' + suffix;

  const rate = await request.put(adminApiUrl + '/api/v1/fx-rates', {
    headers: { Authorization: 'Bearer ' + settings.adminToken },
    data: { currency: 'USD', rate_micros: 7_100_000 },
  });
  expect(rate.status()).toBe(204);

  const seeded = await request.post(adminApiUrl + '/api/v1/runtime-revisions', {
    headers: { Authorization: 'Bearer ' + settings.adminToken },
    data: {
      vendor_id: 'OpenAI',
      native_model_id: vendorModel,
      gateway_model: model,
      native_revision: 'e2e-quota-1.0',
      type: 'image',
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
      documentation: {
        narrative: '# {{platform_name}}\n\n{{parameter_table}}\n',
        fields: {
          '/properties/model': '目录返回的模型名。',
          '/properties/prompt': '提示词。',
        },
      },
      offerings: [
        {
          provider_kind: 'E2EChannel',
          adapter_key: 'apimart-image-v1',
          provider_model_id: 'e2e-quota-upstream',
          base_url: 'http://127.0.0.1:9/stub',
          credential_env: 'E2E_DUMMY_KEY',
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
          consumer_rates_cny: {
            text_input_micros_per_million: 42_600_000,
            image_input_micros_per_million: 68_160_000,
            text_output_micros_per_million: 85_200_000,
            image_output_micros_per_million: 255_600_000,
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
  expect(seeded.ok(), '夹具发布应当成功：' + (await seeded.text())).toBeTruthy();

  await page.goto(consoleUrl);
  await page.getByTestId('admin-email').fill(settings.adminEmail);
  await page.getByTestId('admin-password').fill(settings.adminPassword);
  await page.getByTestId('admin-sign-in').click();

  const input = page.getByTestId('models-quota-input-' + model);
  await expect(input).toBeVisible({ timeout: 15_000 });
  const card = page.locator('.ant-card', { has: input });
  // 没设过名额：显示部署缺省，而不是空白。
  await expect(card).toContainText('用部署缺省（1）');

  // 设成 2：保存后重新载入页面仍能看到 2。
  await input.fill('2');
  await page.getByTestId('models-quota-submit-' + model).click();
  await expect(card).toContainText('2（每账户）', { timeout: 15_000 });
  await page.reload();
  await expect(page.getByTestId('models-quota-input-' + model)).toBeVisible({ timeout: 15_000 });
  await expect(page.locator('.ant-card', { has: page.getByTestId('models-quota-input-' + model) }))
    .toContainText('2（每账户）');

  // 非法值在页面上被点名拒掉：`0` 不会被悄悄压成 1，也不会发出去。
  await page.getByTestId('models-quota-input-' + model).fill('0');
  await page.getByTestId('models-quota-submit-' + model).click();
  await expect(page.getByText('并发名额至少 1')).toBeVisible({ timeout: 15_000 });
  await page.reload();
  await expect(page.locator('.ant-card', { has: page.getByTestId('models-quota-input-' + model) }))
    .toContainText('2（每账户）');

  // 清空＝回到部署缺省：页面显示缺省值与它的实际数字。
  await page.getByTestId('models-quota-input-' + model).fill('');
  await page.getByTestId('models-quota-submit-' + model).click();
  await expect(page.locator('.ant-card', { has: page.getByTestId('models-quota-input-' + model) }))
    .toContainText('用部署缺省（1）', { timeout: 15_000 });
});
