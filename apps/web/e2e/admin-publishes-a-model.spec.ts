import { readFileSync } from 'node:fs';
import { expect, test } from '@playwright/test';
import { adminApiUrl, consoleUrl, settings } from './settings';

/// 发布素材里的合同与承载面：它们是**厂商与渠道给的结构声明**，手写等于重造一遍渠道文档。
/// 界面上就写着"从渠道文档或上一版贴过来"，所以这里照真实做法从已发布的素材里读。
function bootstrapMaterial(): {
  capability_schema: Record<string, unknown>;
  carrier_schema: Record<string, unknown>;
} {
  const raw = readFileSync(
    new URL('../../../config/bootstrap/gpt-image-2.5-flare.json', import.meta.url),
    'utf8',
  );
  const material = JSON.parse(raw) as {
    capability_schema: Record<string, unknown>;
    offerings: { carrier_schema: Record<string, unknown> }[];
  };
  return {
    capability_schema: material.capability_schema,
    carrier_schema: material.offerings[0].carrier_schema,
  };
}

/// **加一个网关模型不需要手写发布命令**（Spec V-D8）。
///
/// 这条判据只有在真浏览器里才成立：要证明的是"运营能用表单把它发出去"，而不是"代码里有个表单组件"。
/// 所以这里从贴合同一路填到发布，最后核对**发布真的成功了**——用平台的答复与模型列表说话。
///
/// **这条验的是工程师那条路**（贴整份技术定义：合同、承载面、渠道三要素）。它现在收在抽屉底部的折叠区里
/// （运营的主路径是"选厂商、勾供给"，见 `platform-model-publish.spec.ts`），但过渡期仍然可用（`0012` §7），
/// 所以这条 spec 保留，只是多一步把折叠区展开。
///
/// 渠道地址填回环上的桩：发布只校验形状与折算率，不真的调用渠道。
test('用表单发布一个新型号，不写整份发布命令', async ({ page, request }) => {
  await page.goto(consoleUrl);
  await page.getByTestId('admin-email').fill(settings.adminEmail);
  await page.getByTestId('admin-password').fill(settings.adminPassword);
  await page.getByTestId('admin-sign-in').click();
  await page.locator('.ant-layout-sider').getByRole('menuitem', { name: '模型目录' }).waitFor();

  // 发布要用的折算率先录好：没有它，按 token 计量的候选会在校验期被拒。
  const rate = await request.put(`${adminApiUrl}/api/v1/fx-rates`, {
    headers: { authorization: `Bearer ${settings.adminToken}` },
    data: { currency: 'USD', rate_micros: 7_100_000 },
  });
  expect(rate.status()).toBe(204);

  // 上架入口在「模型目录」页右上角，表单开在抽屉里（两页已合成一页）。
  await page.getByTestId('models-add').click();
  // **展开工程师那条路**：运营的主路径不给技术字段，所以贴整份定义的表单默认收起。
  await page.getByRole('button', { name: /工程师：贴整份发布定义/ }).click();
  await expect(page.getByTestId('publish-vendor')).toBeVisible();
  // 界面上要说明白"这里填的就是对外价"、而仓库里的素材只是夹具（Spec §6 的那条约束）：
  // 少了这句，读到 `config/bootstrap/*.json` 的人会以为生产价已经定好了。
  await expect(page.getByText(/夹具值，不是生产价/)).toBeVisible();

  const model = `e2e-form-${Date.now()}`;
  await page.getByTestId('publish-vendor').fill('OpenAI');
  await page.getByTestId('publish-native-model').fill(model);
  await page.getByTestId('publish-native-revision').fill('e2e-contract-1.0');
  await page.getByTestId('publish-markup-bps').fill('2000');

  // 候选：渠道、模型名、驱动器、地址与凭证变量名。按 `data-testid` 定位而不是按标签文案——
  // "渠道" 与 "渠道模型名" 这种前缀关系用文案会命中多个。
  await page.getByTestId('publish-provider-kind').fill('AIHubMix');
  await page.getByTestId('publish-provider-model').fill(model);
  await page.getByTestId('publish-adapter-key').fill('aihubmix-image-v1');
  await page.getByTestId('publish-base-url').fill('http://127.0.0.1:9/stub');
  await page.getByTestId('publish-credential-env').fill('E2E_DUMMY_KEY');
  // 渠道费率的出处是必填：对账时要能回去核对当初是从哪一页抄的。
  await page.getByTestId('publish-plan-source-url').fill('https://vendor.example.com/pricing');

  // 给了倍率就必须让候选带一份对客价（按 token 计量的候选随修订带四档 CNY 费率）——
  // 否则发布期会说"markup_bps 给了，但没有候选消费它"。
  await page.getByRole('button', { name: '带上对客价向量' }).click();
  const cny = [
    ['publish-cny-text-input', 42_600_000],
    ['publish-cny-image-input', 68_160_000],
    ['publish-cny-text-output', 85_200_000],
    ['publish-cny-image-output', 255_600_000],
  ] as const;
  for (const [testId, value] of cny) {
    await page.getByTestId(testId).fill(String(value));
  }

  // 合同（capability_schema）：厂商给的 JSON Schema，**模型级一份**。这是唯一必须贴的部分——
  // JSON Schema 收集不进表单，而它是厂商文档。`model.const` 要指向这次发布的型号。
  const material = bootstrapMaterial();
  const capability = {
    ...material.capability_schema,
    properties: {
      ...(material.capability_schema.properties as Record<string, unknown>),
      model: { const: model },
    },
  };
  await page.locator('textarea').last().fill(JSON.stringify(capability));

  // 承载面（carrier_schema）：**候选级**，是 Driver 校验的对象；它还要与合同对得上（承载面不能声明
  // 合同里没有的字段）。同样是贴厂商/渠道给的那一份。
  await page.getByText('技术字段（贴 JSON；留空即不带）').click();
  const carrier = {
    ...material.carrier_schema,
    properties: {
      ...(material.carrier_schema.properties as Record<string, unknown>),
      model: { const: model },
    },
  };
  await page
    .locator('.ant-form-item')
    .filter({ hasText: '承载面 carrier_schema' })
    .locator('textarea')
    .fill(JSON.stringify(carrier));

  await page.getByTestId('publish-submit').click();

  // 发布成功：平台的答复里带出网关模型名与生效修订标识。
  await expect(page.getByText(`已发布 ${model}`).first()).toBeVisible({ timeout: 15_000 });
  await expect(page.getByText('生效修订：')).toBeVisible();

  // 发布成功后关掉抽屉，列表上应当立刻有它（列表在发布成功时重取过）。
  await page.keyboard.press('Escape');
  await expect(page.getByTestId('models-add')).toBeVisible();
  await expect(page.getByText(model).first()).toBeVisible({ timeout: 15_000 });
});
