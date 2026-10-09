import { Alert, Button, Card, Col, Flex, Row, Statistic, Typography } from 'antd';
import {
  AccountBookOutlined,
  HistoryOutlined,
  KeyOutlined,
  SettingOutlined,
} from '@ant-design/icons';
import type { CustomerClient } from '../client';
import { portalPath } from '../routes';
import { pointsText, whenText } from '../../shared/format';
import { useLoadable } from '../../shared/ui';

/// 概览：**首屏只有一个数**——「余额」，外加通往其余四页的入口。
///
/// 这个数是客户**现在能用的钱**（`available_points` = 已结算余额 − 持有中）。内部那套"已结算余额／
/// 持有中／可用额"的分法、以及"什么时候会动"的机制都只在合同与管理员面出现：客户页面只有一个数、
/// 一个名字，也不写解释占用与结算的文案（[控制台 Spec](../../../../../docs/contracts/0001-admin-and-customer-consoles.md)
/// C7、§4.3、V-D5；[账户资金 Spec](../../../../../docs/contracts/0002-account-funds-and-reservations.md) §4、A7）。
/// 取数失败显示错误、不把缺失值画成 0，加载中不显示数字（控制台 Spec V-D14 的口径）。概览只请求当前
/// 账户金额，不请求账单、历史用量或资金流水，也不显示没有区间限定的"扣费总额"或平均扣费。
export function OverviewPage({
  client,
  onOpen,
}: {
  client: CustomerClient;
  onOpen: (to: string) => void;
}) {
  const account = useLoadable(() => client.account(), [client]);

  const entries = [
    { route: 'usage' as const, label: '调用记录', hint: '每一次生成请求与对客状态', icon: <HistoryOutlined /> },
    { route: 'billing' as const, label: '账单与资金记录', hint: '区间汇总与真实收支流水', icon: <AccountBookOutlined /> },
    { route: 'keys' as const, label: 'API Key', hint: '新建、查看与吊销密钥', icon: <KeyOutlined /> },
    { route: 'settings' as const, label: '账户设置', hint: '账户 id、改密码与退出', icon: <SettingOutlined /> },
  ];

  return (
    <Flex vertical gap={16}>
      <Card>
        {account.error ? (
          <Alert style={{ marginBottom: 8 }} type="error" showIcon message={account.error} />
        ) : null}
        <Flex justify="space-between" align="flex-start" gap={8}>
          <Statistic
            data-testid="portal-balance"
            title="余额"
            value={account.data ? pointsText(account.data.available_points) : '—'}
            loading={account.loading}
          />
          {/* 受理与结算都会动这个数：给客户一个自己取准数的地方。 */}
          <Button data-testid="portal-balance-reload" onClick={account.reload}>
            刷新
          </Button>
        </Flex>
        {account.data ? (
          <Typography.Text type="secondary" style={{ fontSize: 12 }}>
            更新于 {whenText(account.data.updated_at)}
          </Typography.Text>
        ) : null}
      </Card>

      <Card title="去哪里看">
        <Row gutter={[16, 16]}>
          {entries.map((entry) => (
            <Col key={entry.route} xs={24} sm={12} lg={6}>
              <Button
                data-testid={`portal-entry-${entry.route}`}
                block
                size="large"
                icon={entry.icon}
                onClick={() => onOpen(portalPath(entry.route))}
                style={{ height: 'auto', padding: '12px 16px', textAlign: 'left' }}
              >
                <Flex vertical align="flex-start" gap={2}>
                  <span>{entry.label}</span>
                  <Typography.Text type="secondary" style={{ fontSize: 12, fontWeight: 'normal' }}>
                    {entry.hint}
                  </Typography.Text>
                </Flex>
              </Button>
            </Col>
          ))}
        </Row>
      </Card>
    </Flex>
  );
}
