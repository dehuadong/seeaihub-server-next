import { Button, Flex, Typography } from 'antd';
import { authPath, usePortalRoute } from '../routes';
import { AuthLayout } from './layout';

/// 忘记密码页：**只指导联系平台客服**，不能提交邮箱申请重置码。
///
/// 平台上没有“提交邮箱就拿到重置码”这条路——不发邮件、不做邮箱验证，那种入口等于“知道邮箱就能
/// 接管账户”。所以这里只有一个说明与两个入口（Spec §2、C12）。
export function ForgotPasswordPage() {
  const { navigate } = usePortalRoute();

  return (
    <AuthLayout title="忘记密码">
      <Flex vertical gap={16}>
        <Typography.Paragraph data-testid="portal-forgot-hint" style={{ margin: 0 }}>
          请联系平台客服获取重置码。
        </Typography.Paragraph>
        <Flex vertical gap={8}>
          <Button
            data-testid="portal-forgot-have-code"
            type="primary"
            block
            onClick={() => navigate(authPath('resetPassword'))}
          >
            已有重置码，设置新密码
          </Button>
          <Button data-testid="portal-forgot-back" block onClick={() => navigate(authPath('login'))}>
            返回登录
          </Button>
        </Flex>
      </Flex>
    </AuthLayout>
  );
}
