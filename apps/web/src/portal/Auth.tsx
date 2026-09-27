import { Alert, App as AntApp, Button, Card, Flex, Form, Input, Segmented, Typography } from 'antd';
import { LockOutlined, MailOutlined, SafetyOutlined } from '@ant-design/icons';
import { useState } from 'react';
import { apiFetch } from '../shared/api';
import { CustomerClient } from './client';
import { useCustomerSession } from './session';
import { useLoadable } from '../shared/ui';

/// 登录 / 注册页。客户用邮箱 + 口令进来；没有账号就注册一个（Spec C1–C3）。
///
/// 未登录时只调公开端点：`/health`、注册、登录。账户数据在登录成功之前**一个请求都不发**。
export function AuthPage() {
  const { signIn } = useCustomerSession();
  const { message } = AntApp.useApp();
  const [mode, setMode] = useState<'login' | 'register'>('login');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [form] = Form.useForm<{ email: string; password: string }>();

  const health = useLoadable(async () => {
    const response = await fetch('/health');
    return { ok: response.ok, status: response.status };
  }, []);

  async function submit(values: { email: string; password: string }) {
    setBusy(true);
    setError(null);
    try {
      const path = mode === 'register' ? '/v1/customers' : '/v1/customer/sessions';
      const session = await apiFetch<{ token: string; email: string; account_id: string }>(
        path,
        () => null,
        { method: 'POST', body: { email: values.email.trim(), password: values.password }, admin: false },
      );
      signIn(session);
    } catch (failure) {
      const text = failure instanceof Error ? failure.message : String(failure);
      setError(text);
      message.error(text);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Flex
      align="center"
      justify="center"
      style={{ minHeight: '100vh', background: '#f5f5f5', padding: 24 }}
    >
      <Flex vertical gap={16} style={{ width: '100%', maxWidth: 460 }}>
        <Card styles={{ body: { padding: 32 } }}>
          <Flex vertical gap={4} style={{ marginBottom: 20 }}>
            <Typography.Title level={3} style={{ margin: 0 }}>
              seeai 控制台
            </Typography.Title>
            <Typography.Text type="secondary">
              用邮箱与口令{mode === 'register' ? '注册' : '登录'}
            </Typography.Text>
          </Flex>

          <Segmented
            block
            value={mode}
            onChange={(value) => setMode(value as 'login' | 'register')}
            options={[
              { label: <span data-testid="portal-mode-login">登录</span>, value: 'login' },
              { label: <span data-testid="portal-mode-register">注册</span>, value: 'register' },
            ]}
            style={{ marginBottom: 20 }}
          />

          <Form form={form} layout="vertical" onFinish={submit} requiredMark={false}>
            <Form.Item
              name="email"
              label="邮箱"
              rules={[{ required: true, message: '请输入邮箱' }]}
            >
              <Input
                data-testid="portal-email"
                prefix={<MailOutlined />}
                placeholder="you@example.com"
                size="large"
                autoComplete="username"
              />
            </Form.Item>
            <Form.Item
              name="password"
              label="口令（至少 8 个字符）"
              rules={[
                { required: true, message: '请输入口令' },
                { min: 8, message: '至少 8 个字符' },
              ]}
            >
              <Input.Password
                data-testid="portal-password"
                prefix={<LockOutlined />}
                size="large"
                autoComplete={mode === 'register' ? 'new-password' : 'current-password'}
                onPressEnter={() => form.submit()}
              />
            </Form.Item>
            <Button
              data-testid="portal-submit"
              type="primary"
              size="large"
              block
              htmlType="submit"
              loading={busy}
            >
              {mode === 'register' ? '注册并进入' : '登录'}
            </Button>
          </Form>

          {error ? <Alert style={{ marginTop: 16 }} type="error" showIcon message={error} /> : null}

          <Typography.Text
            type={health.data?.ok ? 'secondary' : 'danger'}
            style={{ display: 'block', marginTop: 16 }}
          >
            服务探活：
            {health.loading ? '检查中…' : health.data?.ok ? '正常' : `不健康（HTTP ${health.data?.status ?? '无响应'}）`}
          </Typography.Text>
        </Card>

        <ResetPanel />

        <Card size="small">
          <Typography.Paragraph type="secondary" style={{ margin: 0, fontSize: 12 }}>
            平台目前没有在线支付：充值由运营在后台完成，这里只展示充值记录与余额。忘了口令也不能自助
            重置——请找运营签发一枚一次性重置令牌，用它设置新口令。
          </Typography.Paragraph>
        </Card>
      </Flex>
    </Flex>
  );
}

/// 凭运营转交的令牌设置新口令（Spec C12 的对客一侧）。
///
/// 平台上没有"提交邮箱就收到重置链接"这条路——不发邮件、不做邮箱验证，那种入口等于"知道邮箱就能
/// 接管账户"。所以这里只收**运营转交过来的令牌**。
function ResetPanel() {
  const client = new CustomerClient(() => null);
  const { message } = AntApp.useApp();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState(false);
  const [form] = Form.useForm<{ token: string; password: string }>();

  return (
    <Card
      size="small"
      title={
        <Flex align="center" gap={8}>
          <SafetyOutlined />
          用重置令牌设置新口令
        </Flex>
      }
    >
      <Typography.Paragraph type="secondary" style={{ fontSize: 12 }}>
        口令忘了就找运营要一枚一次性重置令牌（平台不发邮件）。重置成功会使此前所有登录会话失效。
      </Typography.Paragraph>
      <Form
        form={form}
        layout="vertical"
        disabled={done}
        onFinish={async (values) => {
          setBusy(true);
          setError(null);
          try {
            await client.redeemPasswordReset(values.token.trim(), values.password);
            setDone(true);
            form.resetFields();
            message.success('口令已重置，回到上面用新口令登录');
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
          name="token"
          label="重置令牌"
          rules={[{ required: true, message: '请填运营给你的重置令牌' }]}
        >
          <Input data-testid="portal-reset-token" placeholder="粘贴令牌" />
        </Form.Item>
        <Form.Item
          name="password"
          label="新口令（至少 8 个字符）"
          rules={[
            { required: true, message: '请输入新口令' },
            { min: 8, message: '至少 8 个字符' },
          ]}
        >
          <Input.Password data-testid="portal-reset-password" autoComplete="new-password" />
        </Form.Item>
        <Button
          data-testid="portal-reset-submit"
          type="primary"
          htmlType="submit"
          loading={busy}
          block
        >
          设置新口令
        </Button>
      </Form>
      {error ? <Alert style={{ marginTop: 12 }} type="error" showIcon message={error} /> : null}
      {done ? (
        <Alert
          style={{ marginTop: 12 }}
          type="success"
          showIcon
          message="口令已重置，回到上面用新口令登录。"
        />
      ) : null}
    </Card>
  );
}
