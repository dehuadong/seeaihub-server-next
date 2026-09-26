import { useEffect, useState, type ReactNode } from 'react';

/// 一处最小的"取一次数据"封装：加载中 / 出错 / 成功三态，外加重取。
///
/// 页面只描述取什么，不需要各自写 `useEffect` + 三态样板；错误用 [`ApiError`] 的文案直接展示
/// （平台的对客与管理端错误体都是 `{error:{code,message}}`，已经在这里解析过）。
export interface Loadable<T> {
  data: T | null;
  error: string | null;
  loading: boolean;
  reload: () => void;
  setData: (value: T | null) => void;
}

export function useLoadable<T>(load: () => Promise<T>, deps: unknown[]): Loadable<T> {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [tick, setTick] = useState(0);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    load()
      .then((value) => {
        if (!cancelled) setData(value);
      })
      .catch((failure: unknown) => {
        if (!cancelled) setError(failure instanceof Error ? failure.message : String(failure));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [...deps, tick]);

  return { data, error, loading, reload: () => setTick((value) => value + 1), setData };
}

/// 一页的骨架：标题 + 右上角的重取按钮 + 三态内容。
export function Page(props: {
  title: string;
  hint?: ReactNode;
  error?: string | null;
  loading?: boolean;
  onReload?: () => void;
  children: ReactNode;
}) {
  return (
    <section className="main">
      <header>
        <h2>{props.title}</h2>
        <div className="row">
          {props.hint ? <span className="hint">{props.hint}</span> : null}
          {props.onReload ? (
            <button type="button" onClick={props.onReload} disabled={props.loading}>
              重取
            </button>
          ) : null}
        </div>
      </header>
      {props.error ? <p className="error">{props.error}</p> : null}
      {props.loading ? <p className="muted">读取中…</p> : null}
      {props.children}
    </section>
  );
}
