// Minimap at the scrollbar (DISP-19): sampled entropy, search and change markers.
import { useEffect, useRef, useState } from "react";
import { api } from "../ipc/api";
import { type Tab, useTabs } from "../state/tabs";
import { readPalette, withAlpha } from "./render";

const SAMPLES = 96;
const SAMPLE_BYTES = 512;

function entropy(b: Uint8Array): number {
  if (b.length === 0) return 0;
  const counts = new Uint32Array(256);
  for (const x of b) counts[x]++;
  let h = 0;
  for (const c of counts) {
    if (!c) continue;
    const p = c / b.length;
    h -= p * Math.log2(p);
  }
  return h / 8;
}

interface Props {
  tab: Tab;
  totalRows: number;
  visibleRows: number;
  topRow: number;
  maxTop: number;
  bpr: number;
}

export function Minimap({ tab, totalRows, visibleRows, topRow, maxTop, bpr }: Props) {
  const ref = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const [levels, setLevels] = useState<number[]>([]);
  const [changes, setChanges] = useState<number[]>([]);
  const [drag, setDrag] = useState(false);
  const len = tab.info.len;

  // Sample entropy across the file (cheap: 96 × 512 bytes).
  useEffect(() => {
    let alive = true;
    const timer = setTimeout(async () => {
      if (len === 0) return setLevels([]);
      const n = Math.min(SAMPLES, Math.max(1, Math.ceil(len / SAMPLE_BYTES)));
      const out: number[] = [];
      for (let i = 0; i < n; i++) {
        const off = Math.floor((len * i) / n);
        try {
          out.push(entropy(await api.read(tab.id, off, Math.min(SAMPLE_BYTES, len - off))));
        } catch {
          out.push(0);
        }
        if (!alive) return;
      }
      setLevels(out);
      const spans = await api.changes(tab.id, 0, len).catch(() => []);
      if (alive) setChanges(spans.map((s) => s.start));
    }, 400);
    return () => {
      alive = false;
      clearTimeout(timer);
    };
  }, [tab.id, len, tab.info.version]);

  useEffect(() => {
    const c = canvas.current;
    const el = ref.current;
    if (!c || !el) return;
    const dpr = window.devicePixelRatio || 1;
    const w = el.clientWidth;
    const h = el.clientHeight;
    c.width = w * dpr;
    c.height = h * dpr;
    c.style.width = `${w}px`;
    c.style.height = `${h}px`;
    const ctx = c.getContext("2d")!;
    ctx.scale(dpr, dpr);
    ctx.clearRect(0, 0, w, h);
    const pal = readPalette();
    const pad = 6;
    const ih = h - pad * 2;
    if (levels.length) {
      const bh = ih / levels.length;
      levels.forEach((lv, i) => {
        ctx.fillStyle =
          lv < 0.05 ? withAlpha(pal.muted, 0.1) : withAlpha(pal.acc, 0.15 + lv * 0.65);
        ctx.beginPath();
        ctx.roundRect(5, pad + i * bh + 0.5, w - 10, Math.max(1, bh - 1), 1);
        ctx.fill();
      });
    }
    if (len > 0) {
      ctx.fillStyle = pal.mod;
      for (const o of changes) ctx.fillRect(w - 4, pad + (o / len) * ih - 1, 3, 3);
      const matches = tab.results?.matches ?? [];
      ctx.fillStyle = "#EAB308";
      let last = -10;
      for (const m of matches) {
        const y = pad + (m.offset / len) * ih;
        if (y - last < 2) continue;
        last = y;
        ctx.fillRect(1, y - 1, 4, 3);
      }
    }
  }, [levels, changes, tab.results?.matches, len]);

  const jump = (clientY: number) => {
    const el = ref.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    const frac = Math.min(1, Math.max(0, (clientY - r.top - 6) / (r.height - 12)));
    const row = frac * totalRows - visibleRows / 2;
    useTabs.getState().update(tab.id, { topRow: Math.max(0, Math.min(maxTop, row)) });
  };

  const onDown = (e: React.MouseEvent) => {
    setDrag(true);
    jump(e.clientY);
    const move = (ev: MouseEvent) => jump(ev.clientY);
    const up = () => {
      setDrag(false);
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  };

  const vpTop = totalRows > 0 ? (topRow / totalRows) * 100 : 0;
  const vpH = totalRows > 0 ? Math.min(100, (visibleRows / totalRows) * 100) : 100;
  void bpr;
  return (
    <div className={`mm${drag ? " drag" : ""}`} ref={ref} onMouseDown={onDown} aria-hidden="true">
      <canvas ref={canvas} />
      <div style={{ position: "absolute", inset: "6px 0", pointerEvents: "none" }}>
        <span className="vp" style={{ top: `${Math.min(vpTop, 100 - vpH)}%`, height: `${vpH}%` }} />
      </div>
    </div>
  );
}
