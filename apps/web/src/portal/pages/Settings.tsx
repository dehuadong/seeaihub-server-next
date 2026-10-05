import { Alert, App as AntApp, Button, Card, Descriptions, Flex, Form, Input, Space, Typography } from 'antd';
import { LockOutlined } from '@ant-design/icons';
import { useEffect, useState } from 'react';
import type { CustomerClient } from '../client';
import { passwordErrorMessage } from '../messages';
import { NO_ONLINE_PAYMENT, RESET_VIA_OPERATIONS } from '../notices';
import { useCustomerSession } from '../session';
import { useLoadable } from '../../shared/ui';
import { accountNameConflictHint, accountNameProblem } from '../../shared/account-name';

/// 账户设置：登录邮箱、可复制的账户 id、改密码与退出。低频资料与低频动作收在这里，不占概览首屏。
///
/// 退出由外壳传入（`onSignOut`）：清掉本地会话与页面数据、把地址带回概览（Spec D5）。
export function SettingsPage({
  client,
  onSignOut,
}: {
  client: CustomerClient;
  onSignOut: () => void;
}) {
  const { email, accountId } = useCustomerSession();
  const { message } = AntApp.useApp();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState(false);
  const [form] = Form.useForm<{ current: string; next: string }>();
  // 账户名称从账户读拿（会话里只有邮箱与账户 id）；首屏先按读到的那一份填进输入框。
  const account = useLoadable(() => client.account(), [client]);
  const [name, setName] = useState('');
  const [savingName, setSavingName] = useState(false);
  const [nameError, setNameError] = useState<string | null>(null);
  useEffect(() => {
    if (account.data) setName(account.data.name);
  }, [account.data]);

  /// 保存名称：成功后就地显示新值（重取一次读，不自己拼），失败保留原值让客户重试。
  async function saveName() {
    const problem = accountNameProblem(name);
    if (problem) {
      // 不合规则不是一种改名：把输入框退回**已保存的那一份**，服务端也就不必收一次注定被拒的请求。
      setName(account.data?.name ?? '');
      setNameError(problem);
      return;
    }
    setSavingName(true);
    setNameError(null);
    try {
      await client.renameAccount(name.trim());
      account.reload();
      message.success('账户名称已保存');
    } catch (failure) {
      setNameError(
        accountNameConflictHint(failure) ??
          (failure instanceof Error ? failure.message : String(failure)),
      );
    } finally {
      setSavingName(false);
    }
  }

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
        <Flex vertical gap={4} style={{ marginTop: 16, maxWidth: 420 }}>
          <Typography.Text type="secondary" style={{ fontSize: 12 }}>
            账户名称
          </Typography.Text>
          <Space.Compact style={{ width: '100%' }}>
            <Input
              data-testid="portal-account-name"
              value={name}
              onChange={(event) => setName(event.target.value)}
              onPressEnter={() => void saveName()}
              disabled={account.loading}
            />
            <Button
              data-testid="portal-account-name-save"
              loading={savingName}
              disabled={account.loading}
              onClick={() => void saveName()}
            >
              保存
            </Button>
          </Space.Compact>
          {/* 只说这个字段是干什么的；不解释名称从哪来、也不出现账户 id 片段这些内部细节。 */}
          <Typography.Text type="secondary" style={{ fontSize: 12 }}>
            账户名称用于识别与账单，可随时修改。
          </Typography.Text>
          {nameError ? (
            <Alert type="error" showIcon data-testid="portal-account-name-error" message={nameError} />
          ) : null}
        </Flex>
        <Button style={{ marginTop: 16 }} onClick={onSignOut}>
          退出登录
        </Button>
      </Card>

      <Card
        title={
          <Space size={8}>
            <LockOutlined />
            改密码
          </Space>
        }
      >
        <Typography.Paragraph type="secondary" style={{ marginTop: 0 }}>
          改完之后此前所有登录会话都会失效，需要用新密码重新登录。
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
              message.success('密码已改，请用新密码重新登录');
            } catch (failure) {
              // 客户面自己映射成“密码”措辞：共享传输那句是给管理端的，仍带“口令”。
              const text = passwordErrorMessage(failure);
              setError(text);
              message.error(text);
            } finally {
              setBusy(false);
            }
          }}
        >
          <Form.Item
            name="current"
            label="当前密码"
            rules={[{ required: true, message: '请输入当前密码' }]}
          >
            <Input.Password
              data-testid="portal-current-password"
              prefix={<LockOutlined />}
              autoComplete="current-password"
            />
          </Form.Item>
          <Form.Item
            name="next"
            label="新密码"
            rules={[
              { required: true, message: '请输入新密码' },
              { min: 8, message: '至少 8 个字符' },
            ]}
          >
            <Input.Password data-testid="portal-new-password" autoComplete="new-password" />
          </Form.Item>
          <Form.Item style={{ marginBottom: 0 }}>
            <Button
              data-testid="portal-change-password"
              type="primary"
              htmlType="submit"
              loading={busy}
            >
              改密码
            </Button>
          </Form.Item>
        </Form>
        {error ? (
          <Alert
            data-testid="portal-change-password-error"
            style={{ marginTop: 12 }}
            type="error"
            showIcon
            message={error}
          />
        ) : null}
        {done ? (
          <Alert
            data-testid="portal-change-password-done"
            style={{ marginTop: 12 }}
            type="success"
            showIcon
            message="密码已改。此前所有登录会话都已失效。"
            action={
              <Button size="small" onClick={onSignOut}>
                回登录页
              </Button>
            }
          />
        ) : null}
      </Card>

      <Card title="关于充值">
        <Typography.Paragraph type="secondary" style={{ margin: 0 }}>
          {NO_ONLINE_PAYMENT}
          {RESET_VIA_OPERATIONS}
        </Typography.Paragraph>
      </Card>
    </Flex>
  );
}
