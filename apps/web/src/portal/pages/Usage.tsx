import { Alert, Button, Card, DatePicker, Divider, Flex, Table, Tag, Typography } from 'antd';
import dayjs from 'dayjs';
import type { CustomerClient } from '../client';
import type { CustomerUsageResponse, CustomerUsageRow } from '../../shared/types';
import { whenText, yuanText } from '../../shared/format';
import { useLoadable } from '../../shared/ui';
import { useCursorPage } from '../history';
import { rangeFromLocalDates, rangeLabel, useHistoryRange } from '../dates';

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

/// 一页取多少条已结束历史。翻页靠服务端给的游标，不把全量历史拉到浏览器再切。
const HISTORY_PAGE = 20;

/// 调用记录：**处理中单列**（可手动刷新），**已结束历史**按本地日期区间连续翻页。
///
/// 只请求本页需要的 `/v1/customer/usage`；余额、账单与资金流水不在这一页（Spec C15）。处理中的请求
/// 会变，所以它不承担稳定历史：游标只在已结束历史那一段用（设计 `0014` §2–§3）。
export function UsagePage({ client }: { client: CustomerClient }) {
  const [range, setRange] = useHistoryRange();
  const active = useLoadable(() => client.usage({ view: 'active', limit: 50 }), [client]);
  const history = useLoadable(
    () =>
      client.usage({
        view: 'completed',
        since: range.since,
        until: range.until,
        limit: HISTORY_PAGE,
      }),
    [client, range.since, range.until],
  );
  /// 续页挂在当前这一份已结束历史第一页上：重取或换区间都会换第一页，续页随之作废。
  const more = useCursorPage<CustomerUsageRow, CustomerUsageResponse>(
    history.data,
    async (cursor) => {
      const page = await client.usage({
        view: 'completed',
        since: range.since,
        until: range.until,
        limit: HISTORY_PAGE,
        cursor,
      });
      return { rows: page.usage, cursor: page.next_cursor };
    },
  );
  const rows = [...(history.data?.usage ?? []), ...more.rows];
  const nextCursor = more.cursor;

  return (
    <Card
      title="调用记录"
      extra={
        <Button data-testid="portal-usage-active-reload" onClick={active.reload}>
          刷新处理中
        </Button>
      }
    >
      <Typography.Title level={5} style={{ marginTop: 0 }}>
        处理中
      </Typography.Title>
      <Typography.Paragraph type="secondary">
        还在跑的请求。结果尚未确定的也在这里，直到结案——它不计入账单的已完成请求数。
      </Typography.Paragraph>
      {active.error ? <Alert type="error" showIcon message={active.error} /> : null}
      <div data-testid="portal-usage-active-table">
        <Table<CustomerUsageRow>
          size="small"
          rowKey={(row, index) => `${row.created_at}-${index ?? 0}`}
          loading={active.loading}
          pagination={false}
          scroll={{ x: 'max-content' }}
          dataSource={active.data?.usage ?? []}
          locale={{
            emptyText: <Alert type="info" showIcon message="没有正在处理的请求。" />,
          }}
          columns={[
            {
              title: '请求时刻',
              dataIndex: 'created_at',
              render: (value: string) => whenText(value),
            },
            { title: '型号', dataIndex: 'gateway_model' },
            {
              title: '状态',
              dataIndex: 'status',
              render: (value: CustomerUsageRow['status']) => (
                <Tag color={statusColor(value)}>{statusLabel(value)}</Tag>
              ),
            },
            // 还没结束就没有结果、也还没扣费：这两列按 0 显示——"没收费"与"读不到"要分得开（C9）。
            { title: '张数', dataIndex: 'image_count', align: 'right' },
            {
              title: '扣费（元）',
              dataIndex: 'charged_microusd',
              align: 'right',
              render: (value: number) => yuanText(value),
            },
          ]}
        />
      </div>

      <Divider />

      <Flex justify="space-between" align="center" wrap gap={8}>
        <Typography.Title level={5} style={{ margin: 0 }}>
          已结束历史
        </Typography.Title>
        <Flex gap={8} align="center">
          <span data-testid="portal-usage-range">
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
          <Button data-testid="portal-usage-history-reload" onClick={history.reload}>
            重取
          </Button>
        </Flex>
      </Flex>
      <Typography.Paragraph type="secondary">
        默认是含今天的最近 30 个自然日，按<b>终态时刻</b>归属；区间：
        <Typography.Text data-testid="portal-usage-range-label">
          {rangeLabel(range)}
        </Typography.Text>
      </Typography.Paragraph>
      {history.error ? <Alert type="error" showIcon message={history.error} /> : null}
      {more.error ? <Alert type="error" showIcon message={more.error} /> : null}
      <div data-testid="portal-usage-history-table">
        <Table<CustomerUsageRow>
          size="small"
          rowKey={(row, index) => `${row.terminal_at ?? row.created_at}-${index ?? 0}`}
          loading={history.loading}
          pagination={false}
          scroll={{ x: 'max-content' }}
          dataSource={rows}
          locale={{
            emptyText: <Alert type="info" showIcon message="这个区间里没有已结束的请求。" />,
          }}
          columns={[
            {
              title: '请求时刻',
              dataIndex: 'created_at',
              render: (value: string) => whenText(value),
            },
            {
              title: '终态时刻',
              dataIndex: 'terminal_at',
              render: (value: string | null) => whenText(value),
            },
            { title: '型号', dataIndex: 'gateway_model' },
            {
              title: '状态',
              dataIndex: 'status',
              render: (value: CustomerUsageRow['status']) => (
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
      </div>
      {nextCursor ? (
        <Button
          style={{ marginTop: 12 }}
          data-testid="portal-usage-more"
          loading={more.loading}
          onClick={more.loadMore}
        >
          继续查看更早的记录
        </Button>
      ) : null}
    </Card>
  );
}
