import { ApiError } from './api';

/// 账户名称的**客户端**预检：只为少一次注定被拒的往返。
///
/// 规则与服务端同一份合同（账户名称 Spec `0003` N2）：去掉首尾空白后 1–100 个 Unicode 字符，不含控制
/// 字符（`Cc`）与格式字符（`Cf`），`Cf` 只为 emoji 组合放行零宽连接符与零宽非连接符。判定权威仍在
/// 服务端——这里放行不代表一定成功，服务端拒了就按服务端的答复显示。
///
/// 返回 `null` 表示通过，否则是给客户/运营看的一句话。
export function accountNameProblem(raw: string): string | null {
  const trimmed = raw.trim();
  const length = [...trimmed].length;
  if (length === 0) return '账户名称不能为空';
  if (length > 100) return '账户名称最多 100 个字符';
  if (/[\p{Cc}\p{Cf}]/u.test(trimmed.replaceAll('\u200c', '').replaceAll('\u200d', ''))) {
    return '账户名称不能包含控制字符或格式字符';
  }
  return null;
}

/// 名称已被别的账户占用时给客户/运营看的一句话：服务端原文是内部口径，不直接呈现。
///
/// 认的是服务端的 `name_taken` 错误码，不是 `409`：同一个状态码也用于邮箱与账户绑定冲突，那两类要换的
/// 是别的参数，提示不能都说成"换一个名称"。返回 `null` 表示不是这一类失败，调用方按原样处理。
export function accountNameConflictHint(failure: unknown): string | null {
  return failure instanceof ApiError && failure.code === 'name_taken'
    ? '这个名称已被占用，请换一个'
    : null;
}
