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
  Table,
  Typography,
} from 'antd';
import { KeyOutlined, PlusOutlined, SearchOutlined } from '@ant-design/icons';
import { useState } from 'react';
import type { AdminClient } from '../client';
import type { CustomerView } from '../../shared/types';
import { useLoadable } from '../../shared/ui';
import { ConsolePage, Panel, whenText } from '../ui';

/// 客户：登录身份的开立与找回口令。
///
/// 它与"账户"是两件事：账户是账本上的一行（余额、流水、密钥），客户是**登录身份**（邮箱 → 账户）。
/// 一个账户可以没有登录身份（运营直接建的），一个身份只指向一个账户。原来把这一块塞在账户页最下面，
/// 两边都不完整——Spec M5 要求"按邮箱找到客户账户"，那是客户区的主入口，不是账户页的附属。
///
/// 口径见 `docs/design/0011-console-information-architecture.md` §1.2 与 §3.3。
export function CustomersPage({ client }: { client: AdminClient }) {
  const { message } = AntApp.useApp();
  const [filter, setFilter] = useState<{ email?: string }>({});
  const [selected, setSelected] = useState<CustomerView | null>(null);
  const [opening, setOpening] = useState(false);
  const [reset, setReset] = useState<{ reset_token: string; expires_at: string } | null>(null);

  const customers = useLoadable(
    () =>
      filter.email?.trim()
        ? client.findCustomer(filter.email.trim())
        : client.listCustomers(100),
    [client, filter],
  );

  return (
    <ConsolePage
      title="客户"
      hint="客户是登录身份：邮箱 → 账户。开立之后客户能自己登录、管密钥、看账务"
      error={customers.error}
      loading={customers.loading}
      onReload={customers.reload}
    >
      {reset ? (
        <Alert
          type="warning"
          showIcon
          closable
          onClose={() => setReset(null)}
          message="重置令牌——只显示这一次，当场转交客户"
          description={
            <Flex vertical gap={4}>
              <Typography.Text data-testid="customers-reset-token" code copyable style={{ fontSize: 14 }}>
                {reset.reset_token}
              </Typography.Text>
              <Typography.Text type="secondary">
                有效期至 {whenText(reset.expires_at)}；用过一次即失效。平台不发邮件，请用你与客户
                已有的渠道转交。
              </Typography.Text>
            </Flex>
          }
        />
      ) : null}

      <Panel
        title="开户"
        description="不填账户标识就新建一个空账户；填了就把它配到那个已有账户上（配身份不动余额、密钥与历史）。不填初始口令时，改用重置令牌让客户自己设。"
      >
        <Form
          layout="vertical"
          onFinish={async (values: { email: string; password?: string; accountId?: string }) => {
            setOpening(true);
            try {
              const created = await client.openCustomer(
                values.email.trim(),
                values.password || undefined,
                values.accountId?.trim() || undefined,
              );
              setSelected(created);
              message.success(`已开户：${created.email}`);
              customers.reload();
            } catch (failure) {
              message.error(failure instanceof Error ? failure.message : String(failure));
            } finally {
              setOpening(false);
            }
          }}
        >
          <Row gutter={16}>
            <Col xs={24} md={8}>
              <Form.Item
                name="email"
                label="客户邮箱"
                rules={[
                  { required: true, message: '请填客户邮箱' },
                  { type: 'email', message: '不像一个邮箱' },
                ]}
              >
                <Input data-testid="customers-open-email" placeholder="customer@example.com" />
              </Form.Item>
            </Col>
            <Col xs={24} md={8}>
              <Form.Item
                name="password"
                label="初始口令（可空）"
                rules={[{ min: 8, message: '至少 8 个字符' }]}
              >
                <Input.Password
                  data-testid="customers-open-password"
                  placeholder="至少 8 个字符"
                  autoComplete="new-password"
                />
              </Form.Item>
            </Col>
            <Col xs={24} md={8}>
              <Form.Item
                name="accountId"
                label="绑到已有账户（可空）"
                tooltip="留空即新建一个空账户；填了就把登录身份配到那个账户上"
              >
                <Input placeholder="账户 id（UUID）" allowClear />
              </Form.Item>
            </Col>
          </Row>
          <Form.Item style={{ marginBottom: 0 }}>
            <Button
              data-testid="customers-open-submit"
              type="primary"
              icon={<PlusOutlined />}
              htmlType="submit"
              loading={opening}
            >
              开户
            </Button>
          </Form.Item>
        </Form>
      </Panel>

      <Panel
        title="找客户"
        description="邮箱精确匹配、大小写不敏感。留空即列出最近的客户。"
        extra={<Button onClick={customers.reload}>重取</Button>}
      >
        <Form
          layout="inline"
          style={{ marginBottom: 16 }}
          onFinish={(values: { email?: string }) => setFilter({ email: values.email })}
        >
          <Form.Item name="email">
            <Input
              data-testid="customers-search-email"
              prefix={<SearchOutlined />}
              placeholder="customer@example.com"
              style={{ width: 280 }}
              allowClear
            />
          </Form.Item>
          <Form.Item>
            <Space>
              <Button type="primary" htmlType="submit">
                查找
              </Button>
              <Button onClick={() => setFilter({})}>清空</Button>
            </Space>
          </Form.Item>
        </Form>

        <Table<CustomerView>
          size="small"
          rowKey="customer_id"
          loading={customers.loading}
          pagination={false}
          dataSource={customers.data?.customers ?? []}
          locale={{
            emptyText: (
              <Alert
                type="info"
                showIcon
                message={
                  filter.email?.trim()
                    ? '没有这个邮箱的登录身份。用上面的「开户」建一个。'
                    : '还没有任何客户登录身份。客户可以自己注册，也可以用上面的「开户」。'
                }
              />
            ),
          }}
          columns={[
            { title: '邮箱', dataIndex: 'email' },
            {
              title: '账户',
              dataIndex: 'account_id',
              render: (value: string) => (
                <Typography.Text code copyable style={{ fontSize: 12 }}>
                  {value}
                </Typography.Text>
              ),
            },
            {
              title: '创建时间',
              dataIndex: 'created_at',
              width: 200,
              render: (value: string) => whenText(value),
            },
            {
              title: '上次登录',
              dataIndex: 'last_login_at',
              width: 200,
              render: (value: string | null) => whenText(value),
            },
            {
              title: '',
              width: 150,
              render: (_value: unknown, customer: CustomerView) => (
                <Space>
                  <Button size="small" onClick={() => setSelected(customer)}>
                    详情
                  </Button>
                </Space>
              ),
            },
          ]}
        />
      </Panel>

      {selected ? (
        <Card
          title={`客户 ${selected.email}`}
          extra={
            <Button type="text" onClick={() => setSelected(null)}>
              收起
            </Button>
          }
        >
          <Descriptions
            size="small"
            bordered
            column={{ xs: 1, sm: 2 }}
            items={[
              { key: 'email', label: '邮箱', children: selected.email },
              {
                key: 'account',
                label: '账户',
                children: (
                  <Typography.Text code copyable>
                    {selected.account_id}
                  </Typography.Text>
                ),
              },
              { key: 'last', label: '上次登录', children: whenText(selected.last_login_at) },
            ]}
          />
          <Space style={{ marginTop: 16 }} wrap>
            <Button
              data-testid="customers-issue-reset"
              icon={<KeyOutlined />}
              onClick={async () => {
                try {
                  setReset(await client.issueCustomerPasswordReset(selected.account_id));
                  message.success('已签发一次性重置令牌');
                } catch (failure) {
                  message.error(failure instanceof Error ? failure.message : String(failure));
                }
              }}
            >
              签发重置令牌
            </Button>
            <Typography.Text type="secondary">
              客户忘了口令时用这个；平台不发邮件，令牌要靠你转交。
            </Typography.Text>
          </Space>
        </Card>
      ) : null}
    </ConsolePage>
  );
}
