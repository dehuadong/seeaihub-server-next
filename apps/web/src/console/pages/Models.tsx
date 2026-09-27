import {
  Alert,
  App as AntApp,
  Button,
  Descriptions,
  Drawer,
  Flex,
  Popconfirm,
  Switch,
  Table,
  Tag,
  Typography,
} from 'antd';
import { DownOutlined, PlusOutlined, RightOutlined } from '@ant-design/icons';
import { useEffect, useState } from 'react';
import type { AdminClient } from '../client';
import type { GatewayModel, GatewayModelCandidate } from '../../shared/types';
import { useLoadable } from '../../shared/ui';
import { ConsolePage, Panel, whenText, yuanText } from '../ui';
import { PublishPanel } from './Publish';

/// 模型目录：在售的模型与它们的价目，运营最常看的一页。
///
/// 它回答"这个平台型号现在是什么状态"：生效修订、加价系数、候选顺序与权重、每条候选能不能走，
/// 以及**这条候选承载得了哪些字段**——"目录里为什么没有它"要在这里看得见。
///
/// **它同时是"看"和"写"的入口**：右上角"上架新模型"与每行的"改价"都把 [`PublishPanel`] 开在抽屉里。
/// 一页看、一页写会让人看不出两者的联系（用户原话："和网关模型的区别是什么"），所以合并在这里。
///
/// `editRequest` 是容器（`App`）给的"要改哪个型号"：给 `null` 时抽屉里是新增表单，给型号名时先载入它
/// 再进改价模式。`#/publish` 这个旧地址仍然可用，它落到本页并把要改的型号带进来。
export function ModelsPage({
  client,
  editRequest,
  onEditRequestHandled,
}: {
  client: AdminClient;
  editRequest?: string | null;
  onEditRequestHandled?: () => void;
}) {
  const { message } = AntApp.useApp();
  const models = useLoadable(() => client.gatewayModels(), [client]);
  const [busy, setBusy] = useState<string | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const [open, setOpen] = useState<string | null>(null);
  /// 抽屉里的表单：`null` 表示抽屉关着；`{ editing: null }` 表示新增；`{ editing: '名' }` 表示改价。
  const [form, setForm] = useState<{ editing: string | null } | null>(null);

  // 容器把"要改这个型号"传进来：开抽屉并交给表单去载入。处理完就清掉，避免下次进本页又自动打开。
  useEffect(() => {
    if (editRequest === undefined) return;
    setForm({ editing: editRequest });
    onEditRequestHandled?.();
  }, [editRequest, onEditRequestHandled]);

  async function toggleModel(model: GatewayModel) {
    setBusy(model.gateway_model);
    setFailure(null);
    try {
      await client.setGatewayModelEnabled(model.gateway_model, !model.enabled);
      message.success(model.enabled ? '已停用' : '已启用');
      models.reload();
    } catch (error) {
      setFailure(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(null);
    }
  }

  const list = models.data?.gateway_models ?? [];

  return (
    <ConsolePage
      title="模型目录"
      hint="在售的模型与它们的价目。上架新模型、改价，都在这一页完成。"
      error={failure ?? models.error}
      loading={models.loading}
      onReload={models.reload}
      extra={
        <Button
          type="primary"
          icon={<PlusOutlined />}
          data-testid="models-add"
          onClick={() => setForm({ editing: null })}
        >
          上架新模型
        </Button>
      }
    >
      {list.length === 0 && !models.loading ? (
        <Panel title="还没有上架过任何模型">
          <Alert
            type="info"
            showIcon
            message="空库时这里什么都没有"
            description={
              <>
                点右上角「上架新模型」，或贴一份发布素材（<code>config/bootstrap/*.json</code>）。
                上架之后这一页会列出每个型号的生效修订、加价系数与候选。
              </>
            }
          />
        </Panel>
      ) : null}

      {list.map((model) => {
        const expanded = open === model.gateway_model;
        return (
          <Panel
            key={model.gateway_model}
            title={
              <Flex align="center" gap={8}>
                <Typography.Text strong style={{ fontSize: 16 }}>
                  {model.gateway_model}
                </Typography.Text>
                <Tag color={model.enabled ? 'success' : 'default'}>
                  {model.enabled ? '启用' : '停用'}
                </Tag>
              </Flex>
            }
            extra={
              <Flex gap={8}>
                <Button
                  data-testid={`models-reprice-${model.gateway_model}`}
                  onClick={() => setForm({ editing: model.gateway_model })}
                >
                  改价
                </Button>
                <Button
                  icon={expanded ? <DownOutlined /> : <RightOutlined />}
                  onClick={() => setOpen(expanded ? null : model.gateway_model)}
                >
                  候选（{model.candidates.length}）
                </Button>
                <Popconfirm
                  title={model.enabled ? '停用这个模型？' : '启用这个模型？'}
                  description={
                    model.enabled
                      ? '停用后对客目录里不再出现它；已受理的 Job 不受影响。'
                      : '启用后它重新出现在对客目录里。'
                  }
                  okText="确认"
                  cancelText="取消"
                  onConfirm={() => toggleModel(model)}
                >
                  <Button danger={model.enabled} loading={busy === model.gateway_model}>
                    {model.enabled ? '停用' : '启用'}
                  </Button>
                </Popconfirm>
              </Flex>
            }
          >
            <Descriptions column={{ xs: 1, sm: 2, lg: 3 }} size="small" bordered>
              <Descriptions.Item label="厂商">
                {model.vendor_id} / {model.native_model_id}
              </Descriptions.Item>
              <Descriptions.Item label="合同修订">{model.native_revision}</Descriptions.Item>
              <Descriptions.Item label="加价系数">
                {model.markup_bps === null
                  ? '未声明'
                  : `${model.markup_bps} 基点（×${(1 + model.markup_bps / 10000).toFixed(4)}）`}
              </Descriptions.Item>
              <Descriptions.Item label="生效修订">
                <Typography.Text code copyable>
                  {model.runtime_revision_id}
                </Typography.Text>
              </Descriptions.Item>
              <Descriptions.Item label="发布时间">
                {whenText(model.published_at)}
              </Descriptions.Item>
            </Descriptions>

            {expanded ? (
              <Table
                style={{ marginTop: 12 }}
                size="small"
                rowKey="offering_id"
                pagination={false}
                scroll={{ x: 'max-content' }}
                dataSource={model.candidates}
                columns={[
                  { title: '档位', dataIndex: 'routing_priority', align: 'right', width: 70 },
                  { title: '权重', dataIndex: 'weight', align: 'right', width: 70 },
                  { title: '渠道', dataIndex: 'provider_kind', width: 110 },
                  {
                    title: '渠道模型',
                    dataIndex: 'provider_model_id',
                    render: (value: string) => <Typography.Text code>{value}</Typography.Text>,
                  },
                  {
                    title: '驱动器',
                    dataIndex: 'adapter_key',
                    render: (value: string) => <Typography.Text code>{value}</Typography.Text>,
                  },
                  {
                    title: '成本（原币种）',
                    dataIndex: 'reference_cost_microusd',
                    render: (_value: number | null, candidate) =>
                      candidate.reference_cost_microusd === null ? (
                        '—'
                      ) : (
                        <Flex vertical>
                          <span>
                            {yuanText(candidate.reference_cost_microusd)}{' '}
                            {candidate.cost_currency ?? ''}
                          </span>
                          <Typography.Text type="secondary" style={{ fontSize: 12 }}>
                            {candidate.cost_basis ?? '—'}
                          </Typography.Text>
                        </Flex>
                      ),
                  },
                  {
                    title: '对客费率（CNY/1M）',
                    dataIndex: 'consumer_rates_cny',
                    render: (_value: unknown, candidate) => <RatesCell candidate={candidate} />,
                  },
                  {
                    title: '可走',
                    dataIndex: 'enabled',
                    width: 90,
                    render: (enabled: boolean) => (
                      <Tag color={enabled ? 'success' : 'default'}>{enabled ? '可走' : '不可走'}</Tag>
                    ),
                  },
                  {
                    title: '',
                    width: 90,
                    render: (_value: unknown, candidate) => (
                      <CandidateSwitch
                        client={client}
                        candidate={candidate}
                        onChanged={models.reload}
                        onError={setFailure}
                      />
                    ),
                  },
                ]}
              />
            ) : null}
          </Panel>
        );
      })}

      <Drawer
        title={form?.editing ? `改价：${form.editing}` : '上架新模型'}
        width="min(960px, 100vw)"
        open={form !== null}
        onClose={() => setForm(null)}
        destroyOnHidden
        styles={{ body: { background: '#f5f5f5' } }}
      >
        {form ? (
          // `key` 让换型号（或从改价切到新增）时表单重新挂载：不清空就会把上一个型号的字段带过来。
          <PublishPanel
            key={form.editing ?? '__new__'}
            client={client}
            editing={form.editing}
            onPublished={models.reload}
          />
        ) : null}
      </Drawer>
    </ConsolePage>
  );
}

/// 对客费率的四种计量分项。没有向量的候选走"成本单价 × 倍率"，这里如实说明，不留空。
function RatesCell({ candidate }: { candidate: GatewayModelCandidate }) {
  const rates = candidate.consumer_rates_cny;
  if (!rates) {
    return (
      <Typography.Text type="secondary">无向量（按成本单价 × 倍率算）</Typography.Text>
    );
  }
  return (
    <Flex vertical style={{ fontSize: 12 }}>
      <span>文入 {yuanText(rates.text_input_micros_per_million)}</span>
      <span>图入 {yuanText(rates.image_input_micros_per_million)}</span>
      <span>文出 {yuanText(rates.text_output_micros_per_million)}</span>
      <span>图出 {yuanText(rates.image_output_micros_per_million)}</span>
    </Flex>
  );
}

function CandidateSwitch(props: {
  client: AdminClient;
  candidate: GatewayModelCandidate;
  onChanged: () => void;
  onError: (message: string) => void;
}) {
  const { message } = AntApp.useApp();
  const [busy, setBusy] = useState(false);

  return (
    <Switch
      size="small"
      checked={props.candidate.enabled}
      loading={busy}
      checkedChildren="可走"
      unCheckedChildren="停用"
      onChange={async (next) => {
        setBusy(true);
        try {
          await props.client.setOfferingEnabled(props.candidate.offering_id, next);
          message.success(next ? '该候选已启用' : '该候选已停用');
          props.onChanged();
        } catch (error) {
          props.onError(error instanceof Error ? error.message : String(error));
        } finally {
          setBusy(false);
        }
      }}
    />
  );
}
