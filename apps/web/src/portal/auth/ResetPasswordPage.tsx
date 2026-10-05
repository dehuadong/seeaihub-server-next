import { Alert, Button, Flex, Form, Input } from 'antd';
import { LockOutlined, SafetyOutlined } from '@ant-design/icons';
import { useEffect, useRef, useState } from 'react';
import { ApiError } from '../../shared/api';
import { CustomerClient } from '../client';
import { authPath, usePortalRoute } from '../routes';
import { waitingMessage } from '../messages';
import { AuthLayout } from './layout';

/// 重置页状态（设计 0016 §3）。
type ResetState = 'editing' | 'submitting' | 'succeeded' | 'terminal-error' | 'unknown-result';

/// 服务端无法区分“码无效/过期/已用”与“新密码不合规”：映射为 Spec §4 的组合提示。
const PARAM_ERROR_COPY =
  '重置码无效或已过期，或新密码不符合要求，请检查后重试；仍无法重置请联系平台客服。';
/// 对应客户不存在（404）：清输入、终止本次表单，保留找回与登录入口。
const MISSING_CUSTOMER_COPY = '无法完成重置，请联系平台客服获取新的重置码。';
/// 5xx、无法读取响应、未识别的成功状态与网络失败：保守归为结果未知，不自动重试、不宣称未变。
const UNKNOWN_RESULT_COPY =
  '暂时无法确认密码是否已更新，请返回登录尝试新密码；若无法登录，请联系平台客服获取新的重置码。';

/// 重置密码页：凭客服转交的一次性重置码设置新密码（Spec C12 的对客一侧、§4）。
///
/// 兑换沿用无认证的 `CustomerClient`；确定成功只认 204，其余 2xx 归为结果未知。服务端“先消费码、
/// 再写密码”是两步，失败也可能已消耗码，因此界面不保证旧码可重试，用结果未知话术兜底。
export function ResetPasswordPage({ onSignOut }: { onSignOut: () => void }) {
  const { navigate } = usePortalRoute();
  const [state, setState] = useState<ResetState>('editing');
  const [error, setError] = useState<string | null>(null);
  const [form] = Form.useForm<{ token: string; password: string; confirm: string }>();
  // 页面存活标识：晚到的结果不得更新已离开的页面，也不得清掉后来建立的会话（设计 0016 §3）。
  const alive = useRef(true);
  // 请求锁：一次页面实例只能有一个进行中的重置请求。
  const lock = useRef(false);
  useEffect(
    () => () => {
      alive.current = false;
    },
    [],
  );

  async function submit(values: { token: string; password: string; confirm: string }) {
    if (lock.current) return;
    lock.current = true;
    setError(null);
    setState('submitting');
    try {
      await new CustomerClient(() => null).redeemPasswordReset(values.token.trim(), values.password);
      if (!alive.current) return;
      form.resetFields();
      onSignOut();
      setState('succeeded');
    } catch (failure) {
      if (!alive.current) return;
      if (failure instanceof ApiError && failure.status === 429) {
        // 尝试超限是确定的拒绝：保留输入与重置码，回编辑态等重试，不归入结果未知。
        setError(waitingMessage(failure));
        setState('editing');
      } else if (failure instanceof ApiError && failure.status === 400) {
        setError(PARAM_ERROR_COPY);
        setState('editing');
      } else if (failure instanceof ApiError && failure.status === 404) {
        form.resetFields();
        setError(MISSING_CUSTOMER_COPY);
        setState('terminal-error');
      } else {
        form.resetFields();
        setError(UNKNOWN_RESULT_COPY);
        setState('unknown-result');
      }
    } finally {
      lock.current = false;
    }
  }

  function backToLogin() {
    // 从重置页返回登录要清掉本地客户会话、确保登录表单可见；不调用服务端退出接口，也不声称撤销
    // 其他客户的会话（Spec §3）。
    onSignOut();
    navigate(authPath('login'));
  }

  const editing = state === 'editing' || state === 'submitting';

  return (
    <AuthLayout
      title="重置密码"
      description={editing ? '请输入客服提供的重置码。' : undefined}
    >
      {editing ? (
        <>
          <Form
            form={form}
            layout="vertical"
            onFinish={submit}
            disabled={state === 'submitting'}
            requiredMark={false}
          >
            {/* 字段外层给一个稳定锚点：用例按 testid 断字段错误，不靠文案选择器。 */}
            <div data-testid="portal-reset-token-field">
              <Form.Item
                name="token"
                label="重置码"
                rules={[{ required: true, whitespace: true, message: '请输入重置码' }]}
              >
                <Input
                  data-testid="portal-reset-token"
                  prefix={<SafetyOutlined />}
                  size="large"
                  autoComplete="off"
                />
              </Form.Item>
            </div>
            <div data-testid="portal-reset-password-field">
              <Form.Item
                name="password"
                label="新密码（至少 8 个字符）"
                rules={[
                  { required: true, message: '请输入新密码' },
                  { min: 8, message: '新密码至少 8 个字符' },
                ]}
              >
                <Input.Password
                  data-testid="portal-reset-password"
                  prefix={<LockOutlined />}
                  size="large"
                  autoComplete="new-password"
                />
              </Form.Item>
            </div>
            <div data-testid="portal-reset-confirm-field">
              <Form.Item
                name="confirm"
                label="确认新密码"
                dependencies={['password']}
                rules={[
                  { required: true, message: '请再次输入新密码' },
                  ({ getFieldValue }) => ({
                    validator(_rule, value) {
                      if (!value || getFieldValue('password') === value) return Promise.resolve();
                      return Promise.reject(new Error('两次输入的密码不一致'));
                    },
                  }),
                ]}
              >
                <Input.Password
                  data-testid="portal-reset-confirm"
                  prefix={<LockOutlined />}
                  size="large"
                  autoComplete="new-password"
                />
              </Form.Item>
            </div>
            <Button
              data-testid="portal-reset-submit"
              type="primary"
              size="large"
              block
              htmlType="submit"
              loading={state === 'submitting'}
            >
              重置密码
            </Button>
          </Form>

          {state === 'submitting' ? (
            <Alert
              data-testid="portal-reset-pending"
              style={{ marginTop: 12 }}
              type="info"
              showIcon
              message="正在重置，请稍候；离开页面不会取消本次操作。"
            />
          ) : null}

          {error ? (
            <Alert
              data-testid="portal-reset-error"
              style={{ marginTop: 12 }}
              type="error"
              showIcon
              message={error}
            />
          ) : null}

          <Flex vertical gap={2} style={{ marginTop: 16 }}>
            <Button
              data-testid="portal-reset-how"
              type="link"
              style={{ padding: 0, height: 'auto', alignSelf: 'flex-start' }}
              onClick={() => navigate(authPath('forgotPassword'))}
            >
              如何获取重置码？
            </Button>
            <Button
              data-testid="portal-reset-back"
              type="link"
              style={{ padding: 0, height: 'auto', alignSelf: 'flex-start' }}
              onClick={backToLogin}
            >
              返回登录
            </Button>
          </Flex>
        </>
      ) : null}

      {state === 'succeeded' ? (
        <Flex vertical gap={12}>
          <Alert
            data-testid="portal-reset-done"
            type="success"
            showIcon
            message="密码已更新，请重新登录"
          />
          <Button type="primary" block data-testid="portal-reset-back" onClick={backToLogin}>
            返回登录
          </Button>
        </Flex>
      ) : null}

      {state === 'terminal-error' || state === 'unknown-result' ? (
        <Flex vertical gap={12}>
          <Alert data-testid="portal-reset-error" type="error" showIcon message={error} />
          <Button type="primary" block data-testid="portal-reset-back" onClick={backToLogin}>
            返回登录
          </Button>
          <Button block data-testid="portal-reset-again" onClick={() => navigate(authPath('forgotPassword'))}>
            重新进入找回流程
          </Button>
        </Flex>
      ) : null}
    </AuthLayout>
  );
}
