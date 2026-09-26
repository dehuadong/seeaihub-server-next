import { useState } from 'react';
import type { AdminClient } from '../client';
import { Page, useLoadable } from '../../shared/ui';
import { when } from '../../shared/routes';

/// 对账与诊断：三张运营清单——对账案例、平台侧失败、成本缺口。
///
/// 它们的分工不同（`docs/design/0007` §7 与 `0009`）：**对账案例**是"执行状态不明、钱扣在对账里"，
/// 要人工去上游核；**平台侧失败**是欠费/凭证/配置这类要去修的事；**成本缺口**是"该有金额却拿不到"，
/// 对客照常结算、毛利侧标未知。
export function DiagnosticsPage({ client }: { client: AdminClient }) {
  const [tab, setTab] = useState<'cases' | 'failures' | 'gaps'>('cases');
  const [refunding, setRefunding] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const cases = useLoadable(() => client.reconciliationCases(), [client]);
  const failures = useLoadable(() => client.providerFailures([], 50), [client]);
  const gaps = useLoadable(() => client.providerCostGaps(50), [client]);

  const current = tab === 'cases' ? cases : tab === 'failures' ? failures : gaps;

  return (
    <Page
      title="对账与诊断"
      error={error ?? current.error}
      loading={current.loading}
      onReload={() => {
        current.reload();
      }}
      hint={
        <span className="row">
          <button type="button" className={tab === 'cases' ? 'active' : ''} onClick={() => setTab('cases')}>
            对账案例（{cases.data?.length ?? 0}）
          </button>
          <button type="button" onClick={() => setTab('failures')}>
            平台侧失败（{failures.data?.count ?? 0}）
          </button>
          <button type="button" onClick={() => setTab('gaps')}>
            成本缺口（{gaps.data?.count ?? 0}）
          </button>
        </span>
      }
    >
      {notice ? <p style={{ color: 'var(--ok)' }}>{notice}</p> : null}

      {tab === 'cases' ? (
        (cases.data ?? []).length === 0 ? (
          <p className="muted">没有未结案的对账案例。</p>
        ) : (
          <table>
            <thead>
              <tr>
                <th>开启时刻</th>
                <th>账户</th>
                <th>Job</th>
                <th>原因</th>
                <th>渠道任务标识</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {(cases.data ?? []).map((item) => (
                <tr key={item.id}>
                  <td>{when(item.created_at)}</td>
                  <td className="mono">{item.account_id}</td>
                  <td className="mono">{item.job_id ?? '—'}</td>
                  <td>{item.reason}</td>
                  <td className="mono">{item.provider_trace_id ?? '—'}</td>
                  <td>
                    {item.job_id ? (
                      <button
                        type="button"
                        disabled={refunding === item.job_id}
                        onClick={async () => {
                          setError(null);
                          setRefunding(item.job_id);
                          try {
                            // 退款要一个业务键做幂等：用 job 与时间拼一个，避免误点两次退两次。
                            const key = `refund-${item.job_id}-${Date.now()}`;
                            await client.refundReconciliation(item.job_id!, 'admin console refund', key);
                            setNotice(`已退款并结案：${item.job_id}`);
                            cases.reload();
                          } catch (failure) {
                            setError(failure instanceof Error ? failure.message : String(failure));
                          } finally {
                            setRefunding(null);
                          }
                        }}
                      >
                        退款并结案
                      </button>
                    ) : (
                      <span className="muted">账户级案例，无 Job</span>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )
      ) : null}

      {tab === 'failures' ? (
        (failures.data?.failures ?? []).length === 0 ? (
          <p className="muted">没有平台侧失败记录。</p>
        ) : (
          <>
            <table>
              <thead>
                <tr>
                  <th>时刻</th>
                  <th>型号</th>
                  <th>渠道</th>
                  <th>类别</th>
                  <th>对客码</th>
                  <th>渠道码</th>
                  <th>渠道原文</th>
                  <th>任务标识</th>
                </tr>
              </thead>
              <tbody>
                {(failures.data?.failures ?? []).map((item, index) => (
                  <tr key={`${item.job_id}-${index}`}>
                    <td>{when(item.updated_at)}</td>
                    <td>{item.gateway_model}</td>
                    <td>{item.provider_kind ?? '—'}</td>
                    <td>{item.kind}</td>
                    <td>{item.error_code}</td>
                    <td>{item.provider_error_code ?? '—'}</td>
                    <td className="mono">{item.provider_error_message ?? '—'}</td>
                    <td className="mono">{item.provider_trace_id ?? '—'}</td>
                  </tr>
                ))}
              </tbody>
            </table>
            {failures.data?.truncated ? <p className="muted">只显示最近 50 条。</p> : null}
          </>
        )
      ) : null}

      {tab === 'gaps' ? (
        (gaps.data?.gaps ?? []).length === 0 ? (
          <p className="muted">没有成本缺口。</p>
        ) : (
          <>
            <table>
              <thead>
                <tr>
                  <th>完成时刻</th>
                  <th>型号</th>
                  <th>渠道</th>
                  <th>账户</th>
                  <th>Job</th>
                  <th>任务标识（去上游核账用）</th>
                </tr>
              </thead>
              <tbody>
                {(gaps.data?.gaps ?? []).map((item) => (
                  <tr key={item.attempt_id}>
                    <td>{when(item.completed_at)}</td>
                    <td>{item.gateway_model}</td>
                    <td>{item.provider_kind ?? '—'}</td>
                    <td className="mono">{item.account_id}</td>
                    <td className="mono">{item.job_id}</td>
                    <td className="mono">{item.provider_trace_id ?? '—'}</td>
                  </tr>
                ))}
              </tbody>
            </table>
            {gaps.data?.truncated ? <p className="muted">只显示最近 50 条。</p> : null}
            <p className="muted">缺口不进对账态：对客结算照常完成，毛利侧标"成本未知"，人工核完上游账单再补录。</p>
          </>
        )
      ) : null}
    </Page>
  );
}
