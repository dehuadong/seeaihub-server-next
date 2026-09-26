import { useState } from 'react';
import type { AdminClient } from '../client';
import type { GatewayModel, GatewayModelCandidate } from '../types';
import { Page, useLoadable } from '../ui';
import { when, yuan } from '../routes';

/// 网关模型清单：运营最常看的一页。
///
/// 它回答"这个平台型号现在是什么状态"：生效修订、加价系数、候选顺序与权重、每条候选能不能走，
/// 以及**这条候选承载得了哪些字段**——"目录里为什么没有它"要在这里看得见。
export function ModelsPage({ client }: { client: AdminClient }) {
  const models = useLoadable(() => client.gatewayModels(), [client]);
  const [busy, setBusy] = useState<string | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const [open, setOpen] = useState<string | null>(null);

  async function toggleModel(model: GatewayModel) {
    setBusy(model.gateway_model);
    setFailure(null);
    try {
      await client.setGatewayModelEnabled(model.gateway_model, !model.enabled);
      models.reload();
    } catch (error) {
      setFailure(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(null);
    }
  }

  const list = models.data?.gateway_models ?? [];

  return (
    <Page
      title="网关模型"
      hint={list.length > 0 ? `${list.length} 个型号` : undefined}
      error={failure ?? models.error}
      loading={models.loading}
      onReload={models.reload}
    >
      {list.length === 0 && !models.loading ? (
        <p className="muted">
          还没有发布过任何网关模型。去「发布修订」贴一份发布素材（<code>config/bootstrap/*.json</code>）。
        </p>
      ) : null}

      {list.map((model) => (
        <div className="panel" key={model.gateway_model}>
          <div className="row" style={{ justifyContent: 'space-between' }}>
            <h3>
              {model.gateway_model}{' '}
              <span className={model.enabled ? 'tag ok' : 'tag off'}>
                {model.enabled ? '启用' : '停用'}
              </span>
            </h3>
            <div className="row">
              <button type="button" disabled={busy === model.gateway_model} onClick={() => toggleModel(model)}>
                {model.enabled ? '停用' : '启用'}
              </button>
              <button
                type="button"
                onClick={() => setOpen(open === model.gateway_model ? null : model.gateway_model)}
              >
                {open === model.gateway_model ? '收起候选' : `候选（${model.candidates.length}）`}
              </button>
            </div>
          </div>

          <div className="grid">
            <Field label="厂商" value={`${model.vendor_id} / ${model.native_model_id}`} />
            <Field label="合同修订" value={model.native_revision} />
            <Field
              label="加价系数"
              value={model.markup_bps === null ? '未声明' : `${model.markup_bps} 基点（×${(1 + model.markup_bps / 10000).toFixed(4)}）`}
            />
            <Field label="生效修订" value={model.runtime_revision_id} mono />
            <Field label="发布时间" value={when(model.published_at)} />
          </div>

          {open === model.gateway_model ? (
            <table style={{ marginTop: 12 }}>
              <thead>
                <tr>
                  <th>档位</th>
                  <th>权重</th>
                  <th>渠道</th>
                  <th>渠道模型</th>
                  <th>驱动器</th>
                  <th>成本（原币种）</th>
                  <th>对客费率（CNY/1M）</th>
                  <th>可走</th>
                  <th />
                </tr>
              </thead>
              <tbody>
                {model.candidates.map((candidate) => (
                  <CandidateRow
                    key={candidate.offering_id}
                    client={client}
                    candidate={candidate}
                    onChanged={models.reload}
                    onError={setFailure}
                  />
                ))}
              </tbody>
            </table>
          ) : null}
        </div>
      ))}
    </Page>
  );
}

function CandidateRow(props: {
  client: AdminClient;
  candidate: GatewayModelCandidate;
  onChanged: () => void;
  onError: (message: string) => void;
}) {
  const { candidate } = props;
  const [busy, setBusy] = useState(false);
  const rates = candidate.consumer_rates_cny;

  async function toggle() {
    setBusy(true);
    try {
      await props.client.setOfferingEnabled(candidate.offering_id, !candidate.enabled);
      props.onChanged();
    } catch (error) {
      props.onError(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(false);
    }
  }

  return (
    <tr>
      <td>{candidate.routing_priority}</td>
      <td>{candidate.weight}</td>
      <td>{candidate.provider_kind}</td>
      <td className="mono">{candidate.provider_model_id}</td>
      <td className="mono">{candidate.adapter_key}</td>
      <td>
        {candidate.reference_cost_microusd === null
          ? '—'
          : `${yuan(candidate.reference_cost_microusd)} ${candidate.cost_currency ?? ''}（${candidate.cost_basis ?? '—'}）`}
      </td>
      <td>
        {rates
          ? `文入 ${yuan(rates.text_input_micros_per_million)} / 图入 ${yuan(rates.image_input_micros_per_million)} / 文出 ${yuan(
              rates.text_output_micros_per_million,
            )} / 图出 ${yuan(rates.image_output_micros_per_million)}`
          : '无向量（按成本单价 × 倍率算）'}
      </td>
      <td>
        <span className={candidate.enabled ? 'tag ok' : 'tag off'}>
          {candidate.enabled ? '可走' : '不可走'}
        </span>
      </td>
      <td>
        <button type="button" disabled={busy} onClick={toggle}>
          {candidate.enabled ? '停用' : '启用'}
        </button>
      </td>
    </tr>
  );
}

function Field(props: { label: string; value: string; mono?: boolean }) {
  return (
    <div className="field">
      <span>{props.label}</span>
      <div className={props.mono ? 'mono' : undefined}>{props.value}</div>
    </div>
  );
}
