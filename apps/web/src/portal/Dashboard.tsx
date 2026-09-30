import {
  Alert,
  App as AntApp,
  Button,
  Card,
  Col,
  Descriptions,
  Flex,
  Form,
  Input,
  Row,
  Space,
  Statistic,
  Table,
  Tabs,
  Tag,
  Tooltip,
  Typography,
} from 'antd';
import { InfoCircleOutlined, KeyOutlined, LockOutlined, PlusOutlined } from '@ant-design/icons';
import { useState } from 'react';
import type { CustomerClient } from './client';
import { useCustomerSession } from './session';
import { whenText, yuanText } from '../shared/format';
import { useLoadable } from '../shared/ui';

/// 客户控制台的主体。
///
/// **首屏只回答两件事**（已结算余额、扣费总额）——那是客户打开这一页的动机；其余按标签页收起
/// 来：用量与账单（明细）、API Key、账户设置（改口令等低频动作）。分组依据是**使用频次**，不是端点
/// 归属：改口令是一次性动作，与余额并列会把两件常看的事切开。
///
/// 余额只渲染 `balance_microusd`（已结算余额）：预授权建立或释放不改变它，持有中、可用额与单笔
/// 预授权额都不在客户页面出现（[账户资金 Spec](../../../../docs/specs/0002-account-funds-and-reservations.md) §4、[控制台 Spec](../../../../docs/specs/0001-admin-and-customer-consoles.md) C7）。
///
/// 口径见 `docs/design/0011-console-information-architecture.md` §3.2。
export function Dashboard({ client }: { client: CustomerClient }) {
  const { email, accountId, signOut } = useCustomerSession();
  const account = useLoadable(() => client.account(), [client]);
  const billing = useLoadable(() => client.billing(), [client]);

  return (
    <Flex vertical gap={16} style={{ maxWidth: 1100, margin: '0 auto', padding: 24 }}>
      <Flex align="center" justify="space-between" gap={16} wrap>
        <div>
          <Typography.Title level={3} style={{ margin: 0 }}>
            seeai 控制台
          </Typography.Title>
          <Typography.Text type="secondary">
            {email}　账户 <Typography.Text code>{accountId}</Typography.Text>
          </Typography.Text>
        </div>
        <Space>
          <Button
            onClick={() => {
              account.reload();
              billing.reload();
            }}
            loading={account.loading || billing.loading}
          >
            刷新
          </Button>
          <Button onClick={signOut}>退出登录</Button>
        </Space>
      </Flex>

      {/* 概览：两个数，一屏可见，不需要滚动也不需要切标签页。 */}
      <Card styles={{ body: { paddingTop: 20 } }}>
        {account.error ? (
          <Alert style={{ marginBottom: 8 }} type="error" showIcon message={account.error} />
        ) : null}
        {billing.error ? <Alert type="error" showIcon message={billing.error} /> : null}
        <Row gutter={[24, 16]}>
          <Col xs={24} sm={12}>
            <Statistic
              data-testid="portal-settled-balance"
              title="已结算余额"
              value={account.data ? yuanText(account.data.balance_microusd) : '—'}
              loading={account.loading}
            />
          </Col>
          <Col xs={24} sm={12}>
            <Statistic
              title={
                <Space size={4}>
                  扣费总额（全部）
                  <Tooltip title="账本里扣费与调整条目的合计。明细与账单汇总在下面的「用量与账单」里。">
                    <InfoCircleOutlined style={{ color: 'rgba(0,0,0,0.45)' }} />
                  </Tooltip>
                </Space>
              }
              value={billing.data ? yuanText(billing.data.charged_microusd) : '—'}
              loading={billing.loading}
            />
          </Col>
        </Row>
        {account.data ? (
          <Typography.Text type="secondary" style={{ fontSize: 12 }}>
            余额写入时刻：{whenText(account.data.updated_at)}
          </Typography.Text>
        ) : null}
      </Card>

      <Tabs
        defaultActiveKey="usage"
        items={[
          { key: 'usage', label: '用量与账单', children: <UsagePanel client={client} /> },
          { key: 'keys', label: 'API Key', children: <KeysPanel client={client} /> },
          {
            key: 'account',
            label: '账户设置',
            children: <AccountSettingsPanel client={client} />,
          },
        ]}
      />
    </Flex>
  );
}

/// 账目流水（充值记录在这里）。金额的**符号是语义的一部分**，所以负的显红。
function LedgerPanel({ client }: { client: CustomerClient }) {
  const ledger = useLoadable(() => client.ledger(50), [client]);

  return (
    <Card
      title="充值记录与账目流水"
      extra={<Button onClick={ledger.reload}>重取</Button>}
    >
      <Typography.Paragraph type="secondary" style={{ marginTop: 0 }}>
        充值由运营在后台完成；扣费在生成请求结算后出现在这里。
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
              message="还没有任何账目。充值由运营在后台完成，完成之后这里会出现一条 credit。"
            />
          ),
        }}
        columns={[
          { title: '时刻', dataIndex: 'created_at', render: (value: string) => whenText(value) },
          {
            title: '类别',
            dataIndex: 'kind',
            render: (value: string) => (
              <Tag color={value === 'credit' ? 'green' : 'default'}>{value}</Tag>
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
        <Typography.Text type="secondary">
          只显示最近 50 条（按时间倒序）。
        </Typography.Text>
      ) : null}
    </Card>
  );
}

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

/// 用量与账单：汇总是**区间全量**，明细按上限截断——两者的口径不同，所以汇总独立于明细的条数。
function UsagePanel({ client }: { client: CustomerClient }) {
  const usage = useLoadable(() => client.usage(50), [client]);
  const billing = useLoadable(() => client.billing(), [client]);

  return (
    <Flex vertical gap={16}>
      <Card
        title="账单汇总"
        extra={<Button onClick={billing.reload}>重取</Button>}
      >
        <Typography.Paragraph type="secondary" style={{ marginTop: 0 }}>
          按整段区间全量计算，不随下面明细的条数变化。
        </Typography.Paragraph>
        {billing.error ? <Alert type="error" showIcon message={billing.error} /> : null}
        <Row gutter={[24, 16]}>
          <Col xs={8}>
            <Statistic
              title="请求数"
              value={billing.data?.requests ?? 0}
              loading={billing.loading}
            />
          </Col>
          <Col xs={8}>
            <Statistic
              title="产出图片数"
              value={billing.data?.images ?? 0}
              loading={billing.loading}
            />
          </Col>
          <Col xs={8}>
            <Statistic
              title="平均每次扣费"
              value={
                billing.data && billing.data.requests > 0
                  ? yuanText(Math.round(billing.data.charged_microusd / billing.data.requests))
                  : '—'
              }
              loading={billing.loading}
            />
          </Col>
        </Row>
      </Card>

      <Card title="逐笔明细" extra={<Button onClick={usage.reload}>重取</Button>}>
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

      <LedgerPanel client={client} />
    </Flex>
  );
}

/// 密钥自助：列、建、吊销。明文只在创建那一次出现。
function KeysPanel({ client }: { client: CustomerClient }) {
  const { message } = AntApp.useApp();
  const keys = useLoadable(() => client.apiKeys(), [client]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [issued, setIssued] = useState<{ api_key: string; key_id: string } | null>(null);
  const [form] = Form.useForm<{ label: string }>();

  return (
    <Card
      title="API Key"
      extra={<Button onClick={keys.reload}>重取</Button>}
    >
      <Typography.Paragraph type="secondary" style={{ marginTop: 0 }}>
        明文只在创建那一次出现，之后谁也拿不回来。吊销立刻生效（不删行）。
      </Typography.Paragraph>
      {error ? <Alert type="error" showIcon message={error} /> : null}
      {keys.error ? <Alert type="error" showIcon message={keys.error} /> : null}
      <Form
        form={form}
        layout="inline"
        onFinish={async (values) => {
          setBusy(true);
          setError(null);
          try {
            const created = await client.issueApiKey(values.label.trim());
            setIssued(created);
            form.resetFields();
            keys.reload();
            message.success('密钥已签发');
          } catch (failure) {
            setError(failure instanceof Error ? failure.message : String(failure));
          } finally {
            setBusy(false);
          }
        }}
      >
        <Form.Item
          name="label"
          label="标签（给自己认的，例如「本地脚本」）"
          rules={[{ required: true, message: '请给密钥起个标签' }]}
        >
          <Input data-testid="portal-key-label" style={{ width: 240 }} />
        </Form.Item>
        <Form.Item>
          <Button
            data-testid="portal-key-create"
            type="primary"
            icon={<PlusOutlined />}
            htmlType="submit"
            loading={busy}
          >
            新建密钥
          </Button>
        </Form.Item>
      </Form>

      {issued ? (
        <Alert
          style={{ marginTop: 16 }}
          type="warning"
          showIcon
          message="密钥明文——只显示这一次，现在就抄走"
          description={
            <Flex vertical gap={4}>
              <Typography.Text
                data-testid="portal-key-plaintext"
                code
                copyable
                style={{ fontSize: 14 }}
              >
                {issued.api_key}
              </Typography.Text>
              <Typography.Text type="secondary">
                密钥标识：
                <Typography.Text code copyable>
                  {issued.key_id}
                </Typography.Text>
              </Typography.Text>
            </Flex>
          }
        />
      ) : null}

      <Table
        style={{ marginTop: 16 }}
        size="small"
        rowKey="key_id"
        loading={keys.loading}
        pagination={false}
        dataSource={keys.data?.keys ?? []}
        locale={{
          emptyText: <Alert type="info" showIcon message="还没有密钥。建一把之后才能调用生成接口。" />,
        }}
        columns={[
          { title: '标签', dataIndex: 'label' },
          { title: '创建时间', dataIndex: 'created_at', render: (value: string) => whenText(value) },
          {
            title: '状态',
            dataIndex: 'revoked_at',
            render: (value: string | null) =>
              value ? <Tag>已吊销（{whenText(value)}）</Tag> : <Tag color="green">可用</Tag>,
          },
          {
            title: '',
            width: 100,
            render: (_value: unknown, key: { key_id: string; revoked_at: string | null }) => {
              if (key.revoked_at) return null;
              const keyId = key.key_id;
              return (
                <Button
                  danger
                  size="small"
                  onClick={async () => {
                    setError(null);
                    try {
                      await client.revokeApiKey(keyId);
                      message.success('已吊销');
                      keys.reload();
                    } catch (failure) {
                      setError(failure instanceof Error ? failure.message : String(failure));
                    }
                  }}
                >
                  吊销
                </Button>
              );
            },
          },
        ]}
      />
    </Card>
  );
}

/// 账户设置：改口令与账号信息。低频动作收在这里，不占首屏。
function AccountSettingsPanel({ client }: { client: CustomerClient }) {
  const { email, accountId, signOut } = useCustomerSession();
  const { message } = AntApp.useApp();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState(false);
  const [form] = Form.useForm<{ current: string; next: string }>();

  return (
    <Flex vertical gap={16}>
      <Card title="账号">
        <Descriptions
          size="small"
          column={1}
          bordered
          items={[
            { key: 'email', label: '登录邮箱', children: email },
            {
              key: 'account',
              label: '账户',
              children: (
                <Typography.Text code copyable>
                  {accountId}
                </Typography.Text>
              ),
            },
          ]}
        />
        <Button style={{ marginTop: 16 }} onClick={signOut}>
          退出登录
        </Button>
      </Card>

      <Card
        title={
          <Space size={8}>
            <LockOutlined />
            改口令
          </Space>
        }
      >
        <Typography.Paragraph type="secondary" style={{ marginTop: 0 }}>
          改完之后此前所有登录会话都会失效，需要用新口令重新登录。
        </Typography.Paragraph>
        <Form
          form={form}
          layout="vertical"
          disabled={done}
          style={{ maxWidth: 420 }}
          onFinish={async (values) => {
            setBusy(true);
            setError(null);
            try {
              await client.changePassword(values.current, values.next);
              setDone(true);
              form.resetFields();
              message.success('口令已改，请用新口令重新登录');
            } catch (failure) {
              const text = failure instanceof Error ? failure.message : String(failure);
              setError(text);
              message.error(text);
            } finally {
              setBusy(false);
            }
          }}
        >
          <Form.Item
            name="current"
            label="当前口令"
            rules={[{ required: true, message: '请输入当前口令' }]}
          >
            <Input.Password
              data-testid="portal-current-password"
              prefix={<LockOutlined />}
              autoComplete="current-password"
            />
          </Form.Item>
          <Form.Item
            name="next"
            label="新口令"
            rules={[
              { required: true, message: '请输入新口令' },
              { min: 8, message: '至少 8 个字符' },
            ]}
          >
            <Input.Password
              data-testid="portal-new-password"
              autoComplete="new-password"
            />
          </Form.Item>
          <Form.Item style={{ marginBottom: 0 }}>
            <Button
              data-testid="portal-change-password"
              type="primary"
              htmlType="submit"
              loading={busy}
            >
              改口令
            </Button>
          </Form.Item>
        </Form>
        {error ? <Alert style={{ marginTop: 12 }} type="error" showIcon message={error} /> : null}
        {done ? (
          <Alert
            style={{ marginTop: 12 }}
            type="success"
            showIcon
            message="口令已改。此前所有登录会话都已失效。"
            action={
              <Button size="small" onClick={signOut}>
                回登录页
              </Button>
            }
          />
        ) : null}
      </Card>

      <Card title="关于充值">
        <Typography.Paragraph type="secondary" style={{ margin: 0 }}>
          平台目前没有在线支付：充值由运营在后台完成，这里只展示充值记录与余额。忘了口令也不能自助
          重置——请找运营签发一枚一次性重置令牌，用它设置新口令。
        </Typography.Paragraph>
      </Card>
    </Flex>
  );
}

/// 密钥面板里那个图标只是提醒"这是密钥"，不承载语义。
export const KEY_ICON = <KeyOutlined />;
