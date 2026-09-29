import {
  Alert,
  App as AntApp,
  Button,
  Card,
  Divider,
  Flex,
  Form,
  Input,
  Modal,
  Space,
  Table,
  Tag,
  Typography,
} from 'antd';
import { CreditCardOutlined, KeyOutlined, PlusOutlined, SearchOutlined } from '@ant-design/icons';
import { useState } from 'react';
import type { AdminClient } from '../client';
import type {
  AccountSummary,
  AdminUsageRow,
  IssueApiKeyResponse,
  LedgerEntry,
} from '../../shared/types';
import { useLoadable } from '../../shared/ui';
import { ConsolePage, Panel, whenText, yuanText } from '../ui';

/// 账户：**先搜到，再操作**。
///
/// 上面找账户、下面直接给三件事——**充值 / 账目流水 / API Key**。没有"打开"这一步、也不弹抽屉：
/// 选中一个账户（点那一行）就在同一页给这三块。运营入口是**客户邮箱与标签**，不是账户标识
/// （标识是 UUID，运营手上没有它）。
///
/// 充值的**幂等键由平台生成**：运营只填金额，双击只充一次。
export function AccountsPage({ client }: { client: AdminClient }) {
  const { message } = AntApp.useApp();
  const [email, setEmail] = useState('');
  const [tag, setTag] = useState('');
  const [directId, setDirectId] = useState('');
  const [filter, setFilter] = useState<{ email?: string; tag?: string }>({});
  const [selected, setSelected] = useState<string | null>(null);
  const [createOpen, setCreateOpen] = useState(false);
  const [creating, setCreating] = useState(false);
  const [createForm] = Form.useForm<{ tag?: string; yuan?: string }>();

  const accounts = useLoadable(() => client.listAccounts({ ...filter, limit: 100 }), [client, filter]);

  /// 建账户：**确认才建**。不填标签时之后只能用账户标识找它——表单里点明这一点。
  async function createAccount(values: { tag?: string; yuan?: string }) {
    setCreating(true);
    try {
      const yuan = Number(values.yuan ?? 0);
      if (!Number.isFinite(yuan) || yuan < 0) throw new Error('初始充值不能是负数');
      const created = await client.createAccount(Math.round(yuan * 1_000_000));
      const label = values.tag?.trim();
      if (label) await client.setAccountTag(created.account_id, label);
      message.success(`已建账户 ${created.account_id}`);
      createForm.resetFields();
      setCreateOpen(false);
      setSelected(created.account_id);
      accounts.reload();
    } catch (failure) {
      message.error(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setCreating(false);
    }
  }

  const selectedSummary = accounts.data?.accounts.find(
    (account) => account.account_id === selected,
  );

  return (
    <ConsolePage
      title="账户"
      hint="按邮箱或标签找到账户，选中它就能充值、看流水、管密钥"
      error={accounts.error}
      loading={accounts.loading}
      onReload={accounts.reload}
      extra={
        <Button
          type="primary"
          icon={<PlusOutlined />}
          data-testid="accounts-create-open"
          onClick={() => setCreateOpen(true)}
        >
          建账户
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
            查看
          </Button>
        </Space.Compact>
      </Panel>

      <Panel
        title="账户列表"
        description={`共 ${accounts.data?.accounts.length ?? 0} 个（最多显示最近 100 个）。点一行看它下面那三块。`}
        extra={<Button onClick={accounts.reload}>重取</Button>}
      >
        <Table<AccountSummary>
          size="small"
          rowKey="account_id"
          loading={accounts.loading}
          pagination={false}
          dataSource={accounts.data?.accounts ?? []}
          onRow={(account) => ({
            onClick: () => setSelected(account.account_id),
            style: { cursor: 'pointer' },
          })}
          rowClassName={(account) =>
            account.account_id === selected ? 'ant-table-row-selected' : ''
          }
          locale={{
            emptyText: (
              <Alert
                type="info"
                showIcon
                message={
                  filter.email || filter.tag
                    ? '没有符合条件的账户。清空筛选看看全部账户。'
                    : '还没有任何账户。点右上角「建账户」，或让客户自己注册。'
                }
              />
            ),
          }}
          columns={[
            {
              title: '客户邮箱',
              dataIndex: 'email',
              width: 220,
              render: (value: string | null) =>
                value ? (
                  <Typography.Text>{value}</Typography.Text>
                ) : (
                  <Typography.Text type="secondary">（没有登录身份）</Typography.Text>
                ),
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
          ]}
        />
      </Panel>

      {selected ? (
        <AccountActions
          key={selected}
          client={client}
          accountId={selected}
          tag={selectedSummary?.tag ?? null}
          onChanged={accounts.reload}
        />
      ) : null}

      <Modal
        title="建账户"
        open={createOpen}
        onCancel={() => setCreateOpen(false)}
        onOk={() => createForm.submit()}
        confirmLoading={creating}
        okText="建"
        cancelText="取消"
        cancelButtonProps={{ 'data-testid': 'accounts-create-cancel' }}
        okButtonProps={{ 'data-testid': 'accounts-create-submit' }}
        destroyOnHidden
      >
        <Form form={createForm} layout="vertical" onFinish={(values) => void createAccount(values)}>
          <Form.Item
            name="tag"
            label="标签（可选）"
            tooltip="运营用它找账户；不填的话之后只能用账户标识找它"
          >
            <Input data-testid="accounts-create-tag" placeholder="vip" allowClear />
          </Form.Item>
          <Form.Item name="yuan" label="初始充值（元，可选）">
            <Input data-testid="accounts-create-yuan" placeholder="0" />
          </Form.Item>
        </Form>
      </Modal>
    </ConsolePage>
  );
}

/// 调用明细里的状态：与对客那条读同一套收敛取值，运营看中文。
const USAGE_STATUS: Record<string, string> = {
  succeeded: '成功',
  failed: '失败',
  pending: '进行中',
  canceled: '已取消',
};

/// 选中账户之后直接给出的三块：**充值记录 / 扣费记录 / API Key**（`#39`、`#40`）。
///
/// 不套一层"打开"、不弹抽屉。充值的**幂等键在这里生成**——进这一块生成一个、充成功后换一个；
/// 运营只填金额。双击（或网络重试）仍是同一个意图，只充一次。
function AccountActions({
  client,
  accountId,
  tag,
  onChanged,
}: {
  client: AdminClient;
  accountId: string;
  tag: string | null;
  onChanged: () => void;
}) {
  const { message } = AntApp.useApp();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [issued, setIssued] = useState<IssueApiKeyResponse | null>(null);
  const [businessKey, setBusinessKey] = useState(() => crypto.randomUUID());
  const [creditForm] = Form.useForm<{ yuan: string }>();
  const [tagForm] = Form.useForm<{ tag?: string }>();
  const balance = useLoadable(() => client.accountBalance(accountId), [client, accountId]);
  // **充值记录**只看 `credit`；**扣费记录**走调用明细（逐笔生成、带请求任务 ID）。
  // 预授权（`hold`/`release`）不进这两块——它是内部机制，排障看「对账与诊断」。
  const credits = useLoadable(
    () => client.accountEntries(accountId, 'credit', 50),
    [client, accountId],
  );
  const usage = useLoadable(() => client.accountUsage(accountId, 50), [client, accountId]);

  return (
    <Flex vertical gap={16}>
      {error ? <Alert type="error" showIcon message={error} /> : null}
      {balance.error ? <Alert type="error" showIcon message={balance.error} /> : null}

      {/* 账户身份那一行：标识、余额、标签。标签是"这个账户是谁"的一部分，不单独占一块。 */}
      <Flex align="center" gap={12} wrap>
        <Typography.Text strong>账户</Typography.Text>
        <Typography.Text code copyable style={{ fontSize: 12 }}>
          {accountId}
        </Typography.Text>
        <Typography.Text type="secondary">余额</Typography.Text>
        <Typography.Text strong data-testid="accounts-balance">
          {balance.data ? yuanText(balance.data.balance_microusd) : '—'}
        </Typography.Text>
        <Form
          form={tagForm}
          layout="inline"
          initialValues={{ tag: tag ?? '' }}
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
          <Form.Item
            name="tag"
            label="标签"
            tooltip="只被生效的 user_tag 策略消费；没有那条策略时不改变任何选路结果"
          >
            <Input data-testid="accounts-tag-input" style={{ width: 140 }} placeholder="vip" allowClear />
          </Form.Item>
          <Form.Item>
            <Button data-testid="accounts-tag-submit" htmlType="submit" loading={busy}>
              写入
            </Button>
          </Form.Item>
        </Form>
      </Flex>

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
          form={creditForm}
          layout="inline"
          onFinish={async (values: { yuan: string }) => {
            setBusy(true);
            setError(null);
            try {
              // 运营按元填、请求收微单位。四舍五入到整数微单位，避免浮点尾巴。
              const micros = Math.round(Number(values.yuan) * 1_000_000);
              if (!Number.isFinite(micros) || micros <= 0) throw new Error('充值金额必须是正数');
              await client.creditAccount(accountId, micros, businessKey);
              message.success(`已充值 ${yuanText(micros)}`);
              // 这次意图完成：换一个键，下一次充值才是新的一次。
              setBusinessKey(crypto.randomUUID());
              creditForm.resetFields();
              balance.reload();
              credits.reload();
              usage.reload();
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

      {/* **充值记录**：只有 `credit`。预授权与结算不混进来。 */}
      <Card
        size="small"
        title="充值记录"
        extra={<Button data-testid="accounts-credits-reload" onClick={credits.reload}>重取</Button>}
      >
        {credits.error ? <Alert type="error" showIcon message={credits.error} /> : null}
        <Table<LedgerEntry>
          size="small"
          rowKey={(entry, index) => `${entry.created_at}-${index ?? 0}`}
          loading={credits.loading}
          pagination={false}
          dataSource={credits.data?.entries ?? []}
          locale={{ emptyText: <Alert type="info" showIcon message="这个账户还没有充值记录。" /> }}
          columns={[
            { title: '时刻', dataIndex: 'created_at', render: (value: string) => whenText(value) },
            {
              title: '金额（元）',
              dataIndex: 'amount_microusd',
              align: 'right',
              render: (value: number) => <Typography.Text>{yuanText(value)}</Typography.Text>,
            },
          ]}
        />
      </Card>

      {/* **扣费记录（调用明细）**：逐笔生成请求——型号、张数、扣费、请求任务 ID。 */}
      <Card
        size="small"
        title="扣费记录（调用明细）"
        extra={<Button data-testid="accounts-usage-reload" onClick={usage.reload}>重取</Button>}
      >
        {usage.error ? <Alert type="error" showIcon message={usage.error} /> : null}
        <Table<AdminUsageRow>
          size="small"
          rowKey="job_id"
          loading={usage.loading}
          pagination={false}
          scroll={{ x: 'max-content' }}
          dataSource={usage.data?.usage ?? []}
          locale={{ emptyText: <Alert type="info" showIcon message="这个账户还没有调用记录。" /> }}
          columns={[
            { title: '时刻', dataIndex: 'created_at', render: (value: string) => whenText(value) },
            { title: '型号', dataIndex: 'gateway_model' },
            {
              title: '状态',
              dataIndex: 'status',
              render: (value: string) => <Tag color={value === 'succeeded' ? 'green' : 'default'}>{USAGE_STATUS[value] ?? value}</Tag>,
            },
            { title: '张数', dataIndex: 'image_count', align: 'right', width: 80 },
            {
              title: '扣费（元）',
              dataIndex: 'charged_microusd',
              align: 'right',
              // 明细给的是账本 `capture` 的和（负数）；列名已经说了是"扣费"，这里显示绝对值。
              render: (value: number) => (
                <Typography.Text>{yuanText(Math.abs(value))}</Typography.Text>
              ),
            },
            {
              title: '请求任务 ID',
              dataIndex: 'job_id',
              render: (value: string) => (
                <Typography.Text code copyable style={{ fontSize: 12 }}>
                  {value}
                </Typography.Text>
              ),
            },
          ]}
        />
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
          <Button
            data-testid="accounts-issue-key"
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
          {issued ? (
            <Alert
              type="warning"
              showIcon
              message="密钥明文——现在抄走，关掉这个页面就没了"
              description={
                <Flex vertical gap={4}>
                  <Typography.Text code copyable style={{ fontSize: 14 }}>
                    {issued.api_key}
                  </Typography.Text>
                  <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                    密钥标识：{issued.key_id}（吊销用它）
                  </Typography.Text>
                </Flex>
              }
            />
          ) : null}
          <Form
            layout="inline"
            onFinish={async (values: { keyId: string }) => {
              setBusy(true);
              setError(null);
              try {
                await client.revokeApiKey(values.keyId.trim());
                message.success('密钥已吊销');
              } catch (failure) {
                setError(failure instanceof Error ? failure.message : String(failure));
              } finally {
                setBusy(false);
              }
            }}
          >
            <Form.Item
              name="keyId"
              rules={[{ required: true, message: '请填要吊销的密钥标识' }]}
            >
              <Input
                data-testid="accounts-revoke-key"
                style={{ width: 320 }}
                placeholder="要吊销的密钥标识"
              />
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
