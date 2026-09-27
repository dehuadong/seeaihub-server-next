import { App as AntApp, ConfigProvider, Layout, Menu, Typography, theme } from 'antd';
import {
  ApiOutlined,
  AuditOutlined,
  DeploymentUnitOutlined,
  DollarOutlined,
  KeyOutlined,
  LogoutOutlined,
  MenuFoldOutlined,
  MenuUnfoldOutlined,
  SwapOutlined,
  TeamOutlined,
  UserOutlined,
} from '@ant-design/icons';
import { useCallback, useMemo, useState } from 'react';
import { AdminClient } from './client';
import { useAdminSession } from './session';
import { useHashRoute, type Route } from '../shared/routes';
import { AccountsPage } from './pages/Accounts';
import { CustomersPage } from './pages/Customers';
import { DiagnosticsPage } from './pages/Diagnostics';
import { ChangePasswordPanel, LoginPage } from './pages/Login';
import { ModelsPage } from './pages/Models';
import { PublishPage } from './pages/Publish';
import { RatesPage } from './pages/Rates';
import { RoutingPage } from './pages/Routing';

const NAV: { route: Route; label: string; icon: React.ReactNode }[] = [
  // 「模型目录」是运营的说法（列在售的模型与它们的价目）；`Gateway Model` 是平台内部名。
  { route: 'models', label: '模型目录', icon: <DeploymentUnitOutlined /> },
  // 「上架与改价」是这一页对运营的用途：让一个型号可售、改它的价。它写的就是发布修订这件事。
  { route: 'publish', label: '上架与改价', icon: <ApiOutlined /> },
  { route: 'accounts', label: '账户', icon: <KeyOutlined /> },
  { route: 'customers', label: '客户', icon: <TeamOutlined /> },
  { route: 'diagnostics', label: '对账与诊断', icon: <AuditOutlined /> },
  { route: 'rates', label: '折算率', icon: <DollarOutlined /> },
  { route: 'routing', label: '路由策略', icon: <SwapOutlined /> },
];

/// 管理端外壳。
///
/// 主题与语言在这里收口：`ConfigProvider` 一处配好，各页不必各自设字号与颜色；`AntApp` 提供
/// `message` / `modal` 的上下文（操作反馈统一走它，不再各页自己摆一行提示文字）。
export function App() {
  return (
    <ConfigProvider
      theme={{
        algorithm: theme.defaultAlgorithm,
        token: { colorPrimary: '#1668dc', borderRadius: 6 },
      }}
    >
      <AntApp>
        <Console />
      </AntApp>
    </ConfigProvider>
  );
}

function Console() {
  const { token, email, signOut } = useAdminSession();
  const [route, navigate] = useHashRoute();
  const [collapsed, setCollapsed] = useState(false);
  /// "改口令"面板默认收起：它是低频操作，不该常占着页面。
  const [showPassword, setShowPassword] = useState(false);

  /// 令牌的取值函数传给客户端：它每次请求时读当前值，所以换会话不必重建客户端。
  const tokenGetter = useCallback(() => token, [token]);
  const client = useMemo(() => new AdminClient(tokenGetter), [tokenGetter]);

  // **路由守卫**：没登录就只渲染登录页。六个页面的组件在登录之前根本不挂载，因此也不会发任何
  // 管理 API 取数请求（Spec M7）——会话过期或被吊销时，返回来的 403 会把我们带回这里。
  if (!token) return <LoginPage />;

  const current = NAV.find((item) => item.route === route);

  return (
    <Layout style={{ minHeight: '100vh' }}>
      <Layout.Sider
        theme="light"
        collapsible
        collapsed={collapsed}
        onCollapse={setCollapsed}
        trigger={null}
        width={216}
        style={{ borderInlineEnd: '1px solid #f0f0f0' }}
      >
        <div style={{ padding: collapsed ? '16px 8px' : '16px 20px' }}>
          <Typography.Text strong style={{ fontSize: 16, whiteSpace: 'nowrap' }}>
            {collapsed ? 'seeai' : 'seeai 运营后台'}
          </Typography.Text>
        </div>
        <Menu
          mode="inline"
          selectedKeys={[route]}
          style={{ borderInlineEnd: 0 }}
          items={NAV.map((item) => ({
            key: item.route,
            icon: item.icon,
            label: item.label,
          }))}
          onClick={({ key }) => navigate(key as Route)}
        />
      </Layout.Sider>
      <Layout>
        <Layout.Header
          style={{
            background: '#fff',
            borderBottom: '1px solid #f0f0f0',
            paddingInline: 20,
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'space-between',
          }}
        >
          <Menu
            mode="horizontal"
            selectable={false}
            style={{ borderBottom: 0, flex: 1 }}
            items={[
              {
                key: 'toggle',
                icon: collapsed ? <MenuUnfoldOutlined /> : <MenuFoldOutlined />,
                label: '',
                onClick: () => setCollapsed((value) => !value),
              },
              { key: 'where', label: current?.label ?? '', disabled: true },
            ]}
          />
          <Menu
            mode="horizontal"
            selectable={false}
            style={{ borderBottom: 0 }}
            items={[
              {
                key: 'me',
                icon: <UserOutlined />,
                label: email ?? '',
                children: [
                  { key: 'password', label: '改口令' },
                  { type: 'divider' as const },
                  { key: 'signout', label: '退出登录', icon: <LogoutOutlined />, danger: true },
                ],
                onClick: ({ key }) => {
                  if (key === 'signout') signOut();
                  if (key === 'password') setShowPassword(true);
                },
              },
            ]}
          />
        </Layout.Header>
        <Layout.Content style={{ padding: 20 }}>
          {showPassword ? (
            <div style={{ marginBottom: 16 }}>
              <ChangePasswordPanel onClose={() => setShowPassword(false)} />
            </div>
          ) : null}
          {route === 'models' ? <ModelsPage client={client} /> : null}
          {route === 'publish' ? <PublishPage client={client} /> : null}
          {route === 'rates' ? <RatesPage client={client} /> : null}
          {route === 'routing' ? <RoutingPage client={client} /> : null}
          {route === 'accounts' ? <AccountsPage client={client} /> : null}
          {route === 'customers' ? <CustomersPage client={client} /> : null}
          {route === 'diagnostics' ? <DiagnosticsPage client={client} /> : null}
        </Layout.Content>
      </Layout>
    </Layout>
  );
}
