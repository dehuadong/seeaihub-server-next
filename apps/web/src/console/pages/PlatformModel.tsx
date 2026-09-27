import { useEffect, useRef, useState } from 'react';
import {
  Alert,
  App as AntApp,
  Button,
  Checkbox,
  Divider,
  Flex,
  Input,
  InputNumber,
  Select,
  Table,
  Tag,
  Typography,
} from 'antd';
import type { AdminClient } from '../client';
import type { SelectableOffering } from '../../shared/types';
import { Panel } from '../ui';
import { useLoadable } from '../../shared/ui';

/// 一条被勾中的候选：引用哪条 Offering、放在哪一档、权重多少、以及**这条候选的价**。
///
/// 技术定义（驱动器、供应商模型名、渠道三要素、承载面、参数映射、限制）**不在这个表单里**——它们由被
/// 引用的 Offering 决定，服务端从库里取（`docs/design/0012-platform-model-publishing.md` §2/§4）。
type Selected = {
  offering: SelectableOffering;
  priority: number;
  weight: number;
  /// 按 token 计量的四档 CNY 对客费率（运营按"成本单价 × 倍率 × 折算率"推导后填入，平台只存）。
  cny: [number, number, number, number];
  /// 按张 / 按次的参考成本与保底（`0007` §2 的载体）。
  referenceCost: number;
  floorAmounts: string;
};

/// 平台模型发布面板：**运营的那条路**。
///
/// 三步：填平台模型名 → 选厂商 → 在该厂商下勾供给（可多条、排档位、设权重）→ 给价。
///
/// 与「发工程素材」那个面板（`PublishPanel`）的区别是这一条**只有商务字段**：渠道、驱动器、渠道地址、
/// 凭证变量名、承载面、参数映射一次都不出现在这里。这正是它存在的理由——那些是渠道部署事实，由工程师
/// 随素材配一次，运营选的是**已经配好的供给**。
export function PlatformModelPanel({
  client,
  editing,
  onPublished,
}: {
  client: AdminClient;
  editing?: string | null;
  onPublished?: () => void;
}) {
  const { message } = AntApp.useApp();
  const catalog = useLoadable(() => client.selectableOfferings(), [client]);
  const models = useLoadable(() => client.gatewayModels(), [client]);
  const rates = useLoadable(() => client.fxRates(), [client]);

  const [name, setName] = useState('');
  const [vendor, setVendor] = useState<string | null>(null);
  const [markupBps, setMarkupBps] = useState(2000);
  const [selected, setSelected] = useState<Selected[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<{ gateway_model: string } | null>(null);

  const all = catalog.data?.offerings ?? [];
  const vendors = [...new Set(all.map((offering) => offering.vendor_id))].sort();
  const inVendor = all
    .filter((offering) => offering.vendor_id === vendor)
    .sort((left, right) => left.provider_kind.localeCompare(right.provider_kind));

  /// 把某个币种的当前折算率读出来（分母 1e6）。缺了返回 null——**不拿 0 顶替**：
  /// 0 会让"这个币种没有折算率"看起来像"折算率是零"，而发布期会因此拒，界面上要说清是缺。
  function fxRate(currency: string | null): number | null {
    if (!currency) return null;
    const found = (rates.data?.rates ?? []).find((rate) => rate.currency === currency);
    return found ? found.rate_micros / 1_000_000 : null;
  }

  // 改价：把已发布型号现有的候选与价带出来。候选的 `offering_id` 从模型视图的候选里拿——
  // 它指向的就是那条供给，所以"改价"等于把同一组引用原样再发一次。
  //
  // 依赖里**要带 `models.data` 与清单**：这两个都是异步读回来的，`editing` 变化时它们常常还是空的
  // （第一条 effect 会在数据到达之前跑完）。只依赖 `editing` 会让改价抽屉打开后一条候选都没有，
  // 运营看到的是"这个型号没有供给"——而那是错的信息。
  //
  // `loaded` 这个 ref 保证**只灌一次**：数据是每次渲染新建的对象，光看依赖会反复把运营改过的价覆盖
  // 回上一版的值（改价改到一半被"还原"，是个很难查的 bug）。
  const loaded = useRef<string | null>(null);
  useEffect(() => {
    if (!editing) return;
    if (loaded.current === editing) return;
    const listing = models.data?.gateway_models;
    if (!listing) return;
    const model = listing.find((item) => item.gateway_model === editing);
    if (!model) return;
    loaded.current = editing;
    setName(model.gateway_model);
    setVendor(model.vendor_id);
    setMarkupBps(model.markup_bps ?? 2000);
    setSelected(
      model.candidates.map((candidate, index) => {
        const offering = all.find((item) => item.offering_id === candidate.offering_id);
        return {
          offering:
            offering ??
            ({
              offering_id: candidate.offering_id,
              vendor_id: model.vendor_id,
              native_model_id: model.native_model_id,
              native_revision: model.native_revision,
              provider_kind: candidate.provider_kind,
              provider_model_id: candidate.provider_model_id,
              adapter_key: candidate.adapter_key,
              formula: 'token_rates',
              cost_currency: candidate.cost_currency,
              cost_rates: null,
              enabled: candidate.enabled,
            } satisfies SelectableOffering),
          priority: index,
          weight: candidate.weight,
          cny: [
            candidate.consumer_rates_cny?.text_input_micros_per_million ?? 0,
            candidate.consumer_rates_cny?.image_input_micros_per_million ?? 0,
            candidate.consumer_rates_cny?.text_output_micros_per_million ?? 0,
            candidate.consumer_rates_cny?.image_output_micros_per_million ?? 0,
          ],
          referenceCost: candidate.reference_cost_microusd ?? 0,
          floorAmounts: '{}',
        };
      }),
    );
  }, [editing, models.data, all]);

  function toggle(offering: SelectableOffering, checked: boolean) {
    setSelected((list) =>
      checked
        ? [
            ...list,
            {
              offering,
              priority: list.length,
              weight: 1,
              cny: [0, 0, 0, 0],
              referenceCost: 0,
              floorAmounts: '{}',
            },
          ]
        : list.filter((item) => item.offering.offering_id !== offering.offering_id),
    );
  }

  function patch(offeringId: string, change: Partial<Selected>) {
    setSelected((list) =>
      list.map((item) =>
        item.offering.offering_id === offeringId ? { ...item, ...change } : item,
      ),
    );
  }

  /// 按表单拼出**引用式**发布命令。只有商务字段：引用、档位、权重、价。
  function buildCommand(): Record<string, unknown> {
    return {
      gateway_model: name.trim(),
      markup_bps: markupBps,
      actor: 'admin-console',
      references: selected.map((item) => {
        const reference: Record<string, unknown> = {
          offering_id: item.offering.offering_id,
          routing_priority: item.priority,
          weight: item.weight,
        };
        if (item.offering.formula === 'token_rates') {
          reference.consumer_rates_cny = {
            text_input_micros_per_million: item.cny[0],
            image_input_micros_per_million: item.cny[1],
            text_output_micros_per_million: item.cny[2],
            image_output_micros_per_million: item.cny[3],
          };
        }
        if (item.referenceCost > 0) reference.reference_cost_microusd = item.referenceCost;
        if (item.offering.cost_currency) reference.cost_currency = item.offering.cost_currency;
        const floors = item.floorAmounts.trim();
        if (floors && floors !== '{}') {
          try {
            reference.floor_amounts = JSON.parse(floors);
          } catch {
            throw new Error('保底表不是合法 JSON');
          }
        }
        return reference;
      }),
    };
  }

  async function publish() {
    setBusy(true);
    setError(null);
    // 上一次的**成功**答复先清掉：留着它，这次失败时屏幕上还挂着"已发布"，看的人会以为成了。
    setDone(null);
    try {
      if (!name.trim()) throw new Error('平台模型名不能为空');
      if (!vendor) throw new Error('先选厂商');
      if (selected.length === 0) throw new Error('至少选一条供给——没有供给的模型调不动');
      const published = await client.publishRevision(buildCommand());
      setDone({ gateway_model: published.gateway_model });
      message.success(`已发布 ${published.gateway_model}`);
      onPublished?.();
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Flex vertical gap={16}>
      {error ? <Alert type="error" showIcon message={error} /> : null}
      {done ? (
        <Alert
          type="success"
          showIcon
          message={`已发布 ${done.gateway_model}`}
          description="去「模型目录」核对候选与定价；对客目录现在按这个名字出牌。"
          action={
            <Button size="small" onClick={() => setDone(null)}>
              再发一份
            </Button>
          }
        />
      ) : null}

      <Panel
        title="平台模型"
        description="平台对外的那个名字，调用方提交 model 时用的就是它。名字由你定，平台不预设。"
      >
        <Flex gap={16} wrap>
          <div style={{ minWidth: 240, flex: 1 }}>
            <Typography.Text type="secondary">平台模型名</Typography.Text>
            <Input
              data-testid="platform-name"
              value={name}
              onChange={(event) => setName(event.target.value)}
              placeholder="gpt-image-2.5-plus"
            />
          </div>
          <div style={{ minWidth: 200 }}>
            <Typography.Text type="secondary">选厂商</Typography.Text>
            <Select
              data-testid="platform-vendor"
              style={{ width: '100%' }}
              placeholder="先选厂商"
              value={vendor}
              options={vendors.map((item) => ({ value: item, label: item }))}
              onChange={(value: string) => {
                setVendor(value);
                // 换厂商就清空已勾的供给：那些供给不属于新厂商，留着会让发布期报"跨厂商"。
                setSelected([]);
              }}
            />
          </div>
          <div style={{ minWidth: 180 }}>
            <Typography.Text type="secondary">加价系数（基点）</Typography.Text>
            <InputNumber
              data-testid="platform-markup-bps"
              style={{ width: '100%' }}
              value={markupBps}
              min={0}
              onChange={(value) => setMarkupBps(Number(value ?? 0))}
            />
            <Typography.Text type="secondary">
              每个平台模型一个。倍率 = 1 + {markupBps}/10000
            </Typography.Text>
          </div>
        </Flex>
      </Panel>

      <Panel
        title="选供给"
        description="这些是工程师已经配好的调用通路。选完就能用——驱动器、渠道地址与凭证变量名不需要你填。"
      >
        {catalog.error ? <Alert type="error" showIcon message={catalog.error} /> : null}
        {!vendor ? (
          <Typography.Text type="secondary">先在上面选一个厂商，这里会列出它下面的供给。</Typography.Text>
        ) : inVendor.length === 0 ? (
          <Typography.Text type="secondary">
            这个厂商下还没有配好的供给。渠道接入由工程师完成，配好之后这里就会出现。
          </Typography.Text>
        ) : (
          <Flex vertical gap={8}>
            {inVendor.map((offering) => {
              const picked = selected.find(
                (item) => item.offering.offering_id === offering.offering_id,
              );
              const rate = fxRate(offering.cost_currency);
              return (
                <div key={offering.offering_id}>
                  <Checkbox
                    data-testid={`platform-pick-${offering.provider_kind}-${offering.provider_model_id}`}
                    checked={Boolean(picked)}
                    disabled={!offering.enabled}
                    onChange={(event) => toggle(offering, event.target.checked)}
                  >
                    <Typography.Text strong>{offering.provider_kind}</Typography.Text>
                    <Typography.Text type="secondary">
                      {' '}
                      · {offering.provider_model_id} · {offering.formula}
                      {offering.cost_currency ? ` · ${offering.cost_currency}` : ''}
                    </Typography.Text>
                    {!offering.enabled ? (
                      <Tag color="default" style={{ marginLeft: 8 }}>
                        已停用，不能选
                      </Tag>
                    ) : null}
                  </Checkbox>
                  {picked ? (
                    <Flex vertical gap={8} style={{ marginTop: 8, marginLeft: 24 }}>
                      {offering.formula === 'token_rates' ? (
                        <>
                          <Typography.Text type="secondary">
                            对客四档费率（CNY／百万 token）：按"渠道费率 × 倍率 × 折算率"推导后填入。
                            {offering.cost_rates
                              ? ` 渠道费率 ${offering.cost_rates.currency} 文入 ${offering.cost_rates.text_input_microusd_per_million}／图入 ${offering.cost_rates.image_input_microusd_per_million}／文出 ${offering.cost_rates.text_output_microusd_per_million}／图出 ${offering.cost_rates.image_output_microusd_per_million}（微单位）`
                              : ''}
                            {rate === null
                              ? offering.cost_currency
                                ? ` · 这个币种还没有生效的折算率，发布会被拒——先去「折算率」录一行 ${offering.cost_currency} → CNY`
                                : ''
                              : ` · 当前折算率 ${rate}`}
                          </Typography.Text>
                          <Flex gap={8}>
                            {(['文入', '图入', '文出', '图出'] as const).map((label, index) => (
                              <InputNumber
                                key={label}
                                data-testid={`platform-cny-${index}`}
                                addonBefore={label}
                                value={picked.cny[index]}
                                onChange={(value) =>
                                  patch(offering.offering_id, {
                                    cny: picked.cny.map((current, at) =>
                                      at === index ? Number(value ?? 0) : current,
                                    ) as [number, number, number, number],
                                  })
                                }
                              />
                            ))}
                          </Flex>
                        </>
                      ) : (
                        <Flex gap={8} wrap>
                          <InputNumber
                            addonBefore="参考成本（原币种微单位）"
                            value={picked.referenceCost}
                            onChange={(value) =>
                              patch(offering.offering_id, { referenceCost: Number(value ?? 0) })
                            }
                          />
                          <Input
                            addonBefore="保底表 JSON"
                            style={{ width: 280 }}
                            value={picked.floorAmounts}
                            onChange={(event) =>
                              patch(offering.offering_id, { floorAmounts: event.target.value })
                            }
                          />
                        </Flex>
                      )}
                      <Flex gap={8}>
                        <InputNumber
                          addonBefore="档位"
                          value={picked.priority}
                          min={0}
                          onChange={(value) =>
                            patch(offering.offering_id, { priority: Number(value ?? 0) })
                          }
                        />
                        <InputNumber
                          addonBefore="档内权重"
                          value={picked.weight}
                          min={1}
                          onChange={(value) =>
                            patch(offering.offering_id, { weight: Number(value ?? 1) })
                          }
                        />
                      </Flex>
                    </Flex>
                  ) : null}
                </div>
              );
            })}
          </Flex>
        )}
      </Panel>

      {selected.length > 0 ? (
        <Panel title={`这次会发布什么（${selected.length} 条候选）`}>
          <Table
            size="small"
            rowKey={(row) => row.offering.offering_id}
            pagination={false}
            dataSource={selected}
            columns={[
              {
                title: '供给',
                render: (_, row) => `${row.offering.provider_kind}／${row.offering.provider_model_id}`,
              },
              { title: '档位', dataIndex: 'priority', width: 80 },
              { title: '权重', dataIndex: 'weight', width: 80 },
              {
                title: '对客价',
                render: (_, row) =>
                  row.offering.formula === 'token_rates'
                    ? row.cny.join(' / ')
                    : `参考成本 ${row.referenceCost}`,
              },
            ]}
          />
          <Divider style={{ margin: '12px 0' }} />
          <Button
            type="primary"
            data-testid="platform-publish"
            loading={busy}
            onClick={() => void publish()}
          >
            发布
          </Button>
        </Panel>
      ) : null}
    </Flex>
  );
}
