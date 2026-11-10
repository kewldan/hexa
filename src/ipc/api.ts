// Typed wrappers around the Tauri commands of `src-tauri`.
import { Channel, invoke } from "@tauri-apps/api/core";

export type EditMode = "readOnly" | "overwrite" | "insert";
export type ShareMode = "shared" | "denyWrite" | "exclusive";
export type Endian = "little" | "big";

export interface DocInfo {
  id: number;
  name: string;
  path: string | null;
  len: number;
  baseLen: number;
  mode: EditMode;
  fixedSize: boolean;
  modified: boolean;
  canUndo: boolean;
  canRedo: boolean;
  writeDenied: boolean;
  version: number;
  pieceCount: number;
  follow: boolean;
  autoReload: boolean;
}

export interface Change {
  start: number;
  removed: number;
  inserted: number;
}

export interface EditResult {
  change: Change | null;
  info: DocInfo;
}

export type ChangeKind = "modified" | "inserted" | "deleted";
export interface ChangeSpan {
  start: number;
  end: number;
  kind: ChangeKind;
}

export type Fill =
  | { kind: "byte"; value: number }
  | { kind: "pattern"; bytes: number[] }
  | { kind: "random"; seed: number }
  | { kind: "sequence"; start: number; step: number; width: number; bigEndian: boolean };

export type EditOp =
  | { op: "overwrite"; pos: number; bytes: number[] }
  | { op: "insert"; pos: number; bytes: number[] }
  | { op: "delete"; start: number; end: number }
  | { op: "replace"; start: number; end: number; bytes: number[] }
  | { op: "fill"; start: number; end: number; fill: Fill }
  | { op: "insertFill"; pos: number; len: number; fill: Fill }
  | { op: "insertFile"; pos: number; path: string }
  | { op: "resize"; len: number; fill: Fill }
  | { op: "byteSwap"; start: number; end: number; width: number }
  | { op: "reverse"; start: number; end: number }
  | { op: "move"; start: number; end: number; to: number };

export interface HistoryItem {
  id: number;
  parent: number | null;
  label: string;
  timeMs: number;
  current: boolean;
  saved: boolean;
  onRedoPath: boolean;
}

export type SaveStrategy = "auto" | "atomic" | "inPlace";
export interface SaveOptions {
  strategy: SaveStrategy;
  backup: boolean;
  journal: boolean;
  force: boolean;
}
export interface SaveReport {
  strategy: SaveStrategy;
  bytesWritten: number;
}

export interface CmdError {
  kind: string;
  message: string;
}

export function isCmdError(e: unknown): e is CmdError {
  return typeof e === "object" && e !== null && "kind" in e && "message" in e;
}

export function errorText(e: unknown): string {
  if (isCmdError(e)) return e.message;
  if (e instanceof Error) return e.message;
  return String(e);
}

export type NumType = "i8" | "u8" | "i16" | "u16" | "i32" | "u32" | "i64" | "u64" | "f32" | "f64";
export type Query =
  | { kind: "hex"; pattern: string }
  | { kind: "text"; text: string; encodings: string[]; caseSensitive: boolean }
  | { kind: "regex"; pattern: string; unicode: boolean }
  | {
      kind: "number";
      ty: NumType;
      endian: "little" | "big" | "both";
      value: string;
      max: string | null;
      tolerance: number | null;
    }
  | { kind: "sequence"; items: SeqItem[] };

export interface SeqItem {
  ty: NumType;
  bigEndian: boolean;
  value: string | null;
  max: string | null;
  tolerance: number | null;
}

export type MarkKind = "bookmark" | "label" | "annotation";
export interface Mark {
  id: number;
  kind: MarkKind;
  start: number;
  end: number;
  name: string;
  color: string;
  note: string;
  dataType: string | null;
  orphaned: boolean;
}

export interface ExtRow {
  key: string;
  group: string;
  label: string;
  value: string | null;
  size: number;
  swatch: string | null;
}

export type Replacement =
  | { kind: "bytes"; bytes: number[] }
  | { kind: "template"; template: number[] };

export interface Planned {
  offset: number;
  len: number;
  old: number[];
  new: number[];
}

export interface Scope {
  start: number;
  end: number;
  align: number;
}

export interface Match {
  offset: number;
  len: number;
}

export type SearchEvent =
  | { type: "batch"; matches: Match[] }
  | { type: "progress"; done: number; total: number }
  | { type: "done"; count: number; truncated: boolean }
  | { type: "error"; error: CmdError };

export interface AlgoInfo {
  id: string;
  name: string;
  group: "checksum" | "crc" | "crypto";
}

export interface CrcParams {
  width: number;
  poly: number;
  init: number;
  refin: boolean;
  refout: boolean;
  xorout: number;
}

export interface HashResult {
  id: string;
  value: string;
}

export type HashEvent =
  | { type: "progress"; done: number; total: number }
  | { type: "done"; results: HashResult[] }
  | { type: "error"; error: CmdError };

export type InspectType =
  | "binary"
  | "octal"
  | "hex"
  | "i8"
  | "u8"
  | "i16"
  | "u16"
  | "i24"
  | "u24"
  | "i32"
  | "u32"
  | "i48"
  | "u48"
  | "i64"
  | "u64"
  | "i128"
  | "u128"
  | "f16"
  | "bf16"
  | "f32"
  | "f64"
  | "ascii"
  | "utf8"
  | "utf16"
  | "utf32";

export interface InspectRow {
  ty: InspectType;
  label: string;
  value: string | null;
  size: number;
}

export interface EncodingInfo {
  id: string;
  name: string;
  group: string;
  kind: "singleByte" | "utf8" | "utf16Le" | "utf16Be" | "utf32Le" | "utf32Be";
}

export type CopyFormat =
  | { kind: "hexSpaced" }
  | { kind: "hex" }
  | { kind: "hex0x" }
  | { kind: "escaped" }
  | { kind: "c" }
  | { kind: "cpp" }
  | { kind: "cSharp" }
  | { kind: "rust" }
  | { kind: "go" }
  | { kind: "java" }
  | { kind: "kotlin" }
  | { kind: "swift" }
  | { kind: "javaScript" }
  | { kind: "typeScript" }
  | { kind: "python" }
  | { kind: "pascal" }
  | { kind: "visualBasic" }
  | { kind: "powerShell" }
  | { kind: "asm" }
  | { kind: "base64" }
  | { kind: "base32" }
  | { kind: "base85" }
  | { kind: "url" }
  | { kind: "uuencode" }
  | { kind: "xxd"; offset: number }
  | { kind: "html"; offset: number }
  | { kind: "rtf"; offset: number }
  | {
      kind: "template";
      prefix: string;
      item: string;
      separator: string;
      perLine: number;
      linePrefix: string;
      suffix: string;
    };

export interface PasteCandidate {
  kind: "xxd" | "hex" | "array" | "base64" | "text";
  label: string;
  bytes: number[];
}

export interface RecentItem {
  path: string;
  name: string;
  dir: string;
  size: number | null;
  openedMs: number;
  pinned: boolean;
  exists: boolean;
  head: number[];
}

export interface RecoveryItem {
  key: string;
  path: string | null;
  name: string;
  savedMs: number;
}

export interface FileProps {
  path: string | null;
  name: string;
  size: number;
  diskSize: number | null;
  createdMs: number | null;
  modifiedMs: number | null;
  accessedMs: number | null;
  readonly: boolean;
  permissions: string | null;
  owner: string | null;
  pieces: number;
  history: number;
  diskChanged: boolean;
}

export interface OpenOpts {
  mode?: EditMode;
  loadInMemory?: boolean;
  share?: ShareMode;
  fixedSize?: boolean;
}

export type NewDoc =
  | { kind: "empty" }
  | { kind: "sized"; len: number; fill: Fill }
  | { kind: "bytes"; bytes: number[]; name?: string };

export interface SaveProgress {
  done: number;
  total: number;
}

const noop = () => {};

function progressChannel(onProgress?: (p: SaveProgress) => void): Channel<SaveProgress> {
  const ch = new Channel<SaveProgress>();
  ch.onmessage = onProgress ?? noop;
  return ch;
}

export const api = {
  open: (path: string, opts?: OpenOpts) => invoke<DocInfo>("doc_open", { path, opts }),
  create: (kind: NewDoc) => invoke<DocInfo>("doc_new", { kind }),
  attach: (id: number) => invoke<DocInfo>("doc_attach", { id }),
  release: (id: number) => invoke<void>("doc_release", { id }),
  info: (id: number) => invoke<DocInfo>("doc_info", { id }),
  read: async (id: number, offset: number, len: number): Promise<Uint8Array> => {
    const buf = await invoke<ArrayBuffer | number[]>("doc_read", { id, offset, len });
    return buf instanceof ArrayBuffer ? new Uint8Array(buf) : Uint8Array.from(buf);
  },
  changes: (id: number, start: number, end: number) =>
    invoke<ChangeSpan[]>("doc_changes", { id, start, end }),
  typeByte: (id: number, pos: number, byte: number, amend: boolean) =>
    invoke<EditResult>("doc_type", { id, pos, byte, amend }),
  edit: (id: number, op: EditOp) => invoke<EditResult>("doc_edit", { id, op }),
  undo: (id: number) => invoke<EditResult>("doc_undo", { id }),
  redo: (id: number) => invoke<EditResult>("doc_redo", { id }),
  history: (id: number) => invoke<HistoryItem[]>("doc_history", { id }),
  historyGoto: (id: number, entry: number) => invoke<EditResult>("doc_history_goto", { id, entry }),
  historyRevert: (id: number, entry: number) =>
    invoke<EditResult>("doc_history_revert", { id, entry }),
  setMode: (id: number, mode: EditMode) => invoke<DocInfo>("doc_set_mode", { id, mode }),
  setFixed: (id: number, fixed: boolean) => invoke<DocInfo>("doc_set_fixed", { id, fixed }),
  save: (id: number, opts: SaveOptions, onProgress?: (p: SaveProgress) => void) =>
    invoke<{ report: SaveReport | null; info: DocInfo }>("doc_save", {
      id,
      opts,
      channel: progressChannel(onProgress),
    }),
  saveAs: (id: number, path: string, onProgress?: (p: SaveProgress) => void) =>
    invoke<{ report: null; info: DocInfo }>("doc_save_as", {
      id,
      path,
      channel: progressChannel(onProgress),
    }),
  saveCopy: (
    id: number,
    path: string,
    range?: { start: number; end: number },
    onProgress?: (p: SaveProgress) => void,
  ) =>
    invoke<void>("doc_save_copy", {
      id,
      path,
      start: range?.start ?? null,
      end: range?.end ?? null,
      channel: progressChannel(onProgress),
    }),
  saveElevated: (id: number) => invoke<DocInfo>("doc_save_elevated", { id }),
  reload: (id: number) => invoke<DocInfo>("doc_reload", { id }),
  keepMine: (id: number) => invoke<DocInfo>("doc_keep_mine", { id }),
  setWatchMode: (id: number, follow?: boolean, autoReload?: boolean) =>
    invoke<DocInfo>("doc_set_watch_mode", {
      id,
      follow: follow ?? null,
      autoReload: autoReload ?? null,
    }),
  props: (id: number) => invoke<FileProps>("doc_props", { id }),
  journalPending: (path: string) => invoke<boolean>("journal_pending", { path }),
  journalRollback: (path: string) => invoke<void>("journal_rollback", { path }),
  journalDiscard: (path: string) => invoke<void>("journal_discard", { path }),
  inspect: (id: number, pos: number, endian: Endian) =>
    invoke<InspectRow[]>("inspect", { id, pos, endian }),
  inspectorSet: (id: number, pos: number, ty: InspectType, text: string, endian: Endian) =>
    invoke<EditResult>("inspector_set", { id, pos, ty, text, endian }),

  marksList: (id: number) => invoke<Mark[]>("marks_list", { id }),
  marksAdd: (id: number, mark: Partial<Mark>) =>
    invoke<number>("marks_add", { id, mark: { id: 0, ...mark } }),
  marksUpdate: (id: number, mark: Mark) => invoke<boolean>("marks_update", { id, mark }),
  marksRemove: (id: number, mark: number) => invoke<boolean>("marks_remove", { id, mark }),
  marksSet: (id: number, marks: Partial<Mark>[]) => invoke<Mark[]>("marks_set", { id, marks }),
  inspectExt: (id: number, pos: number, len: number | null, endian: Endian, q: [number, number]) =>
    invoke<ExtRow[]>("inspect_ext", { id, pos, len, endian, qInt: q[0], qFrac: q[1] }),
  replacePlan: (
    id: number,
    query: Query,
    scope: Scope | null,
    replacement: Replacement,
    max?: number,
  ) => invoke<Planned[]>("replace_plan", { id, query, scope, replacement, max: max ?? null }),
  replaceApply: (id: number, plan: { offset: number; len: number; new: number[] }[]) =>
    invoke<EditResult>("replace_apply", { id, plan }),
  readTextFile: (path: string) => invoke<string>("read_text_file", { path }),
  writeTextFile: (path: string, content: string) =>
    invoke<void>("write_text_file", { path, content }),
  cancelJob: (job: number) => invoke<void>("job_cancel", { job }),
  searchNext: (
    id: number,
    query: Query,
    scope: Scope | null,
    cursor: number,
    backwards: boolean,
    wrap: boolean,
  ) => invoke<Match | null>("search_next", { id, query, scope, cursor, backwards, wrap }),
  searchAll: (
    id: number,
    query: Query,
    scope: Scope | null,
    max: number,
    onEvent: (e: SearchEvent) => void,
  ) => {
    const channel = new Channel<SearchEvent>();
    channel.onmessage = onEvent;
    return invoke<number>("search_all", { id, query, scope, max, channel });
  },
  hashAlgorithms: () => invoke<AlgoInfo[]>("hash_algorithms"),
  hashRun: (
    id: number,
    start: number,
    end: number,
    algorithms: string[],
    custom: CrcParams | null,
    onEvent: (e: HashEvent) => void,
  ) => {
    const channel = new Channel<HashEvent>();
    channel.onmessage = onEvent;
    return invoke<number>("hash_run", { id, start, end, algorithms, custom, channel });
  },
  hashQuick: (id: number, start: number, end: number, algorithm: string) =>
    invoke<string | null>("hash_quick", { id, start, end, algorithm }),
  encodings: () => invoke<EncodingInfo[]>("encodings"),
  encodingTable: (id: string) => invoke<(string | null)[]>("encoding_table", { id }),
  encodeText: (text: string, encoding: string) =>
    invoke<number[]>("encode_text", { text, encoding }),
  parseHex: (text: string) => invoke<number[]>("parse_hex", { text }),
  clipCopy: (id: number, start: number, end: number, format?: CopyFormat) =>
    invoke<{ text: string | null; len: number; written: boolean }>("clip_copy", {
      id,
      start,
      end,
      format: format ?? null,
    }),
  copyAs: (id: number, start: number, end: number, format: CopyFormat) =>
    invoke<string>("copy_as", { id, start, end, format }),
  clipInfo: () => invoke<{ len: number; text: string | null } | null>("clip_info"),
  clipPaste: (id: number, pos: number, end: number | null, overwrite: boolean) =>
    invoke<EditResult>("clip_paste", { id, pos, end, overwrite }),
  clipPasteAuto: (id: number, pos: number, end: number | null, overwrite: boolean) =>
    invoke<EditResult | null>("clip_paste_auto", { id, pos, end, overwrite }),
  clipParse: (text: string) => invoke<PasteCandidate[]>("clip_parse", { text }),

  settingsGet: () => invoke<Record<string, unknown>>("settings_get"),
  settingsSet: (value: unknown) => invoke<void>("settings_set", { value }),
  recentList: () => invoke<RecentItem[]>("recent_list"),
  recentPin: (path: string, pinned: boolean) => invoke<void>("recent_pin", { path, pinned }),
  recentRemove: (path: string) => invoke<void>("recent_remove", { path }),
  initialFiles: () => invoke<string[]>("app_initial_files"),
  newWindow: (docs?: number[]) => invoke<void>("window_new", { docs: docs ?? null }),
  workspaceSave: (path: string, content: string) =>
    invoke<void>("workspace_save", { path, content }),
  workspaceLoad: (path: string) => invoke<string>("workspace_load", { path }),
  recoveryList: () => invoke<RecoveryItem[]>("recovery_list"),
  recoveryRestore: (key: string) => invoke<DocInfo>("recovery_restore", { key }),
  recoveryDiscard: (key: string) => invoke<void>("recovery_discard", { key }),
};
