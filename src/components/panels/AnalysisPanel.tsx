// Analysis of the selection or the whole file: entropy curve with region
// classification, histogram, Hilbert and digraph maps, numeric plots, raw
// bitmap decoding and embedded files (ANL-07…10, ANL-15…17, SRCH-10).
import { useEffect, useRef, useState } from "react";
import {
  type AnlRegion,
  anl,
  type BitmapOptions,
  type Digraph,
  type Embedded,
  type HilbertMap,
  type NumType,
  type Overview,
  type PixelFormat,
  type PlotSeries,
  type RegionKind,
  type Rgba,
  type YaraReport,
} from "../../ipc/analysis";
import { api } from "../../ipc/api";
import { fmtSize, hexN } from "../../lib/format";
import "../../styles/analysis.css";
import * as A from "../../state/actions";
import { hasSelection, selection, type Tab } from "../../state/tabs";

/** Larger files are analysed on request only. */
const AUTO_LIMIT = 256 << 20;

export const REGION_COLORS: Record<RegionKind, string> = {
  zero: "#6B6B73",
  padding: "#A1A1AA",
  text: "#2F6BF0",
  code: "#7C4DFF",
  compressed: "#E8890C",
  encrypted: "#DC2626",
  data: "#1F9D74",
};
const REGION_LABEL: Record<RegionKind, string> = {
  zero: "нули",
  padding: "заполнитель",
  text: "текст",
  code: "код",
  compressed: "сжатое",
  encrypted: "шифр/случайное",
  data: "данные",
};
const CLASS_COLORS = ["#000000", "#1F9D74", "#2F6BF0", "#DC2626", "#FFFFFF"];

type Section = "overview" | "map" | "plot" | "image" | "embedded" | "yara";

function cssVar(name: string, fallback: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim() || fallback;
}

function useRange(tab: Tab) {
  const sel = hasSelection(tab) ? selection(tab) : null;
  return sel ?? { start: 0, end: tab.info.len };
}

/** Canvas sized to its CSS box at device pixel ratio. */
function useCanvas(
  draw: (ctx: CanvasRenderingContext2D, w: number, h: number) => void,
  deps: unknown[],
) {
  const ref = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    const c = ref.current;
    if (!c) return;
    const paint = () => {
      const dpr = window.devicePixelRatio || 1;
      const w = c.clientWidth;
      const h = c.clientHeight;
      c.width = Math.round(w * dpr);
      c.height = Math.round(h * dpr);
      const ctx = c.getContext("2d");
      if (!ctx) return;
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, w, h);
      draw(ctx, w, h);
    };
    paint();
    const ro = new ResizeObserver(paint);
    ro.observe(c);
    return () => ro.disconnect();
  }, deps);
  return ref;
}

function mergeRegions(rs: AnlRegion[]): AnlRegion[] {
  const out: AnlRegion[] = [];
  for (const r of rs) {
    const last = out[out.length - 1];
    if (last && last.kind === r.kind && last.end === r.start) last.end = r.end;
    else out.push({ ...r });
  }
  return out;
}

function OverviewSection({ tab }: { tab: Tab }) {
  const range = useRange(tab);
  const [data, setData] = useState<Overview | null>(null);
  const [busy, setBusy] = useState(false);
  const [logScale, setLogScale] = useState(true);
  const [hover, setHover] = useState<number | null>(null);
  const run = () => {
    setBusy(true);
    anl
      .overview(tab.id, range.start, range.end)
      .then(setData)
      .catch(A.report)
      .finally(() => setBusy(false));
  };
  useEffect(() => {
    if (range.end - range.start <= AUTO_LIMIT) {
      const t = setTimeout(run, 300);
      return () => clearTimeout(t);
    }
    setData(null);
  }, [tab.id, tab.info.version, range.start, range.end]);

  const entropyRef = useCanvas(
    (ctx, w, h) => {
      if (!data) return;
      const strip = 8;
      const span = Math.max(1, range.end - range.start);
      for (const r of data.regions) {
        ctx.fillStyle = REGION_COLORS[r.kind];
        const x0 = ((r.start - range.start) / span) * w;
        const x1 = ((r.end - range.start) / span) * w;
        ctx.fillRect(x0, h - strip, Math.max(1, x1 - x0), strip);
      }
      const vals = data.entropy.values;
      if (!vals.length) return;
      const ph = h - strip - 4;
      const acc = cssVar("--acc", "#2F6BF0");
      ctx.beginPath();
      ctx.moveTo(0, ph);
      vals.forEach((v, i) => {
        ctx.lineTo((i / Math.max(1, vals.length - 1)) * w, ph - (v / 8) * ph);
      });
      ctx.lineTo(w, ph);
      ctx.closePath();
      ctx.fillStyle = `${acc}33`;
      ctx.fill();
      ctx.beginPath();
      vals.forEach((v, i) => {
        const x = (i / Math.max(1, vals.length - 1)) * w;
        const y = ph - (v / 8) * ph;
        if (i) ctx.lineTo(x, y);
        else ctx.moveTo(x, y);
      });
      ctx.strokeStyle = acc;
      ctx.lineWidth = 1.5;
      ctx.stroke();
      // Cursor position.
      if (tab.cursor >= range.start && tab.cursor <= range.end) {
        const x = ((tab.cursor - range.start) / span) * w;
        ctx.fillStyle = cssVar("--ink", "#111");
        ctx.fillRect(x, 0, 1, h);
      }
    },
    [data, tab.cursor, range.start, range.end],
  );
  const histRef = useCanvas(
    (ctx, w, h) => {
      if (!data) return;
      const counts = data.histogram.counts;
      const f = logScale ? (v: number) => Math.log1p(v) : (v: number) => v;
      const max = Math.max(1, ...counts.map(f));
      const bw = w / 256;
      const acc = cssVar("--acc", "#2F6BF0");
      counts.forEach((c, i) => {
        const bh = (f(c) / max) * (h - 2);
        ctx.fillStyle = i === hover ? cssVar("--ink", "#111") : acc;
        ctx.fillRect(i * bw, h - bh, Math.max(1, bw - 0.3), bh);
      });
    },
    [data, logScale, hover],
  );
  const h = data?.histogram;
  return (
    <>
      {!data ? (
        <div className="empty">
          <span>
            {busy
              ? "Считаю…"
              : `Анализ ${fmtSize(range.end - range.start)}: энтропия, классы данных и частоты байт.`}
          </span>
          {!busy && (
            <button type="button" className="btn" onClick={run}>
              Анализировать
            </button>
          )}
        </div>
      ) : (
        <>
          <div className="anl-cap">
            <span className="cap">Энтропия</span>
            <span className="note">{busy ? "обновляю…" : `0–8 бит/байт`}</span>
          </div>
          <canvas
            ref={entropyRef}
            className="anl-canvas"
            style={{ height: 90 }}
            aria-label="График энтропии"
            onClick={(e) => {
              const r = e.currentTarget.getBoundingClientRect();
              const off =
                range.start +
                Math.floor(((e.clientX - r.left) / r.width) * (range.end - range.start));
              A.setCursor(off, false, { jump: true });
            }}
          />
          <div className="anl-legend">
            {(Object.keys(REGION_COLORS) as RegionKind[])
              .filter((k) => data.regions.some((r) => r.kind === k))
              .map((k) => (
                <span key={k}>
                  <i style={{ background: REGION_COLORS[k] }} />
                  {REGION_LABEL[k]}
                </span>
              ))}
          </div>
          <div className="anl-cap">
            <span className="cap">Частоты байт</span>
            <label className="chk" style={{ margin: 0 }}>
              <input
                type="checkbox"
                checked={logScale}
                onChange={(e) => setLogScale(e.target.checked)}
              />
              лог.
            </label>
          </div>
          <canvas
            ref={histRef}
            className="anl-canvas"
            style={{ height: 70 }}
            aria-label="Гистограмма"
            onMouseMove={(e) => {
              const r = e.currentTarget.getBoundingClientRect();
              setHover(
                Math.min(255, Math.max(0, Math.floor(((e.clientX - r.left) / r.width) * 256))),
              );
            }}
            onMouseLeave={() => setHover(null)}
          />
          {h && (
            <div className="kv" style={{ padding: "0 6px" }}>
              {hover !== null && (
                <>
                  <span>Байт 0x{hexN(hover, 2)}</span>
                  <b>
                    {h.counts[hover].toLocaleString("ru")} ·{" "}
                    {((100 * h.counts[hover]) / Math.max(1, h.total)).toFixed(2)}%
                  </b>
                </>
              )}
              <span>Энтропия</span>
              <b>{h.entropy.toFixed(3)} бит/байт</b>
              <span>Разных значений</span>
              <b>{h.distinct}</b>
              <span>Среднее</span>
              <b>{h.mean.toFixed(2)}</b>
              <span>χ²</span>
              <b>{h.chiSquare.toFixed(1)}</b>
              {h.mostCommon !== null && (
                <>
                  <span>Чаще всего</span>
                  <b className="mono">0x{hexN(h.mostCommon, 2)}</b>
                </>
              )}
            </div>
          )}
          <div className="anl-cap">
            <span className="cap">Области</span>
          </div>
          <div className="list">
            {mergeRegions(data.regions)
              .slice(0, 300)
              .map((r) => (
                <button
                  type="button"
                  key={r.start}
                  className="li"
                  style={{ gridTemplateColumns: "10px minmax(0,1fr)", animation: "none" }}
                  onClick={() => A.selectRange(r.start, r.end)}
                >
                  <span className="dotc" style={{ background: REGION_COLORS[r.kind] }} />
                  <span className="t">
                    <b>
                      {REGION_LABEL[r.kind]}
                      {r.detail ? ` · ${r.detail}` : ""}
                    </b>
                    <span>
                      0x{hexN(r.start, 8)} · {fmtSize(r.end - r.start)}
                    </span>
                  </span>
                </button>
              ))}
          </div>
        </>
      )}
    </>
  );
}

/** Hilbert curve index → (x, y). */
function d2xy(side: number, d: number): [number, number] {
  let x = 0;
  let y = 0;
  let t = d;
  for (let s = 1; s < side; s *= 2) {
    const rx = 1 & (t / 2);
    const ry = 1 & (t ^ rx);
    if (ry === 0) {
      if (rx === 1) {
        x = s - 1 - x;
        y = s - 1 - y;
      }
      [x, y] = [y, x];
    }
    x += s * rx;
    y += s * ry;
    t = Math.floor(t / 4);
  }
  return [x, y];
}

function xy2d(side: number, x0: number, y0: number): number {
  let d = 0;
  let x = x0;
  let y = y0;
  for (let s = side / 2; s >= 1; s /= 2) {
    const rx = (x & s) > 0 ? 1 : 0;
    const ry = (y & s) > 0 ? 1 : 0;
    d += s * s * ((3 * rx) ^ ry);
    if (ry === 0) {
      if (rx === 1) {
        x = side - 1 - x;
        y = side - 1 - y;
      }
      [x, y] = [y, x];
    }
  }
  return d;
}

function MapSection({ tab }: { tab: Tab }) {
  const range = useRange(tab);
  const [map, setMap] = useState<HilbertMap | null>(null);
  const [dg, setDg] = useState<Digraph | null>(null);
  const [mode, setMode] = useState<"hilbert" | "digraph">("hilbert");
  useEffect(() => {
    const t = setTimeout(() => {
      if (mode === "hilbert")
        anl.hilbert(tab.id, range.start, range.end, 8).then(setMap).catch(A.report);
      else anl.digraph(tab.id, range.start, range.end, 256).then(setDg).catch(A.report);
    }, 200);
    return () => clearTimeout(t);
  }, [tab.id, tab.info.version, range.start, range.end, mode]);
  const ref = useCanvas(
    (ctx, w) => {
      if (mode === "hilbert" && map) {
        const img = ctx.createImageData(map.side, map.side);
        map.cells.forEach((c, d) => {
          const [x, y] = d2xy(map.side, d);
          const hex = CLASS_COLORS[c] ?? "#888888";
          const i = (y * map.side + x) * 4;
          img.data[i] = Number.parseInt(hex.slice(1, 3), 16);
          img.data[i + 1] = Number.parseInt(hex.slice(3, 5), 16);
          img.data[i + 2] = Number.parseInt(hex.slice(5, 7), 16);
          img.data[i + 3] = 255;
        });
        const off = new OffscreenCanvas(map.side, map.side);
        off.getContext("2d")?.putImageData(img, 0, 0);
        ctx.imageSmoothingEnabled = false;
        ctx.drawImage(off, 0, 0, w, w);
      } else if (mode === "digraph" && dg) {
        const n = dg.size;
        const img = ctx.createImageData(n, n);
        const max = Math.log1p(dg.max || 1);
        dg.counts.forEach((c, i) => {
          const v = Math.round((Math.log1p(c) / max) * 255);
          img.data[i * 4] = v;
          img.data[i * 4 + 1] = Math.round(v * 0.8);
          img.data[i * 4 + 2] = 255 - Math.round(v * 0.5);
          img.data[i * 4 + 3] = c ? 255 : 30;
        });
        const off = new OffscreenCanvas(n, n);
        off.getContext("2d")?.putImageData(img, 0, 0);
        ctx.imageSmoothingEnabled = false;
        ctx.drawImage(off, 0, 0, w, w);
      }
    },
    [map, dg, mode],
  );
  return (
    <>
      <div className="anl-cap">
        <select
          className="sel-input"
          aria-label="Карта"
          value={mode}
          onChange={(e) => setMode(e.target.value as typeof mode)}
        >
          <option value="hilbert">Кривая Гильберта</option>
          <option value="digraph">Диграф пар байт</option>
        </select>
        {mode === "hilbert" && map && (
          <span className="note">{fmtSize(map.bytesPerCell)} на точку</span>
        )}
      </div>
      <canvas
        ref={ref}
        className="anl-canvas sq"
        aria-label="Карта файла"
        onClick={(e) => {
          if (mode !== "hilbert" || !map) return;
          const r = e.currentTarget.getBoundingClientRect();
          const x = Math.floor(((e.clientX - r.left) / r.width) * map.side);
          const y = Math.floor(((e.clientY - r.top) / r.height) * map.side);
          A.setCursor(map.start + xy2d(map.side, x, y) * map.bytesPerCell, false, { jump: true });
        }}
      />
      {mode === "hilbert" ? (
        <div className="anl-legend">
          {["0x00", "управляющие", "ASCII", "старшие", "0xFF"].map((l, i) => (
            <span key={l}>
              <i style={{ background: CLASS_COLORS[i], border: "1px solid var(--line)" }} />
              {l}
            </span>
          ))}
        </div>
      ) : (
        <div className="note" style={{ padding: "0 6px" }}>
          По осям — первый и второй байт пары; яркость — частота. Текст, код и сжатые данные дают
          узнаваемые узоры.
        </div>
      )}
    </>
  );
}

function PlotSection({ tab }: { tab: Tab }) {
  const range = useRange(tab);
  const [numType, setNumType] = useState<NumType>("u8");
  const [be, setBe] = useState(false);
  const [data, setData] = useState<PlotSeries | null>(null);
  useEffect(() => {
    const t = setTimeout(
      () =>
        anl
          .plot(tab.id, range.start, range.end, {
            numType,
            bigEndian: be,
            buckets: 600,
            stride: null,
          })
          .then(setData)
          .catch(A.report),
      200,
    );
    return () => clearTimeout(t);
  }, [tab.id, tab.info.version, range.start, range.end, numType, be]);
  const ref = useCanvas(
    (ctx, w, h) => {
      if (!data || data.min === null || data.max === null) return;
      const lo = data.min;
      const span = data.max - lo || 1;
      const acc = cssVar("--acc", "#2F6BF0");
      ctx.fillStyle = acc;
      const n = data.buckets.length;
      data.buckets.forEach((b, i) => {
        if (b.min === null || b.max === null) return;
        const x = (i / n) * w;
        const y0 = h - ((b.max - lo) / span) * (h - 2) - 1;
        const y1 = h - ((b.min - lo) / span) * (h - 2) - 1;
        ctx.fillRect(x, y0, Math.max(1, w / n), Math.max(1, y1 - y0));
      });
    },
    [data],
  );
  return (
    <>
      <div className="anl-cap">
        <select
          className="sel-input"
          aria-label="Тип значений"
          value={numType}
          onChange={(e) => setNumType(e.target.value as NumType)}
        >
          {(["u8", "i8", "u16", "i16", "u32", "i32", "u64", "i64", "f32", "f64"] as const).map(
            (t) => (
              <option key={t}>{t}</option>
            ),
          )}
        </select>
        <label className="chk" style={{ margin: 0 }}>
          <input type="checkbox" checked={be} onChange={(e) => setBe(e.target.checked)} />
          big-endian
        </label>
      </div>
      <canvas
        ref={ref}
        className="anl-canvas"
        style={{ height: 160 }}
        aria-label="График значений"
      />
      {data && (
        <div className="kv" style={{ padding: "0 6px" }}>
          <span>Значений</span>
          <b>{data.count.toLocaleString("ru")}</b>
          <span>Мин · макс</span>
          <b>
            {data.min ?? "—"} · {data.max ?? "—"}
          </b>
          <span>Среднее</span>
          <b>{data.mean?.toFixed(3) ?? "—"}</b>
          {data.nonFinite > 0 && (
            <>
              <span>NaN/∞</span>
              <b>{data.nonFinite}</b>
            </>
          )}
        </div>
      )}
    </>
  );
}

const PIXEL_FORMATS: PixelFormat[] = [
  "rgb888",
  "bgr888",
  "rgba8888",
  "bgra8888",
  "argb8888",
  "rgb565",
  "rgb555",
  "gray8",
  "gray4",
  "gray2",
  "gray1",
  "indexed8",
];

function ImageSection({ tab }: { tab: Tab }) {
  const [opts, setOpts] = useState<BitmapOptions>({
    offset: tab.cursor,
    width: 256,
    height: null,
    stride: null,
    format: "rgb888",
    bigEndian: false,
    lsbFirst: false,
    flipVertical: false,
    palette: null,
  });
  const [img, setImg] = useState<Rgba | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [asFile, setAsFile] = useState(false);
  const range = useRange(tab);
  useEffect(() => {
    const t = setTimeout(() => {
      const p = asFile
        ? anl.image(tab.id, range.start, range.end)
        : anl.bitmap(tab.id, { ...opts, height: opts.height || 512 });
      p.then((r) => {
        setImg(r);
        setErr(null);
      }).catch((e) => {
        setImg(null);
        setErr(e?.message ?? String(e));
      });
    }, 200);
    return () => clearTimeout(t);
  }, [tab.id, tab.info.version, opts, asFile, range.start, range.end]);
  const ref = useCanvas(
    (ctx, w) => {
      if (!img?.width) return;
      const off = new OffscreenCanvas(img.width, img.height);
      off
        .getContext("2d")
        ?.putImageData(new ImageData(new Uint8ClampedArray(img.rgba), img.width, img.height), 0, 0);
      const scale = Math.min(w / img.width, 4);
      ctx.imageSmoothingEnabled = scale < 1;
      ctx.drawImage(off, 0, 0, img.width * scale, img.height * scale);
    },
    [img],
  );
  const num = (k: "offset" | "width") => (e: React.ChangeEvent<HTMLInputElement>) => {
    const v = e.target.value.trim();
    const n = v.startsWith("0x") ? Number.parseInt(v.slice(2), 16) : Number(v);
    if (Number.isFinite(n) && n >= 0) setOpts({ ...opts, [k]: n });
  };
  const height = img?.width ? Math.min(400, (img.height / img.width) * 300) : 120;
  return (
    <>
      <div className="anl-cap">
        <label className="chk" style={{ margin: 0 }}>
          <input type="checkbox" checked={asFile} onChange={(e) => setAsFile(e.target.checked)} />
          выделение как файл (PNG, JPEG, GIF, BMP, ICO, WebP)
        </label>
      </div>
      {!asFile && (
        <div className="xf-body" style={{ padding: "0 6px" }}>
          <label className="xf-f">
            <span>Смещение</span>
            <input
              className="inp mono"
              defaultValue={`0x${hexN(opts.offset)}`}
              onChange={num("offset")}
              onKeyDown={(e) => e.stopPropagation()}
            />
          </label>
          <label className="xf-f">
            <span>Ширина, px</span>
            <input
              className="inp"
              defaultValue={opts.width}
              onChange={num("width")}
              onKeyDown={(e) => e.stopPropagation()}
            />
          </label>
          <label className="xf-f">
            <span>Формат пикселя</span>
            <select
              className="sel-input"
              value={opts.format}
              onChange={(e) => setOpts({ ...opts, format: e.target.value as PixelFormat })}
            >
              {PIXEL_FORMATS.map((f) => (
                <option key={f}>{f}</option>
              ))}
            </select>
          </label>
          <label className="chk">
            <input
              type="checkbox"
              checked={opts.flipVertical}
              onChange={(e) => setOpts({ ...opts, flipVertical: e.target.checked })}
            />
            снизу вверх
          </label>
          <button
            type="button"
            className="chip"
            onClick={() => setOpts({ ...opts, offset: tab.cursor })}
          >
            С курсора
          </button>
        </div>
      )}
      {err ? (
        <div className="xf-err">{err}</div>
      ) : (
        <canvas ref={ref} className="anl-canvas" style={{ height }} aria-label="Изображение" />
      )}
      {img && (
        <div className="note" style={{ padding: "0 6px" }}>
          {img.width}×{img.height}
        </div>
      )}
    </>
  );
}

function EmbeddedSection({ tab }: { tab: Tab }) {
  const range = useRange(tab);
  const [list, setList] = useState<Embedded[] | null>(null);
  const [busy, setBusy] = useState(false);
  const run = () => {
    setBusy(true);
    anl
      .embedded(tab.id, range.start, range.end)
      .then(setList)
      .catch(A.report)
      .finally(() => setBusy(false));
  };
  return (
    <>
      <div className="anl-cap">
        <button type="button" className="btn" onClick={run} disabled={busy}>
          {busy ? "Ищу…" : "Найти вложенные файлы"}
        </button>
        {list && <span className="note">найдено {list.length}</span>}
      </div>
      {list && list.length === 0 && (
        <div className="empty">
          <span>Сигнатур известных форматов не найдено.</span>
        </div>
      )}
      <div className="list">
        {list?.map((e) => (
          <div
            key={e.offset}
            className="li"
            style={{ gridTemplateColumns: "minmax(0,1fr) auto", animation: "none" }}
          >
            <button
              type="button"
              className="t"
              style={{ textAlign: "left" }}
              onClick={() => A.selectRange(e.offset, e.offset + (e.lenGuess ?? 1))}
            >
              <b>
                {e.kind} · {e.description}
              </b>
              <span>
                0x{hexN(e.offset, 8)}
                {e.lenGuess !== null ? ` · ${fmtSize(e.lenGuess)}` : " · длина неизвестна"}
              </span>
            </button>
            {e.lenGuess !== null && (
              <button
                type="button"
                className="chip"
                title="Открыть в новой вкладке"
                onClick={() =>
                  api
                    .xfrmToNew(
                      tab.id,
                      { start: e.offset, end: e.offset + (e.lenGuess ?? 0) },
                      { steps: [] },
                    )
                    .then(A.adopt)
                    .catch(A.report)
                }
              >
                Извлечь
              </button>
            )}
          </div>
        ))}
      </div>
    </>
  );
}

const YARA_SAMPLE = `rule elf_header {
  strings:
    $magic = { 7F 45 4C 46 }
  condition:
    $magic at 0
}`;

function YaraSection({ tab }: { tab: Tab }) {
  const range = useRange(tab);
  const [rules, setRules] = useState(() => {
    try {
      return localStorage.getItem("hx-yara") ?? YARA_SAMPLE;
    } catch {
      return YARA_SAMPLE;
    }
  });
  const [rep, setRep] = useState<YaraReport | null>(null);
  const [busy, setBusy] = useState(false);
  const run = () => {
    try {
      localStorage.setItem("hx-yara", rules);
    } catch {}
    setBusy(true);
    anl
      .yara(tab.id, rules, range.start, range.end)
      .then(setRep)
      .catch(A.report)
      .finally(() => setBusy(false));
  };
  const load = async () => {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const p = await open({
      multiple: false,
      filters: [{ name: "YARA", extensions: ["yar", "yara", "txt"] }],
    });
    if (typeof p === "string") setRules(await api.readTextFile(p));
  };
  return (
    <>
      <textarea
        className="inp mono yara-src"
        aria-label="Правила YARA"
        spellCheck={false}
        value={rules}
        onChange={(e) => setRules(e.target.value)}
        onKeyDown={(e) => {
          e.stopPropagation();
          if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) run();
        }}
      />
      <div className="xf-actions">
        <button type="button" className="btn pri" onClick={run} disabled={busy}>
          {busy ? "Проверяю…" : "Проверить"}
        </button>
        <button type="button" className="chip" onClick={load}>
          Из файла…
        </button>
        {rep && <span className="note">сработало правил: {rep.matches.length}</span>}
      </div>
      {rep?.warnings.map((w) => (
        <div key={w} className="note" style={{ padding: "0 6px" }}>
          ⚠ {w}
        </div>
      ))}
      <div className="list">
        {rep?.matches.map((m) => (
          <div key={`${m.namespace}:${m.rule}`} className="fif">
            <b>
              {m.rule}
              {m.tags.length > 0 && <span className="note"> · {m.tags.join(", ")}</span>}
            </b>
            <div className="fif-offs">
              {m.strings.slice(0, 64).map((s) => (
                <button
                  type="button"
                  key={`${s.identifier}@${s.offset}`}
                  className="chip mono"
                  onClick={() => A.selectRange(s.offset, s.offset + Math.max(1, s.len))}
                >
                  {s.identifier}@{hexN(s.offset)}
                </button>
              ))}
            </div>
          </div>
        ))}
      </div>
    </>
  );
}

const SECTIONS: { id: Section; title: string }[] = [
  { id: "overview", title: "Обзор" },
  { id: "map", title: "Карта" },
  { id: "plot", title: "График" },
  { id: "image", title: "Картинка" },
  { id: "embedded", title: "Вложения" },
  { id: "yara", title: "YARA" },
];

export function AnalysisPanel({ tab }: { tab: Tab }) {
  const [section, setSection] = useState<Section>("overview");
  const range = useRange(tab);
  return (
    <>
      <div className="ph">
        <span className="cap">Анализ</span>
        <span className="tag">
          {hasSelection(tab) ? `выделение ${fmtSize(range.end - range.start)}` : "весь файл"}
        </span>
      </div>
      <div className="seg anl-tabs" role="tablist">
        {SECTIONS.map((s) => (
          <button
            type="button"
            role="tab"
            aria-selected={section === s.id}
            key={s.id}
            className={section === s.id ? "on" : ""}
            onClick={() => setSection(s.id)}
          >
            {s.title}
          </button>
        ))}
      </div>
      {section === "overview" && <OverviewSection tab={tab} />}
      {section === "map" && <MapSection tab={tab} />}
      {section === "plot" && <PlotSection tab={tab} />}
      {section === "image" && <ImageSection tab={tab} />}
      {section === "embedded" && <EmbeddedSection tab={tab} />}
      {section === "yara" && <YaraSection tab={tab} />}
    </>
  );
}
