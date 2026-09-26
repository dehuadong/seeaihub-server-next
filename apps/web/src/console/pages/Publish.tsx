import { Alert, App as AntApp, Button, Descriptions, Form, Input, Typography } from 'antd';
import { SendOutlined } from '@ant-design/icons';
import { useState } from 'react';
import type { AdminClient } from '../client';
import { ConsolePage, Panel } from '../ui';

/// 发布修订：运营在这里"加一个网关模型"——贴一份发布命令，带上定价与倍率。
///
/// 素材就是发布命令本身（`config/bootstrap/*.json` 的形状），所以这一页不做表单化改写：
/// 合同与候选是**结构化数据**，把它拆成几十个输入框只会让人以为平台在替它做决定。这里只负责
/// 提交、把平台的原话（校验错误逐条）显示出来，并如实说明这一次发布做了什么、没做什么。
export function PublishPage({ client }: { client: AdminClient }) {
  const { message } = AntApp.useApp();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<{ gateway_model: string; runtime_revision_id: string } | null>(
    null,
  );
  const [form] = Form.useForm<{ command: string }>();

  async function submit(values: { command: string }) {
    setBusy(true);
    setError(null);
    setDone(null);
    try {
      const command: unknown = JSON.parse(values.command);
      const published = await client.publishRevision(command);
      setDone(published);
      form.resetFields();
      message.success(`已发布 ${published.gateway_model}`);
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setBusy(false);
    }
  }

  return (
    <ConsolePage title="发布修订" hint="发布即原子替换该型号的全部候选" error={error}>
      <Panel title="发布命令的字段">
        <Typography.Paragraph type="secondary" style={{ marginBottom: 8 }}>
          <code>vendor_id</code>、<code>native_model_id</code>、<code>gateway_model</code>
          （平台对客名，省略时取原生名）、<code>native_revision</code>（改价必须换修订号）、
          <code>capability_schema</code>（合同）、<code>offerings</code>（候选数组）、
          <code>markup_bps</code>（加价系数，基点）、<code>actor</code>。
        </Typography.Paragraph>
        <Typography.Paragraph type="secondary" style={{ marginBottom: 0 }}>
          按候选的钱是 <code>consumer_rates_cny</code>（四档 CNY 费率向量）、
          <code>cost_unit_price_microusd</code>（按张/按次的成本单价）、
          <code>reference_cost_microusd</code>（定价参考）、<code>floor_amounts</code>（保底表）。
          渠道币种的折算率要先录（见「折算率」页），否则发布期会以"没有生效折算率"拒绝。
        </Typography.Paragraph>
      </Panel>

      <Panel title="发布素材">
        <Form form={form} layout="vertical" onFinish={submit}>
          <Form.Item
            name="command"
            rules={[
              { required: true, message: '请贴上发布命令的 JSON' },
              {
                validator: (_rule, value: string) => {
                  if (!value || !value.trim()) return Promise.resolve();
                  try {
                    JSON.parse(value);
                    return Promise.resolve();
                  } catch (failure) {
                    return Promise.reject(
                      new Error(`不是合法 JSON：${failure instanceof Error ? failure.message : failure}`),
                    );
                  }
                },
              },
            ]}
          >
            <Input.TextArea
              rows={16}
              style={{ fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace' }}
              placeholder='{"vendor_id":"OpenAI","native_model_id":"gpt-image-2.5-flare","native_revision":"...","markup_bps":2000,"actor":"ops","offerings":[...]}'
            />
          </Form.Item>
          <Form.Item style={{ marginBottom: 0 }}>
            <Button type="primary" icon={<SendOutlined />} htmlType="submit" loading={busy}>
              发布
            </Button>
            <Typography.Text type="secondary" style={{ marginInlineStart: 12 }}>
              提交前请先确认合同里的 <code>model.const</code> 等于 <code>native_model_id</code>
              （发布期会校验）。
            </Typography.Text>
          </Form.Item>
        </Form>
      </Panel>

      {done ? (
        <Panel title="已发布">
          <Descriptions column={1} size="small" bordered>
            <Descriptions.Item label="网关模型">{done.gateway_model}</Descriptions.Item>
            <Descriptions.Item label="生效修订">
              <Typography.Text code copyable>
                {done.runtime_revision_id}
              </Typography.Text>
            </Descriptions.Item>
          </Descriptions>
          <Alert
            style={{ marginTop: 12 }}
            type="info"
            showIcon
            message="去「网关模型」页核对候选与定价是否就是你要的那一份。"
          />
        </Panel>
      ) : null}
    </ConsolePage>
  );
}
