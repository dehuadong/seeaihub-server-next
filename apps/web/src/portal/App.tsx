import { App as AntApp, ConfigProvider, theme } from 'antd';
import { useCallback, useMemo } from 'react';
import { AuthPage } from './Auth';
import { CustomerClient } from './client';
import { Dashboard } from './Dashboard';
import { CustomerSessionProvider, useCustomerSession } from './session';

/// 客户控制台的入口。
///
/// **未登录时只渲染登录/注册页**：账户、密钥与账务的组件在登录成功之前根本不挂载，所以不会向
/// 服务端发出任何取数请求（与 Spec C11 同一条纪律）。
function Portal() {
  const { token } = useCustomerSession();
  const tokenGetter = useCallback(() => token, [token]);
  const client = useMemo(() => new CustomerClient(tokenGetter), [tokenGetter]);

  if (!token) return <AuthPage />;
  return <Dashboard client={client} />;
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
