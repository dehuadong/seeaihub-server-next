import { expect, test, type APIRequestContext } from '@playwright/test';
import { adminApiUrl, consoleUrl, settings } from './settings';

/// 夹具模型的最小合同与承载面（一个 vendor 模型下的两条供给共用同一份合同）。
const capability = (nativeModel: string) => ({
  type: 'object',
  additionalProperties: false,
  required: ['model', 'prompt'],
  properties: {
    model: { const: nativeModel },
    prompt: { type: 'string', minLength: 1 },
  },
});
const promptOnly = { allowed_branches: ['prompt_only'], max_reference_images: 0 };

/// 模型说明的最小正文：发布要求带说明，内容与这两个用例无关。
const documentation = {
  narrative: '# {{platform_name}}\n\n{{parameter_table}}\n',
  fields: {
    '/properties/model': '目录返回的模型名。',
    '/properties/prompt': '提示词。',
  },
};

/// 用内联形状发布一份夹具：e2e 环境是空库、没有素材导入，供给只能这么造。
async function seed(request: APIRequestContext, body: Record<string, unknown>) {
  const response = await request.post(adminApiUrl + '/api/v1/runtime-revisions', {
    headers: { Authorization: 'Bearer ' + settings.adminToken },
    data: body,
  });
  expect(response.ok(), '夹具发布应当成功：' + (await response.text())).toBeTruthy();
}

/// **对客计价形态由运营选，与成本形态独立**（Spec `0001` §M2/§5.3 V-D12、`0007` §1/§2、`ADR-0021`）。
///
/// 验的界面事实（Spec V-D12 的界面验收）：对客形态只有**按 token 四档 / 上游声明金额 × 倍率**两种，
/// 而**这条通路真能算出来的那一种才摆出来**——驱动器给不出四分项用量的通路不能按 token 四档卖，
/// 取不到金额的通路不能按上游声明金额卖（发布期同样会拒）；新候选的默认形态也落在**可选的那一种**
/// 上，不落在一个算不出来、发不出去的形态上。
/// token 四档的初始值取**该 vendor／模型已知的价目**（模型声明的对客参考价目，旧形状回退到某条供给的
/// Price Plan）× 倍率 × 折算率，与选哪条候选无关。
/// 整页不出现渠道地址、凭证变量名、驱动器。
///
/// 夹具：e2e 环境是空库、没有素材导入，用旧的内联形状发布**一个 vendor 模型、两条供给**——一条按
/// token 四档计价（驱动器给得出四分项用量），一条由上游声明金额（驱动器只回金额），模拟"同一个模型
/// 两家渠道"。
test('对客计价形态只摆这条通路算得出的那一种：默认形态与 token 初始价都取算得出来的那份', async ({
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

  // 一个 vendor 模型、两条供给：一条按 token 四档（有渠道价目），一条由上游声明金额。
  await seed(request, {
    vendor_id: 'OpenAI',
    native_model_id: model,
    gateway_model: `e2e-cf-seed-${suffix}`,
    native_revision: 'e2e-1',
    type: 'image',
    actor: 'e2e',
    markup_bps: 2000,
    capability_schema: capability(model),
    documentation,
    offerings: [
      {
        provider_kind: 'E2ECfToken',
        // 这条供给按 token 四档计价：只有给得出四分项用量的驱动器承载得了它。
        adapter_key: 'apimart-image-v1',
        provider_model_id: tokenUpstream,
        base_url: 'https://e2e-do-not-show.example.com',
        credential_env: 'E2E_DO_NOT_SHOW_KEY',
        restrictions: promptOnly,
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
        restrictions: promptOnly,
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

  // ── 只回金额那条：对客形态只有"上游声明金额 × 倍率"一项，默认就落在它上面，界面写清为什么 ──
  const declaredPick = page.getByTestId(`platform-pick-AIHubMix-${declaredUpstream}`);
  await declaredPick.check();
  const declaredForm = page.getByTestId(`platform-consumer-form-AIHubMix-${declaredUpstream}`);
  // 默认值必须是**可选的那一种**：落在"按 token 四档"上时下拉显示的是原值、四个金额输入框还会摆出来，
  // 而那个形态这条通路算不出来、发布期也拒。
  await expect(declaredForm).toContainText('上游声明金额 × 倍率');
  await expect(page.getByTestId('platform-amount-0')).toHaveCount(0);
  await declaredForm.click();
  await expect(page.getByRole('option')).toHaveCount(1);
  await expect(
    page.getByText('这条通路只回金额、不给用量，所以对客只能按上游声明金额 × 倍率。'),
  ).toBeVisible();
  await page.keyboard.press('Escape');
  // 保持勾选：这条候选的形态要能照原样发出去（它没有金额要填）。

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
  // 只回金额那条候选照原样发得出去：形态就是界面默认落在的那一种，没有金额要填。
  const declaredCandidate = published?.candidates.find(
    (item) => item.provider_model_id === declaredUpstream,
  );
  expect(declaredCandidate, '只回金额那条候选也应当在').toBeTruthy();
  expect(declaredCandidate?.consumer_formula).toBe('upstream_declared');

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

/// **按 token 四档的初始价取供给声明的对客参考价目**（Spec `0001` §5.3 V-D12、`0007` §2）。
///
/// 参考价目不是成本参数：成本按上游声明金额的供给（`upstream_declared`，没有 Price Plan）照样可以
/// 声明它，同模型下按 token 四档卖的候选就取它当初始价——这份价目一断，按 token 卖的那条候选就没有
/// 默认值了。没有它时界面说清"按报价填"，不把它说成"缺折算率"。
///
/// 夹具：一个 vendor 模型两条供给——一条按 token 四档卖、成本按上游声明金额（给得出四分项用量），
/// 一条声明对客参考价目；另有一个模型只放前者，用来验"没有参考价目"的说法。
test('按 token 四档的初始价取模型声明的对客参考价目，没有它就说清要按报价填', async ({
  page,
  request,
}) => {
  const suffix = Date.now();
  const model = `e2e-cf-ref-${suffix}`;
  const tokenUpstream = `e2e-cf-ref-token-${suffix}`;
  const listUpstream = `e2e-cf-ref-list-${suffix}`;
  const bareModel = `e2e-cf-nolist-${suffix}`;
  const bareUpstream = `e2e-cf-nolist-token-${suffix}`;

  const fx = await request.put(adminApiUrl + '/api/v1/fx-rates', {
    headers: { Authorization: 'Bearer ' + settings.adminToken },
    data: { currency: 'USD', rate_micros: 7_200_000 },
  });
  expect(fx.ok(), '折算率录入应当成功：' + (await fx.text())).toBeTruthy();

  const tokenOffering = (providerModelId: string) => ({
    provider_kind: 'APIMart',
    // 给得出四分项用量、也声明金额：对客可以按 token 四档卖，成本仍按上游声明的金额。
    adapter_key: 'apimart-image-v1',
    provider_model_id: providerModelId,
    base_url: 'https://e2e-do-not-show.example.com',
    credential_env: 'E2E_DO_NOT_SHOW_KEY',
    restrictions: promptOnly,
    carrier_schema: capability(model),
    parameter_mapping: {},
    formula: 'upstream_declared',
    cost_currency: 'USD',
  });
  await seed(request, {
    vendor_id: 'OpenAI',
    native_model_id: model,
    gateway_model: `e2e-cf-ref-seed-${suffix}`,
    native_revision: 'e2e-1',
    type: 'image',
    actor: 'e2e',
    markup_bps: 2000,
    capability_schema: capability(model),
    documentation,
    offerings: [
      tokenOffering(tokenUpstream),
      {
        provider_kind: 'AIHubMix',
        // 它自己只回金额、不按 token 卖；模型级的参考价目与它无关。
        adapter_key: 'aihubmix-image-v1',
        provider_model_id: listUpstream,
        base_url: 'https://e2e-do-not-show.example.com',
        credential_env: 'E2E_DO_NOT_SHOW_KEY',
        restrictions: promptOnly,
        carrier_schema: capability(model),
        parameter_mapping: {},
        formula: 'upstream_declared',
        cost_currency: 'USD',
      },
    ],
    // 参考价目是**模型级**的一份（与合同同级）：该模型下按 token 四档卖的候选取它当初始价。
    consumer_reference_rates: {
      currency: 'USD',
      text_input_microusd_per_million: 5_000_000,
      image_input_microusd_per_million: 8_000_000,
      text_output_microusd_per_million: 10_000_000,
      image_output_microusd_per_million: 30_000_000,
    },
  });
  // 另一个模型：只有按 token 四档卖的那条，全模型没有任何参考价目可作默认值。
  await seed(request, {
    vendor_id: 'OpenAI',
    native_model_id: bareModel,
    gateway_model: `e2e-cf-nolist-seed-${suffix}`,
    native_revision: 'e2e-1',
    type: 'image',
    actor: 'e2e',
    markup_bps: 2000,
    capability_schema: capability(bareModel),
    documentation,
    offerings: [tokenOffering(bareUpstream)],
  });

  await page.goto(consoleUrl);
  await page.getByTestId('admin-email').fill(settings.adminEmail);
  await page.getByTestId('admin-password').fill(settings.adminPassword);
  await page.getByTestId('admin-sign-in').click();

  await page.locator('.ant-layout-sider').getByRole('menuitem', { name: '模型目录' }).click();
  await page.getByRole('button', { name: /发布新模型|上架新模型/ }).click();
  await page.getByTestId('platform-vendor').click();
  await page.getByTitle('OpenAI').click();

  // 按 token 四档卖那条：初始价取**另一条供给声明的参考价目**——它自己没有 Price Plan（成本按上游
  // 声明金额），旧口径在这里是空的，靠参考价目才有默认值。
  await page.getByTestId(`platform-pick-APIMart-${tokenUpstream}`).check();
  await expect(page.getByTestId(`platform-consumer-form-APIMart-${tokenUpstream}`)).toContainText(
    '按 token 四档',
  );
  await expect(page.getByTestId('platform-amount-0')).toHaveValue('5');
  await expect(page.getByTestId('platform-amount-3')).toHaveValue('30');
  await expect(page.getByText('默认取该 vendor／模型已知的对客参考价目')).toBeVisible();
  // 人民币对客价照旧由"金额 × 折算率 7.2 × 倍率 1.2"算出。
  await expect(
    page.getByText(
      '对客人民币价（每 1M token）：文入 ¥43.20／图入 ¥69.12／文出 ¥86.40／图出 ¥259.20',
    ),
  ).toBeVisible();
  await page.getByTestId(`platform-pick-APIMart-${tokenUpstream}`).uncheck();

  // 没有参考价目的模型：说清"按报价填"，**不**把它说成缺折算率（折算率上面已经录过）。
  await page.getByTestId(`platform-pick-APIMart-${bareUpstream}`).check();
  await expect(page.getByTestId('platform-amount-0')).toHaveValue('');
  await expect(
    page.getByText(
      '这个 vendor／模型还没有已知的价目可作默认值——请按报价填四档金额（原币种），空着不许发。',
    ),
  ).toBeVisible();
  await expect(page.getByText('还缺折算率，人民币对客价算不出来。')).toHaveCount(0);
});
