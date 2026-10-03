import { ApiError, apiFetch } from '../shared/api';
import type {
  CustomerBilling,
  CustomerKeysResponse,
  CustomerLedgerResponse,
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

  /// 改自己账户的名称。账户由会话确定，请求体里没有账户标识；服务端不接受清空。
  renameAccount(name: string): Promise<void> {
    return this.send('/v1/customer/account/name', 'PUT', { name });
  }

  /// 账单汇总：按区间**全量**算，不随逐笔列表的条数上限变化。
  billing(range: { since?: string; until?: string } = {}): Promise<CustomerBilling> {
    return this.get(`/v1/customer/billing?${windowQuery(range)}`);
  }

  /// 真实资金流水：区间半开 `[since, until)`、可按类别筛选、可带上一页给的游标。
  ///
  /// 不带任何新参数时就是旧调用（全部类别、第一页）。
  ledger(params: LedgerQuery = {}): Promise<CustomerLedgerResponse> {
    return this.get(`/v1/customer/ledger?${windowQuery(params)}`);
  }

  /// 调用记录：`view=active` 是处理中、`view=completed` 是已结束历史；不带 `view` 是两者的合并。
  usage(params: UsageQuery = {}): Promise<CustomerUsageResponse> {
    return this.get(`/v1/customer/usage?${windowQuery(params)}`);
  }
}

/// 区间、条数上限与翻页参数。缺省条数由服务端夹上限，这里给一个够看一屏的数。
const DEFAULT_PAGE = 50;

function windowQuery(params: {
  since?: string;
  until?: string;
  limit?: number;
  cursor?: string;
  kind?: string;
  view?: string;
}): string {
  const query = new URLSearchParams();
  if (params.since) query.set('since', params.since);
  if (params.until) query.set('until', params.until);
  if (params.kind) query.set('kind', params.kind);
  if (params.view) query.set('view', params.view);
  if (params.cursor) query.set('cursor', params.cursor);
  query.set('limit', String(params.limit ?? DEFAULT_PAGE));
  return `${query}`;
}

interface WindowQuery {
  since?: string;
  until?: string;
  limit?: number;
}

/// 资金流水查询：窗口 + 类别 + 游标。
export interface LedgerQuery extends WindowQuery {
  kind?: string;
  cursor?: string;
}

/// 调用记录查询：窗口 + 视图 + 游标。
export interface UsageQuery extends WindowQuery {
  view?: 'active' | 'completed';
  cursor?: string;
}
