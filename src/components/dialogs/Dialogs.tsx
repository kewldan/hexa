// Modal dialogs.
import { type ReactNode, useEffect, useState } from "react";
import type { Mark, Planned } from "../../ipc/api";
import {
  api,
  type CopyFormat,
  type FileProps,
  type Fill,
  type PasteCandidate,
  type RecoveryItem,
} from "../../ipc/api";
import { evalExpr } from "../../lib/expr";
import { fmtSize, groupDec, hexN, parseHexBytes, relTime } from "../../lib/format";
import { renderMarkdown } from "../../lib/markdown";
import * as A from "../../state/actions";
import { startDiff } from "../../state/diff";
import { selection, selectionOrByte, useActiveTab, useTabs } from "../../state/tabs";
import { closeDialog, type Dialog, toast, useUi } from "../../state/ui";
import { MARK_COLORS } from "../panels/MarksPanel";
import { Merge3Dialog, PatchApplyDialog, PatchCreateDialog } from "./CompareDialogs";
import { SettingsDialog } from "./Settings";

export function Modal({
  title,
  children,
  footer,
  wide,
  onClose = closeDialog,
}: {
  title: string;
  children: ReactNode;
  footer?: ReactNode;
  wide?: boolean;
  onClose?: () => void;
}) {
  return (
    <div className="scrim center" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div
        className={`dlg${wide ? " wide" : ""}`}
        role="dialog"
        aria-modal="true"
        aria-label={title}
        onKeyDown={(e) => {
          e.stopPropagation();
          if (e.key === "Escape") onClose();
        }}
      >
        <div className="dlg-h">
          <h2>{title}</h2>
        </div>
        <div className="dlg-b">{children}</div>
        {footer && <div className="dlg-f">{footer}</div>}
      </div>
    </div>
  );
}

function useNumber(initial: string) {
  const [text, setText] = useState(initial);
  const tab = useActiveTab();
  let value: number | null = null;
  try {
    value = text.trim()
      ? evalExpr(text, { cursor: tab?.cursor ?? 0, length: tab?.info.len ?? 0 }).value
      : null;
  } catch {
    value = null;
  }
  return { text, setText, value, bad: text.trim() !== "" && (value === null || value < 0) };
}

type FillKind = "byte" | "pattern" | "random" | "sequence";

function FillEditor({ onChange }: { onChange: (f: Fill | null) => void }) {
  const [kind, setKind] = useState<FillKind>("byte");
  const [byte, setByte] = useState("00");
  const [pattern, setPattern] = useState("DE AD BE EF");
  const [seed, setSeed] = useState(String(Date.now() % 1_000_000));
  const [seqStart, setSeqStart] = useState("0");
  const [seqStep, setSeqStep] = useState("1");
  const [width, setWidth] = useState(1);
  const [be, setBe] = useState(false);
  useEffect(() => {
    let f: Fill | null = null;
    if (kind === "byte") {
      const b = parseHexBytes(byte);
      f = b && b.length === 1 ? { kind: "byte", value: b[0] } : null;
    } else if (kind === "pattern") {
      const b = parseHexBytes(pattern);
      f = b ? { kind: "pattern", bytes: b } : null;
    } else if (kind === "random") {
      const n = Number.parseInt(seed, 10);
      f = Number.isFinite(n) ? { kind: "random", seed: n } : null;
    } else {
      const s0 = Number.parseInt(seqStart, 16);
      const st = Number.parseInt(seqStep, 16);
      f =
        Number.isFinite(s0) && Number.isFinite(st)
          ? { kind: "sequence", start: s0, step: st, width, bigEndian: be }
          : null;
    }
    onChange(f);
  }, [kind, byte, pattern, seed, seqStart, seqStep, width, be, onChange]);
  return (
    <>
      <div className="row-f">
        <span>Чем заполнить</span>
        <select className="inp" value={kind} onChange={(e) => setKind(e.target.value as FillKind)}>
          <option value="byte">Байт</option>
          <option value="pattern">Паттерн (повтор)</option>
          <option value="random">Случайные байты с seed</option>
          <option value="sequence">Возрастающая последовательность</option>
        </select>
      </div>
      {kind === "byte" && (
        <div className="row-f">
          <span>Байт (hex)</span>
          <input className="inp" value={byte} onChange={(e) => setByte(e.target.value)} />
        </div>
      )}
      {kind === "pattern" && (
        <div className="row-f">
          <span>Паттерн (hex)</span>
          <input
            className={`inp${parseHexBytes(pattern) ? "" : " bad"}`}
            value={pattern}
            onChange={(e) => setPattern(e.target.value)}
          />
        </div>
      )}
      {kind === "random" && (
        <div className="row-f">
          <span>Seed</span>
          <input className="inp" value={seed} onChange={(e) => setSeed(e.target.value)} />
        </div>
      )}
      {kind === "sequence" && (
        <>
          <div className="row-f">
            <span>Начало · шаг (hex)</span>
            <div style={{ display: "flex", gap: 6 }}>
              <input
                className="inp"
                value={seqStart}
                onChange={(e) => setSeqStart(e.target.value)}
              />
              <input className="inp" value={seqStep} onChange={(e) => setSeqStep(e.target.value)} />
            </div>
          </div>
          <div className="row-f">
            <span>Ширина</span>
            <div style={{ display: "flex", gap: 6 }}>
              <select
                className="inp"
                value={width}
                onChange={(e) => setWidth(Number(e.target.value))}
              >
                {[1, 2, 4, 8].map((w) => (
                  <option key={w} value={w}>
                    {w * 8} бит
                  </option>
                ))}
              </select>
              <select
                className="inp"
                value={be ? "be" : "le"}
                onChange={(e) => setBe(e.target.value === "be")}
              >
                <option value="le">LE</option>
                <option value="be">BE</option>
              </select>
            </div>
          </div>
        </>
      )}
      <div className="note">
        Байты не создаются в памяти: заполнение гигабайтов происходит мгновенно и отменяется одним
        шагом.
      </div>
    </>
  );
}

function NewFileDialog() {
  const size = useNumber("1000");
  const [kind, setKind] = useState<"empty" | "sized" | "clipboard">("empty");
  const [fill, setFill] = useState<Fill | null>({ kind: "byte", value: 0 });
  const create = async () => {
    closeDialog();
    if (kind === "empty") await A.newDocument({ kind: "empty" });
    else if (kind === "clipboard") await A.newFromClipboard();
    else if (size.value !== null && fill)
      await A.newDocument({ kind: "sized", len: size.value, fill });
  };
  return (
    <Modal
      title="Новый файл"
      footer={
        <>
          <button type="button" className="btn" onClick={closeDialog}>
            Отмена
          </button>
          <button
            type="button"
            className="btn pri"
            onClick={create}
            disabled={kind === "sized" && (size.bad || !fill)}
          >
            Создать
          </button>
        </>
      }
    >
      <div className="opt-list">
        {(
          [
            ["empty", "Пустой", "Начните вводить байты"],
            ["sized", "Заданного размера", "Заполнен байтом, паттерном или случайными данными"],
            [
              "clipboard",
              "Из буфера обмена",
              "Hex-строка, массив, Base64 или xxd распознаются автоматически",
            ],
          ] as const
        ).map(([k, t, d]) => (
          <button
            type="button"
            key={k}
            className={`opt${kind === k ? " on" : ""}`}
            onClick={() => setKind(k)}
          >
            <input type="radio" readOnly checked={kind === k} tabIndex={-1} />
            <span className="t" style={{ display: "flex", flexDirection: "column" }}>
              <b>{t}</b>
              <span className="note">{d}</span>
            </span>
            <span />
          </button>
        ))}
      </div>
      {kind === "sized" && (
        <>
          <div className="row-f">
            <span>Размер (hex или 128d)</span>
            <input
              className={`inp${size.bad ? " bad" : ""}`}
              value={size.text}
              onChange={(e) => size.setText(e.target.value)}
            />
          </div>
          {size.value !== null && (
            <div className="note">
              {fmtSize(size.value)} · {groupDec(size.value)} байт
            </div>
          )}
          <FillEditor onChange={setFill} />
        </>
      )}
    </Modal>
  );
}

function FillDialog() {
  const tab = useActiveTab();
  const [fill, setFill] = useState<Fill | null>(null);
  if (!tab) return null;
  const r = selectionOrByte(tab);
  return (
    <Modal
      title="Заполнить выделение"
      footer={
        <>
          <button type="button" className="btn" onClick={closeDialog}>
            Отмена
          </button>
          <button
            type="button"
            className="btn pri"
            disabled={!fill}
            onClick={() => {
              closeDialog();
              if (fill) A.edit({ op: "fill", start: r.start, end: r.end, fill });
            }}
          >
            Заполнить
          </button>
        </>
      }
    >
      <div className="note">
        Диапазон 0x{hexN(r.start)}…0x{hexN(r.end)} · {fmtSize(r.end - r.start)}
      </div>
      <FillEditor onChange={setFill} />
    </Modal>
  );
}

function InsertBytesDialog() {
  const tab = useActiveTab();
  const n = useNumber("10");
  const [fill, setFill] = useState<Fill | null>(null);
  if (!tab) return null;
  return (
    <Modal
      title="Вставить байты"
      footer={
        <>
          <button type="button" className="btn" onClick={closeDialog}>
            Отмена
          </button>
          <button
            type="button"
            className="btn pri"
            disabled={n.bad || n.value === null || !fill}
            onClick={() => {
              closeDialog();
              if (n.value !== null && fill)
                A.edit({ op: "insertFill", pos: tab.cursor, len: n.value, fill }, tab.cursor);
            }}
          >
            Вставить
          </button>
        </>
      }
    >
      <div className="row-f">
        <span>Сколько (hex или 128d)</span>
        <input
          className={`inp${n.bad ? " bad" : ""}`}
          value={n.text}
          onChange={(e) => n.setText(e.target.value)}
        />
      </div>
      <div className="note">В позицию 0x{hexN(tab.cursor)}</div>
      <FillEditor onChange={setFill} />
    </Modal>
  );
}

function ResizeDialog() {
  const tab = useActiveTab();
  const n = useNumber(tab ? hexN(tab.info.len) : "0");
  const [fill, setFill] = useState<Fill | null>(null);
  if (!tab) return null;
  const grow = n.value !== null && n.value > tab.info.len;
  return (
    <Modal
      title="Изменить размер"
      footer={
        <>
          <button type="button" className="btn" onClick={closeDialog}>
            Отмена
          </button>
          <button
            type="button"
            className={`btn ${n.value !== null && n.value < tab.info.len ? "danger" : "pri"}`}
            disabled={n.bad || n.value === null || (grow && !fill)}
            onClick={() => {
              closeDialog();
              if (n.value !== null)
                A.edit({ op: "resize", len: n.value, fill: fill ?? { kind: "byte", value: 0 } });
            }}
          >
            {n.value !== null && n.value < tab.info.len ? "Обрезать" : "Применить"}
          </button>
        </>
      }
    >
      <div className="row-f">
        <span>Новый размер</span>
        <input
          className={`inp${n.bad ? " bad" : ""}`}
          value={n.text}
          onChange={(e) => n.setText(e.target.value)}
        />
      </div>
      <div className="note">
        Сейчас {fmtSize(tab.info.len)}
        {n.value !== null ? ` → ${fmtSize(n.value)}` : ""}. Поддерживаются выражения: end+100,
        end-0x10.
      </div>
      {grow && <FillEditor onChange={setFill} />}
    </Modal>
  );
}

function SelectRangeDialog() {
  const tab = useActiveTab();
  const sel = tab ? selection(tab) : { start: 0, end: 0 };
  const from = useNumber(hexN(sel.start));
  const [mode, setMode] = useState<"len" | "to">("len");
  const second = useNumber(hexN(Math.max(1, sel.end - sel.start)));
  if (!tab) return null;
  const end =
    from.value === null || second.value === null
      ? null
      : mode === "len"
        ? from.value + second.value
        : second.value;
  return (
    <Modal
      title="Выделить диапазон"
      footer={
        <>
          <button type="button" className="btn" onClick={closeDialog}>
            Отмена
          </button>
          <button
            type="button"
            className="btn pri"
            disabled={from.value === null || end === null || end < from.value}
            onClick={() => {
              closeDialog();
              if (from.value !== null && end !== null) A.selectRange(from.value, end);
            }}
          >
            Выделить
          </button>
        </>
      }
    >
      <div className="row-f">
        <span>Начало</span>
        <input
          className={`inp${from.bad ? " bad" : ""}`}
          value={from.text}
          onChange={(e) => from.setText(e.target.value)}
        />
      </div>
      <div className="row-f">
        <select
          className="inp"
          value={mode}
          onChange={(e) => setMode(e.target.value as "len" | "to")}
        >
          <option value="len">Длина</option>
          <option value="to">Конец (не включая)</option>
        </select>
        <input
          className={`inp${second.bad ? " bad" : ""}`}
          value={second.text}
          onChange={(e) => second.setText(e.target.value)}
        />
      </div>
      {from.value !== null && end !== null && (
        <div className="note">
          0x{hexN(from.value)}…0x{hexN(end)} · {fmtSize(Math.max(0, end - from.value))}
        </div>
      )}
    </Modal>
  );
}

const FORMATS: [CopyFormat["kind"], string][] = [
  ["hexSpaced", "Hex с пробелами"],
  ["hex", "Hex слитно"],
  ["hex0x", "0x-префиксы"],
  ["escaped", "\\xNN"],
  ["c", "C"],
  ["cpp", "C++ std::array"],
  ["cSharp", "C#"],
  ["rust", "Rust"],
  ["go", "Go"],
  ["java", "Java"],
  ["kotlin", "Kotlin"],
  ["swift", "Swift"],
  ["javaScript", "JavaScript"],
  ["typeScript", "TypeScript"],
  ["python", "Python"],
  ["pascal", "Pascal / Delphi"],
  ["visualBasic", "Visual Basic"],
  ["powerShell", "PowerShell"],
  ["asm", "Ассемблер db"],
  ["base64", "Base64"],
  ["base32", "Base32"],
  ["base85", "Base85 (Ascii85)"],
  ["url", "URL-encode"],
  ["uuencode", "uuencode"],
  ["xxd", "Дамп xxd"],
  ["html", "HTML с цветами"],
  ["rtf", "RTF с цветами"],
  ["template", "Свой шаблон…"],
];

function CopyAsDialog() {
  const tab = useActiveTab();
  const [kind, setKind] = useState<CopyFormat["kind"]>("c");
  const [tpl, setTpl] = useState({
    prefix: "[",
    item: "0x{X}",
    separator: ", ",
    perLine: 16,
    linePrefix: "",
    suffix: "]",
  });
  const [preview, setPreview] = useState("");
  const r = tab ? selectionOrByte(tab) : { start: 0, end: 0 };
  const fmt: CopyFormat =
    kind === "xxd" || kind === "html" || kind === "rtf"
      ? { kind, offset: r.start }
      : kind === "template"
        ? { kind, ...tpl }
        : ({ kind } as CopyFormat);
  const fmtKey = JSON.stringify(fmt);
  useEffect(() => {
    if (!tab) return;
    const fmt = JSON.parse(fmtKey) as CopyFormat;
    const end = Math.min(r.end, r.start + 256);
    api
      .copyAs(tab.id, r.start, end, fmt)
      .then((t) => setPreview(t + (end < r.end ? "\n…" : "")))
      .catch((e) => setPreview(String(e.message ?? e)));
  }, [tab?.id, r.start, r.end, fmtKey]);
  if (!tab) return null;
  return (
    <Modal
      wide
      title={`Копировать как · ${fmtSize(r.end - r.start)}`}
      footer={
        <>
          <button type="button" className="btn" onClick={closeDialog}>
            Отмена
          </button>
          <button
            type="button"
            className="btn pri"
            onClick={() => {
              closeDialog();
              A.copyAsText(fmt);
            }}
          >
            Копировать
          </button>
        </>
      }
    >
      <div className="chips" style={{ display: "flex", flexWrap: "wrap", gap: 6 }}>
        {FORMATS.map(([k, label]) => (
          <button
            type="button"
            key={k}
            className={`chip${k === kind ? " on" : ""}`}
            onClick={() => setKind(k)}
          >
            {label}
          </button>
        ))}
      </div>
      {kind === "template" && (
        <div style={{ display: "grid", gridTemplateColumns: "repeat(3, 1fr)", gap: 6 }}>
          {(["prefix", "item", "separator", "linePrefix", "suffix"] as const).map((k) => (
            <label key={k} className="note">
              {k}
              <input
                className="inp"
                value={tpl[k]}
                onChange={(e) => setTpl({ ...tpl, [k]: e.target.value })}
              />
            </label>
          ))}
          <label className="note">
            perLine
            <input
              className="inp"
              value={tpl.perLine}
              onChange={(e) => setTpl({ ...tpl, perLine: Number(e.target.value) || 0 })}
            />
          </label>
          <div className="note" style={{ gridColumn: "1 / -1" }}>
            В item: {"{X}"} {"{x}"} hex, {"{d}"} десятичное, {"{o}"} восьмеричное, {"{b}"} двоичное,{" "}
            {"{c}"} символ, {"{i}"} индекс
          </div>
        </div>
      )}
      <div className="pre">{preview}</div>
    </Modal>
  );
}

function PasteDialog({ text }: { text: string }) {
  const tab = useActiveTab();
  const [cands, setCands] = useState<PasteCandidate[]>([]);
  const [pick, setPick] = useState(0);
  const [mode, setMode] = useState<"insert" | "overwrite">(
    tab?.info.mode === "insert" ? "insert" : "overwrite",
  );
  useEffect(() => {
    api
      .clipParse(text)
      .then(setCands)
      .catch(() => {});
  }, [text]);
  const c = cands[pick];
  return (
    <Modal
      wide
      title="Вставить из буфера"
      footer={
        <>
          <select
            className="inp"
            style={{ width: 200, marginRight: "auto" }}
            value={mode}
            onChange={(e) => setMode(e.target.value as "insert" | "overwrite")}
          >
            <option value="insert">Со сдвигом (вставка)</option>
            <option value="overwrite">С заменой</option>
          </select>
          <button type="button" className="btn" onClick={closeDialog}>
            Отмена
          </button>
          <button
            type="button"
            className="btn pri"
            disabled={!c}
            onClick={() => {
              closeDialog();
              if (c) A.pasteBytes(c.bytes, mode === "overwrite");
            }}
          >
            Вставить {c ? fmtSize(c.bytes.length) : ""}
          </button>
        </>
      }
    >
      <div className="opt-list">
        {cands.map((x, i) => (
          <button
            type="button"
            key={x.kind}
            className={`opt${i === pick ? " on" : ""}`}
            onClick={() => setPick(i)}
          >
            <input type="radio" readOnly checked={i === pick} tabIndex={-1} />
            <span>
              <b>{x.label}</b>
              <span className="note"> · {fmtSize(x.bytes.length)}</span>
            </span>
            <span className="tag">{i === 0 ? "вероятнее всего" : ""}</span>
          </button>
        ))}
      </div>
      {c && (
        <div className="pre">
          {c.bytes
            .slice(0, 256)
            .map((b) => hexN(b, 2))
            .join(" ")}
          {c.bytes.length > 256 ? " …" : ""}
        </div>
      )}
    </Modal>
  );
}

function PropsDialog() {
  const tab = useActiveTab();
  const [p, setP] = useState<FileProps | null>(null);
  useEffect(() => {
    if (tab) api.props(tab.id).then(setP).catch(A.report);
  }, [tab?.id]);
  if (!tab) return null;
  const d = (ms: number | null) => (ms ? new Date(ms).toLocaleString("ru-RU") : "—");
  return (
    <Modal
      title="Свойства файла"
      footer={
        <>
          {tab.info.path && (
            <button
              type="button"
              className="btn"
              style={{ marginRight: "auto" }}
              onClick={() =>
                import("@tauri-apps/plugin-opener").then((o) => o.revealItemInDir(tab.info.path!))
              }
            >
              Показать в папке
            </button>
          )}
          <button type="button" className="btn pri" onClick={closeDialog}>
            Готово
          </button>
        </>
      }
    >
      {p && (
        <div className="props">
          <span>Имя</span>
          <span>{p.name}</span>
          <span>Путь</span>
          <span>{p.path ?? "не сохранён"}</span>
          <span>Размер</span>
          <span>
            {fmtSize(p.size)} · {groupDec(p.size)} Б
          </span>
          <span>На диске</span>
          <span>
            {p.diskSize === null
              ? "—"
              : `${groupDec(p.diskSize)} Б${p.diskChanged ? " · изменён другой программой" : ""}`}
          </span>
          <span>Создан</span>
          <span>{d(p.createdMs)}</span>
          <span>Изменён</span>
          <span>{d(p.modifiedMs)}</span>
          <span>Открыт</span>
          <span>{d(p.accessedMs)}</span>
          <span>Права</span>
          <span>
            {p.permissions ?? (p.readonly ? "только чтение" : "чтение и запись")}
            {p.owner ? ` · ${p.owner}` : ""}
          </span>
          <span>Режим</span>
          <span>
            {tab.info.mode}
            {tab.info.fixedSize ? " · фиксированный размер" : ""}
          </span>
          <span>Фрагментов</span>
          <span>{groupDec(p.pieces)}</span>
          <span>Записей истории</span>
          <span>{p.history}</span>
        </div>
      )}
      <div className="note">Хэши файла — на панели «Хэши».</div>
    </Modal>
  );
}

function ReplacePreviewDialog({ plan }: { plan: Planned[] }) {
  const tab = useActiveTab();
  const [skip, setSkip] = useState<Set<number>>(new Set());
  if (!tab) return null;
  const hx = (b: number[]) =>
    b.length > 24
      ? `${b
          .slice(0, 24)
          .map((x) => hexN(x, 2))
          .join(" ")} …`
      : b.map((x) => hexN(x, 2)).join(" ");
  const chosen = plan.filter((_, i) => !skip.has(i));
  return (
    <Modal
      wide
      title={`Заменить ${chosen.length} из ${plan.length}`}
      footer={
        <>
          <button type="button" className="btn" onClick={closeDialog}>
            Отмена
          </button>
          <button
            type="button"
            className="btn pri"
            disabled={!chosen.length}
            onClick={async () => {
              closeDialog();
              try {
                const r = await api.replaceApply(
                  tab.id,
                  chosen.map((p) => ({ offset: p.offset, len: p.len, new: p.new })),
                );
                A.applyResult(tab.id, r);
                toast(`Заменено: ${chosen.length} — отменяется одним шагом`);
              } catch (e) {
                A.report(e);
              }
            }}
          >
            Заменить
          </button>
        </>
      }
    >
      <div className="note">
        Предпросмотр всех замен (SRCH-22). Снимите отметку, чтобы пропустить.
      </div>
      <div className="list" style={{ maxHeight: 360, overflow: "auto" }}>
        {plan.slice(0, 2000).map((p, i) => (
          <label
            key={p.offset}
            className="li"
            style={{ gridTemplateColumns: "16px 100px minmax(0,1fr)", animation: "none" }}
          >
            <input
              type="checkbox"
              checked={!skip.has(i)}
              onChange={() => {
                const n = new Set(skip);
                if (n.has(i)) n.delete(i);
                else n.add(i);
                setSkip(n);
              }}
            />
            <span className="mono">0x{hexN(p.offset, 8)}</span>
            <span className="mono" style={{ fontSize: 11.5 }}>
              <span style={{ color: "var(--mod)" }}>{hx(p.old)}</span> →{" "}
              <span style={{ color: "var(--ins)" }}>{hx(p.new)}</span>
            </span>
          </label>
        ))}
      </div>
      {plan.length > 2000 && <div className="note">Показаны первые 2000.</div>}
    </Modal>
  );
}

function PickTabDialog({ mode }: { mode: "side" | "diff" }) {
  const tabs = useTabs((st) => st.tabs);
  const cur = useActiveTab();
  const others = tabs.filter((t) => t.id !== cur?.id);
  return (
    <Modal
      title={mode === "diff" ? "Сравнить с…" : "Открыть рядом"}
      footer={
        <button type="button" className="btn" onClick={closeDialog}>
          Отмена
        </button>
      }
    >
      {others.length === 0 ? (
        <div className="note">
          Откройте второй файл, чтобы {mode === "diff" ? "сравнить с ним" : "показать его рядом"}.
        </div>
      ) : (
        <div className="opt-list">
          {others.map((t) => (
            <button
              type="button"
              key={t.id}
              className="opt"
              onClick={() => {
                closeDialog();
                if (!cur) return;
                if (mode === "diff") void startDiff(cur.id, t.id);
                else useUi.getState().set({ compare: { a: cur.id, b: t.id, sync: true } });
              }}
            >
              <span />
              <span>
                <b>{t.info.name}</b>
                <span className="note"> · {fmtSize(t.info.len)}</span>
              </span>
              <span />
            </button>
          ))}
        </div>
      )}
    </Modal>
  );
}

function MarkDialog({ mark }: { mark: Mark }) {
  const tab = useActiveTab();
  const [m, setM] = useState(mark);
  const [preview, setPreview] = useState(false);
  if (!tab) return null;
  const set = (p: Partial<Mark>) => setM({ ...m, ...p });
  const save = async () => {
    closeDialog();
    await api.marksUpdate(tab.id, m);
    await A.refreshMarks(tab.id);
  };
  return (
    <Modal
      wide
      title={m.kind === "label" ? "Метка" : m.kind === "annotation" ? "Заметка" : "Закладка"}
      footer={
        <>
          <button
            type="button"
            className="btn danger"
            style={{ marginRight: "auto" }}
            onClick={async () => {
              closeDialog();
              await api.marksRemove(tab.id, m.id);
              await A.refreshMarks(tab.id);
            }}
          >
            Удалить
          </button>
          <button type="button" className="btn" onClick={closeDialog}>
            Отмена
          </button>
          <button type="button" className="btn pri" onClick={save}>
            Сохранить
          </button>
        </>
      }
    >
      <div className="row-f">
        <span>Имя</span>
        <input className="inp" value={m.name} onChange={(e) => set({ name: e.target.value })} />
      </div>
      <div className="row-f">
        <span>Тип</span>
        <select
          className="inp"
          value={m.kind}
          onChange={(e) => set({ kind: e.target.value as Mark["kind"] })}
        >
          <option value="bookmark">Закладка</option>
          <option value="label">Метка (имя для «Перейти»)</option>
          <option value="annotation">Заметка на данных</option>
        </select>
      </div>
      <div className="row-f">
        <span>Цвет</span>
        <div style={{ display: "flex", gap: 6 }}>
          {Object.entries(MARK_COLORS).map(([name, c]) => (
            <button
              type="button"
              key={name}
              aria-label={name}
              onClick={() => set({ color: name })}
              style={{
                width: 22,
                height: 22,
                borderRadius: 6,
                background: c,
                boxShadow: m.color === name ? `0 0 0 2px var(--ed), 0 0 0 4px ${c}` : "none",
              }}
            />
          ))}
        </div>
      </div>
      <div className="row-f">
        <span>Диапазон (hex)</span>
        <div style={{ display: "flex", gap: 6 }}>
          <input
            className="inp"
            value={hexN(m.start)}
            onChange={(e) => {
              const v = Number.parseInt(e.target.value || "0", 16);
              if (Number.isFinite(v)) set({ start: v });
            }}
          />
          <input
            className="inp"
            value={hexN(m.end)}
            onChange={(e) => {
              const v = Number.parseInt(e.target.value || "0", 16);
              if (Number.isFinite(v)) set({ end: v });
            }}
          />
        </div>
      </div>
      <div className="row-f">
        <span>Тип данных</span>
        <select
          className="inp"
          value={m.dataType ?? ""}
          onChange={(e) => set({ dataType: e.target.value || null })}
        >
          <option value="">—</option>
          {["u8", "u16", "u32", "u64", "i32", "f32", "f64", "string", "utf16", "bytes"].map((t) => (
            <option key={t}>{t}</option>
          ))}
        </select>
      </div>
      <div className="row-f" style={{ alignItems: "start" }}>
        <span>
          Заметка (Markdown){" "}
          <button type="button" className="chip" onClick={() => setPreview(!preview)}>
            {preview ? "Править" : "Просмотр"}
          </button>
        </span>
        {preview ? (
          // biome-ignore lint/security/noDangerouslySetInnerHtml: rendered from escaped Markdown
          <div className="md" dangerouslySetInnerHTML={{ __html: renderMarkdown(m.note) }} />
        ) : (
          <textarea
            className="inp"
            value={m.note}
            onChange={(e) => set({ note: e.target.value })}
          />
        )}
      </div>
    </Modal>
  );
}

function MoveBlockDialog() {
  const tab = useActiveTab();
  const to = useNumber("0");
  if (!tab) return null;
  const r = selectionOrByte(tab);
  const inside = to.value !== null && to.value >= r.start && to.value <= r.end;
  return (
    <Modal
      title="Переместить блок"
      footer={
        <>
          <button type="button" className="btn" onClick={closeDialog}>
            Отмена
          </button>
          <button
            type="button"
            className="btn pri"
            disabled={to.bad || to.value === null || inside || to.value > tab.info.len}
            onClick={() => {
              closeDialog();
              if (to.value !== null)
                A.edit({ op: "move", start: r.start, end: r.end, to: to.value });
            }}
          >
            Переместить
          </button>
        </>
      }
    >
      <div className="note">
        Блок 0x{hexN(r.start)}…0x{hexN(r.end)} · {fmtSize(r.end - r.start)}. Байты не копируются:
        перемещается ссылка на фрагмент.
      </div>
      <div className="row-f">
        <span>Куда (смещение до перемещения)</span>
        <input
          className={`inp${to.bad || inside ? " bad" : ""}`}
          value={to.text}
          onChange={(e) => to.setText(e.target.value)}
        />
      </div>
    </Modal>
  );
}

function FollowPointerDialog() {
  const [bits, setBits] = useState<8 | 16 | 32 | 64>(32);
  const [endian, setEndian] = useState<"little" | "big">("little");
  const base = useNumber("0");
  return (
    <Modal
      title="Перейти по указателю"
      footer={
        <>
          <button type="button" className="btn" onClick={closeDialog}>
            Отмена
          </button>
          <button
            type="button"
            className="btn pri"
            onClick={() => {
              closeDialog();
              A.followPointer(bits, endian, -(base.value ?? 0));
            }}
          >
            Перейти
          </button>
        </>
      }
    >
      <div className="row-f">
        <span>Разрядность</span>
        <select
          className="inp"
          value={bits}
          onChange={(e) => setBits(Number(e.target.value) as 8 | 16 | 32 | 64)}
        >
          {[8, 16, 32, 64].map((b) => (
            <option key={b} value={b}>
              {b} бит
            </option>
          ))}
        </select>
      </div>
      <div className="row-f">
        <span>Порядок байтов</span>
        <select
          className="inp"
          value={endian}
          onChange={(e) => setEndian(e.target.value as "little" | "big")}
        >
          <option value="little">Little-endian</option>
          <option value="big">Big-endian</option>
        </select>
      </div>
      <div className="row-f">
        <span>База (вычесть)</span>
        <input className="inp" value={base.text} onChange={(e) => base.setText(e.target.value)} />
      </div>
      <div className="note">Например, для виртуального адреса 0x400000 укажите базу 400000.</div>
    </Modal>
  );
}

function RecoveryDialog() {
  const [items, setItems] = useState<RecoveryItem[]>([]);
  useEffect(() => {
    api
      .recoveryList()
      .then(setItems)
      .catch(() => {});
  }, []);
  const restore = async (it: RecoveryItem) => {
    try {
      const info = await api.recoveryRestore(it.key);
      await A.adopt(info);
      setItems((l) => l.filter((x) => x.key !== it.key));
      toast(`Восстановлено: ${info.name}`);
    } catch (e) {
      A.report(e);
    }
  };
  return (
    <Modal
      title="Восстановление после сбоя"
      footer={
        <button type="button" className="btn pri" onClick={closeDialog}>
          Готово
        </button>
      }
    >
      {items.length === 0 ? (
        <div className="note">Несохранённых правок от прошлых запусков нет.</div>
      ) : (
        <>
          <div className="note">
            Hexa нашла правки, которые не были сохранены перед закрытием программы.
          </div>
          <div className="list">
            {items.map((it) => (
              <div
                key={it.key}
                className="li"
                style={{ gridTemplateColumns: "minmax(0,1fr) auto auto" }}
              >
                <span className="t">
                  <b>{it.name}</b>
                  <span>
                    {it.path ?? "новый файл"} · {relTime(it.savedMs)}
                  </span>
                </span>
                <button
                  type="button"
                  className="btn"
                  onClick={() =>
                    api
                      .recoveryDiscard(it.key)
                      .then(() => setItems((l) => l.filter((x) => x.key !== it.key)))
                  }
                >
                  Удалить
                </button>
                <button type="button" className="btn pri" onClick={() => restore(it)}>
                  Восстановить
                </button>
              </div>
            ))}
          </div>
        </>
      )}
    </Modal>
  );
}

function ConfirmDialog({ d }: { d: Extract<Dialog, { kind: "confirm" }> }) {
  return (
    <Modal
      title={d.title}
      footer={d.actions.map((a) => (
        <button
          type="button"
          key={a.label}
          className={`btn${a.primary ? " pri" : ""}${a.danger ? " danger" : ""}`}
          // biome-ignore lint/a11y/noAutofocus: default button of a confirmation
          autoFocus={a.primary}
          onClick={() => {
            closeDialog();
            Promise.resolve(a.run()).catch(A.report);
          }}
        >
          {a.label}
        </button>
      ))}
    >
      <div className="note" style={{ whiteSpace: "pre-wrap", fontSize: 13 }}>
        {d.body}
      </div>
    </Modal>
  );
}

function AboutDialog() {
  return (
    <Modal
      title="Hexa"
      footer={
        <button type="button" className="btn pri" onClick={closeDialog}>
          Закрыть
        </button>
      }
    >
      <div className="hero">
        <div className="mark" aria-hidden="true">
          {Array.from({ length: 9 }, (_, i) => (
            // biome-ignore lint/suspicious/noArrayIndexKey: decorative
            <span key={i} />
          ))}
        </div>
        <div>
          <b>Hexa 0.1.0</b>
          <div className="note">Кросс-платформенный hex-редактор для файлов любого размера.</div>
        </div>
      </div>
      <div className="note">
        Данные не покидают компьютер. Правки хранятся как дерево фрагментов: открытие мгновенное,
        копирование гигабайтов бесплатное, история — дерево с ветками.
      </div>
    </Modal>
  );
}

export function Dialogs() {
  const d = useUi((u) => u.dialog);
  if (!d) return null;
  switch (d.kind) {
    case "newFile":
      return <NewFileDialog />;
    case "fill":
      return <FillDialog />;
    case "insertBytes":
      return <InsertBytesDialog />;
    case "resize":
      return <ResizeDialog />;
    case "selectRange":
      return <SelectRangeDialog />;
    case "copyAs":
      return <CopyAsDialog />;
    case "paste":
      return <PasteDialog text={d.text} />;
    case "props":
      return <PropsDialog />;
    case "replacePreview":
      return <ReplacePreviewDialog plan={d.plan} />;
    case "pickTab":
      return <PickTabDialog mode={d.mode ?? "side"} />;
    case "merge3":
      return <Merge3Dialog />;
    case "patchCreate":
      return <PatchCreateDialog />;
    case "patchApply":
      return <PatchApplyDialog path={d.path} />;
    case "mark":
      return <MarkDialog mark={d.mark} />;
    case "moveBlock":
      return <MoveBlockDialog />;
    case "followPointer":
      return <FollowPointerDialog />;
    case "confirm":
      return <ConfirmDialog d={d} />;
    case "recovery":
      return <RecoveryDialog />;
    case "about":
      return <AboutDialog />;
    case "settings":
      return <SettingsDialog section={d.section} />;
    default:
      return null;
  }
}
