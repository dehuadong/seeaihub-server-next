/// 管理 API 的错误形状：平台所有失败响应都是 `{"error": {"code", "message"}}`。
export interface ApiErrorBody {
  error: { code: string; message: string };
}

export class ApiError extends Error {
  readonly status: number;
  readonly code: string;

  constructor(status: number, code: string, message: string) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
    this.code = code;
  }
}

/// 管理员凭证由调用方注入（本控制台不持有登录态，见 `settings.ts`）。
export type AdminToken = () => string | null;

export interface RequestOptions {
  method?: string;
  body?: unknown;
  /// 缺省带管理员令牌；置 `false` 用于 `/health` 这类公开端点。
  admin?: boolean;
}

/// 调一次管理 API。
///
/// 这是**唯一**出网的地方：页面只描述"要点什么"，令牌、错误形状与 JSON 编解码都在这里收口。
/// 非 2xx 一律抛 [`ApiError`]，页面不必各自解析错误体；204 回 `undefined`。
export async function apiFetch<T>(
  path: string,
  token: AdminToken,
  options: RequestOptions = {},
): Promise<T> {
  const headers: Record<string, string> = {};
  if (options.admin !== false) {
    const value = token();
    if (!value) throw new ApiError(0, 'no_token', '还没有填管理员令牌');
    headers['Authorization'] = `Bearer ${value}`;
  }
  if (options.body !== undefined) headers['Content-Type'] = 'application/json';

  const response = await fetch(path, {
    method: options.method ?? (options.body === undefined ? 'GET' : 'POST'),
    headers,
    body: options.body === undefined ? undefined : JSON.stringify(options.body),
  });

  if (response.status === 204) return undefined as T;
  const text = await response.text();
  const payload: unknown = text ? JSON.parse(text) : null;
  if (!response.ok) {
    const failure = payload as ApiErrorBody | null;
    // 403 在管理面只有一个含义：**这次凭据不被接受**（会话过期、被吊销，或拿的是共享令牌而这条
    // 端点只认会话）。原文案是英文的 `admin authorization failed`，对运营没有可操作性——换成
    // "重新登录"，因为那是唯一能解决它的动作。
    const message =
      response.status === 403
        ? '登录已过期或不被接受，请重新登录（退出登录后重新输入邮箱与口令）'
        : (failure?.error?.message ?? `请求失败（HTTP ${response.status}）`);
    throw new ApiError(response.status, failure?.error?.code ?? 'unknown', message);
  }
  return payload as T;
}
