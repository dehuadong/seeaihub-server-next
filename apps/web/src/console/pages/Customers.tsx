import {
  Alert,
  App as AntApp,
  Button,
  Col,
  Descriptions,
  Flex,
  Form,
  Input,
  Row,
  Segmented,
  Space,
  Table,
  Typography,
} from 'antd';
import { ArrowLeftOutlined, KeyOutlined, PlusOutlined, SearchOutlined } from '@ant-design/icons';
import { useRef, useState } from 'react';
import type { AdminClient } from '../client';
import type { CustomerView } from '../../shared/types';
import { accountNameConflictHint } from '../../shared/account-name';
import { useLoadable } from '../../shared/ui';
import { useScreenState } from '../screen-state';
import { ConsoleNotFound, ConsolePage, Panel, whenText } from '../ui';

/// 客户：登录身份的开立与找回口令，**列表与详情分开**（`docs/design/0011` §4.4）。
///
/// 它与"账户"是两件事：账户是账本上的一行（余额、流水、密钥），客户是**登录身份**（邮箱 → 账户）。
/// 一个账户可以没有登录身份（运营直接建的），一个身份只指向一个账户。列表只负责开户、按邮箱找客户与
/// 列出客户；点「详情」或开户成功都**换地址**进入该客户的详情页（`#/customers/{customer_id}`）。
export function CustomersPage({
  client,
  onOpenCustomer,
}: {
  client: AdminClient;
  onOpenCustomer: (customerId: string) => void;
}) {
  const { message } = AntApp.useApp();
  /// 本次查找条件活在**会话状态**里：进详情再返回、或前进后退回来时恢复（邮箱不进地址）。
  /// 输入框也从它起：回来时看到的必须是**正在生效的**那个条件，而不是空框配一份筛过的列表。
  const [filter, setFilter] = useScreenState<{ email?: string }>('customers.filter', {});
  const [searchEmail, setSearchEmail] = useState(filter.email ?? '');
  const [opening, setOpening] = useState(false);
  /// 开户的两个模式显式分开：新建要名称，绑定只认已有账户 id，两者的字段不混在一次提交里。
  const [mode, setMode] = useState<'new' | 'existing'>('new');
  const [openForm] = Form.useForm<{
    email: string;
    password?: string;
    name?: string;
    accountId?: string;
  }>();
  /// 绑定的**预览**：只有当前输入 id 的成功响应能用于确认，换 id 立刻作废。
  const [preview, setPreview] = useState<{ accountId: string; name: string; email: string | null } | null>(
    null,
  );
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [previewing, setPreviewing] = useState(false);
  /// 请求序号：迟到的响应直接丢掉，不能用它确认一个已经换掉的 id。
  const previewSequence = useRef(0);

  async function loadPreview(rawAccountId: string) {
    const accountId = rawAccountId.trim();
    const sequence = ++previewSequence.current;
    if (!accountId) {
      setPreview(null);
      setPreviewError(null);
      return;
    }
    setPreviewing(true);
    setPreviewError(null);
    try {
      const summary = await client.accountSummary(accountId);
      if (sequence !== previewSequence.current) return;
      setPreview({ accountId, name: summary.name, email: summary.email });
    } catch (failure) {
      if (sequence !== previewSequence.current) return;
      setPreview(null);
      setPreviewError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      if (sequence === previewSequence.current) setPreviewing(false);
    }
  }

  const customers = useLoadable(
    () =>
      filter.email?.trim()
        ? client.findCustomer(filter.email.trim())
        : client.listCustomers(100),
    [client, filter],
  );

  return (
    <ConsolePage
      title="客户登录"
      hint="登录身份：邮箱 → 账户。开立之后客户能自己登录、管密钥、看账务"
      error={customers.error}
      loading={customers.loading}
      onReload={customers.reload}
    >
      <Panel
        title="开通邮箱登录"
        description="新建一个账户并开通登录，或给已有账户配上登录身份。绑定不动余额、API Key 与历史，也不改它已有的名称；不填初始口令时，改用重置令牌让客户自己设。"
      >
        <Segmented
          data-testid="customers-open-mode"
          options={[
            { value: 'new', label: '新建账户并开通邮箱登录' },
            { value: 'existing', label: '为已有账户开通邮箱登录' },
          ]}
          value={mode}
          onChange={(value) => {
            setMode(value as 'new' | 'existing');
            // 换模式即作废另一套字段与预览：不适用字段绝不能跟着提交。
            openForm.setFieldsValue({ name: undefined, accountId: undefined });
            setPreview(null);
            setPreviewError(null);
            previewSequence.current += 1;
          }}
          style={{ marginBottom: 16 }}
        />
        <Form
          form={openForm}
          layout="vertical"
          onFinish={async (values: {
            email: string;
            password?: string;
            name?: string;
            accountId?: string;
          }) => {
            const accountId = values.accountId?.trim();
            if (mode === 'existing') {
              if (!accountId) {
                message.error('请填已有账户的标识');
                return;
              }
              // 提交守卫：只有**当前输入 id** 的成功预览能作数，预览说它已有登录身份就不能提交。
              if (!preview || preview.accountId !== accountId) {
                message.error('请先确认这个账户的预览');
                return;
              }
              if (preview.email) {
                message.error('这个账户已经有登录身份了');
                return;
              }
            }
            setOpening(true);
            try {
              const created = await client.openCustomer(
                values.email.trim(),
                values.password || undefined,
                mode === 'existing' ? accountId : undefined,
                mode === 'new' ? values.name : undefined,
              );
              message.success(`已开通：${created.email}`);
              // 开完直接进这个客户的详情页：签发重置令牌是运营的下一步。
              onOpenCustomer(created.customer_id);
            } catch (failure) {
              const text =
                accountNameConflictHint(failure) ??
                (failure instanceof Error ? failure.message : String(failure));
              message.error(<span data-testid="customers-open-error">{text}</span>);
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
            {mode === 'new' ? (
              <Col xs={24} md={8}>
                <Form.Item
                  name="name"
                  label="账户名称（可空）"
                  tooltip="留空将按登录邮箱自动生成，之后随时可以改"
                >
                  <Input
                    data-testid="customers-open-name"
                    placeholder="留空即自动生成"
                    allowClear
                  />
                </Form.Item>
              </Col>
            ) : (
              <Col xs={24} md={8}>
                <Form.Item
                  name="accountId"
                  label="已有账户标识"
                  tooltip="填完整账户 id 后点「预览」确认是这个账户"
                >
                  <Space.Compact style={{ width: '100%' }}>
                    <Input
                      data-testid="customers-open-account-id"
                      placeholder="账户 id（UUID）"
                      // 换 id 立刻清掉旧预览：旧对象的信息不该停在一个已经换了目标的表单上。
                      onChange={(event) => {
                        if (event.target.value.trim() !== preview?.accountId) {
                          previewSequence.current += 1;
                          setPreview(null);
                          setPreviewError(null);
                        }
                      }}
                      onPressEnter={() => void loadPreview(openForm.getFieldValue('accountId') ?? '')}
                      allowClear
                    />
                    <Button
                      data-testid="customers-open-preview"
                      loading={previewing}
                      onClick={() => void loadPreview(openForm.getFieldValue('accountId') ?? '')}
                    >
                      预览
                    </Button>
                  </Space.Compact>
                </Form.Item>
              </Col>
            )}
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
          </Row>
          {mode === 'existing' ? (
            <Alert
              style={{ marginBottom: 16 }}
              type={preview ? 'success' : previewError ? 'error' : 'info'}
              showIcon
              data-testid="customers-open-preview-result"
              message={
                preview
                  ? `名称：${preview.name}；账户：${preview.accountId}；登录身份：${
                      preview.email ? `已绑定 ${preview.email}` : '未开通'
                    }。绑定保留余额、API Key 与历史。`
                  : previewError
                    ? previewError
                    : '填账户标识后点「预览」确认对象；预览确认前不能提交。'
              }
            />
          ) : null}
          <Form.Item style={{ marginBottom: 0 }}>
            <Button
              data-testid="customers-open-submit"
              type="primary"
              icon={<PlusOutlined />}
              htmlType="submit"
              loading={opening}
            >
              开通
            </Button>
          </Form.Item>
        </Form>
      </Panel>

      <Panel
        title="找客户"
        description="邮箱精确匹配、大小写不敏感。留空即列出最近的客户。"
        extra={<Button onClick={customers.reload}>重取</Button>}
      >
        {/* 与账户页同一套写法：普通 state + 一个按钮。这一处只需要"一个输入框 + 一个动作"，
            取点击那一刻的值，不需要校验也不需要受控字段。 */}
        <Flex gap={12} wrap align="flex-end" style={{ marginBottom: 16 }}>
          <Flex vertical gap={4}>
            <Typography.Text type="secondary" style={{ fontSize: 12 }}>
              客户邮箱
            </Typography.Text>
            <Input
              data-testid="customers-search-email"
              prefix={<SearchOutlined />}
              placeholder="customer@example.com"
              value={searchEmail}
              onChange={(event) => setSearchEmail(event.target.value)}
              onPressEnter={() => setFilter({ email: searchEmail })}
              style={{ width: 280 }}
              allowClear
            />
          </Flex>
          <Space>
            <Button
              data-testid="customers-search-submit"
              type="primary"
              onClick={() => setFilter({ email: searchEmail })}
            >
              查找
            </Button>
            <Button
              onClick={() => {
                setSearchEmail('');
                setFilter({});
              }}
            >
              清空
            </Button>
          </Space>
        </Flex>

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
            {
              title: '账户名称',
              dataIndex: 'account_name',
              width: 200,
              render: (value: string) => (
                <Typography.Text strong data-testid="customers-row-name">
                  {value}
                </Typography.Text>
              ),
            },
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
                  <Button
                    data-testid="customers-open-detail"
                    size="small"
                    onClick={() => onOpenCustomer(customer.customer_id)}
                  >
                    详情
                  </Button>
                </Space>
              ),
            },
          ]}
        />
      </Panel>
    </ConsolePage>
  );
}

/// 一个客户的详情页：按地址里的客户标识**独立取数**，不依赖刚才那次查找或开户的响应。
///
/// 关联账户是通往账户详情的入口——运营无须复制标识再查一次。重置令牌的明文只在这次签发后的提示里
/// 出现；关闭提示、离开详情或刷新即随组件卸载消失（Spec M5、C12）。
export function CustomerDetailPage({
  client,
  customerId,
  onBack,
  onOpenAccount,
}: {
  client: AdminClient;
  customerId: string;
  onBack: () => void;
  onOpenAccount: (accountId: string) => void;
}) {
  const { message } = AntApp.useApp();
  const [reset, setReset] = useState<{ reset_token: string; expires_at: string } | null>(null);
  const [busy, setBusy] = useState(false);

  const customer = useLoadable(() => client.customerView(customerId), [client, customerId]);
  const accountId = customer.data?.account_id ?? null;

  // 400 是地址里的标识不成形、404 是客户不在了：两条都显示找不到，不把上一个客户的资料留在屏幕上。
  if (customer.status === 404 || customer.status === 400) {
    return <ConsoleNotFound what="客户" onBack={onBack} />;
  }

  return (
    <ConsolePage
      title="客户详情"
      hint="这一个客户的登录身份、关联账户与重置令牌"
      error={customer.error}
      loading={customer.loading}
      onReload={customer.reload}
      extra={
        <Button icon={<ArrowLeftOutlined />} data-testid="customers-back-to-list" onClick={onBack}>
          返回列表
        </Button>
      }
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

      <Panel title="客户" description="客户是登录身份：一个邮箱指向一个账户。">
        <Descriptions
          size="small"
          bordered
          column={{ xs: 1, sm: 2 }}
          items={[
            {
              key: 'account_name',
              label: '账户名称',
              children: (
                <Typography.Text strong data-testid="customers-detail-account-name">
                  {customer.data?.account_name ?? '—'}
                </Typography.Text>
              ),
            },
            {
              key: 'email',
              label: '邮箱',
              children: (
                <Typography.Text data-testid="customers-detail-email">
                  {customer.data?.email ?? '—'}
                </Typography.Text>
              ),
            },
            {
              key: 'account',
              label: '关联账户',
              children: accountId ? (
                <Space>
                  <Typography.Text code copyable style={{ fontSize: 12 }}>
                    {accountId}
                  </Typography.Text>
                  <Button
                    size="small"
                    data-testid="customers-open-account"
                    onClick={() => onOpenAccount(accountId)}
                  >
                    进入账户详情
                  </Button>
                </Space>
              ) : (
                '—'
              ),
            },
            {
              key: 'created',
              label: '创建时间',
              children: whenText(customer.data?.created_at ?? null),
            },
            {
              key: 'last',
              label: '上次登录',
              children: whenText(customer.data?.last_login_at ?? null),
            },
          ]}
        />
        <Space style={{ marginTop: 16 }} wrap>
          <Button
            data-testid="customers-issue-reset"
            icon={<KeyOutlined />}
            loading={busy}
            onClick={async () => {
              if (!customer.data) return;
              setBusy(true);
              try {
                setReset(await client.issueCustomerPasswordReset(customer.data.account_id));
                message.success('已签发一次性重置令牌');
              } catch (failure) {
                message.error(failure instanceof Error ? failure.message : String(failure));
              } finally {
                setBusy(false);
              }
            }}
          >
            签发重置令牌
          </Button>
          <Typography.Text type="secondary">
            客户忘了口令时用这个；平台不发邮件，令牌要靠你转交。
          </Typography.Text>
        </Space>
      </Panel>
    </ConsolePage>
  );
}
