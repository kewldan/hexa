// Search results list (SRCH-17), virtualised.
import { useEffect, useRef, useState } from "react";
import { cacheFor } from "../../grid/cache";
import { fmtSize, groupDec, hex2, hexN } from "../../lib/format";
import * as A from "../../state/actions";
import type { Tab } from "../../state/tabs";

const ROW = 34;

export function ResultsPanel({ tab }: { tab: Tab }) {
  const r = tab.results;
  const box = useRef<HTMLDivElement>(null);
  const [scroll, setScroll] = useState(0);
  const [height, setHeight] = useState(400);
  const [, setTick] = useState(0);
  const cache = cacheFor(tab.id);
  useEffect(() => cache.subscribe(() => setTick((x) => x + 1)), [cache]);
  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setHeight(el.clientHeight));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // Keep the current result visible.
  useEffect(() => {
    const el = box.current;
    if (!el || !r || r.current < 0) return;
    const y = r.current * ROW;
    if (y < el.scrollTop || y > el.scrollTop + el.clientHeight - ROW)
      el.scrollTop = y - el.clientHeight / 2;
  }, [r?.current, r]);

  if (!r) {
    return (
      <div className="empty">
        <b>Нет результатов</b>
        <span>Откройте поиск (⌘F) — все совпадения появятся здесь, на миникарте и в сетке.</span>
      </div>
    );
  }
  const first = Math.max(0, Math.floor(scroll / ROW) - 4);
  const last = Math.min(r.matches.length, first + Math.ceil(height / ROW) + 8);
  const visible = r.matches.slice(first, last);
  for (const m of visible) cache.ensure(Math.max(0, m.offset - 4), m.offset + 12);

  return (
    <>
      <div className="ph">
        <span className="cap">Поиск</span>
        <span className="tag">{r.label}</span>
        <div className="grow" />
        <span className="tag">
          {groupDec(r.matches.length)}
          {r.truncated ? "+" : ""}
        </span>
        <button type="button" className="chip" onClick={A.clearResults}>
          Сбросить
        </button>
      </div>
      {r.running && (
        <div className="progress" style={{ width: "100%" }}>
          <span style={{ width: `${(r.done / Math.max(1, r.total)) * 100}%` }} />
        </div>
      )}
      <div
        ref={box}
        style={{ flex: "1 1 auto", overflow: "auto", minHeight: 0 }}
        onScroll={(e) => setScroll(e.currentTarget.scrollTop)}
      >
        <div style={{ height: r.matches.length * ROW, position: "relative" }}>
          {visible.map((m, i) => {
            const idx = first + i;
            const ctx: string[] = [];
            for (let k = 0; k < Math.min(m.len, 6); k++) {
              const b = cache.byte(m.offset + k);
              ctx.push(b === undefined ? "··" : hex2(b));
            }
            return (
              <button
                type="button"
                key={m.offset}
                className={`li${idx === r.current ? " cur" : ""}`}
                style={{
                  position: "absolute",
                  top: idx * ROW,
                  left: 0,
                  right: 0,
                  height: ROW - 2,
                  animation: "none",
                }}
                onClick={() => A.showMatch(m)}
              >
                <span className="v">{idx + 1}</span>
                <span className="t">
                  <b className="mono">0x{hexN(m.offset, 8)}</b>
                  <span>
                    {ctx.join(" ")}
                    {m.len > 6 ? " …" : ""}
                  </span>
                </span>
                <span className="v">{fmtSize(m.len)}</span>
              </button>
            );
          })}
        </div>
      </div>
      {r.truncated && <div className="note">Показаны первые 100 000 совпадений.</div>}
    </>
  );
}
