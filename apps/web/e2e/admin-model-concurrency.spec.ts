import { expect, test } from '@playwright/test';
import { adminApiUrl, consoleUrl, settings } from './settings';

/// **并发名额由运营在模型上设、必填，改价态不会无意改它**（Spec V-D17、V-D18；工作项 #94/#99）。
///
/// 夹具发一个平台模型（发布命令带 2 的名额），再从「模型目录」那张卡上改名额：
/// 设 3 之后重新载入页面看到 3；填 0 或清空被点名拒掉；改价抽屉里能看到该模型当前的值，
/// 且不改它就不发送这个字段。
test('并发名额：能改、不能清空，改价态显示当前值', async ({ page, request }) => {
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
      max_concurrent_jobs: 2,
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
  // 发布命令带的名额：卡片上就是那个数字。
  await expect(card).toContainText('2（每账户）');

  // 改成 3：保存后重新载入页面仍能看到 3。
  await input.fill('3');
  await page.getByTestId('models-quota-submit-' + model).click();
  await expect(card).toContainText('3（每账户）', { timeout: 15_000 });
  await page.reload();
  await expect(page.getByTestId('models-quota-input-' + model)).toBeVisible({ timeout: 15_000 });
  await expect(page.locator('.ant-card', { has: page.getByTestId('models-quota-input-' + model) }))
    .toContainText('3（每账户）');

  // 非法值在页面上被点名拒掉：填 0 或清空都不发出去（列必填，没有"清空"这一态）。
  for (const bad of ['0', '']) {
    await page.getByTestId('models-quota-input-' + model).fill(bad);
    await page.getByTestId('models-quota-submit-' + model).click();
    await expect(page.getByText('并发名额至少 1')).toBeVisible({ timeout: 15_000 });
    await page.getByTestId('models-quota-input-' + model).fill('3');
  }

  // 改价抽屉：显示该模型当前的名额，并且不改它就不发送这个字段。
  await page.getByTestId('models-reprice-' + model).click();
  await expect(page.getByTestId('platform-quota')).toBeVisible({ timeout: 15_000 });
  await expect(page.getByTestId('platform-quota')).toHaveValue('3');
  await expect(page.getByText('该模型当前是 3；不改就不发这个字段')).toBeVisible();
});
