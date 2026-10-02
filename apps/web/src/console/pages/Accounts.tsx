import {
  Alert,
  App as AntApp,
  Button,
  Divider,
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
import {
  ArrowLeftOutlined,
  CreditCardOutlined,
  KeyOutlined,
  PlusOutlined,
  SearchOutlined,
} from '@ant-design/icons';
import { useState } from 'react';
import type { AdminClient } from '../client';
import type {
  AccountSummary,
  AdminUsageRow,
  IssueApiKeyResponse,
  LedgerEntry,
} from '../../shared/types';
import { useLoadable } from '../../shared/ui';
import { useScreenState } from '../screen-state';
import { ConsoleNotFound, ConsolePage, Panel, whenText, yuanText } from '../ui';

/// 账户：**列表与详情分开**（`docs/design/0011-console-information-architecture.md` §4.3）。
///
/// 列表只负责按邮箱／标签找账户、按完整账户标识直达、建账户；点行或建账户成功都**换地址**进入该账户
/// 自己的详情页（`#/accounts/{account_id}`），详情替换主内容区，不在列表下方追加。运营入口是客户
/// 邮箱与标签，不是账户标识（标识是 UUID，运营手上没有它）。
///
/// 这是"先找到再操作"的第一半；详情见 [`AccountDetailPage`]。
export function AccountsPage({
  client,
  onOpenAccount,
}: {
  client: AdminClient;
  onOpenAccount: (accountId: string) => void;
}) {
  const { message } = AntApp.useApp();
  /// 本次查找条件活在**会话状态**里：进详情再返回、或前进后退回来时恢复（邮箱不进地址）。
  /// 两个输入框也从它起：回来时看到的必须是**正在生效的**那个条件，而不是空框配一份筛过的列表。
  const [filter, setFilter] = useScreenState<{ email?: string; tag?: string }>(
    'accounts.filter',
    {},
  );
  const [email, setEmail] = useState(filter.email ?? '');
  const [tag, setTag] = useState(filter.tag ?? '');
  const [directId, setDirectId] = useState('');
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
      if (label) {
        // 标签是建账户之后的**第二步**。它失败时账户与初始充值已经成立，不能把人留在列表上——
        // 那样运营手上只剩一个"找不回来"的账户（没标签就只能按标识找）。进详情页，标签在那里补写。
        try {
          await client.setAccountTag(created.account_id, label);
        } catch (failure) {
          const reason = failure instanceof Error ? failure.message : String(failure);
          message.warning(`账户已建，但标签没写上（${reason}）；在详情页可以重写标签`);
        }
      }
      message.success(`已建账户 ${created.account_id}`);
      createForm.resetFields();
      setCreateOpen(false);
      // 建完直接进它的详情页：运营下一步就是充值或发密钥，不该再让他从列表里找一遍。
      onOpenAccount(created.account_id);
    } catch (failure) {
      message.error(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setCreating(false);
    }
  }

  return (
    <ConsolePage
      title="账户"
      hint="按邮箱或标签找到账户，点一行进入它的详情页；详情里才有充值、记录与密钥"
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
        {/* 收着按 id 直达：列表不可用时（例如刚拿到一个 id）仍要能进得去。 */}
        <Divider plain style={{ marginBlock: 16 }}>
          或按账户标识直达
        </Divider>
        <Space.Compact style={{ maxWidth: 560, width: '100%' }}>
          <Input
            data-testid="accounts-lookup-id"
            value={directId}
            onChange={(event) => setDirectId(event.target.value)}
            // 从事件里取当前值：受控 state 在同一次交互里可能还没回流，按 id 直达不能读到旧值。
            onPressEnter={(event) => {
              const value = event.currentTarget.value.trim();
              if (value) onOpenAccount(value);
            }}
            placeholder="账户 id（UUID）"
            allowClear
          />
          <Button
            data-testid="accounts-open-by-id"
            disabled={!directId.trim()}
            onClick={() => onOpenAccount(directId.trim())}
          >
            查看
          </Button>
        </Space.Compact>
      </Panel>

      <Panel
        title="账户列表"
        description={`共 ${accounts.data?.accounts.length ?? 0} 个（最多显示最近 100 个）。点一行进入它的详情页。`}
        extra={<Button onClick={accounts.reload}>重取</Button>}
      >
        <Table<AccountSummary>
          size="small"
          rowKey="account_id"
          loading={accounts.loading}
          pagination={false}
          dataSource={accounts.data?.accounts ?? []}
          onRow={(account) => ({
            onClick: () => onOpenAccount(account.account_id),
            style: { cursor: 'pointer' },
          })}
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

/// 详情里的四个模块：一次只挂载当前这一个。
type Module = 'credit' | 'credits' | 'usage' | 'keys';

/// 模块内容需要的壳：账户标识、客户端，以及"这一笔改变了金额"的回调。
type ModuleProps = { client: AdminClient; accountId: string; onCredited: () => void };

/// 四个模块**一处定义**：页内导航的按钮与挂载的内容都从这张表来，加一个模块只改这里。
const MODULES: {
  key: Module;
  label: string;
  content: (props: ModuleProps) => React.ReactNode;
}[] = [
  { key: 'credit', label: '充值', content: (props) => <CreditModule {...props} /> },
  { key: 'credits', label: '充值记录', content: (props) => <CreditsModule {...props} /> },
  { key: 'usage', label: '扣费记录（调用明细）', content: (props) => <UsageModule {...props} /> },
  { key: 'keys', label: 'API Key', content: (props) => <KeysModule {...props} /> },
];

/// 一个账户的详情页：地址里的账户标识**独立取数**，不依赖列表刚才加载过的那一行。
///
/// 金额与标签分别来自两条读——`GET /api/v1/accounts/{id}/summary`（标识、标签、绑定邮箱）与既有
/// `GET /api/v1/accounts/{id}`（已结算余额、持有中、可用额）；两者都不在页面重算（账户资金 Spec
/// `0002` §4）。任一读回"账户不存在"时显示找不到，且**不残留上一个账户的读数**。
///
/// 四个模块在金额与标签下面用页内按钮切换，一次只挂载当前模块：切走即卸载，所以 API Key 的签发明文
/// 不会在切回来时又出现（Spec M5、V-D15）。
export function AccountDetailPage({
  client,
  accountId,
  onBack,
}: {
  client: AdminClient;
  accountId: string;
  onBack: () => void;
}) {
  const { message } = AntApp.useApp();
  const [module, setModule] = useState<Module>('credit');
  const [busy, setBusy] = useState(false);
  const [tagError, setTagError] = useState<string | null>(null);

  const summary = useLoadable(() => client.accountSummary(accountId), [client, accountId]);
  const balance = useLoadable(() => client.accountBalance(accountId), [client, accountId]);

  /// 充值成功：余额与自身摘要重取。**充值记录不用在这里刷**——它按需挂载，切过去就是一次新读。
  const onCredited = () => {
    balance.reload();
    summary.reload();
  };

  const active = MODULES.find((item) => item.key === module);

  // 404 是"这个账户不在了"；400 是地址里的标识不成形（还是一条不能用的地址）。两者都显示找不到，
  // 而不是把上一次的读数留在屏幕上。
  const missing = [summary.status, balance.status].some((status) => status === 404 || status === 400);
  if (missing) return <ConsoleNotFound what="账户" onBack={onBack} />;

  const tagForm = (
    <Form
      layout="inline"
      key={summary.data?.tag ?? ''}
      initialValues={{ tag: summary.data?.tag ?? '' }}
      onFinish={async (values: { tag?: string }) => {
        setBusy(true);
        setTagError(null);
        try {
          await client.setAccountTag(accountId, values.tag?.trim() || null);
          message.success(values.tag?.trim() ? '标签已设置' : '标签已清除');
          summary.reload();
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
  );

  return (
    <ConsolePage
      title="账户详情"
      hint="这一个账户的金额、标签与四项操作；点上方按钮切换模块，一次只显示一个"
      error={summary.error ?? balance.error}
      loading={summary.loading || balance.loading}
      onReload={() => {
        summary.reload();
        balance.reload();
      }}
      extra={
        <Button icon={<ArrowLeftOutlined />} data-testid="accounts-back-to-list" onClick={onBack}>
          返回列表
        </Button>
      }
    >
      <Panel
        title="账户"
        description="账户 id、绑定邮箱与三个金额。金额来自账本，页面不重算；预授权与释放不出现在这里。"
      >
        <Flex vertical gap={16}>
          <Flex align="center" gap={12} wrap>
            <Typography.Text
              code
              copyable
              style={{ fontSize: 12 }}
              data-testid="accounts-selected-id"
            >
              {accountId}
            </Typography.Text>
            <Typography.Text type="secondary" data-testid="accounts-detail-email">
              {summary.data
                ? (summary.data.email ?? '（没有登录身份）')
                : '—'}
            </Typography.Text>
            {tagForm}
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

          {tagError ? <Alert type="error" showIcon message={tagError} /> : null}
        </Flex>
      </Panel>

      <Panel title="操作" description="一次只显示当前模块；切走即卸载，API Key 明文因此不会回来。">
        <Flex vertical gap={16}>
          <Space wrap>
            {MODULES.map((item) => (
              <Button
                key={item.key}
                data-testid={`accounts-module-${item.key}`}
                type={module === item.key ? 'primary' : 'default'}
                onClick={() => setModule(item.key)}
              >
                {item.label}
              </Button>
            ))}
          </Space>

          {active ? active.content({ client, accountId, onCredited }) : null}
        </Flex>
      </Panel>
    </ConsolePage>
  );
}

/// **充值**：只填金额。**幂等键由平台生成**——同一次意图重复提交只入账一次；成功后换一个键。
///
/// 模块卸载（切走、离开详情）就把没提交的金额与错误丢掉：重进是新的一次意图。
function CreditModule({
  client,
  accountId,
  onCredited,
}: {
  client: AdminClient;
  accountId: string;
  onCredited: () => void;
}) {
  const { message } = AntApp.useApp();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [businessKey, setBusinessKey] = useState(() => crypto.randomUUID());
  const [form] = Form.useForm<{ yuan: string }>();

  return (
    <Flex vertical gap={12} style={{ maxWidth: 420 }}>
      {error ? <Alert type="error" showIcon message={error} /> : null}
      <Form
        form={form}
        layout="vertical"
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
            form.resetFields();
            onCredited();
          } catch (failure) {
            setError(failure instanceof Error ? failure.message : String(failure));
          } finally {
            setBusy(false);
          }
        }}
      >
        <Form.Item name="yuan" label="金额（元）" rules={[{ required: true, message: '请填金额' }]}>
          <Input data-testid="accounts-credit-yuan" placeholder="100" />
        </Form.Item>
        <Form.Item style={{ marginBottom: 0 }}>
          <Button
            type="primary"
            htmlType="submit"
            loading={busy}
            icon={<CreditCardOutlined />}
            data-testid="accounts-credit-submit"
          >
            充值
          </Button>
        </Form.Item>
      </Form>
    </Flex>
  );
}

/// **充值记录**：只有实际入账的 `credit`。预授权与释放不混进来。
function CreditsModule({ client, accountId }: { client: AdminClient; accountId: string }) {
  const credits = useLoadable(() => client.accountEntries(accountId, 'credit', 50), [client, accountId]);
  return (
    <Flex vertical gap={12}>
      {credits.error ? <Alert type="error" showIcon message={credits.error} /> : null}
      <Space>
        <Button data-testid="accounts-credits-reload" onClick={credits.reload}>
          重取
        </Button>
      </Space>
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
  );
}

/// **扣费记录（调用明细）**：逐笔生成请求——型号、张数、扣费、请求任务 ID。
function UsageModule({ client, accountId }: { client: AdminClient; accountId: string }) {
  const usage = useLoadable(() => client.accountUsage(accountId, 50), [client, accountId]);
  return (
    <Flex vertical gap={12}>
      {usage.error ? <Alert type="error" showIcon message={usage.error} /> : null}
      <Space>
        <Button data-testid="accounts-usage-reload" onClick={usage.reload}>
          重取
        </Button>
      </Space>
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
            render: (value: string) => (
              <Tag color={value === 'succeeded' ? 'green' : 'default'}>
                {USAGE_STATUS[value] ?? value}
              </Tag>
            ),
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
  );
}

/// **API Key**：可签发、吊销。明文只在签发成功后的当前模块里出现一次；切走模块或离开详情即卸载，
/// 明文随之消失；吊销先二次确认（Spec M5）。
function KeysModule({ client, accountId }: { client: AdminClient; accountId: string }) {
  const { message } = AntApp.useApp();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [issued, setIssued] = useState<IssueApiKeyResponse | null>(null);
  const [revokeForm] = Form.useForm<{ keyId: string }>();

  return (
    <Flex vertical gap={12}>
      {error ? <Alert type="error" showIcon message={error} /> : null}
      <Typography.Paragraph type="secondary" style={{ margin: 0 }}>
        签发后明文只显示这一次，切走这个模块就没了；吊销要再确认一次。
      </Typography.Paragraph>
      <Space wrap>
        <Button
          data-testid="accounts-issue-key"
          icon={<KeyOutlined />}
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
      </Space>
      {issued ? (
        <Alert
          type="warning"
          showIcon
          message="密钥明文——现在抄走，切走这个模块就没了"
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
        preserve={false}
        onFinish={async (values: { keyId: string }) => {
          setBusy(true);
          setError(null);
          try {
            await client.revokeApiKey(values.keyId.trim());
            message.success('密钥已吊销');
            revokeForm.resetFields();
          } catch (failure) {
            setError(failure instanceof Error ? failure.message : String(failure));
          } finally {
            setBusy(false);
          }
        }}
      >
        <Form.Item name="keyId" rules={[{ required: true, message: '请填要吊销的密钥标识' }]}>
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
  );
}
