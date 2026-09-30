import { Alert, Button, Card, Col, Flex, Row, Statistic, Table, Tag, Typography } from 'antd';
import type { CustomerClient } from '../client';
import { whenText, yuanText } from '../../shared/format';
import { useLoadable } from '../../shared/ui';

/// 真实收支类别的展示文案。取值由服务端给，界面只翻译；不认识的原样透出，不猜。
const KIND_LABELS: Record<string, string> = {
  credit: '充值',
  capture: '实际扣费',
  adjustment: '资金调整',
  cost: '平台成本',
};

/// 有符号净额的展示：负值以正数金额标"净支出"，正值标"净返还"，零标"收支相抵"。
///
/// 不把净额除以请求数并称作平均扣费——那是 Spec §4.3 明确禁止的口径。
function netText(microusd: number): string {
  if (microusd === 0) return '收支相抵';
  return microusd < 0 ? `净支出 ${yuanText(-microusd)}` : `净返还 ${yuanText(microusd)}`;
}

/// 账单与资金记录：区间汇总（请求数、产出图片数、扣费净额）与真实收支流水。
///
/// 汇总按整段区间全量计算，不随流水的条数变化；充值不计入扣费净额（Spec C8、C10）。本页只请求
/// `/v1/customer/billing` 与 `/v1/customer/ledger`。
export function BillingPage({ client }: { client: CustomerClient }) {
  const billing = useLoadable(() => client.billing(), [client]);
  const ledger = useLoadable(() => client.ledger(50), [client]);

  return (
    <Flex vertical gap={16}>
      <Card title="账单汇总" extra={<Button onClick={billing.reload}>重取</Button>}>
        <Typography.Paragraph type="secondary" style={{ marginTop: 0 }}>
          按整段区间全量计算，不随下面流水的条数变化。充值不计入扣费净额。
        </Typography.Paragraph>
        {billing.error ? <Alert type="error" showIcon message={billing.error} /> : null}
        <Row gutter={[24, 16]}>
          <Col xs={8}>
            <Statistic
              title="请求数"
              // 失败时 `data` 保持 null：显示 `—` 而不是 0——把"没读到"画成"零"是错的读数（V-D14）。
              value={billing.data ? billing.data.requests : '—'}
              loading={billing.loading}
            />
          </Col>
          <Col xs={8}>
            <Statistic
              title="产出图片数"
              value={billing.data ? billing.data.images : '—'}
              loading={billing.loading}
            />
          </Col>
          <Col xs={8}>
            <Statistic
              title="扣费净额"
              value={billing.data ? netText(billing.data.charged_microusd) : '—'}
              loading={billing.loading}
            />
          </Col>
        </Row>
      </Card>

      <Card title="资金流水" extra={<Button onClick={ledger.reload}>重取</Button>}>
        <Typography.Paragraph type="secondary" style={{ marginTop: 0 }}>
          充值由运营在后台完成；扣费在生成请求结算后出现在这里。金额的符号是语义的一部分。
        </Typography.Paragraph>
        {ledger.error ? <Alert type="error" showIcon message={ledger.error} /> : null}
        <Table
          size="small"
          rowKey={(entry, index) => `${entry.created_at}-${index ?? 0}`}
          loading={ledger.loading}
          pagination={false}
          dataSource={ledger.data?.entries ?? []}
          locale={{
            emptyText: (
              <Alert
                type="info"
                showIcon
                message="还没有任何资金记录。充值由运营在后台完成，完成之后这里会出现一条 credit。"
              />
            ),
          }}
          columns={[
            { title: '时刻', dataIndex: 'created_at', render: (value: string) => whenText(value) },
            {
              title: '类别',
              dataIndex: 'kind',
              render: (value: string) => (
                <Tag color={value === 'credit' ? 'green' : 'default'}>
                  {KIND_LABELS[value] ?? value}
                </Tag>
              ),
            },
            {
              title: '金额（元）',
              dataIndex: 'amount_microusd',
              align: 'right',
              render: (value: number) => (
                <Typography.Text type={value < 0 ? 'danger' : undefined}>
                  {yuanText(value)}
                </Typography.Text>
              ),
            },
          ]}
        />
        {ledger.data?.truncated ? (
          <Typography.Text type="secondary">只显示最近 50 条（按时间倒序）。</Typography.Text>
        ) : null}
      </Card>
    </Flex>
  );
}
