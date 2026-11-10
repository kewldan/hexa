// Data Inspector (INSP-01…03, 06, 07, 15): every type at once, editable in place.
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { useEffect, useRef, useState } from "react";
import { cacheFor } from "../../grid/cache";
import { api, type ExtRow, type InspectRow, type InspectType } from "../../ipc/api";
import { hexN } from "../../lib/format";
import * as A from "../../state/actions";
import { useSettings } from "../../state/settings";
import { selection, type Tab, useTabs } from "../../state/tabs";
import { toast } from "../../state/ui";

const SHORT: Partial<Record<InspectType, string>> = {
  binary: "bin",
  octal: "oct",
  hex: "hex",
};

export function InspectorPanel({ tab }: { tab: Tab }) {
  const endian = useSettings((s) => s.s.inspectorEndian);
  const set = useSettings((s) => s.set);
  const [rows, setRows] = useState<InspectRow[]>([]);
  const hidden = useSettings((st) => st.s.inspectorHidden);
  const [editing, setEditing] = useState<InspectType | null>(null);
  const [draft, setDraft] = useState("");
  const [bad, setBad] = useState(false);
  const prev = useRef<Map<InspectType, string | null>>(new Map());
  const [flashed, setFlashed] = useState<Set<InspectType>>(new Set());
  const cache = cacheFor(tab.id);
  const [, setTick] = useState(0);
  useEffect(() => cache.subscribe(() => setTick((x) => x + 1)), [cache]);
  useEffect(() => cache.ensure(tab.cursor, tab.cursor + 16), [cache, tab.cursor]);

  useEffect(() => {
    let alive = true;
    api
      .inspect(tab.id, tab.cursor, endian)
      .then((r) => {
        if (!alive) return;
        const changed = new Set<InspectType>();
        for (const row of r) {
          if (prev.current.has(row.ty) && prev.current.get(row.ty) !== row.value)
            changed.add(row.ty);
          prev.current.set(row.ty, row.value);
        }
        setFlashed(changed);
        setRows(r);
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [tab.id, tab.cursor, endian, tab.info.version]);

  const byte = cache.byte(tab.cursor);
  const readOnly = tab.info.mode === "readOnly";

  const commit = async () => {
    if (!editing) return;
    const ok = await A.inspectorSet(editing, draft);
    setBad(!ok);
    if (ok) setEditing(null);
  };

  const toggleBit = (bit: number) => {
    if (byte === undefined || readOnly) return;
    A.edit({ op: "overwrite", pos: tab.cursor, bytes: [byte ^ (1 << bit)] });
  };

  const idx = endian === "little" ? 0 : 1;
  return (
    <>
      <div className="ph">
        <span className="cap">Инспектор</span>
        <div className="grow" />
        <div className="seg sm">
          <span
            className="thumb"
            style={{ width: "calc(50% - 2px)", transform: `translateX(${idx * 100}%)` }}
          />
          <button
            type="button"
            className={endian === "little" ? "on" : ""}
            onClick={() => set({ inspectorEndian: "little" })}
          >
            LE
          </button>
          <button
            type="button"
            className={endian === "big" ? "on" : ""}
            onClick={() => set({ inspectorEndian: "big" })}
          >
            BE
          </button>
        </div>
      </div>
      <div className="crumb">
        <span className="mono">0x{hexN(tab.cursor, 8)}</span>
        <span>
          {tab.cursor >= tab.info.len
            ? "конец файла"
            : `байт ${byte === undefined ? "…" : `0x${hexN(byte, 2)}`}`}
          {readOnly ? "" : " · двойной клик — изменить"}
        </span>
      </div>
      <div className="insp">
        {rows
          .filter((r) => !hidden.includes(r.ty))
          .map((r) =>
            editing === r.ty ? (
              <div key={r.ty} className="ir">
                <span className="ik">{SHORT[r.ty] ?? r.label}</span>
                <input
                  className={bad ? "bad" : ""}
                  // biome-ignore lint/a11y/noAutofocus: editing starts on double click
                  autoFocus
                  value={draft}
                  onChange={(e) => {
                    setDraft(e.target.value);
                    setBad(false);
                  }}
                  onKeyDown={(e) => {
                    e.stopPropagation();
                    if (e.key === "Enter") commit();
                    if (e.key === "Escape") setEditing(null);
                  }}
                  onBlur={() => setEditing(null)}
                  spellCheck={false}
                />
              </div>
            ) : (
              <button
                type="button"
                key={r.ty}
                className={`ir${r.value === null ? " na" : ""}${flashed.has(r.ty) ? " fl" : ""}`}
                title="Клик — скопировать, двойной клик — изменить"
                onClick={() =>
                  r.value &&
                  writeText(r.value).then(() => toast(`${r.label}: ${r.value} скопировано`))
                }
                onDoubleClick={() => {
                  if (readOnly || tab.cursor >= tab.info.len) return;
                  setEditing(r.ty);
                  setBad(false);
                  const v = r.value ?? "";
                  setDraft(v.startsWith("'") ? v.slice(1, v.indexOf("'", 1)) : v);
                }}
              >
                <span className="ik">{SHORT[r.ty] ?? r.label}</span>
                <span className="iv">{r.value ?? "—"}</span>
              </button>
            ),
          )}
      </div>
      <div className="ph">
        <span className="cap">Биты</span>
        <div className="grow" />
        <span className="tag">0x{hexN(tab.cursor, 8)}</span>
      </div>
      <div className="bits">
        {[7, 6, 5, 4, 3, 2, 1, 0].map((bit) => {
          const on = byte !== undefined && (byte >> bit) & 1;
          return (
            <button
              type="button"
              key={bit}
              className={`bit${on ? " on" : ""}`}
              aria-label={`Бит ${bit}`}
              aria-pressed={!!on}
              disabled={readOnly || byte === undefined}
              onClick={() => toggleBit(bit)}
            >
              {on ? 1 : 0}
            </button>
          );
        })}
      </div>
      <div className="bitn" aria-hidden="true">
        {[7, 6, 5, 4, 3, 2, 1, 0].map((b) => (
          <span key={b}>{b}</span>
        ))}
      </div>
      <ExtendedRows tab={tab} />
    </>
  );
}

/** Dates, GUIDs, network, varints, strings, BCD, Q-format, colours (INSP-04…13),
 * the selection as a whole (INSP-19), row configuration (INSP-17) and watches (INSP-20). */
function ExtendedRows({ tab }: { tab: Tab }) {
  const endian = useSettings((st) => st.s.inspectorEndian);
  const hidden = useSettings((st) => st.s.inspectorHidden);
  const q = useSettings((st) => st.s.qFormat);
  const set = useSettings((st) => st.set);
  const [ext, setExt] = useState<ExtRow[]>([]);
  const [configure, setConfigure] = useState(false);
  const [open, setOpen] = useState<Record<string, boolean>>({ "Дата и время": true });
  const sel = selection(tab);
  const selLen = sel.end - sel.start;
  const useSel = selLen > 1 && selLen <= 4096;
  const pos = useSel ? sel.start : tab.cursor;

  useEffect(() => {
    let alive = true;
    api
      .inspectExt(tab.id, pos, useSel ? selLen : 64, endian, q)
      .then((r) => alive && setExt(r))
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [tab.id, pos, selLen, useSel, endian, q, tab.info.version]);

  const groups = [...new Set(ext.map((r) => r.group))];
  const toggleHidden = (key: string) =>
    set({
      inspectorHidden: hidden.includes(key) ? hidden.filter((k) => k !== key) : [...hidden, key],
    });
  const watch = (ty: string) =>
    useTabs.getState().update(tab.id, (t) => ({ watches: [...t.watches, { offset: pos, ty }] }));

  return (
    <>
      <div className="ph">
        <span className="cap">{useSel ? `Выделение · ${selLen} Б` : "Ещё типы"}</span>
        <div className="grow" />
        <button
          type="button"
          className={`chip${configure ? " on" : ""}`}
          onClick={() => setConfigure(!configure)}
        >
          Настроить
        </button>
      </div>
      {configure && (
        <div className="note" style={{ padding: "0 8px" }}>
          Снимите отметку, чтобы скрыть строку. Q-формат:{" "}
          <input
            className="sel-input"
            style={{ width: 44 }}
            value={q[0]}
            onChange={(e) => set({ qFormat: [Number(e.target.value) || 0, q[1]] })}
          />
          .
          <input
            className="sel-input"
            style={{ width: 44 }}
            value={q[1]}
            onChange={(e) => set({ qFormat: [q[0], Number(e.target.value) || 0] })}
          />
        </div>
      )}
      {groups.map((g) => (
        <div key={g}>
          <button
            type="button"
            className="pgh"
            style={{ width: "100%", textAlign: "left" }}
            onClick={() => setOpen({ ...open, [g]: !open[g] })}
          >
            {open[g] ? "▾" : "▸"} {g}
          </button>
          {open[g] &&
            ext
              .filter((r) => r.group === g && (configure || !hidden.includes(r.key)))
              .map((r) => (
                <div
                  key={r.key}
                  className={`ir${r.value === null ? " na" : ""}`}
                  style={{
                    gridTemplateColumns: configure
                      ? "16px 92px minmax(0,1fr)"
                      : "92px minmax(0,1fr) 18px",
                  }}
                >
                  {configure && (
                    <input
                      type="checkbox"
                      checked={!hidden.includes(r.key)}
                      onChange={() => toggleHidden(r.key)}
                      aria-label={r.label}
                    />
                  )}
                  <span className="ik">{r.label}</span>
                  <button
                    type="button"
                    className="iv"
                    style={{ textAlign: "left", display: "flex", gap: 6, alignItems: "center" }}
                    title="Нажмите, чтобы скопировать"
                    onClick={() =>
                      r.value && writeText(r.value).then(() => toast(`${r.label} скопировано`))
                    }
                  >
                    {r.swatch && (
                      <span className="swatch" style={{ width: 14, height: 14, flex: "none" }}>
                        <span style={{ background: r.swatch }} />
                      </span>
                    )}
                    {r.value ?? "—"}
                  </button>
                  {!configure && r.value !== null && (
                    <button
                      type="button"
                      className="ib"
                      style={{ width: 18, height: 18 }}
                      title="Наблюдать за значением по этому адресу"
                      onClick={() => watch(r.key)}
                    >
                      ◎
                    </button>
                  )}
                </div>
              ))}
        </div>
      ))}
      <Watches tab={tab} />
    </>
  );
}

/** Values pinned to addresses, re-read after every edit (INSP-20). */
function Watches({ tab }: { tab: Tab }) {
  const endian = useSettings((st) => st.s.inspectorEndian);
  const q = useSettings((st) => st.s.qFormat);
  const [values, setValues] = useState<string[]>([]);
  useEffect(() => {
    let alive = true;
    Promise.all(
      tab.watches.map(async (w) => {
        const core = await api.inspect(tab.id, w.offset, endian).catch(() => []);
        const hit = core.find((r) => r.ty === w.ty);
        if (hit) return hit.value ?? "—";
        const ext = await api.inspectExt(tab.id, w.offset, 64, endian, q).catch(() => []);
        return ext.find((r) => r.key === w.ty)?.value ?? "—";
      }),
    ).then((v) => alive && setValues(v));
    return () => {
      alive = false;
    };
  }, [tab.watches, tab.id, tab.info.version, endian, q]);
  if (!tab.watches.length) return null;
  return (
    <>
      <div className="ph">
        <span className="cap">Наблюдение</span>
      </div>
      {tab.watches.map((w, i) => (
        <div
          key={`${w.offset}-${w.ty}`}
          className="ir"
          style={{ gridTemplateColumns: "92px minmax(0,1fr) 18px" }}
        >
          <button
            type="button"
            className="ik"
            style={{ textAlign: "left" }}
            onClick={() => A.jumpTo(w.offset)}
          >
            0x{hexN(w.offset)} · {w.ty}
          </button>
          <span className="iv">{values[i] ?? "…"}</span>
          <button
            type="button"
            className="ib"
            style={{ width: 18, height: 18 }}
            aria-label="Убрать"
            onClick={() =>
              useTabs
                .getState()
                .update(tab.id, (t) => ({ watches: t.watches.filter((_, j) => j !== i) }))
            }
          >
            ×
          </button>
        </div>
      ))}
    </>
  );
}
