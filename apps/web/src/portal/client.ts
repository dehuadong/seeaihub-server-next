import { ApiError, apiFetch } from '../shared/api';
import type {
  AccountEntriesResponse,
  CustomerBilling,
  CustomerKeysResponse,
  CustomerSession,
  CustomerUsageResponse,
  IssueApiKeyResponse,
  OwnAccount,
} from '../shared/types';

/// 对客 API 的客户端。**只调 `/v1/customer/*`**：管理面那批端点一条都不在这里，
/// 客户会话调它们只会被拒（那正是服务端的隔离）。
export class CustomerClient {
  constructor(
    private readonly token: () => string | null,
    /// 会话失效（服务端回 401）时的回调：由外壳清掉本地会话与页面数据、回登录页。
    private readonly onUnauthorized?: () => void,
  ) {}

  private get<T>(path: string): Promise<T> {
    return this.observe(apiFetch<T>(path, this.token));
  }

  private send<T>(path: string, method: string, body?: unknown, admin = true): Promise<T> {
    return this.observe(apiFetch<T>(path, this.token, { method, body, admin }));
  }

  /// 401 是"这次会话不被接受"：集中清屏，不让旧账务或密钥留在可见页面上（`docs/design/0014` §4）。
  private async observe<T>(call: Promise<T>): Promise<T> {
    try {
      return await call;
    } catch (failure) {
      if (failure instanceof ApiError && failure.status === 401) this.onUnauthorized?.();
      throw failure;
    }
  }

  /// 注册（未认证）：拿到会话与自己的账户。
  register(email: string, password: string): Promise<CustomerSession> {
    return this.send('/v1/customers', 'POST', { email, password }, false);
  }

  /// 登录（未认证）。
  login(email: string, password: string): Promise<CustomerSession> {
    return this.send('/v1/customer/sessions', 'POST', { email, password }, false);
  }

  logout(): Promise<void> {
    return this.send('/v1/customer/sessions', 'DELETE');
  }

  changePassword(currentPassword: string, newPassword: string): Promise<void> {
    return this.send('/v1/customer/password', 'PUT', {
      current_password: currentPassword,
      new_password: newPassword,
    });
  }

  /// 凭运营转交的一次性令牌设置新口令（**无需登录**：忘了口令的人本来就进不来）。
  redeemPasswordReset(resetToken: string, newPassword: string): Promise<void> {
    return this.send(
      '/v1/customer/password-resets/redeem',
      'POST',
      { reset_token: resetToken, new_password: newPassword },
      false,
    );
  }

  apiKeys(): Promise<CustomerKeysResponse> {
    return this.get('/v1/customer/api-keys');
  }

  issueApiKey(label: string): Promise<IssueApiKeyResponse> {
    return this.send('/v1/customer/api-keys', 'POST', { label });
  }

  revokeApiKey(keyId: string): Promise<void> {
    return this.send(`/v1/customer/api-keys/${encodeURIComponent(keyId)}`, 'DELETE');
  }

  account(): Promise<OwnAccount> {
    return this.get('/v1/customer/account');
  }

  ledger(limit = 50): Promise<AccountEntriesResponse> {
    return this.get(`/v1/customer/ledger?limit=${limit}`);
  }

  usage(limit = 50): Promise<CustomerUsageResponse> {
    return this.get(`/v1/customer/usage?limit=${limit}`);
  }

  billing(): Promise<CustomerBilling> {
    return this.get('/v1/customer/billing');
  }
}
