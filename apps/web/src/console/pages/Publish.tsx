import {
  Alert,
  App as AntApp,
  Button,
  Card,
  Col,
  Collapse,
  Descriptions,
  Divider,
  Flex,
  Form,
  Input,
  Row,
  Select,
  Space,
  Typography,
} from 'antd';
import { DeleteOutlined, PlusOutlined, SendOutlined } from '@ant-design/icons';
import { useEffect, useRef, useState } from 'react';
import type { AdminClient } from '../client';
import { useLoadable } from '../../shared/ui';
import { Panel } from '../ui';

/// 上架与改价：运营让一个型号可售、或改它的价的地方（写的就是发布修订这件事）。
///
/// **表单收集商业条款，技术字段贴入**。发布命令里绝大多数内容是运营不做决定的样板：
/// `capability_schema` 与 `carrier_schema` 是厂商给的 JSON Schema，`parameter_mapping` 是渠道包装声明；
/// 运营真正决定的是几个数——型号、倍率、候选的供应商与成本、对客费率。
///
/// 为什么不做成"只改倍率就能发"：发布命令的 `offerings` 要求**完整、有序**的候选集合（每次给全），
/// 而其中的 `base_url`、`credential_env` 是渠道部署事实、管理端视图不回显——前端拼不出完整命令。
/// 界面上留了"从已发布型号载入"把能拿到的字段填回来，剩下的一次性贴入。
///
/// 口径见 `docs/design/0011-console-information-architecture.md` §3.1（含三条可选路径）。
const FORMULAS = [
  { value: 'token_rates', label: '按 token 计量（渠道给四档费率）' },
  { value: 'per_image', label: '按张计价（给单价）' },
  { value: 'per_call', label: '按次计价（给单价）' },
  { value: 'upstream_declared', label: '上游直接给金额' },
];

interface OfferingForm {
  provider_kind: string;
  adapter_key: string;
  provider_model_id: string;
  base_url: string;
  credential_env: string;
  formula: string;
  weight: number;
  /** Price Plan（渠道币种）：`token_rates` 才有。 */
  plan_currency: string;
  plan_source_url: string;
  plan_text_input: number;
  plan_image_input: number;
  plan_text_output: number;
  plan_image_output: number;
  /** 按张 / 按次的成本单价。 */
  cost_unit_price_microusd: number;
  cost_currency: string;
  /** `upstream_declared` 的参考成本。 */
  reference_cost_microusd: number;
  /** 技术字段：贴 JSON。 */
  carrier_schema: string;
  parameter_mapping: string;
  restrictions: string;
  /** 对客四档 CNY 费率（按 token 计量的候选随修订带一份）。 */
  margin_enabled: boolean;
  cny_text_input: number;
  cny_image_input: number;
  cny_text_output: number;
  cny_image_output: number;
}

const EMPTY_OFFERING: OfferingForm = {
  provider_kind: '',
  adapter_key: '',
  provider_model_id: '',
  base_url: '',
  credential_env: '',
  formula: 'token_rates',
  weight: 1,
  plan_currency: 'USD',
  plan_source_url: '',
  plan_text_input: 0,
  plan_image_input: 0,
  plan_text_output: 0,
  plan_image_output: 0,
  cost_unit_price_microusd: 0,
  cost_currency: 'USD',
  reference_cost_microusd: 0,
  carrier_schema: '',
  parameter_mapping: '',
  restrictions: '',
  margin_enabled: false,
  cny_text_input: 0,
  cny_image_input: 0,
  cny_text_output: 0,
  cny_image_output: 0,
};

/// 上架与改价的**表单**：运营让一个型号可售、或改它的价的地方。
///
/// 它是「模型目录」页右上角那个动作的内容，由那一页放进抽屉渲染——不是独立一页。两个页面的关系就是
/// "一页看、一页写"，分在两个导航项里会让人看不出它们的联系（用户原话："和网关模型的区别是什么"）。
///
/// `editing` 给一个已发布型号名时，表单先载入它的身份与商务字段，然后进**改价模式**：命令里不带渠道
/// 三要素与驱动器，由服务端从当前生效的修订沿用（合同见 `docs/design/0010` §4.1）。
export function PublishPanel({
  client,
  editing,
  onPublished,
}: {
  client: AdminClient;
  editing?: string | null;
  onPublished?: () => void;
}) {
  const { message } = AntApp.useApp();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<{ gateway_model: string; runtime_revision_id: string } | null>(
    null,
  );

  const [identity, setIdentity] = useState({
    vendor_id: '',
    native_model_id: '',
    gateway_model: '',
    native_revision: '',
    markup_bps: 2000,
    capability_schema: '',
  });
  const [offerings, setOfferings] = useState<OfferingForm[]>([{ ...EMPTY_OFFERING }]);

  function patchOffering(index: number, patch: Partial<OfferingForm>) {
    setOfferings((list) =>
      list.map((item, at) => (at === index ? { ...item, ...patch } : item)),
    );
  }

  /// 把技术字段从 JSON 文本变成对象。空串按"没给"处理（这些字段在命令里都可缺省）。
  function parseJson(text: string, what: string): unknown {
    const trimmed = text.trim();
    if (!trimmed) return undefined;
    try {
      return JSON.parse(trimmed);
    } catch (failure) {
      throw new Error(
        `${what}不是合法 JSON：${failure instanceof Error ? failure.message : failure}`,
      );
    }
  }

  /// 按表单拼出发布命令。**只做翻译**：不做本地校验（形状对不对由发布期的校验答复负责），
  /// 也不替运营算费率（那是运营填的，平台只原样保存与冻结）。
  ///
  /// 改价模式（`loaded !== null`）下**不带渠道三要素与驱动器**：服务端按 `provider_kind` +
  /// `provider_model_id` 从该型号当前生效的修订沿用（合同见 `docs/design/0010` §4.1）。这样运营改价
  /// 时不必碰渠道地址与凭证变量名——它们这次一个字都没变。
  function buildCommand(): Record<string, unknown> {
    const parsed = parseJson(identity.capability_schema, '合同（capability_schema）');
    const command: Record<string, unknown> = {
      vendor_id: identity.vendor_id.trim(),
      native_model_id: identity.native_model_id.trim(),
      native_revision: identity.native_revision.trim(),
      markup_bps: identity.markup_bps,
      actor: 'admin-console',
      offerings: offerings.map((offering, index) => {
        const item: Record<string, unknown> = {
          provider_kind: offering.provider_kind.trim(),
          provider_model_id: offering.provider_model_id.trim(),
          // 档位缺省即下标，这里显式给出来让人能同档分摊。
          routing_priority: index,
          weight: offering.weight,
        };
        if (!repricing) {
          item.adapter_key = offering.adapter_key.trim();
          item.base_url = offering.base_url.trim();
          item.credential_env = offering.credential_env.trim();
          item.formula = offering.formula;
        }
        if (parsed !== undefined) item.capability_schema = parsed;
        const carrier = parseJson(offering.carrier_schema, '承载面（carrier_schema）');
        if (carrier !== undefined) item.carrier_schema = carrier;
        const mapping = parseJson(offering.parameter_mapping, '参数映射（parameter_mapping）');
        if (mapping !== undefined) item.parameter_mapping = mapping;
        const restrictions = parseJson(offering.restrictions, '限制（restrictions）');
        if (restrictions !== undefined) item.restrictions = restrictions;

        if (repricing) {
          // 改价：渠道价目与价目出处**一并省略**，由服务端从当前生效的修订沿用——它们与渠道地址
          // 是同一类东西（渠道怎么结算），这次没变，而管理端视图也不回显它们。这里只发对客费率。
          if (offering.margin_enabled) {
            item.consumer_rates_cny = {
              text_input_micros_per_million: offering.cny_text_input,
              image_input_micros_per_million: offering.cny_image_input,
              text_output_micros_per_million: offering.cny_text_output,
              image_output_micros_per_million: offering.cny_image_output,
            };
          }
          return item;
        }

        if (offering.formula === 'token_rates') {
          item.price_plan = {
            currency: offering.plan_currency.trim().toUpperCase(),
            text_input_microusd_per_million: offering.plan_text_input,
            image_input_microusd_per_million: offering.plan_image_input,
            text_output_microusd_per_million: offering.plan_text_output,
            image_output_microusd_per_million: offering.plan_image_output,
            // 出处是必填：渠道费率对账时要能回去看当初是从哪一页抄的。
            source_url: offering.plan_source_url.trim(),
          };
          item.cost_currency = offering.plan_currency.trim().toUpperCase();
          if (offering.margin_enabled) {
            item.consumer_rates_cny = {
              text_input_micros_per_million: offering.cny_text_input,
              image_input_micros_per_million: offering.cny_image_input,
              text_output_micros_per_million: offering.cny_text_output,
              image_output_micros_per_million: offering.cny_image_output,
            };
          }
        } else if (offering.formula === 'per_image' || offering.formula === 'per_call') {
          // 单价是这两种形态唯一的成本参数。
          item.cost_unit_price_microusd = offering.cost_unit_price_microusd;
          item.cost_currency = offering.cost_currency.trim().toUpperCase();
          if (offering.reference_cost_microusd > 0) {
            item.reference_cost_microusd = offering.reference_cost_microusd;
          }
        } else {
          item.cost_currency = offering.cost_currency.trim().toUpperCase();
          if (offering.reference_cost_microusd > 0) {
            item.reference_cost_microusd = offering.reference_cost_microusd;
          }
        }
        return item;
      }),
    };
    if (identity.gateway_model.trim()) command.gateway_model = identity.gateway_model.trim();
    return command;
  }

  async function publish() {
    setBusy(true);
    setError(null);
    // 上一次的**成功**答复必须先清掉：留着它，这次失败时屏幕上还挂着"已发布"，看的人会以为成了。
    setDone(null);
    try {
      const command = buildCommand();
      const published = await client.publishRevision(command);
      setDone(published);
      message.success(`已发布 ${published.gateway_model}`);
      // 让容器（模型目录）重取列表：刚刚改的价要立刻显示在那一页上。
      onPublished?.();
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setBusy(false);
    }
  }

  /// 从已发布型号载入：把管理端视图**拿得到**的字段填回来（身份、倍率、候选的渠道/驱动器/成本/
  /// 费率）。拿不到的（`base_url`、`credential_env`、承载面）留空，由运营补齐——视图不回显渠道
  /// 部署事实。
  async function loadFromPublished(gatewayModel: string) {
    setBusy(true);
    setError(null);
    // 载入另一个型号时同样要清掉上一次的答复：它说的是别的型号。
    setDone(null);
    try {
      const models = await client.gatewayModels();
      const model = models.gateway_models.find((item) => item.gateway_model === gatewayModel);
      if (!model) throw new Error(`没有找到已发布型号 ${gatewayModel}`);
      setIdentity({
        vendor_id: model.vendor_id,
        native_model_id: model.native_model_id,
        gateway_model: model.gateway_model,
        native_revision: model.native_revision,
        markup_bps: model.markup_bps ?? 2000,
        // 合同随模型视图一起给（它不是渠道配置）：改价要重发同一份，不让运营重贴一遍。
        capability_schema: JSON.stringify(model.capability_schema, null, 2),
      });
      // 身份载入完成 = 进改价模式：命令里不再带渠道三要素，由服务端从当前生效修订沿用。
      setLoaded(gatewayModel);
      setOfferings(
        model.candidates.map((candidate) => ({
          ...EMPTY_OFFERING,
          provider_kind: candidate.provider_kind,
          adapter_key: candidate.adapter_key,
          provider_model_id: candidate.provider_model_id,
          weight: candidate.weight,
          plan_currency: candidate.cost_currency ?? 'USD',
          cost_currency: candidate.cost_currency ?? 'USD',
          reference_cost_microusd: candidate.reference_cost_microusd ?? 0,
          carrier_schema: JSON.stringify(candidate.carrier_schema ?? {}, null, 2),
          parameter_mapping: JSON.stringify(candidate.parameter_mapping ?? {}, null, 2),
          margin_enabled: candidate.consumer_rates_cny !== null,
          cny_text_input: candidate.consumer_rates_cny?.text_input_micros_per_million ?? 0,
          cny_image_input: candidate.consumer_rates_cny?.image_input_micros_per_million ?? 0,
          cny_text_output: candidate.consumer_rates_cny?.text_output_micros_per_million ?? 0,
          cny_image_output: candidate.consumer_rates_cny?.image_output_micros_per_million ?? 0,
        })),
      );
      message.info(
        `已载入 ${gatewayModel}。改价只需要改倍率与对客费率——渠道地址、凭证变量名与驱动器由服务端从当前生效的修订沿用。`,
      );
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setBusy(false);
    }
  }

  /// 改价模式的判据：已经从某个已发布型号载入了身份。非空时 buildCommand 不带渠道字段。
  const [loaded, setLoaded] = useState<string | null>(null);
  const repricing = loaded !== null;

  // 容器给了"要改哪个型号"就载入它。放进依赖数组的只能是 `editing`：`loadFromPublished` 每次渲染都是
  // 新的函数值，把它算进去会无限重载。
  const loadRef = useRef(loadFromPublished);
  loadRef.current = loadFromPublished;
  useEffect(() => {
    if (editing) void loadRef.current(editing);
  }, [editing]);
  // 算价算式要用当前生效的折算率：没有它就算不出 CNY 对客费率，界面上要如实说缺而不是拿 0 顶替。
  const rates = useLoadable(() => client.fxRates(), [client]);
  const currencies: Record<string, number> = { CNY: 1 };
  for (const row of rates.data?.rates ?? []) {
    currencies[row.currency.toUpperCase()] = row.rate_micros / 1_000_000;
  }

  return (
    <Flex vertical gap={16}>
      {error ? <Alert type="error" showIcon message={error} /> : null}
      {done ? (
        <Alert
          type="success"
          showIcon
          message={`已发布 ${done.gateway_model}`}
          description={
            <Flex vertical gap={4}>
              <Typography.Text>
                生效修订：<Typography.Text code>{done.runtime_revision_id}</Typography.Text>
              </Typography.Text>
              <Typography.Text type="secondary">
                去「模型目录」核对候选与定价是否就是你要的那一份。
              </Typography.Text>
            </Flex>
          }
          action={
            <Button size="small" onClick={() => setDone(null)}>
              再发一份
            </Button>
          }
        />
      ) : null}

      <Panel
        title="这个型号是什么"
        description="`gateway_model` 是平台对客名（调用方提交 model 时用的那个）；留空时取原生型号名。改价必须换合同修订号。"
      >
        {repricing ? (
          <>
            <Descriptions
              size="small"
              bordered
              column={{ xs: 1, sm: 2, lg: 3 }}
              items={[
                { key: 'vendor', label: '厂商', children: identity.vendor_id },
                { key: 'native', label: '原生型号名', children: identity.native_model_id },
                { key: 'gateway', label: '平台对客名', children: identity.gateway_model },
              ]}
            />
            <Typography.Paragraph type="secondary" style={{ marginTop: 12, marginBottom: 0 }}>
              这几项与合同一起沿用当前生效的那一份，只有合同修订号要换——改价必须换修订号，否则平台的
              校验结论会说"这份合同已经发过"。
            </Typography.Paragraph>
          </>
        ) : null}
        <Form layout="vertical" style={{ marginTop: repricing ? 16 : 0 }}>
          {repricing ? null : (
            <Row gutter={16}>
              <Col xs={24} md={6}>
                <Form.Item label="厂商" required>
                  <Input
                    data-testid="publish-vendor"
                    value={identity.vendor_id}
                    onChange={(event) => setIdentity({ ...identity, vendor_id: event.target.value })}
                    placeholder="OpenAI"
                  />
                </Form.Item>
              </Col>
              <Col xs={24} md={6}>
                <Form.Item label="原生型号名" required>
                  <Input
                    data-testid="publish-native-model"
                    value={identity.native_model_id}
                    onChange={(event) =>
                      setIdentity({ ...identity, native_model_id: event.target.value })
                    }
                    placeholder="gpt-image-2.5-flare"
                  />
                </Form.Item>
              </Col>
              <Col xs={24} md={6}>
                <Form.Item label="平台对客名（可空）">
                  <Input
                    value={identity.gateway_model}
                    onChange={(event) =>
                      setIdentity({ ...identity, gateway_model: event.target.value })
                    }
                    placeholder="留空即取原生名"
                  />
                </Form.Item>
              </Col>
              <Col xs={24} md={6}>
                <Form.Item label="合同修订号" required>
                  <Input
                    data-testid="publish-native-revision"
                    value={identity.native_revision}
                    onChange={(event) =>
                      setIdentity({ ...identity, native_revision: event.target.value })
                    }
                    placeholder="2026-10-01-contract-1.1"
                  />
                </Form.Item>
              </Col>
            </Row>
          )}
          {repricing ? (
            <Row gutter={16}>
              <Col xs={24} md={6}>
                <Form.Item label="合同修订号" required tooltip="改价必须换修订号">
                  <Input
                    data-testid="publish-native-revision"
                    value={identity.native_revision}
                    onChange={(event) =>
                      setIdentity({ ...identity, native_revision: event.target.value })
                    }
                    placeholder="2026-10-01-contract-1.1"
                  />
                </Form.Item>
              </Col>
            </Row>
          ) : null}
          <Row gutter={16}>
            <Col xs={24} md={6}>
              <Form.Item
                label="加价系数（基点）"
                tooltip="倍数 = 1 + 基点/10000。2000 基点即 ×1.2。按张/按次/上游给金额的候选缺它会被拒。"
                required
              >
                <Input
                  data-testid="publish-markup-bps"
                  type="number"
                  value={identity.markup_bps}
                  onChange={(event) =>
                    setIdentity({ ...identity, markup_bps: Number(event.target.value) })
                  }
                />
              </Form.Item>
            </Col>
          </Row>
        </Form>
      </Panel>

      {offerings.map((offering, index) => (
        <Panel
          key={index}
          title={`候选 ${index + 1}`}
          extra={
            offerings.length > 1 ? (
              <Button
                danger
                size="small"
                icon={<DeleteOutlined />}
                onClick={() => setOfferings((list) => list.filter((_item, at) => at !== index))}
              >
                移除
              </Button>
            ) : null
          }
        >
          <Form layout="vertical">
            <Row gutter={16}>
              <Col xs={24} md={6}>
                <Form.Item label="渠道模型名" required>
                  <Input
                    data-testid="publish-provider-model"
                    value={offering.provider_model_id}
                    onChange={(event) =>
                      patchOffering(index, { provider_model_id: event.target.value })
                    }
                  />
                </Form.Item>
              </Col>
            </Row>
            {repricing ? (
              // 改价模式：渠道地址、凭证变量名与驱动器这次一个字都没变，服务端从当前生效的修订沿用
              // （`docs/design/0010` §4.1）。不摆出来是为了不让运营以为需要重填。
              <Alert
                type="info"
                showIcon
                style={{ marginBottom: 16 }}
                message={`沿用当前渠道：${offering.provider_kind || '（未载入）'} · ${offering.adapter_key || '（未载入）'}`}
                description="渠道地址与凭证变量名由服务端从当前生效的修订取；换渠道请用「新增型号 / 重签合同」。"
              />
            ) : (
              <>
                <Row gutter={16}>
                  <Col xs={24} md={6}>
                    <Form.Item label="渠道" required tooltip="渠道类别，例如 AIHubMix / APIMart">
                      <Input
                        data-testid="publish-provider-kind"
                        value={offering.provider_kind}
                        onChange={(event) =>
                          patchOffering(index, { provider_kind: event.target.value })
                        }
                      />
                    </Form.Item>
                  </Col>
                  <Col xs={24} md={6}>
                    <Form.Item
                      label="驱动器"
                      required
                      tooltip="用哪个 Driver 发出去，例如 aihubmix-image-v1"
                    >
                      <Input
                        data-testid="publish-adapter-key"
                        value={offering.adapter_key}
                        onChange={(event) =>
                          patchOffering(index, { adapter_key: event.target.value })
                        }
                      />
                    </Form.Item>
                  </Col>
                  <Col xs={24} md={12}>
                    <Form.Item
                      label="渠道地址"
                      required
                      tooltip="这个渠道的调用入口；同一入口与凭证身份只算一个渠道"
                    >
                      <Input
                        data-testid="publish-base-url"
                        value={offering.base_url}
                        onChange={(event) => patchOffering(index, { base_url: event.target.value })}
                        placeholder="https://api.example.com"
                      />
                    </Form.Item>
                  </Col>
                </Row>
                <Row gutter={16}>
                  <Col xs={24} md={12}>
                    <Form.Item
                      label="凭证环境变量名"
                      required
                      tooltip="只填变量名（例如 AIHUBMIX_API_KEY），不填密钥本身；密钥只从进程环境读"
                    >
                      <Input
                        data-testid="publish-credential-env"
                        value={offering.credential_env}
                        onChange={(event) =>
                          patchOffering(index, { credential_env: event.target.value })
                        }
                        placeholder="AIHUBMIX_API_KEY"
                      />
                    </Form.Item>
                  </Col>
                </Row>
              </>
            )}

            <Divider plain>成本与对客价</Divider>
            {repricing ? (
              // 改价：渠道怎么结算（计价形态、四档渠道费率、价目出处、成本币种）这次没变，由服务端从
              // 当前生效的修订沿用。摆出来却不随命令发出，等于让人"改了个不生效的数"。
              <Alert
                type="info"
                showIcon
                style={{ marginBottom: 16 }}
                message="渠道成本与计价形态沿用当前生效的那一份"
                description="这一页只改对客价：倍率与下面那四档 CNY 费率。要换计价形态或改渠道费率，请用「上架新模型」重发一份，或去渠道那边重签。"
              />
            ) : null}
            <Row gutter={16}>
              {repricing ? null : (
                <Col xs={24} md={8}>
                  <Form.Item label="计价形态" required>
                    <Select
                      value={offering.formula}
                      options={FORMULAS}
                      onChange={(value: string) => patchOffering(index, { formula: value })}
                    />
                  </Form.Item>
                </Col>
              )}
              {!repricing && offering.formula === 'token_rates' ? (
                <>
                  <Col xs={24} md={4}>
                    <Form.Item label="成本币种">
                      <Input
                        data-testid="publish-plan-currency"
                        value={offering.plan_currency}
                        onChange={(event) =>
                          patchOffering(index, { plan_currency: event.target.value })
                        }
                      />
                    </Form.Item>
                  </Col>
                  <Col xs={24} md={12}>
                    <Form.Item
                      label="渠道费率（每百万 token，成本币种微单位）"
                      tooltip="文入 / 图入 / 文出 / 图出，四个数"
                    >
                      <Space.Compact block>
                        <Input
                          data-testid="publish-plan-text-input"
                          type="number"
                          addonBefore="文入"
                          value={offering.plan_text_input}
                          onChange={(event) =>
                            patchOffering(index, { plan_text_input: Number(event.target.value) })
                          }
                        />
                        <Input
                          data-testid="publish-plan-image-input"
                          type="number"
                          addonBefore="图入"
                          value={offering.plan_image_input}
                          onChange={(event) =>
                            patchOffering(index, { plan_image_input: Number(event.target.value) })
                          }
                        />
                        <Input
                          data-testid="publish-plan-text-output"
                          type="number"
                          addonBefore="文出"
                          value={offering.plan_text_output}
                          onChange={(event) =>
                            patchOffering(index, { plan_text_output: Number(event.target.value) })
                          }
                        />
                        <Input
                          data-testid="publish-plan-image-output"
                          type="number"
                          addonBefore="图出"
                          value={offering.plan_image_output}
                          onChange={(event) =>
                            patchOffering(index, { plan_image_output: Number(event.target.value) })
                          }
                        />
                      </Space.Compact>
                    </Form.Item>
                  </Col>
                </>
              ) : (
                <>
                  <Col xs={24} md={4}>
                    <Form.Item label="成本币种">
                      <Input
                        value={offering.cost_currency}
                        onChange={(event) =>
                          patchOffering(index, { cost_currency: event.target.value })
                        }
                      />
                    </Form.Item>
                  </Col>
                  {offering.formula === 'per_image' || offering.formula === 'per_call' ? (
                    <Col xs={24} md={4}>
                      <Form.Item label="成本单价（微单位）">
                        <Input
                          type="number"
                          value={offering.cost_unit_price_microusd}
                          onChange={(event) =>
                            patchOffering(index, {
                              cost_unit_price_microusd: Number(event.target.value),
                            })
                          }
                        />
                      </Form.Item>
                    </Col>
                  ) : null}
                  <Col xs={24} md={4}>
                    <Form.Item label="定价参考（微单位）" tooltip="成本原币种的参考值，不是售价的被乘数">
                      <Input
                        type="number"
                        value={offering.reference_cost_microusd}
                        onChange={(event) =>
                          patchOffering(index, {
                            reference_cost_microusd: Number(event.target.value),
                          })
                        }
                      />
                    </Form.Item>
                  </Col>
                </>
              )}
            </Row>

            {!repricing && offering.formula === 'token_rates' ? (
              <Row gutter={16}>
                <Col xs={24}>
                  <Form.Item
                    label="费率出处（source_url）"
                    required
                    tooltip="这一组渠道费率是从哪一页抄的。对账时要能回去核对，所以它是必填。"
                  >
                    <Input
                      data-testid="publish-plan-source-url"
                      value={offering.plan_source_url}
                      onChange={(event) =>
                        patchOffering(index, { plan_source_url: event.target.value })
                      }
                      placeholder="https://vendor.example.com/pricing"
                    />
                  </Form.Item>
                </Col>
              </Row>
            ) : null}

            {offering.formula === 'token_rates' ? (
              <>
                <Form.Item style={{ marginBottom: 8 }}>
                  <Space>
                    <Button
                      size="small"
                      onClick={() => patchOffering(index, { margin_enabled: !offering.margin_enabled })}
                    >
                      {offering.margin_enabled ? '移除对客价向量' : '带上对客价向量'}
                    </Button>
                    <Typography.Text type="secondary">
                      按 token 计量的候选随修订带一份四档 CNY 费率，由运营按"成本单价 × 倍率 ×
                      折算率"推导后填入；平台只原样保存与冻结、不在服务端替算。这里填的就是对外价，
                      而 <code>config/bootstrap/*.json</code> 里的对客费率只是夹具值，不是生产价。
                    </Typography.Text>
                  </Space>
                </Form.Item>
                {offering.margin_enabled ? (
                  <Row gutter={16}>
                    <Col xs={24}>
                      <Form.Item label="对客费率（每百万 token，CNY 微单位）">
                        <Space.Compact block>
                          <Input
                            data-testid="publish-cny-text-input"
                            type="number"
                            addonBefore="文入"
                            value={offering.cny_text_input}
                            onChange={(event) =>
                              patchOffering(index, { cny_text_input: Number(event.target.value) })
                            }
                          />
                          <Input
                            data-testid="publish-cny-image-input"
                            type="number"
                            addonBefore="图入"
                            value={offering.cny_image_input}
                            onChange={(event) =>
                              patchOffering(index, { cny_image_input: Number(event.target.value) })
                            }
                          />
                          <Input
                            data-testid="publish-cny-text-output"
                            type="number"
                            addonBefore="文出"
                            value={offering.cny_text_output}
                            onChange={(event) =>
                              patchOffering(index, { cny_text_output: Number(event.target.value) })
                            }
                          />
                          <Input
                            data-testid="publish-cny-image-output"
                            type="number"
                            addonBefore="图出"
                            value={offering.cny_image_output}
                            onChange={(event) =>
                              patchOffering(index, { cny_image_output: Number(event.target.value) })
                            }
                          />
                        </Space.Compact>
                      </Form.Item>
                    </Col>
                  </Row>
                ) : null}
              </>
            ) : null}

            <Collapse
              ghost
              items={[
                {
                  key: 'technical',
                  label: '技术字段（贴 JSON；留空即不带）',
                  children: (
                    <Flex vertical gap={12}>
                      <Typography.Text type="secondary">
                        这些是厂商与渠道给的结构声明，表单收集不了，从渠道文档或上一版贴过来。
                      </Typography.Text>
                      <Form.Item label="承载面 carrier_schema" style={{ marginBottom: 0 }}>
                        <Input.TextArea
                          rows={4}
                          value={offering.carrier_schema}
                          onChange={(event) =>
                            patchOffering(index, { carrier_schema: event.target.value })
                          }
                          style={{ fontFamily: 'ui-monospace, Consolas, monospace', fontSize: 12 }}
                        />
                      </Form.Item>
                      <Form.Item label="参数映射 parameter_mapping" style={{ marginBottom: 0 }}>
                        <Input.TextArea
                          rows={3}
                          value={offering.parameter_mapping}
                          onChange={(event) =>
                            patchOffering(index, { parameter_mapping: event.target.value })
                          }
                          style={{ fontFamily: 'ui-monospace, Consolas, monospace', fontSize: 12 }}
                        />
                      </Form.Item>
                      <Form.Item label="限制 restrictions" style={{ marginBottom: 0 }}>
                        <Input.TextArea
                          rows={3}
                          value={offering.restrictions}
                          onChange={(event) =>
                            patchOffering(index, { restrictions: event.target.value })
                          }
                          style={{ fontFamily: 'ui-monospace, Consolas, monospace', fontSize: 12 }}
                        />
                      </Form.Item>
                    </Flex>
                  ),
                },
              ]}
            />
          </Form>
        </Panel>
      ))}

      <Panel title="模型的合同（capability_schema）">
        <Typography.Paragraph type="secondary" style={{ marginTop: 0 }}>
          厂商给的 JSON Schema：调用方能用哪些字段、各自什么形状。模型级一份，所有候选共用。贴进来的
          原文会被原样保存与冻结。
        </Typography.Paragraph>
        <Input.TextArea
          rows={8}
          value={identity.capability_schema}
          onChange={(event) =>
            setIdentity({ ...identity, capability_schema: event.target.value })
          }
          placeholder='{"$schema":"...","type":"object","properties":{...}}'
          style={{ fontFamily: 'ui-monospace, Consolas, monospace', fontSize: 12 }}
        />
        {identity.capability_schema.trim() ? (
          <Alert
            style={{ marginTop: 8 }}
            type="info"
            showIcon
            message="合同里 model.const 要等于原生型号名（发布期会校验）。"
          />
        ) : null}
      </Panel>

      <Card>
        <Flex justify="space-between" align="center" wrap gap={12}>
          <Space>
            <Button
              icon={<PlusOutlined />}
              onClick={() => setOfferings((list) => [...list, { ...EMPTY_OFFERING }])}
            >
              加一条候选
            </Button>
            <Button
              data-testid="publish-submit"
              onClick={() => void publish()}
              type="primary"
              icon={<SendOutlined />}
              loading={busy}
            >
              发布
            </Button>
          </Space>
          <Typography.Text type="secondary">
            形状对不对由发布期的校验答复负责；这里不做本地预校验，免得出现两套判定。
          </Typography.Text>
        </Flex>
      </Card>

      <PublishedSummary
        offerings={offerings}
        markupBps={identity.markup_bps}
        currencies={currencies}
      />
    </Flex>
  );
}

/// 已发布型号的名字，给"从…载入"那个下拉用。取不到就是空列表——它是辅助信息，失败不影响发布。

/// 把"倍率怎么影响对客价"摆出来给人核对（Spec M2：页面要能看出加价系数怎么影响对客价）。
///
/// 它是**展示**，不是决定：算式是"渠道费率 ×（1 + 倍率）"，平台只照着运营填的数算给他看，并把结果放在
/// 一个只读位置供他抄进对客费率那一栏。**不产出任何合成数**（比如毛利）——成本是渠道币种、售价是 CNY，
/// 两者不相减；谁更便宜是把输入连同用量放在一起才成立的判断，归结算与选路。
function PublishedSummary({
  offerings,
  markupBps,
  currencies,
}: {
  offerings: OfferingForm[];
  markupBps: number;
  currencies: Record<string, number>;
}) {
  if (offerings.length === 0) return null;
  const multiplier = 1 + markupBps / 10000;
  return (
    <Panel
      title="这次会发布什么"
      description={`加价系数 ${markupBps} 基点，即倍数 ×${multiplier.toFixed(4)}。`}
    >
      <Descriptions size="small" column={1} bordered>
        {offerings.map((offering, index) => (
          <Descriptions.Item key={index} label={`候选 ${index + 1}`}>
            {offering.provider_kind || '（未填渠道）'} · {offering.provider_model_id || '（未填模型）'} ·{' '}
            {FORMULAS.find((item) => item.value === offering.formula)?.label}
            {offering.formula === 'token_rates' ? (
              <Derivation offering={offering} markupBps={markupBps} currencies={currencies} />
            ) : offering.formula === 'upstream_declared' ? (
              // 上游直接给金额的候选没有费率可乘——它的对客价由结算按上游声明的那笔钱算。
              <div>（对客价按上游这次声明的金额算）</div>
            ) : (
              <div>（对客价按成本单价 × 倍率 × 折算率算）</div>
            )}
          </Descriptions.Item>
        ))}
      </Descriptions>
    </Panel>
  );
}

/// 按 token 计量候选的推导算式：渠道费率 → 折成 CNY → 乘倍率 → 该填的对客费率。
///
/// 逐档算，因为四档（文入／图入／文出／图出）各乘同一个倍率。缺折算率时**如实说缺**，不拿 0 顶替——
/// 折算率没录的话发布期本来就会被拒，这里看到 0 会以为价是 0。
function Derivation({
  offering,
  markupBps,
  currencies,
}: {
  offering: OfferingForm;
  markupBps: number;
  currencies: Record<string, number>;
}) {
  if (!offering.margin_enabled) {
    return <div>（不带对客价向量，对客价由结算按成本 × 倍率算）</div>;
  }
  const code = offering.plan_currency.trim().toUpperCase();
  const fx = code === 'CNY' ? 1 : currencies[code];
  const rows: [string, number, number][] = [
    ['文入', offering.plan_text_input, offering.cny_text_input],
    ['图入', offering.plan_image_input, offering.cny_image_input],
    ['文出', offering.plan_text_output, offering.cny_text_output],
    ['图出', offering.plan_image_output, offering.cny_image_output],
  ];
  return (
    <div style={{ marginTop: 6 }}>
      {fx === undefined ? (
        <Typography.Text type="warning">
          还没有 {code} 的生效折算率——先到「折算率」页录一行，否则发布会被拒。
        </Typography.Text>
      ) : (
        <Typography.Text type="secondary" style={{ fontSize: 12 }}>
          算式：渠道费率（{code}/百万 token）× {fx}（折算率）× {(1 + markupBps / 10000).toFixed(4)}
          （倍率）= 应填的对客费率（CNY/百万 token）
        </Typography.Text>
      )}
      <div style={{ marginTop: 4 }}>
        {rows.map(([label, cost, filled]) => {
          const expected = fx === undefined ? null : Math.round(cost * fx * (1 + markupBps / 10000));
          const matches = expected !== null && expected === filled;
          return (
            <Typography.Text key={label} style={{ fontSize: 12, display: 'block' }}>
              {label}：{cost} × {fx ?? '？'} × {(1 + markupBps / 10000).toFixed(4)} ={' '}
              {expected ?? '？'}
              {expected === null ? null : matches ? (
                <Typography.Text type="success">　已按算式填好</Typography.Text>
              ) : (
                <Typography.Text type="warning">
                  　当前填的是 {filled}
                  {filled === 0 ? '（还没填）' : '（与算式不同——按你自己的判断来，平台不算价）'}
                </Typography.Text>
              )}
            </Typography.Text>
          );
        })}
      </div>
    </div>
  );
}
