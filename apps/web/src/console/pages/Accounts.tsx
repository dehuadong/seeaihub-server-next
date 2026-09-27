import {
  Alert,
  App as AntApp,
  Button,
  Card,
  Col,
  Descriptions,
  Divider,
  Drawer,
  Flex,
  Form,
  Input,
  Row,
  Space,
  Table,
  Tag,
  Typography,
} from 'antd';
import { CreditCardOutlined, KeyOutlined, PlusOutlined, SearchOutlined } from '@ant-design/icons';
import { useState } from 'react';
import type { AdminClient } from '../client';
import type { AccountSummary, IssueApiKeyResponse, LedgerEntry } from '../../shared/types';
import { useLoadable } from '../../shared/ui';
import { ConsolePage, Panel, whenText, yuanText } from '../ui';

/// 账户：**先搜到，再操作**。
///
/// 组织依据是运营的工作顺序，不是端点：列表回答"有哪些账户、各自多少余额"，详情回答"这个账户能做
/// 什么"（充值、标签、密钥、流水）。原来把六件事堆在一页长滚动里，运营读完余额要滚下去充值、再滚
/// 上来核对；而"查账户"只收一个 UUID——运营手上没有 UUID，他们有的是客户邮箱或自己设的标签。
///
/// 口径见 `docs/design/0011-console-information-architecture.md` §1.2 与 §3.2。
export function AccountsPage({ client }: { client: AdminClient }) {
  const { message } = AntApp.useApp();
  const [email, setEmail] = useState('');
  const [tag, setTag] = useState('');
  const [directId, setDirectId] = useState('');
  const [filter, setFilter] = useState<{ email?: string; tag?: string }>({});
  const [selected, setSelected] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);

  const accounts = useLoadable(() => client.listAccounts({ ...filter, limit: 100 }), [client, filter]);
  // 邮箱在客户身份那边。列表要能显示它：运营是**按邮箱**找账户的，搜出来的行必须能确认是谁。
  const customers = useLoadable(() => client.listCustomers(200), [client]);
  const emailOf = (accountId: string): string | null =>
    customers.data?.customers.find((item) => item.account_id === accountId)?.email ?? null;

  async function createAccount(initialMicros: number) {
    setCreating(true);
    try {
      const created = await client.createAccount(initialMicros);
      message.success(`已建账户 ${created.account_id}`);
      setSelected(created.account_id);
      accounts.reload();
    } catch (failure) {
      message.error(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setCreating(false);
    }
  }

  return (
    <ConsolePage
      title="账户"
      hint="按邮箱或标签找到账户，再在详情里充值、看流水、管密钥"
      error={accounts.error}
      loading={accounts.loading}
      onReload={accounts.reload}
      extra={
        <Button
          type="primary"
          icon={<PlusOutlined />}
          loading={creating}
          onClick={() => void createAccount(0)}
        >
          建空账户
        </Button>
      }
    >
      <Panel title="找账户" description="两个条件都填时是「与」的关系。标签是运营自己设的，邮箱来自客户登录身份。">
        {/* 用普通表单而不是 antd 的 `Form`：这一处只需要"两个输入框 + 一个动作"，
            取的是**点击那一刻**的值，不需要校验、不需要受控字段。少一层托管就少一处说不清。 */}
        <Flex gap={12} wrap align="flex-end">
          <Flex vertical gap={4}>
            <Typography.Text type="secondary" style={{ fontSize: 12 }}>
              客户邮箱
            </Typography.Text>
            <Input
              data-testid="accounts-lookup-email"
              prefix={<SearchOutlined />}
              placeholder="customer@example.com"
              value={email}
              onChange={(event) => setEmail(event.target.value)}
              onPressEnter={() => setFilter({ email, tag })}
              style={{ width: 240 }}
              allowClear
            />
          </Flex>
          <Flex vertical gap={4}>
            <Typography.Text type="secondary" style={{ fontSize: 12 }}>
              标签
            </Typography.Text>
            <Input
              data-testid="accounts-lookup-tag"
              placeholder="vip"
              value={tag}
              onChange={(event) => setTag(event.target.value)}
              onPressEnter={() => setFilter({ email, tag })}
              style={{ width: 160 }}
              allowClear
            />
          </Flex>
          <Space>
            <Button
              data-testid="accounts-search"
              type="primary"
              onClick={() => setFilter({ email, tag })}
            >
              查找
            </Button>
            <Button
              onClick={() => {
                setEmail('');
                setTag('');
                setFilter({});
              }}
            >
              清空
            </Button>
          </Space>
        </Flex>
        {/* 收着旧的按 id 直达：列表不可用时（例如刚拿到一个 id）仍要能查。 */}
        <Divider plain style={{ marginBlock: 16 }}>
          或按账户标识直达
        </Divider>
        <Space.Compact style={{ maxWidth: 560, width: '100%' }}>
          <Input
            data-testid="accounts-lookup-id"
            value={directId}
            onChange={(event) => setDirectId(event.target.value)}
            placeholder="账户 id（UUID）"
            allowClear
          />
          <Button
            data-testid="accounts-open-by-id"
            disabled={!directId.trim()}
            onClick={() => setSelected(directId.trim())}
          >
            打开
          </Button>
        </Space.Compact>
      </Panel>

      <Panel
        title="账户列表"
        description={`共 ${accounts.data?.accounts.length ?? 0} 个（最多显示最近 100 个）。`}
        extra={<Button onClick={accounts.reload}>重取</Button>}
      >
        <Table<AccountSummary>
          size="small"
          rowKey="account_id"
          loading={accounts.loading}
          pagination={false}
          dataSource={accounts.data?.accounts ?? []}
          locale={{
            emptyText: (
              <Alert
                type="info"
                showIcon
                message={
                  filter.email || filter.tag
                    ? '没有符合条件的账户。清空筛选看看全部账户。'
                    : '还没有任何账户。点右上角「建空账户」，或让客户自己注册。'
                }
              />
            ),
          }}
          columns={[
            {
              title: '客户邮箱',
              key: 'email',
              width: 220,
              render: (_value: unknown, account: AccountSummary) => {
                const email = emailOf(account.account_id);
                // 运营直接建的账户还没有登录身份——如实说"没有"，不显示空白让人以为是加载失败。
                return email ? (
                  <Typography.Text>{email}</Typography.Text>
                ) : (
                  <Typography.Text type="secondary">（没有登录身份）</Typography.Text>
                );
              },
            },
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
              title: '标签',
              dataIndex: 'tag',
              width: 140,
              render: (value: string | null) => (value ? <Tag color="blue">{value}</Tag> : '—'),
            },
            {
              title: '余额',
              dataIndex: 'balance_microusd',
              align: 'right',
              width: 160,
              render: (value: number) => (
                <Typography.Text strong>{yuanText(value)}</Typography.Text>
              ),
            },
            {
              title: '创建时间',
              dataIndex: 'created_at',
              width: 200,
              render: (value: string) => whenText(value),
            },
            {
              title: '',
              width: 90,
              render: (_value: unknown, account: AccountSummary) => (
                <Button size="small" onClick={() => setSelected(account.account_id)}>
                  打开
                </Button>
              ),
            },
          ]}
        />
      </Panel>

      <AccountDrawer
        client={client}
        accountId={selected}
        onClose={() => setSelected(null)}
        onChanged={accounts.reload}
      />
    </ConsolePage>
  );
}

/// 一个账户的详情与可做的动作，按**频次**排序：余额与持有在最上，充值其次，其余在后。
function AccountDrawer(props: {
  client: AdminClient;
  accountId: string | null;
  onClose: () => void;
  onChanged: () => void;
}) {
  const { accountId } = props;
  const open = accountId !== null;

  return (
    <Drawer
      open={open}
      onClose={props.onClose}
      width={720}
      title={accountId ? `账户 ${accountId}` : ''}
      destroyOnHidden
    >
      {accountId ? (
        <AccountDetail
          key={accountId}
          client={props.client}
          accountId={accountId}
          onChanged={props.onChanged}
        />
      ) : null}
    </Drawer>
  );
}

function AccountDetail({
  client,
  accountId,
  onChanged,
}: {
  client: AdminClient;
  accountId: string;
  onChanged: () => void;
}) {
  const { message } = AntApp.useApp();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [issued, setIssued] = useState<IssueApiKeyResponse | null>(null);
  const balance = useLoadable(() => client.accountBalance(accountId), [client, accountId]);
  const entries = useLoadable(() => client.accountEntries(accountId, 50), [client, accountId]);

  return (
    <Flex vertical gap={16}>
      {error ? <Alert type="error" showIcon message={error} /> : null}
      {balance.error ? <Alert type="error" showIcon message={balance.error} /> : null}

      <Descriptions
        size="small"
        bordered
        column={1}
        items={[
          {
            key: 'balance',
            label: '余额',
            children: balance.data ? yuanText(balance.data.balance_microusd) : '—',
          },
          {
            key: 'updated',
            label: '写入时刻',
            children: balance.data ? whenText(balance.data.updated_at) : '—',
          },
        ]}
      />

      <Card
        size="small"
        title={
          <Space size={8}>
            <CreditCardOutlined />
            充值
          </Space>
        }
      >
        <Form
          layout="inline"
          onFinish={async (values: { yuan: string; businessKey: string }) => {
            setBusy(true);
            setError(null);
            try {
              // 运营按元填、请求收微单位。四舍五入到整数微单位，避免浮点尾巴。
              const micros = Math.round(Number(values.yuan) * 1_000_000);
              if (!Number.isFinite(micros) || micros <= 0) throw new Error('充值金额必须是正数');
              await client.creditAccount(accountId, micros, values.businessKey);
              message.success(`已充值 ${yuanText(micros)}`);
              balance.reload();
              entries.reload();
              onChanged();
            } catch (failure) {
              setError(failure instanceof Error ? failure.message : String(failure));
            } finally {
              setBusy(false);
            }
          }}
        >
          <Form.Item
            name="yuan"
            label="金额（元）"
            rules={[{ required: true, message: '请填金额' }]}
          >
            <Input data-testid="accounts-credit-yuan" style={{ width: 140 }} placeholder="100" />
          </Form.Item>
          <Form.Item
            name="businessKey"
            label="业务键"
            tooltip="幂等：同一个键只会充一次，误点两下不会充两次"
            rules={[{ required: true, message: '请填业务键' }]}
          >
            <Input
              data-testid="accounts-credit-key"
              style={{ width: 200 }}
              placeholder="topup-2026-09-001"
            />
          </Form.Item>
          <Form.Item>
            <Button
              data-testid="accounts-credit-submit"
              type="primary"
              htmlType="submit"
              loading={busy}
            >
              充值
            </Button>
          </Form.Item>
        </Form>
      </Card>

      <Card size="small" title="账目流水" extra={<Button onClick={entries.reload}>重取</Button>}>
        {entries.error ? <Alert type="error" showIcon message={entries.error} /> : null}
        <Table<LedgerEntry>
          size="small"
          rowKey={(entry, index) => `${entry.created_at}-${index ?? 0}`}
          loading={entries.loading}
          pagination={false}
          dataSource={entries.data?.entries ?? []}
          locale={{ emptyText: <Alert type="info" showIcon message="这个账户还没有任何账目。" /> }}
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
      </Card>

      <Card size="small" title="标签">
        <Form
          layout="inline"
          onFinish={async (values: { tag?: string }) => {
            setBusy(true);
            try {
              await client.setAccountTag(accountId, values.tag?.trim() || null);
              message.success(values.tag?.trim() ? '标签已设置' : '标签已清除');
              onChanged();
            } catch (failure) {
              setError(failure instanceof Error ? failure.message : String(failure));
            } finally {
              setBusy(false);
            }
          }}
        >
          <Form.Item name="tag" tooltip="只被生效的 user_tag 策略消费；没有那条策略时不改变任何选路结果">
            <Input style={{ width: 200 }} placeholder="vip" allowClear />
          </Form.Item>
          <Form.Item>
            <Button htmlType="submit" loading={busy}>
              写入标签
            </Button>
          </Form.Item>
        </Form>
      </Card>

      <Card
        size="small"
        title={
          <Space size={8}>
            <KeyOutlined />
            API Key
          </Space>
        }
      >
        <Flex vertical gap={12}>
          <Row gutter={8}>
            <Col>
              <Button
                disabled={busy}
                onClick={async () => {
                  setBusy(true);
                  setError(null);
                  try {
                    setIssued(await client.issueApiKey(accountId, 'admin-console'));
                    message.success('密钥已签发');
                  } catch (failure) {
                    setError(failure instanceof Error ? failure.message : String(failure));
                  } finally {
                    setBusy(false);
                  }
                }}
              >
                签发一把密钥
              </Button>
            </Col>
          </Row>
          {issued ? (
            <Alert
              type="warning"
              showIcon
              message="密钥明文——现在抄走，关掉这个抽屉就没了"
              description={
                <Flex vertical gap={4}>
                  <Typography.Text code copyable style={{ fontSize: 14 }}>
                    {issued.api_key}
                  </Typography.Text>
                  <Typography.Text type="secondary">
                    密钥标识（吊销用它）：
                    <Typography.Text code copyable>
                      {issued.key_id}
                    </Typography.Text>
                  </Typography.Text>
                </Flex>
              }
            />
          ) : null}
          <Form
            layout="inline"
            onFinish={async (values: { keyId: string }) => {
              setBusy(true);
              try {
                await client.revokeApiKey(values.keyId.trim());
                message.success('已吊销；吊销立刻生效（不删行）');
              } catch (failure) {
                setError(failure instanceof Error ? failure.message : String(failure));
              } finally {
                setBusy(false);
              }
            }}
          >
            <Form.Item name="keyId" rules={[{ required: true, message: '请填密钥标识' }]}>
              <Input style={{ width: 320 }} placeholder="要吊销的密钥标识" allowClear />
            </Form.Item>
            <Form.Item>
              <Button danger htmlType="submit" loading={busy}>
                吊销
              </Button>
            </Form.Item>
          </Form>
        </Flex>
      </Card>
    </Flex>
  );
}
