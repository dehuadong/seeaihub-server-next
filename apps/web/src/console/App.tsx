import { App as AntApp, ConfigProvider, Layout, Menu, Typography, theme } from 'antd';
import {
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
import { useCallback, useMemo, useRef, useState } from 'react';
import { AdminClient } from './client';
import { useAdminSession } from './session';
import { accountPath, customerPath, useHashRoute, type Route } from '../shared/routes';
import { AccountDetailPage, AccountsPage } from './pages/Accounts';
import { CustomerDetailPage, CustomersPage } from './pages/Customers';
import { DiagnosticsPage } from './pages/Diagnostics';
import { ChangePasswordPanel, LoginPage } from './pages/Login';
import { ModelsPage } from './pages/Models';
import { RatesPage } from './pages/Rates';
import { RoutingPage } from './pages/Routing';
import { ConsoleNotFound } from './ui';

const NAV: { route: Route; label: string; icon: React.ReactNode }[] = [
  // 一页看、一页写会让人看不出两者的联系，所以「上架与改价」并进了「模型目录」：那一页右上角
  // 就是"上架新模型"，每行有一个"改价"。`#/publish` 仍作为旧地址落到同一页。
  { route: 'models', label: '模型目录', icon: <DeploymentUnitOutlined /> },
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
  const [location, navigate] = useHashRoute();
  const [collapsed, setCollapsed] = useState(false);
  /// "改口令"面板默认收起：它是低频操作，不该常占着页面。
  const [showPassword, setShowPassword] = useState(false);

  /// 令牌的取值函数传给客户端：它每次请求时读当前值，所以换会话不必重建客户端。
  const tokenGetter = useCallback(() => token, [token]);
  /// 现在这枚凭据的最新值。403 的收尾要拿它比对，而**旧页面里在飞的请求持有的是旧闭包**——只有
  /// ref 读到的是最新值，否则换人登录后一条迟到的 403 会把新会话踢回登录页。
  const tokenRef = useRef<string | null>(token);
  tokenRef.current = token;
  const onUnauthorized = useCallback(
    (rejected: string | null) => {
      if (rejected !== null && rejected === tokenRef.current) signOut();
    },
    [signOut],
  );
  const client = useMemo(
    () => new AdminClient(tokenGetter, onUnauthorized),
    [tokenGetter, onUnauthorized],
  );

  // **路由守卫**：没登录就只渲染登录页。六个页面的组件在登录之前根本不挂载，因此也不会发任何
  // 管理 API 取数请求（Spec M7）——会话过期或被吊销时，返回来的 403 会把我们带回这里。
  //
  // 地址**保持不变**：登录成功后仍然解析同一段 hash，于是直接打开的账户／客户详情、以及刷新时
  // 停留的页面都会自动回到原处（Spec D6）。
  if (!token) return <LoginPage />;

  const current = NAV.find((item) => item.route === location.page);
  const openAccount = (accountId: string) => navigate(accountPath(accountId));
  const openCustomer = (customerId: string) => navigate(customerPath(customerId));

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
          // 列表与详情映射到同一个侧栏项：详情页里「账户」仍然是选中的那一项。
          selectedKeys={[location.page]}
          style={{ borderInlineEnd: 0 }}
          items={NAV.map((item) => ({
            key: item.route,
            icon: item.icon,
            // 列表与详情共用一个工作区，所以"当前在哪个工作区"由地址里的页面段决定；`aria-current`
            // 让这件事既能被读屏软件念出来，也能被浏览器用例当作稳定锚点断言。
            label: (
              <span aria-current={location.page === item.route ? 'page' : undefined}>
                {item.label}
              </span>
            ),
          }))}
          onClick={({ key }) => navigate(key)}
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
              {
                key: 'where',
                label: current?.label ?? (location.page === 'not-found' ? '找不到' : ''),
                disabled: true,
              },
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
          {location.page === 'models' || location.page === 'publish' ? (
            <ModelsPage
              client={client}
              // `#/publish` 是合并之前的旧地址：它现在落到本页并把"改价"抽屉带上，旧链接与书签不废。
              editRequest={location.page === 'publish' ? '' : undefined}
              onEditRequestHandled={
                location.page === 'publish' ? () => navigate('models') : undefined
              }
            />
          ) : null}
          {location.page === 'rates' ? <RatesPage client={client} /> : null}
          {location.page === 'routing' ? <RoutingPage client={client} /> : null}
          {location.page === 'accounts' ? (
            location.detailId ? (
              <AccountDetailPage
                // 换一个账户就重挂：一次性明文与模块状态必须跟着标识换掉，不能留在下一个账户上。
                key={location.detailId}
                client={client}
                accountId={location.detailId}
                onBack={() => navigate('accounts')}
              />
            ) : (
              <AccountsPage client={client} onOpenAccount={openAccount} />
            )
          ) : null}
          {location.page === 'customers' ? (
            location.detailId ? (
              <CustomerDetailPage
                key={location.detailId}
                client={client}
                customerId={location.detailId}
                onBack={() => navigate('customers')}
                onOpenAccount={openAccount}
              />
            ) : (
              <CustomersPage client={client} onOpenCustomer={openCustomer} />
            )
          ) : null}
          {location.page === 'diagnostics' ? <DiagnosticsPage client={client} /> : null}
          {location.page === 'not-found' ? (
            <ConsoleNotFound
              what="页面"
              backLabel="回模型目录"
              onBack={() => navigate('models')}
            />
          ) : null}
        </Layout.Content>
      </Layout>
    </Layout>
  );
}
