// Data Inspector (INSP-01…03, 06, 07, 15): every type at once, editable in place.
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { useEffect, useRef, useState } from "react";
import { cacheFor } from "../../grid/cache";
import { api, type InspectRow, type InspectType } from "../../ipc/api";
import { hexN } from "../../lib/format";
import * as A from "../../state/actions";
import { useSettings } from "../../state/settings";
import type { Tab } from "../../state/tabs";
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
  const rgba = [0, 1, 2, 3].map((i) => cache.byte(tab.cursor + i));
  const rgbaOk = rgba.every((x) => x !== undefined);
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
        {rows.map((r) =>
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
      <div className="ph">
        <span className="cap">Цвет · RGBA</span>
      </div>
      <div className="color">
        <span className="swatch">
          <span
            style={{
              background: rgbaOk
                ? `rgba(${rgba[0]}, ${rgba[1]}, ${rgba[2]}, ${(rgba[3] ?? 255) / 255})`
                : "transparent",
            }}
          />
        </span>
        <span className="mono">
          {rgbaOk ? `#${rgba.map((x) => hexN(x ?? 0, 2)).join("")}` : "—"}
        </span>
      </div>
    </>
  );
}
