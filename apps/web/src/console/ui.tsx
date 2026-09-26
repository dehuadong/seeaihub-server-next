import { Alert, Button, Card, Flex, Skeleton, Space, Typography } from 'antd';
import type { ReactNode } from 'react';
import { ReloadOutlined } from '@ant-design/icons';

/// 管理端一页的骨架：标题 + 说明 + 右上角重取 + 三态内容。
///
/// 放在 `console/` 而不是共享层：它用的是 Ant Design，而客户控制台不该把 antd 打进自己的产物里
/// （`docs/specs/0001-admin-and-customer-consoles.md` §5 的 V-D6：客户入口产物里不含管理界面的代码）。
/// 共享层只留与渲染无关的取数封装。
export function ConsolePage(props: {
  title: string;
  hint?: ReactNode;
  error?: string | null;
  loading?: boolean;
  onReload?: () => void;
  extra?: ReactNode;
  children: ReactNode;
}) {
  return (
    <Flex vertical gap={16}>
      <Flex align="flex-start" justify="space-between" gap={16} wrap>
        <div>
          <Typography.Title level={3} style={{ margin: 0 }}>
            {props.title}
          </Typography.Title>
          {props.hint ? (
            <Typography.Text type="secondary">{props.hint}</Typography.Text>
          ) : null}
        </div>
        <Space>
          {props.extra}
          {props.onReload ? (
            <Button icon={<ReloadOutlined />} disabled={props.loading} onClick={props.onReload}>
              重取
            </Button>
          ) : null}
        </Space>
      </Flex>
      {props.error ? <Alert type="error" showIcon message={props.error} /> : null}
      {props.loading ? <Skeleton active paragraph={{ rows: 3 }} /> : null}
      {props.children}
    </Flex>
  );
}

/// 一块内容面板。管理端几乎每个页面都是"若干块表格 + 若干条操作"，统一用这个包。
export function Panel(props: {
  title: ReactNode;
  description?: ReactNode;
  extra?: ReactNode;
  children: ReactNode;
}) {
  return (
    <Card
      title={props.title}
      extra={props.extra}
      styles={{ body: { paddingTop: 16 } }}
    >
      {props.description ? (
        <Typography.Paragraph type="secondary" style={{ marginTop: 0 }}>
          {props.description}
        </Typography.Paragraph>
      ) : null}
      {props.children}
    </Card>
  );
}

/// 表里"没有数据"的统一空态文案：区分"还没做过"与"做过了但没数据"，避免只显示一个空表格。
export function EmptyHint({ children }: { children: ReactNode }) {
  return (
    <Typography.Text type="secondary" italic>
      {children}
    </Typography.Text>
  );
}

/// 微单位人民币 → 展示用的元。**只在展示层换算**：传输与判断都保持整数微单位。
///
/// 与 `shared/routes.ts` 的 `yuan` 是同一条规则；这里再导出一次是为了让管理端页面不必从共享层
/// 拉那个同时还带着 `useHashRoute` 的模块（那样会把路由钩子也带进每一页的依赖图）。
export function yuanText(micros: number): string {
  return `${(micros / 1_000_000).toFixed(6).replace(/0+$/, '').replace(/\.$/, '')} 元`;
}

/// 时间戳按本地时区展示；空值原样显示为 `—`，不猜。
export function whenText(value: string | null | undefined): string {
  if (!value) return '—';
  const at = new Date(value);
  return Number.isNaN(at.getTime()) ? value : at.toLocaleString();
}
