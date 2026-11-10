// Editor operations shared by keyboard shortcuts, menus and the palette.
import { readText, writeText } from "@tauri-apps/plugin-clipboard-manager";
import { open as openFileDialog, save as saveFileDialog } from "@tauri-apps/plugin-dialog";
import { cacheFor, dropCache } from "../grid/cache";
import { encodeChar } from "../grid/text";
import {
  api,
  type CopyFormat,
  type DocInfo,
  type EditMode,
  type EditResult,
  type EncodingInfo,
  errorText,
  isCmdError,
  type Match,
  type Query,
} from "../ipc/api";
import { fmtSize, hexN } from "../lib/format";
import { settings, useSettings } from "./settings";
import { activeTab, hasSelection, selection, selectionOrByte, type Tab, useTabs } from "./tabs";
import { confirm, openDialog, toast, useUi } from "./ui";

// ---------------------------------------------------------------- viewport

/** Grid geometry published by the grid component. */
export const viewport = { bpr: 16, rows: 30, cellBytes: 1 };

// ---------------------------------------------------------------- encodings

let encodingList: EncodingInfo[] = [];
const tables = new Map<string, (string | null)[]>();

export async function loadEncodings() {
  encodingList = await api.encodings();
  return encodingList;
}

export function encodings(): EncodingInfo[] {
  return encodingList;
}

export function encodingInfo(id: string): EncodingInfo {
  return encodingList.find((e) => e.id === id) ?? { id, name: id, group: "", kind: "singleByte" };
}

export async function encodingTable(id: string): Promise<(string | null)[] | null> {
  if (encodingInfo(id).kind !== "singleByte") return null;
  const hit = tables.get(id);
  if (hit) return hit;
  const t = await api.encodingTable(id);
  tables.set(id, t);
  return t;
}

export function cachedTable(id: string): (string | null)[] | null {
  return tables.get(id) ?? null;
}

// ---------------------------------------------------------------- helpers

export function report(e: unknown) {
  if (isCmdError(e) && e.kind === "cancelled") return;
  toast(humanError(e), true);
}

function humanError(e: unknown): string {
  if (isCmdError(e)) {
    switch (e.kind) {
      case "readOnly":
        return "Документ только для чтения — переключите режим (Insert)";
      case "fixedSize":
        return "Размер файла фиксирован: правка изменила бы длину";
      case "busy":
        return "Файл сейчас сохраняется";
      case "outOfRange":
        return "За пределами файла";
      default:
        return e.message;
    }
  }
  return errorText(e);
}

function tab(id?: number): Tab | undefined {
  if (id === undefined) return activeTab();
  return useTabs.getState().tabs.find((t) => t.id === id);
}

function patch(id: number, p: Partial<Tab> | ((t: Tab) => Partial<Tab>)) {
  useTabs.getState().update(id, p);
}

export function applyInfo(info: DocInfo) {
  const c = cacheFor(info.id);
  c.sync(info.len, info.version);
  useTabs.getState().setInfo(info);
}

/** Applies an edit result: invalidates the cache, updates the tab. */
export function applyResult(id: number, r: EditResult, cursor?: number, anchor?: number) {
  cacheFor(id).applyChange(r.change, r.info.len, r.info.version);
  useTabs.getState().setInfo(r.info);
  if (cursor !== undefined) {
    patch(id, { cursor, anchor: anchor ?? cursor, nibble: 0 });
    scrollIntoView(id, cursor);
  }
  refreshHistory();
}

let historyListeners = new Set<() => void>();
export function onHistoryChange(fn: () => void) {
  historyListeners.add(fn);
  return () => {
    historyListeners.delete(fn);
  };
}
function refreshHistory() {
  for (const fn of historyListeners) fn();
}
export function resetHistoryListeners() {
  historyListeners = new Set();
}

// ---------------------------------------------------------------- scrolling & cursor

export function scrollIntoView(id: number, offset: number, center = false) {
  const t = tab(id);
  if (!t) return;
  const row = Math.floor(offset / viewport.bpr);
  const rows = Math.max(1, viewport.rows - 1);
  let top = t.topRow;
  if (center && (row < top || row >= top + rows)) top = row - Math.floor(rows / 2);
  else if (row < top) top = row;
  else if (row >= top + rows) top = row - rows + 1;
  const maxTop = Math.max(0, Math.ceil((t.info.len + 1) / viewport.bpr) - rows);
  top = Math.max(0, Math.min(top, maxTop));
  if (top !== t.topRow) patch(id, { topRow: top });
}

export function setCursor(offset: number, extend = false, opts: { jump?: boolean } = {}) {
  const t = activeTab();
  if (!t) return;
  const max = t.info.len;
  const cursor = Math.max(0, Math.min(Math.round(offset), max));
  patch(t.id, (cur) => ({
    cursor,
    anchor: extend ? cur.anchor : cursor,
    nibble: 0,
    jumpSeq: opts.jump ? cur.jumpSeq + 1 : cur.jumpSeq,
  }));
  commitCellBuffer();
  scrollIntoView(t.id, cursor, opts.jump);
}

/** Jump with navigation history (NAV-03). */
export function jumpTo(offset: number, length = 0) {
  const t = activeTab();
  if (!t) return;
  const target = Math.max(0, Math.min(offset, t.info.len));
  patch(t.id, (cur) => ({
    back: [...cur.back.slice(-99), cur.cursor],
    forward: [],
    flash: { start: target, end: target + Math.max(1, length), seq: (cur.flash?.seq ?? 0) + 1 },
  }));
  if (length > 0) {
    patch(t.id, (cur) => ({
      cursor: target + length - 1,
      anchor: target,
      nibble: 0,
      jumpSeq: cur.jumpSeq + 1,
    }));
    scrollIntoView(t.id, target, true);
  } else {
    setCursor(target, false, { jump: true });
  }
}

export function navBack() {
  const t = activeTab();
  if (!t || t.back.length === 0) return;
  const to = t.back[t.back.length - 1];
  patch(t.id, (cur) => ({ back: cur.back.slice(0, -1), forward: [...cur.forward, cur.cursor] }));
  setCursor(to, false, { jump: true });
}

export function navForward() {
  const t = activeTab();
  if (!t || t.forward.length === 0) return;
  const to = t.forward[t.forward.length - 1];
  patch(t.id, (cur) => ({ forward: cur.forward.slice(0, -1), back: [...cur.back, cur.cursor] }));
  setCursor(to, false, { jump: true });
}

export function moveBy(delta: number, extend: boolean) {
  const t = activeTab();
  if (!t) return;
  setCursor(t.cursor + delta, extend);
}

export function moveRows(rows: number, extend: boolean) {
  moveBy(rows * viewport.bpr, extend);
}

export function moveHome(extend: boolean, file = false) {
  const t = activeTab();
  if (!t) return;
  setCursor(file ? 0 : t.cursor - (t.cursor % viewport.bpr), extend);
}

export function moveEnd(extend: boolean, file = false) {
  const t = activeTab();
  if (!t) return;
  const target = file
    ? t.info.len
    : Math.min(t.cursor - (t.cursor % viewport.bpr) + viewport.bpr - 1, t.info.len);
  setCursor(target, extend);
}

/** Nibble movement (NAV-07). */
export function moveNibble(dir: 1 | -1) {
  const t = activeTab();
  if (t?.column !== "hex") return moveBy(dir, false);
  if (dir > 0) {
    if (t.nibble === 0) patch(t.id, { nibble: 1, anchor: t.cursor });
    else setCursor(t.cursor + 1);
  } else if (t.nibble === 1) {
    patch(t.id, { nibble: 0 });
  } else if (t.cursor > 0) {
    setCursor(t.cursor - 1);
    patch(t.id, { nibble: 1 });
  }
}

export function switchColumn() {
  const t = activeTab();
  if (!t) return;
  commitCellBuffer();
  patch(t.id, { column: t.column === "hex" ? "text" : "hex", nibble: 0 });
}

export function selectAll() {
  const t = activeTab();
  if (!t || t.info.len === 0) return;
  patch(t.id, { anchor: 0, cursor: t.info.len - 1, nibble: 0 });
}

export function selectRow() {
  const t = activeTab();
  if (!t) return;
  const start = t.cursor - (t.cursor % viewport.bpr);
  const end = Math.min(start + viewport.bpr, t.info.len) - 1;
  if (end >= start) patch(t.id, { anchor: start, cursor: end });
}

export function selectRange(start: number, end: number) {
  const t = activeTab();
  if (!t) return;
  const s = Math.max(0, Math.min(start, t.info.len));
  const e = Math.max(s, Math.min(end, t.info.len));
  if (e <= s) return setCursor(s);
  patch(t.id, { anchor: s, cursor: e - 1, nibble: 0 });
  scrollIntoView(t.id, s, true);
}

// ---------------------------------------------------------------- files

export async function openPaths(paths: string[], opts: { readOnly?: boolean } = {}) {
  for (const path of paths) {
    try {
      if (await api.journalPending(path)) {
        await new Promise<void>((resolve) =>
          confirm(
            "Прерванное сохранение",
            `Сохранение «${path}» было прервано. Hexa может откатить файл к состоянию до сохранения.`,
            [
              { label: "Оставить как есть", run: () => api.journalDiscard(path).then(resolve) },
              {
                label: "Откатить",
                primary: true,
                run: () =>
                  api
                    .journalRollback(path)
                    .then(() => toast("Файл восстановлен"))
                    .then(resolve),
              },
            ],
          ),
        );
      }
      const s = settings();
      const existing = useTabs.getState().tabs.find((t) => t.info.path === path);
      if (existing) {
        useTabs.getState().activate(existing.id);
        continue;
      }
      const info = await api.open(path, {
        mode: opts.readOnly ? "readOnly" : s.defaultMode,
        share: s.share,
      });
      await adopt(info);
      if (s.loadInMemoryMb > 0 && info.len <= s.loadInMemoryMb * 1024 * 1024) {
        // Small files are fast either way; the option only matters for paging.
      }
    } catch (e) {
      report(e);
    }
  }
}

/** Shows a document that exists on the Rust side in this window. */
export async function adopt(info: DocInfo) {
  const attached = await api.attach(info.id);
  cacheFor(attached.id).sync(attached.len, attached.version);
  useTabs.getState().add(attached);
  return attached;
}

export async function openWithDialog(readOnly = false) {
  const picked = await openFileDialog({ multiple: true, directory: false });
  if (!picked) return;
  await openPaths(Array.isArray(picked) ? picked : [picked], { readOnly });
}

export async function newDocument(kind: Parameters<typeof api.create>[0]) {
  try {
    const info = await api.create(kind);
    await adopt(info);
  } catch (e) {
    report(e);
  }
}

export async function newFromClipboard() {
  try {
    const text = (await readText()) ?? "";
    const candidates = await api.clipParse(text);
    const best = candidates[0];
    await newDocument({ kind: "bytes", bytes: best?.bytes ?? [] });
    if (best) toast(`Создан из буфера: ${best.label}, ${fmtSize(best.bytes.length)}`);
  } catch (e) {
    report(e);
  }
}

async function release(id: number) {
  useTabs.getState().remove(id);
  dropCache(id);
  await api.release(id).catch(() => {});
}

export async function closeTab(id?: number) {
  const t = tab(id);
  if (!t) return;
  if (t.info.modified && settings().confirmClose) {
    confirm(`Сохранить изменения в «${t.info.name}»?`, "Несохранённые правки будут потеряны.", [
      { label: "Не сохранять", danger: true, run: () => release(t.id) },
      { label: "Отмена", run: () => {} },
      {
        label: "Сохранить",
        primary: true,
        run: async () => {
          if (await save(t.id)) await release(t.id);
        },
      },
    ]);
    return;
  }
  await release(t.id);
}

/** Moves a tab to a new window (FILE-09 tear-off). */
export async function tearOff(id?: number) {
  const t = tab(id);
  if (!t) return;
  await api.newWindow([t.id]);
  // The new window attaches before we release, so the document survives.
  setTimeout(() => release(t.id), 600);
}

function withProgress<T>(label: string, run: (p: (d: number, t: number) => void) => Promise<T>) {
  const ui = useUi.getState();
  const timer = setTimeout(() => ui.set({ progress: { label, done: 0, total: 1 } }), 250);
  const progress = (done: number, total: number) =>
    useUi.getState().set({ progress: { label, done, total } });
  return run(progress).finally(() => {
    clearTimeout(timer);
    useUi.getState().set({ progress: null });
  });
}

export async function save(id?: number, force = false): Promise<boolean> {
  const t = tab(id);
  if (!t) return false;
  if (!t.info.path) return saveAs(t.id);
  try {
    const opts = { ...settings().save, force };
    const r = await withProgress("Сохранение", (p) =>
      api.save(t.id, opts, (x) => p(x.done, x.total)),
    );
    applyInfo(r.info);
    cacheFor(t.id).clear();
    const how = r.report?.strategy === "inPlace" ? "на месте" : "атомарно";
    toast(`Сохранено ${how}${r.report ? ` · записано ${fmtSize(r.report.bytesWritten)}` : ""}`);
    refreshHistory();
    return true;
  } catch (e) {
    if (isCmdError(e) && e.kind === "externallyModified") {
      confirm("Файл изменён на диске", "Другая программа изменила файл после открытия.", [
        { label: "Отмена", run: () => {} },
        { label: "Перезаписать", danger: true, run: () => void save(t.id, true) },
      ]);
    } else if (isCmdError(e) && e.kind === "permission") {
      confirm("Нет прав на запись", `${e.message}\n\nСохранить с правами администратора?`, [
        { label: "Отмена", run: () => {} },
        { label: "Повысить права", primary: true, run: () => void saveElevated(t.id) },
      ]);
    } else report(e);
    return false;
  }
}

export async function saveAs(id?: number): Promise<boolean> {
  const t = tab(id);
  if (!t) return false;
  const path = await saveFileDialog({ defaultPath: t.info.path ?? t.info.name });
  if (!path) return false;
  try {
    const r = await withProgress("Сохранение", (p) =>
      api.saveAs(t.id, path, (x) => p(x.done, x.total)),
    );
    applyInfo(r.info);
    cacheFor(t.id).clear();
    toast(`Сохранено как ${r.info.name}`);
    refreshHistory();
    return true;
  } catch (e) {
    report(e);
    return false;
  }
}

export async function saveCopy(selectionOnly = false) {
  const t = activeTab();
  if (!t) return;
  const range = selectionOnly ? selectionOrByte(t) : undefined;
  const path = await saveFileDialog({
    defaultPath: selectionOnly ? `${t.info.name}.part` : `${t.info.name}.copy`,
  });
  if (!path) return;
  try {
    await withProgress("Запись", (p) => api.saveCopy(t.id, path, range, (x) => p(x.done, x.total)));
    toast(
      selectionOnly
        ? `Выделение сохранено (${fmtSize(range!.end - range!.start)})`
        : "Копия сохранена",
    );
  } catch (e) {
    report(e);
  }
}

export async function saveElevated(id?: number) {
  const t = tab(id);
  if (!t) return;
  try {
    const info = await api.saveElevated(t.id);
    applyInfo(info);
    cacheFor(t.id).clear();
    patch(t.id, { banner: null });
    toast("Сохранено с правами администратора");
  } catch (e) {
    report(e);
  }
}

export async function reload(id?: number) {
  const t = tab(id);
  if (!t) return;
  try {
    const info = await api.reload(t.id);
    cacheFor(t.id).clear();
    applyInfo(info);
    patch(t.id, { banner: null });
    refreshHistory();
  } catch (e) {
    report(e);
  }
}

export async function keepMine(id?: number) {
  const t = tab(id);
  if (!t) return;
  const info = await api.keepMine(t.id);
  applyInfo(info);
  patch(t.id, { banner: null });
}

export async function setWatch(follow?: boolean, autoReload?: boolean) {
  const t = activeTab();
  if (!t) return;
  const info = await api.setWatchMode(t.id, follow, autoReload);
  applyInfo(info);
}

// ---------------------------------------------------------------- modes

export async function setMode(mode: EditMode) {
  const t = activeTab();
  if (!t) return;
  try {
    applyInfo(await api.setMode(t.id, mode));
  } catch (e) {
    report(e);
  }
}

export function toggleInsert() {
  const t = activeTab();
  if (!t) return;
  if (t.info.mode === "readOnly") return setMode("overwrite");
  return setMode(t.info.mode === "insert" ? "overwrite" : "insert");
}

export async function toggleFixed() {
  const t = activeTab();
  if (!t) return;
  applyInfo(await api.setFixed(t.id, !t.info.fixedSize));
}

// ---------------------------------------------------------------- history

export async function undo() {
  const t = activeTab();
  if (!t) return;
  try {
    const r = await api.undo(t.id);
    applyResult(t.id, r, r.change ? r.change.start : undefined);
  } catch (e) {
    report(e);
  }
}

export async function redo() {
  const t = activeTab();
  if (!t) return;
  try {
    const r = await api.redo(t.id);
    applyResult(t.id, r, r.change ? r.change.start : undefined);
  } catch (e) {
    report(e);
  }
}

export async function historyGoto(entry: number) {
  const t = activeTab();
  if (!t) return;
  try {
    applyResult(t.id, await api.historyGoto(t.id, entry));
  } catch (e) {
    report(e);
  }
}

// ---------------------------------------------------------------- typing (EDIT-02)

/** Digits typed into a dec/oct/bin or multi-byte cell before it is written. */
export const cellBuffer = { text: "", listeners: new Set<() => void>() };
function setCellBuffer(text: string) {
  cellBuffer.text = text;
  for (const l of cellBuffer.listeners) l();
}

let typingQueue: Promise<unknown> = Promise.resolve();
function queue(fn: () => Promise<unknown>) {
  typingQueue = typingQueue.then(fn, fn);
  return typingQueue;
}

export function typeHexNibble(n: number) {
  const t0 = activeTab();
  if (!t0) return;
  const s = settings();
  if (s.cellView !== "hex" || s.cellWidth !== 1) return typeCellDigit(n.toString(16));
  queue(async () => {
    const t = activeTab();
    if (!t || t.id !== t0.id) return;
    if (t.info.mode === "readOnly") return report({ kind: "readOnly", message: "" });
    const c = cacheFor(t.id);
    try {
      if (hasSelection(t) && t.info.mode === "insert") {
        const sel = selection(t);
        await api.edit(t.id, { op: "delete", start: sel.start, end: sel.end });
        patch(t.id, { cursor: sel.start, anchor: sel.start });
      }
      const cur = activeTab()!;
      const pos = cur.cursor;
      if (cur.nibble === 0) {
        const old = pos < cur.info.len && cur.info.mode !== "insert" ? (c.byte(pos) ?? 0) : 0;
        const byte = (n << 4) | (old & 0x0f);
        const r = await api.typeByte(cur.id, pos, byte, false);
        applyResult(cur.id, r);
        patch(cur.id, { cursor: pos, anchor: pos, nibble: 1 });
      } else {
        const old = c.byte(pos) ?? 0;
        const byte = (old & 0xf0) | n;
        const r = await api.typeByte(cur.id, pos, byte, true);
        applyResult(cur.id, r);
        const next = Math.min(pos + 1, r.info.len);
        patch(cur.id, { cursor: next, anchor: next, nibble: 0 });
        scrollIntoView(cur.id, next);
      }
      editPop(pos);
    } catch (e) {
      report(e);
    }
  });
}

export function typeText(ch: string) {
  const t0 = activeTab();
  if (!t0) return;
  queue(async () => {
    const t = activeTab();
    if (!t || t.id !== t0.id) return;
    if (t.info.mode === "readOnly") return report({ kind: "readOnly", message: "" });
    const enc = encodingInfo(settings().encoding);
    const bytes = encodeChar(ch, enc, await encodingTable(enc.id));
    if (!bytes) return toast(`«${ch}» нельзя записать в ${enc.name}`, true);
    try {
      let pos = t.cursor;
      if (hasSelection(t) && t.info.mode === "insert") {
        const sel = selection(t);
        await api.edit(t.id, { op: "delete", start: sel.start, end: sel.end });
        pos = sel.start;
      }
      let r: EditResult | undefined;
      for (const [i, b] of bytes.entries()) r = await api.typeByte(t.id, pos + i, b, false);
      if (r) {
        const next = Math.min(pos + bytes.length, r.info.len);
        applyResult(t.id, r, next);
        editPop(pos);
      }
    } catch (e) {
      report(e);
    }
  });
}

/** Typing into non-hex or multi-byte cells: digits collect until the cell is full. */
export function typeCellDigit(d: string) {
  const s = settings();
  const radix =
    s.cellView === "dec" || s.cellView === "signed"
      ? 10
      : s.cellView === "oct"
        ? 8
        : s.cellView === "bin"
          ? 2
          : 16;
  if (d === "-" && s.cellView === "signed" && cellBuffer.text === "") return setCellBuffer("-");
  if (Number.isNaN(Number.parseInt(d, radix))) return;
  const max = Math.ceil((s.cellWidth * 8) / Math.log2(radix)) + (s.cellView === "signed" ? 1 : 0);
  const text = cellBuffer.text + d;
  setCellBuffer(text);
  if (text.replace("-", "").length >= max) commitCellBuffer();
}

export function commitCellBuffer() {
  const text = cellBuffer.text;
  if (!text) return;
  setCellBuffer("");
  const t = activeTab();
  if (!t) return;
  const s = settings();
  const radix =
    s.cellView === "dec" || s.cellView === "signed"
      ? 10
      : s.cellView === "oct"
        ? 8
        : s.cellView === "bin"
          ? 2
          : 16;
  const w = s.cellWidth;
  let v: bigint;
  try {
    const neg = text.startsWith("-");
    const body = neg ? text.slice(1) : text;
    v = 0n;
    for (const ch of body) v = v * BigInt(radix) + BigInt(Number.parseInt(ch, radix));
    if (neg) v = -v;
  } catch {
    return;
  }
  const bits = BigInt(w * 8);
  const min = s.cellView === "signed" ? -(1n << (bits - 1n)) : 0n;
  const maxV = s.cellView === "signed" ? (1n << (bits - 1n)) - 1n : (1n << bits) - 1n;
  if (v < min || v > maxV) return toast(`${text} не помещается в ${w * 8} бит`, true);
  if (v < 0n) v += 1n << bits;
  const bytes: number[] = [];
  for (let i = 0; i < w; i++) bytes.push(Number((v >> BigInt(8 * i)) & 0xffn));
  if (s.cellEndian === "big") bytes.reverse();
  const pos = t.cursor - (t.cursor % w);
  queue(async () => {
    try {
      const r = await api.edit(t.id, { op: "overwrite", pos, bytes });
      applyResult(t.id, r, Math.min(pos + w, r.info.len));
      editPop(pos);
    } catch (e) {
      report(e);
    }
  });
}

export function cancelCellBuffer() {
  setCellBuffer("");
}

export async function deleteBytes(backward: boolean) {
  const t = activeTab();
  if (!t) return;
  let { start, end } = selection(t);
  if (end <= start) {
    if (backward) {
      if (t.cursor === 0) return;
      start = t.cursor - 1;
      end = t.cursor;
    } else {
      if (t.cursor >= t.info.len) return;
      start = t.cursor;
      end = t.cursor + 1;
    }
  }
  try {
    if (t.info.fixedSize) {
      // Fixed size: blank the bytes instead (CORE-20).
      const r = await api.edit(t.id, { op: "fill", start, end, fill: { kind: "byte", value: 0 } });
      applyResult(t.id, r, backward ? start : t.cursor);
      return;
    }
    const r = await api.edit(t.id, { op: "delete", start, end });
    applyResult(t.id, r, start);
  } catch (e) {
    report(e);
  }
}

// ---------------------------------------------------------------- clipboard (EDIT-03…17)

export async function copy(format?: CopyFormat, cut = false) {
  const t = activeTab();
  if (!t || t.info.len === 0) return;
  const { start, end } = selectionOrByte(t);
  try {
    const r = await api.clipCopy(t.id, start, end, format);
    if (r.text !== null) await writeText(r.text);
    if (cut) {
      await deleteBytes(false);
      toast(`Вырезано ${fmtSize(r.len)}`);
    } else {
      toast(
        r.text === null
          ? `Скопировано ${fmtSize(r.len)} (внутренний буфер — слишком много для текста)`
          : `Скопировано ${fmtSize(r.len)}`,
      );
    }
  } catch (e) {
    report(e);
  }
}

export async function copyAsText(format: CopyFormat) {
  const t = activeTab();
  if (!t) return;
  const { start, end } = selectionOrByte(t);
  try {
    const text = await api.copyAs(t.id, start, end, format);
    await writeText(text);
    toast(`Скопировано как ${format.kind}`);
  } catch (e) {
    report(e);
  }
}

/** Paste: internal pieces when the clipboard still holds Hexa's copy, else parsed text. */
export async function paste(mode: "auto" | "insert" | "overwrite" = "auto") {
  const t = activeTab();
  if (!t) return;
  if (t.info.mode === "readOnly") return report({ kind: "readOnly", message: "" });
  const overwrite = mode === "overwrite" || (mode === "auto" && t.info.mode !== "insert");
  const sel = selection(t);
  const hasSel = sel.end > sel.start;
  try {
    const [text, internal] = await Promise.all([readText().catch(() => null), api.clipInfo()]);
    if (internal && (internal.text === null || internal.text === text)) {
      const r = await api.clipPaste(
        t.id,
        hasSel ? sel.start : t.cursor,
        hasSel ? sel.end : null,
        overwrite,
      );
      const at = hasSel ? sel.start : t.cursor;
      applyResult(t.id, r, Math.min(at + internal.len, r.info.len));
      flashRange(at, at + internal.len);
      return;
    }
    if (!text) return toast("Буфер обмена пуст", true);
    const candidates = await api.clipParse(text);
    if (candidates.length > 1 && candidates[0].kind !== "hex") {
      openDialog({ kind: "paste", text });
      return;
    }
    await pasteBytes(candidates[0].bytes, overwrite);
  } catch (e) {
    report(e);
  }
}

export async function pasteBytes(bytes: number[], overwrite: boolean) {
  const t = activeTab();
  if (!t) return;
  const sel = selection(t);
  const at = sel.end > sel.start ? sel.start : t.cursor;
  try {
    const r =
      sel.end > sel.start
        ? await api.edit(t.id, { op: "replace", start: sel.start, end: sel.end, bytes })
        : overwrite
          ? await api.edit(t.id, { op: "overwrite", pos: at, bytes })
          : await api.edit(t.id, { op: "insert", pos: at, bytes });
    applyResult(t.id, r, Math.min(at + bytes.length, r.info.len));
    flashRange(at, at + bytes.length);
    toast(`Вставлено ${fmtSize(bytes.length)}`);
  } catch (e) {
    report(e);
  }
}

// ---------------------------------------------------------------- transforms (EDIT-05…09)

export async function edit(op: Parameters<typeof api.edit>[1], cursor?: number) {
  const t = activeTab();
  if (!t) return;
  try {
    const r = await api.edit(t.id, op);
    applyResult(t.id, r, cursor);
    if (r.change) flashRange(r.change.start, r.change.start + r.change.inserted);
  } catch (e) {
    report(e);
  }
}

export async function insertFile() {
  const t = activeTab();
  if (!t) return;
  const p = await openFileDialog({ multiple: false });
  if (!p || Array.isArray(p)) return;
  await edit({ op: "insertFile", pos: t.cursor, path: p }, t.cursor);
}

export async function inspectorSet(ty: Parameters<typeof api.inspectorSet>[2], text: string) {
  const t = activeTab();
  if (!t) return false;
  try {
    const r = await api.inspectorSet(t.id, t.cursor, ty, text, settings().inspectorEndian);
    applyResult(t.id, r);
    editPop(t.cursor);
    return true;
  } catch (e) {
    report(e);
    return false;
  }
}

// ---------------------------------------------------------------- effects

type FxListener = (fx: { kind: "pop" | "flash"; start: number; end: number }) => void;
const fxListeners = new Set<FxListener>();
export function onFx(fn: FxListener) {
  fxListeners.add(fn);
  return () => {
    fxListeners.delete(fn);
  };
}
export function editPop(at: number) {
  for (const l of fxListeners) l({ kind: "pop", start: at, end: at + 1 });
}
export function flashRange(start: number, end: number) {
  for (const l of fxListeners) l({ kind: "flash", start, end: Math.max(end, start + 1) });
}

// ---------------------------------------------------------------- search

let lastQuery: Query | null = null;

export function setLastQuery(q: Query | null) {
  lastQuery = q;
}

export async function findNext(backwards = false, query = lastQuery) {
  const t = activeTab();
  if (!t || !query) return null;
  try {
    const from = backwards
      ? t.cursor
      : hasSelection(t)
        ? selection(t).start + 1
        : t.cursor + (t.anchor === t.cursor ? 0 : 1);
    const m = await api.searchNext(t.id, query, null, from, backwards, true);
    if (!m) {
      toast("Не найдено", true);
      return null;
    }
    showMatch(m);
    return m;
  } catch (e) {
    report(e);
    return null;
  }
}

export function showMatch(m: Match) {
  jumpTo(m.offset, m.len);
  const t = activeTab();
  if (!t?.results) return;
  const idx = lowerBound(t.results.matches, m.offset);
  patch(t.id, (cur) => (cur.results ? { results: { ...cur.results, current: idx } } : {}));
}

export function lowerBound(arr: Match[], offset: number): number {
  let lo = 0;
  let hi = arr.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (arr[mid].offset < offset) lo = mid + 1;
    else hi = mid;
  }
  return lo;
}

export async function findAll(query: Query, label: string) {
  const t = activeTab();
  if (!t) return;
  const prev = t.results?.job;
  if (prev) api.cancelJob(prev).catch(() => {});
  lastQuery = query;
  patch(t.id, {
    results: {
      matches: [],
      current: -1,
      running: true,
      done: 0,
      total: t.info.len,
      truncated: false,
      job: null,
      label,
    },
  });
  let pending: Match[] = [];
  let flushTimer: ReturnType<typeof setTimeout> | undefined;
  const flush = () => {
    flushTimer = undefined;
    const add = pending;
    pending = [];
    patch(t.id, (cur) =>
      cur.results ? { results: { ...cur.results, matches: cur.results.matches.concat(add) } } : {},
    );
  };
  try {
    const job = await api.searchAll(t.id, query, null, 100_000, (e) => {
      if (e.type === "batch") {
        pending = pending.concat(e.matches);
        flushTimer ??= setTimeout(flush, 60);
      } else if (e.type === "progress") {
        patch(t.id, (cur) =>
          cur.results ? { results: { ...cur.results, done: e.done, total: e.total } } : {},
        );
      } else if (e.type === "done") {
        if (flushTimer) clearTimeout(flushTimer);
        flush();
        patch(t.id, (cur) =>
          cur.results
            ? { results: { ...cur.results, running: false, truncated: e.truncated, job: null } }
            : {},
        );
      } else {
        patch(t.id, (cur) =>
          cur.results ? { results: { ...cur.results, running: false, job: null } } : {},
        );
        report(e.error);
      }
    });
    patch(t.id, (cur) => (cur.results?.running ? { results: { ...cur.results, job } } : {}));
  } catch (e) {
    patch(t.id, { results: null });
    report(e);
  }
}

export function stepResult(dir: 1 | -1) {
  const t = activeTab();
  const r = t?.results;
  if (!t || !r || r.matches.length === 0) return findNext(dir < 0);
  const at = lowerBound(r.matches, t.cursor + (dir > 0 && hasSelection(t) ? 1 : 0));
  let idx = dir > 0 ? at : at - 1;
  if (dir > 0 && r.matches[idx]?.offset === t.cursor && !hasSelection(t)) idx++;
  if (idx >= r.matches.length) idx = 0;
  if (idx < 0) idx = r.matches.length - 1;
  showMatch(r.matches[idx]);
  return Promise.resolve(r.matches[idx]);
}

export function clearResults() {
  const t = activeTab();
  if (!t) return;
  if (t.results?.job) api.cancelJob(t.results.job).catch(() => {});
  patch(t.id, { results: null });
}

/** NAV-05: next/previous changed region. */
export async function nextChange(dir: 1 | -1) {
  const t = activeTab();
  if (!t) return;
  const spans = await api.changes(t.id, 0, t.info.len);
  const list = spans.map((s) => s.start);
  const target =
    dir > 0 ? list.find((s) => s > t.cursor) : list.reverse().find((s) => s < t.cursor);
  if (target === undefined) return toast(dir > 0 ? "Дальше изменений нет" : "Раньше изменений нет");
  jumpTo(target);
}

/** NAV-06: next zero / non-zero block, next byte different from the current. */
export async function nextBlock(kind: "zero" | "nonzero" | "different", backwards = false) {
  const t = activeTab();
  if (!t) return;
  const cur = cacheFor(t.id).byte(t.cursor) ?? 0;
  const pattern =
    kind === "zero" ? "\\x00{16,}" : kind === "nonzero" ? "[^\\x00]" : `[^\\x${hexN(cur, 2)}]`;
  // Skip the block we are in first.
  const q: Query = { kind: "regex", pattern, unicode: false };
  const m = await api
    .searchNext(t.id, q, null, backwards ? t.cursor : t.cursor + 1, backwards, false)
    .catch((e) => {
      report(e);
      return null;
    });
  if (!m) return toast("Не найдено", true);
  jumpTo(m.offset);
}

/** NAV-04: treat the value under the cursor as an offset. */
export function followPointer(bits: 8 | 16 | 32 | 64, endian: "little" | "big", base = 0) {
  const t = activeTab();
  if (!t) return;
  const c = cacheFor(t.id);
  const n = bits / 8;
  let v = 0n;
  for (let i = 0; i < n; i++) {
    const b = c.byte(t.cursor + i);
    if (b === undefined) return toast("Недостаточно байт под курсором", true);
    const shift = endian === "little" ? i : n - 1 - i;
    v |= BigInt(b) << BigInt(8 * shift);
  }
  const target = Number(v) + base;
  if (target > t.info.len || !Number.isSafeInteger(target)) {
    return toast(`Указатель 0x${v.toString(16).toUpperCase()} за пределами файла`, true);
  }
  jumpTo(target);
}

// ---------------------------------------------------------------- workspace (FILE-11)

export async function saveWorkspace() {
  const path = await saveFileDialog({ defaultPath: "project.hexa-workspace.json" });
  if (!path) return;
  const st = useTabs.getState();
  const s = settings();
  const ws = {
    version: 1,
    files: st.tabs
      .filter((t) => t.info.path)
      .map((t) => ({
        path: t.info.path,
        cursor: t.cursor,
        anchor: t.anchor,
        topRow: t.topRow,
        column: t.column,
      })),
    active: st.tabs.find((t) => t.id === st.active)?.info.path ?? null,
    view: {
      bytesPerRow: s.bytesPerRow,
      group: s.group,
      encoding: s.encoding,
      cellView: s.cellView,
      offsetBase: s.offsetBase,
    },
    layout: s.layout,
    notes: "",
  };
  try {
    await api.workspaceSave(path, JSON.stringify(ws, null, 2));
    toast("Проект сохранён — исходные файлы не тронуты");
  } catch (e) {
    report(e);
  }
}

export async function loadWorkspace() {
  const path = await openFileDialog({
    multiple: false,
    filters: [{ name: "Hexa workspace", extensions: ["json"] }],
  });
  if (!path || Array.isArray(path)) return;
  try {
    const ws = JSON.parse(await api.workspaceLoad(path));
    if (ws.view) useSettings.getState().set(ws.view);
    if (ws.layout) useSettings.getState().set({ layout: ws.layout });
    for (const f of ws.files ?? []) {
      await openPaths([f.path]);
      const t = useTabs.getState().tabs.find((x) => x.info.path === f.path);
      if (t)
        patch(t.id, {
          cursor: f.cursor ?? 0,
          anchor: f.anchor ?? 0,
          topRow: f.topRow ?? 0,
          column: f.column ?? "hex",
        });
    }
    const active = useTabs.getState().tabs.find((x) => x.info.path === ws.active);
    if (active) useTabs.getState().activate(active.id);
  } catch (e) {
    report(e);
  }
}
