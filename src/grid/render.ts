// Canvas renderer of the hex grid (DISP-01…28).
import type { ChangeSpan, Match } from "../ipc/api";
import { byteKind, fmtOffset, hex2 } from "../lib/format";
import type { Settings } from "../state/settings";
import type { DocCache } from "./cache";
import type { GridLayout } from "./layout";
import type { TextDecoder2 } from "./text";

export interface Palette {
  ed: string;
  ink: string;
  muted: string;
  line: string;
  rowcur: string;
  zebra: string;
  hov2: string;
  sel: string;
  colcur: string;
  match: string;
  matchc: string;
  same: string;
  kinds: string[]; // k0 kp kc kh kf
  mod: string;
  ins: string;
  del: string;
  acc: string;
}

function cssVar(st: CSSStyleDeclaration, name: string, fallback: string): string {
  const v = st.getPropertyValue(name).trim();
  return v || fallback;
}

/** `#rgb`, `#rrggbb` or `rgb()` with an alpha, without relying on color-mix() in canvas. */
export function withAlpha(color: string, a: number): string {
  const c = color.trim();
  let r = 0;
  let g = 0;
  let b = 0;
  if (c.startsWith("#")) {
    const h = c.length === 4 ? [...c.slice(1)].map((x) => x + x).join("") : c.slice(1, 7);
    r = Number.parseInt(h.slice(0, 2), 16);
    g = Number.parseInt(h.slice(2, 4), 16);
    b = Number.parseInt(h.slice(4, 6), 16);
  } else {
    const m = c.match(/[\d.]+/g);
    if (m) [r, g, b] = m.map(Number);
  }
  return `rgba(${r}, ${g}, ${b}, ${a})`;
}

export function readPalette(): Palette {
  const st = getComputedStyle(document.documentElement);
  const acc = cssVar(st, "--acc", "#2f6bf0");
  const mix = (color: string, pct: number) => withAlpha(color, pct / 100);
  return {
    ed: cssVar(st, "--ed", "#fff"),
    ink: cssVar(st, "--ink", "#111"),
    muted: cssVar(st, "--muted", "#666"),
    line: cssVar(st, "--line", "rgba(0,0,0,.1)"),
    rowcur: cssVar(st, "--rowcur", "rgba(0,0,0,.03)"),
    zebra: cssVar(st, "--zebra", "rgba(0,0,0,.02)"),
    hov2: cssVar(st, "--hov2", "rgba(0,0,0,.08)"),
    sel: mix(acc, 24),
    colcur: mix(acc, 6),
    match: cssVar(st, "--match", "rgba(250,204,21,.38)"),
    matchc: cssVar(st, "--matchc", "rgba(245,158,11,.8)"),
    same: mix(acc, 12),
    kinds: ["--k0", "--kp", "--kc", "--kh", "--kf"].map((k) => cssVar(st, k, "#333")),
    mod: cssVar(st, "--mod", "#c2410c"),
    ins: cssVar(st, "--ins", "#0f766e"),
    del: cssVar(st, "--del", "#dc2626"),
    acc,
  };
}

/** Pre-rendered "00".."FF" in every colour (glyph atlas). */
export class HexAtlas {
  readonly canvas: HTMLCanvasElement;
  readonly key: string;
  private cellW: number;
  private cellH: number;
  private dpr: number;

  constructor(
    font: string,
    colors: string[],
    cw: number,
    rowH: number,
    dpr: number,
    lower: boolean,
  ) {
    this.key = `${font}|${colors.join(",")}|${cw}|${rowH}|${dpr}|${lower}`;
    this.dpr = dpr;
    this.cellW = Math.ceil(cw * 2 + 2);
    this.cellH = Math.ceil(rowH);
    this.canvas = document.createElement("canvas");
    this.canvas.width = this.cellW * 256 * dpr;
    this.canvas.height = this.cellH * colors.length * dpr;
    const ctx = this.canvas.getContext("2d")!;
    ctx.scale(dpr, dpr);
    ctx.font = font;
    ctx.textBaseline = "middle";
    ctx.textAlign = "center";
    colors.forEach((color, ci) => {
      ctx.fillStyle = color;
      for (let b = 0; b < 256; b++) {
        ctx.fillText(
          hex2(b, lower),
          b * this.cellW + this.cellW / 2,
          ci * this.cellH + this.cellH / 2,
        );
      }
    });
  }

  draw(ctx: CanvasRenderingContext2D, b: number, color: number, cx: number, y: number) {
    const d = this.dpr;
    ctx.drawImage(
      this.canvas,
      b * this.cellW * d,
      color * this.cellH * d,
      this.cellW * d,
      this.cellH * d,
      cx - this.cellW / 2,
      y,
      this.cellW,
      this.cellH,
    );
  }
}

export interface RenderInput {
  ctx: CanvasRenderingContext2D;
  width: number;
  height: number;
  layout: GridLayout;
  s: Settings;
  pal: Palette;
  font: string;
  atlas: HexAtlas | null;
  cache: DocCache;
  decoder: TextDecoder2;
  len: number;
  topRow: number;
  scrollX: number;
  cursor: number;
  anchor: number;
  column: "hex" | "text";
  hover: number;
  matches: Match[];
  currentMatch: number;
  readOnly: boolean;
}

const F_SEL = 1;
const F_MATCH = 2;
const F_CUR_MATCH = 4;
const F_MOD = 8;
const F_INS = 16;
const F_SAME = 32;

function lowerBound(m: Match[], off: number): number {
  let lo = 0;
  let hi = m.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (m[mid].offset + m[mid].len <= off) lo = mid + 1;
    else hi = mid;
  }
  return lo;
}

function formatCell(view: Settings["cellView"], v: bigint, bytes: number, lower: boolean): string {
  const bits = BigInt(bytes * 8);
  switch (view) {
    case "hex": {
      const s = v.toString(16).padStart(bytes * 2, "0");
      return lower ? s : s.toUpperCase();
    }
    case "dec":
      return v.toString(10);
    case "oct":
      return v.toString(8).padStart(Math.ceil((bytes * 8) / 3), "0");
    case "bin":
      return v.toString(2).padStart(bytes * 8, "0");
    case "signed": {
      const sv = v >= 1n << (bits - 1n) ? v - (1n << bits) : v;
      return sv.toString(10);
    }
    case "hexii": {
      const b = Number(v);
      if (b === 0) return "  ";
      if (b === 0xff) return "##";
      if (b > 0x20 && b < 0x7f) return `.${String.fromCharCode(b)}`;
      return lower ? hex2(b, true) : hex2(b);
    }
  }
}

export function renderGrid(p: RenderInput): void {
  const { ctx, layout: l, s, pal, cache, len } = p;
  const rowH = l.rowH;
  ctx.save();
  ctx.fillStyle = pal.ed;
  ctx.fillRect(0, 0, p.width, p.height);
  ctx.translate(-p.scrollX, 0);
  ctx.font = p.font;
  ctx.textBaseline = "middle";

  const first = Math.floor(p.topRow);
  const frac = (p.topRow - first) * rowH;
  const rows = Math.ceil(p.height / rowH) + 1;
  const totalRows = Math.floor(len / l.bpr) + 1;
  const viewStart = first * l.bpr;
  const viewEnd = Math.min((first + rows) * l.bpr, len);
  cache.ensure(Math.max(0, viewStart - l.bpr * 8), viewEnd + l.bpr * 8);

  // Per-byte flags for the visible window.
  const n = Math.max(0, viewEnd - viewStart);
  const flags = new Uint8Array(n);
  const sel = { start: Math.min(p.cursor, p.anchor), end: Math.max(p.cursor, p.anchor) + 1 };
  const hasSel = p.cursor !== p.anchor;
  if (hasSel) {
    for (let o = Math.max(sel.start, viewStart); o < Math.min(sel.end, viewEnd); o++)
      flags[o - viewStart] |= F_SEL;
  }
  if (p.matches.length) {
    for (let i = lowerBound(p.matches, viewStart); i < p.matches.length; i++) {
      const m = p.matches[i];
      if (m.offset >= viewEnd) break;
      const f = i === p.currentMatch ? F_CUR_MATCH : F_MATCH;
      for (let o = Math.max(m.offset, viewStart); o < Math.min(m.offset + m.len, viewEnd); o++)
        flags[o - viewStart] |= f;
    }
  }
  let deletions: number[] = [];
  if (s.highlightChanges) {
    const spans: ChangeSpan[] = cache.changes(viewStart, viewEnd);
    for (const sp of spans) {
      if (sp.kind === "deleted") {
        if (sp.start >= viewStart && sp.start <= viewEnd) deletions.push(sp.start);
        continue;
      }
      const f = sp.kind === "modified" ? F_MOD : F_INS;
      for (let o = Math.max(sp.start, viewStart); o < Math.min(sp.end, viewEnd); o++)
        flags[o - viewStart] |= f;
    }
  }
  const curByte = cache.byte(p.cursor);
  if (s.highlightSame && curByte !== undefined) {
    for (let o = viewStart; o < viewEnd; o++)
      if (o !== p.cursor && cache.byte(o) === curByte) flags[o - viewStart] |= F_SAME;
  }
  deletions = deletions.filter((d, i, a) => a.indexOf(d) === i);

  const cursorRow = Math.floor(p.cursor / l.bpr);
  const cursorCol = p.cursor % l.bpr;
  const cellOf = (byteInRow: number) => Math.floor(byteInRow / l.cellBytes);
  const pad = 1.5;

  // Column highlight (DISP-18).
  if (s.highlightColumn && l.columns.includes("hex")) {
    ctx.fillStyle = pal.colcur;
    const cx = l.hexX + l.cellX[cellOf(cursorCol)];
    ctx.fillRect(cx, 0, l.cellW, p.height);
    if (l.columns.includes("text"))
      ctx.fillRect(l.textX + cursorCol * l.textCharW, 0, l.textCharW, p.height);
  }

  const widthAll = Math.max(p.width + p.scrollX, l.totalW);
  for (let r = 0; r < rows; r++) {
    const row = first + r;
    if (row >= totalRows) break;
    const y = r * rowH - frac;
    const rowOff = row * l.bpr;
    if (s.zebra && row % 2 === 1) {
      ctx.fillStyle = pal.zebra;
      ctx.fillRect(0, y, widthAll, rowH);
    }
    if (s.highlightRow && row === cursorRow) {
      ctx.fillStyle = pal.rowcur;
      ctx.fillRect(0, y, widthAll, rowH);
    }
    const mid = y + rowH / 2;

    // Offset column (DISP-06).
    if (l.columns.includes("offset")) {
      let shown = rowOff + s.baseAddress;
      let sign = "";
      if (s.offsetRelative) {
        const d = rowOff - p.cursor + (p.cursor % l.bpr);
        sign = d < 0 ? "-" : d > 0 ? "+" : " ";
        shown = Math.abs(d);
      }
      const { z, v } = fmtOffset(shown, s.offsetBase, l.offsetDigits);
      ctx.textAlign = "left";
      let x = l.offsetX;
      if (sign) {
        ctx.fillStyle = pal.muted;
        ctx.fillText(sign, x - l.cw, mid);
      }
      ctx.globalAlpha = 0.42;
      ctx.fillStyle = pal.muted;
      ctx.fillText(z, x, mid);
      ctx.globalAlpha = 1;
      x += ctx.measureText(z).width;
      ctx.fillText(v, x, mid);
    }

    const rowBytes = Math.max(0, Math.min(l.bpr, len - rowOff));

    // Hex column.
    if (l.columns.includes("hex")) {
      const cells = Math.ceil(rowBytes / l.cellBytes);
      for (let c = 0; c < cells; c++) {
        const o = rowOff + c * l.cellBytes;
        const x = l.hexX + l.cellX[c];
        let f = 0;
        for (let k = 0; k < l.cellBytes && o + k < viewEnd; k++) f |= flags[o + k - viewStart] ?? 0;
        const bg =
          f & F_CUR_MATCH
            ? pal.matchc
            : f & F_SEL
              ? pal.sel
              : f & F_MATCH
                ? pal.match
                : f & F_SAME
                  ? pal.same
                  : o === p.hover
                    ? pal.hov2
                    : null;
        if (bg) {
          ctx.fillStyle = bg;
          roundRect(ctx, x, y + pad, l.cellW, rowH - pad * 2, 4);
        }
        let value: bigint | null = 0n;
        for (let k = 0; k < l.cellBytes; k++) {
          const idx = s.cellEndian === "big" ? k : l.cellBytes - 1 - k;
          const b = o + idx < len ? cache.byte(o + idx) : 0;
          if (b === undefined) {
            value = null;
            break;
          }
          value = (value << 8n) | BigInt(b);
        }
        const cx = x + l.cellW / 2;
        if (value === null) {
          ctx.fillStyle = pal.line;
          ctx.fillRect(cx - l.cw * 0.6, mid - 1, l.cw * 1.2, 2);
          continue;
        }
        const changed = f & (F_MOD | F_INS);
        const kindColor = s.colorBytes && l.cellBytes === 1 ? byteKind(Number(value)) : 1;
        if (p.atlas && l.cellBytes === 1 && s.cellView === "hex" && !changed) {
          p.atlas.draw(ctx, Number(value), kindColor, cx, y);
        } else {
          ctx.textAlign = "center";
          ctx.fillStyle = changed ? (f & F_MOD ? pal.mod : pal.ins) : pal.kinds[kindColor];
          if (changed) ctx.font = `600 ${p.font.replace(/^\d+ /, "")}`;
          ctx.fillText(formatCell(s.cellView, value, l.cellBytes, s.lowercase), cx, mid);
          if (changed) ctx.font = p.font;
        }
        if (changed) {
          ctx.fillStyle = f & F_MOD ? pal.mod : pal.ins;
          ctx.fillRect(x + l.cellW * 0.22, y + rowH - pad - 3, l.cellW * 0.56, 2);
        }
      }
    }

    // Text column.
    if (l.columns.includes("text")) {
      ctx.textAlign = "center";
      for (let i = 0; i < rowBytes; i++) {
        const o = rowOff + i;
        const x = l.textX + i * l.textCharW;
        const f = flags[o - viewStart] ?? 0;
        const bg =
          f & F_CUR_MATCH
            ? pal.matchc
            : f & F_SEL
              ? pal.sel
              : f & F_MATCH
                ? pal.match
                : o === p.hover
                  ? pal.hov2
                  : null;
        if (bg) {
          ctx.fillStyle = bg;
          ctx.fillRect(x, y + pad, l.textCharW, rowH - pad * 2);
        }
        const g = p.decoder.glyph((q) => (q >= 0 && q < len ? cache.byte(q) : undefined), o);
        if (!g.ch) continue;
        ctx.fillStyle = f & F_MOD ? pal.mod : f & F_INS ? pal.ins : g.dim ? pal.muted : pal.ink;
        ctx.globalAlpha = g.dim ? 0.7 : 1;
        ctx.fillText(g.ch, x + l.textCharW / 2, mid);
        ctx.globalAlpha = 1;
      }
    }

    // Deletion markers.
    for (const d of deletions) {
      if (d < rowOff || d > rowOff + l.bpr) continue;
      const i = d - rowOff;
      if (i === l.bpr && d !== len) continue;
      ctx.fillStyle = pal.del;
      if (l.columns.includes("hex")) {
        const c = Math.min(cellOf(i), l.cellX.length);
        const x = c < l.cellX.length ? l.hexX + l.cellX[c] - 2 : l.hexX + l.hexW + 1;
        ctx.fillRect(x - 1, y + 3, 2, rowH - 6);
        ctx.beginPath();
        ctx.moveTo(x - 4, y + 2);
        ctx.lineTo(x + 3, y + 2);
        ctx.lineTo(x, y + 6);
        ctx.fill();
      }
      if (l.columns.includes("text"))
        ctx.fillRect(l.textX + i * l.textCharW - 1, y + 3, 2, rowH - 6);
    }
  }

  // Separator before the text column.
  if (l.columns.includes("text")) {
    ctx.fillStyle = pal.line;
    ctx.fillRect(l.textX - 16, 0, 1, p.height);
  }
  ctx.restore();
}

function roundRect(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  w: number,
  h: number,
  r: number,
) {
  ctx.beginPath();
  ctx.roundRect(x, y, w, h, r);
  ctx.fill();
}

/** Text for the cursor element (the byte value in the cursor's cell). */
export function cursorLabel(
  cache: DocCache,
  s: Settings,
  l: GridLayout,
  offset: number,
  len: number,
): string {
  if (offset >= len) return "";
  const start = offset - (offset % l.cellBytes);
  let v = 0n;
  for (let k = 0; k < l.cellBytes; k++) {
    const idx = s.cellEndian === "big" ? k : l.cellBytes - 1 - k;
    const b = start + idx < len ? cache.byte(start + idx) : 0;
    if (b === undefined) return "";
    v = (v << 8n) | BigInt(b);
  }
  return formatCell(s.cellView, v, l.cellBytes, s.lowercase);
}
