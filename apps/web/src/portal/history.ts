import { useState } from 'react';

/// 一段“第一页 + 继续查看”的翻页状态。
///
/// **以第一页为准**：续页只挂在它当时那一份第一页上。重取第一页（刷新、换区间、换类别）就换了一个
/// 对象，续页与上一次的错误随之作废——否则刷新之后会把刷新前的第二页起继续拼在新第一页后面，出现重复
/// 或串页。游标只留在页面状态里，不进地址（设计 `0014` §3）。
export interface CursorPage<Row> {
  /// 第一页之后的续页。
  rows: Row[];
  /// 下一页的游标；没有更多时为 `null`。
  cursor: string | null;
  loading: boolean;
  error: string | null;
  loadMore: () => void;
}

/// `firstPage` 是第一页的响应（`null` 表示还没读到），`fetchMore` 用游标取下一页。
export function useCursorPage<Row, Page extends { next_cursor: string | null }>(
  firstPage: Page | null,
  fetchMore: (cursor: string) => Promise<{ rows: Row[]; cursor: string | null }>,
): CursorPage<Row> {
  const [state, setState] = useState<{
    page: Page;
    rows: Row[];
    cursor: string | null;
    error: string | null;
  } | null>(null);
  const [loading, setLoading] = useState(false);
  const continued = state && firstPage !== null && state.page === firstPage ? state : null;
  const cursor = continued ? continued.cursor : (firstPage?.next_cursor ?? null);

  const loadMore = () => {
    if (!cursor) return;
    setLoading(true);
    void fetchMore(cursor)
      .then((page) =>
        setState({
          page: firstPage as Page,
          rows: [...(continued?.rows ?? []), ...page.rows],
          cursor: page.cursor,
          error: null,
        }),
      )
      .catch((failure: unknown) =>
        setState({
          page: firstPage as Page,
          rows: continued?.rows ?? [],
          cursor: continued?.cursor ?? firstPage?.next_cursor ?? null,
          error: failure instanceof Error ? failure.message : String(failure),
        }),
      )
      .finally(() => setLoading(false));
  };

  return {
    rows: continued?.rows ?? [],
    cursor,
    loading,
    error: continued?.error ?? null,
    loadMore,
  };
}
