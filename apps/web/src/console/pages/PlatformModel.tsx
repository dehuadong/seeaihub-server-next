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
import type { ConsumerFormula, ConsumerReferenceRates, SelectableOffering } from '../../shared/types';
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

/// 这条通路**允许**的对客计价形态——只摆这条通路真能算出来的那一种。
///
/// - "按 token 四档"要求驱动器给得出四分项用量（`provides_token_usage`），否则算不出对客价；
/// - "上游声明金额 × 倍率"要求驱动器会从上游响应里取到金额（`declares_cost`）。
///
/// 判据是**显式的 `false`**：字段缺失（前端与后端版本错位时）按"不确定"处理、两项都留着，
/// 免得把一个本来能选的形态静默藏掉；真选错了发布期那道校验仍会拒。
function consumerFormulas(
  declaresCost: boolean | undefined,
  providesTokenUsage: boolean | undefined,
): {
  value: ConsumerFormula;
  label: string;
}[] {
  return CONSUMER_FORMULAS.filter(
    (item) =>
      (item.value === 'token_rates' && providesTokenUsage !== false) ||
      (item.value === 'upstream_declared' && declaresCost !== false),
  );
}

/// 微单位 ↔ 原币种金额：库里存的是微单位（`5000000` = $5／每 1M token），给运营看与填的是**原币种
/// 金额**（`5`）。换算在界面这一层做——运营不做单位换算。
const MICRO_PER_UNIT = 1_000_000;
const toMajor = (micros: number) => micros / MICRO_PER_UNIT;
const toMicros = (value: number) => Math.round(value * MICRO_PER_UNIT);
/// 微单位 → 人民币元的文本（`43200000` → `43.20`）。
const yuanPerMillion = (micros: number) => (micros / MICRO_PER_UNIT).toFixed(2);

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
  /// 对客选 token 四档时的四档金额，**渠道原币种**（每百万 token，微单位）。
  /// `null` = 运营还没动过，按"该 vendor／模型已知渠道价目"取默认值；改动后固定为运营填的那份。
  /// 对客人民币价由它 × 折算率 × 倍率算出来，运营不直接填人民币微单位。
  amounts: [number, number, number, number] | null;
};

/// 平台模型发布面板：**运营的那条路**。
///
/// 三步：填平台模型名 → 选厂商 → 在该厂商下勾供给（可多条、排档位、设权重）→ 给对客形态与价。
///
/// 这个面板**只有商务字段**：渠道、驱动器、渠道地址、凭证变量名、承载面、参数映射一次都不出现。
/// 那些是渠道部署事实，由工程师随发布素材（`config/bootstrap/*.json`）配好、服务启动时导入；
/// 运营选的是**已经配好的供给**。运营后台没有"贴整份技术定义"的入口。
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

  /// 该 vendor／模型**已知的价目**：对客 token 四档的初始值取它，与勾哪条候选无关（`0007` §2）。
  ///
  /// 来源按优先次序取一条：**模型声明的对客参考价目**（`consumer_reference_rates`，与成本形态无关）
  /// 优先，其次是按 token 计量的那条供给的成本费率（旧口径的 Price Plan）。参考价目是模型级的一份，
  /// 该模型下每条供给带回来的是同一个值；成本费率仍按候选自己的那条取。
  function knownRates(offering: SelectableOffering): {
    rates: ConsumerReferenceRates;
    fromReference: boolean;
  } | null {
    if (offering.consumer_reference_rates) {
      return { rates: offering.consumer_reference_rates, fromReference: true };
    }
    if (offering.cost_rates) {
      return { rates: offering.cost_rates, fromReference: false };
    }
    return null;
  }

  /// 这条候选的**成本币种**：供给自己声明的优先，否则取它那份成本费率的币种。
  ///
  /// 它随候选发布（`cost_currency`），与四档金额用的币种是两件事：金额可能取自**另一条供给声明的
  /// 参考价目**，那份价目的币种才是金额的币种（见 [`amountsCurrency`]）。
  function costCurrency(offering: SelectableOffering): string | null {
    return offering.cost_currency ?? offering.cost_rates?.currency ?? null;
  }

  /// 四档金额用的**币种**：取那份已知价目的币种——金额就是从它抄来的，换算也必须用它，不能拿候选
  /// 自己的成本币种去折（两者可以不同：参考价目按渠道记，候选的成本币种按供给声明）。
  function amountsCurrency(offering: SelectableOffering): string | null {
    return knownRates(offering)?.rates.currency ?? offering.cost_currency ?? null;
  }

  /// 四档金额的**默认值**：该 vendor／模型已知的价目（原币种，每百万 token 微单位）。
  ///
  /// 这是**默认值**、不是平台替运营定价：值只是预填给运营，改了就按他填的发布、随 Job 快照冻结。
  /// 该 vendor／模型没有已知价目时返回 `null`——**不拿 0 顶替**：0 是一个真实的价（等于白送），
  /// 而"没有默认值"不是 0；界面让运营自己填，空着不许发。
  function defaultAmounts(offering: SelectableOffering): [number, number, number, number] | null {
    const known = knownRates(offering)?.rates;
    if (!known) return null;
    return [
      known.text_input_microusd_per_million,
      known.image_input_microusd_per_million,
      known.text_output_microusd_per_million,
      known.image_output_microusd_per_million,
    ];
  }

  /// 勾选或载入一条供给时的**对客形态**：取这条通路可选的那一种。
  ///
  /// 带回来的那个（改价载入的是**历史修订**里的形态）只要还在可选集合里就用它，否则落到集合里的
  /// 第一种（`CONSUMER_FORMULAS` 的顺序就是优先次序——按 token 四档优先，Spec 0001 v9）。渠道能力
  /// 可能已经变了（AIHubMix 改走 `/ai/v1` 之后不再给四分项用量），那时历史值落在不可选的形态上，
  /// 界面会显示一个发不出去的形态；一种都给不出时保留带回来的那个，由发布期拒。
  function resolveConsumerFormula(
    preferred: ConsumerFormula | null,
    offering: SelectableOffering,
  ): ConsumerFormula {
    const allowed = consumerFormulas(offering.declares_cost, offering.provides_token_usage);
    if (preferred && allowed.some((item) => item.value === preferred)) return preferred;
    return allowed[0]?.value ?? preferred ?? 'token_rates';
  }

  /// 这条候选实际用的四档金额：运营填过就用他填的，否则用默认值。
  function effectiveAmounts(item: Selected): [number, number, number, number] | null {
    return item.amounts ?? defaultAmounts(item.offering);
  }

  /// 对客人民币价（每百万 token，微单位）= **原币种金额 × 折算率 × 倍率**（`0007` §2）。
  /// 金额或折算率缺一个就是 `null`——那时不能发，由 `publish` 先拦。
  function effectiveCny(item: Selected): [number, number, number, number] | null {
    const amounts = effectiveAmounts(item);
    if (!amounts) return null;
    const fx = fxRate(amountsCurrency(item.offering));
    if (fx === null) return null;
    const scale = fx * multiplier;
    return amounts.map((value) => Math.round(value * scale)) as [number, number, number, number];
  }

  // 改价：把已发布型号现有的候选与价带出来。候选的 `offering_id` 从模型视图的候选里拿——
  // 它指向的就是那条供给，所以"改价"等于把同一组引用原样再发一次。
  //
  // 依赖里**要带 `models.data` 与清单**：这两个都是异步读回来的，`editing` 变化时它们常常还是空的
  // （第一条 effect 会在数据到达之前跑完）。只依赖 `editing` 会让改价抽屉打开后一条候选都没有，
  // 运营看到的是"这个型号没有供给"——而那是错的信息。
  //
  // 灌两次就够：第一次在数据到齐时，第二次在**折算率表**到达时（已发布的是人民币价，要反推回渠道
  // 原币种金额；没有折算率就反推不出来）。`touched` 之后不再灌——数据是每次渲染新建的对象，光看依赖
  // 会反复把运营改过的值覆盖回上一版。
  const loadedEditing = useRef<string | null>(null);
  const loadedWithFx = useRef(false);
  const touched = useRef(false);
  useEffect(() => {
    if (!editing) {
      loadedEditing.current = null;
      loadedWithFx.current = false;
      touched.current = false;
      return;
    }
    if (loadedEditing.current !== editing) {
      loadedEditing.current = editing;
      loadedWithFx.current = false;
      touched.current = false;
    }
    const hasFx = Boolean(rates.data);
    if (loadedWithFx.current === hasFx) return;
    if (touched.current) return;
    const listing = models.data?.gateway_models;
    if (!listing) return;
    const model = listing.find((item) => item.gateway_model === editing);
    if (!model) return;
    loadedWithFx.current = hasFx;
    setName(model.gateway_model);
    setVendor(model.vendor_id);
    setMarkupBps(model.markup_bps ?? 2000);
    const modelMultiplier = 1 + (model.markup_bps ?? 2000) / 10000;
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
            // 这条供给不在可选清单里（已删/停用）时查不到驱动器声明；对**已发布**的候选取宽松值，
            // 免得把已经发出去的"上游声明金额"形态在改价时藏掉。
            declares_cost: true,
            provides_token_usage: true,
            cost_currency: candidate.cost_currency,
            cost_rates: null,
            consumer_reference_rates: null,
            enabled: candidate.enabled,
          } satisfies SelectableOffering);
        // 已发布的是**人民币**价，反推回渠道原币种金额：界面给运营看与改的是原币种金额，人民币价由它
        // 算出来。缺折算率或没带价时交给默认值。
        const published = candidate.consumer_rates_cny;
        const fx = fxRate(amountsCurrency(resolved));
        const amounts =
          published && fx !== null
            ? ([ 
                published.text_input_micros_per_million,
                published.image_input_micros_per_million,
                published.text_output_micros_per_million,
                published.image_output_micros_per_million,
              ].map((value) => Math.round(value / (fx * modelMultiplier))) as [
                number,
                number,
                number,
                number,
              ])
            : null;
        return {
          offering: resolved,
          priority: index,
          weight: candidate.weight,
          consumerFormula: resolveConsumerFormula(
            parseConsumerFormula(candidate.consumer_formula) ??
              parseConsumerFormula(resolved.formula),
            resolved,
          ),
          amounts,
        };
      }),
    );
  }, [editing, models.data, all, rates.data]);

  function toggle(offering: SelectableOffering, checked: boolean) {
    touched.current = true;
    setSelected((list) =>
      checked
        ? [
            ...list,
            {
              offering,
              priority: list.length,
              weight: 1,
              // 对客形态的默认是这条通路**可选的第一种**：按 token 四档优先（平台主流收法），
              // 它不成立时落到唯一可选的那种——AIHubMix 只回金额、不给用量，默认就是它。
              consumerFormula: resolveConsumerFormula(null, offering),
              amounts: null,
            },
          ]
        : list.filter((item) => item.offering.offering_id !== offering.offering_id),
    );
  }

  function patch(offeringId: string, change: Partial<Selected>) {
    touched.current = true;
    setSelected((list) =>
      list.map((item) =>
        item.offering.offering_id === offeringId ? { ...item, ...change } : item,
      ),
    );
  }

  const [fxDraft, setFxDraft] = useState<Record<string, number | null>>({});
  const [recordingFx, setRecordingFx] = useState<string | null>(null);

  /// 当场录一条折算率——缺它就算不出人民币对客价。录完重取折算率表，界面立刻出结果。
  async function recordFx(currency: string) {
    const value = fxDraft[currency];
    if (!value || value <= 0) {
      setError('折算率要填一个大于 0 的数');
      return;
    }
    setRecordingFx(currency);
    setError(null);
    try {
      await client.upsertFxRate(currency, Math.round(value * MICRO_PER_UNIT));
      rates.reload();
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setRecordingFx(null);
    }
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
        // 渠道币种是**成本侧的管道**，不是运营填的字段：服务端按它取折算率、把成本折成人民币做毛利。
        const currency = costCurrency(item.offering);
        if (currency) reference.cost_currency = currency;
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
        throw new Error('按 token 四档要先给四档金额，并给该币种的折算率');
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
        {!catalog.loading && !catalog.error && all.length === 0 ? (
          <Typography.Text type="secondary" data-testid="platform-supply-empty">
            还没有任何可选的供给。供给来自工程师配置的发布素材，由服务启动时导入；素材配好、服务重启
            之后，这里会按厂商列出可选供给。
          </Typography.Text>
        ) : !vendor ? (
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
              const rateSource = knownRates(offering);
              const known = rateSource?.rates ?? null;
              // 折算率那一行要录的是**这次定价真正用到的币种**：按 token 四档卖时是四档金额的币种
              // （可能来自另一条供给声明的参考价目），其余形态是候选的成本币种（上游声明的金额）。
              const currency =
                picked?.consumerFormula === 'token_rates'
                  ? amountsCurrency(offering)
                  : costCurrency(offering);
              const shownAmounts = picked ? effectiveAmounts(picked) : null;
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
                          options={consumerFormulas(offering.declares_cost, offering.provides_token_usage)}
                          onChange={(value: ConsumerFormula) =>
                            patch(offering.offering_id, { consumerFormula: value })
                          }
                        />
                        <Typography.Text type="secondary">
                          与成本形态（{offering.formula}）相互独立：成本怎么算由渠道定，对客怎么收你定。
                          {offering.provides_token_usage === false
                            ? '这条通路只回金额、不给用量，所以对客只能按上游声明金额 × 倍率。'
                            : offering.declares_cost === false
                              ? '这条通路只回用量、不给金额，所以对客只能按 token 四档。'
                              : ''}
                        </Typography.Text>
                      </Flex>

                      {picked.consumerFormula === 'token_rates' ? (
                        <>
                          <Typography.Text type="secondary">
                            {known && rateSource
                              ? `四档金额按渠道原币种（${known.currency}／每 1M token）填，默认取该 vendor／模型已知的${rateSource.fromReference ? '对客参考价目' : '渠道成本费率'}，可以改。`
                              : '这个 vendor／模型还没有已知的价目可作默认值——请按报价填四档金额（原币种），空着不许发。'}
                            对客人民币价 = 金额 × 折算率 × 倍率（当前 ×{multiplier.toFixed(4)}）。
                          </Typography.Text>
                          <Flex gap={8} wrap>
                            {(['文入', '图入', '文出', '图出'] as const).map((label, index) => (
                              <InputNumber
                                key={label}
                                data-testid={`platform-amount-${index}`}
                                addonBefore={label}
                                addonAfter={currency ?? undefined}
                                min={0}
                                value={
                                  shownAmounts ? toMajor(shownAmounts[index] ?? 0) : undefined
                                }
                                onChange={(value) =>
                                  patch(offering.offering_id, {
                                    amounts: (shownAmounts ?? [0, 0, 0, 0]).map((current, at) =>
                                      at === index ? toMicros(Number(value ?? 0)) : current,
                                    ) as [number, number, number, number],
                                  })
                                }
                              />
                            ))}
                          </Flex>
                          {shownCny ? (
                            <Typography.Text type="secondary">
                              对客人民币价（每 1M token）：文入 ¥{yuanPerMillion(shownCny[0])}／图入 ¥
                              {yuanPerMillion(shownCny[1])}／文出 ¥{yuanPerMillion(shownCny[2])}／图出 ¥
                              {yuanPerMillion(shownCny[3])}
                            </Typography.Text>
                          ) : shownAmounts !== null ? (
                            // 金额缺了不说"缺折算率"：那句上面已经说清要按报价填，这里再报一次会把
                            // 两种缺法混成一句话。
                            <Typography.Text type="warning">
                              还缺折算率，人民币对客价算不出来。
                            </Typography.Text>
                          ) : null}
                        </>
                      ) : null}

                      {picked.consumerFormula === 'upstream_declared' ? (
                        <Typography.Text type="secondary">
                          对客价按上游这次声明的金额 × 倍率（当前 ×{multiplier.toFixed(4)}）× 折算率算，
                          这里不用填价。
                        </Typography.Text>
                      ) : null}

                      {currency ? (
                        <FxRateRow
                          currency={currency}
                          rate={fxRate(currency)}
                          draft={fxDraft[currency] ?? null}
                          recording={recordingFx === currency}
                          onDraft={(value) =>
                            setFxDraft((current) => ({ ...current, [currency]: value }))
                          }
                          onRecord={() => void recordFx(currency)}
                        />
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
                    return cny === null
                      ? '（未填）'
                      : cny.map((value) => `¥${yuanPerMillion(value)}`).join(' / ');
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

/// 一条候选定价处显示的**折算率**（该币种 → CNY）：有就显示当前值；缺就当场录。
///
/// 缺它就算不出人民币对客价，所以不让人跳去别的页面——录完（`upsertFxRate`）重取折算率表，界面立刻
/// 出结果。
function FxRateRow({
  currency,
  rate,
  draft,
  recording,
  onDraft,
  onRecord,
}: {
  currency: string;
  rate: number | null;
  draft: number | null;
  recording: boolean;
  onDraft: (value: number) => void;
  onRecord: () => void;
}) {
  if (rate !== null) {
    return (
      <Typography.Text type="secondary">
        折算率 {currency} → CNY：{rate}（在「折算率」页可改）
      </Typography.Text>
    );
  }
  return (
    <Flex gap={8} align="center" wrap>
      <Typography.Text type="warning">还没有 {currency} → CNY 的折算率</Typography.Text>
      <InputNumber
        data-testid="platform-fx-rate"
        addonBefore={`1 ${currency} =`}
        addonAfter="CNY"
        min={0}
        value={draft ?? undefined}
        onChange={(value) => onDraft(Number(value ?? 0))}
      />
      <Button
        size="small"
        loading={recording}
        data-testid="platform-fx-record"
        onClick={onRecord}
      >
        记录折算率
      </Button>
    </Flex>
  );
}
