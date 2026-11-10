// Summary and navigation of a comparison (DIFF-05, DIFF-08, DIFF-09, DIFF-11).
import { fmtSize, hexN } from "../lib/format";
import {
  applyRegion,
  DIFF_COLORS,
  exportDiff,
  KIND_LABEL,
  nextRegion,
  rerunDiff,
  stopDiff,
  useDiff,
} from "../state/diff";
import { useUi } from "../state/ui";
import { Icon } from "./icons";

export function DiffBar() {
  const d = useDiff();
  const st = d.result?.stats;
  const cur = d.current >= 0 ? d.result?.regions[d.current] : undefined;
  return (
    <div className="diff-bar" role="toolbar" aria-label="Сравнение">
      <span className="cap">Сравнение</span>
      {d.running ? (
        <span className="note">
          считаю… {Math.round(d.progress * 100)}%
          <span className="prog" style={{ ["--p" as string]: `${d.progress * 100}%` }} />
        </span>
      ) : st ? (
        <>
          <b className="pct">
            {st.matchPercent.toFixed(st.matchPercent >= 99.95 && st.differences ? 2 : 1)}%
          </b>
          <span className="note">совпадает</span>
          {(["changed", "inserted", "deleted"] as const).map((k) => (
            <span
              key={k}
              className="diff-k"
              title={`${fmtSize(st[k].bytesA)} → ${fmtSize(st[k].bytesB)}`}
            >
              <i style={{ background: DIFF_COLORS[k] }} />
              {st[k].count} {KIND_LABEL[k]}
            </span>
          ))}
          {d.result?.truncated && <span className="tag">список обрезан</span>}
        </>
      ) : null}
      <div className="grow" />
      {cur && (
        <span
          className="note mono"
          title={`A 0x${hexN(cur.a.start)}+${cur.a.len} · B 0x${hexN(cur.b.start)}+${cur.b.len}`}
        >
          {d.current + 1}/{d.result?.regions.length}
        </span>
      )}
      <button
        type="button"
        className="ib"
        aria-label="Предыдущее отличие"
        title="Предыдущее (⇧F7)"
        onClick={() => nextRegion(-1)}
      >
        ↑
      </button>
      <button
        type="button"
        className="ib"
        aria-label="Следующее отличие"
        title="Следующее (F7)"
        onClick={() => nextRegion(1)}
      >
        ↓
      </button>
      <button
        type="button"
        className="chip"
        title="Скопировать отличие справа налево"
        onClick={() => applyRegion(true)}
        disabled={!cur}
      >
        ← взять
      </button>
      <button
        type="button"
        className="chip"
        title="Скопировать отличие слева направо"
        onClick={() => applyRegion(false)}
        disabled={!cur}
      >
        отдать →
      </button>
      <select
        className="sel-input"
        aria-label="Алгоритм"
        value={d.opts.mode}
        onChange={(e) => {
          d.set({ opts: { ...d.opts, mode: e.target.value as "bytewise" | "shiftAware" } });
          rerunDiff();
        }}
      >
        <option value="shiftAware">со сдвигами</option>
        <option value="bytewise">побайтно</option>
      </select>
      <button
        type="button"
        className="ib"
        aria-label="Пересчитать"
        title="Пересчитать"
        onClick={rerunDiff}
      >
        <Icon.refresh />
      </button>
      <button type="button" className="chip" onClick={exportDiff} disabled={!d.result}>
        Отчёт…
      </button>
      <button
        type="button"
        className="ib"
        aria-label="Закрыть сравнение"
        onClick={() => {
          stopDiff();
          useUi.getState().set({ compare: null });
        }}
      >
        <Icon.close />
      </button>
    </div>
  );
}
