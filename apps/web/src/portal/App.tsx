import {
  App as AntApp,
  Button,
  ConfigProvider,
  Drawer,
  Grid,
  Layout,
  Menu,
  Typography,
  theme,
} from 'antd';
import {
  AccountBookOutlined,
  HistoryOutlined,
  HomeOutlined,
  KeyOutlined,
  MenuOutlined,
  SettingOutlined,
} from '@ant-design/icons';
import { useCallback, useEffect, useMemo, useState, type ReactNode } from 'react';
import { ForgotPasswordPage } from './auth/ForgotPasswordPage';
import { LoginPage } from './auth/LoginPage';
import { ResetPasswordPage } from './auth/ResetPasswordPage';
import { CustomerClient } from './client';
import { clearReturnTo } from './return-to';
import { CustomerSessionProvider, useCustomerSession } from './session';
import { isAuthRoute, portalPath, usePortalRoute } from './routes';
import type { PortalRoute } from './paths';
import { BillingPage } from './pages/Billing';
import { KeysPage } from './pages/Keys';
import { NotFoundPage } from './pages/NotFound';
import { OverviewPage } from './pages/Overview';
import { SettingsPage } from './pages/Settings';
import { UsagePage } from './pages/Usage';

/// 五个客户页面的固定导航（`docs/design/0014` §1）。标签就是页面名，与地址一一对应。
const NAV: { route: PortalRoute; label: string; icon: ReactNode }[] = [
  { route: 'overview', label: '概览', icon: <HomeOutlined /> },
  { route: 'usage', label: '调用记录', icon: <HistoryOutlined /> },
  { route: 'billing', label: '账单与资金记录', icon: <AccountBookOutlined /> },
  { route: 'keys', label: 'API Key', icon: <KeyOutlined /> },
  { route: 'settings', label: '账户设置', icon: <SettingOutlined /> },
];

/// 有会话访问 `/login` 或 `/forgot-password`：回概览，不请求新的登录会话（Spec §3）。
function RedirectToOverview() {
  const { replace } = usePortalRoute();
  useEffect(() => {
    replace(portalPath('overview'));
  }, [replace]);
  return null;
}

/// 已登录的客户外壳：固定导航与五页内容。
function CustomerShell() {
  const { token, email, signOut } = useCustomerSession();
  const { route, navigate } = usePortalRoute();
  const [menuOpen, setMenuOpen] = useState(false);
  const screens = Grid.useBreakpoint();
  // 首次渲染断点还没算出来时按桌面处理：桌面浏览器与 e2e 不会先看到窄屏外壳再换回来。
  const isDesktop = screens.md !== false;

  // 服务端回 401 就是"这次会话不被接受"：集中清屏，不让旧账务或密钥留在可见页面上（设计 0014 §4）。
  // 同时清掉旧回跳目标——失效会话不该把用户带回上一次的账单区间（设计 0016 §2）。
  const onUnauthorized = useCallback(() => {
    signOut();
    clearReturnTo();
  }, [signOut]);
  const tokenGetter = useCallback(() => token, [token]);
  const client = useMemo(
    () => new CustomerClient(tokenGetter, onUnauthorized),
    [tokenGetter, onUnauthorized],
  );

  const onSignOut = () => {
    signOut();
    clearReturnTo();
    navigate(portalPath('overview'));
  };

  const page = (() => {
    switch (route) {
      case 'overview':
        return <OverviewPage client={client} onOpen={navigate} />;
      case 'usage':
        return <UsagePage client={client} />;
      case 'billing':
        return <BillingPage client={client} />;
      case 'keys':
        return <KeysPage client={client} />;
      case 'settings':
        return <SettingsPage client={client} onSignOut={onSignOut} />;
      default:
        return <NotFoundPage onBack={() => navigate(portalPath('overview'))} />;
    }
  })();

  const navItems = NAV.map((item) => ({ key: item.route, icon: item.icon, label: item.label }));
  const onNavClick = ({ key }: { key: string }) => {
    const target = NAV.find((item) => item.route === key);
    if (target) navigate(portalPath(target.route));
    setMenuOpen(false);
  };
  const selected = route === 'not-found' ? [] : [route];

  return (
    <Layout style={{ minHeight: '100vh' }}>
      <Layout.Header
        style={{
          background: '#fff',
          borderBottom: '1px solid #f0f0f0',
          paddingInline: 20,
          display: 'flex',
          alignItems: 'center',
          gap: 16,
        }}
      >
        <Typography.Text strong style={{ fontSize: 16, whiteSpace: 'nowrap' }}>
          seeai 控制台
        </Typography.Text>
        {isDesktop ? (
          <Menu
            mode="horizontal"
            selectedKeys={selected}
            items={navItems}
            onClick={onNavClick}
            style={{ flex: 1, borderBottom: 0, minWidth: 0 }}
          />
        ) : (
          <Button
            data-testid="portal-menu-open"
            icon={<MenuOutlined />}
            onClick={() => setMenuOpen(true)}
          >
            菜单
          </Button>
        )}
        <Typography.Text
          type="secondary"
          style={{ marginInlineStart: 'auto', whiteSpace: 'nowrap' }}
        >
          {email}
        </Typography.Text>
      </Layout.Header>
      <Layout.Content style={{ padding: 24 }}>
        <div style={{ maxWidth: 1100, margin: '0 auto' }}>{page}</div>
      </Layout.Content>
      <Drawer
        title="导航"
        placement="left"
        open={menuOpen}
        onClose={() => setMenuOpen(false)}
        width={240}
      >
        <Menu
          mode="inline"
          selectedKeys={selected}
          items={navItems}
          onClick={onNavClick}
          style={{ borderInlineEnd: 0 }}
        />
      </Drawer>
    </Layout>
  );
}

/// 公开认证地址与受保护地址的分发（设计 0016 §1）。
///
/// 公开认证页在会话守卫**之前**识别：未登录直达受保护或未知地址仍在原地址显示登录、不挂载账户组件；
/// 有会话访问登录或忘记密码回概览，重置页始终可开且不展示账户数据。
function Portal() {
  const { token, signOut } = useCustomerSession();
  const { route } = usePortalRoute();

  if (isAuthRoute(route)) {
    if (route === 'resetPassword') return <ResetPasswordPage onSignOut={signOut} />;
    if (token) return <RedirectToOverview />;
    return route === 'login' ? <LoginPage context="public" /> : <ForgotPasswordPage />;
  }

  if (!token) return <LoginPage context="guard" />;
  return <CustomerShell />;
}

/// 与运营后台同一套主题与语言：两边的组件库与观感一致，运维与客户看到的不是两种东西。
///
/// 这个入口**只引客户侧的模块**：管理端的页面与它的取数封装都不在这里的依赖图里，所以产物里不会
/// 出现管理端代码（`docs/specs/0001-admin-and-customer-consoles.md` §5 的 V-D6）。构建末尾的隔离核对
/// 会验证这一点。
export function PortalApp() {
  return (
    <ConfigProvider
      theme={{
        algorithm: theme.defaultAlgorithm,
        token: { colorPrimary: '#1668dc', borderRadius: 6 },
      }}
    >
      <AntApp>
        <CustomerSessionProvider>
          <Portal />
        </CustomerSessionProvider>
      </AntApp>
    </ConfigProvider>
  );
}
