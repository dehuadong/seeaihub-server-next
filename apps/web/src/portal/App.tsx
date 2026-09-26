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

export function PortalApp() {
  return (
    <CustomerSessionProvider>
      <Portal />
    </CustomerSessionProvider>
  );
}
