/// 管理 API 的错误形状：平台所有失败响应都是 `{"error": {"code", "message"}}`。
export interface ApiErrorBody {
  error: { code: string; message: string };
}

export class ApiError extends Error {
  readonly status: number;
  readonly code: string;
  /// `Retry-After`（秒）；只有“等一下再来”这类失败有它。调用方用它告诉用户等多久。
  readonly retryAfterSeconds: number | null;

  constructor(
    status: number,
    code: string,
    message: string,
    retryAfterSeconds: number | null = null,
  ) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
    this.code = code;
    this.retryAfterSeconds = retryAfterSeconds;
  }
}

/// 管理员凭证由调用方注入（本控制台不持有登录态，见 `settings.ts`）。
export type AdminToken = () => string | null;

export interface RequestOptions {
  method?: string;
  body?: unknown;
  /// 缺省带管理员令牌；置 `false` 用于 `/health` 这类公开端点。
  admin?: boolean;
  /// 期望的**成功**状态；给了就要求实际状态与它逐位相符，其余 2xx（含 204 与 JSON 体）也当失败。
  expectedStatus?: number;
}

/// 调一次管理 API。
///
/// 这是**唯一**出网的地方：页面只描述"要点什么"，令牌、错误形状与 JSON 编解码都在这里收口。
/// 非 2xx 一律抛 [`ApiError`]，页面不必各自解析错误体；204 回 `undefined`。
///
/// 传 `expectedStatus` 时，成功状态必须与它逐位相符——重置兑换只认 204，其余 2xx 也按失败抛错，
/// 免得把“200 空体/JSON”当成确定成功。
///
/// **答复体不一定是 JSON**：平台自己的失败响应是 `{"error":{...}}`，但框架层抛出的错误（例如拒绝
/// 构造响应时的 `500 Failed to …`）是纯文本。硬按 JSON 解析会把那些错误吞成一句"不是合法 JSON"，
/// 运营看到的就只剩噪音——所以解析失败时回退到原文。
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

  const text = response.status === 204 ? '' : await response.text();
  const payload = parseJsonOrNull(text);
  if (!response.ok) {
    const failure = payload as ApiErrorBody | null;
    throw new ApiError(
      response.status,
      failure?.error?.code ?? 'unknown',
      messageFor(response.status, failure, text),
      retryAfterSeconds(response),
    );
  }
  // 给了期望状态就要求逐位相符：204 的“成功”与 200 空体的“成功”不是一回事（设计 0016 §3）。
  if (options.expectedStatus !== undefined && response.status !== options.expectedStatus) {
    throw new ApiError(
      response.status,
      'unexpected_status',
      `答复状态不是预期的 ${options.expectedStatus}（HTTP ${response.status}）`,
      retryAfterSeconds(response),
    );
  }
  if (response.status === 204) return undefined as T;
  if (text && payload === null) {
    // 2xx 却不是 JSON：调用方拿不到它要的形状，与其让它读到 `null` 之后在别处炸，不如在这里说清。
    throw new ApiError(response.status, 'invalid_body', `答复不是合法 JSON：${preview(text)}`);
  }
  return payload as T;
}

/// `Retry-After` 只认秒数（平台自己发的就是秒）；HTTP-date 或缺失都不回值。
function retryAfterSeconds(response: Response): number | null {
  const raw = response.headers.get('Retry-After');
  if (!raw) return null;
  const seconds = Number(raw.trim());
  return Number.isFinite(seconds) && seconds >= 0 ? Math.ceil(seconds) : null;
}

/// 解析 JSON；不是 JSON 就回 `null`（由调用方决定怎么处理，不在这里抛）。
function parseJsonOrNull(text: string): unknown {
  if (!text) return null;
  try {
    return JSON.parse(text);
  } catch {
    return null;
  }
}

/// 把一段文本截短，免得把整页 HTML 或长错误体铺到界面上。
function preview(text: string): string {
  const flat = text.replace(/\s+/g, ' ').trim();
  return flat.length > 200 ? `${flat.slice(0, 200)}…` : flat;
}

/// 把服务端的错误答复翻成**对界面用户可执行**的话。
///
/// 服务端的文案是给调用方（程序）看的英文，直接铺在界面上对人不友好；而登录/改口令这类端点又故意
/// 不区分"邮箱不存在"与"口令不对"，
/// 所以这里也只能给一句不区分的话。不认识的错误原样透出——编一句更含糊的会丢掉排查线索。
function messageFor(status: number, failure: ApiErrorBody | null, raw: string): string {
  // 403 在管理面只有一个含义：**这次凭据不被接受**（会话过期、被吊销，或拿的是共享令牌而这条端点
  // 只认会话）。对运营唯一能解决它的动作是重新登录。
  if (status === 403) {
    return '登录已过期或不被接受，请重新登录（退出登录后重新输入邮箱与口令）';
  }
  // 登录与改口令的"凭据不对"：服务端回 `invalid_parameter` 且文案是英文。
  if (status === 400 && failure?.error?.code === 'invalid_parameter') {
    return '邮箱或口令不对；新口令至少要 8 个字符';
  }
  if (failure?.error?.message) return failure.error.message;
  // 拿不到平台的错误体时**把原文带上**：那是唯一能指向故障的线索。
  return raw.trim() ? `请求失败（HTTP ${status}）：${preview(raw)}` : `请求失败（HTTP ${status}）`;
}
