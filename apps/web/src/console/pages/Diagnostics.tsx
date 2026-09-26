import { Alert, App as AntApp, Badge, Button, Popconfirm, Table, Tabs, Tag, Typography } from 'antd';
import { useState } from 'react';
import type { AdminClient } from '../client';
import { useLoadable } from '../../shared/ui';
import { ConsolePage, Panel, whenText } from '../ui';

/// 对账与诊断：三张运营清单——对账案例、平台侧失败、成本缺口。
///
/// 它们的分工不同（`docs/design/0007` §7 与 `0009`）：**对账案例**是"执行状态不明、钱扣在对账里"，
/// 要人工去上游核；**平台侧失败**是欠费/凭证/配置这类要去修的事；**成本缺口**是"该有金额却拿不到"，
/// 对客照常结算、毛利侧标未知。
export function DiagnosticsPage({ client }: { client: AdminClient }) {
  const { message } = AntApp.useApp();
  const [tab, setTab] = useState<'cases' | 'failures' | 'gaps'>('cases');
  const [refunding, setRefunding] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const cases = useLoadable(() => client.reconciliationCases(), [client]);
  const failures = useLoadable(() => client.providerFailures([], 50), [client]);
  const gaps = useLoadable(() => client.providerCostGaps(50), [client]);

  const current = tab === 'cases' ? cases : tab === 'failures' ? failures : gaps;

  return (
    <ConsolePage
      title="对账与诊断"
      hint="三张清单分工不同：要人工去上游核的、要去修配置的、要事后补录成本的"
      error={error ?? current.error}
      loading={current.loading}
      onReload={() => current.reload()}
    >
      <Tabs
        activeKey={tab}
        onChange={(key) => setTab(key as 'cases' | 'failures' | 'gaps')}
        items={[
          {
            key: 'cases',
            label: (
              <Badge count={cases.data?.length ?? 0} offset={[10, 0]} size="small">
                对账案例
              </Badge>
            ),
            children: (
              <Panel
                title="对账案例"
                description="执行状态不明、钱还扣在对账里的那些。逐条去上游核实之后再退款结案。"
                extra={<Button onClick={cases.reload}>重取</Button>}
              >
                <Table
                  size="small"
                  rowKey="id"
                  loading={cases.loading}
                  pagination={false}
                  scroll={{ x: 'max-content' }}
                  dataSource={cases.data ?? []}
                  locale={{
                    emptyText: <Alert type="success" showIcon message="没有未结案的对账案例。" />,
                  }}
                  columns={[
                    {
                      title: '开启时刻',
                      dataIndex: 'created_at',
                      render: (value: string) => whenText(value),
                    },
                    {
                      title: '账户',
                      dataIndex: 'account_id',
                      render: (value: string) => <Typography.Text code>{value}</Typography.Text>,
                    },
                    {
                      title: 'Job',
                      dataIndex: 'job_id',
                      render: (value: string | null) =>
                        value ? <Typography.Text code>{value}</Typography.Text> : '—',
                    },
                    { title: '原因', dataIndex: 'reason' },
                    {
                      title: '渠道任务标识',
                      dataIndex: 'provider_trace_id',
                      render: (value: string | null) => value ?? '—',
                    },
                    {
                      title: '',
                      width: 140,
                      render: (_value: unknown, item: { job_id: string | null }) => {
                        if (!item.job_id) {
                          return (
                            <Typography.Text type="secondary">账户级案例，无 Job</Typography.Text>
                          );
                        }
                        const jobId = item.job_id;
                        return (
                          <Popconfirm
                            title="退款并结案？"
                            description="给这个账户退回顾客那笔钱，并把案例标记为已结案。这一动作幂等。"
                            okText="退款"
                            cancelText="取消"
                            onConfirm={async () => {
                              setError(null);
                              setRefunding(jobId);
                              try {
                                // 退款要一个业务键做幂等：用 job 与时间拼一个，避免误点两次退两次。
                                const key = `refund-${jobId}-${Date.now()}`;
                                await client.refundReconciliation(
                                  jobId,
                                  'admin console refund',
                                  key,
                                );
                                message.success(`已退款并结案：${jobId}`);
                                cases.reload();
                              } catch (failure) {
                                setError(
                                  failure instanceof Error ? failure.message : String(failure),
                                );
                              } finally {
                                setRefunding(null);
                              }
                            }}
                          >
                            <Button danger loading={refunding === jobId}>
                              退款并结案
                            </Button>
                          </Popconfirm>
                        );
                      },
                    },
                  ]}
                />
              </Panel>
            ),
          },
          {
            key: 'failures',
            label: (
              <Badge count={failures.data?.count ?? 0} offset={[10, 0]} size="small">
                平台侧失败
              </Badge>
            ),
            children: (
              <Panel
                title="平台侧失败"
                description="欠费、凭证、配置这类要去修的事，带渠道原始码与原文。"
                extra={<Button onClick={failures.reload}>重取</Button>}
              >
                <Table
                  size="small"
                  rowKey={(item, index) => `${item.job_id}-${index ?? 0}`}
                  loading={failures.loading}
                  pagination={false}
                  scroll={{ x: 'max-content' }}
                  dataSource={failures.data?.failures ?? []}
                  locale={{
                    emptyText: <Alert type="success" showIcon message="没有平台侧失败记录。" />,
                  }}
                  columns={[
                    {
                      title: '时刻',
                      dataIndex: 'updated_at',
                      render: (value: string) => whenText(value),
                    },
                    { title: '型号', dataIndex: 'gateway_model' },
                    {
                      title: '渠道',
                      dataIndex: 'provider_kind',
                      render: (value: string | null) => value ?? '—',
                    },
                    {
                      title: '类别',
                      dataIndex: 'kind',
                      render: (value: string) => <Tag color="warning">{value}</Tag>,
                    },
                    { title: '对客码', dataIndex: 'error_code' },
                    {
                      title: '渠道码',
                      dataIndex: 'provider_error_code',
                      render: (value: string | null) => value ?? '—',
                    },
                    {
                      title: '渠道原文',
                      dataIndex: 'provider_error_message',
                      render: (value: string | null) =>
                        value ? (
                          <Typography.Text style={{ fontSize: 12 }}>{value}</Typography.Text>
                        ) : (
                          '—'
                        ),
                    },
                    {
                      title: '任务标识',
                      dataIndex: 'provider_trace_id',
                      render: (value: string | null) => value ?? '—',
                    },
                  ]}
                />
                {failures.data?.truncated ? (
                  <Typography.Text type="secondary">只显示最近 50 条。</Typography.Text>
                ) : null}
              </Panel>
            ),
          },
          {
            key: 'gaps',
            label: (
              <Badge count={gaps.data?.count ?? 0} offset={[10, 0]} size="small">
                成本缺口
              </Badge>
            ),
            children: (
              <Panel
                title="成本缺口"
                description="执行发生了、成本本该有金额却拿不到的那些。缺口不进对账态：对客结算照常完成，毛利侧标「成本未知」，人工核完上游账单再补录。"
                extra={<Button onClick={gaps.reload}>重取</Button>}
              >
                <Table
                  size="small"
                  rowKey="attempt_id"
                  loading={gaps.loading}
                  pagination={false}
                  scroll={{ x: 'max-content' }}
                  dataSource={gaps.data?.gaps ?? []}
                  locale={{
                    emptyText: <Alert type="success" showIcon message="没有成本缺口。" />,
                  }}
                  columns={[
                    {
                      title: '完成时刻',
                      dataIndex: 'completed_at',
                      render: (value: string) => whenText(value),
                    },
                    { title: '型号', dataIndex: 'gateway_model' },
                    {
                      title: '渠道',
                      dataIndex: 'provider_kind',
                      render: (value: string | null) => value ?? '—',
                    },
                    {
                      title: '账户',
                      dataIndex: 'account_id',
                      render: (value: string) => <Typography.Text code>{value}</Typography.Text>,
                    },
                    {
                      title: 'Job',
                      dataIndex: 'job_id',
                      render: (value: string) => <Typography.Text code>{value}</Typography.Text>,
                    },
                    {
                      title: '任务标识（去上游核账用）',
                      dataIndex: 'provider_trace_id',
                      render: (value: string | null) => value ?? '—',
                    },
                  ]}
                />
                {gaps.data?.truncated ? (
                  <Typography.Text type="secondary">只显示最近 50 条。</Typography.Text>
                ) : null}
              </Panel>
            ),
          },
        ]}
      />
    </ConsolePage>
  );
}
