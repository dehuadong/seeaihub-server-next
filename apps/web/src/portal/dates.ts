import { useEffect, useState } from 'react';
import { usePortalRoute } from './routes';

/// 客户历史页的时间区间：**客户选的是本地日期**，发给服务端的是 UTC 半开区间 `[since, until)`。
///
/// 两件事必须一起成立（设计 `0014` §1、§3）：
///
/// * 默认区间是含今天的最近 30 个本地自然日——从 29 天前的本地零点到**打开页面的当前时刻**；
/// * 区间一旦算出就**钉住**，翻页时原样复用，不重算“现在”（重算会让第二页与游标里的筛选面不符）。
///
/// 展示上始终标明本地时区：客户看到的日期与平台按 UTC 归属的区间不是一回事，不写时区就会读错一天。
export interface HistoryRange {
  /// ISO 8601（UTC），半开区间的下界（含）。
  since: string;
  /// ISO 8601（UTC），半开区间的上界（不含）。
  until: string;
}

/// 本地零点。
function localDayStart(year: number, month: number, day: number): Date {
  return new Date(year, month, day, 0, 0, 0, 0);
}

/// 含今天的最近 30 个本地自然日：29 天前的本地零点 → 现在。
export function defaultHistoryRange(now: Date = new Date()): HistoryRange {
  const from = localDayStart(now.getFullYear(), now.getMonth(), now.getDate() - 29);
  return { since: from.toISOString(), until: now.toISOString() };
}

/// 由两个**本地日期**（`YYYY-MM-DD`，含首含尾）落成半开区间：上界取结束日期的次日零点。
export function rangeFromLocalDates(from: string, to: string): HistoryRange {
  const [fromYear = 0, fromMonth = 1, fromDay = 1] = from.split('-').map(Number);
  const [toYear = 0, toMonth = 1, toDay = 1] = to.split('-').map(Number);
  const since = localDayStart(fromYear, fromMonth - 1, fromDay);
  const until = localDayStart(toYear, toMonth - 1, toDay + 1);
  return { since: since.toISOString(), until: until.toISOString() };
}

/// 把一个 UTC 时刻显示成**本地日期**（`YYYY-MM-DD`）。
export function localDateOf(instant: string): string {
  const date = new Date(instant);
  const month = `${date.getMonth() + 1}`.padStart(2, '0');
  const day = `${date.getDate()}`.padStart(2, '0');
  return `${date.getFullYear()}-${month}-${day}`;
}

/// 本地时区标签：名字加上相对 UTC 的偏移，例如 `Asia/Shanghai (UTC+08:00)`。
export function timeZoneLabel(now: Date = new Date()): string {
  const name = Intl.DateTimeFormat().resolvedOptions().timeZone;
  const offsetMinutes = -now.getTimezoneOffset();
  const sign = offsetMinutes < 0 ? '-' : '+';
  const hours = `${Math.floor(Math.abs(offsetMinutes) / 60)}`.padStart(2, '0');
  const minutes = `${Math.abs(offsetMinutes) % 60}`.padStart(2, '0');
  return `${name} (UTC${sign}${hours}:${minutes})`;
}

/// 区间的展示文案：起止都按本地日期写，并带时区。
export function rangeLabel(range: HistoryRange, now: Date = new Date()): string {
  return `${localDateOf(range.since)} ~ ${localDateOf(
    new Date(new Date(range.until).getTime() - 1).toISOString(),
  )}（${timeZoneLabel(now)}）`;
}

/// 从地址查询参数里读区间；缺一个或不成形时给 `null`，由调用方落回默认区间。
export function rangeFromSearch(search: string): HistoryRange | null {
  const params = new URLSearchParams(search);
  const since = params.get('since');
  const until = params.get('until');
  if (!since || !until) return null;
  const parsedSince = new Date(since);
  const parsedUntil = new Date(until);
  if (Number.isNaN(parsedSince.getTime()) || Number.isNaN(parsedUntil.getTime())) return null;
  return { since: parsedSince.toISOString(), until: parsedUntil.toISOString() };
}

/// 把区间写回地址查询参数：只放这两个非敏感时刻，不带凭据、密钥或游标（Spec D5）。
///
/// 类别筛选不进地址：它是页面上的一次筛选，不是"我在哪一页、看哪一段"。
export function searchWithRange(pathname: string, range: HistoryRange): string {
  const params = new URLSearchParams();
  params.set('since', range.since);
  params.set('until', range.until);
  return `${pathname}?${params}`;
}

/// 页面用的区间：**以地址为准**。
///
/// 直接打开或前进后退时按地址里的区间取数；地址里没有时算一次默认区间并**改写当前这条历史**
/// （`replace`，不新增），刷新与翻页因此都落在同一个区间上——翻页复用同一个 `since`/`until`，
/// 不重算“现在”（设计 `0014` §3）。
export function useHistoryRange(): [HistoryRange, (range: HistoryRange) => void] {
  const { pathname, search, navigate, replace } = usePortalRoute();
  const [range, setRange] = useState<HistoryRange>(
    () => rangeFromSearch(search) ?? defaultHistoryRange(),
  );

  // 缺省区间只补写一次（挂载时）：地址里已经有区间时不动它，否则会把客户选的区间覆盖掉。
  useEffect(() => {
    if (!rangeFromSearch(window.location.search)) {
      replace(searchWithRange(pathname, range));
    }
    // 只在挂载时补写：`range`、`pathname`、`replace` 随后变化都不该重跑，否则会把客户选的区间改回去。
  }, []);

  // 前进后退或直接打开带区间的地址：以地址为准。
  //
  // 值相同就**原样返回同一个对象**：补写缺省区间之后 `search` 也会变一次，若每次都换新对象，
  // 依赖区间的取数会被这份"其实没变"的区间再触发一次（同一页读两遍）。
  useEffect(() => {
    const fromUrl = rangeFromSearch(search);
    if (!fromUrl) return;
    setRange((current) =>
      current.since === fromUrl.since && current.until === fromUrl.until ? current : fromUrl,
    );
  }, [search]);

  const update = (next: HistoryRange) => {
    setRange(next);
    navigate(searchWithRange(pathname, next));
  };

  return [range, update];
}
