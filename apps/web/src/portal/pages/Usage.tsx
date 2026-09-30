import { Alert, Button, Card, Table, Tag, Typography } from 'antd';
import type { CustomerClient } from '../client';
import { whenText, yuanText } from '../../shared/format';
import { useLoadable } from '../../shared/ui';

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

function statusColor(status: 'succeeded' | 'failed' | 'pending' | 'canceled'): string {
  switch (status) {
    case 'succeeded':
      return 'green';
    case 'failed':
      return 'red';
    case 'canceled':
      return 'default';
    default:
      return 'processing';
  }
}

/// 调用记录：每一次生成请求的对客投影（型号、状态、时刻、张数与实际扣费）。
///
/// 只请求本页需要的 `/v1/customer/usage`；概览的数与账单、资金流水不在这一页（Spec C15）。
export function UsagePage({ client }: { client: CustomerClient }) {
  const usage = useLoadable(() => client.usage(50), [client]);

  return (
    <Card title="调用记录" extra={<Button onClick={usage.reload}>重取</Button>}>
      {usage.error ? <Alert type="error" showIcon message={usage.error} /> : null}
      <Table
        size="small"
        rowKey={(row, index) => `${row.created_at}-${index ?? 0}`}
        loading={usage.loading}
        pagination={false}
        scroll={{ x: 'max-content' }}
        dataSource={usage.data?.usage ?? []}
        locale={{ emptyText: <Alert type="info" showIcon message="还没有任何调用记录。" /> }}
        columns={[
          { title: '时刻', dataIndex: 'created_at', render: (value: string) => whenText(value) },
          { title: '型号', dataIndex: 'gateway_model' },
          {
            title: '类别',
            dataIndex: 'kind',
            render: (value: string) => (value === 'edit' ? '图片编辑' : '同步生成'),
          },
          {
            title: '状态',
            dataIndex: 'status',
            render: (value: 'succeeded' | 'failed' | 'pending' | 'canceled') => (
              <Tag color={statusColor(value)}>{statusLabel(value)}</Tag>
            ),
          },
          { title: '张数', dataIndex: 'image_count', align: 'right' },
          {
            title: '扣费（元）',
            dataIndex: 'charged_microusd',
            align: 'right',
            render: (value: number) => yuanText(value),
          },
        ]}
      />
      {usage.data?.truncated ? (
        <Typography.Text type="secondary">只显示最近 50 条。</Typography.Text>
      ) : null}
    </Card>
  );
}
