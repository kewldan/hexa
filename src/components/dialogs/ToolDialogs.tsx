// Calculator (UI-09), command help (UI-10), clipboard history (EDIT-18),
// printing (IO-06), find in files and tabs (SRCH-23/24), splitting files
// (FILE-13) and shareable view links (IO-08).
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { open as openFile, save as saveFile } from "@tauri-apps/plugin-dialog";
import { Fragment, useEffect, useMemo, useState } from "react";
import { createPortal } from "react-dom";
import { api, type Query } from "../../ipc/api";
import { files } from "../../ipc/files";
import { calc, views } from "../../lib/calc";
import { COMMANDS, keysFor } from "../../lib/commands";
import { fmtSize, hexN, relTime } from "../../lib/format";
import { displayKeys } from "../../lib/keys";
import { captureView, parseView, viewToLink } from "../../lib/viewlink";
import * as A from "../../state/actions";
import { useClipHist } from "../../state/cliphist";
import { hasSelection, selection, useActiveTab, useTabs } from "../../state/tabs";
import { closeDialog, openDialog, toast } from "../../state/ui";
import { Modal } from "./Dialogs";
import "../../styles/tools.css";

const stop = (e: React.KeyboardEvent) => e.stopPropagation();

// ------------------------------------------------------------------ UI-09

export function CalcDialog() {
  const tab = useActiveTab();
  const [src, setSrc] = useState("");
  const [hist, setHist] = useState<{ src: string; v: bigint }[]>([]);
  const res = useMemo((): { v: bigint | null; err: string | null } | null => {
    if (!src.trim()) return null;
    try {
      return { v: calc(src), err: null };
    } catch (e) {
      return { v: null, err: (e as Error).message };
    }
  }, [src]);
  const v = res?.v ?? null;
  const w = v !== null ? views(v) : null;
  const rows: [string, string][] = w
    ? [
        ["Десятичное", w.dec],
        ["Hex", w.hex],
        ["Восьмеричное", w.oct],
        ["Двоичное", w.bin],
        ["u64", w.u64],
        ["i8 · i16", `${w.i8} · ${w.i16}`],
        ["i32 · i64", `${w.i32} · ${w.i64}`],
        ["float32 (биты)", w.f32],
        ["float64 (биты)", w.f64],
        ["Символы", w.chars || "—"],
      ]
    : [];
  return (
    <Modal
      title="Калькулятор"
      footer={
        <>
          <button type="button" className="btn" onClick={closeDialog}>
            Закрыть
          </button>
          <button
            type="button"
            className="btn pri"
            disabled={v === null || !tab || v < 0n}
            onClick={() => {
              closeDialog();
              if (v !== null) A.setCursor(Number(v), false, { jump: true });
            }}
          >
            Перейти к смещению
          </button>
        </>
      }
    >
      <input
        className={`inp mono${res?.err ? " bad" : ""}`}
        aria-label="Выражение"
        placeholder="0x1F * 4 + 0b101 << 2"
        value={src}
        // biome-ignore lint/a11y/noAutofocus: the dialog exists to type here
        autoFocus
        onChange={(e) => setSrc(e.target.value)}
        onKeyDown={(e) => {
          stop(e);
          if (e.key === "Enter" && v !== null) setHist([{ src, v }, ...hist].slice(0, 10));
        }}
      />
      {res?.err && <div className="xf-err">{res.err}</div>}
      {w && (
        <div className="kv calc-kv">
          {rows.map(([k, val]) => (
            <Fragment key={k}>
              <span>{k}</span>
              <button
                type="button"
                className="link mono"
                title="Скопировать"
                onClick={() => writeText(val).then(() => toast("Скопировано"))}
              >
                {val}
              </button>
            </Fragment>
          ))}
        </div>
      )}
      {hist.length > 0 && (
        <div className="list">
          {hist.map((h) => (
            <button
              type="button"
              key={h.src}
              className="str-row"
              style={{ gridTemplateColumns: "minmax(0,1fr) auto" }}
              onClick={() => setSrc(h.src)}
            >
              <span className="tx">{h.src}</span>
              <span className="mono">{views(h.v).hex}</span>
            </button>
          ))}
        </div>
      )}
      <div className="note">
        Литералы: 0x1F, 1Fh, 0b101, 0o17, 1_000. Операции: + − × / % ** &lt;&lt; &gt;&gt; &amp; | ^
        ~. Enter — в историю.
      </div>
    </Modal>
  );
}

// ------------------------------------------------------------------ UI-10

export function HelpDialog() {
  const [q, setQ] = useState("");
  const groups = useMemo(() => {
    const f = q.toLowerCase();
    const list = COMMANDS.filter(
      (c) =>
        !c.hidden &&
        (!f ||
          c.title.toLowerCase().includes(f) ||
          c.id.includes(f) ||
          c.group.toLowerCase().includes(f)),
    );
    const m = new Map<string, typeof list>();
    for (const c of list) m.set(c.group, [...(m.get(c.group) ?? []), c]);
    return [...m.entries()];
  }, [q]);
  return (
    <Modal title="Справка по командам" wide>
      <input
        className="inp"
        placeholder="Найти команду"
        value={q}
        onChange={(e) => setQ(e.target.value)}
        onKeyDown={stop}
        aria-label="Найти команду"
      />
      <div className="help-list">
        {groups.map(([g, cmds]) => (
          <section key={g}>
            <h4>{g}</h4>
            {cmds.map((c) => (
              <button
                type="button"
                key={c.id}
                className="help-row"
                onClick={() => {
                  closeDialog();
                  setTimeout(() => c.run(), 0);
                }}
              >
                <span>
                  <b>{c.title}</b>
                  {c.hint && <span className="note"> — {c.hint}</span>}
                </span>
                <span className="kbd-list">
                  {keysFor(c.id) && <kbd>{displayKeys(keysFor(c.id))}</kbd>}
                </span>
              </button>
            ))}
          </section>
        ))}
      </div>
      <div className="note">
        ⌘K — палитра команд с нечётким поиском; правый клик по сетке — контекстное меню. Любое
        сочетание меняется в настройках.
      </div>
    </Modal>
  );
}

// ------------------------------------------------------------------ EDIT-18

export function ClipHistoryDialog() {
  const entries = useClipHist((s) => s.entries);
  const use = (text: string) => {
    closeDialog();
    setTimeout(() => openDialog({ kind: "paste", text }), 0);
  };
  return (
    <Modal
      title="История буфера обмена"
      footer={
        <>
          <button
            type="button"
            className="btn"
            onClick={() => useClipHist.getState().clear()}
            disabled={!entries.length}
          >
            Очистить
          </button>
          <button type="button" className="btn" onClick={closeDialog}>
            Закрыть
          </button>
        </>
      }
    >
      {entries.length === 0 ? (
        <div className="note">Скопированное в этой сессии появится здесь (до 20 записей).</div>
      ) : (
        <div className="list">
          {entries.map((e, i) => (
            <div
              key={e.id}
              className="li"
              style={{ gridTemplateColumns: "18px minmax(0,1fr) auto", animation: "none" }}
            >
              <span className="note">{i + 1}</span>
              <button
                type="button"
                className="t"
                style={{ textAlign: "left" }}
                onClick={() => use(e.text)}
              >
                <b
                  className="mono"
                  style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}
                >
                  {e.text.slice(0, 120)}
                </b>
                <span>
                  {fmtSize(e.len)} · {e.source} · {relTime(e.time)}
                </span>
              </button>
              <button
                type="button"
                className="ib"
                aria-label="Удалить"
                onClick={() => useClipHist.getState().remove(e.id)}
              >
                ×
              </button>
            </div>
          ))}
        </div>
      )}
    </Modal>
  );
}

// ------------------------------------------------------------------ IO-06

const PRINT_LIMIT = 1 << 20;

export function PrintDialog() {
  const tab = useActiveTab();
  const [width, setWidth] = useState(16);
  const [bytes, setBytes] = useState<Uint8Array | null>(null);
  const [printing, setPrinting] = useState(false);
  const r = tab
    ? hasSelection(tab)
      ? selection(tab)
      : { start: 0, end: Math.min(tab.info.len, PRINT_LIMIT) }
    : null;
  useEffect(() => {
    if (tab && r)
      api
        .read(tab.id, r.start, Math.min(r.end - r.start, PRINT_LIMIT))
        .then((b) => setBytes(new Uint8Array(b)))
        .catch(A.report);
  }, [tab?.id, r?.start, r?.end]);
  if (!tab || !r) return null;
  const lines: string[] = [];
  if (bytes)
    for (let i = 0; i < bytes.length; i += width) {
      const row = Array.from(bytes.slice(i, i + width));
      lines.push(
        `${hexN(r.start + i, 8)}  ${row
          .map((b) => hexN(b, 2))
          .join(" ")
          .padEnd(
            width * 3 - 1,
          )}  ${row.map((b) => (b >= 0x20 && b < 0x7f ? String.fromCharCode(b) : ".")).join("")}`,
      );
    }
  const doPrint = async () => {
    setPrinting(true);
    // The sheet renders into <body>; print CSS hides everything else.
    await new Promise((res) => setTimeout(res, 50));
    try {
      await files.print();
    } catch {
      window.print();
    }
    setTimeout(() => setPrinting(false), 500);
  };
  return (
    <Modal
      title="Печать"
      wide
      footer={
        <>
          <button type="button" className="btn" onClick={closeDialog}>
            Отмена
          </button>
          <button type="button" className="btn pri" onClick={doPrint} disabled={!bytes}>
            Печать…
          </button>
        </>
      }
    >
      <div className="row-f">
        <span>Байт в строке</span>
        <select className="inp" value={width} onChange={(e) => setWidth(Number(e.target.value))}>
          {[8, 16, 24, 32].map((n) => (
            <option key={n}>{n}</option>
          ))}
        </select>
      </div>
      <div className="note">
        {hasSelection(tab) ? "Выделение" : "Начало файла"}: 0x{hexN(r.start)}…0x{hexN(r.end)} ·{" "}
        {fmtSize(r.end - r.start)}
        {r.end - r.start > PRINT_LIMIT && " (печатается первый 1 МБ)"} · {lines.length} строк
      </div>
      <pre className="print-preview">{lines.slice(0, 200).join("\n")}</pre>
      {printing &&
        createPortal(
          <div className="print-sheet">
            <h3>
              {tab.info.name} · 0x{hexN(r.start)}–0x{hexN(r.end)}
            </h3>
            <pre>{lines.join("\n")}</pre>
          </div>,
          document.body,
        )}
    </Modal>
  );
}

// ------------------------------------------------------------------ SRCH-23/24

interface FileHits {
  path: string;
  tabId?: number;
  offsets: number[];
  len: number;
}

function buildQuery(kind: string, text: string): Query {
  if (kind === "hex") return { kind: "hex", pattern: text };
  if (kind === "regex") return { kind: "regex", pattern: text, unicode: false };
  return { kind: "text", text, encodings: ["ascii", "utf-8", "utf-16le"], caseSensitive: false };
}

export function FindInFilesDialog() {
  const tabs = useTabs((s) => s.tabs);
  const [scope, setScope] = useState<"files" | "tabs">("files");
  const [root, setRoot] = useState("");
  const [mask, setMask] = useState("*");
  const [recursive, setRecursive] = useState(true);
  const [kind, setKind] = useState("text");
  const [text, setText] = useState("");
  const [hits, setHits] = useState<FileHits[]>([]);
  const [status, setStatus] = useState("");
  const [job, setJob] = useState<number | null>(null);
  const pick = async () => {
    const p = await openFile({ directory: true, multiple: false });
    if (typeof p === "string") setRoot(p);
  };
  const run = async () => {
    setHits([]);
    const query = buildQuery(kind, text);
    if (scope === "tabs") {
      setStatus("Ищу во вкладках…");
      const all: FileHits[] = [];
      for (const t of tabs) {
        await new Promise<void>((resolve) => {
          const offs: number[] = [];
          let len = 0;
          api
            .searchAll(t.id, query, null, 1000, (e) => {
              if (e.type === "batch") {
                for (const m of e.matches) {
                  offs.push(m.offset);
                  len = m.len;
                }
              } else if (e.type === "done" || e.type === "error") {
                if (offs.length) all.push({ path: t.info.name, tabId: t.id, offsets: offs, len });
                resolve();
              }
            })
            .catch(() => resolve());
        });
      }
      setHits(all);
      setStatus(`Вкладок с совпадениями: ${all.length} из ${tabs.length}`);
      return;
    }
    if (!root) return toast("Выберите папку", true);
    setStatus("Ищу…");
    try {
      const id = await files.search(root, mask, recursive, query, (e) => {
        if (e.type === "hits")
          setHits((h) => [...h, { path: e.path, offsets: e.offsets, len: e.len }]);
        else if (e.type === "progress") setStatus(`Файлов: ${e.files} · ${e.current}`);
        else if (e.type === "done") {
          setStatus(
            `Готово: файлов ${e.files}, совпадений ${e.hits}${e.truncated ? " (список файлов обрезан)" : ""}`,
          );
          setJob(null);
        } else if (e.type === "error") A.report(e.error);
      });
      setJob(id);
    } catch (e) {
      A.report(e);
    }
  };
  const openHit = async (h: FileHits, off: number) => {
    try {
      let id = h.tabId;
      if (id === undefined) {
        const existing = useTabs.getState().tabs.find((t) => t.info.path === h.path);
        id = existing?.id ?? (await A.adopt(await api.open(h.path))).id;
      }
      useTabs.getState().activate(id);
      setTimeout(() => A.selectRange(off, off + h.len), 50);
    } catch (e) {
      A.report(e);
    }
  };
  return (
    <Modal
      title="Найти в файлах"
      wide
      onClose={() => {
        if (job !== null) void api.cancelJob(job);
        closeDialog();
      }}
      footer={
        <>
          {job !== null && (
            <button
              type="button"
              className="btn"
              onClick={() => api.cancelJob(job).then(() => setJob(null))}
            >
              Остановить
            </button>
          )}
          <button type="button" className="btn pri" onClick={run} disabled={!text || job !== null}>
            Искать
          </button>
        </>
      }
    >
      <div className="seg anl-tabs" role="tablist">
        {(
          [
            ["files", "Папка"],
            ["tabs", "Открытые вкладки"],
          ] as const
        ).map(([id, label]) => (
          <button
            key={id}
            type="button"
            role="tab"
            aria-selected={scope === id}
            className={scope === id ? "on" : ""}
            onClick={() => setScope(id)}
          >
            {label}
          </button>
        ))}
      </div>
      {scope === "files" && (
        <>
          <div className="row-f">
            <span>Папка</span>
            <div style={{ display: "flex", gap: 6 }}>
              <input
                className="inp"
                value={root}
                onChange={(e) => setRoot(e.target.value)}
                onKeyDown={stop}
                aria-label="Папка"
              />
              <button type="button" className="btn" onClick={pick}>
                Выбрать…
              </button>
            </div>
          </div>
          <div className="row-f">
            <span>Маска</span>
            <input
              className="inp mono"
              value={mask}
              onChange={(e) => setMask(e.target.value)}
              onKeyDown={stop}
              aria-label="Маска файлов"
            />
          </div>
          <label className="chk">
            <input
              type="checkbox"
              checked={recursive}
              onChange={(e) => setRecursive(e.target.checked)}
            />
            Во вложенных папках
          </label>
        </>
      )}
      <div className="row-f">
        <span>Что искать</span>
        <div style={{ display: "flex", gap: 6 }}>
          <select
            className="sel-input"
            value={kind}
            onChange={(e) => setKind(e.target.value)}
            aria-label="Тип поиска"
          >
            <option value="text">текст</option>
            <option value="hex">hex</option>
            <option value="regex">regex</option>
          </select>
          <input
            className="inp mono"
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              stop(e);
              if (e.key === "Enter" && text) void run();
            }}
            aria-label="Строка поиска"
          />
        </div>
      </div>
      {status && <div className="note">{status}</div>}
      <div className="list" style={{ maxHeight: 320, overflow: "auto" }}>
        {hits.map((h) => (
          <div key={h.path} className="fif">
            <b title={h.path}>
              {h.path.split(/[\\/]/).pop()} <span className="note">· {h.offsets.length}</span>
            </b>
            <div className="fif-offs">
              {h.offsets.slice(0, 64).map((o) => (
                <button type="button" key={o} className="chip mono" onClick={() => openHit(h, o)}>
                  {hexN(o)}
                </button>
              ))}
              {h.offsets.length > 64 && <span className="note">+{h.offsets.length - 64}</span>}
            </div>
          </div>
        ))}
      </div>
    </Modal>
  );
}

// ------------------------------------------------------------------ FILE-13

export function SplitDialog() {
  const tab = useActiveTab();
  const [mode, setMode] = useState<"size" | "count">("size");
  const [value, setValue] = useState("1M");
  if (!tab) return null;
  const parse = (s: string): number | null => {
    const m = /^\s*(\d+(?:\.\d+)?)\s*([kmg]?)i?b?\s*$/i.exec(s);
    if (!m) return null;
    const mul = { "": 1, k: 1024, m: 1 << 20, g: 1 << 30 }[m[2].toLowerCase()] ?? 1;
    return Math.floor(Number(m[1]) * mul);
  };
  const n = parse(value);
  const partSize = n && (mode === "size" ? n : Math.ceil(tab.info.len / n));
  const parts = partSize ? Math.ceil(tab.info.len / partSize) : 0;
  const run = async () => {
    if (!partSize) return;
    const base = await saveFile({ defaultPath: tab.info.name });
    if (!base) return;
    try {
      const out = await files.split(tab.id, partSize, base);
      closeDialog();
      toast(`Создано частей: ${out.length}`);
    } catch (e) {
      A.report(e);
    }
  };
  return (
    <Modal
      title="Разделить файл"
      footer={
        <>
          <button type="button" className="btn" onClick={closeDialog}>
            Отмена
          </button>
          <button type="button" className="btn pri" disabled={!partSize || parts < 1} onClick={run}>
            Сохранить части…
          </button>
        </>
      }
    >
      <div className="row-f">
        <span>Делить</span>
        <select
          className="inp"
          value={mode}
          onChange={(e) => setMode(e.target.value as typeof mode)}
        >
          <option value="size">по размеру части</option>
          <option value="count">на N частей</option>
        </select>
      </div>
      <div className="row-f">
        <span>{mode === "size" ? "Размер (1.44M, 512K…)" : "Количество"}</span>
        <input
          className={`inp${n ? "" : " bad"}`}
          value={value}
          onChange={(e) => setValue(e.target.value)}
          onKeyDown={stop}
        />
      </div>
      <div className="note">
        {parts > 0
          ? `${parts} частей по ${fmtSize(partSize ?? 0)}: имя.001, имя.002…`
          : "Укажите размер или количество."}{" "}
        Склеить обратно — «Склеить файлы…».
      </div>
    </Modal>
  );
}

export async function concatFiles() {
  const picked = await openFile({ multiple: true });
  if (!Array.isArray(picked) || picked.length < 2) {
    if (picked) toast("Выберите несколько файлов (порядок — по имени)", true);
    return;
  }
  try {
    await A.adopt(await files.concat([...picked].sort()));
    toast(`Склеено файлов: ${picked.length}`);
  } catch (e) {
    A.report(e);
  }
}

// ------------------------------------------------------------------ IO-08

export function ViewLinkDialog() {
  const tab = useActiveTab();
  const [input, setInput] = useState("");
  if (!tab) return null;
  const link = viewToLink(captureView(tab));
  const apply = async () => {
    try {
      const v = parseView(input);
      if (v.name !== tab.info.name || v.len !== tab.info.len)
        toast(`Вид создан для «${v.name}» (${fmtSize(v.len)}), применяю к текущему файлу`);
      useTabs
        .getState()
        .update(tab.id, { cursor: v.cursor, anchor: v.anchor, topRow: v.topRow, folds: v.folds });
      if (v.marks.length) {
        await api.marksSet(tab.id, [...tab.marks, ...v.marks]);
        await A.refreshMarks(tab.id);
      }
      closeDialog();
    } catch (e) {
      A.report(e);
    }
  };
  const saveFileView = async () => {
    const path = await saveFile({
      defaultPath: `${tab.info.name}.hexaview`,
      filters: [{ name: "Вид Hexa", extensions: ["hexaview"] }],
    });
    if (path)
      await api
        .writeTextFile(path, JSON.stringify(captureView(tab), null, 2))
        .then(() => toast("Вид сохранён"))
        .catch(A.report);
  };
  const loadFileView = async () => {
    const path = await openFile({
      multiple: false,
      filters: [{ name: "Вид Hexa", extensions: ["hexaview", "json"] }],
    });
    if (typeof path === "string") setInput(await api.readTextFile(path));
  };
  return (
    <Modal
      title="Поделиться видом"
      footer={
        <>
          <button type="button" className="btn" onClick={closeDialog}>
            Закрыть
          </button>
          <button type="button" className="btn pri" onClick={apply} disabled={!input.trim()}>
            Применить вид
          </button>
        </>
      }
    >
      <div className="note">
        Позиция, выделение, закладки и свёртки — без байтов файла. Отправьте ссылку коллеге с тем же
        файлом.
      </div>
      <div style={{ display: "flex", gap: 6, margin: "6px 0" }}>
        <input className="inp mono" readOnly value={link} aria-label="Ссылка на вид" />
        <button
          type="button"
          className="btn"
          onClick={() => writeText(link).then(() => toast("Ссылка скопирована"))}
        >
          Копировать
        </button>
        <button type="button" className="btn" onClick={saveFileView}>
          Файлом…
        </button>
      </div>
      <div className="row-f">
        <span>Открыть вид</span>
        <div style={{ display: "flex", gap: 6 }}>
          <input
            className="inp mono"
            placeholder="hexa://view?s=…"
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={stop}
            aria-label="Вставьте ссылку"
          />
          <button type="button" className="btn" onClick={loadFileView}>
            Из файла…
          </button>
        </div>
      </div>
    </Modal>
  );
}

// ------------------------------------------------------------------ UI-15

const RELEASES = "https://api.github.com/repos/kewldan/hexa/releases/latest";

/** Opt-in update check: one anonymous request to GitHub releases. */
export async function checkUpdates(manual: boolean) {
  try {
    const { version } = await files.appInfo();
    const r = await fetch(RELEASES, { headers: { Accept: "application/vnd.github+json" } });
    if (!r.ok) {
      if (manual) toast("Не удалось узнать о новых версиях", true);
      return;
    }
    const tag = String((await r.json()).tag_name ?? "").replace(/^v/, "");
    const newer = tag.localeCompare(version, undefined, { numeric: true }) > 0;
    if (newer) toast(`Доступна версия ${tag} (у вас ${version})`);
    else if (manual) toast(`Установлена последняя версия ${version}`);
  } catch {
    if (manual) toast("Нет связи с сервером обновлений", true);
  }
}
