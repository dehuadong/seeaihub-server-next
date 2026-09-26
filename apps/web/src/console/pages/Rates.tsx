import { useState } from 'react';
import type { AdminClient } from '../client';
import { Page, useLoadable } from '../../shared/ui';
import { when, yuan } from '../../shared/routes';

/// 折算率：渠道币种 → CNY。**按币种维护、不进修订**，受理时取"受理时刻生效的那一行"。
///
/// 没有生效折算率的币种在发布期就会被拒，所以这一页通常排在发布之前用。页面下半部分列**当前生效**
/// 的那些行：录入之后要能一眼看出平台真正在用哪个数，而不是只看到"提交成功"。
export function RatesPage({ client }: { client: AdminClient }) {
  const [currency, setCurrency] = useState('USD');
  const [rate, setRate] = useState('7.1');
  const [effectiveAt, setEffectiveAt] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [ok, setOk] = useState<string | null>(null);
  const rates = useLoadable(() => client.fxRates(), [client]);

  async function submit() {
    setBusy(true);
    setError(null);
    setOk(null);
    try {
      const parsed = Number(rate);
      if (!Number.isFinite(parsed) || parsed <= 0) throw new Error('折算率必须是正数');
      // 请求收的是**微单位**整数：7.1 → 7100000。四舍五入到整数微单位，避免浮点尾巴。
      const micros = Math.round(parsed * 1_000_000);
      await client.upsertFxRate(currency.trim().toUpperCase(), micros, effectiveAt || undefined);
      setOk(`已录入 ${currency.toUpperCase()} → CNY = ${parsed}（${micros} 微单位）${effectiveAt ? `，生效于 ${effectiveAt}` : '，立即生效'}`);
      rates.reload();
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Page title="折算率" hint="按币种维护；不填生效时刻＝立即生效" error={error}>
      <div className="panel">
        <div className="row">
          <label className="field">
            <span>币种</span>
            <input value={currency} onChange={(event) => setCurrency(event.target.value)} size={8} />
          </label>
          <label className="field">
            <span>1 单位该币种 = 多少 CNY</span>
            <input value={rate} onChange={(event) => setRate(event.target.value)} size={10} />
          </label>
          <label className="field">
            <span>生效时刻（RFC3339，可空）</span>
            <input
              value={effectiveAt}
              onChange={(event) => setEffectiveAt(event.target.value)}
              size={26}
              placeholder="2026-10-01T00:00:00Z"
            />
          </label>
          <button type="button" disabled={busy} onClick={submit}>
            {busy ? '提交中…' : '录入'}
          </button>
        </div>
        <p className="muted">
          同币种（CNY → CNY）按定义恒为 1，不需要录。给了未来时刻就是**调价预告**——受理时取的仍是受理时刻之前
          已生效的那一行。
        </p>
        {ok ? <p style={{ color: 'var(--ok)' }}>{ok}</p> : null}
      </div>

      <div className="panel">
        <h3>当前生效的折算率</h3>
        {rates.error ? <p className="error">{rates.error}</p> : null}
        {rates.loading ? <p className="muted">读取中…</p> : null}
        {rates.data && rates.data.rates.length === 0 ? (
          <p className="muted">
            一个币种都没有。渠道成本币种没有生效折算率时，发布会在校验期被拒。
          </p>
        ) : null}
        {rates.data && rates.data.rates.length > 0 ? (
          <table>
            <thead>
              <tr>
                <th>币种</th>
                <th>1 单位 = 多少 CNY</th>
                <th>生效时刻</th>
              </tr>
            </thead>
            <tbody>
              {rates.data.rates.map((row) => (
                <tr key={row.currency}>
                  <td>{row.currency}</td>
                  <td>{yuan(row.rate_micros)}</td>
                  <td>{when(row.effective_at)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        ) : null}
        <p className="muted">
          <button type="button" onClick={rates.reload}>
            重取
          </button>{' '}
          每个币种只显示**当前生效**的那一行——受理时用的就是它。
        </p>
      </div>
    </Page>
  );
}
