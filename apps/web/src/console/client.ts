import { ApiError, apiFetch, type AdminToken } from '../shared/api';
import type {
  AccountBalance,
  AccountEntriesResponse,
  AccountsResponse,
  AccountSummary,
  AdminUsageResponse,
  CreateAccountResponse,
  CustomerView,
  GatewayModelsResponse,
  IssueApiKeyResponse,
  ProviderCostGapsResponse,
  ProviderFailuresResponse,
  ReconciliationCase,
  RoutePoliciesResponse,
  RoutePolicy,
  RouteStrategy,
  SelectableOfferingsResponse,
} from '../shared/types';

/// 管理 API 的每一个端点在这里各有一个函数：**页面不拼路径、不拼查询串**。
///
/// 与 `apps/api/src/main.rs` 的路由表一一对应；域名前缀由 Vite 代理（开发）或同源部署（生产）提供。
export class AdminClient {
  constructor(
    private readonly token: AdminToken,
    /// 凭据不再被接受时的收尾动作（入口传 `signOut`）。参数是这次请求真正用的那枚凭据。
    private readonly onUnauthorized?: (rejectedToken: string | null) => void,
  ) {}

  /// 一次管理 API 调用。
  ///
  /// **403 在管理面只有一个含义：这次凭据不被接受**——会话过期、被吊销，或拿的是共享令牌而这条端点
  /// 只认会话。此时唯一能解决它的动作是重新登录，所以在这里集中上报（Spec M7；会话结束要顺带清掉
  /// 上一名运营留下的列表筛选，见设计 `0011` §4.4.1），页面不必各自判断。
  ///
  /// 上报时把**这次用的那枚凭据**一起交出去：403 可能迟到（换人登录之后才回来），收尾那一侧要能
  /// 判断被拒的是不是现在这枚，别拿一条迟到的 403 把新会话踢回登录页。
  private call<T>(run: () => Promise<T>): Promise<T> {
    const used = this.token();
    return run().catch((failure: unknown) => {
      if (failure instanceof ApiError && failure.status === 403) this.onUnauthorized?.(used);
      throw failure;
    });
  }

  private get<T>(path: string): Promise<T> {
    return this.call(() => apiFetch<T>(path, this.token));
  }

  private send<T>(path: string, method: string, body?: unknown): Promise<T> {
    return this.call(() => apiFetch<T>(path, this.token, { method, body }));
  }

  gatewayModels(): Promise<GatewayModelsResponse> {
    return this.get('/api/v1/gateway-models');
  }

  setGatewayModelEnabled(gatewayModel: string, enabled: boolean): Promise<void> {
    return this.send(`/api/v1/gateway-models/${encodeURIComponent(gatewayModel)}`, 'PATCH', { enabled });
  }

  /// 只改并发名额（必填的正整数）。
  setGatewayModelConcurrency(gatewayModel: string, maxConcurrentJobs: number): Promise<void> {
    return this.send(`/api/v1/gateway-models/${encodeURIComponent(gatewayModel)}`, 'PATCH', {
      max_concurrent_jobs: maxConcurrentJobs,
    });
  }

  setOfferingEnabled(offeringId: string, enabled: boolean): Promise<void> {
    return this.send(`/api/v1/offerings/${offeringId}`, 'PATCH', { enabled });
  }

  /// **可被选中的 Offering 清单**：发布平台模型时"选 vendor → 选供给"用的那一份。
  ///
  /// 它不含渠道地址与凭证变量名——那是渠道部署事实，选择用不到（`docs/design/0012` §2.1）。
  selectableOfferings(): Promise<SelectableOfferingsResponse> {
    return this.get('/api/v1/offerings');
  }

  publishRevision(command: unknown): Promise<{ runtime_revision_id: string; gateway_model: string }> {
    return this.send('/api/v1/runtime-revisions', 'POST', command);
  }

  upsertFxRate(currency: string, rateMicros: number, effectiveAt?: string): Promise<void> {
    const body: Record<string, unknown> = { currency, rate_micros: rateMicros };
    if (effectiveAt) body.effective_at = effectiveAt;
    return this.send('/api/v1/fx-rates', 'PUT', body);
  }

  /// 每个币种**当前生效**的那一行（页面要能看出平台真正在用哪个数）。
  fxRates(): Promise<{ rates: { currency: string; rate_micros: number; effective_at: string }[] }> {
    return this.get('/api/v1/fx-rates');
  }

  /// 替客户开户：不给 `accountId` 就新建空账户，给了就把登录身份配到那个**已有账户**上。
  /// 开户：两个模式共用一个请求——给了 `accountId` 就是绑定已有账户（此时不能再给名称），
  /// 否则新建账户，`accountName` 留空即由服务端按登录邮箱生成。
  openCustomer(
    email: string,
    password?: string,
    accountId?: string,
    accountName?: string,
  ): Promise<CustomerView> {
    const body: Record<string, unknown> = { email };
    if (password) body.password = password;
    if (accountId) body.account_id = accountId;
    if (accountName?.trim()) body.account_name = accountName.trim();
    return this.send('/api/v1/customers', 'POST', body);
  }

  /// 按邮箱找客户账户（给客户充值、签重置令牌都要先拿到账户标识）。
  findCustomer(email: string): Promise<{ customers: CustomerView[] }> {
    return this.get(`/api/v1/customers?email=${encodeURIComponent(email)}`);
  }

  listCustomers(limit = 50): Promise<{ customers: CustomerView[] }> {
    return this.get(`/api/v1/customers?limit=${limit}`);
  }

  /// 按客户标识读客户视图（详情直达与刷新用）。不存在时 404。
  customerView(customerId: string): Promise<CustomerView> {
    return this.get(`/api/v1/customers/${encodeURIComponent(customerId)}`);
  }

  /// 为客户账户签发一次性重置令牌（运营转交；平台不发邮件）。
  issueCustomerPasswordReset(
    accountId: string,
  ): Promise<{ reset_token: string; expires_at: string }> {
    return this.send(
      `/api/v1/accounts/${encodeURIComponent(accountId)}/password-reset`,
      'POST',
    );
  }

  routePolicies(): Promise<RoutePoliciesResponse> {
    return this.get('/api/v1/route-policies');
  }

  upsertRoutePolicy(
    gatewayModel: string | null,
    strategy: RouteStrategy,
    discountRates: Record<string, number> = {},
    tagChannelMap: Record<string, string> = {},
  ): Promise<RoutePolicy> {
    return this.send('/api/v1/route-policies', 'PUT', {
      gateway_model: gatewayModel,
      strategy,
      discount_rates: discountRates,
      tag_channel_map: tagChannelMap,
    });
  }

  /// 建账户：名称与标签和初始充值**一次提交**（不再有"账户建好了但标签没写上"的中间状态）。
  /// 名称为空即省略——服务端按规则生成，不由前端拼。
  createAccount(
    initialCreditMicrousd: number,
    name?: string,
    tag?: string,
  ): Promise<CreateAccountResponse> {
    const body: Record<string, unknown> = { initial_credit_microusd: initialCreditMicrousd };
    if (name?.trim()) body.name = name.trim();
    if (tag?.trim()) body.tag = tag.trim();
    return this.send('/api/v1/accounts', 'POST', body);
  }

  accountBalance(accountId: string): Promise<AccountBalance> {
    return this.get(`/api/v1/accounts/${encodeURIComponent(accountId)}`);
  }

  /// 按账户标识读摘要（详情直达与刷新用；理由见 `HubRepository::find_account_summary`）。不存在时 404。
  accountSummary(accountId: string): Promise<AccountSummary> {
    return this.get(`/api/v1/accounts/${encodeURIComponent(accountId)}/summary`);
  }

  /// 列账户：运营**先找到再操作**的入口。`email` 与 `tag` 都是精确匹配（服务端按"与"处理）。
  listAccounts(
    filter: { email?: string; tag?: string; name?: string; limit?: number } = {},
  ): Promise<AccountsResponse> {
    const query = new URLSearchParams();
    if (filter.email?.trim()) query.set('email', filter.email.trim());
    if (filter.tag?.trim()) query.set('tag', filter.tag.trim());
    if (filter.name?.trim()) query.set('name', filter.name.trim());
    query.set('limit', String(filter.limit ?? 50));
    return this.get(`/api/v1/accounts?${query}`);
  }

  /// 账目条目。`kind` 只读某一类（**充值记录**只看 `credit`）；缺省读全部。
  accountEntries(
    accountId: string,
    kind?: string,
    limit = 50,
  ): Promise<AccountEntriesResponse> {
    const query = new URLSearchParams();
    if (kind) query.set('kind', kind);
    query.set('limit', String(limit));
    return this.get(`/api/v1/accounts/${encodeURIComponent(accountId)}/entries?${query}`);
  }

  /// 某个账户的**调用明细**（管理员面）：逐笔生成请求，带 `job_id`。
  accountUsage(accountId: string, limit = 50): Promise<AdminUsageResponse> {
    return this.get(
      `/api/v1/accounts/${encodeURIComponent(accountId)}/usage?limit=${limit}`,
    );
  }

  creditAccount(accountId: string, amountMicrousd: number, businessKey: string): Promise<void> {
    return this.send(`/api/v1/accounts/${encodeURIComponent(accountId)}/credits`, 'POST', {
      amount_microusd: amountMicrousd,
      business_key: businessKey,
    });
  }

  setAccountTag(accountId: string, tag: string | null): Promise<void> {
    return this.send(`/api/v1/accounts/${encodeURIComponent(accountId)}/tag`, 'PUT', { tag });
  }

  /// 改账户名称：只动资料与审计，不碰余额、标签与凭据。
  renameAccount(accountId: string, name: string): Promise<void> {
    return this.send(`/api/v1/accounts/${encodeURIComponent(accountId)}/name`, 'PUT', { name });
  }

  issueApiKey(accountId: string, label: string): Promise<IssueApiKeyResponse> {
    return this.send(`/api/v1/accounts/${encodeURIComponent(accountId)}/api-keys`, 'POST', { label });
  }

  providerFailures(kinds: string[] = [], limit = 50): Promise<ProviderFailuresResponse> {
    const query = new URLSearchParams();
    if (kinds.length > 0) query.set('kind', kinds.join(','));
    query.set('limit', String(limit));
    return this.get(`/api/v1/provider-failures?${query}`);
  }

  providerCostGaps(limit = 50): Promise<ProviderCostGapsResponse> {
    return this.get(`/api/v1/provider-cost-gaps?limit=${limit}`);
  }

  reconciliationCases(): Promise<ReconciliationCase[]> {
    return this.get('/api/v1/reconciliation-cases');
  }

  refundReconciliation(jobId: string, note: string, businessKey: string): Promise<void> {
    return this.send(`/api/v1/reconciliation-cases/${encodeURIComponent(jobId)}/refund`, 'POST', {
      note,
      business_key: businessKey,
    });
  }
}
