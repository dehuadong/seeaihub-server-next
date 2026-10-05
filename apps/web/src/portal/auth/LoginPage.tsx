import { Alert, App as AntApp, Button, Flex, Form, Input } from 'antd';
import { LockOutlined, MailOutlined } from '@ant-design/icons';
import { useEffect, useRef, useState } from 'react';
import type { CustomerSession } from '../../shared/types';
import { CustomerClient } from '../client';
import { authPath, portalPath, usePortalRoute } from '../routes';
import { captureReturnTo, consumeReturnTo } from '../return-to';
import { useCustomerSession } from '../session';
import { AuthLayout } from './layout';
import { loginErrorMessage } from '../messages';

/// 登录 / 注册页。客户用邮箱 + 密码进来；没有账号就在同一地址注册（Spec C1–C3）。
///
/// `context` 决定登录成功后的去向：`guard` 是未登录直达受保护或未知地址时在原地址挂载的登录，
/// 成功后保留原地址；`public` 是公开 `/login`，成功后消费回跳目标，缺失时进概览。注册只在登录页
/// 局部切换，不增加注册地址；路由改变即卸载表单，重新进入默认登录（设计 0016 §1）。
export function LoginPage({ context }: { context: 'public' | 'guard' }) {
  const { signIn } = useCustomerSession();
  const { pathname, search, navigate, replace } = usePortalRoute();
  const { message } = AntApp.useApp();
  const [mode, setMode] = useState<'login' | 'register'>('login');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [form] = Form.useForm<{ email: string; password: string }>();
  // 页面存活标识：晚到的响应不得更新已离开的页面，也不得替它建会话（设计 0016 §3）。
  const alive = useRef(true);
  // 请求锁：同一帧的连续提交只发一次请求，不能只靠按钮 loading（下一次渲染才生效）。
  const lock = useRef(false);
  useEffect(
    () => () => {
      alive.current = false;
    },
    [],
  );

  async function submit(values: { email: string; password: string }) {
    if (lock.current) return;
    lock.current = true;
    setBusy(true);
    setError(null);
    try {
      const client = new CustomerClient(() => null);
      const session: CustomerSession =
        mode === 'register'
          ? await client.register(values.email.trim(), values.password)
          : await client.login(values.email.trim(), values.password);
      if (!alive.current) return;
      if (context === 'public') {
        // 先 replace 到确定目标、再写会话：否则会话守卫会抢先把登录页带回概览（设计 0016 §1）。
        replace(consumeReturnTo() ?? portalPath('overview'));
      } else {
        consumeReturnTo();
      }
      signIn(session);
    } catch (failure) {
      if (!alive.current) return;
      const text = loginErrorMessage(mode, failure);
      setError(text);
      message.error(text);
    } finally {
      if (alive.current) setBusy(false);
      lock.current = false;
    }
  }

  function switchMode(next: 'login' | 'register') {
    setMode(next);
    setError(null);
    form.resetFields(['password']);
  }

  function openForgotPassword() {
    // 从受保护地址进入找回前，以当前地址与日期区间覆盖旧目标；公开页之间切换只读不覆盖。
    if (context === 'guard') captureReturnTo(pathname, search);
    navigate(authPath('forgotPassword'));
  }

  return (
    <AuthLayout
      title={mode === 'register' ? '注册' : '登录'}
      description={mode === 'register' ? '用邮箱与密码注册账号' : '用邮箱与密码登录'}
    >
      <Form form={form} layout="vertical" onFinish={submit} requiredMark={false}>
        <Form.Item name="email" label="邮箱" rules={[{ required: true, message: '请输入邮箱' }]}>
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
          label="密码（至少 8 个字符）"
          rules={[
            { required: true, message: '请输入密码' },
            { min: 8, message: '密码至少 8 个字符' },
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

      {error ? (
        <Alert
          data-testid="portal-auth-error"
          style={{ marginTop: 16 }}
          type="error"
          showIcon
          message={error}
        />
      ) : null}

      <Flex vertical gap={2} style={{ marginTop: 16 }}>
        {mode === 'login' ? (
          <>
            <Button
              data-testid="portal-mode-register"
              type="link"
              style={{ padding: 0, height: 'auto', alignSelf: 'flex-start' }}
              onClick={() => switchMode('register')}
            >
              注册账号
            </Button>
            <Button
              data-testid="portal-forgot-link"
              type="link"
              style={{ padding: 0, height: 'auto', alignSelf: 'flex-start' }}
              onClick={openForgotPassword}
            >
              忘记密码？
            </Button>
          </>
        ) : (
          <Button
            data-testid="portal-mode-login"
            type="link"
            style={{ padding: 0, height: 'auto', alignSelf: 'flex-start' }}
            onClick={() => switchMode('login')}
          >
            返回登录
          </Button>
        )}
      </Flex>
    </AuthLayout>
  );
}
