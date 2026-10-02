import { useState } from 'react';

/// 列表页的最近一次查找条件：存在**当前标签页的会话状态**（`sessionStorage`）里，不进 URL。
///
/// 需求来自设计 `0011` §4.4.1：详情刷新、返回列表与浏览器前进后退时要恢复这次查找条件，而邮箱是
/// 客户资料，不该留在地址栏与浏览器历史里（Spec D2、D6）。它也不能只活在列表组件的 state 里——
/// 进详情页会把列表卸载掉。
///
/// 键名带 `console` 前缀，与客户控制台各存各的。退出登录或重新登录时由 [`clearScreenState`] 清掉：
/// 换个人用同一台机器，不该看到上一个人找过的邮箱。
const PREFIX = 'seeai.console.screen.';

export function readScreenState<T>(key: string, fallback: T): T {
  const raw = sessionStorage.getItem(`${PREFIX}${key}`);
  if (raw === null) return fallback;
  try {
    return JSON.parse(raw) as T;
  } catch {
    // 存进去的东西坏了就当没存过：一份筛选条件不值得让整页读不出来。
    return fallback;
  }
}

export function writeScreenState<T>(key: string, value: T): void {
  sessionStorage.setItem(`${PREFIX}${key}`, JSON.stringify(value));
}

/// 一份会跨页面存活的列表状态：初值来自会话，写入即落盘。
export function useScreenState<T>(key: string, fallback: T): [T, (value: T) => void] {
  const [value, setValue] = useState<T>(() => readScreenState(key, fallback));
  const update = (next: T) => {
    writeScreenState(key, next);
    setValue(next);
  };
  return [value, update];
}

/// 清掉本入口存下的全部列表状态。
export function clearScreenState(): void {
  // 倒着删：`sessionStorage` 的下标会随着删除前移，正着遍历会跳过一项。
  for (let index = sessionStorage.length - 1; index >= 0; index -= 1) {
    const key = sessionStorage.key(index);
    if (key?.startsWith(PREFIX)) sessionStorage.removeItem(key);
  }
}
