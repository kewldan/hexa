// .tbl character tables for ROM hacking (DISP-12).
//
// Lines look like `41=A`, `8140=あ` (multi-byte keys), `/FF=<end>` or `*FE`
// for end/newline markers. Comments start with `#` or `;`.
import type { ByteAt, Glyph } from "./text";

export interface TblTable {
  /** Hex key (upper case, 1–4 bytes) → text. */
  entries: Map<string, string>;
  maxLen: number;
}

export function parseTbl(src: string): TblTable {
  const entries = new Map<string, string>();
  let maxLen = 1;
  for (const raw of src.split(/\r?\n/)) {
    const line = raw.replace(/^﻿/, "");
    if (!line.trim() || line.startsWith("#") || line.startsWith(";")) continue;
    let m = /^[/*$]?([0-9a-fA-F]{2,8})=(.*)$/.exec(line);
    if (!m) {
      const end = /^[/*]([0-9a-fA-F]{2,8})$/.exec(line.trim());
      if (end)
        m = [line, end[1], line.startsWith("*") ? "↵" : "<end>"] as unknown as RegExpExecArray;
    }
    if (!m || m[1].length % 2 !== 0) continue;
    const key = m[1].toUpperCase();
    entries.set(key, m[2] === "" ? " " : m[2]);
    maxLen = Math.max(maxLen, key.length / 2);
  }
  return { entries, maxLen: Math.min(maxLen, 4) };
}

function hexAt(at: ByteAt, offset: number, n: number): string | null {
  let s = "";
  for (let i = 0; i < n; i++) {
    const b = at(offset + i);
    if (b === undefined) return null;
    s += b.toString(16).toUpperCase().padStart(2, "0");
  }
  return s;
}

/** Longest-match decoder; continuation bytes of a multi-byte entry are blank. */
export class TblDecoder {
  constructor(private table: TblTable) {}

  glyph(at: ByteAt, offset: number): Glyph {
    for (let back = 1; back < this.table.maxLen; back++) {
      for (let n = this.table.maxLen; n > back; n--) {
        const k = hexAt(at, offset - back, n);
        if (k && this.table.entries.has(k)) return { ch: "", dim: true };
      }
    }
    for (let n = this.table.maxLen; n >= 1; n--) {
      const k = hexAt(at, offset, n);
      const v = k ? this.table.entries.get(k) : undefined;
      if (v !== undefined) return { ch: v.length > 2 ? `${v.slice(0, 1)}…` : v, dim: false };
    }
    return { ch: "·", dim: true };
  }
}
