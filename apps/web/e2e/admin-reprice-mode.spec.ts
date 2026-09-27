import { readFileSync } from 'node:fs';
import { expect, test } from '@playwright/test';
import { adminApiUrl, consoleUrl, settings } from './settings';

/// 厂商合同与渠道承载面：**厂商与渠道给的结构声明**，手写等于重造一遍渠道文档。
/// 界面上就写着"从渠道文档或上一版贴过来"，所以照真实做法从已发布的素材里读。
function bootstrapMaterial(): {
  capability_schema: Record<string, unknown> & { properties: Record<string, unknown> };
  offerings: { carrier_schema: Record<string, unknown> & { properties: Record<string, unknown> } }[];
} {
  const raw = readFileSync(
    new URL('../../../config/bootstrap/gpt-image-2.5-flare.json', import.meta.url),
    'utf8',
  );
  return JSON.parse(raw) as ReturnType<typeof bootstrapMaterial>;
}

/// **改价不用碰渠道技术字段**（Spec V-D9）。
///
/// 上架一个型号（技术字段齐全），再从「模型目录」那一行点「改价」，只改倍率与合同修订号并发布。
/// 要证明的是：改价时界面里**没有**渠道地址与凭证变量名这两个输入，而且发布之后那个型号的生效候选仍
/// 指向同一渠道、倍率变成新值——沿用真的发生了。
///
/// 这条同时守住"两页合成一页"：入口在「模型目录」（右上角"上架新模型" + 每行"改价"），表单在抽屉里，
/// 不再有第二个导航项。
test('改价：界面不出现渠道地址与凭证变量名，发布后仍指向同一渠道', async ({ page, request }) => {
  const model = `e2e-reprice-${Date.now()}`;
  const material = bootstrapMaterial();

  // 折算率要先录：按 token 计量的候选没有它就发不出去。
  const rate = await request.put(`${adminApiUrl}/api/v1/fx-rates`, {
    headers: { authorization: `Bearer ${settings.adminToken}` },
    data: { currency: 'USD', rate_micros: 7_100_000 },
  });
  expect(rate.status()).toBe(204);

  await page.goto(consoleUrl);
  await page.getByTestId('admin-email').fill(settings.adminEmail);
  await page.getByTestId('admin-password').fill(settings.adminPassword);
  await page.getByTestId('admin-sign-in').click();

  // 第一次：上架（技术入口，字段齐全）。**要先展开工程师那条折叠区**——运营的主路径不给技术字段，
  // 所以贴整份定义的表单默认收起；改价本身仍然走运营那条路（下面第二步）。
  await page.getByTestId('models-add').click();
  await page.getByRole('button', { name: /工程师：贴整份发布定义/ }).click();
  await expect(page.getByTestId('publish-vendor')).toBeVisible();
  await page.getByTestId('publish-vendor').fill('OpenAI');
  await page.getByTestId('publish-native-model').fill(model);
  await page.getByTestId('publish-native-revision').fill('e2e-reprice-1.0');
  await page.getByTestId('publish-markup-bps').fill('2000');
  await page.getByTestId('publish-provider-kind').fill('AIHubMix');
  await page.getByTestId('publish-provider-model').fill(model);
  await page.getByTestId('publish-adapter-key').fill('aihubmix-image-v1');
  await page.getByTestId('publish-base-url').fill('http://127.0.0.1:9/stub');
  await page.getByTestId('publish-credential-env').fill('E2E_DUMMY_KEY');
  await page.getByTestId('publish-plan-source-url').fill('https://vendor.example.com/pricing');
  // 渠道费率的四档（成本币种微单位/百万 token）：按 token 计量的候选必须有它。
  for (const [testId, value] of [
    ['publish-plan-text-input', 5_000_000],
    ['publish-plan-image-input', 8_000_000],
    ['publish-plan-text-output', 10_000_000],
    ['publish-plan-image-output', 30_000_000],
  ] as const) {
    await page.getByTestId(testId).fill(String(value));
  }

  // 合同与承载面：`model.const` 必须等于原生型号名，所以只改这一处。
  // 先展开"技术字段"再填合同——合同输入框在折叠面板里，先填会被折叠吞掉。
  await page.getByText('技术字段（贴 JSON；留空即不带）').click();
  const contract = {
    ...material.capability_schema,
    properties: { ...material.capability_schema.properties, model: { const: model } },
  };
  const contractBox = page
    .locator('.ant-card')
    .filter({ hasText: '模型的合同（capability_schema）' })
    .locator('textarea');
  await expect(contractBox).toHaveCount(1);
  await contractBox.fill(JSON.stringify(contract));

  const carrier = {
    ...material.offerings[0].carrier_schema,
    properties: { ...material.offerings[0].carrier_schema.properties, model: { const: model } },
  };
  await page
    .locator('.ant-form-item')
    .filter({ hasText: '承载面 carrier_schema' })
    .locator('textarea')
    .fill(JSON.stringify(carrier));

  // 按 token 计量的候选随修订带一份四档 CNY 对客价，所以那个开关要打开、四个数要填。
  await page.getByRole('button', { name: '带上对客价向量' }).click();
  for (const [testId, value] of [
    ['publish-cny-text-input', 42_600_000],
    ['publish-cny-image-input', 68_160_000],
    ['publish-cny-text-output', 85_200_000],
    ['publish-cny-image-output', 255_600_000],
  ] as const) {
    await page.getByTestId(testId).fill(String(value));
  }

  await page.getByTestId('publish-submit').click();
  await expect(page.getByText(`已发布 ${model}`).first()).toBeVisible({ timeout: 15_000 });
  await page.keyboard.press('Escape');
  const reprice = page.getByTestId(`models-reprice-${model}`);
  await expect(reprice).toBeVisible({ timeout: 15_000 });

  // 第二次：改价。**走运营那条路**（选厂商、勾供给、给价）——它的判据与上一条 spec 同一件事：
  // 界面上不该出现渠道地址、凭证变量名、驱动器与渠道费率。这里断言的是**整页正文**（不是某几个输入框
  // 不存在），因为"某个字段没渲染"与"它没从别处漏出来"是两件事。
  await reprice.click();
  await expect(page.getByText(`改价：${model}`)).toBeVisible({ timeout: 15_000 });
  await expect(page.getByTestId('platform-publish')).toBeVisible();
  const repriceBody = await page.locator('body').innerText();
  for (const forbidden of [
    'http://127.0.0.1:9/stub',
    'E2E_DUMMY_KEY',
    'publish-base-url',
    'publish-credential-env',
    'publish-adapter-key',
    'publish-plan-text-input',
  ]) {
    expect(repriceBody, `改价界面不该出现 \`${forbidden}\``).not.toContain(forbidden);
  }
  // 老的那几个技术输入框一个都不该在（改价路径不渲染它们）。
  await expect(page.getByTestId('publish-base-url')).toHaveCount(0);
  await expect(page.getByTestId('publish-credential-env')).toHaveCount(0);
  await expect(page.getByTestId('publish-provider-kind')).toHaveCount(0);
  await expect(page.getByTestId('publish-adapter-key')).toHaveCount(0);
  await expect(page.getByTestId('publish-plan-text-input')).toHaveCount(0);

  await page.getByTestId('platform-markup-bps').fill('3500');
  await page.getByTestId('platform-publish').click();
  await expect(page.getByText(`已发布 ${model}`).first()).toBeVisible({ timeout: 15_000 });

  // 沿用真的发生了：倍率是新值，候选仍在，而**渠道价目没被改坏**。
  //
  // 这里轮询而不是读一次：发布是一次事务，界面上的"已发布"来自 POST 的响应，而紧接着的这次读可能
  // 落在提交可见之前。一条断言不该因为这种时序而随机红。
  await expect
    .poll(
      async () => {
        const listing = await request.get(`${adminApiUrl}/api/v1/gateway-models`, {
          headers: { authorization: `Bearer ${settings.adminToken}` },
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
          // 成本币种从上一版沿用：界面从头到尾没让运营碰过它。
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
