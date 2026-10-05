import type { ModelType, UsageAmounts } from './types';

/// 账单页按模型类型列出用量的顺序。
export const MODEL_TYPES: readonly ModelType[] = ['image', 'video', 'chat'];

const LABELS: Record<ModelType, string> = {
  image: '图片',
  video: '视频',
  chat: '对话',
};

/// 模型类型的中文名；目录里出现未知取值时原样回显，不猜。
export function modelTypeLabel(type: string): string {
  return LABELS[type as ModelType] ?? type;
}

/// 把一次执行（或一段区间）的按类型用量渲染成一行文字。
///
/// 图片给产出张数、视频给秒数、对话给输入与输出 token；该类型还没有量落点时整项为空，这里与
/// 缺值一样显示占位「—」而不是 0（模型类型 Spec 0006 §4.2/§4.3）。
export function usageText(type: string, usage: UsageAmounts | undefined): string {
  if (!usage) return '—';
  switch (type) {
    case 'image':
      return usage.images === undefined ? '—' : `${usage.images} 张`;
    case 'video':
      return usage.seconds === undefined ? '—' : `${usage.seconds} 秒`;
    case 'chat':
      return usage.input_tokens === undefined && usage.output_tokens === undefined
        ? '—'
        : `输入 ${usage.input_tokens ?? '—'} / 输出 ${usage.output_tokens ?? '—'} tokens`;
    default:
      return '—';
  }
}
