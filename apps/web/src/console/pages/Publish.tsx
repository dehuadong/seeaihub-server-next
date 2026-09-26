import { useState } from 'react';
import type { AdminClient } from '../client';
import { Page } from '../../shared/ui';

/// 发布修订：运营在这里"加一个网关模型"——贴一份发布命令，带上定价与倍率。
///
/// 素材就是发布命令本身（`config/bootstrap/*.json` 的形状），所以这一页不做表单化改写：
/// 合同与候选是**结构化数据**，把它拆成几十个输入框只会让人以为平台在替它做决定。这里只负责
/// 提交、把平台的原话（校验错误逐条）显示出来，并如实说明这一次发布做了什么、没做什么。
export function PublishPage({ client }: { client: AdminClient }) {
  const [text, setText] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<{ gateway_model: string; runtime_revision_id: string } | null>(null);

  async function submit() {
    setBusy(true);
    setError(null);
    setDone(null);
    try {
      const command: unknown = JSON.parse(text);
      const published = await client.publishRevision(command);
      setDone(published);
      setText('');
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Page title="发布修订" hint="发布即原子替换该型号的全部候选" error={error}>
      <div className="panel">
        <p className="muted">
          发布命令的字段：<code>vendor_id</code>、<code>native_model_id</code>、<code>gateway_model</code>（平台对客名，
          省略时取原生名）、<code>native_revision</code>（改价必须换修订号）、<code>capability_schema</code>（合同）、
          <code>offerings</code>（候选数组）、<code>markup_bps</code>（加价系数，基点）、<code>actor</code>。
          按候选的钱是 <code>consumer_rates_cny</code>（四档 CNY 费率向量）、<code>cost_unit_price_microusd</code>
          （按张/按次的成本单价）、<code>reference_cost_microusd</code>（定价参考）、<code>floor_amounts</code>（保底表）。
        </p>
        <p className="muted">
          渠道币种的折算率要先录（见「折算率」页），否则发布期会以"没有生效折算率"拒绝。
        </p>
      </div>

      <div className="panel">
        <textarea
          value={text}
          onChange={(event) => setText(event.target.value)}
          rows={18}
          style={{ width: '100%' }}
          placeholder='{"vendor_id":"OpenAI","native_model_id":"gpt-image-2.5-flare","native_revision":"...","markup_bps":2000,"actor":"ops","offerings":[...]}'
        />
        <div className="row" style={{ marginTop: 8 }}>
          <button type="button" disabled={busy || !text.trim()} onClick={submit}>
            {busy ? '发布中…' : '发布'}
          </button>
          <span className="muted">提交前请先确认合同里的 model.const 等于 native_model_id（发布期会校验）。</span>
        </div>
      </div>

      {done ? (
        <div className="panel">
          <h3>已发布</h3>
          <div className="grid">
            <div className="field">
              <span>网关模型</span>
              <div>{done.gateway_model}</div>
            </div>
            <div className="field">
              <span>生效修订</span>
              <div className="mono">{done.runtime_revision_id}</div>
            </div>
          </div>
          <p className="muted">去「网关模型」页核对候选与定价是否就是你要的那一份。</p>
        </div>
      ) : null}
    </Page>
  );
}
