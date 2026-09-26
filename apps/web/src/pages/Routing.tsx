import { useState } from 'react';
import type { AdminClient } from '../client';
import type { RouteStrategy } from '../types';
import { Page, useLoadable } from '../ui';

const STRATEGIES: { value: RouteStrategy; label: string }[] = [
  { value: 'priority_failover', label: '按档位顺序（默认）' },
  { value: 'weighted_random', label: '按权重随机' },
  { value: 'least_cost', label: '按折后成本最小' },
  { value: 'user_tag', label: '按账户标签映射' },
];

/// 路由策略：**在合格候选里挑哪一条**由运营配置。它不进不可变修订，改它即刻影响之后的受理；
/// 已受理的 Job 的候选与定价早已随快照冻结，不受影响。
export function RoutingPage({ client }: { client: AdminClient }) {
  const policies = useLoadable(() => client.routePolicies(), [client]);
  const [scope, setScope] = useState('');
  const [strategy, setStrategy] = useState<RouteStrategy>('priority_failover');
  const [discounts, setDiscounts] = useState('');
  const [tags, setTags] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function submit() {
    setBusy(true);
    setError(null);
    try {
      await client.upsertRoutePolicy(
        scope.trim() || null,
        strategy,
        parseNumberMap(discounts, '折扣率'),
        parseStringMap(tags, '标签映射'),
      );
      policies.reload();
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Page title="路由策略" hint="不配置＝按档位顺序，即零配置行为" error={error ?? policies.error} loading={policies.loading} onReload={policies.reload}>
      <div className="panel">
        <h3>写入（或覆盖）一条策略</h3>
        <div className="row">
          <label className="field">
            <span>作用域（空＝全局）</span>
            <input value={scope} onChange={(event) => setScope(event.target.value)} placeholder="gpt-image-2.5-flare" size={26} />
          </label>
          <label className="field">
            <span>策略</span>
            <select value={strategy} onChange={(event) => setStrategy(event.target.value as RouteStrategy)}>
              {STRATEGIES.map((item) => (
                <option key={item.value} value={item.value}>
                  {item.label}
                </option>
              ))}
            </select>
          </label>
        </div>
        <div className="row" style={{ marginTop: 8 }}>
          <label className="field">
            <span>折扣率（只有按成本最小用；形如 offeringId=8000，逗号分隔）</span>
            <input value={discounts} onChange={(event) => setDiscounts(event.target.value)} size={40} />
          </label>
        </div>
        <div className="row" style={{ marginTop: 8 }}>
          <label className="field">
            <span>标签映射（只有账户标签策略用；形如 vip=offeringId，逗号分隔）</span>
            <input value={tags} onChange={(event) => setTags(event.target.value)} size={40} />
          </label>
          <button type="button" disabled={busy} onClick={submit}>
            {busy ? '写入中…' : '写入'}
          </button>
        </div>
        <p className="muted">
          任何策略都不得选中不合格候选——策略只决定"在合格候选里挑哪一条"。写进一个实现不了的取值会被拒，
          不会悄悄落成默认。
        </p>
      </div>

      <div className="panel">
        <h3>当前策略</h3>
        {(policies.data?.route_policies ?? []).length === 0 ? (
          <p className="muted">一条策略都没有：走默认的按档位顺序。</p>
        ) : (
          <table>
            <thead>
              <tr>
                <th>作用域</th>
                <th>策略</th>
                <th>折扣率</th>
                <th>标签映射</th>
                <th>版本</th>
              </tr>
            </thead>
            <tbody>
              {(policies.data?.route_policies ?? []).map((policy, index) => (
                <tr key={`${policy.gateway_model ?? 'global'}-${index}`}>
                  <td>{policy.gateway_model ?? '全局'}</td>
                  <td>{policy.strategy}</td>
                  <td className="mono">{Object.entries(policy.discount_rates).map(([k, v]) => `${k}=${v}`).join(', ') || '—'}</td>
                  <td className="mono">{Object.entries(policy.tag_channel_map).map(([k, v]) => `${k}=${v}`).join(', ') || '—'}</td>
                  <td>{policy.version}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </Page>
  );
}

/// `a=1,b=2` → `{a: 1, b: 2}`。形状写错就当场说清，不静默丢键。
function parseNumberMap(text: string, what: string): Record<string, number> {
  const out: Record<string, number> = {};
  for (const pair of text.split(',').map((item) => item.trim()).filter(Boolean)) {
    const index = pair.indexOf('=');
    if (index <= 0) throw new Error(`${what}要写成 键=值：${pair}`);
    const value = Number(pair.slice(index + 1));
    if (!Number.isFinite(value)) throw new Error(`${what}的值必须是数字：${pair}`);
    out[pair.slice(0, index).trim()] = value;
  }
  return out;
}

function parseStringMap(text: string, what: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const pair of text.split(',').map((item) => item.trim()).filter(Boolean)) {
    const index = pair.indexOf('=');
    if (index <= 0) throw new Error(`${what}要写成 键=值：${pair}`);
    out[pair.slice(0, index).trim()] = pair.slice(index + 1).trim();
  }
  return out;
}
