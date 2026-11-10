// Page cache of document bytes in the UI (CORE-06 on the frontend side).
//
// The grid asks for bytes synchronously; missing pages are fetched in batches
// and listeners are notified when they arrive. Edits invalidate only the
// pages they can affect, and invalidated pages stay visible until their
// fresh copy arrives so the grid never flashes empty after a keystroke.
import { api, type Change, type ChangeSpan } from "../ipc/api";

export const PAGE = 4096;
const MAX_PAGES = 4096; // 16 MiB

type Listener = () => void;

export class DocCache {
  readonly id: number;
  private pages = new Map<number, Uint8Array>();
  /** Invalidated pages, shown until the refetched page replaces them. */
  private stale = new Map<number, Uint8Array>();
  private pending = new Set<number>();
  /** Pages that failed to read (unmapped process memory), shown as `??`. */
  private bad = new Set<number>();
  private listeners = new Set<Listener>();
  private changeWin: { start: number; end: number; spans: ChangeSpan[]; gen: number } | null = null;
  private changeReq: { start: number; end: number; gen: number } | null = null;
  private gen = 0;
  version = 0;
  len = 0;

  constructor(id: number) {
    this.id = id;
  }

  subscribe(fn: Listener): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  private emit() {
    for (const l of this.listeners) l();
  }

  /** Byte at `offset`, or `undefined` while its page is loading. */
  byte(offset: number): number | undefined {
    const p = Math.floor(offset / PAGE);
    const page = this.pages.get(p) ?? this.stale.get(p);
    if (!page) return undefined;
    const i = offset % PAGE;
    return i < page.length ? page[i] : undefined;
  }

  /** The byte at `offset` could not be read (e.g. unmapped memory). */
  unreadable(offset: number): boolean {
    return this.bad.has(Math.floor(offset / PAGE));
  }

  /** Copies `[start, end)` into `out`; returns false when something is missing. */
  slice(start: number, end: number, out: Uint8Array): boolean {
    let ok = true;
    for (let p = Math.floor(start / PAGE); p * PAGE < end; p++) {
      const page = this.pages.get(p) ?? this.stale.get(p);
      if (!page) {
        ok = false;
        continue;
      }
      const ps = p * PAGE;
      const from = Math.max(start, ps);
      const to = Math.min(end, ps + page.length);
      out.set(page.subarray(from - ps, to - ps), from - start);
    }
    return ok;
  }

  /** Requests the pages covering `[start, end)` that are not cached yet. */
  ensure(start: number, end: number) {
    const last = Math.min(end, this.len);
    if (last <= start) return;
    const first = Math.floor(start / PAGE);
    const lastPage = Math.floor((last - 1) / PAGE);
    let runStart = -1;
    const flush = (to: number) => {
      if (runStart < 0) return;
      const from = runStart;
      runStart = -1;
      for (let p = from; p <= to; p++) this.pending.add(p);
      const gen = this.gen;
      api
        .read(this.id, from * PAGE, (to - from + 1) * PAGE)
        .then((bytes) => {
          if (gen !== this.gen) return;
          for (let p = from; p <= to; p++) {
            this.pending.delete(p);
            const off = (p - from) * PAGE;
            if (off >= bytes.length) continue;
            this.pages.set(p, bytes.subarray(off, Math.min(off + PAGE, bytes.length)));
            this.stale.delete(p);
          }
          this.evict();
          this.emit();
        })
        .catch(() => {
          for (let p = from; p <= to; p++) this.pending.delete(p);
          if (gen !== this.gen) return;
          // Narrow a failed run down to the pages that cannot be read.
          if (from === to) {
            this.bad.add(from);
            this.emit();
          } else for (let p = from; p <= to; p++) this.ensure(p * PAGE, (p + 1) * PAGE);
        });
    };
    for (let p = first; p <= lastPage; p++) {
      if (this.pages.has(p) || this.pending.has(p) || this.bad.has(p)) {
        flush(p - 1);
      } else if (runStart < 0) {
        runStart = p;
      }
    }
    flush(lastPage);
  }

  private evict() {
    if (this.pages.size <= MAX_PAGES) return;
    const drop = this.pages.size - MAX_PAGES;
    let n = 0;
    for (const k of this.pages.keys()) {
      if (n++ >= drop) break;
      this.pages.delete(k);
    }
  }

  /** Moves a page to the stale set: still drawn, but fetched again. */
  private retire(p: number) {
    const page = this.pages.get(p);
    if (!page) return;
    this.pages.delete(p);
    this.stale.set(p, page);
  }

  /** Invalidates what an edit may have touched. */
  applyChange(change: Change | null, len: number, version: number) {
    this.len = len;
    this.version = version;
    if (!change) {
      this.refresh();
      return;
    }
    const firstPage = Math.floor(change.start / PAGE);
    if (change.removed !== change.inserted) {
      for (const p of [...this.pages.keys()]) if (p >= firstPage) this.retire(p);
    } else {
      const lastPage = Math.floor((change.start + Math.max(change.inserted, 1) - 1) / PAGE);
      for (let p = firstPage; p <= lastPage; p++) this.retire(p);
    }
    for (const p of [...this.stale.keys()]) if (p * PAGE >= len) this.stale.delete(p);
    this.gen++;
    this.pending.clear();
    this.bad.clear();
    this.emit();
  }

  /** Refetches everything while keeping the current bytes on screen. */
  private refresh() {
    for (const p of [...this.pages.keys()]) this.retire(p);
    if (this.stale.size > MAX_PAGES) this.stale.clear();
    this.pending.clear();
    this.bad.clear();
    this.gen++;
    this.emit();
  }

  clear() {
    this.pages.clear();
    this.stale.clear();
    this.pending.clear();
    this.bad.clear();
    this.changeWin = null;
    this.gen++;
    this.emit();
  }

  sync(len: number, version: number) {
    if (version !== this.version) {
      this.len = len;
      this.version = version;
      this.refresh();
    } else {
      this.len = len;
    }
  }

  /** Change spans (DISP-15) for a window; fetched lazily. */
  changes(start: number, end: number): ChangeSpan[] {
    const w = this.changeWin;
    if (w && w.gen === this.gen && w.start <= start && w.end >= end) return w.spans;
    const r = this.changeReq;
    if (!(r && r.gen === this.gen && r.start <= start && r.end >= end)) {
      const span = end - start;
      const req = { start: Math.max(0, start - span), end: end + span, gen: this.gen };
      this.changeReq = req;
      api
        .changes(this.id, req.start, req.end)
        .then((spans) => {
          if (req.gen !== this.gen) return;
          this.changeWin = { ...req, spans };
          this.emit();
        })
        .catch(() => {});
    }
    // Until the new spans arrive keep showing the previous ones (no flash).
    return w ? w.spans : [];
  }
}

const caches = new Map<number, DocCache>();

export function cacheFor(id: number): DocCache {
  let c = caches.get(id);
  if (!c) {
    c = new DocCache(id);
    caches.set(id, c);
  }
  return c;
}

export function dropCache(id: number) {
  caches.delete(id);
}
