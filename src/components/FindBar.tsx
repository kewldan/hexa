// Find bar (SRCH-01…05, 13…17): incremental, background, cancellable.
import { useEffect, useMemo, useRef, useState } from "react";
import { api, type NumType, type Query, type Replacement, type SeqItem } from "../ipc/api";
import { groupDec, parseHexBytes } from "../lib/format";
import * as A from "../state/actions";
import { useSettings } from "../state/settings";
import { selection, type Tab, useTabs } from "../state/tabs";
import { openDialog, toast, useUi } from "../state/ui";
import { Icon } from "./icons";

type Kind = "hex" | "text" | "regex" | "number" | "seq";
const KINDS: { id: Kind; label: string }[] = [
  { id: "hex", label: "Hex" },
  { id: "text", label: "Текст" },
  { id: "regex", label: "Regex" },
  { id: "number", label: "Число" },
  { id: "seq", label: "Посл." },
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
  const [replaceOpen, setReplaceOpen] = useState(false);
  const [repl, setRepl] = useState("");

  const replacementBytes = async (): Promise<Replacement | null> => {
    if (kind === "regex") {
      const bytes: number[] = [];
      const src = repl.replace(/\\x([0-9a-f]{2})/gi, (_, h) =>
        String.fromCharCode(Number.parseInt(h, 16)),
      );
      for (const ch of src) {
        const c = ch.codePointAt(0) ?? 0;
        if (c < 0x100) bytes.push(c);
        else bytes.push(...new TextEncoder().encode(ch));
      }
      return { kind: "template", template: bytes };
    }
    if (kind === "text") {
      const enc = encs[0] ?? "ascii";
      return { kind: "bytes", bytes: await api.encodeText(repl, enc) };
    }
    const b = repl.trim() === "" ? [] : parseHexBytes(repl);
    if (!b) {
      toast("Замена: ожидаются hex-байты", true);
      return null;
    }
    return { kind: "bytes", bytes: b };
  };
  const scopeNow = () =>
    inSel || align > 1
      ? { start: inSel?.start ?? 0, end: inSel?.end ?? tab.info.len, align }
      : null;
  const replaceOne = async () => {
    if (!query) return;
    const r = await replacementBytes();
    if (!r) return;
    try {
      const sel = selection(tab);
      const plan = await api.replacePlan(
        tab.id,
        query,
        { start: sel.start, end: Math.max(sel.end, sel.start), align: 1 },
        r,
        1,
      );
      if (!plan.length || plan[0].offset !== sel.start) {
        A.stepResult(1);
        return;
      }
      const res = await api.replaceApply(tab.id, plan);
      A.applyResult(tab.id, res, plan[0].offset + plan[0].new.length);
      A.stepResult(1);
    } catch (e) {
      A.report(e);
    }
  };
  const replaceAll = async () => {
    if (!query) return;
    const r = await replacementBytes();
    if (!r) return;
    try {
      const plan = await api.replacePlan(tab.id, query, scopeNow(), r, 100_000);
      if (!plan.length) return toast("Нечего заменять", true);
      openDialog({ kind: "replacePreview", plan });
    } catch (e) {
      A.report(e);
    }
  };
  // SRCH-13/14: limit to the selection captured when the toggle was switched on.
  const [inSel, setInSel] = useState<{ start: number; end: number } | null>(null);
  const [align, setAlign] = useState(1);
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
      case "seq":
        return parseSequence(text);
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
        const scope =
          inSel || align > 1
            ? { start: inSel?.start ?? 0, end: inSel?.end ?? tab.info.len, align }
            : null;
        await A.findAll(query, label, scope);
      } catch (e) {
        setError(String(e));
      }
    }, 220);
    return () => clearTimeout(timer);
  }, [query, kind, q, inSel, align]);

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

  const remember = () => {
    const h = settings.queryHistory.filter((x) => x !== q);
    setSettings({ queryHistory: [q, ...h].slice(0, 30) });
  };
  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === "Enter") {
      e.preventDefault();
      if (q.trim()) remember();
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
          : kind === "seq"
            ? "u32le=0x10, u16le=1..5, u8=*"
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
          list="hx-query-history"
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
        <button
          type="button"
          className={`tog${inSel ? " on" : ""}`}
          title="Искать только в выделении"
          onClick={() => {
            if (inSel) return setInSel(null);
            const s = selection(tab);
            if (s.end > s.start) setInSel(s);
          }}
        >
          {inSel ? `в выделении · ${inSel.end - inSel.start} Б` : "в выделении"}
        </button>
        <select
          className="sel-input"
          value={align}
          onChange={(e) => setAlign(Number(e.target.value))}
          aria-label="Выравнивание"
          title="Только смещения, кратные N"
        >
          {[1, 2, 4, 8, 16].map((a) => (
            <option key={a} value={a}>
              {a === 1 ? "любое смещение" : `кратно ${a}`}
            </option>
          ))}
        </select>
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
      <button
        type="button"
        className={`tog${replaceOpen ? " on" : ""}`}
        title="Замена (SRCH-20)"
        onClick={() => setReplaceOpen(!replaceOpen)}
      >
        Замена
      </button>
      <button
        type="button"
        className="tog"
        title="Сохранить запрос"
        disabled={!q.trim()}
        onClick={() => {
          setSettings({
            savedQueries: [
              ...settings.savedQueries.filter((x) => x.text !== q),
              { label: q, kind, text: q },
            ],
          });
          toast("Запрос сохранён");
        }}
      >
        ★
      </button>
      {settings.savedQueries.length > 0 && (
        <select
          className="sel-input"
          value=""
          aria-label="Сохранённые запросы"
          onChange={(e) => {
            const sq = settings.savedQueries[Number(e.target.value)];
            if (sq) {
              setKind(sq.kind as Kind);
              setQ(sq.text);
            }
          }}
        >
          <option value="">Сохранённые…</option>
          {settings.savedQueries.map((x, i) => (
            <option key={`${x.kind}-${x.text}`} value={i}>
              {x.kind}: {x.label}
            </option>
          ))}
        </select>
      )}
      <span className="fhint">↵ далее · ⇧↵ назад · esc закрыть</span>
      <button type="button" className="ib" aria-label="Закрыть поиск" onClick={close}>
        <Icon.close />
      </button>
      <datalist id="hx-query-history">
        {settings.queryHistory.map((h) => (
          <option key={h} value={h} />
        ))}
      </datalist>
      {replaceOpen && (
        <div className="find-replace">
          <label className="fld">
            <span className="vh">Заменить на</span>
            <input
              value={repl}
              onChange={(e) => setRepl(e.target.value)}
              onKeyDown={(e) => {
                e.stopPropagation();
                if (e.key === "Enter") replaceOne();
              }}
              placeholder={
                kind === "regex"
                  ? "Шаблон: $1 — группа, \\xNN — байт"
                  : kind === "text"
                    ? "Текст замены"
                    : "Hex-байты замены"
              }
              spellCheck={false}
            />
          </label>
          <button type="button" className="btn" onClick={replaceOne} disabled={!query}>
            Заменить
          </button>
          <button type="button" className="btn pri" onClick={replaceAll} disabled={!query}>
            Заменить всё…
          </button>
        </div>
      )}
    </div>
  );
}

/** "u32le=0x10, u16be=1..5, u8=*" → sequence query (SRCH-06). */
export function parseSequence(text: string): Query | null {
  const items: SeqItem[] = [];
  for (const part of text
    .split(",")
    .map((x) => x.trim())
    .filter(Boolean)) {
    const m = /^([iuf])(8|16|32|64)(le|be)?\s*(?:=\s*(.+))?$/i.exec(part);
    if (!m) return null;
    const ty = `${m[1].toLowerCase()}${m[2]}` as NumType;
    if (!["i8", "u8", "i16", "u16", "i32", "u32", "i64", "u64", "f32", "f64"].includes(ty))
      return null;
    const v = (m[4] ?? "*").trim();
    const [lo, hi] = v.split(/\s*(?:\.\.|…)\s*/);
    items.push({
      ty,
      bigEndian: (m[3] ?? "le").toLowerCase() === "be",
      value: lo === "*" ? null : lo,
      max: hi ?? null,
      tolerance: null,
    });
  }
  return items.length ? { kind: "sequence", items } : null;
}

export function useResultsOf(id: number) {
  return useTabs((s) => s.tabs.find((t) => t.id === id)?.results ?? null);
}
