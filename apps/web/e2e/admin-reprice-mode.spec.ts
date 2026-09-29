import { expect, test } from '@playwright/test';
import { adminApiUrl, consoleUrl, settings } from './settings';

/// **改价不用碰渠道技术字段**（Spec V-D9）。
///
/// 夹具先用内联形状把一条供给与一个平台模型发出来（渠道与供给本该由工程师随素材配置；e2e 是空库、
/// 没有素材导入，所以这里直接调 API 造夹具）。再从「模型目录」那一行点「改价」，只改倍率并发布。
/// 要证明的是：改价时界面里**没有**渠道地址与凭证变量名这两个输入，而且发布之后那个型号的生效候选
/// 仍指向同一渠道、倍率变成新值——沿用真的发生了。
test('改价：界面不出现渠道地址与凭证变量名，发布后仍指向同一渠道', async ({ page, request }) => {
  const suffix = Date.now();
  const vendorModel = 'e2e-reprice-supply-' + suffix;
  const model = 'e2e-reprice-' + suffix;

  // 折算率：按 token 计量的候选没有它就发不出去，夹具发布之前先录。
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
      native_revision: 'e2e-reprice-1.0',
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
          provider_model_id: 'e2e-reprice-upstream',
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

  // ── 改价：走运营那条路 ──
  await page.goto(consoleUrl);
  await page.getByTestId('admin-email').fill(settings.adminEmail);
  await page.getByTestId('admin-password').fill(settings.adminPassword);
  await page.getByTestId('admin-sign-in').click();

  const reprice = page.getByTestId('models-reprice-' + model);
  await expect(reprice).toBeVisible({ timeout: 15_000 });
  await reprice.click();
  await expect(page.getByText('改价：' + model)).toBeVisible({ timeout: 15_000 });
  await expect(page.getByTestId('platform-publish')).toBeVisible();
  // 断言的是**整页正文**（不是某几个输入框不存在），因为'某个字段没渲染'与'它没从别处漏出来'是两件事。
  const repriceBody = await page.locator('body').innerText();
  for (const forbidden of [
    'http://127.0.0.1:9/stub',
    'E2E_DUMMY_KEY',
    'publish-base-url',
    'publish-credential-env',
    'publish-adapter-key',
    'publish-plan-text-input',
  ]) {
    expect(repriceBody, '改价界面不该出现 ' + forbidden).not.toContain(forbidden);
  }

  await page.getByTestId('platform-markup-bps').fill('3500');
  await page.getByTestId('platform-publish').click();
  await expect(page.getByText('已发布 ' + model).first()).toBeVisible({ timeout: 15_000 });

  await expect
    .poll(
      async () => {
        const listing = await request.get(adminApiUrl + '/api/v1/gateway-models', {
          headers: { authorization: 'Bearer ' + settings.adminToken },
        });
        const body = (await listing.json()) as {
          gateway_models: {
            gateway_model: string;
            markup_bps: number | null;
            candidates: { cost_currency: string | null; consumer_rates_cny: unknown }[];
          }[];
        };
        const found = body.gateway_models.find((item) => item.gateway_model === model);
        return {
          markup_bps: found?.markup_bps ?? null,
          candidates: found?.candidates.length ?? 0,
          cost_currency: found?.candidates[0]?.cost_currency ?? null,
          has_consumer_rates: found?.candidates[0]?.consumer_rates_cny != null,
        };
      },
      { timeout: 15_000, message: '发布之后模型列表里应当有这个型号、倍率是新值、渠道成本沿用' },
    )
    .toEqual({
      markup_bps: 3_500,
      candidates: 1,
      cost_currency: 'USD',
      has_consumer_rates: true,
    });
});
