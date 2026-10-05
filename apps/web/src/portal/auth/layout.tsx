import { Card, Flex, Typography } from 'antd';
import type { ReactNode } from 'react';

/// 三个公开认证页面共用的窄内容区与标题层级（`docs/specs/0004` §2）。
///
/// 只负责品牌、页面标题、内容宽度与导航呈现；各页面拥有自己的表单、错误与提交状态。
export function AuthLayout({
  title,
  description,
  children,
}: {
  title: string;
  description?: string;
  children: ReactNode;
}) {
  return (
    <Flex
      align="center"
      justify="center"
      style={{ minHeight: '100vh', background: '#f5f5f5', padding: 24 }}
    >
      <Flex vertical gap={16} style={{ width: '100%', maxWidth: 460 }}>
        <Card styles={{ body: { padding: 32 } }}>
          <Flex vertical gap={4} style={{ marginBottom: 20 }}>
            <Typography.Text type="secondary" style={{ fontSize: 12 }}>
              seeai 控制台
            </Typography.Text>
            <Typography.Title data-testid="portal-auth-title" level={3} style={{ margin: 0 }}>
              {title}
            </Typography.Title>
            {description ? <Typography.Text type="secondary">{description}</Typography.Text> : null}
          </Flex>
          {children}
        </Card>
      </Flex>
    </Flex>
  );
}
