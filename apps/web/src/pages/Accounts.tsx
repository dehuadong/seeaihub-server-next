import { useState } from 'react';
import type { AdminClient } from '../client';
import { Page } from '../ui';
import { when, yuan } from '../routes';

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
    </Page>
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
