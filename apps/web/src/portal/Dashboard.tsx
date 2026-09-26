import { useState } from 'react';
import type { CustomerClient } from './client';
import { useCustomerSession } from './session';
import { yuan, when } from '../shared/routes';
import { useLoadable } from '../shared/ui';

/// 客户控制台的主体：余额与持有、密钥自助、用量与账单（Spec C5、C7–C10）。
///
/// 每个板块各取各的数据，一个板块失败不影响别的；金额与时间只在展示层换算，判断都用服务端给的数。
export function Dashboard({ client }: { client: CustomerClient }) {
  const { email, accountId, signOut } = useCustomerSession();

  return (
    <div className="main">
      <header>
        <h2>seeai 控制台</h2>
        <div className="row">
          <span className="hint">
            {email}（账户 {accountId}）
          </span>
          <button type="button" onClick={signOut}>
            退出登录
          </button>
        </div>
      </header>
      <AccountPanel client={client} />
      <KeysPanel client={client} />
      <UsagePanel client={client} />
    </div>
  );
}

/// 余额与持有中。**两个数分开**：持有中是已预授权、还没扣的部分，不是可用额的一部分。
function AccountPanel({ client }: { client: CustomerClient }) {
  const account = useLoadable(() => client.account(), [client]);
  const ledger = useLoadable(() => client.ledger(20), [client]);

  return (
    <div className="panel">
      <h3>余额与持有</h3>
      {account.error ? <p className="error">{account.error}</p> : null}
      {account.loading ? <p className="muted">读取中…</p> : null}
      {account.data ? (
        <div className="grid">
          <div className="field">
            <span>可用余额</span>
            <div>{yuan(account.data.balance_microusd)} 元</div>
          </div>
          <div className="field">
            <span>持有中（已预授权、还没结算）</span>
            <div>{yuan(account.data.held_microusd)} 元</div>
          </div>
          <div className="field">
            <span>写入时刻</span>
            <div>{when(account.data.updated_at)}</div>
          </div>
        </div>
      ) : null}

      <h3 style={{ marginTop: 12 }}>充值记录与账目流水</h3>
      {ledger.error ? <p className="error">{ledger.error}</p> : null}
      {ledger.data && ledger.data.entries.length === 0 ? (
        <p className="muted">还没有任何账目。充值由运营在后台完成，完成之后这里会出现一条 credit。</p>
      ) : null}
      {ledger.data && ledger.data.entries.length > 0 ? (
        <table>
          <thead>
            <tr>
              <th>时刻</th>
              <th>类别</th>
              <th>金额（元）</th>
            </tr>
          </thead>
          <tbody>
            {ledger.data.entries.map((entry, index) => (
              <tr key={`${entry.created_at}-${index}`}>
                <td>{when(entry.created_at)}</td>
                <td>{entry.kind}</td>
                <td>{yuan(entry.amount_microusd)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : null}
      <p className="muted">
        <button type="button" onClick={() => { account.reload(); ledger.reload(); }}>
          重取
        </button>
      </p>
    </div>
  );
}

/// 密钥自助：列、建、吊销。明文只在创建那一次出现。
function KeysPanel({ client }: { client: CustomerClient }) {
  const keys = useLoadable(() => client.apiKeys(), [client]);
  const [label, setLabel] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [issued, setIssued] = useState<{ api_key: string; key_id: string } | null>(null);

  async function run(action: () => Promise<void>) {
    setBusy(true);
    setError(null);
    try {
      await action();
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="panel">
      <h3>API Key</h3>
      {error ?? keys.error ? <p className="error">{error ?? keys.error}</p> : null}
      <div className="row">
        <label className="field">
          <span>标签（给自己认的，例如 "本地脚本"）</span>
          <input value={label} onChange={(event) => setLabel(event.target.value)} size={24} />
        </label>
        <button
          type="button"
          disabled={busy || !label.trim()}
          onClick={() =>
            run(async () => {
              const created = await client.issueApiKey(label.trim());
              setIssued(created);
              setLabel('');
              keys.reload();
            })
          }
        >
          新建密钥
        </button>
      </div>
      {issued ? (
        <div className="panel" style={{ marginTop: 8 }}>
          <div className="field">
            <span>密钥明文——**只显示这一次**，现在就抄走</span>
            <div className="mono">{issued.api_key}</div>
          </div>
          <p className="muted">密钥标识：<code>{issued.key_id}</code>（吊销用它）</p>
        </div>
      ) : null}
      {keys.data && keys.data.keys.length === 0 ? (
        <p className="muted">还没有密钥。建一把之后才能调用生成接口。</p>
      ) : null}
      {keys.data && keys.data.keys.length > 0 ? (
        <table>
          <thead>
            <tr>
              <th>标签</th>
              <th>创建时间</th>
              <th>状态</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {keys.data.keys.map((key) => (
              <tr key={key.key_id}>
                <td>{key.label}</td>
                <td>{when(key.created_at)}</td>
                <td>
                  <span className={key.revoked_at ? 'tag off' : 'tag ok'}>
                    {key.revoked_at ? `已吊销（${when(key.revoked_at)}）` : '可用'}
                  </span>
                </td>
                <td>
                  {key.revoked_at ? null : (
                    <button
                      type="button"
                      disabled={busy}
                      onClick={() =>
                        run(async () => {
                          await client.revokeApiKey(key.key_id);
                          keys.reload();
                        })
                      }
                    >
                      吊销
                    </button>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : null}
    </div>
  );
}

/// 对客状态的展示文案。取值由服务端收敛，界面只翻译，不自己判断含义。
function statusLabel(status: 'succeeded' | 'failed' | 'pending' | 'canceled'): string {
  switch (status) {
    case 'succeeded':
      return '成功';
    case 'failed':
      return '未产出';
    case 'canceled':
      return '已取消';
    default:
      return '处理中';
  }
}

/// 用量与账单：逐笔明细按时间倒序、汇总按区间全量。
function UsagePanel({ client }: { client: CustomerClient }) {
  const usage = useLoadable(() => client.usage(50), [client]);
  const billing = useLoadable(() => client.billing(), [client]);

  return (
    <div className="panel">
      <h3>用量与账单</h3>
      {billing.error ?? usage.error ? <p className="error">{billing.error ?? usage.error}</p> : null}
      {billing.data ? (
        <div className="grid">
          <div className="field">
            <span>请求数（全部）</span>
            <div>{billing.data.requests}</div>
          </div>
          <div className="field">
            <span>产出图片数</span>
            <div>{billing.data.images}</div>
          </div>
          <div className="field">
            <span>扣费总额</span>
            <div>{yuan(billing.data.charged_microusd)} 元</div>
          </div>
        </div>
      ) : null}
      <p className="muted">
        汇总按整段区间**全量**计算，不随下面明细的条数变化。
      </p>
      {usage.data && usage.data.usage.length === 0 ? (
        <p className="muted">还没有任何调用记录。</p>
      ) : null}
      {usage.data && usage.data.usage.length > 0 ? (
        <>
          <table>
            <thead>
              <tr>
                <th>时刻</th>
                <th>型号</th>
                <th>类别</th>
                <th>状态</th>
                <th>张数</th>
                <th>扣费（元）</th>
              </tr>
            </thead>
            <tbody>
              {usage.data.usage.map((row, index) => (
                <tr key={`${row.created_at}-${index}`}>
                  <td>{when(row.created_at)}</td>
                  <td>{row.gateway_model}</td>
                  <td>{row.kind === 'edit' ? '图片编辑' : '同步生成'}</td>
                  <td>{statusLabel(row.status)}</td>
                  <td>{row.image_count}</td>
                  <td>{yuan(row.charged_microusd)}</td>
                </tr>
              ))}
            </tbody>
          </table>
          {usage.data.truncated ? <p className="muted">只显示最近 50 条。</p> : null}
        </>
      ) : null}
      <p className="muted">
        <button type="button" onClick={() => { usage.reload(); billing.reload(); }}>
          重取
        </button>
      </p>
    </div>
  );
}
