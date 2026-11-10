// Shareable view state (IO-08): position, selection, bookmarks and view
// options without the file's bytes, as a `hexa://view?…` link or a file.
import type { Mark } from "../ipc/api";
import type { Tab } from "../state/tabs";

export interface ViewState {
  v: 1;
  name: string;
  len: number;
  cursor: number;
  anchor: number;
  topRow: number;
  marks: Partial<Mark>[];
  folds: { start: number; end: number }[];
}

export function captureView(t: Tab): ViewState {
  return {
    v: 1,
    name: t.info.name,
    len: t.info.len,
    cursor: t.cursor,
    anchor: t.anchor,
    topRow: t.topRow,
    marks: t.marks.map(({ id: _, orphaned: __, ...m }) => m),
    folds: t.folds.map((f) => ({ start: f.start, end: f.end })),
  };
}

function toB64(s: string): string {
  const bytes = new TextEncoder().encode(s);
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function fromB64(s: string): string {
  const bin = atob(s.replace(/-/g, "+").replace(/_/g, "/"));
  return new TextDecoder().decode(Uint8Array.from(bin, (c) => c.charCodeAt(0)));
}

export function viewToLink(v: ViewState): string {
  return `hexa://view?s=${toB64(JSON.stringify(v))}`;
}

/** Accepts a link, a bare encoded state or the JSON of a `.hexaview` file. */
export function parseView(input: string): ViewState {
  const s = input.trim();
  const json = s.startsWith("{") ? s : fromB64(s.replace(/^hexa:\/\/view\?s=/, ""));
  const v = JSON.parse(json) as ViewState;
  if (v.v !== 1 || typeof v.cursor !== "number") throw new Error("это не состояние вида Hexa");
  return v;
}
