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
  Statistic,
  Table,
  Tag,
  Typography,
} from 'antd';
import { KeyOutlined, LockOutlined, PlusOutlined } from '@ant-design/icons';
import { useState } from 'react';
import type { CustomerClient } from './client';
import { useCustomerSession } from './session';
import { whenText, yuanText } from '../shared/format';
import { useLoadable } from '../shared/ui';

/// 客户控制台的主体：余额与持有、改口令、密钥自助、用量与账单（Spec C5、C7–C10）。
///
/// 每个板块各取各的数据，一个板块失败不影响别的；金额与时间只在展示层换算，判断都用服务端给的数。
export function Dashboard({ client }: { client: CustomerClient }) {
  const { email, accountId, signOut } = useCustomerSession();

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
        <Button onClick={signOut}>退出登录</Button>
      </Flex>

      <AccountPanel client={client} />
      <PasswordPanel client={client} />
      <KeysPanel client={client} />
      <UsagePanel client={client} />
    </Flex>
  );
}

/// 改自己的口令（Spec C4）。改完**该客户此前所有会话都失效**，所以只能回到登录页重新登录。
function PasswordPanel({ client }: { client: CustomerClient }) {
  const { signOut } = useCustomerSession();
  const { message } = AntApp.useApp();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState(false);
  const [form] = Form.useForm<{ current: string; next: string }>();

  return (
    <Card title="改口令">
      <Form
        form={form}
        layout="inline"
        disabled={done}
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
            style={{ width: 200 }}
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
            style={{ width: 200 }}
            autoComplete="new-password"
          />
        </Form.Item>
        <Form.Item>
          <Button data-testid="portal-change-password" type="primary" htmlType="submit" loading={busy}>
            改口令
          </Button>
        </Form.Item>
      </Form>
      {error ? <Alert style={{ marginTop: 8 }} type="error" showIcon message={error} /> : null}
      {done ? (
        <Alert
          style={{ marginTop: 8 }}
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
  );
}

/// 余额与持有中。**两个数分开**：持有中是已预授权、还没扣的部分，不是可用额的一部分。
function AccountPanel({ client }: { client: CustomerClient }) {
  const account = useLoadable(() => client.account(), [client]);
  const ledger = useLoadable(() => client.ledger(20), [client]);

  return (
    <Card
      title="余额与持有"
      extra={
        <Button
          onClick={() => {
            account.reload();
            ledger.reload();
          }}
        >
          重取
        </Button>
      }
    >
      {account.error ? <Alert type="error" showIcon message={account.error} /> : null}
      {account.data ? (
        <Row gutter={[24, 16]}>
          <Col xs={12} md={8}>
            <Statistic title="可用余额" value={yuanText(account.data.balance_microusd)} />
          </Col>
          <Col xs={12} md={8}>
            <Statistic
              title="持有中（已预授权、还没结算）"
              value={yuanText(account.data.held_microusd)}
            />
          </Col>
          <Col xs={24} md={8}>
            <Descriptions
              size="small"
              column={1}
              items={[
                { key: 'updated', label: '写入时刻', children: whenText(account.data.updated_at) },
              ]}
            />
          </Col>
        </Row>
      ) : null}

      <Typography.Title level={5} style={{ marginTop: 24 }}>
        充值记录与账目流水
      </Typography.Title>
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
          {
            title: '时刻',
            dataIndex: 'created_at',
            render: (value: string) => whenText(value),
          },
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
    </Card>
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
                密钥标识：<Typography.Text code copyable>{issued.key_id}</Typography.Text>
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
          emptyText: (
            <Alert type="info" showIcon message="还没有密钥。建一把之后才能调用生成接口。" />
          ),
        }}
        columns={[
          { title: '标签', dataIndex: 'label' },
          {
            title: '创建时间',
            dataIndex: 'created_at',
            render: (value: string) => whenText(value),
          },
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

/// 用量与账单：逐笔明细按时间倒序、汇总按区间全量。
function UsagePanel({ client }: { client: CustomerClient }) {
  const usage = useLoadable(() => client.usage(50), [client]);
  const billing = useLoadable(() => client.billing(), [client]);

  return (
    <Card
      title="用量与账单"
      extra={
        <Button
          onClick={() => {
            usage.reload();
            billing.reload();
          }}
        >
          重取
        </Button>
      }
    >
      {billing.error ? <Alert type="error" showIcon message={billing.error} /> : null}
      {usage.error ? <Alert type="error" showIcon message={usage.error} /> : null}
      {billing.data ? (
        <Row gutter={[24, 16]}>
          <Col xs={8}>
            <Statistic title="请求数（全部）" value={billing.data.requests} />
          </Col>
          <Col xs={8}>
            <Statistic title="产出图片数" value={billing.data.images} />
          </Col>
          <Col xs={8}>
            <Statistic title="扣费总额" value={yuanText(billing.data.charged_microusd)} />
          </Col>
        </Row>
      ) : null}
      <Typography.Paragraph type="secondary" style={{ marginTop: 12, marginBottom: 0 }}>
        汇总按整段区间全量计算，不随下面明细的条数变化。
      </Typography.Paragraph>

      <Table
        style={{ marginTop: 16 }}
        size="small"
        rowKey={(row, index) => `${row.created_at}-${index ?? 0}`}
        loading={usage.loading}
        pagination={false}
        scroll={{ x: 'max-content' }}
        dataSource={usage.data?.usage ?? []}
        locale={{
          emptyText: <Alert type="info" showIcon message="还没有任何调用记录。" />,
        }}
        columns={[
          {
            title: '时刻',
            dataIndex: 'created_at',
            render: (value: string) => whenText(value),
          },
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

/// 密钥面板里那个图标只是提醒"这是密钥"，不承载语义。
export const KEY_ICON = <KeyOutlined />;
