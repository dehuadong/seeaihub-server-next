import {
  Alert,
  App as AntApp,
  Button,
  Card,
  Flex,
  Form,
  Input,
  Popconfirm,
  Table,
  Tag,
  Typography,
} from 'antd';
import { PlusOutlined } from '@ant-design/icons';
import { useState } from 'react';
import type { CustomerClient } from '../client';
import { whenText } from '../../shared/format';
import { useLoadable } from '../../shared/ui';

/// API Key：列、建、吊销。明文只在创建那一次出现。
///
/// 只请求本页需要的 `/v1/customer/api-keys`；余额、账单与用量不在这一页（Spec C15）。
export function KeysPage({ client }: { client: CustomerClient }) {
  const { message } = AntApp.useApp();
  const keys = useLoadable(() => client.apiKeys(), [client]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [issued, setIssued] = useState<{ api_key: string; key_id: string } | null>(null);
  const [form] = Form.useForm<{ label: string }>();

  /// 吊销：**确认之后才发请求**，成功后重取列表（Spec C5、V-D14）。
  async function revoke(keyId: string): Promise<void> {
    setError(null);
    try {
      await client.revokeApiKey(keyId);
      message.success('已吊销');
      keys.reload();
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    }
  }

  return (
    <Card
      title="API Key"
      extra={
        <Button data-testid="portal-keys-reload" onClick={keys.reload}>
          重取
        </Button>
      }
    >
      <Typography.Paragraph type="secondary" style={{ marginTop: 0 }}>
        明文只在创建那一次出现，之后谁也拿不回来。吊销立刻生效（不删行）。
      </Typography.Paragraph>
      {error ? <Alert type="error" showIcon message={error} /> : null}
      {keys.error ? <Alert type="error" showIcon message={keys.error} /> : null}
      <Form
        form={form}
        layout="inline"
        onFinish={async (values) => {
          setBusy(true);
          setError(null);
          try {
            const created = await client.issueApiKey(values.label.trim());
            setIssued(created);
            form.resetFields();
            keys.reload();
            message.success('密钥已签发');
          } catch (failure) {
            setError(failure instanceof Error ? failure.message : String(failure));
          } finally {
            setBusy(false);
          }
        }}
      >
        <Form.Item
          name="label"
          label="标签（给自己认的，例如「本地脚本」）"
          rules={[{ required: true, message: '请给密钥起个标签' }]}
        >
          <Input data-testid="portal-key-label" style={{ width: 240 }} />
        </Form.Item>
        <Form.Item>
          <Button
            data-testid="portal-key-create"
            type="primary"
            icon={<PlusOutlined />}
            htmlType="submit"
            loading={busy}
          >
            新建密钥
          </Button>
        </Form.Item>
      </Form>

      {issued ? (
        <Alert
          style={{ marginTop: 16 }}
          type="warning"
          showIcon
          message="密钥明文——只显示这一次，现在就抄走"
          description={
            <Flex vertical gap={8}>
              <Typography.Text
                data-testid="portal-key-plaintext"
                code
                copyable
                style={{ fontSize: 14 }}
              >
                {issued.api_key}
              </Typography.Text>
              <Typography.Text type="secondary">
                密钥标识：
                <Typography.Text code copyable>
                  {issued.key_id}
                </Typography.Text>
              </Typography.Text>
              {/* 关掉一次性展示就把明文从页面状态里清掉：它不该在页面上多留一秒（Spec C5）。 */}
              <Button
                size="small"
                data-testid="portal-key-plaintext-close"
                onClick={() => setIssued(null)}
              >
                我已抄走，关闭明文
              </Button>
            </Flex>
          }
        />
      ) : null}

      <Table
        style={{ marginTop: 16 }}
        size="small"
        rowKey="key_id"
        loading={keys.loading}
        pagination={false}
        dataSource={keys.data?.keys ?? []}
        locale={{
          emptyText: <Alert type="info" showIcon message="还没有密钥。建一把之后才能调用生成接口。" />,
        }}
        columns={[
          { title: '标签', dataIndex: 'label' },
          { title: '创建时间', dataIndex: 'created_at', render: (value: string) => whenText(value) },
          {
            title: '状态',
            dataIndex: 'revoked_at',
            render: (value: string | null) =>
              value ? <Tag>已吊销（{whenText(value)}）</Tag> : <Tag color="green">可用</Tag>,
          },
          {
            title: '',
            width: 100,
            render: (_value: unknown, key: { key_id: string; label: string; revoked_at: string | null }) => {
              if (key.revoked_at) return null;
              return (
                <Popconfirm
                  title="吊销这把密钥？"
                  description={`吊销后用它调用会被拒，不能撤销。标签：${key.label}`}
                  okText="吊销"
                  cancelText="取消"
                  okButtonProps={{ 'data-testid': 'portal-key-revoke-confirm' }}
                  onConfirm={() => void revoke(key.key_id)}
                >
                  <Button danger size="small" data-testid="portal-key-revoke">
                    吊销
                  </Button>
                </Popconfirm>
              );
            },
          },
        ]}
      />
    </Card>
  );
}
