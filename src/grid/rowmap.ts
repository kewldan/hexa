// Display rows with folded regions (DISP-23/24): each fold covers whole rows
// and is shown as a single row.

export interface Fold {
  start: number;
  end: number;
}

export type RowInfo =
  | { kind: "data"; offset: number }
  | { kind: "fold"; fold: Fold; index: number };

interface Span {
  /** First and one-past-last normal row covered by the fold. */
  a: number;
  b: number;
  /** Display row of the fold. */
  disp: number;
  /** Rows hidden up to and including this fold. */
  cum: number;
  fold: Fold;
}

export class RowMap {
  readonly bpr: number;
  readonly totalRows: number;
  private spans: Span[];

  constructor(len: number, bpr: number, folds: Fold[]) {
    this.bpr = bpr;
    const normalRows = Math.floor(len / bpr) + 1;
    const sorted = folds
      .map((f) => ({ a: Math.floor(f.start / bpr), b: Math.ceil(f.end / bpr), fold: f }))
      .filter((f) => f.b - f.a >= 2 && f.a < normalRows)
      .sort((x, y) => x.a - y.a);
    this.spans = [];
    let cum = 0;
    let lastB = -1;
    for (const f of sorted) {
      if (f.a < lastB) continue; // overlapping folds: keep the first
      const b = Math.min(f.b, normalRows);
      const disp = f.a - cum;
      cum += b - f.a - 1;
      this.spans.push({ a: f.a, b, disp, cum, fold: f.fold });
      lastB = b;
    }
    this.totalRows = normalRows - cum;
  }

  get folds(): number {
    return this.spans.length;
  }

  private lastSpanBy(pred: (s: Span) => boolean): number {
    let lo = 0;
    let hi = this.spans.length - 1;
    let ans = -1;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      if (pred(this.spans[mid])) {
        ans = mid;
        lo = mid + 1;
      } else hi = mid - 1;
    }
    return ans;
  }

  /** Display row showing `offset`. */
  rowOf(offset: number): number {
    const r = Math.floor(offset / this.bpr);
    const i = this.lastSpanBy((s) => s.a <= r);
    if (i < 0) return r;
    const s = this.spans[i];
    if (r < s.b) return s.disp;
    return r - s.cum;
  }

  /** What the display row shows. */
  info(row: number): RowInfo {
    const i = this.lastSpanBy((s) => s.disp <= row);
    if (i < 0) return { kind: "data", offset: row * this.bpr };
    const s = this.spans[i];
    if (row === s.disp) return { kind: "fold", fold: s.fold, index: i };
    return { kind: "data", offset: (row + s.cum) * this.bpr };
  }

  /** Fold containing the offset, if any. */
  foldAt(offset: number): Fold | null {
    const r = Math.floor(offset / this.bpr);
    const i = this.lastSpanBy((s) => s.a <= r);
    return i >= 0 && r < this.spans[i].b ? this.spans[i].fold : null;
  }
}
