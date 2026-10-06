import { test } from '@playwright/test';
import type { APIRequestContext, Page } from '@playwright/test';
import { adminApiUrl, consoleUrl, portalUrl, settings } from './settings';

/// 给两套界面的各个页面抓整页截图，落到 `apps/web/screenshots/`。
///
/// 它是**给人看的留档**：界面好不好看、间距对不对，是 spec 断言不了的（根 `AGENTS.md` 的
/// 「浏览器行为」把这一条划给人眼）。所以这里不做断言，只负责把"现在长什么样"固定下来。
///
/// 它与 e2e 分开配置（`playwright.capture.config.ts`）：`npm run e2e` 只跑断言，`npm run capture`
/// 只抓图。合成一份的话抓图会混进常规运行、白花时间还把用例总数搞乱。
const CONSOLE_PAGES = ['模型目录', '账户', '客户', '对账与诊断', '折算率', '路由策略'] as const;

test('运营后台六个页面各抓一张截图', async ({ page, request }) => {
  // 先造一点数据，免得每页都是空态：一个币种的折算率 + 一个带余额的账户。
  await request.put(`${adminApiUrl}/api/v1/fx-rates`, {
    headers: { authorization: `Bearer ${settings.adminToken}` },
    data: { currency: 'USD', rate_micros: 7_100_000 },
  });
  await request.post(`${adminApiUrl}/api/v1/accounts`, {
    headers: { authorization: `Bearer ${settings.adminToken}` },
    data: { initial_credit_microusd: 25_000_000 },
  });

  await page.goto(consoleUrl);
  await page.screenshot({ path: 'screenshots/00-运营后台-登录页.png', fullPage: true });
  await page.getByTestId('admin-email').fill(settings.adminEmail);
  await page.getByTestId('admin-password').fill(settings.adminPassword);
  await page.getByTestId('admin-sign-in').click();
  await page.locator('.ant-layout-sider').getByRole('menuitem', { name: '模型目录' }).waitFor();

  // **先造数据再看页**：可选供给（运营那条路的清单来源）与一条已上架的型号。放到挨着页面循环之前，
  // 否则「模型目录」抓到的是一张空态图——空态也是真的，但它固定不住"目录里有什么"这个最要紧的一屏。
  await publishCaptureFixture(request);
  // 重新加载一次：页面在**第一次进入**时就取了数据，那时夹具还没发布；而点一个已经在的菜单项不会
  // 重新拉。不重载的话后面那张图仍旧是空态，而且**看起来完全正常**——这正是抓图最容易骗过人的地方。
  await page.reload();
  await page.locator('.ant-layout-sider').getByRole('menuitem', { name: '模型目录' }).waitFor();
  // 抓图前先把"这一屏应该有内容"钉住：图是给人看的，静默抓到空态没人会发现。
  await assertCatalogHasFixture(page);

  for (const [index, label] of CONSOLE_PAGES.entries()) {
    await page.locator('.ant-layout-sider').getByRole('menuitem', { name: label }).click();
    await page.waitForTimeout(700);
    await page.screenshot({
      path: `screenshots/0${index + 1}-运营后台-${label}.png`,
      fullPage: true,
    });
  }

  // 「上架新模型」的表单开在模型目录的抽屉里，它也是要给人看的一屏。
  //
  // **画的是运营那条路**（选厂商 → 勾供给 → 给价）：它是运营实际看到的界面，留档就该固定这一屏。
  // 工程师那条"贴整份发布定义"的老路收在抽屉底部的折叠区里，默认收起，不在这一屏里。
  //
  // 但这条路的清单来自**库里现成的供给**，而抓图用的是空库、也没有素材导入——不先造一条，
  // 截出来的就是"这个厂商下还没有配好的供给"那句提示，等于没固定住界面。供给已经在页面循环之前
  // 由 `publishCaptureFixture` 造好，这里直接打开运营的抽屉：勾一条，价与折算率那一屏也就跟着出来了。
  await page.locator('.ant-layout-sider').getByRole('menuitem', { name: '模型目录' }).click();
  await page.getByTestId('models-add').click();
  await page.getByTestId('platform-name').fill('gpt-image-2.5-plus');
  await page.getByTestId('platform-vendor').click();
  await page.getByTitle(CAPTURE_VENDOR).click();
  await page.getByTestId(`platform-pick-${CAPTURE_CHANNEL}-${CAPTURE_PROVIDER_MODEL}`).check();
  await page.waitForTimeout(700);
  await page.screenshot({ path: 'screenshots/07-运营后台-上架新模型.png', fullPage: true });
});

/// 抓图用的供应商身份。用**一眼看得出是夹具**的名字：留档图会被人当成真实配置看，越像真的越容易误导。
const CAPTURE_VENDOR = 'OpenAI';
const CAPTURE_CHANNEL = 'ShotChannel';
const CAPTURE_PROVIDER_MODEL = 'shot-upstream-model';
/// 夹具发布的那个平台模型名。抓图前用它确认目录真的画出了内容。
const CAPTURE_GATEWAY_MODEL = 'shot-gateway-model';

/// 确认「模型目录」画的是**有内容**的那一屏。
///
/// 抓图不做断言的话，一次静默的空态就会变成"留档"，而看图的人以为目录本来就长这样。这里只钉一件
/// 事：夹具那个名字在页面上——它不在，说明数据没到或者页面没重拉，那这张图就不能要。
async function assertCatalogHasFixture(page: Page): Promise<void> {
  await page
    .getByText(CAPTURE_GATEWAY_MODEL)
    .first()
    .waitFor({ timeout: 15_000 });
}

/// 造一条**可被运营选中**的供给（内联形状，即工程师那条路）。
///
/// 为什么抓图也要造数据：可选清单是"库里现成的供给"，空库抓出来只有一句空态提示，固定不住界面。
/// 用的渠道身份、地址与凭证变量名都是**占位值**，不会有人真的去调它。
async function publishCaptureFixture(request: APIRequestContext): Promise<void> {
  const vendorModel = 'shot-vendor-model';
  const capability = {
    type: 'object',
    additionalProperties: false,
    required: ['model', 'prompt'],
    properties: {
      model: { const: vendorModel },
      prompt: { type: 'string', minLength: 1 },
    },
  };
  const response = await request.post(`${adminApiUrl}/api/v1/runtime-revisions`, {
    headers: { authorization: `Bearer ${settings.adminToken}` },
    data: {
      vendor_id: CAPTURE_VENDOR,
      native_model_id: vendorModel,
      gateway_model: CAPTURE_GATEWAY_MODEL,
      native_revision: 'shot-1',
      type: 'image',
      actor: 'capture',
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
          provider_kind: CAPTURE_CHANNEL,
          adapter_key: 'apimart-image-v1',
          provider_model_id: CAPTURE_PROVIDER_MODEL,
          base_url: 'https://shot-placeholder.example.com',
          credential_env: 'SHOT_PLACEHOLDER_KEY',
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
            source_url: 'https://shot-placeholder.example.com/pricing',
          },
          consumer_rates_cny: {
            text_input_micros_per_million: 9_100_000,
            image_input_micros_per_million: 14_500_000,
            text_output_micros_per_million: 18_200_000,
            image_output_micros_per_million: 54_600_000,
          },
          cost_currency: 'USD',
        },
      ],
    },
  });
  if (!response.ok()) {
    throw new Error(`抓图夹具发布失败：${response.status()} ${await response.text()}`);
  }
}

test('客户控制台的登录页与控制台各抓一张截图', async ({ page }) => {
  await page.goto(portalUrl);
  await page.screenshot({ path: 'screenshots/10-客户控制台-登录页.png', fullPage: true });

  await page.getByTestId('portal-mode-register').click();
  await page.getByTestId('portal-email').fill(`shot-${Date.now()}@example.com`);
  await page.getByTestId('portal-password').fill('a-long-enough-password');
  await page.getByTestId('portal-submit').click();
  // 控制台渲染出来的标志是首屏那三个数（不依赖当前停在哪个标签页）。
  await page.locator('.ant-statistic').first().waitFor();
  await page.waitForTimeout(700);
  await page.screenshot({ path: 'screenshots/11-客户控制台-控制台.png', fullPage: true });
});
