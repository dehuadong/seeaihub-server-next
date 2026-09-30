import { Alert, App as AntApp, Button, Card, Descriptions, Flex, Form, Input, Space, Typography } from 'antd';
import { LockOutlined } from '@ant-design/icons';
import { useState } from 'react';
import type { CustomerClient } from '../client';
import { NO_ONLINE_PAYMENT, RESET_VIA_OPERATIONS } from '../notices';
import { useCustomerSession } from '../session';

/// 账户设置：登录邮箱、可复制的账户 id、改口令与退出。低频资料与低频动作收在这里，不占概览首屏。
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
        <Button style={{ marginTop: 16 }} onClick={onSignOut}>
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
            <Input.Password data-testid="portal-new-password" autoComplete="new-password" />
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
