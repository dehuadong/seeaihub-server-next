/// 展示层的换算与格式化。**两个入口共用**，所以放在这里而不是某个入口下面。
///
/// 它们不含任何 UI 库依赖（只有 `Intl` 与算术），因此客户控制台引它不会把 Ant Design 带进自己的产物
/// ——`docs/specs/0001-admin-and-customer-consoles.md` §5 的 V-D6 要求客户入口产物里不含管理界面的代码，
/// 构建末尾的隔离核对照这条把关。

/// 微单位人民币 → 展示用的元。**只在展示层换算**：传输与判断都保持整数微单位。
///
/// 去掉末尾多余的零：`12340000` → `12.34`，而不是 `12.340000`。
export function yuan(micros: number): string {
  return (micros / 1_000_000).toFixed(6).replace(/0+$/, '').replace(/\.$/, '');
}

/// 同上，但带上单位。界面上的金额列几乎都要它。
export function yuanText(micros: number): string {
  return `${yuan(micros)} 元`;
}

/// 时间戳按本地时区展示；空值原样显示为 `—`，不猜。
export function when(value: string | null | undefined): string {
  if (!value) return '—';
  const at = new Date(value);
  return Number.isNaN(at.getTime()) ? value : at.toLocaleString();
}

/// `when` 的别名，语义上更明确一点：界面里读作"什么时候"。
export const whenText = when;
