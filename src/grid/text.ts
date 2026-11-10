// Text column decoding (DISP-09…11).
import type { EncodingInfo } from "../ipc/api";
import type { NonPrintable } from "../state/settings";

export interface Glyph {
  /** Text to draw ("" for continuation bytes of a multi-byte character). */
  ch: string;
  /** Rendered dim: replacement for non-printable bytes. */
  dim: boolean;
}

const CONT: Glyph = { ch: "", dim: true };

function nonPrintable(b: number, mode: NonPrintable): Glyph {
  if (mode === "blank") return { ch: " ", dim: true };
  if (mode === "pictures") {
    if (b < 0x20) return { ch: String.fromCharCode(0x2400 + b), dim: true };
    if (b === 0x7f) return { ch: "␡", dim: true };
  }
  return { ch: "·", dim: true };
}

function printable(ch: string): boolean {
  const c = ch.codePointAt(0) ?? 0;
  return !(c < 0x20 || (c >= 0x7f && c < 0xa0) || c === 0xad || (c >= 0xfff0 && c <= 0xffff));
}

export type ByteAt = (offset: number) => number | undefined;

export class TextDecoder2 {
  private enc: EncodingInfo;
  private table: (string | null)[] | null;
  private mode: NonPrintable;

  constructor(enc: EncodingInfo, table: (string | null)[] | null, mode: NonPrintable) {
    this.enc = enc;
    this.table = table;
    this.mode = mode;
  }

  glyph(at: ByteAt, offset: number): Glyph {
    const b = at(offset);
    if (b === undefined) return { ch: " ", dim: true };
    switch (this.enc.kind) {
      case "singleByte": {
        const ch = this.table?.[b] ?? null;
        if (ch === null || !printable(ch)) return nonPrintable(b, this.mode);
        return { ch, dim: false };
      }
      case "utf8":
        return this.utf8(at, offset, b);
      case "utf16Le":
      case "utf16Be":
        return this.utf16(at, offset, this.enc.kind === "utf16Be");
      case "utf32Le":
      case "utf32Be":
        return this.utf32(at, offset, this.enc.kind === "utf32Be");
    }
  }

  private utf8(at: ByteAt, offset: number, b: number): Glyph {
    if (b < 0x80)
      return b < 0x20 || b === 0x7f
        ? nonPrintable(b, this.mode)
        : { ch: String.fromCharCode(b), dim: false };
    if ((b & 0xc0) === 0x80) {
      // Continuation: belongs to a lead byte up to 3 bytes back.
      for (let k = 1; k <= 3; k++) {
        const lead = at(offset - k);
        if (lead === undefined) break;
        const n = lead >= 0xf0 ? 4 : lead >= 0xe0 ? 3 : lead >= 0xc0 ? 2 : 0;
        if (n === 0) continue;
        if (n > k && this.utf8Seq(at, offset - k, n)) return CONT;
        break;
      }
      return nonPrintable(b, this.mode);
    }
    const n = b >= 0xf0 && b <= 0xf4 ? 4 : b >= 0xe0 ? 3 : b >= 0xc2 && b < 0xe0 ? 2 : 0;
    const ch = n ? this.utf8Seq(at, offset, n) : null;
    return ch && printable(ch) ? { ch, dim: false } : nonPrintable(b, this.mode);
  }

  private utf8Seq(at: ByteAt, offset: number, n: number): string | null {
    const bytes: number[] = [];
    for (let i = 0; i < n; i++) {
      const v = at(offset + i);
      if (v === undefined) return null;
      if (i > 0 && (v & 0xc0) !== 0x80) return null;
      bytes.push(v);
    }
    try {
      return new TextDecoder("utf-8", { fatal: true }).decode(new Uint8Array(bytes));
    } catch {
      return null;
    }
  }

  private unit16(at: ByteAt, o: number, be: boolean): number | undefined {
    const a = at(o);
    const b = at(o + 1);
    if (a === undefined || b === undefined) return undefined;
    return be ? (a << 8) | b : (b << 8) | a;
  }

  private utf16(at: ByteAt, offset: number, be: boolean): Glyph {
    const start = offset - (offset % 2);
    if (offset !== start) return CONT;
    const u = this.unit16(at, start, be);
    if (u === undefined) return nonPrintable(at(offset) ?? 0, this.mode);
    if (u >= 0xdc00 && u < 0xe000) {
      const prev = this.unit16(at, start - 2, be);
      if (prev !== undefined && prev >= 0xd800 && prev < 0xdc00) return CONT;
      return nonPrintable(u & 0xff, this.mode);
    }
    let ch: string;
    if (u >= 0xd800 && u < 0xdc00) {
      const lo = this.unit16(at, start + 2, be);
      if (lo === undefined || lo < 0xdc00 || lo >= 0xe000) return nonPrintable(u & 0xff, this.mode);
      ch = String.fromCharCode(u, lo);
    } else {
      ch = String.fromCharCode(u);
    }
    return printable(ch) ? { ch, dim: false } : nonPrintable(u & 0xff, this.mode);
  }

  private utf32(at: ByteAt, offset: number, be: boolean): Glyph {
    if (offset % 4 !== 0) return CONT;
    const b = [at(offset), at(offset + 1), at(offset + 2), at(offset + 3)];
    if (b.some((x) => x === undefined)) return nonPrintable(b[0] ?? 0, this.mode);
    const [a0, a1, a2, a3] = b as number[];
    const v = be
      ? ((a0 << 24) | (a1 << 16) | (a2 << 8) | a3) >>> 0
      : ((a3 << 24) | (a2 << 16) | (a1 << 8) | a0) >>> 0;
    if (v > 0x10ffff || (v >= 0xd800 && v < 0xe000)) return nonPrintable(a0, this.mode);
    const ch = String.fromCodePoint(v);
    return printable(ch) ? { ch, dim: false } : nonPrintable(a0, this.mode);
  }
}

/** Bytes typed in the text column (EDIT-02), encoded in the current encoding. */
export function encodeChar(
  ch: string,
  enc: EncodingInfo,
  table: (string | null)[] | null,
): number[] | null {
  switch (enc.kind) {
    case "singleByte": {
      const i = table?.indexOf(ch) ?? -1;
      return i >= 0 ? [i] : null;
    }
    case "utf8":
      return Array.from(new TextEncoder().encode(ch));
    case "utf16Le":
    case "utf16Be": {
      const out: number[] = [];
      for (let i = 0; i < ch.length; i++) {
        const u = ch.charCodeAt(i);
        if (enc.kind === "utf16Le") out.push(u & 0xff, u >> 8);
        else out.push(u >> 8, u & 0xff);
      }
      return out;
    }
    case "utf32Le":
    case "utf32Be": {
      const v = ch.codePointAt(0) ?? 0;
      const le = [v & 0xff, (v >> 8) & 0xff, (v >> 16) & 0xff, (v >>> 24) & 0xff];
      return enc.kind === "utf32Le" ? le : le.reverse();
    }
  }
}
