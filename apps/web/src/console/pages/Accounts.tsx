import { useState } from 'react';
import type { AdminClient } from '../client';
import type { CustomerView } from '../../shared/types';
import { Page } from '../../shared/ui';
import { when, yuan } from '../../shared/routes';

/// 账户与密钥。建账户、充值、改标签、发/吊销密钥，以及看余额与流水。
///
/// 账户 id 靠**粘贴**而不是下拉列表：管理 API 今天没有"列账户"这一条（`GET /api/v1/accounts` 只有
/// `POST`），所以这一页不假装能列出它。建账户或充值之后把 id 复制过来即可。
export function AccountsPage({ client }: { client: AdminClient }) {
  const [accountId, setAccountId] = useState('');
  const [balance, setBalance] = useState<{ balance_microusd: number; updated_at: string } | null>(null);
  const [entries, setEntries] = useState<{ kind: string; amount_microusd: number; job_id: string | null; created_at: string }[]>([]);
  const [truncated, setTruncated] = useState(false);
  const [issued, setIssued] = useState<{ api_key: string; key_id: string } | null>(null);
  const [revokeId, setRevokeId] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  async function run(action: () => Promise<void>) {
    setError(null);
    setNotice(null);
    try {
      await action();
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    }
  }

  const read = (id: string) =>
    run(async () => {
      const [next, ledger] = await Promise.all([client.accountBalance(id), client.accountEntries(id, 50)]);
      setBalance(next);
      setEntries(ledger.entries);
      setTruncated(ledger.truncated);
    });

  return (
    <Page title="账户与密钥" error={error}>
      <div className="panel">
        <h3>查一个账户</h3>
        <div className="row">
          <input
            data-testid="accounts-lookup-id"
            value={accountId}
            onChange={(event) => setAccountId(event.target.value)}
            placeholder="账户 id（UUID）"
            size={40}
          />
          <button type="button" disabled={!accountId.trim()} onClick={() => read(accountId.trim())}>
            读余额与流水
          </button>
        </div>
        <p className="muted">读的是账本那一行，不读缓存——运营对账看的就是它。</p>
        {balance ? (
          <div className="grid" style={{ marginTop: 8 }}>
            <div className="field">
              <span>余额</span>
              <div>
                {yuan(balance.balance_microusd)} 元（{balance.balance_microusd} 微单位）
              </div>
            </div>
            <div className="field">
              <span>写入时刻</span>
              <div>{when(balance.updated_at)}</div>
            </div>
          </div>
        ) : null}
        {entries.length > 0 ? (
          <>
            <table style={{ marginTop: 10 }}>
              <thead>
                <tr>
                  <th>时刻</th>
                  <th>类别</th>
                  <th>金额（元）</th>
                  <th>Job</th>
                </tr>
              </thead>
              <tbody>
                {entries.map((entry, index) => (
                  <tr key={`${entry.created_at}-${index}`}>
                    <td>{when(entry.created_at)}</td>
                    <td>{entry.kind}</td>
                    <td>{yuan(entry.amount_microusd)}</td>
                    <td className="mono">{entry.job_id ?? '—'}</td>
                  </tr>
                ))}
              </tbody>
            </table>
            {truncated ? <p className="muted">只显示最近 50 条（按时间倒序）。</p> : null}
          </>
        ) : null}
      </div>

      <div className="panel">
        <h3>建账户 / 充值</h3>
        <CreateAccount client={client} onCreated={(id) => setAccountId(id)} onError={setError} onNotice={setNotice} />
        <Credit client={client} accountId={accountId} onError={setError} onNotice={setNotice} />
        <Tag client={client} accountId={accountId} onError={setError} onNotice={setNotice} />
      </div>

      <div className="panel">
        <h3>API Key</h3>
        <div className="row">
          <button
            type="button"
            disabled={!accountId.trim()}
            onClick={() =>
              run(async () => {
                const created = await client.issueApiKey(accountId.trim(), 'admin-console');
                setIssued(created);
              })
            }
          >
            给该账户发一把密钥
          </button>
          <span className="muted">明文只在这一次响应里出现，事后谁也拿不回来。</span>
        </div>
        {issued ? (
          <div className="panel" style={{ marginTop: 8 }}>
            <div className="field">
              <span>密钥明文（现在抄走）</span>
              <div className="mono">{issued.api_key}</div>
            </div>
            <div className="field">
              <span>密钥标识（吊销用它）</span>
              <div className="mono">{issued.key_id}</div>
            </div>
          </div>
        ) : null}
        <div className="row" style={{ marginTop: 8 }}>
          <input value={revokeId} onChange={(event) => setRevokeId(event.target.value)} placeholder="要吊销的密钥标识" size={40} />
          <button
            type="button"
            disabled={!revokeId.trim()}
            onClick={() =>
              run(async () => {
                await client.revokeApiKey(revokeId.trim());
                setNotice('已吊销；吊销立刻生效（不删行）。');
              })
            }
          >
            吊销
          </button>
        </div>
      </div>

      {notice ? <p style={{ color: 'var(--ok)' }}>{notice}</p> : null}

      <CustomersPanel client={client} onError={setError} onNotice={setNotice} />
    </Page>
  );
}

/// 客户登录身份：替客户开户、按邮箱找账户、签一次性重置令牌。
///
/// 三件事都要先有账户标识：客户自助注册出来的账户只存在于客户表里，运营不查就找不到它——没有这一块，
/// "给新客户充值""帮忘了口令的客户重置"都没有入口。
function CustomersPanel(props: {
  client: AdminClient;
  onError: (message: string) => void;
  onNotice: (message: string) => void;
}) {
  const [email, setEmail] = useState('');
  const [password, setPassword] = useState('');
  const [customerAccountId, setCustomerAccountId] = useState('');
  const [found, setFound] = useState<CustomerView | null>(null);
  const [issued, setIssued] = useState<{ reset_token: string; expires_at: string } | null>(null);

  async function run(action: () => Promise<void>) {
    try {
      await action();
    } catch (failure) {
      props.onError(failure instanceof Error ? failure.message : String(failure));
    }
  }

  return (
    <div className="panel">
      <h3>客户登录身份</h3>
      <p className="muted">
        一个客户邮箱对应一个账户。**不填账户标识就新建一个空账户**；填了就把它配到那个已有账户上
        （配身份不动余额、密钥与历史）。不填口令时运营改用重置令牌让客户自己设。
      </p>
      <div className="row" style={{ marginTop: 8 }}>
        <label className="field">
          <span>客户邮箱</span>
          <input value={email} onChange={(event) => setEmail(event.target.value)} size={26} />
        </label>
        <label className="field">
          <span>初始口令（至少 8 字符，可空）</span>
          <input
            type="password"
            value={password}
            onChange={(event) => setPassword(event.target.value)}
            size={22}
          />
        </label>
        <label className="field">
          <span>已有账户标识（可空）</span>
          <input
            value={customerAccountId}
            onChange={(event) => setCustomerAccountId(event.target.value)}
            size={36}
          />
        </label>
      </div>
      <div className="row" style={{ marginTop: 8 }}>
        <button
          type="button"
          disabled={!email.trim()}
          onClick={() =>
            run(async () => {
              const created = await props.client.openCustomer(
                email.trim(),
                password || undefined,
                customerAccountId.trim() || undefined,
              );
              setFound(created);
              props.onNotice(`已开户：${created.email} → 账户 ${created.account_id}`);
            })
          }
        >
          开户
        </button>
        <button
          type="button"
          disabled={!email.trim()}
          onClick={() =>
            run(async () => {
              const result = await props.client.findCustomer(email.trim());
              const first = result.customers[0];
              if (!first) {
                setFound(null);
                props.onNotice(`没有找到 ${email.trim()} 的登录身份`);
                return;
              }
              setFound(first);
              setCustomerAccountId(first.account_id);
            })
          }
        >
          按邮箱找账户
        </button>
      </div>

      {found ? (
        <div className="grid" style={{ marginTop: 8 }}>
          <div className="field">
            <span>客户</span>
            <div>{found.email}</div>
          </div>
          <div className="field">
            <span>账户</span>
            <div className="mono">{found.account_id}</div>
          </div>
          <div className="field">
            <span>上次登录</span>
            <div>{when(found.last_login_at)}</div>
          </div>
          <div className="field">
            <span>重置口令</span>
            <div>
              <button
                type="button"
                onClick={() =>
                  run(async () => {
                    const token = await props.client.issueCustomerPasswordReset(found.account_id);
                    setIssued(token);
                    props.onNotice('已签发一次性重置令牌——请当面或经既有渠道转交客户');
                  })
                }
              >
                签发重置令牌
              </button>
            </div>
          </div>
        </div>
      ) : null}

      {issued ? (
        <div className="panel" style={{ marginTop: 8 }}>
          <div className="field">
            <span>重置令牌（只显示这一次，转交客户后由他设置新口令）</span>
            <div className="mono">{issued.reset_token}</div>
          </div>
          <p className="muted">有效期至 {when(issued.expires_at)}；用过一次即失效。</p>
        </div>
      ) : null}
    </div>
  );
}

function CreateAccount(props: { client: AdminClient; onCreated: (id: string) => void; onError: (m: string) => void; onNotice: (m: string) => void }) {
  const [credit, setCredit] = useState('20');
  return (
    <div className="row" style={{ marginTop: 8 }}>
      <label className="field">
        <span>初始充值（元）</span>
        <input value={credit} onChange={(event) => setCredit(event.target.value)} size={10} />
      </label>
      <button
        type="button"
        onClick={async () => {
          try {
            const micros = Math.round(Number(credit) * 1_000_000);
            if (!Number.isFinite(micros) || micros < 0) throw new Error('初始充值必须是非负数');
            const created = await props.client.createAccount(micros);
            props.onCreated(created.account_id);
            props.onNotice(`已建账户 ${created.account_id}（初始 ${credit} 元）`);
          } catch (failure) {
            props.onError(failure instanceof Error ? failure.message : String(failure));
          }
        }}
      >
        建账户
      </button>
    </div>
  );
}

function Credit(props: { client: AdminClient; accountId: string; onError: (m: string) => void; onNotice: (m: string) => void }) {
  const [amount, setAmount] = useState('10');
  const [businessKey, setBusinessKey] = useState('');
  return (
    <div className="row" style={{ marginTop: 8 }}>
      <label className="field">
        <span>充值（元）</span>
        <input value={amount} onChange={(event) => setAmount(event.target.value)} size={10} />
      </label>
      <label className="field">
        <span>业务键（幂等；同一个键重放不会重复充值）</span>
        <input value={businessKey} onChange={(event) => setBusinessKey(event.target.value)} size={28} />
      </label>
      <button
        type="button"
        disabled={!props.accountId.trim() || !businessKey.trim()}
        onClick={async () => {
          try {
            const micros = Math.round(Number(amount) * 1_000_000);
            if (!Number.isFinite(micros) || micros <= 0) throw new Error('充值金额必须是正数');
            await props.client.creditAccount(props.accountId.trim(), micros, businessKey.trim());
            props.onNotice(`已充值 ${amount} 元`);
          } catch (failure) {
            props.onError(failure instanceof Error ? failure.message : String(failure));
          }
        }}
      >
        充值
      </button>
    </div>
  );
}

function Tag(props: { client: AdminClient; accountId: string; onError: (m: string) => void; onNotice: (m: string) => void }) {
  const [tag, setTag] = useState('');
  return (
    <div className="row" style={{ marginTop: 8 }}>
      <label className="field">
        <span>账户标签（空＝清除；只有账户标签策略消费它）</span>
        <input value={tag} onChange={(event) => setTag(event.target.value)} size={20} />
      </label>
      <button
        type="button"
        disabled={!props.accountId.trim()}
        onClick={async () => {
          try {
            await props.client.setAccountTag(props.accountId.trim(), tag.trim() || null);
            props.onNotice(tag.trim() ? `已设标签 ${tag.trim()}` : '已清除标签');
          } catch (failure) {
            props.onError(failure instanceof Error ? failure.message : String(failure));
          }
        }}
      >
        写入标签
      </button>
    </div>
  );
}
