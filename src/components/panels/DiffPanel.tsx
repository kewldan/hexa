// List of differences of the running comparison (DIFF-05, DIFF-08).
import { useMemo, useState } from "react";
import { fmtSize, hexN } from "../../lib/format";
import { DIFF_COLORS, gotoRegion, KIND_LABEL, useDiff } from "../../state/diff";
import type { Tab } from "../../state/tabs";
import { openDialog } from "../../state/ui";
import { Icon } from "../icons";

const PAGE = 500;

export function DiffPanel({ tab }: { tab: Tab }) {
  const d = useDiff();
  const [limit, setLimit] = useState(PAGE);
  const [kind, setKind] = useState<"" | "changed" | "inserted" | "deleted">("");
  const list = useMemo(() => {
    const all = d.result?.regions ?? [];
    return all.map((r, i) => ({ r, i })).filter(({ r }) => !kind || r.kind === kind);
  }, [d.result, kind]);
  const active = d.a === tab.id || d.b === tab.id;
  return (
    <>
      <div className="ph">
        <span className="cap">Отличия</span>
        {active && d.result && <span className="tag">{d.result.regions.length}</span>}
        <div className="grow" />
        <button
          type="button"
          className="chip"
          onClick={() => openDialog({ kind: "pickTab", mode: "diff" })}
        >
          <Icon.split /> Сравнить…
        </button>
      </div>
      <div style={{ display: "flex", flexWrap: "wrap", gap: 6, padding: "0 4px" }}>
        <button type="button" className="chip" onClick={() => openDialog({ kind: "nway" })}>
          Несколько
        </button>
        <button type="button" className="chip" onClick={() => openDialog({ kind: "merge3" })}>
          Слияние
        </button>
        <button type="button" className="chip" onClick={() => openDialog({ kind: "patchCreate" })}>
          Патч…
        </button>
        <button type="button" className="chip" onClick={() => openDialog({ kind: "patchApply" })}>
          Применить патч…
        </button>
      </div>
      {!active || !d.result ? (
        <div className="empty">
          <span>
            {d.running
              ? "Сравниваю…"
              : "Сравните вкладку с другим файлом: отличия подсветятся в обоих, прокрутка синхронна. F7 — к следующему отличию."}
          </span>
        </div>
      ) : (
        <>
          <div style={{ display: "flex", gap: 6, padding: "0 4px" }}>
            <select
              className="sel-input"
              aria-label="Тип отличий"
              value={kind}
              onChange={(e) => setKind(e.target.value as typeof kind)}
            >
              <option value="">все типы</option>
              <option value="changed">изменено</option>
              <option value="inserted">вставлено</option>
              <option value="deleted">удалено</option>
            </select>
          </div>
          <div className="list">
            {list.slice(0, limit).map(({ r, i }) => (
              <button
                type="button"
                key={i}
                className={`li${i === d.current ? " cur" : ""}`}
                style={{ gridTemplateColumns: "10px minmax(0,1fr)", textAlign: "left" }}
                onClick={() => gotoRegion(i)}
              >
                <span className="dotc" style={{ background: DIFF_COLORS[r.kind] }} />
                <span className="t">
                  <b>
                    0x{hexN(r.a.start, 8)} ↔ 0x{hexN(r.b.start, 8)}
                  </b>
                  <span>
                    {KIND_LABEL[r.kind]} · {fmtSize(r.a.len)} → {fmtSize(r.b.len)}
                  </span>
                </span>
              </button>
            ))}
            {list.length > limit && (
              <button type="button" className="chip" onClick={() => setLimit(limit + PAGE)}>
                Ещё {Math.min(PAGE, list.length - limit)}
              </button>
            )}
          </div>
        </>
      )}
    </>
  );
}
