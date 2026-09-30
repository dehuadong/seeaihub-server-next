import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import { adminApiUrl, portalUrl, settings } from './settings';
import { captureActiveHold, releaseActiveHold } from './account-state';

/// 客户概览只显示**已结算余额**：预授权建立或释放都不改变这个读数，页面也不出现可用额、持有中或
/// 单笔预授权金额（账户资金 Spec A7 的客户那半、控制台 Spec C7）。
///
/// "预授权 30"是**真跑**出来的：e2e 不起 Worker，同步入口在窗口后超时（504），请求停在持有中，
/// 已结算余额不变——这正是 Spec 的受理语义。实收扣减与释放预授权是 Worker 的结算/收尾事务，
/// 浏览器用例里跑不出来，由 `account-state.ts` 把**结果**摆进 e2e 库；事务本身由
/// `apps/api/tests/http_contract/cases_lifecycle.rs` 用真 Worker + 假上游覆盖。
///
/// 夹具供给用内联发布形状造（与 `platform-model-publish.spec.ts` 同一条路）：e2e 是空库、没有素材
/// 导入，先发布一条带保底表的供给，受理才有一条候选可选。保底表按 2K 档给 30 元——请求缺省 `size`
/// 归到 2K 档，所以预授权额就是 30 元。

const PORTAL = portalUrl;
const FORBIDDEN = ['可用余额', '可用额', '持有中', '预授权'];

function unique(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.floor(Math.random() * 10_000)}`;
}

/// 发布一条只够本次用例的供给：合同只要 `model` + `prompt`，保底表 2K 档 30 元。
async function publishHoldFixture(
  request: APIRequestContext,
  gatewayModel: string,
  vendorModel: string,
): Promise<void> {
  // 渠道成本币种是 USD，发布期要有一条折算率，否则夹具自己先失败。
  const fx = await request.put(`${adminApiUrl}/api/v1/fx-rates`, {
    headers: { Authorization: `Bearer ${settings.adminToken}` },
    data: { currency: 'USD', rate_micros: 7_200_000 },
  });
  expect(fx.ok(), `折算率录入应当成功：${await fx.text()}`).toBeTruthy();

  const schema = {
    type: 'object',
    additionalProperties: false,
    required: ['model', 'prompt'],
    properties: {
      model: { const: vendorModel },
      prompt: { type: 'string', minLength: 1 },
    },
  };
  const published = await request.post(`${adminApiUrl}/api/v1/runtime-revisions`, {
    headers: { Authorization: `Bearer ${settings.adminToken}` },
    data: {
      vendor_id: 'OpenAI',
      native_model_id: vendorModel,
      gateway_model: gatewayModel,
      native_revision: 'e2e-settled-1',
      actor: 'e2e',
      markup_bps: 2000,
      capability_schema: schema,
      offerings: [
        {
          provider_kind: 'AIHubMix',
          adapter_key: 'aihubmix-image-v1',
          provider_model_id: 'e2e-settled-upstream',
          // 没有 Worker，这个地址一次都不会被访问；受理只用保底表。
          base_url: 'http://127.0.0.1:1',
          credential_env: 'E2E_SETTLED_KEY',
          restrictions: { allowed_branches: ['prompt_only'], max_reference_images: 0 },
          carrier_schema: schema,
          parameter_mapping: {},
          formula: 'token_rates',
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
          reference_cost_microusd: 11_354,
          cost_basis: 'computed',
          tier_prices: { '2K': 250_000 },
          floor_amounts: { amounts: { '2K': 30_000_000 }, cap_microusd: 30_000_000 },
        },
      ],
    },
  });
  expect(published.ok(), `夹具发布应当成功：${await published.text()}`).toBeTruthy();
}

/// 造一个"充值 100、能登录、有一把 Key"的客户。
async function fundedCustomer(request: APIRequestContext): Promise<{
  accountId: string;
  email: string;
  password: string;
  apiKey: string;
}> {
  const opened = await request.post(`${adminApiUrl}/api/v1/accounts`, {
    headers: { Authorization: `Bearer ${settings.adminToken}` },
    data: { initial_credit_microusd: 100_000_000 },
  });
  expect(opened.ok(), `开户应当成功：${await opened.text()}`).toBeTruthy();
  const accountId = (await opened.json()).account_id as string;

  const email = `${unique('e2e-settled')}@example.com`;
  const password = 'e2e-settled-password';
  const bound = await request.post(`${adminApiUrl}/api/v1/customers`, {
    headers: { Authorization: `Bearer ${settings.adminToken}` },
    data: { email, password, account_id: accountId },
  });
  expect(bound.status(), `绑定登录身份应当成功：${await bound.text()}`).toBe(201);

  const issued = await request.post(`${adminApiUrl}/api/v1/accounts/${accountId}/api-keys`, {
    headers: { Authorization: `Bearer ${settings.adminToken}` },
    data: { label: 'e2e-settled' },
  });
  expect(issued.ok(), `发 Key 应当成功：${await issued.text()}`).toBeTruthy();
  const apiKey = (await issued.json()).api_key as string;
  return { accountId, email, password, apiKey };
}

async function signIn(page: Page, email: string, password: string): Promise<void> {
  await page.goto(PORTAL);
  await page.getByTestId('portal-email').fill(email);
  await page.getByTestId('portal-password').fill(password);
  await page.getByTestId('portal-submit').click();
  await expect(page.getByTestId('portal-settled-balance')).toContainText('已结算余额');
}

/// 受理一次并让它停在持有中：没有 Worker，同步入口在窗口后超时。
async function createHold(
  request: APIRequestContext,
  gatewayModel: string,
  apiKey: string,
): Promise<void> {
  const generation = await request.post(
    `http://127.0.0.1:${settings.port}/v1/images/generations`,
    {
      headers: {
        Authorization: `Bearer ${apiKey}`,
        'Idempotency-Key': unique('settled-hold'),
      },
      data: { model: gatewayModel, prompt: 'a job left holding' },
      timeout: 30_000,
    },
  );
  expect(generation.status(), `没有 Worker 时同步入口超时：${await generation.text()}`).toBe(504);
}

/// 接口确认这次受理真的占用了 30 元，而已结算余额仍是 100 元。
async function expectHold(request: APIRequestContext, apiKey: string): Promise<void> {
  const own = await request.get(`http://127.0.0.1:${settings.port}/v1/account`, {
    headers: { Authorization: `Bearer ${apiKey}` },
  });
  const body = (await own.json()) as { balance_microusd: number; held_microusd: number };
  expect(body.balance_microusd).toBe(100_000_000);
  expect(body.held_microusd).toBe(30_000_000);
}

/// 页面只显示已结算余额：读数正确，且不出现可用额、持有中或预授权金额。
async function expectSettledBalance(page: Page, text: string): Promise<void> {
  await expect(page.getByTestId('portal-settled-balance')).toContainText('已结算余额');
  await expect(page.getByTestId('portal-settled-balance')).toContainText(text);
  for (const forbidden of FORBIDDEN) {
    await expect(page.getByText(forbidden)).toHaveCount(0);
  }
}

test('充值 100 后预授权 30：概览仍显示已结算余额 100', async ({ page, request }) => {
  const gatewayModel = unique('e2e-settled');
  await publishHoldFixture(request, gatewayModel, unique('e2e-settled-vendor'));
  const { email, password, apiKey } = await fundedCustomer(request);

  await signIn(page, email, password);
  await expectSettledBalance(page, '100 元');

  await createHold(request, gatewayModel, apiKey);
  await expectHold(request, apiKey);

  // 刷新后读数不变：预授权建立不改变客户页面的余额。
  await page.reload();
  await expectSettledBalance(page, '100 元');
});

test('仅释放预授权：不改变已结算余额读数', async ({ page, request }) => {
  const gatewayModel = unique('e2e-settled');
  await publishHoldFixture(request, gatewayModel, unique('e2e-settled-vendor'));
  const { accountId, email, password, apiKey } = await fundedCustomer(request);

  await signIn(page, email, password);
  await createHold(request, gatewayModel, apiKey);
  await expectHold(request, apiKey);
  await expectSettledBalance(page, '100 元');

  // 释放占用（结果由合同用例的真事务覆盖）：余额读数不变。
  await releaseActiveHold(accountId);
  await page.reload();
  await expectSettledBalance(page, '100 元');
});

test('实收 20 后：概览显示已结算余额 80', async ({ page, request }) => {
  const gatewayModel = unique('e2e-settled');
  await publishHoldFixture(request, gatewayModel, unique('e2e-settled-vendor'));
  const { accountId, email, password, apiKey } = await fundedCustomer(request);

  await signIn(page, email, password);
  await createHold(request, gatewayModel, apiKey);
  await expectHold(request, apiKey);
  await expectSettledBalance(page, '100 元');

  // 成功结算扣 20 元（结果由合同用例的真事务覆盖）：读数变成 80，仍不出现预授权金额。
  await captureActiveHold(accountId, 20_000_000);
  await page.reload();
  await expectSettledBalance(page, '80 元');
});
