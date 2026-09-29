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
import type { ConsumerFormula, SelectableOffering } from '../../shared/types';
import { Panel } from '../ui';
import { useLoadable } from '../../shared/ui';

const CONSUMER_FORMULAS: { value: ConsumerFormula; label: string }[] = [
  { value: 'token_rates', label: '按 token 四档' },
  { value: 'upstream_declared', label: '上游声明金额 × 倍率' },
];

/// 把服务端回来的对客形态收成受控取值。
///
/// 对客只有这两个取值；认不出（**含历史修订没带这个字段**——那时对客形态就是成本形态）时返回
/// `null`，由调用方按"等于成本形态"补上（`0007` §2 兼容）。
function parseConsumerFormula(value: string | null | undefined): ConsumerFormula | null {
  return value === 'token_rates' || value === 'upstream_declared' ? value : null;
}

/// 一条被勾中的候选：引用哪条 Offering、放在哪一档、权重多少、以及**这条候选的对客价**。
///
/// 技术定义（驱动器、供应商模型名、渠道三要素、承载面、参数映射、限制）**不在这个表单里**——它们由被
/// 引用的 Offering 决定，服务端从库里取（`docs/design/0012-platform-model-publishing.md` §2/§4）。
type Selected = {
  offering: SelectableOffering;
  priority: number;
  weight: number;
  /// 对客计价形态：运营按候选选，与 Offering 的成本形态独立（`ADR-0021`）。
  consumerFormula: ConsumerFormula;
  /// 对客选 token 四档时的四档 CNY 费率（每百万 token，微单位）。
  /// `null` = 运营还没动过，按"该 vendor／模型已知渠道价目 × 倍率 × 折算率"实时推导；改动后固定为
  /// 运营填的那份。
  cny: [number, number, number, number] | null;
  /// 该候选的参考成本与保底（`0007` §2 的载体）。
  referenceCost: number;
  floorAmounts: string;
};

/// 平台模型发布面板：**运营的那条路**。
///
/// 三步：填平台模型名 → 选厂商 → 在该厂商下勾供给（可多条、排档位、设权重）→ 给对客形态与价。
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
  /// 倍率 = 1 + markup_bps / 10000：对客 token 初始价的推导要用它，上游金额形态的提示也要显示它。
  const multiplier = 1 + markupBps / 10000;

  /// 把某个币种的当前折算率读出来（分母 1e6）。缺了返回 null——**不拿 0 顶替**：
  /// 0 会让"这个币种没有折算率"看起来像"折算率是零"，而发布期会因此拒，界面上要说清是缺。
  function fxRate(currency: string | null): number | null {
    if (!currency) return null;
    const found = (rates.data?.rates ?? []).find((rate) => rate.currency === currency);
    return found ? found.rate_micros / 1_000_000 : null;
  }

  /// 该 vendor／模型**已知的渠道价目**来自哪条供给：对客 token 四档的初始值取它，与选哪条候选无关
  /// （`0007` §2）。
  ///
  /// 平台维护的是"这个 vendor／模型"的一张价目表，所以这里在该 vendor／模型下带 `cost_rates` 的供给
  /// 里取一条，**按渠道名与模型名定序**（不依赖清单顺序）；取到的来源标在界面上，让运营知道这份
  /// 初始价是从哪条供给抄的。
  function knownRateSource(offering: SelectableOffering): SelectableOffering | null {
    const withRates = all
      .filter(
        (item) =>
          item.vendor_id === offering.vendor_id &&
          item.native_model_id === offering.native_model_id &&
          item.cost_rates,
      )
      .sort((left, right) =>
        (left.provider_kind + '/' + left.provider_model_id).localeCompare(
          right.provider_kind + '/' + right.provider_model_id,
        ),
      );
    return withRates[0] ?? null;
  }

  /// 对客 token 四档的**初始值**：该 vendor／模型已知渠道价目 × 倍率 × 折算率（`0007` §2）。
  ///
  /// 这是**推导**、不是平台替运营定价：值只是预填给运营看，改了就按他填的发布、随 Job 快照冻结。
  /// 该 vendor／模型没有已知价目、或该币种没有折算率时返回 `null`——**不拿 0 顶替**：0 是一个真实的
  /// 价（等于白送），而"推不出来"不是 0；界面会让运营自己填，空着不许发。
  function derivedCny(offering: SelectableOffering): [number, number, number, number] | null {
    const known = knownRateSource(offering)?.cost_rates;
    if (!known) return null;
    const fx = fxRate(known.currency);
    if (fx === null) return null;
    const scale = fx * multiplier;
    const convert = (value: number) => Math.round(value * scale);
    return [
      convert(known.text_input_microusd_per_million),
      convert(known.image_input_microusd_per_million),
      convert(known.text_output_microusd_per_million),
      convert(known.image_output_microusd_per_million),
    ];
  }

  /// 一条候选对客选 token 四档时实际会发出去的费率：运营填过就用他填的，否则用推导值；
  /// 推不出来（没有渠道费率 / 折算率）时是 `null`——那时不能发，由 `publish` 先拦。
  function effectiveCny(item: Selected): [number, number, number, number] | null {
    return item.cny ?? derivedCny(item.offering);
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
        const resolved =
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
          } satisfies SelectableOffering);
        return {
          offering: resolved,
          priority: index,
          weight: candidate.weight,
          consumerFormula:
            parseConsumerFormula(candidate.consumer_formula) ??
            parseConsumerFormula(resolved.formula) ??
            'token_rates',
          // 已发布的对客费率是运营确认过的那份，原样带回来；这一版没有就交给"按成本推导"。
          cny: candidate.consumer_rates_cny
            ? [
                candidate.consumer_rates_cny.text_input_micros_per_million,
                candidate.consumer_rates_cny.image_input_micros_per_million,
                candidate.consumer_rates_cny.text_output_micros_per_million,
                candidate.consumer_rates_cny.image_output_micros_per_million,
              ]
            : null,
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
              // 对客形态的默认是**按 token 四档**（平台主流收法），运营可以立刻在下拉里改。
              consumerFormula: 'token_rates',
              cny: null,
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

  /// 按表单拼出**引用式**发布命令。只有商务字段：引用、档位、权重、对客形态与对客价。
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
          consumer_formula: item.consumerFormula,
        };
        if (item.consumerFormula === 'token_rates') {
          const cny = effectiveCny(item);
          if (cny !== null) {
            reference.consumer_rates_cny = {
              text_input_micros_per_million: cny[0],
              image_input_micros_per_million: cny[1],
              text_output_micros_per_million: cny[2],
              image_output_micros_per_million: cny[3],
            };
          }
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
      // 对客价缺了就让服务端拒只会报得晚，也容易把"推不出来的 0"当成价发出去；这里先说清。
      if (
        selected.some(
          (item) => item.consumerFormula === 'token_rates' && effectiveCny(item) === null,
        )
      ) {
        throw new Error('按 token 四档要先填对客费率');
      }
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
              const rateSource = knownRateSource(offering);
              const known = rateSource?.cost_rates ?? null;
              const shownCny = picked ? effectiveCny(picked) : null;
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
                      · {offering.provider_model_id} · 成本形态 {offering.formula}
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
                      <Flex gap={8} align="center" wrap>
                        <Typography.Text type="secondary">对客计价形态</Typography.Text>
                        <Select
                          data-testid={`platform-consumer-form-${offering.provider_kind}-${offering.provider_model_id}`}
                          style={{ minWidth: 200 }}
                          value={picked.consumerFormula}
                          options={CONSUMER_FORMULAS}
                          onChange={(value: ConsumerFormula) =>
                            patch(offering.offering_id, { consumerFormula: value })
                          }
                        />
                        <Typography.Text type="secondary">
                          与成本形态（{offering.formula}）相互独立：成本怎么算由渠道定，对客怎么收你定。
                        </Typography.Text>
                      </Flex>

                      {picked.consumerFormula === 'token_rates' ? (
                        <>
                          {known ? (
                            <Typography.Text type="secondary">
                              初始值 = 该 vendor／模型已知渠道价目（取自 {rateSource?.provider_kind}）× 倍率 × 折算率：
                              {known.currency} 文入{' '}
                              {known.text_input_microusd_per_million}／图入{' '}
                              {known.image_input_microusd_per_million}／文出{' '}
                              {known.text_output_microusd_per_million}／图出{' '}
                              {known.image_output_microusd_per_million}（微单位）
                              {fxRate(known.currency) === null
                                ? ` · 当前折算率未录——先去「折算率」录一行 ${known.currency} → CNY，否则发布会拒`
                                : ` · 当前折算率 ${fxRate(known.currency)}`}
                              。已按此预填，改动后按你填的发布。
                            </Typography.Text>
                          ) : (
                            <Typography.Text type="secondary">
                              这个 vendor／模型还没有已知的渠道 token 价目可推导——对客四档请按报价直接填，
                              空着不许发。
                            </Typography.Text>
                          )}
                          <Flex gap={8}>
                            {(['文入', '图入', '文出', '图出'] as const).map((label, index) => (
                              <InputNumber
                                key={label}
                                data-testid={`platform-cny-${index}`}
                                addonBefore={label}
                                value={shownCny?.[index] ?? undefined}
                                onChange={(value) =>
                                  patch(offering.offering_id, {
                                    cny: (shownCny ?? [0, 0, 0, 0]).map((current, at) =>
                                      at === index ? Number(value ?? 0) : current,
                                    ) as [number, number, number, number],
                                  })
                                }
                              />
                            ))}
                          </Flex>
                        </>
                      ) : null}


                      {picked.consumerFormula === 'upstream_declared' ? (
                        <Typography.Text type="secondary">
                          对客价按上游这次声明的金额 × 倍率（当前 ×{multiplier.toFixed(4)}）算，
                          这里不用填价。
                        </Typography.Text>
                      ) : null}

                      {offering.formula !== 'token_rates' ? (
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
                      ) : null}

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
                title: '对客计价形态',
                render: (_, row) =>
                  CONSUMER_FORMULAS.find((item) => item.value === row.consumerFormula)?.label ??
                  row.consumerFormula,
              },
              {
                title: '对客价',
                render: (_, row) => {
                  if (row.consumerFormula === 'token_rates') {
                    const cny = effectiveCny(row);
                    return cny === null ? '（未填）' : cny.join(' / ');
                  }
                  return '上游声明额 × 倍率';
                },
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
