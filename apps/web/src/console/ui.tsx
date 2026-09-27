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

// 金额与时间的展示规则在共享层（`shared/format.ts`）：两个入口都要用同一套，而它不含 antd。
export { whenText, yuan, yuanText } from '../shared/format';
