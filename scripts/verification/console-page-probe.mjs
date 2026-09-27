// V-D7 的机器可核部分：把六个管理页面读的端点逐个打一遍，落成一份可复核的记录。
// 页面的每个取数都走这些端点（见 apps/web/src/console/client.ts），所以"页面能不能读出数"
// 在这里可以判定；"页面渲染得对不对"仍归人眼与截图。
const base = 'http://127.0.0.1:8090';
const token = 'delivery-shared-token';
const headers = { authorization: `Bearer ${token}`, 'content-type': 'application/json' };

async function call(method, path, body) {
  const response = await fetch(base + path, {
    method,
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await response.text();
  let parsed;
  try {
    parsed = JSON.parse(text);
  } catch {
    parsed = text;
  }
  return { status: response.status, body: parsed };
}

function summarize(value) {
  if (Array.isArray(value)) return `数组 ${value.length} 项`;
  if (value && typeof value === 'object') return `对象，键：${Object.keys(value).join(', ')}`;
  return JSON.stringify(value).slice(0, 80);
}

const account = await call('POST', '/api/v1/accounts', { initial_credit_microusd: 25_000_000 });
const accountId = account.body.account_id;
console.log(`建账户: ${account.status} ${accountId}`);

const customer = await call('POST', '/api/v1/customers', {
  email: 'ops-acceptance@example.com',
  password: 'acceptance-pass-1',
  account_id: accountId,
});
console.log(`替客户开户: ${customer.status} ${summarize(customer.body)}`);

const rate = await call('PUT', '/api/v1/fx-rates', {
  currency: 'USD',
  rate_micros: 7_100_000,
});
console.log(`录折算率: ${rate.status}`);

const pages = [
  ['模型目录', '/api/v1/gateway-models'],
  ['折算率', '/api/v1/fx-rates'],
  ['路由策略', '/api/v1/route-policies'],
  ['账户', `/api/v1/accounts?limit=20`],
  ['账户详情', `/api/v1/accounts/${accountId}`],
  ['账户流水', `/api/v1/accounts/${accountId}/entries?limit=20`],
  ['客户', '/api/v1/customers?limit=20'],
  ['对账与诊断 · 对账案例', '/api/v1/reconciliation-cases'],
  ['对账与诊断 · 平台侧失败', '/api/v1/provider-failures?limit=20'],
  ['对账与诊断 · 成本缺口', '/api/v1/provider-cost-gaps?limit=20'],
];

console.log('\n页面端点逐条:');
for (const [page, path] of pages) {
  const result = await call('GET', path);
  console.log(`  ${page.padEnd(22)} ${path.padEnd(46)} -> ${result.status}  ${summarize(result.body)}`);
}

const models = await call('GET', '/api/v1/gateway-models');
const names = (models.body.gateway_models ?? []).map((model) => model.gateway_model);
console.log(`\n模型目录里的对客名: ${names.join(', ') || '（空）'}`);
