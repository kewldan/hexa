// Geometry of the grid columns (DISP-01…07).
import type { CellView, Column, Settings } from "../state/settings";

export interface GridLayout {
  /** Bytes per row. */
  bpr: number;
  /** Bytes per cell (multi-byte cells, DISP-05). */
  cellBytes: number;
  cellChars: number;
  cw: number;
  rowH: number;
  padX: number;
  offsetDigits: number;
  offsetX: number;
  offsetW: number;
  hexX: number;
  hexW: number;
  textX: number;
  textW: number;
  textCharW: number;
  /** x of each cell's left edge relative to hexX. */
  cellX: number[];
  cellW: number;
  totalW: number;
  columns: Column[];
}

/** Characters needed to show one cell. */
export function cellChars(view: CellView, bytes: number): number {
  const bits = bytes * 8;
  switch (view) {
    case "hex":
      return bytes * 2;
    case "hexii":
      return 2;
    case "dec":
      return String(2 ** bits - 1).length;
    case "oct":
      return Math.ceil(bits / 3);
    case "bin":
      return bits;
    case "signed":
      return String(2 ** (bits - 1)).length + 1;
  }
}

interface Input {
  s: Pick<Settings, "bytesPerRow" | "group" | "cellView" | "cellWidth" | "columns" | "offsetBase">;
  cw: number;
  rowH: number;
  width: number;
  offsetDigits: number;
}

function measure(i: Input, bpr: number): Omit<GridLayout, "totalW"> & { totalW: number } {
  const { s, cw } = i;
  const cellBytes =
    s.cellView === "hexii" ? 1 : Math.min(s.cellWidth, bpr) > 0 ? Math.min(s.cellWidth, bpr) : 1;
  const chars = cellChars(s.cellView, cellBytes);
  const cellW = (chars + 0.55) * cw;
  const gap = 0.4 * cw;
  const groupGap = 1.25 * cw;
  const cells = Math.ceil(bpr / cellBytes);
  const groupCells = Math.max(1, Math.round(s.group / cellBytes));
  const cellX: number[] = [];
  let x = 0;
  for (let c = 0; c < cells; c++) {
    cellX.push(x);
    x += cellW + ((c + 1) % groupCells === 0 ? groupGap : gap);
  }
  const hexW = Math.max(0, x - groupGap);
  const textCharW = 1.3 * cw;
  const textW = bpr * textCharW;
  const padX = 20;
  const offsetW = (i.offsetDigits + 2) * cw;
  const pos: Record<Column, number> = { offset: 0, hex: 0, text: 0 };
  let cur = padX;
  for (const col of s.columns) {
    if (col === "offset") {
      pos.offset = cur;
      cur += offsetW;
    } else if (col === "hex") {
      pos.hex = cur;
      cur += hexW + 18;
    } else {
      pos.text = cur + 16;
      cur += 16 + textW + 18;
    }
  }
  return {
    bpr,
    cellBytes,
    cellChars: chars,
    cw,
    rowH: i.rowH,
    padX,
    offsetDigits: i.offsetDigits,
    offsetX: pos.offset,
    offsetW,
    hexX: pos.hex,
    hexW,
    textX: pos.text,
    textW,
    textCharW,
    cellX,
    cellW,
    totalW: cur + padX,
    columns: s.columns,
  };
}

export function computeLayout(i: Input): GridLayout {
  const want = i.s.bytesPerRow;
  if (typeof want === "number") return measure(i, Math.max(1, want));
  const step = Math.max(1, i.s.group, i.s.cellWidth);
  let best = step;
  if (want === "pow2") {
    for (let b = 1; b <= 1024; b *= 2) {
      if (measure(i, b).totalW <= i.width) best = b;
      else break;
    }
    return measure(i, Math.max(1, best));
  }
  for (let b = step; b <= 1024; b += step) {
    if (measure(i, b).totalW <= i.width) best = b;
    else break;
  }
  return measure(i, best);
}

/** Cell index under x (relative to the grid), or -1. */
export function hitHexCell(l: GridLayout, x: number): number {
  const rel = x - l.hexX;
  if (rel < -2 || rel > l.hexW + 2) return -1;
  let lo = 0;
  let hi = l.cellX.length - 1;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (l.cellX[mid] <= rel + 2) lo = mid;
    else hi = mid - 1;
  }
  return lo;
}

export function hitTextByte(l: GridLayout, x: number): number {
  if (!l.columns.includes("text")) return -1;
  const rel = x - l.textX;
  if (rel < -4 || rel > l.textW + 4) return -1;
  return Math.max(0, Math.min(l.bpr - 1, Math.floor(rel / l.textCharW)));
}
