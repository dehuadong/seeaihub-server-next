import {
  Alert,
  App as AntApp,
  Button,
  Card,
  Col,
  Descriptions,
  Divider,
  Flex,
  Form,
  Input,
  Row,
  Space,
  Table,
  Tag,
  Typography,
} from 'antd';
import {
  CreditCardOutlined,
  KeyOutlined,
  PlusOutlined,
  SearchOutlined,
  TagOutlined,
} from '@ant-design/icons';
import { useState } from 'react';
import type { AdminClient } from '../client';
import type {
  AccountBalance,
  CustomerView,
  IssueApiKeyResponse,
  LedgerEntry,
} from '../../shared/types';
import { ConsolePage, Panel, whenText, yuanText } from '../ui';

/// 账户与密钥。建账户、充值、改标签、发/吊销密钥，看余额与流水，以及客户的登录身份。
///
/// 这一页的每一块都**先要有账户标识**：运营的活都是围着某个账户做的。标识来自三种地方——新建账户、
/// 按邮箱找到客户、或从别处抄过来，所以顶部那条"查一个账户"是这一页的入口，下面的操作都取它的值。
export function AccountsPage({ client }: { client: AdminClient }) {
  const { message } = AntApp.useApp();
  const [accountId, setAccountId] = useState('');
  const [balance, setBalance] = useState<AccountBalance | null>(null);
  const [entries, setEntries] = useState<LedgerEntry[]>([]);
  const [truncated, setTruncated] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [issued, setIssued] = useState<IssueApiKeyResponse | null>(null);
  const [revokeId, setRevokeId] = useState('');

  async function read(id: string) {
    setBusy(true);
    setError(null);
    try {
      const [next, ledger] = await Promise.all([
        client.accountBalance(id),
        client.accountEntries(id, 50),
      ]);
      setBalance(next);
      setEntries(ledger.entries);
      setTruncated(ledger.truncated);
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setBusy(false);
    }
  }

  return (
    <ConsolePage title="账户与密钥" error={error}>
      <Panel title="查一个账户" description="读的是账本那一行，不读缓存——运营对账看的就是它。">
        <Space.Compact style={{ width: '100%', maxWidth: 640 }}>
          <Input
            data-testid="accounts-lookup-id"
            value={accountId}
            onChange={(event) => setAccountId(event.target.value)}
            placeholder="账户 id（UUID）"
            prefix={<SearchOutlined />}
            allowClear
          />
          <Button
            type="primary"
            loading={busy}
            disabled={!accountId.trim()}
            onClick={() => void read(accountId.trim())}
          >
            读余额与流水
          </Button>
        </Space.Compact>

        {balance ? (
          <Descriptions
            style={{ marginTop: 16 }}
            size="small"
            bordered
            column={{ xs: 1, sm: 2 }}
            items={[
              {
                key: 'balance',
                label: '余额',
                children: (
                  <Flex vertical>
                    <Typography.Text strong style={{ fontSize: 16 }}>
                      {yuanText(balance.balance_microusd)}
                    </Typography.Text>
                    <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                      {balance.balance_microusd} 微单位
                    </Typography.Text>
                  </Flex>
                ),
              },
              {
                key: 'updated',
                label: '写入时刻',
                children: whenText(balance.updated_at),
              },
            ]}
          />
        ) : null}

        {entries.length > 0 ? (
          <>
            <Table
              style={{ marginTop: 16 }}
              size="small"
              rowKey={(entry, index) => `${entry.created_at}-${index ?? 0}`}
              pagination={false}
              dataSource={entries}
              columns={[
                {
                  title: '时刻',
                  dataIndex: 'created_at',
                  render: (value: string) => whenText(value),
                },
                {
                  title: '类别',
                  dataIndex: 'kind',
                  render: (value: string) => <EntryKindTag kind={value} />,
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
                {
                  title: 'Job',
                  dataIndex: 'job_id',
                  render: (value: string | null) =>
                    value ? <Typography.Text code>{value}</Typography.Text> : '—',
                },
              ]}
            />
            {truncated ? (
              <Typography.Text type="secondary">只显示最近 50 条（按时间倒序）。</Typography.Text>
            ) : null}
          </>
        ) : null}
      </Panel>

      <Panel title="建账户 / 充值 / 标签">
        <Row gutter={[24, 24]}>
          <Col xs={24} lg={8}>
            <CreateAccount
              client={client}
              onCreated={(id) => {
                setAccountId(id);
                message.success(`已建账户 ${id}`);
              }}
            />
          </Col>
          <Col xs={24} lg={8}>
            <Credit client={client} accountId={accountId} />
          </Col>
          <Col xs={24} lg={8}>
            <SetTag client={client} accountId={accountId} />
          </Col>
        </Row>
      </Panel>

      <Panel
        title="API Key"
        description="明文只在这一次响应里出现，事后谁也拿不回来。吊销立刻生效（不删行）。"
      >
        <Space wrap>
          <Button
            icon={<KeyOutlined />}
            disabled={!accountId.trim()}
            onClick={async () => {
              setError(null);
              try {
                setIssued(await client.issueApiKey(accountId.trim(), 'admin-console'));
                message.success('密钥已签发');
              } catch (failure) {
                setError(failure instanceof Error ? failure.message : String(failure));
              }
            }}
          >
            给该账户发一把密钥
          </Button>
          <Space.Compact>
            <Input
              value={revokeId}
              onChange={(event) => setRevokeId(event.target.value)}
              placeholder="要吊销的密钥标识"
              style={{ width: 320 }}
              allowClear
            />
            <Button
              danger
              disabled={!revokeId.trim()}
              onClick={async () => {
                setError(null);
                try {
                  await client.revokeApiKey(revokeId.trim());
                  message.success('已吊销；吊销立刻生效（不删行）。');
                  setRevokeId('');
                } catch (failure) {
                  setError(failure instanceof Error ? failure.message : String(failure));
                }
              }}
            >
              吊销
            </Button>
          </Space.Compact>
        </Space>

        {issued ? (
          <Alert
            style={{ marginTop: 16 }}
            type="warning"
            showIcon
            message="密钥明文——现在抄走，页面刷新之后就没了"
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
      </Panel>

      <CustomersPanel
        client={client}
        onFoundAccount={(id) => setAccountId(id)}
        onError={setError}
      />
    </ConsolePage>
  );
}

/// 账本条目的类别，用颜色区分"进钱"与"出钱"：运营扫一眼就该看出方向。
function EntryKindTag({ kind }: { kind: string }) {
  const color =
    kind === 'credit' ? 'green' : kind === 'capture' || kind === 'cost' ? 'red' : 'default';
  return <Tag color={color}>{kind}</Tag>;
}

function CreateAccount(props: { client: AdminClient; onCreated: (id: string) => void }) {
  const [busy, setBusy] = useState(false);
  const [form] = Form.useForm<{ micros: string }>();

  return (
    <Card size="small" title="建账户" styles={{ body: { paddingTop: 12 } }}>
      <Form
        form={form}
        layout="vertical"
        initialValues={{ micros: '0' }}
        onFinish={async (values) => {
          setBusy(true);
          try {
            const micros = Math.round(Number(values.micros));
            if (!Number.isFinite(micros) || micros < 0) throw new Error('初始余额必须是非负数');
            const created = await props.client.createAccount(micros);
            props.onCreated(created.account_id);
            form.resetFields();
          } finally {
            setBusy(false);
          }
        }}
      >
        <Form.Item
          name="micros"
          label="初始余额（微单位；1 元 = 1000000）"
          rules={[{ required: true, message: '请填初始余额' }]}
        >
          <Input suffix="微单位" />
        </Form.Item>
        <Button type="primary" icon={<PlusOutlined />} htmlType="submit" loading={busy} block>
          建账户
        </Button>
      </Form>
    </Card>
  );
}

function Credit(props: { client: AdminClient; accountId: string }) {
  const { message } = AntApp.useApp();
  const [busy, setBusy] = useState(false);
  const [form] = Form.useForm<{ micros: string; businessKey: string }>();

  return (
    <Card size="small" title="充值" styles={{ body: { paddingTop: 12 } }}>
      <Form
        form={form}
        layout="vertical"
        onFinish={async (values) => {
          setBusy(true);
          try {
            const micros = Math.round(Number(values.micros));
            if (!Number.isFinite(micros) || micros <= 0) throw new Error('充值金额必须是正数');
            await props.client.creditAccount(props.accountId.trim(), micros, values.businessKey);
            message.success(`已充值 ${yuanText(micros)}`);
            form.resetFields();
          } catch (failure) {
            message.error(failure instanceof Error ? failure.message : String(failure));
          } finally {
            setBusy(false);
          }
        }}
      >
        <Form.Item
          name="micros"
          label="金额（微单位）"
          rules={[{ required: true, message: '请填金额' }]}
        >
          <Input suffix="微单位" />
        </Form.Item>
        <Form.Item
          name="businessKey"
          label="业务键（幂等：同一键只充一次）"
          rules={[{ required: true, message: '请填业务键' }]}
        >
          <Input placeholder="topup-2026-09-001" />
        </Form.Item>
        <Button
          type="primary"
          icon={<CreditCardOutlined />}
          htmlType="submit"
          loading={busy}
          disabled={!props.accountId.trim()}
          block
        >
          充值
        </Button>
      </Form>
    </Card>
  );
}

function SetTag(props: { client: AdminClient; accountId: string }) {
  const { message } = AntApp.useApp();
  const [busy, setBusy] = useState(false);
  const [form] = Form.useForm<{ tag: string }>();

  return (
    <Card size="small" title="账户标签" styles={{ body: { paddingTop: 12 } }}>
      <Typography.Paragraph type="secondary" style={{ fontSize: 12 }}>
        标签只被生效的 <code>user_tag</code> 策略消费；没有那条策略时不改变任何选路结果。
      </Typography.Paragraph>
      <Form
        form={form}
        layout="vertical"
        onFinish={async (values) => {
          setBusy(true);
          try {
            await props.client.setAccountTag(props.accountId.trim(), values.tag.trim() || null);
            message.success(values.tag.trim() ? '标签已设置' : '标签已清除');
            form.resetFields();
          } catch (failure) {
            message.error(failure instanceof Error ? failure.message : String(failure));
          } finally {
            setBusy(false);
          }
        }}
      >
        <Form.Item name="tag" label="标签（留空即清除）">
          <Input prefix={<TagOutlined />} placeholder="vip" allowClear />
        </Form.Item>
        <Button
          htmlType="submit"
          loading={busy}
          disabled={!props.accountId.trim()}
          block
        >
          写入标签
        </Button>
      </Form>
    </Card>
  );
}

/// 客户登录身份：替客户开户、按邮箱找账户、签一次性重置令牌。
///
/// 三件事都要先有账户标识：客户自助注册出来的账户只存在于客户表里，运营不查就找不到它——没有这一块，
/// "给新客户充值""帮忘了口令的客户重置"都没有入口。
function CustomersPanel(props: {
  client: AdminClient;
  onFoundAccount: (id: string) => void;
  onError: (message: string) => void;
}) {
  const { message } = AntApp.useApp();
  const [busy, setBusy] = useState(false);
  const [found, setFound] = useState<CustomerView | null>(null);
  const [issued, setIssued] = useState<{ reset_token: string; expires_at: string } | null>(null);
  const [form] = Form.useForm<{ email: string; password?: string; accountId?: string }>();

  return (
    <Panel
      title="客户登录身份"
      description="一个客户邮箱对应一个账户。不填账户标识就新建一个空账户；填了就把它配到那个已有账户上（配身份不动余额、密钥与历史）。不填口令时运营改用重置令牌让客户自己设。"
    >
      <Form form={form} layout="vertical" onFinish={async (values) => {
        setBusy(true);
        try {
          const created = await props.client.openCustomer(
            values.email.trim(),
            values.password || undefined,
            values.accountId?.trim() || undefined,
          );
          setFound(created);
          props.onFoundAccount(created.account_id);
          message.success(`已开户：${created.email}`);
          form.resetFields(['password', 'accountId']);
        } catch (failure) {
          props.onError(failure instanceof Error ? failure.message : String(failure));
        } finally {
          setBusy(false);
        }
      }}>
        <Row gutter={16}>
          <Col xs={24} md={8}>
            <Form.Item
              name="email"
              label="客户邮箱"
              rules={[{ required: true, message: '请填客户邮箱' }]}
            >
              <Input placeholder="customer@example.com" />
            </Form.Item>
          </Col>
          <Col xs={24} md={8}>
            <Form.Item name="password" label="初始口令（可空）">
              <Input.Password placeholder="至少 8 个字符" autoComplete="new-password" />
            </Form.Item>
          </Col>
          <Col xs={24} md={8}>
            <Form.Item name="accountId" label="已有账户标识（可空）">
              <Input placeholder="留空即新建空账户" allowClear />
            </Form.Item>
          </Col>
        </Row>
        <Space wrap>
          <Button type="primary" htmlType="submit" loading={busy} icon={<PlusOutlined />}>
            开户
          </Button>
          <Button
            icon={<SearchOutlined />}
            onClick={async () => {
              const email = form.getFieldValue('email') as string | undefined;
              if (!email?.trim()) {
                message.warning('先填客户邮箱');
                return;
              }
              setBusy(true);
              try {
                const result = await props.client.findCustomer(email.trim());
                const first = result.customers[0];
                if (!first) {
                  setFound(null);
                  message.info(`没有找到 ${email.trim()} 的登录身份`);
                  return;
                }
                setFound(first);
                props.onFoundAccount(first.account_id);
                form.setFieldValue('accountId', first.account_id);
              } catch (failure) {
                props.onError(failure instanceof Error ? failure.message : String(failure));
              } finally {
                setBusy(false);
              }
            }}
          >
            按邮箱找账户
          </Button>
        </Space>
      </Form>

      {found ? (
        <>
          <Divider />
          <Descriptions
            size="small"
            bordered
            column={{ xs: 1, sm: 3 }}
            items={[
              { key: 'email', label: '客户', children: found.email },
              {
                key: 'account',
                label: '账户',
                children: (
                  <Typography.Text code copyable>
                    {found.account_id}
                  </Typography.Text>
                ),
              },
              { key: 'last', label: '上次登录', children: whenText(found.last_login_at) },
            ]}
          />
          <Button
            style={{ marginTop: 12 }}
            icon={<KeyOutlined />}
            onClick={async () => {
              setBusy(true);
              try {
                const token = await props.client.issueCustomerPasswordReset(found.account_id);
                setIssued(token);
                message.success('已签发一次性重置令牌——请当面或经既有渠道转交客户');
              } catch (failure) {
                props.onError(failure instanceof Error ? failure.message : String(failure));
              } finally {
                setBusy(false);
              }
            }}
          >
            签发重置令牌
          </Button>
        </>
      ) : null}

      {issued ? (
        <Alert
          style={{ marginTop: 12 }}
          type="warning"
          showIcon
          message="重置令牌（只显示这一次，转交客户后由他设置新口令）"
          description={
            <Flex vertical gap={4}>
              <Typography.Text code copyable style={{ fontSize: 14 }}>
                {issued.reset_token}
              </Typography.Text>
              <Typography.Text type="secondary">
                有效期至 {whenText(issued.expires_at)}；用过一次即失效。
              </Typography.Text>
            </Flex>
          }
        />
      ) : null}
    </Panel>
  );
}
