import { Alert, App as AntApp, Button, Flex, Form, Input, Table, Typography } from 'antd';
import { PlusOutlined } from '@ant-design/icons';
import { useState } from 'react';
import type { AdminClient } from '../client';
import { useLoadable } from '../../shared/ui';
import { ConsolePage, Panel, whenText } from '../ui';

/// 折算率：渠道币种 → CNY。**按币种维护、不进修订**，受理时取"受理时刻生效的那一行"。
///
/// 没有生效折算率的币种在发布期就会被拒，所以这一页通常排在发布之前用。页面下半部分列**当前生效**
/// 的那些行：录入之后要能一眼看出平台真正在用哪个数，而不是只看到"提交成功"。
export function RatesPage({ client }: { client: AdminClient }) {
  const { message } = AntApp.useApp();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const rates = useLoadable(() => client.fxRates(), [client]);
  const [form] = Form.useForm<{ currency: string; rate: string; effectiveAt?: string }>();

  async function submit(values: { currency: string; rate: string; effectiveAt?: string }) {
    setBusy(true);
    setError(null);
    try {
      const parsed = Number(values.rate);
      if (!Number.isFinite(parsed) || parsed <= 0) throw new Error('折算率必须是正数');
      // 请求收的是**微单位**整数：7.1 → 7100000。四舍五入到整数微单位，避免浮点尾巴。
      const micros = Math.round(parsed * 1_000_000);
      await client.upsertFxRate(
        values.currency.trim().toUpperCase(),
        micros,
        values.effectiveAt || undefined,
      );
      message.success(`已录入 ${values.currency.trim().toUpperCase()} → CNY = ${parsed}`);
      form.resetFields(['rate', 'effectiveAt']);
      rates.reload();
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setBusy(false);
    }
  }

  return (
    <ConsolePage
      title="折算率"
      hint="按币种维护；不填生效时刻＝立即生效"
      error={error}
      onReload={rates.reload}
      loading={rates.loading}
    >
      <Panel
        title="录入折算率"
        description="同币种（CNY → CNY）按定义恒为 1，不需要录。给了未来时刻就是调价预告——受理时取的仍是受理时刻之前已生效的那一行。"
      >
        <Form
          form={form}
          layout="inline"
          onFinish={submit}
          initialValues={{ currency: 'USD', rate: '7.1' }}
        >
          <Form.Item
            name="currency"
            label="币种"
            rules={[{ required: true, message: '请填币种' }]}
          >
            <Input style={{ width: 100 }} placeholder="USD" />
          </Form.Item>
          <Form.Item
            name="rate"
            label="1 单位该币种 = 多少 CNY"
            rules={[{ required: true, message: '请填折算率' }]}
          >
            <Input style={{ width: 140 }} placeholder="7.1" />
          </Form.Item>
          <Form.Item name="effectiveAt" label="生效时刻（RFC3339，可空）">
            <Input style={{ width: 240 }} placeholder="2026-10-01T00:00:00Z" />
          </Form.Item>
          <Form.Item>
            <Button type="primary" icon={<PlusOutlined />} htmlType="submit" loading={busy}>
              录入
            </Button>
          </Form.Item>
        </Form>
      </Panel>

      <Panel
        title="当前生效的折算率"
        description="每个币种只显示当前生效的那一行——受理时用的就是它。"
        extra={<Button onClick={rates.reload}>重取</Button>}
      >
        <Table
          size="small"
          rowKey="currency"
          loading={rates.loading}
          dataSource={rates.data?.rates ?? []}
          pagination={false}
          locale={{
            emptyText: (
              <Alert
                type="warning"
                showIcon
                message="一个币种都没有。渠道成本币种没有生效折算率时，发布会在校验期被拒。"
              />
            ),
          }}
          columns={[
            {
              title: '币种',
              dataIndex: 'currency',
              render: (value: string) => <Typography.Text strong>{value}</Typography.Text>,
            },
            {
              title: '1 单位 = 多少 CNY',
              dataIndex: 'rate_micros',
              align: 'right',
              render: (value: number) => (
                <Flex vertical align="flex-end">
                  <Typography.Text>{(value / 1_000_000).toFixed(6).replace(/0+$/, '').replace(/\.$/, '')}</Typography.Text>
                  <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                    {value} 微单位
                  </Typography.Text>
                </Flex>
              ),
            },
            {
              title: '生效时刻',
              dataIndex: 'effective_at',
              render: (value: string) => whenText(value),
            },
          ]}
        />
      </Panel>
    </ConsolePage>
  );
}
