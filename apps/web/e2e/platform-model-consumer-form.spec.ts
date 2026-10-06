import { expect, test } from '@playwright/test';
import { adminApiUrl, consoleUrl, settings } from './settings';

/// **对客计价形态由运营选，与成本形态独立**（Spec `0001` §M2/§5.3 V-D12、`0007` §1/§2、`ADR-0021`）。
///
/// 验的界面事实（Spec V-D12 的界面验收）：对客形态只有**按 token 四档 / 上游声明金额 × 倍率**两种，
/// 而**这条通路真能算出来的那一种才摆出来**——驱动器给不出四分项用量的通路不能按 token 四档卖，
/// 取不到金额的通路不能按上游声明金额卖（发布期同样会拒）。
/// token 四档的初始值取**该 vendor／模型已知的渠道价目** × 倍率 × 折算率，与选哪条候选无关。
/// 整页不出现渠道地址、凭证变量名、驱动器。
///
/// 夹具：e2e 环境是空库、没有素材导入，用旧的内联形状发布**一个 vendor 模型、两条供给**——一条按
/// token 四档计价（驱动器给得出四分项用量），一条由上游声明金额（驱动器只回金额），模拟"同一个模型
/// 两家渠道"。
test('对客计价形态只摆这条通路算得出的那一种：token 初始价取 vendor/模型已知价目', async ({
  page,
  request,
}) => {
  const suffix = Date.now();
  const model = `e2e-cf-${suffix}`;
  const tokenUpstream = `e2e-cf-token-${suffix}`;
  const declaredUpstream = `e2e-cf-declared-${suffix}`;
  const platformName = `e2e-cf-platform-${suffix}`;

  // 折算率：渠道币种没录过，对客 token 初始价推不出来，发布期也会拒。
  const fx = await request.put(adminApiUrl + '/api/v1/fx-rates', {
    headers: { Authorization: 'Bearer ' + settings.adminToken },
    data: { currency: 'USD', rate_micros: 7_200_000 },
  });
  expect(fx.ok(), '折算率录入应当成功：' + (await fx.text())).toBeTruthy();

  const capability = (nativeModel: string) => ({
    type: 'object',
    additionalProperties: false,
    required: ['model', 'prompt'],
    properties: {
      model: { const: nativeModel },
      prompt: { type: 'string', minLength: 1 },
    },
  });
  const restrictions = { allowed_branches: ['prompt_only'], max_reference_images: 0 };
  const seed = async (body: Record<string, unknown>) => {
    const response = await request.post(adminApiUrl + '/api/v1/runtime-revisions', {
      headers: { Authorization: 'Bearer ' + settings.adminToken },
      data: body,
    });
    expect(response.ok(), '夹具发布应当成功：' + (await response.text())).toBeTruthy();
  };

  // 一个 vendor 模型、两条供给：一条按 token 四档（有渠道价目），一条由上游声明金额。
  await seed({
    vendor_id: 'OpenAI',
    native_model_id: model,
    gateway_model: `e2e-cf-seed-${suffix}`,
    native_revision: 'e2e-1',
    type: 'image',
    actor: 'e2e',
    markup_bps: 2000,
    capability_schema: capability(model),
    documentation: {
      narrative: '# {{platform_name}}\n\n{{parameter_table}}\n',
      fields: {
        '/properties/model': '目录返回的模型名。',
        '/properties/prompt': '提示词。',
      },
    },
    offerings: [
      {
        provider_kind: 'E2ECfToken',
        // 这条供给按 token 四档计价：只有给得出四分项用量的驱动器承载得了它。
        adapter_key: 'apimart-image-v1',
        provider_model_id: tokenUpstream,
        base_url: 'https://e2e-do-not-show.example.com',
        credential_env: 'E2E_DO_NOT_SHOW_KEY',
        restrictions,
        carrier_schema: capability(model),
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
      {
        provider_kind: 'AIHubMix',
        // 这条供给只回上游声明的金额、给不出四分项用量。
        adapter_key: 'aihubmix-image-v1',
        provider_model_id: declaredUpstream,
        base_url: 'https://e2e-do-not-show.example.com',
        credential_env: 'E2E_DO_NOT_SHOW_KEY',
        restrictions,
        carrier_schema: capability(model),
        parameter_mapping: {},
        formula: 'upstream_declared',
        cost_currency: 'USD',
      },
    ],
  });

  // ── 运营的路：登录 → 上架新模型 ──
  await page.goto(consoleUrl);
  await page.getByTestId('admin-email').fill(settings.adminEmail);
  await page.getByTestId('admin-password').fill(settings.adminPassword);
  await page.getByTestId('admin-sign-in').click();

  await page.locator('.ant-layout-sider').getByRole('menuitem', { name: '模型目录' }).click();
  await page.getByRole('button', { name: /发布新模型|上架新模型/ }).click();

  await page.getByTestId('platform-name').fill(platformName);
  await page.getByTestId('platform-vendor').click();
  await page.getByTitle('OpenAI').click();

  // **整页不出现渠道部署事实与驱动器**。
  const scanBody = async () => {
    const body = await page.locator('body').innerText();
    for (const forbidden of [
      'e2e-do-not-show.example.com',
      'E2E_DO_NOT_SHOW_KEY',
      'base_url',
      'credential_env',
      'adapter_key',
      'carrier_schema',
      'parameter_mapping',
      'aihubmix-image-v1',
      'apimart-image-v1',
    ]) {
      expect(body, '运营界面不该出现 ' + forbidden).not.toContain(forbidden);
    }
  };
  await scanBody();

  // ── 只回金额那条：对客形态只有"上游声明金额 × 倍率"一项，界面写清为什么 ──
  const declaredPick = page.getByTestId(`platform-pick-AIHubMix-${declaredUpstream}`);
  await declaredPick.check();
  const declaredForm = page.getByTestId(`platform-consumer-form-AIHubMix-${declaredUpstream}`);
  await declaredForm.click();
  await expect(page.getByRole('option')).toHaveCount(1);
  await expect(
    page.getByText('这条通路只回金额、不给用量，所以对客只能按上游声明金额 × 倍率。'),
  ).toBeVisible();
  await page.keyboard.press('Escape');
  // 这条只用来验可选形态，不发布。
  await declaredPick.uncheck();

  // ── 给得出四分项用量那条：默认按 token 四档，两种形态都成立（它也声明金额）──
  const tokenPick = page.getByTestId(`platform-pick-E2ECfToken-${tokenUpstream}`);
  await tokenPick.check();
  const tokenForm = page.getByTestId(`platform-consumer-form-E2ECfToken-${tokenUpstream}`);
  await expect(tokenForm).toContainText('按 token 四档');
  await tokenForm.click();
  await expect(page.getByRole('option')).toHaveCount(2);
  await page.keyboard.press('Escape');
  // 四档金额按**渠道原币种**（USD）预填：该 vendor／模型已知的渠道价目 5/8/10/30；
  // 人民币对客价 = 金额 × 折算率 7.2 × 倍率 1.2 = 43.20/69.12/86.40/259.20（元／每 1M token）。
  await expect(page.getByTestId('platform-amount-0')).toHaveValue('5');
  await expect(page.getByTestId('platform-amount-1')).toHaveValue('8');
  await expect(page.getByTestId('platform-amount-2')).toHaveValue('10');
  await expect(page.getByTestId('platform-amount-3')).toHaveValue('30');
  await expect(
    page.getByText('对客人民币价（每 1M token）：文入 ¥43.20／图入 ¥69.12／文出 ¥86.40／图出 ¥259.20'),
  ).toBeVisible();
  // 运营可改：改的是**原币种金额**，人民币价跟着变（5.5 × 8.64 = 47.52）。
  await page.getByTestId('platform-amount-0').fill('5.5');
  await expect(page.getByText(/文入 ¥47\.52/)).toBeVisible();
  // 改成发布要用的那一组（原币种 7/9/11/40）。
  for (const [index, value] of [7, 9, 11, 40].entries()) {
    await page.getByTestId('platform-amount-' + index).fill(String(value));
  }

  await scanBody();
  // **P3：成本侧与技术细节不在运营表单里**：参考成本、保底表 JSON、微单位一个都不该出现。
  const pricingBody = await page.locator('body').innerText();
  for (const gone of ['微单位', '参考成本', '保底表']) {
    expect(pricingBody, `运营定价处不该出现 \`${gone}\``).not.toContain(gone);
  }
  await page.getByTestId('platform-publish').click();
  await expect(page.getByText(platformName).first()).toBeVisible({ timeout: 15_000 });

  // 发布物：对客形态是运营选的那一个（7 × 7.2 × 1.2 = 60.48 元／每 1M token）。
  const listing = await request.get(adminApiUrl + '/api/v1/gateway-models', {
    headers: { Authorization: 'Bearer ' + settings.adminToken },
  });
  const catalog = (await listing.json()) as {
    gateway_models: {
      gateway_model: string;
      candidates: {
        provider_model_id: string;
        consumer_formula: string | null;
        consumer_rates_cny: { text_input_micros_per_million: number } | null;
      }[];
    }[];
  };
  const published = catalog.gateway_models.find((item) => item.gateway_model === platformName);
  expect(published, '发布出来的平台模型要能在目录里查到').toBeTruthy();
  const candidate = published?.candidates.find(
    (item) => item.provider_model_id === tokenUpstream,
  );
  expect(candidate, '按 token 计价那条候选应当在').toBeTruthy();
  expect(candidate?.consumer_formula).toBe('token_rates');
  expect(candidate?.consumer_rates_cny?.text_input_micros_per_million).toBe(60_480_000);

  // 供给登记的成本形态没被对客选择改写：仍是 token_rates。
  const offerings = await request.get(adminApiUrl + '/api/v1/offerings', {
    headers: { Authorization: 'Bearer ' + settings.adminToken },
  });
  const offeringsBody = (await offerings.json()) as {
    offerings: { native_model_id: string; provider_model_id: string; formula: string }[];
  };
  const supply = offeringsBody.offerings.find(
    (item) => item.native_model_id === model && item.provider_model_id === tokenUpstream,
  );
  expect(supply?.formula, '成本形态仍是渠道事实 token_rates').toBe('token_rates');
});
