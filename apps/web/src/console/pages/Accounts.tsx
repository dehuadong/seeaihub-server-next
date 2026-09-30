import {
  Alert,
  App as AntApp,
  Button,
  Divider,
  Drawer,
  Flex,
  Form,
  Input,
  Modal,
  Popconfirm,
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
/// 一页只有两样：找账户（搜索 + 列表）和选中之后的那一行。点列表行即选中，选中区只给四个按钮——
/// **充值 / 充值记录 / 扣费记录（调用明细）/ API Key**；模块内容不摊在页面上，充值开弹窗，
/// 其余三个各开侧边栏。切换选中的账户会重挂这一块，当前弹层随之关掉。
///
/// 运营入口是**客户邮箱与标签**，不是账户标识（标识是 UUID，运营手上没有它）。
///
/// 充值的**幂等键由平台生成**：运营只填金额，同一次意图重复提交只入账一次。
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
      hint="按邮箱或标签找到账户；选中它就有充值、记录与密钥的入口"
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
            // 从事件里取当前值：受控 state 在同一次交互里可能还没回流，按 id 直达不能读到旧值。
            onPressEnter={(event) => setSelected(event.currentTarget.value.trim())}
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
        description={`共 ${accounts.data?.accounts.length ?? 0} 个（最多显示最近 100 个）。点一行选中它。`}
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
              title: '已结算余额',
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

/// 选中账户之后给出的那一行：账户 id、三个金额（已结算余额 / 持有中 / 可用额）与可就地改的标签，
/// 外加**四个按钮**。模块内容一律不摊在页面上：充值开弹窗，充值记录、扣费记录（调用明细）与
/// API Key 各开一个侧边栏（`#41` 的 P1/P2）。
///
/// 三个金额分别显示、不在页面里重算（账户资金 Spec `0002` §4）。预授权与释放不出现在任何一处。
///
/// 充值的**幂等键在这里生成**——打开弹窗生成一个、充成功后换一个；运营只填金额。提交期间按钮
/// 处于 loading，双击发不出第二次请求；即便网络重试，同一个键也只入账一次。
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
  const [layer, setLayer] = useState<'credit' | 'credits' | 'usage' | 'keys' | null>(null);
  const [busy, setBusy] = useState(false);
  const [tagError, setTagError] = useState<string | null>(null);
  const [creditError, setCreditError] = useState<string | null>(null);
  const [keyError, setKeyError] = useState<string | null>(null);
  const [issued, setIssued] = useState<IssueApiKeyResponse | null>(null);
  const [businessKey, setBusinessKey] = useState(() => crypto.randomUUID());
  const [creditForm] = Form.useForm<{ yuan: string }>();
  const [tagForm] = Form.useForm<{ tag?: string }>();
  const [revokeForm] = Form.useForm<{ keyId: string }>();

  const balance = useLoadable(() => client.accountBalance(accountId), [client, accountId]);
  // **充值记录**只看 `credit`；**扣费记录**走调用明细（逐笔生成、带请求任务 ID）。
  // 预授权（`hold`/`release`）不进这两块——它是内部机制，排障看「对账与诊断」。
  const credits = useLoadable(
    () => client.accountEntries(accountId, 'credit', 50),
    [client, accountId],
  );
  const usage = useLoadable(() => client.accountUsage(accountId, 50), [client, accountId]);

  /// 每次打开充值弹窗都是**新的一次意图**：换一个幂等键；上一次没提交的金额由表单的
  /// `preserve={false}` 在关闭时清掉，不在这里手动重置。
  function openCredit() {
    setCreditError(null);
    setBusinessKey(crypto.randomUUID());
    setLayer('credit');
  }

  return (
    <Panel
      title="选中账户"
      description="账户 id、三个金额与标签。充值开弹窗，其余三个各自开侧边栏；切换账户会关掉当前弹层。"
    >
      <Flex vertical gap={16}>
        <Flex align="center" gap={12} wrap>
          <Typography.Text code copyable style={{ fontSize: 12 }} data-testid="accounts-selected-id">
            {accountId}
          </Typography.Text>
          <Form
            form={tagForm}
            layout="inline"
            initialValues={{ tag: tag ?? '' }}
            onFinish={async (values: { tag?: string }) => {
              setBusy(true);
              setTagError(null);
              try {
                await client.setAccountTag(accountId, values.tag?.trim() || null);
                message.success(values.tag?.trim() ? '标签已设置' : '标签已清除');
                onChanged();
              } catch (failure) {
                setTagError(failure instanceof Error ? failure.message : String(failure));
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

        {/* 管理员可以分别显示三个金额（账户资金 Spec §4）：它们来自同一次账户读，页面不重算。 */}
        <Flex gap={24} wrap>
          <Flex vertical gap={0}>
            <Typography.Text type="secondary" style={{ fontSize: 12 }}>
              已结算余额
            </Typography.Text>
            <Typography.Text strong data-testid="accounts-balance">
              {balance.data ? yuanText(balance.data.balance_microusd) : '—'}
            </Typography.Text>
          </Flex>
          <Flex vertical gap={0}>
            <Typography.Text type="secondary" style={{ fontSize: 12 }}>
              持有中
            </Typography.Text>
            <Typography.Text data-testid="accounts-held">
              {balance.data ? yuanText(balance.data.held_microusd) : '—'}
            </Typography.Text>
          </Flex>
          <Flex vertical gap={0}>
            <Typography.Text type="secondary" style={{ fontSize: 12 }}>
              可用额
            </Typography.Text>
            <Typography.Text data-testid="accounts-available">
              {balance.data ? yuanText(balance.data.available_microusd) : '—'}
            </Typography.Text>
          </Flex>
        </Flex>

        {balance.error ? <Alert type="error" showIcon message={balance.error} /> : null}
        {tagError ? <Alert type="error" showIcon message={tagError} /> : null}

        <Space wrap>
          <Button
            type="primary"
            icon={<CreditCardOutlined />}
            data-testid="accounts-credit-open"
            onClick={openCredit}
          >
            充值
          </Button>
          <Button data-testid="accounts-credits-open" onClick={() => setLayer('credits')}>
            充值记录
          </Button>
          <Button data-testid="accounts-usage-open" onClick={() => setLayer('usage')}>
            扣费记录（调用明细）
          </Button>
          <Button
            icon={<KeyOutlined />}
            data-testid="accounts-keys-open"
            onClick={() => setLayer('keys')}
          >
            API Key
          </Button>
        </Space>
      </Flex>

      {/* **充值**：弹窗，只填金额。内容只在打开时挂载——页面上不存在这个输入框。 */}
      <Modal
        title="充值"
        open={layer === 'credit'}
        onCancel={() => setLayer(null)}
        onOk={() => creditForm.submit()}
        confirmLoading={busy}
        okText="充值"
        cancelText="取消"
        cancelButtonProps={{ 'data-testid': 'accounts-credit-cancel' }}
        okButtonProps={{ 'data-testid': 'accounts-credit-submit' }}
        destroyOnHidden
      >
        {layer === 'credit' ? (
          <Flex vertical gap={12}>
            {creditError ? <Alert type="error" showIcon message={creditError} /> : null}
            <Form
              form={creditForm}
              layout="vertical"
              // 关掉保留：取消后重开是空表单，不会把上一次没提交的金额带过来。
              preserve={false}
              onFinish={async (values: { yuan: string }) => {
                setBusy(true);
                setCreditError(null);
                try {
                  // 运营按元填、请求收微单位。四舍五入到整数微单位，避免浮点尾巴。
                  const micros = Math.round(Number(values.yuan) * 1_000_000);
                  if (!Number.isFinite(micros) || micros <= 0) throw new Error('充值金额必须是正数');
                  await client.creditAccount(accountId, micros, businessKey);
                  message.success(`已充值 ${yuanText(micros)}`);
                  // 这次意图完成：换一个键，下一次充值才是新的一次。
                  setBusinessKey(crypto.randomUUID());
                  creditForm.resetFields();
                  setLayer(null);
                  balance.reload();
                  credits.reload();
                  usage.reload();
                  onChanged();
                } catch (failure) {
                  setCreditError(failure instanceof Error ? failure.message : String(failure));
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
                <Input data-testid="accounts-credit-yuan" placeholder="100" />
              </Form.Item>
            </Form>
          </Flex>
        ) : null}
      </Modal>

      {/* **充值记录**：侧边栏，只有 `credit`。预授权与结算不混进来。 */}
      <Drawer
        title="充值记录"
        width="min(720px, 100vw)"
        open={layer === 'credits'}
        onClose={() => setLayer(null)}
        // 侧边栏不挡主页面：切换选中账户时当前弹层会随选中区重挂而关闭。
        mask={false}
        destroyOnHidden
        extra={
          <Button data-testid="accounts-credits-reload" onClick={credits.reload}>
            重取
          </Button>
        }
      >
        {layer === 'credits' ? (
          <Flex vertical gap={12}>
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
          </Flex>
        ) : null}
      </Drawer>

      {/* **扣费记录（调用明细）**：侧边栏，逐笔生成请求——型号、张数、扣费、请求任务 ID。 */}
      <Drawer
        title="扣费记录（调用明细）"
        width="min(960px, 100vw)"
        open={layer === 'usage'}
        onClose={() => setLayer(null)}
        mask={false}
        destroyOnHidden
        extra={
          <Button data-testid="accounts-usage-reload" onClick={usage.reload}>
            重取
          </Button>
        }
      >
        {layer === 'usage' ? (
          <Flex vertical gap={12}>
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
          </Flex>
        ) : null}
      </Drawer>

      {/* **API Key**：侧边栏，可签发、吊销。明文只在签发成功时出现一次；吊销要二次确认。 */}
      <Drawer
        title="API Key"
        width="min(720px, 100vw)"
        open={layer === 'keys'}
        // 关掉侧边栏就把签发明文丢掉：它只在签发那一次出现（Spec M5）。
        onClose={() => {
          setLayer(null);
          setIssued(null);
          setKeyError(null);
        }}
        mask={false}
        destroyOnHidden
      >
        {layer === 'keys' ? (
          <Flex vertical gap={12}>
            {keyError ? <Alert type="error" showIcon message={keyError} /> : null}
            <Typography.Paragraph type="secondary" style={{ margin: 0 }}>
              签发后明文只显示这一次，关掉侧边栏就没了；吊销要再确认一次。
            </Typography.Paragraph>
            <Button
              data-testid="accounts-issue-key"
              disabled={busy}
              onClick={async () => {
                setBusy(true);
                setKeyError(null);
                try {
                  setIssued(await client.issueApiKey(accountId, 'admin-console'));
                  message.success('密钥已签发');
                } catch (failure) {
                  setKeyError(failure instanceof Error ? failure.message : String(failure));
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
                message="密钥明文——现在抄走，关掉这个侧边栏就没了"
                description={
                  <Flex vertical gap={4}>
                    <Typography.Text code copyable style={{ fontSize: 14 }} data-testid="accounts-issued-key">
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
              form={revokeForm}
              layout="inline"
              onFinish={async (values: { keyId: string }) => {
                setBusy(true);
                setKeyError(null);
                try {
                  await client.revokeApiKey(values.keyId.trim());
                  message.success('密钥已吊销');
                  revokeForm.resetFields();
                } catch (failure) {
                  setKeyError(failure instanceof Error ? failure.message : String(failure));
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
                <Popconfirm
                  title="吊销这把密钥？"
                  description="吊销后用它调用会被拒，不能撤销。"
                  okText="吊销"
                  cancelText="取消"
                  okButtonProps={{ 'data-testid': 'accounts-revoke-confirm' }}
                  onConfirm={() => revokeForm.submit()}
                >
                  <Button danger data-testid="accounts-revoke-open" loading={busy}>
                    吊销
                  </Button>
                </Popconfirm>
              </Form.Item>
            </Form>
          </Flex>
        ) : null}
      </Drawer>
    </Panel>
  );
}
