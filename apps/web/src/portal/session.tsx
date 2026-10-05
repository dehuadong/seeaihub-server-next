import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from 'react';
import { clearReturnTo } from './return-to';

/// 客户会话的存放位置。
///
/// 键名带 `portal` 前缀，与管理端各存各的：同一个浏览器同时开两个控制台也不会串。
/// 令牌对客户端是不透明的——服务端每次都读库判有效性，过期了会被拒，届时回到登录/注册页。
/// **凭据只存 `sessionStorage`，不进 URL**（Spec D2）。
const TOKEN_KEY = 'seeai.portal.session';

interface CustomerSessionState {
  token: string | null;
  email: string | null;
  accountId: string | null;
  signIn: (session: { token: string; expires_at: string; email: string; account_id: string }) => void;
  signOut: () => void;
}

const CustomerSessionContext = createContext<CustomerSessionState | null>(null);

export function CustomerSessionProvider({ children }: { children: ReactNode }) {
  const [token, setToken] = useState<string | null>(() => sessionStorage.getItem(TOKEN_KEY));
  const [email, setEmail] = useState<string | null>(() =>
    sessionStorage.getItem(`${TOKEN_KEY}.email`),
  );
  const [accountId, setAccountId] = useState<string | null>(() =>
    sessionStorage.getItem(`${TOKEN_KEY}.account`),
  );
  const [expiresAt, setExpiresAt] = useState<string | null>(() =>
    sessionStorage.getItem(`${TOKEN_KEY}.expires`),
  );

  const signIn = useCallback(
    (session: { token: string; expires_at: string; email: string; account_id: string }) => {
      sessionStorage.setItem(TOKEN_KEY, session.token);
      sessionStorage.setItem(`${TOKEN_KEY}.email`, session.email);
      sessionStorage.setItem(`${TOKEN_KEY}.account`, session.account_id);
      sessionStorage.setItem(`${TOKEN_KEY}.expires`, session.expires_at);
      setToken(session.token);
      setEmail(session.email);
      setAccountId(session.account_id);
      setExpiresAt(session.expires_at);
    },
    [],
  );

  const signOut = useCallback(() => {
    sessionStorage.removeItem(TOKEN_KEY);
    sessionStorage.removeItem(`${TOKEN_KEY}.email`);
    sessionStorage.removeItem(`${TOKEN_KEY}.account`);
    sessionStorage.removeItem(`${TOKEN_KEY}.expires`);
    setToken(null);
    setEmail(null);
    setAccountId(null);
    setExpiresAt(null);
  }, []);

  /// 会话到期即清屏：服务端每次读库判有效性，但页面不能等下一次请求才发现"过期了"——到点就清掉
  /// 本地会话与页面数据、回登录页（`docs/design/0014` §4）。
  useEffect(() => {
    if (!token || !expiresAt) return;
    const expire = () => {
      // 与 401 清屏同一规则：会话失效时连旧回跳目标一起去掉（设计 0016 §2）。
      clearReturnTo();
      signOut();
    };
    const remaining = new Date(expiresAt).getTime() - Date.now();
    if (Number.isNaN(remaining)) return;
    if (remaining <= 0) {
      expire();
      return;
    }
    const timer = setTimeout(expire, remaining);
    return () => clearTimeout(timer);
  }, [token, expiresAt, signOut]);

  const value = useMemo(
    () => ({ token, email, accountId, signIn, signOut }),
    [token, email, accountId, signIn, signOut],
  );
  return <CustomerSessionContext.Provider value={value}>{children}</CustomerSessionContext.Provider>;
}

export function useCustomerSession(): CustomerSessionState {
  const session = useContext(CustomerSessionContext);
  if (!session) throw new Error('useCustomerSession 必须在 CustomerSessionProvider 内使用');
  return session;
}
