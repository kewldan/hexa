// Find bar (SRCH-01…05, 13…17): incremental, background, cancellable.
import { useEffect, useMemo, useRef, useState } from "react";
import type { NumType, Query } from "../ipc/api";
import { groupDec } from "../lib/format";
import * as A from "../state/actions";
import { useSettings } from "../state/settings";
import { type Tab, useTabs } from "../state/tabs";
import { useUi } from "../state/ui";
import { Icon } from "./icons";

type Kind = "hex" | "text" | "regex" | "number";
const KINDS: { id: Kind; label: string }[] = [
  { id: "hex", label: "Hex" },
  { id: "text", label: "Текст" },
  { id: "regex", label: "Regex" },
  { id: "number", label: "Число" },
];
const NUMS: NumType[] = ["u8", "i8", "u16", "i16", "u32", "i32", "u64", "i64", "f32", "f64"];
const TEXT_ENCODINGS = [
  "ascii",
  "utf-8",
  "utf-16le",
  "utf-16be",
  "windows-1251",
  "koi8-r",
  "ibm866",
  "ebcdic-037",
];

export function FindBar({ tab }: { tab: Tab }) {
  const focusSeq = useUi((u) => u.findFocus);
  const settings = useSettings((s) => s.s);
  const setSettings = useSettings((s) => s.set);
  const input = useRef<HTMLInputElement>(null);
  const [kind, setKind] = useState<Kind>("hex");
  const [q, setQ] = useState("");
  const [caseSensitive, setCase] = useState(false);
  const [unicode, setUnicode] = useState(false);
  const [num, setNum] = useState<NumType>("u32");
  const [endian, setEndian] = useState<"little" | "big" | "both">("little");
  const [tolerance, setTolerance] = useState("");
  const [error, setError] = useState("");
  const encs = settings.searchEncodings;

  useEffect(() => {
    if (focusSeq >= 0) {
      input.current?.focus();
      input.current?.select();
    }
  }, [focusSeq]);

  const query: Query | null = useMemo(() => {
    const text = q.trim();
    if (!text) return null;
    switch (kind) {
      case "hex":
        return { kind: "hex", pattern: text };
      case "text":
        return { kind: "text", text: q, encodings: encs.length ? encs : ["ascii"], caseSensitive };
      case "regex":
        return { kind: "regex", pattern: q, unicode };
      case "number": {
        const [lo, hi] = text.split(/\s*(?:\.\.|…)\s*/);
        return {
          kind: "number",
          ty: num,
          endian,
          value: lo,
          max: hi ?? null,
          tolerance: tolerance ? Number.parseFloat(tolerance.replace(",", ".")) : null,
        };
      }
    }
  }, [q, kind, encs, caseSensitive, unicode, num, endian, tolerance]);

  // Incremental search (SRCH-15): all matches in the background, debounced.
  useEffect(() => {
    if (!query) {
      A.clearResults();
      setError("");
      return;
    }
    const timer = setTimeout(async () => {
      setError("");
      const label = kind === "text" ? `«${q}»` : q;
      try {
        await A.findAll(query, label);
      } catch (e) {
        setError(String(e));
      }
    }, 220);
    return () => clearTimeout(timer);
  }, [query, kind, q]);

  // Jump to the first match after the cursor once results arrive.
  const jumped = useRef<string>("");
  const r = tab.results;
  useEffect(() => {
    const key = JSON.stringify(query);
    if (!r || r.matches.length === 0 || jumped.current === key) return;
    jumped.current = key;
    const idx = A.lowerBound(r.matches, tab.cursor);
    A.showMatch(r.matches[idx] ?? r.matches[0]);
    input.current?.focus();
  }, [r, query, tab.cursor]);

  const close = () => {
    useUi.getState().set({ find: false });
    document.querySelector<HTMLElement>("[data-hx-grid]")?.focus();
  };

  const count = !r
    ? ""
    : r.running && r.matches.length === 0
      ? `${Math.round((r.done / Math.max(1, r.total)) * 100)}%…`
      : r.matches.length === 0
        ? "нет совпадений"
        : `${r.current >= 0 ? `${groupDec(r.current + 1)} из ` : ""}${groupDec(r.matches.length)}${r.truncated ? "+" : ""}${r.running ? "…" : ""}`;

  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === "Enter") {
      e.preventDefault();
      A.stepResult(e.shiftKey ? -1 : 1);
    } else if (e.key === "Escape") {
      e.preventDefault();
      close();
    }
    e.stopPropagation();
  };

  const placeholder =
    kind === "hex"
      ? "DE AD ?? 4? {01xx0101}"
      : kind === "text"
        ? "Текст"
        : kind === "regex"
          ? "Регулярное выражение по байтам"
          : "Значение или диапазон 10..20";

  const idx = KINDS.findIndex((k) => k.id === kind);
  return (
    <div className="findbar" role="search">
      <div className="seg">
        <span
          className="thumb"
          style={{
            width: `calc((100% - 4px) / ${KINDS.length})`,
            transform: `translateX(${idx * 100}%)`,
          }}
        />
        {KINDS.map((k) => (
          <button
            type="button"
            key={k.id}
            className={k.id === kind ? "on" : ""}
            onClick={() => setKind(k.id)}
          >
            {k.label}
          </button>
        ))}
      </div>
      <label className={`fld${error ? " bad" : ""}`}>
        <Icon.search />
        <span className="vh">Что искать</span>
        <input
          ref={input}
          value={q}
          onChange={(e) => setQ(e.target.value)}
          onKeyDown={onKey}
          placeholder={placeholder}
          spellCheck={false}
          autoComplete="off"
        />
        {r?.running && (
          <button
            type="button"
            className="ib"
            style={{ width: 20, height: 20 }}
            title="Остановить"
            onClick={A.clearResults}
          >
            <Icon.stop />
          </button>
        )}
      </label>
      <span className="fcount">{count}</span>
      <button
        type="button"
        className="ib"
        aria-label="Предыдущее совпадение"
        onClick={() => A.stepResult(-1)}
      >
        <Icon.up />
      </button>
      <button
        type="button"
        className="ib"
        aria-label="Следующее совпадение"
        onClick={() => A.stepResult(1)}
      >
        <Icon.down />
      </button>
      <div className="fopts">
        {kind === "text" && (
          <>
            <button
              type="button"
              className={`tog${caseSensitive ? " on" : ""}`}
              title="Учитывать регистр"
              onClick={() => setCase(!caseSensitive)}
            >
              Aa
            </button>
            {TEXT_ENCODINGS.map((e) => (
              <button
                type="button"
                key={e}
                className={`tog${encs.includes(e) ? " on" : ""}`}
                title="Искать в этой кодировке (можно несколько сразу)"
                onClick={() =>
                  setSettings({
                    searchEncodings: encs.includes(e) ? encs.filter((x) => x !== e) : [...encs, e],
                  })
                }
              >
                {e}
              </button>
            ))}
          </>
        )}
        {kind === "regex" && (
          <button
            type="button"
            className={`tog${unicode ? " on" : ""}`}
            title="Регулярное выражение по UTF-8 тексту"
            onClick={() => setUnicode(!unicode)}
          >
            UTF-8
          </button>
        )}
        {kind === "number" && (
          <>
            <select
              className="sel-input"
              value={num}
              onChange={(e) => setNum(e.target.value as NumType)}
              aria-label="Тип числа"
            >
              {NUMS.map((n) => (
                <option key={n}>{n}</option>
              ))}
            </select>
            <select
              className="sel-input"
              value={endian}
              onChange={(e) => setEndian(e.target.value as typeof endian)}
              aria-label="Порядок байтов"
            >
              <option value="little">LE</option>
              <option value="big">BE</option>
              <option value="both">LE + BE</option>
            </select>
            {(num === "f32" || num === "f64") && (
              <input
                className="sel-input"
                style={{ width: 80 }}
                placeholder="± допуск"
                value={tolerance}
                onChange={(e) => setTolerance(e.target.value)}
                onKeyDown={(e) => e.stopPropagation()}
              />
            )}
          </>
        )}
      </div>
      <div className="grow" />
      <span className="fhint">↵ далее · ⇧↵ назад · esc закрыть</span>
      <button type="button" className="ib" aria-label="Закрыть поиск" onClick={close}>
        <Icon.close />
      </button>
    </div>
  );
}

export function useResultsOf(id: number) {
  return useTabs((s) => s.tabs.find((t) => t.id === id)?.results ?? null);
}
