import { useEffect, useState } from 'react';
import { ApiError } from './api';

/// 一处最小的"取一次数据"封装：加载中 / 出错 / 成功三态，外加重取。
///
/// 页面只描述取什么，不需要各自写 `useEffect` + 三态样板；错误用 [`ApiError`] 的文案直接展示
/// （平台的对客与管理端错误体都是 `{error:{code,message}}`，已经在这里解析过）。
///
/// **这是共享层唯一剩下的界面相关的东西**，而且它只是状态机，不渲染任何东西——所以两个入口引它都不会
/// 把对方的组件库带进自己的产物。页面的外观由各入口自己的 Ant Design 组件决定（管理端在
/// `console/ui.tsx`，客户端在 `portal/pages/` 各自的页面里）。
export interface Loadable<T> {
  data: T | null;
  error: string | null;
  /// 失败的 HTTP 状态码；不是平台答复（断网、解析失败）时为 `null`。
  ///
  /// 页面要能把"这个对象不存在"（404，或地址里的标识不成形时的 400）与其他失败分开：前者显示
  /// 找不到并清掉上一个对象的数据，后者显示错误并让人重取。
  status: number | null;
  loading: boolean;
  reload: () => void;
  setData: (value: T | null) => void;
}

export function useLoadable<T>(load: () => Promise<T>, deps: unknown[]): Loadable<T> {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<number | null>(null);
  const [loading, setLoading] = useState(true);
  const [tick, setTick] = useState(0);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    setStatus(null);
    load()
      .then((value) => {
        if (!cancelled) setData(value);
      })
      .catch((failure: unknown) => {
        if (cancelled) return;
        setError(failure instanceof Error ? failure.message : String(failure));
        setStatus(failure instanceof ApiError ? failure.status : null);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [...deps, tick]);

  return { data, error, status, loading, reload: () => setTick((value) => value + 1), setData };
}
