// Number and size formatting helpers.
import { locale } from "../i18n";

const HEX = "0123456789ABCDEF";

export function hex2(b: number, lower = false): string {
  const s = HEX[(b >> 4) & 15] + HEX[b & 15];
  return lower ? s.toLowerCase() : s;
}

export function hexN(n: number, width = 0, lower = false): string {
  let s = Math.trunc(n).toString(16).toUpperCase();
  if (lower) s = s.toLowerCase();
  return s.length < width ? "0".repeat(width - s.length) + s : s;
}

/** Thin-space grouped decimal: 1 234 567. */
export function groupDec(n: number): string {
  return String(Math.trunc(n)).replace(/\B(?=(\d{3})+(?!\d))/g, " ");
}

const UNITS = ["Б", "КБ", "МБ", "ГБ", "ТБ", "ПБ"];
const UNITS_EN = ["B", "KB", "MB", "GB", "TB", "PB"];

export function fmtSize(n: number): string {
  const en = locale() === "en";
  if (n < 1024) return en ? `${groupDec(n)} B` : `${groupDec(n)} Б`;
  let v = n;
  let i = 0;
  while (v >= 1024 && i < UNITS.length - 1) {
    v /= 1024;
    i++;
  }
  const num = v.toFixed(v < 10 ? 1 : 0);
  return en ? `${num} ${UNITS_EN[i]}` : `${num.replace(".", ",")} ${UNITS[i]}`;
}

export type OffsetBase = "hex" | "dec" | "oct" | "seg";

/** Segment:offset form, `SSSS:OOOO` for 64 KiB segments (DISP-06). */
export function fmtSegment(n: number, segSize: number): string {
  const size = Math.max(1, segSize);
  const offDigits = Math.max(4, hexN(size - 1).length);
  return `${hexN(Math.floor(n / size), 4)}:${hexN(n % size, offDigits)}`;
}

/** Offset text with the zero-padded prefix separated (for dimming). */
export function fmtOffset(
  n: number,
  base: OffsetBase,
  width: number,
  segSize = 0x10000,
): { z: string; v: string } {
  if (base === "seg") return { z: "", v: fmtSegment(n, segSize) };
  const raw = base === "hex" ? hexN(n) : base === "oct" ? n.toString(8) : String(n);
  const padded = raw.length < width ? "0".repeat(width - raw.length) + raw : raw;
  const firstNonZero = padded.search(/[^0]/);
  const cut = firstNonZero < 0 ? padded.length - 1 : firstNonZero;
  return { z: padded.slice(0, cut), v: padded.slice(cut) };
}

export function offsetWidth(maxOffset: number, base: OffsetBase, segSize = 0x10000): number {
  if (base === "seg") return fmtSegment(maxOffset, segSize).length;
  const digits =
    base === "hex"
      ? Math.max(1, Math.ceil(Math.log2(maxOffset + 1) / 4))
      : base === "oct"
        ? maxOffset.toString(8).length
        : String(maxOffset).length;
  return Math.max(base === "hex" ? 8 : 8, digits);
}

export function relTime(ms: number, now = Date.now()): string {
  const d = Math.max(0, now - ms) / 1000;
  if (locale() === "en") return relTimeEn(d, ms);
  if (d < 60) return "только что";
  if (d < 3600) return `${Math.floor(d / 60)} мин назад`;
  if (d < 86400) return `${Math.floor(d / 3600)} ч назад`;
  if (d < 86400 * 2) return "вчера";
  if (d < 86400 * 30) return `${Math.floor(d / 86400)} дн назад`;
  return new Date(ms).toLocaleDateString("ru-RU");
}

function relTimeEn(d: number, ms: number): string {
  if (d < 60) return "just now";
  if (d < 3600) return `${Math.floor(d / 60)} min ago`;
  if (d < 86400) return `${Math.floor(d / 3600)} h ago`;
  if (d < 86400 * 2) return "yesterday";
  if (d < 86400 * 30) return `${Math.floor(d / 86400)} d ago`;
  return new Date(ms).toLocaleDateString("en-US");
}

/** Byte category for colouring (DISP-13). */
export type ByteKind = 0 | 1 | 2 | 3 | 4;
export const K_NULL = 0;
export const K_PRINT = 1;
export const K_CTRL = 2;
export const K_HIGH = 3;
export const K_FF = 4;

export function byteKind(v: number): ByteKind {
  if (v === 0) return K_NULL;
  if (v === 0xff) return K_FF;
  if (v > 0x20 && v < 0x7f) return K_PRINT;
  if (v <= 0x20 || v === 0x7f) return K_CTRL;
  return K_HIGH;
}

export function parseHexBytes(text: string): number[] | null {
  const clean = text.replace(/0x/gi, "").replace(/[\s,:-]/g, "");
  if (clean.length === 0 || clean.length % 2 !== 0 || /[^0-9a-f]/i.test(clean)) return null;
  const out: number[] = [];
  for (let i = 0; i < clean.length; i += 2) out.push(Number.parseInt(clean.slice(i, i + 2), 16));
  return out;
}
