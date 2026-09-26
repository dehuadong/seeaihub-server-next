import { Alert, App as AntApp, Button, Form, Input, Select, Table, Tag, Typography } from 'antd';
import { SaveOutlined } from '@ant-design/icons';
import { useState } from 'react';
import type { AdminClient } from '../client';
import type { RouteStrategy } from '../../shared/types';
import { useLoadable } from '../../shared/ui';
import { ConsolePage, Panel } from '../ui';

const STRATEGIES: { value: RouteStrategy; label: string }[] = [
  { value: 'priority_failover', label: '按档位顺序（默认）' },
  { value: 'weighted_random', label: '按权重随机' },
  { value: 'least_cost', label: '按折后成本最小' },
  { value: 'user_tag', label: '按账户标签映射' },
];

const STRATEGY_LABEL = new Map(STRATEGIES.map((item) => [item.value, item.label]));

/// 路由策略：**在合格候选里挑哪一条**由运营配置。它不进不可变修订，改它即刻影响之后的受理；
/// 已受理的 Job 的候选与定价早已随快照冻结，不受影响。
export function RoutingPage({ client }: { client: AdminClient }) {
  const { message } = AntApp.useApp();
  const policies = useLoadable(() => client.routePolicies(), [client]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [form] = Form.useForm<{
    scope?: string;
    strategy: RouteStrategy;
    discounts?: string;
    tags?: string;
  }>();

  async function submit(values: {
    scope?: string;
    strategy: RouteStrategy;
    discounts?: string;
    tags?: string;
  }) {
    setBusy(true);
    setError(null);
    try {
      await client.upsertRoutePolicy(
        values.scope?.trim() || null,
        values.strategy,
        parseNumberMap(values.discounts ?? '', '折扣率'),
        parseStringMap(values.tags ?? '', '标签映射'),
      );
      message.success('策略已写入');
      form.resetFields(['scope', 'discounts', 'tags']);
      policies.reload();
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setBusy(false);
    }
  }

  return (
    <ConsolePage
      title="路由策略"
      hint="不配置＝按档位顺序，即零配置行为"
      error={error ?? policies.error}
      loading={policies.loading}
      onReload={policies.reload}
    >
      <Panel
        title="写入（或覆盖）一条策略"
        description="任何策略都不得选中不合格候选——策略只决定在合格候选里挑哪一条。写进一个实现不了的取值会被拒，不会悄悄落成默认。"
      >
        <Form
          form={form}
          layout="vertical"
          onFinish={submit}
          initialValues={{ strategy: 'priority_failover' }}
        >
          <div
            style={{
              display: 'grid',
              gridTemplateColumns: 'repeat(auto-fit, minmax(240px, 1fr))',
              gap: 16,
            }}
          >
            <Form.Item name="scope" label="作用域（空＝全局）">
              <Input placeholder="gpt-image-2.5-flare" allowClear />
            </Form.Item>
            <Form.Item name="strategy" label="策略" rules={[{ required: true }]}>
              <Select options={STRATEGIES} />
            </Form.Item>
            <Form.Item
              name="discounts"
              label="折扣率（只有按成本最小用）"
              tooltip="形如 offeringId=8000，逗号分隔"
            >
              <Input placeholder="offering-id=8000" allowClear />
            </Form.Item>
            <Form.Item
              name="tags"
              label="标签映射（只有账户标签策略用）"
              tooltip="形如 vip=offeringId，逗号分隔"
            >
              <Input placeholder="vip=offering-id" allowClear />
            </Form.Item>
          </div>
          <Form.Item style={{ marginBottom: 0 }}>
            <Button type="primary" icon={<SaveOutlined />} htmlType="submit" loading={busy}>
              写入
            </Button>
          </Form.Item>
        </Form>
      </Panel>

      <Panel title="当前策略" extra={<Button onClick={policies.reload}>重取</Button>}>
        <Table
          size="small"
          loading={policies.loading}
          rowKey={(policy, index) => `${policy.gateway_model ?? 'global'}-${index ?? 0}`}
          dataSource={policies.data?.route_policies ?? []}
          pagination={false}
          locale={{
            emptyText: (
              <Alert type="info" showIcon message="一条策略都没有：走默认的按档位顺序。" />
            ),
          }}
          columns={[
            {
              title: '作用域',
              dataIndex: 'gateway_model',
              render: (value: string | null) =>
                value ? <Typography.Text code>{value}</Typography.Text> : <Tag>全局</Tag>,
            },
            {
              title: '策略',
              dataIndex: 'strategy',
              render: (value: RouteStrategy) => STRATEGY_LABEL.get(value) ?? value,
            },
            {
              title: '折扣率',
              dataIndex: 'discount_rates',
              render: (value: Record<string, number>) => mapText(value),
            },
            {
              title: '标签映射',
              dataIndex: 'tag_channel_map',
              render: (value: Record<string, string>) => mapText(value),
            },
            { title: '版本', dataIndex: 'version', align: 'right', width: 80 },
          ]}
        />
      </Panel>
    </ConsolePage>
  );
}

/// 把 `{a: 1}` 这类映射渲染成一行等宽文本；空映射显示 `—`，不显示一个空单元格。
function mapText(value: Record<string, unknown> | null | undefined): string {
  const entries = Object.entries(value ?? {});
  if (entries.length === 0) return '—';
  return entries.map(([key, item]) => `${key}=${item}`).join(', ');
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
