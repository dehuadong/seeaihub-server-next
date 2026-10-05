import { ApiError } from '../shared/api';

/// 客户面的认证措辞与错误映射（`docs/specs/0004` §1、§4；`docs/design/0016` §4）。
///
/// 共享传输里的“口令”文案继续供管理端使用，客户面在这里自己映射成“密码”，不靠改共享常量达成统一；
/// 429 一律显示可读的等待提示，不归入结果未知、也不算成功。
export function waitingMessage(failure: ApiError): string {
  const seconds = failure.retryAfterSeconds;
  return seconds && seconds > 0
    ? `尝试过于频繁，请稍后重试。（约 ${seconds} 秒后可再试）`
    : '尝试过于频繁，请稍后重试。';
}

export function loginErrorMessage(mode: 'login' | 'register', failure: unknown): string {
  if (failure instanceof ApiError) {
    if (failure.status === 429) return waitingMessage(failure);
    if (mode === 'register' && failure.status === 409) {
      return '这个邮箱已经注册过了，请换一个邮箱或直接登录。';
    }
    if (failure.status === 400) {
      return mode === 'register'
        ? '邮箱或密码不符合要求：密码至少 8 个字符。'
        : '邮箱或密码不正确';
    }
    if (failure.status >= 500) return '服务器暂时不可用，请稍后重试。';
  }
  return '暂时无法连接服务器，请检查网络后重试。';
}

/// 账户设置页改密码：只给客户能理解、且不带“口令”或邮箱误导的措辞。
export function passwordErrorMessage(failure: unknown): string {
  if (failure instanceof ApiError) {
    if (failure.status === 429) return waitingMessage(failure);
    if (failure.status === 400) return '当前密码不正确，或新密码不符合要求（至少 8 个字符）。';
    if (failure.status >= 500) return '服务器暂时不可用，请稍后重试。';
  }
  return '暂时无法连接服务器，请检查网络后重试。';
}
