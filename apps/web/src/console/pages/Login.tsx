import { Alert, Button, Card, Flex, Form, Input, Typography, App as AntApp } from 'antd';
import { LockOutlined, MailOutlined } from '@ant-design/icons';
import { useState } from 'react';
import { apiFetch } from '../../shared/api';
import { useAdminSession } from '../session';
import { useLoadable } from '../../shared/ui';

/// 登录页。管理员用**邮箱 + 口令**换一条会话；拿不到就不进后台。
///
/// 这是 Spec M7 的落点：**未认证时一个管理 API 请求都不发**。所以这里只调 `/health`（公开）与
/// 登录端点，六个页面的取数在登录成功之前根本不会挂载。
export function LoginPage() {
  const { signIn } = useAdminSession();
  const { message } = AntApp.useApp();
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
      // 登录端点不需要凭据：用 `admin: false` 明确说明这一点。
      const body = await apiFetch<{ token: string; email: string }>(
        '/api/v1/admin/sessions',
        () => null,
        { method: 'POST', body: { email: values.email.trim(), password: values.password }, admin: false },
      );
      signIn(body.token, body.email);
    } catch (failure) {
      const text = failure instanceof Error ? failure.message : String(failure);
      setError(text);
      message.error(text);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Flex align="center" justify="center" style={{ minHeight: '100vh', background: '#f5f5f5' }}>
      <Card style={{ width: 420 }} styles={{ body: { padding: 32 } }}>
        <Flex vertical gap={4} style={{ marginBottom: 24 }}>
          <Typography.Title level={3} style={{ margin: 0 }}>
            seeai 运营后台
          </Typography.Title>
          <Typography.Text type="secondary">用管理员邮箱与口令登录</Typography.Text>
        </Flex>

        <Form form={form} layout="vertical" onFinish={submit} requiredMark={false}>
          <Form.Item
            name="email"
            label="邮箱"
            rules={[{ required: true, message: '请输入管理员邮箱' }]}
          >
            <Input
              data-testid="admin-email"
              prefix={<MailOutlined />}
              placeholder="ops@example.com"
              size="large"
              autoComplete="username"
            />
          </Form.Item>
          <Form.Item
            name="password"
            label="口令"
            rules={[{ required: true, message: '请输入口令' }]}
          >
            <Input.Password
              data-testid="admin-password"
              prefix={<LockOutlined />}
              size="large"
              autoComplete="current-password"
              onPressEnter={() => form.submit()}
            />
          </Form.Item>
          <Button
            data-testid="admin-sign-in"
            type="primary"
            size="large"
            block
            htmlType="submit"
            loading={busy}
          >
            登录
          </Button>
        </Form>

        {error ? (
          <Alert style={{ marginTop: 16 }} type="error" showIcon message={error} />
        ) : null}

        <Flex justify="space-between" align="center" style={{ marginTop: 16 }}>
          <Typography.Text type={health.data?.ok ? 'secondary' : 'danger'}>
            API 探活：
            {health.loading ? '检查中…' : health.data?.ok ? '健康' : `不健康（HTTP ${health.data?.status ?? '无响应'}）`}
          </Typography.Text>
        </Flex>

        <Typography.Paragraph type="secondary" style={{ marginTop: 16, marginBottom: 0, fontSize: 12 }}>
          还没有账号？运维在部署时用环境变量 <code>ADMIN_EMAIL</code> / <code>ADMIN_PASSWORD</code> 引导出
          第一个管理员；口令只在账号不存在时写入。
        </Typography.Paragraph>
      </Card>
    </Flex>
  );
}

/// 改自己的口令。成功后**该管理员的全部会话都失效**（含当前这条），所以这里只能回登录页。
export function ChangePasswordPanel({ onClose }: { onClose?: () => void }) {
  const { email, signOut } = useAdminSession();
  const { message } = AntApp.useApp();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState(false);
  const [form] = Form.useForm<{ current: string; next: string }>();

  async function submit(values: { current: string; next: string }) {
    setBusy(true);
    setError(null);
    try {
      await apiFetch('/api/v1/admin/password', () => sessionStorage.getItem('seeai.console.session'), {
        method: 'PUT',
        body: { current_password: values.current, new_password: values.next },
      });
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
  }

  return (
    <Card
      title="改口令"
      extra={
        onClose ? (
          <Button type="text" onClick={onClose}>
            收起
          </Button>
        ) : null
      }
    >
      <Typography.Paragraph type="secondary" style={{ marginTop: 0 }}>
        当前登录：{email}。改完之后该管理员的全部会话都会失效，包括当前这条。
      </Typography.Paragraph>
      <Form form={form} layout="inline" onFinish={submit} disabled={done}>
        <Form.Item
          name="current"
          label="当前口令"
          rules={[{ required: true, message: '请输入当前口令' }]}
        >
          <Input.Password autoComplete="current-password" style={{ width: 200 }} />
        </Form.Item>
        <Form.Item
          name="next"
          label="新口令"
          rules={[
            { required: true, message: '请输入新口令' },
            { min: 8, message: '至少 8 个字符' },
          ]}
        >
          <Input.Password autoComplete="new-password" style={{ width: 200 }} />
        </Form.Item>
        <Form.Item>
          <Button type="primary" htmlType="submit" loading={busy}>
            改口令
          </Button>
        </Form.Item>
      </Form>
      {error ? <Alert type="error" showIcon message={error} style={{ marginTop: 8 }} /> : null}
      {done ? (
        <Alert
          type="success"
          showIcon
          style={{ marginTop: 8 }}
          message="口令已改。全部会话都已失效，请用新口令重新登录。"
          action={
            <Button size="small" onClick={signOut}>
              回登录页
            </Button>
          }
        />
      ) : null}
    </Card>
  );
}
