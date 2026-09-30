import { Button, Card, Result } from 'antd';

/// 客户侧 404：地址不在五页之内时显示它，不回落到概览或管理界面（`docs/design/0014` §4）。
export function NotFoundPage({ onBack }: { onBack: () => void }) {
  return (
    <Card>
      <div data-testid="portal-not-found">
        <Result
          status="404"
          title="找不到这个页面"
          subTitle="这个客户地址不存在。检查地址，或回到概览。"
          extra={
            <Button type="primary" onClick={onBack}>
              回概览
            </Button>
          }
        />
      </div>
    </Card>
  );
}
