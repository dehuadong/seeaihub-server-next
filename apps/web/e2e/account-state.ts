import { randomUUID } from 'node:crypto';
import postgres from 'postgres';
import { settings } from './settings';

/// 把 Worker 收尾事务的**结果**直接摆进 e2e 库。
///
/// e2e 只起 API、**不起 Worker**，所以"成功结算扣 20 元"与"只释放预授权"这两个事务在浏览器用例里
/// 跑不出来（要起 Worker 与假上游）。事务本身由 `apps/api/tests/http_contract/cases_lifecycle.rs`
/// 用真 Worker + 假上游覆盖；这里只摆结果，让浏览器用例能验"页面读的是结算/释放之后的余额"。
/// 连接用 e2e 自己的 `postgres` 客户端——与 `ensure-database.mjs` 同一个依赖，不引入新装置。
///
/// **只摆页面读数依赖的那几处**：`ledger.accounts` 的余额/占用/版本、`ledger.holds` 的状态、
/// `ledger.entries` 的 `capture`（口径见 `docs/design/0013-account-funds-and-reservations.md`
/// §2.3–§2.4）。真收尾事务还会改 `generation.jobs` 的状态与终态时刻、清租约并 bump
/// `updated_at`——这些夹具**不碰**，所以它摆出来的不是真系统会有的
/// 完整状态，只够验"页面读的是结算/释放之后的余额"。

/// 每条用例只期望**恰好一条** active Hold：多了说明夹具没摆干净，宁可报错也不猜哪一条。
async function soleActiveHoldJob(tx: postgres.TransactionSql, accountId: string): Promise<string> {
  const rows = await tx<{ job_id: string }[]>`
    SELECT job_id FROM ledger.holds WHERE account_id = ${accountId} AND status = 'active'
  `;
  if (rows.length !== 1) {
    throw new Error(`expected exactly one active hold for ${accountId}, got ${rows.length}`);
  }
  return rows[0].job_id;
}

async function withTransaction(
  work: (tx: postgres.TransactionSql) => Promise<void>,
): Promise<void> {
  const sql = postgres(settings.database, { max: 1, onnotice: () => {} });
  try {
    await sql.begin(work);
  } finally {
    await sql.end();
  }
}

/// 成功结算：已结算余额减 `chargeMicrousd`、占用减该 Job 的预授权额，Hold 转 `captured`，
/// 并写一条指向该 Job 的 `capture` 流水。
export async function captureActiveHold(accountId: string, chargeMicrousd: number): Promise<void> {
  await withTransaction(async (tx) => {
    const jobId = await soleActiveHoldJob(tx, accountId);
    await tx`
      UPDATE ledger.accounts
      SET balance_microusd = balance_microusd - ${chargeMicrousd},
          held_microusd = held_microusd - (SELECT amount_microusd FROM ledger.holds WHERE job_id = ${jobId}),
          version = version + 1
      WHERE id = ${accountId}
    `;
    await tx`UPDATE ledger.holds SET status = 'captured', updated_at = now() WHERE job_id = ${jobId}`;
    await tx`
      INSERT INTO ledger.entries (id, account_id, job_id, kind, amount_microusd, business_key)
      VALUES (${randomUUID()}, ${accountId}, ${jobId}, 'capture', ${-chargeMicrousd}, ${'e2e-capture-' + jobId})
    `;
  });
}

/// 只释放预授权：占用减该 Job 的预授权额，Hold 转 `released`；已结算余额与资金流水都不动。
export async function releaseActiveHold(accountId: string): Promise<void> {
  await withTransaction(async (tx) => {
    const jobId = await soleActiveHoldJob(tx, accountId);
    await tx`
      UPDATE ledger.accounts
      SET held_microusd = held_microusd - (SELECT amount_microusd FROM ledger.holds WHERE job_id = ${jobId}),
          version = version + 1
      WHERE id = ${accountId}
    `;
    await tx`UPDATE ledger.holds SET status = 'released', updated_at = now() WHERE job_id = ${jobId}`;
  });
}

/// 注册用的客户邮箱对应的账户 id。
///
/// 浏览器用例只认得自己填的邮箱；要往这个账户摆状态就得从库里反查它——比从界面上抄 id 稳，
/// 也不把"设置页怎么显示 id"变成用例的前置。
export async function accountIdOfEmail(email: string): Promise<string> {
  const sql = postgres(settings.database, { max: 1, onnotice: () => {} });
  try {
    const rows = await sql<{ account_id: string }[]>`
      SELECT account_id FROM identity.customers WHERE email = ${email}
    `;
    if (rows.length !== 1) {
      throw new Error(`expected exactly one customer for ${email}, got ${rows.length}`);
    }
    return rows[0].account_id;
  } finally {
    await sql.end();
  }
}

/// 给这个账户摆出**多于一页**的真实流水：`count` 条 `capture`，外加一条正式调整（`adjustment`）。
///
/// 浏览器用例要验"一页放不下时能继续查看"，而 e2e 不起 Worker，多于一页的历史造不出来。这里只摆
/// 翻页读数依赖的那一处：`ledger.entries`（真收尾事务还会动余额、Hold 与日累计，翻页用例不读它们）。
///
/// **只摆流水，不摆调用记录**：后者要往 `generation.jobs` 插行，而 jobs 的外键指向渠道、型号、
/// Offering 与 Runtime Revision——e2e 故意让供给为空（`platform-model-empty-supply.spec.ts` 就验这个），
/// 插供给行会把它弄红。多于一页的**已结束请求**由 `apps/api/tests/http_contract/cases_customer_history.rs`
/// 在真库上验；翻了页的那段前端状态机与流水页是同一处 `useCursorPage`。
export async function seedLedgerPage(accountId: string, count: number): Promise<void> {
  await withTransaction(async (tx) => {
    const rows = await tx`
      INSERT INTO ledger.entries (id, account_id, kind, amount_microusd, business_key, created_at)
      SELECT gen_random_uuid(), ${accountId}, 'capture', -20000,
             ${'e2e-history-capture-' + randomUUID() + '-'} || g,
             now() - (g || ' minutes')::interval
      FROM generate_series(1, ${count}) g
      RETURNING id
    `;
    if (rows.length !== count) {
      throw new Error(`expected ${count} seeded ledger entries, got ${rows.length}`);
    }
    // 一条正式调整：类别筛选要能把"充值"与"运营调整"分开，夹具里就得两种都有。
    await tx`
      INSERT INTO ledger.entries (id, account_id, kind, amount_microusd, business_key)
      VALUES (${randomUUID()}, ${accountId}, 'adjustment', 5000,
              ${'e2e-history-adjustment-' + randomUUID()})
    `;
  });
}
