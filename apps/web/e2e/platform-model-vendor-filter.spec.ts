import { expect, test } from '@playwright/test';
import { adminApiUrl, consoleUrl, settings } from './settings';

/// **选完厂商后清单只列该厂商的供给**（Spec V-D8 的"选择该 vendor 适配的渠道模型"）。
///
/// 为什么这条不能只靠人眼：接口返回的是**全部**供给（`GET /api/v1/offerings` 不带厂商参数，因为"按厂商
/// 分组"是给界面用的读法），按厂商过滤纯粹是这一页的行为。过滤写错——漏过滤、按厂商模型名过滤、或者拿
/// `native_model_id` 当厂商——界面上都只是"多了几条"，看着不像故障，而运营会据此把两个厂商的供给勾在
/// 一次发布里，然后被发布期的跨厂商校验拒掉，却不知道是清单先骗了他。
///
/// 所以夹具要**两个厂商**：现有用例都只造一个，正是这个判据之前没有机器证据的原因。
test('清单只列所选厂商的供给', async ({ request, page }) => {
  const suffix = Date.now();
  const fx = await request.put(`${adminApiUrl}/api/v1/fx-rates`, {
    headers: { Authorization: `Bearer ${settings.adminToken}` },
    data: { currency: 'USD', rate_micros: 7_200_000 },
  });
  expect(fx.ok(), `折算率录入应当成功：${await fx.text()}`).toBeTruthy();

  // 两个厂商各发布一条供给。**厂商身份由 `vendor_id` 决定**，两家用不同的厂商模型名。
  for (const vendor of ['OpenAI', 'Anthropic']) {
    const vendorModel = `e2e-vendor-${vendor}-${suffix}`;
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
        vendor_id: vendor,
        native_model_id: vendorModel,
        gateway_model: `e2e-vendor-seed-${vendor}-${suffix}`,
        native_revision: 'e2e-1',
        type: 'image',
        actor: 'e2e',
        markup_bps: 2000,
        capability_schema: capability,
        offerings: [
          {
            provider_kind: `E2EChannel${vendor}`,
            adapter_key: 'aihubmix-image-v1',
            provider_model_id: `e2e-upstream-${vendor}`,
            base_url: `https://e2e-${vendor}.example.com`,
            credential_env: `E2E_${vendor.toUpperCase()}_KEY`,
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
              source_url: `https://e2e-${vendor}-price.example.com`,
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
    expect(seeded.ok(), `${vendor} 的夹具发布应当成功：${await seeded.text()}`).toBeTruthy();
  }

  await page.goto(consoleUrl);
  await page.getByTestId('admin-email').fill(settings.adminEmail);
  await page.getByTestId('admin-password').fill(settings.adminPassword);
  await page.getByTestId('admin-sign-in').click();
  await page.locator('.ant-layout-sider').getByRole('menuitem', { name: '模型目录' }).click();
  await page.getByRole('button', { name: /发布新模型|上架新模型/ }).click();
  await page.getByTestId('platform-vendor').click();

  // 两个厂商都在下拉里（否则下面的断言等于没验：清单是空的，自然"没有别家"）。
  await expect(page.getByTitle('OpenAI')).toBeVisible();
  await expect(page.getByTitle('Anthropic')).toBeVisible();
  await page.getByTitle('OpenAI').click();

  // 这一家那条在，另一家那条**不在**。
  await expect(page.getByTestId('platform-pick-E2EChannelOpenAI-e2e-upstream-OpenAI')).toBeVisible();
  await expect(page.getByTestId('platform-pick-E2EChannelAnthropic-e2e-upstream-Anthropic')).toHaveCount(
    0,
  );
  // 顺带把"整页看不到别家供给"也钉住：过滤写错时漏进来的往往不只一个输入框。
  const body = await page.locator('body').innerText();
  expect(body, '选了 OpenAI 就不该看到 Anthropic 的供给').not.toContain('e2e-upstream-Anthropic');

  // 换一家，反过来成立——一条方向对了不代表另一条对了。
  await page.getByTestId('platform-vendor').click();
  await page.getByTitle('Anthropic').click();
  await expect(
    page.getByTestId('platform-pick-E2EChannelAnthropic-e2e-upstream-Anthropic'),
  ).toBeVisible();
  await expect(page.getByTestId('platform-pick-E2EChannelOpenAI-e2e-upstream-OpenAI')).toHaveCount(0);
});
