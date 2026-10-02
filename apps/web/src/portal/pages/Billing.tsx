import {
  Alert,
  Button,
  Card,
  Col,
  DatePicker,
  Flex,
  Row,
  Segmented,
  Statistic,
  Table,
  Tag,
  Typography,
} from 'antd';
import dayjs from 'dayjs';
import { useState } from 'react';
import type { CustomerClient } from '../client';
import type { CustomerLedgerResponse, LedgerEntry } from '../../shared/types';
import { whenText, yuanText } from '../../shared/format';
import { useLoadable } from '../../shared/ui';
import { useCursorPage } from '../history';
import { rangeFromLocalDates, rangeLabel, useHistoryRange } from '../dates';

/// 真实收支类别的展示文案。取值由服务端给，界面只翻译；不认识的原样透出，不猜。
///
/// 只列**对客可见**的三类：`hold` / `release` / `cost` 服务端根本不回（Spec C8、V-C15）。
const KIND_LABELS: Record<string, string> = {
  credit: '充值',
  capture: '实际扣费',
  adjustment: '资金调整',
};

/// 客户能筛的类别：**没有 `cost`**——平台成本不是客户的事实（Spec C8）。标签给一个稳定的锚点，
/// 浏览器用例按它点，不按文案。
const KIND_OPTIONS = [
  { value: '', label: <span data-testid="portal-ledger-kind-all">全部</span> },
  {
    value: 'credit',
    label: <span data-testid="portal-ledger-kind-credit">充值</span>,
  },
  {
    value: 'capture',
    label: <span data-testid="portal-ledger-kind-capture">实际扣费</span>,
  },
  {
    value: 'adjustment',
    label: <span data-testid="portal-ledger-kind-adjustment">资金调整</span>,
  },
];

/// 一页取多少条流水；更早的记录用服务端给的游标继续取。
const LEDGER_PAGE = 20;

/// 有符号净额的展示：负值以正数金额标"净支出"，正值标"净返还"，零标"收支相抵"。
///
/// 不把净额除以请求数并称作平均扣费——那是 Spec §4.3 明确禁止的口径。
function netText(microusd: number): string {
  if (microusd === 0) return '收支相抵';
  return microusd < 0 ? `净支出 ${yuanText(-microusd)}` : `净返还 ${yuanText(microusd)}`;
}

/// 账单与资金记录：**同一个日期区间**驱动区间汇总与真实收支流水。
///
/// 汇总按整段区间全量计算、不随流水的条数变化（Spec C10）；区间进地址查询参数，返回时仍在
/// （V-D13）；翻页复用同一个 `since`/`until` 与游标，不重算"现在"（设计 `0014` §3）。
export function BillingPage({ client }: { client: CustomerClient }) {
  const [range, setRange] = useHistoryRange();
  const [kind, setKind] = useState('');
  const billing = useLoadable(
    () => client.billing({ since: range.since, until: range.until }),
    [client, range.since, range.until],
  );
  const ledger = useLoadable(
    () =>
      client.ledger({
        since: range.since,
        until: range.until,
        kind: kind || undefined,
        limit: LEDGER_PAGE,
      }),
    [client, range.since, range.until, kind],
  );
  /// 续页挂在当前这一份流水第一页上：重取、换区间或换类别都会换第一页，续页随之作废。
  const more = useCursorPage<LedgerEntry, CustomerLedgerResponse>(ledger.data, async (cursor) => {
    const page = await client.ledger({
      since: range.since,
      until: range.until,
      kind: kind || undefined,
      limit: LEDGER_PAGE,
      cursor,
    });
    return { rows: page.entries, cursor: page.next_cursor };
  });
  const rows = [...(ledger.data?.entries ?? []), ...more.rows];
  const nextCursor = more.cursor;

  return (
    <Flex vertical gap={16}>
      <Card
        title="账单汇总"
        extra={
          <Flex gap={8} align="center">
            <span data-testid="portal-billing-range">
              <DatePicker.RangePicker
                allowClear={false}
                value={[
                  dayjs(range.since),
                  dayjs(new Date(new Date(range.until).getTime() - 1).toISOString()),
                ]}
                onChange={(values) => {
                  if (!values?.[0] || !values[1]) return;
                  setRange(
                    rangeFromLocalDates(
                      values[0].format('YYYY-MM-DD'),
                      values[1].format('YYYY-MM-DD'),
                    ),
                  );
                }}
              />
            </span>
            <Button
              data-testid="portal-billing-reload"
              // 刷新按**同一区间**把汇总与流水一起重取（V-D14）。
              onClick={() => {
                billing.reload();
                ledger.reload();
              }}
            >
              刷新
            </Button>
          </Flex>
        }
      >
        <Typography.Paragraph type="secondary" style={{ marginTop: 0 }}>
          按整段区间全量计算，不随下面流水的条数变化。充值不计入扣费净额。区间：
          <Typography.Text data-testid="portal-billing-range-label">
            {rangeLabel(range)}
          </Typography.Text>
        </Typography.Paragraph>
        {billing.error ? <Alert type="error" showIcon message={billing.error} /> : null}
        <Row gutter={[24, 16]}>
          <Col xs={8}>
            <span data-testid="portal-billing-requests">
              <Statistic
                title="请求数"
                // 失败时 `data` 保持 null：显示 `—` 而不是 0——把"没读到"画成"零"是错的读数（V-D14）。
                value={billing.data ? billing.data.requests : '—'}
                loading={billing.loading}
              />
            </span>
          </Col>
          <Col xs={8}>
            <span data-testid="portal-billing-images">
              <Statistic
                title="产出图片数"
                value={billing.data ? billing.data.images : '—'}
                loading={billing.loading}
              />
            </span>
          </Col>
          <Col xs={8}>
            <span data-testid="portal-billing-net">
              <Statistic
                title="扣费净额"
                value={billing.data ? netText(billing.data.charged_microusd) : '—'}
                loading={billing.loading}
              />
            </span>
          </Col>
        </Row>
      </Card>

      <Card title="资金流水">
        <Typography.Paragraph type="secondary" style={{ marginTop: 0 }}>
          充值由运营在后台完成；扣费在生成请求结算后出现在这里。金额的符号是语义的一部分。
        </Typography.Paragraph>
        <Flex gap={8} align="center" wrap style={{ marginBottom: 12 }}>
          <span data-testid="portal-ledger-kind">
            <Segmented
              options={KIND_OPTIONS}
              value={kind}
              onChange={(value) => setKind(String(value))}
            />
          </span>
          <Button data-testid="portal-ledger-reload" onClick={ledger.reload}>
            重取
          </Button>
        </Flex>
        {ledger.error ? <Alert type="error" showIcon message={ledger.error} /> : null}
        {more.error ? <Alert type="error" showIcon message={more.error} /> : null}
        <div data-testid="portal-ledger-table">
          <Table<LedgerEntry>
            size="small"
            rowKey={(entry, index) => `${entry.created_at}-${index ?? 0}`}
            loading={ledger.loading}
            pagination={false}
            dataSource={rows}
            locale={{
              emptyText: (
                <Alert
                  type="info"
                  showIcon
                  message="这个区间里没有资金记录。充值由运营在后台完成，完成之后这里会出现一条 credit。"
                />
              ),
            }}
            columns={[
              {
                title: '时刻',
                dataIndex: 'created_at',
                render: (value: string) => whenText(value),
              },
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
        </div>
        {nextCursor ? (
          <Button
            style={{ marginTop: 12 }}
            data-testid="portal-ledger-more"
            loading={more.loading}
            onClick={more.loadMore}
          >
            继续查看更早的记录
          </Button>
        ) : null}
      </Card>
    </Flex>
  );
}
