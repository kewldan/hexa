// The hex grid: canvas for the bytes, DOM overlay for the animated cursor.
import {
  type MouseEvent as ReactMouseEvent,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { markColor } from "../components/panels/MarksPanel";
import { keysFor } from "../lib/commands";
import { hexN, offsetWidth } from "../lib/format";
import { displayKeys, isMac } from "../lib/keys";
import { renderMarkdown } from "../lib/markdown";
import * as A from "../state/actions";
import { useOverlays } from "../state/overlays";
import { fontSize, rowHeight, useSettings } from "../state/settings";
import { hasSelection, selection, type Tab, useTabs } from "../state/tabs";
import { type MenuItem, openDialog, useUi } from "../state/ui";
import { cacheFor } from "./cache";
import { computeLayout, type GridLayout, hitHexCell, hitTextByte } from "./layout";
import { Minimap } from "./Minimap";
import { cursorLabel, HexAtlas, type Palette, readPalette, renderGrid, withAlpha } from "./render";
import { parseTbl, TblDecoder } from "./tbl";
import { TextDecoder2 } from "./text";

function measureChar(font: string): number {
  const c = document.createElement("canvas").getContext("2d")!;
  c.font = font;
  return c.measureText("0123456789ABCDEF").width / 16;
}

interface Fx {
  id: number;
  kind: "pop" | "flash";
  x: number;
  y: number;
  w: number;
  h: number;
}

export function HexGrid({ tab }: { tab: Tab }) {
  const s = useSettings((st) => st.s);
  const wrapRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [size, setSize] = useState({ w: 800, h: 600 });
  const [hover, setHover] = useState(-1);
  const [tick, setTick] = useState(0);
  const [palette, setPalette] = useState<Palette | null>(null);
  const [fx, setFx] = useState<Fx[]>([]);
  const [focused, setFocused] = useState(true);
  const cache = cacheFor(tab.id);
  const update = useUi((u) => u.set);

  const fs = fontSize(s);
  const family =
    s.fontFamily || getComputedStyle(document.documentElement).getPropertyValue("--mono");
  const font = `400 ${fs}px ${family}`;
  const rowH = rowHeight(s);
  const cw = useMemo(() => measureChar(font), [font]);
  const layout: GridLayout = useMemo(
    () =>
      computeLayout({
        s,
        cw,
        rowH,
        width: size.w,
        offsetDigits: offsetWidth(tab.info.len + s.baseAddress, s.offsetBase, s.offsetSegment),
      }),
    [s, cw, rowH, size.w, tab.info.len],
  );
  A.viewport.bpr = layout.bpr;
  A.viewport.rows = Math.max(1, Math.floor(size.h / rowH));
  A.viewport.cellBytes = layout.cellBytes;

  // Palette follows theme, accent and overrides.
  useEffect(() => {
    const read = () => setPalette(readPalette());
    read();
    const mo = new MutationObserver(read);
    mo.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["data-theme", "style", "data-cvd"],
    });
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    mq.addEventListener("change", read);
    return () => {
      mo.disconnect();
      mq.removeEventListener("change", read);
    };
  }, []);

  const atlas = useMemo(() => {
    if (!palette) return null;
    return new HexAtlas(
      font,
      palette.kinds.slice(0, 1).concat(palette.kinds.slice(1)),
      cw,
      rowH,
      window.devicePixelRatio || 1,
      s.lowercase,
    );
  }, [palette, font, cw, rowH, s.lowercase]);

  const [decoder, setDecoder] = useState<TextDecoder2 | TblDecoder>(
    () => new TextDecoder2(A.encodingInfo(s.encoding), A.cachedTable(s.encoding), s.nonPrintable),
  );
  useEffect(() => {
    let alive = true;
    if (s.encoding.startsWith("tbl:")) {
      const src = s.tblTables[s.encoding.slice(4)];
      if (src !== undefined) setDecoder(new TblDecoder(parseTbl(src)));
      return;
    }
    A.encodingTable(s.encoding).then((t) => {
      if (alive) setDecoder(new TextDecoder2(A.encodingInfo(s.encoding), t, s.nonPrintable));
    });
    return () => {
      alive = false;
    };
  }, [s.encoding, s.nonPrintable, s.tblTables]);

  useEffect(() => cache.subscribe(() => setTick((x) => x + 1)), [cache]);
  useEffect(() => {
    cache.sync(tab.info.len, tab.info.version);
  }, [cache, tab.info.len, tab.info.version]);

  useLayoutEffect(() => {
    const el = wrapRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setSize({ w: el.clientWidth, h: el.clientHeight }));
    ro.observe(el);
    setSize({ w: el.clientWidth, h: el.clientHeight });
    return () => ro.disconnect();
  }, []);

  const totalRows = Math.floor(tab.info.len / layout.bpr) + 1;
  const visibleRows = Math.max(1, Math.floor(size.h / rowH));
  const maxTop = Math.max(0, totalRows - visibleRows + 1);
  const topRow = Math.min(tab.topRow, maxTop);

  // Keep the cursor visible when the row width changes.
  const prevBpr = useRef(layout.bpr);
  useEffect(() => {
    if (prevBpr.current !== layout.bpr) {
      prevBpr.current = layout.bpr;
      A.scrollIntoView(tab.id, tab.cursor, true);
    }
  }, [layout.bpr, tab.id, tab.cursor]);

  // Highlight layers (DISP-16): marks plus regions contributed by panels.
  const overlayLayers = useOverlays((o) => o.layers[tab.id]);
  const { regions, pins } = useMemo(() => {
    const regions: { start: number; end: number; color: string }[] = [];
    const pins: { offset: number; color: string; annotation: boolean }[] = [];
    if (s.layers.marks) {
      for (const m of tab.marks) {
        const c = markColor(m.color);
        if (m.kind !== "label" && m.end > m.start)
          regions.push({ start: m.start, end: m.end, color: withAlpha(c, 0.2) });
        pins.push({ offset: m.start, color: c, annotation: m.kind === "annotation" });
      }
    }
    for (const [name, list] of Object.entries(overlayLayers ?? {})) {
      const key = name as keyof typeof s.layers;
      if (key in s.layers && !s.layers[key]) continue;
      regions.push(...list);
    }
    return { regions, pins };
  }, [tab.marks, overlayLayers, s.layers]);

  // Draw.
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || !palette) return;
    const dpr = window.devicePixelRatio || 1;
    const w = size.w;
    const h = size.h;
    if (canvas.width !== Math.round(w * dpr) || canvas.height !== Math.round(h * dpr)) {
      canvas.width = Math.round(w * dpr);
      canvas.height = Math.round(h * dpr);
      canvas.style.width = `${w}px`;
      canvas.style.height = `${h}px`;
    }
    const ctx = canvas.getContext("2d")!;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    const r = tab.results;
    renderGrid({
      ctx,
      width: w,
      height: h,
      layout,
      s,
      pal: palette,
      font,
      atlas,
      cache,
      decoder,
      len: tab.info.len,
      topRow,
      scrollX: tab.scrollX,
      cursor: tab.cursor,
      anchor: tab.anchor,
      column: tab.column,
      hover,
      matches: s.layers.search ? (r?.matches ?? []) : [],
      currentMatch: r?.current ?? -1,
      readOnly: tab.info.mode === "readOnly",
      regions,
      pins,
      extraSel: tab.extra,
      bit: tab.bit,
    });
  });

  // Effects (edit pop, jump flash).
  const rectOf = useCallback(
    (offset: number, col: "hex" | "text") => {
      const row = Math.floor(offset / layout.bpr);
      const i = offset % layout.bpr;
      const y = (row - topRow) * rowH + 1.5;
      if (col === "hex") {
        const c = Math.floor(i / layout.cellBytes);
        const x =
          c < layout.cellX.length ? layout.hexX + layout.cellX[c] : layout.hexX + layout.hexW + 4;
        return { x: x - tab.scrollX, y, w: layout.cellW, h: rowH - 3 };
      }
      return {
        x: layout.textX + i * layout.textCharW - tab.scrollX,
        y,
        w: layout.textCharW,
        h: rowH - 3,
      };
    },
    [layout, topRow, rowH, tab.scrollX],
  );
  useEffect(
    () =>
      A.onFx((e) => {
        const r = rectOf(e.start, tab.column);
        const id = Math.random();
        let { w } = r;
        if (e.kind === "flash" && e.end - e.start > 1) {
          const sameRow = Math.floor(e.start / layout.bpr) === Math.floor((e.end - 1) / layout.bpr);
          if (sameRow) w = rectOf(e.end - 1, tab.column).x + r.w - r.x;
        }
        setFx((list) => [...list.slice(-6), { id, kind: e.kind, ...r, w }]);
        setTimeout(() => setFx((list) => list.filter((f) => f.id !== id)), 1000);
      }),
    [rectOf, tab.column, layout.bpr],
  );

  // Cursor overlay.
  const [buffer, setBuffer] = useState("");
  useEffect(() => {
    const l = () => setBuffer(A.cellBuffer.text);
    A.cellBuffer.listeners.add(l);
    return () => {
      A.cellBuffer.listeners.delete(l);
    };
  }, []);
  const cursorOff = tab.cursor;
  const main = rectOf(
    cursorOff - (tab.column === "hex" ? cursorOff % layout.cellBytes : 0),
    tab.column,
  );
  const ghostCol = tab.column === "hex" ? "text" : "hex";
  const ghost = rectOf(
    cursorOff - (ghostCol === "hex" ? cursorOff % layout.cellBytes : 0),
    ghostCol,
  );
  const label =
    buffer || (tab.column === "hex" ? cursorLabel(cache, s, layout, cursorOff, tab.info.len) : "");
  const textChar =
    tab.column === "text" && cursorOff < tab.info.len
      ? decoder.glyph((q) => (q >= 0 && q < tab.info.len ? cache.byte(q) : undefined), cursorOff).ch
      : "";
  const insertMode = tab.info.mode === "insert";
  const showCursor = !hasSelection(tab) || true;
  const visible = main.y > -rowH && main.y < size.h;

  // Disable the glide while scrolling so the cursor sticks to its byte.
  const lastTop = useRef(topRow);
  const scrolling = lastTop.current !== topRow;
  lastTop.current = topRow;
  const lastJump = useRef(tab.jumpSeq);
  const jumping = lastJump.current !== tab.jumpSeq;
  lastJump.current = tab.jumpSeq;

  // ------------------------------------------------------------- input
  const hit = useCallback(
    (e: { clientX: number; clientY: number }) => {
      const el = wrapRef.current!;
      const r = el.getBoundingClientRect();
      const x = e.clientX - r.left + tab.scrollX;
      const y = e.clientY - r.top;
      const row = Math.floor(topRow + y / rowH);
      const hc = layout.columns.includes("hex") ? hitHexCell(layout, x) : -1;
      if (hc >= 0) {
        const off = row * layout.bpr + hc * layout.cellBytes;
        const cellX = layout.hexX + layout.cellX[hc];
        const nib = layout.cellBytes === 1 && x - cellX > layout.cellW / 2 ? 1 : 0;
        return { offset: Math.min(Math.max(0, off), tab.info.len), column: "hex" as const, nib };
      }
      const tb = hitTextByte(layout, x);
      if (tb >= 0)
        return {
          offset: Math.min(Math.max(0, row * layout.bpr + tb), tab.info.len),
          column: "text" as const,
          nib: 0,
        };
      // Outside columns: snap to row start/end.
      const off = row * layout.bpr + (x < layout.hexX ? 0 : layout.bpr - 1);
      return { offset: Math.min(Math.max(0, off), tab.info.len), column: tab.column, nib: 0 };
    },
    [layout, topRow, rowH, tab.scrollX, tab.info.len, tab.column],
  );

  const dragging = useRef(false);
  const onMouseDown = (e: ReactMouseEvent) => {
    if (e.button !== 0) return;
    wrapRef.current?.focus();
    A.commitCellBuffer();
    const h = hit(e);
    const update = useTabs.getState().update;
    if (e.detail === 2) {
      // Double click: select the group under the cursor.
      const g = Math.max(1, s.group);
      const start = h.offset - (h.offset % g);
      A.selectRange(start, Math.min(start + g, tab.info.len));
      return;
    }
    const additive = isMac ? e.metaKey : e.ctrlKey;
    const rect = e.altKey;
    update(tab.id, (t) => {
      // NAV-10: ⌘/Ctrl-click keeps the current selection as an extra range.
      const keep =
        additive && t.anchor !== t.cursor
          ? [
              ...t.extra,
              { start: Math.min(t.anchor, t.cursor), end: Math.max(t.anchor, t.cursor) + 1 },
            ]
          : additive
            ? t.extra
            : [];
      return {
        cursor: h.offset,
        anchor: e.shiftKey ? t.anchor : h.offset,
        column: h.column,
        nibble: 0,
        bit: 0,
        extra: rect ? [] : keep,
      };
    });
    dragging.current = true;
    let lastY = e.clientY;
    let auto: ReturnType<typeof setInterval> | undefined;
    const bpr = layout.bpr;
    const r0 = Math.floor(h.offset / bpr);
    const c0 = h.offset % bpr;
    const move = (ev: MouseEvent) => {
      lastY = ev.clientY;
      const p = hit(ev);
      if (rect) {
        // NAV-11: rectangular (column) selection, one range per row.
        const r1 = Math.floor(p.offset / bpr);
        const c1 = p.offset % bpr;
        const [ra, rb] = r0 <= r1 ? [r0, r1] : [r1, r0];
        const [ca, cb] = c0 <= c1 ? [c0, c1] : [c1, c0];
        const extra: { start: number; end: number }[] = [];
        for (let r = ra; r <= rb && extra.length < 100_000; r++) {
          const start = r * bpr + ca;
          if (start >= tab.info.len) break;
          extra.push({ start, end: Math.min(r * bpr + cb + 1, tab.info.len) });
        }
        update(tab.id, { cursor: p.offset, anchor: p.offset, extra, nibble: 0 });
        return;
      }
      update(tab.id, { cursor: p.offset, nibble: 0 });
    };
    auto = setInterval(() => {
      const r = wrapRef.current?.getBoundingClientRect();
      if (!r || !dragging.current) return;
      const d = lastY < r.top ? lastY - r.top : lastY > r.bottom ? lastY - r.bottom : 0;
      if (d !== 0) {
        update(tab.id, (t) => ({ topRow: Math.max(0, Math.min(maxTop, t.topRow + d / rowH / 2)) }));
      }
    }, 30);
    const up = () => {
      dragging.current = false;
      clearInterval(auto);
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  };

  const [tip, setTip] = useState<{ x: number; y: number; offset: number } | null>(null);
  const tipTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const onMouseMove = (e: ReactMouseEvent) => {
    if (dragging.current) return;
    const h = hit(e);
    if (h.offset !== hover) {
      setHover(h.offset < tab.info.len ? h.offset : -1);
      setTip(null);
      clearTimeout(tipTimer.current);
      const r = wrapRef.current?.getBoundingClientRect();
      if (r && h.offset < tab.info.len) {
        const pos = { x: e.clientX - r.left + 14, y: e.clientY - r.top + 16, offset: h.offset };
        tipTimer.current = setTimeout(() => setTip(pos), 450);
      }
    }
  };
  useEffect(() => () => clearTimeout(tipTimer.current), []);
  const tipContent = (() => {
    if (!tip) return null;
    const notes = tab.marks.filter(
      (m) =>
        m.kind === "annotation" &&
        tip.offset >= m.start &&
        tip.offset < Math.max(m.end, m.start + 1),
    );
    const labels = Object.values(overlayLayers ?? {})
      .flat()
      .filter((r) => r.label && tip.offset >= r.start && tip.offset < r.end)
      .map((r) => r.label as string);
    const values: string[] = [];
    if (s.hoverInspector) {
      const b = [0, 1, 2, 3, 4, 5, 6, 7].map((i) => cache.byte(tip.offset + i));
      if (b.slice(0, 4).every((x) => x !== undefined)) {
        const dv = new DataView(Uint8Array.from(b.map((x) => x ?? 0)).buffer);
        const le = s.inspectorEndian === "little";
        values.push(
          `u8 ${b[0]} · i8 ${dv.getInt8(0)}`,
          `u16 ${dv.getUint16(0, le)} · i16 ${dv.getInt16(0, le)}`,
          `u32 ${dv.getUint32(0, le)} · i32 ${dv.getInt32(0, le)}`,
          `f32 ${dv.getFloat32(0, le).toPrecision(7)}`,
        );
      }
    }
    if (!notes.length && !labels.length && !values.length) return null;
    return (
      <div className="note-tip" style={{ left: tip.x, top: tip.y }}>
        {notes.map((n) => (
          <div key={n.id}>
            <b>{n.name}</b>
            {/* biome-ignore lint/security/noDangerouslySetInnerHtml: escaped Markdown */}
            <div
              className="md"
              dangerouslySetInnerHTML={{ __html: renderMarkdown(n.note || "—") }}
            />
          </div>
        ))}
        {labels.map((l) => (
          <div key={l} className="mono" style={{ fontSize: 12 }}>
            {l}
          </div>
        ))}
        {values.length > 0 && (
          <div className="mono mut" style={{ fontSize: 11.5 }}>
            {values.map((v) => (
              <div key={v}>{v}</div>
            ))}
          </div>
        )}
      </div>
    );
  })();

  // Native wheel listener so preventDefault works (passive: false).
  useEffect(() => {
    const el = wrapRef.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      if (e.ctrlKey || e.metaKey) {
        const st = useSettings.getState();
        const z = Math.min(2, Math.max(0.6, +(st.s.zoom - Math.sign(e.deltaY) * 0.05).toFixed(2)));
        st.set({ zoom: z });
        return;
      }
      const mult = e.deltaMode === 1 ? rowH : e.deltaMode === 2 ? size.h : 1;
      const dy = e.deltaY * mult;
      const dx = (e.shiftKey ? e.deltaY : e.deltaX) * (e.deltaMode === 1 ? cw : 1);
      useTabs.getState().update(tab.id, (t) => {
        const maxX = Math.max(0, layout.totalW - size.w);
        return {
          topRow: e.shiftKey ? t.topRow : Math.max(0, Math.min(maxTop, t.topRow + dy / rowH)),
          scrollX: Math.max(0, Math.min(maxX, t.scrollX + (Math.abs(dx) > 0 ? dx : 0))),
        };
      });
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, [tab.id, rowH, maxTop, size, layout.totalW, cw]);

  const onContextMenu = (e: ReactMouseEvent) => {
    e.preventDefault();
    const h = hit(e);
    const sel = selection(tab);
    if (!(hasSelection(tab) && h.offset >= sel.start && h.offset < sel.end)) {
      useTabs.getState().update(tab.id, { cursor: h.offset, anchor: h.offset, column: h.column });
    }
    update({ menu: { x: e.clientX, y: e.clientY, items: contextItems() } });
  };

  const colHead = useMemo(() => {
    const cells: { x: number; t: string; i: number }[] = [];
    for (let c = 0; c < layout.cellX.length; c++) {
      const i = c * layout.cellBytes;
      cells.push({ x: layout.hexX + layout.cellX[c], t: hexN(i, 2, s.lowercase), i });
    }
    return cells;
  }, [layout, s.lowercase]);
  const curCol = Math.floor((tab.cursor % layout.bpr) / layout.cellBytes);

  // Screen reader announcement (UI-17).
  const b = cache.byte(tab.cursor);
  const announce = `Смещение ${hexN(tab.cursor)}, значение ${b === undefined ? "нет" : hexN(b, 2)}`;

  return (
    <section className="ed" aria-label="Данные файла" data-tick={tick}>
      {s.showColHeader && (
        <div className="colhead" aria-hidden="true">
          <div
            style={{ position: "absolute", inset: 0, transform: `translateX(${-tab.scrollX}px)` }}
          >
            {layout.columns.includes("offset") && (
              <span className="asc-h" style={{ left: layout.offsetX, width: layout.offsetW }}>
                Смещение
              </span>
            )}
            {layout.columns.includes("hex") &&
              colHead.map((h, c) => (
                <span
                  key={h.i}
                  className={c === curCol ? "on" : ""}
                  style={{ left: h.x, width: layout.cellW }}
                >
                  {h.t}
                </span>
              ))}
            {layout.columns.includes("text") && (
              <span className="asc-h" style={{ left: layout.textX, width: layout.textW + 80 }}>
                {A.encodingInfo(s.encoding).name}
              </span>
            )}
          </div>
        </div>
      )}
      <div style={{ display: "flex", flex: "1 1 auto", minHeight: 0 }}>
        <div
          ref={wrapRef}
          className="gridwrap"
          data-hx-grid=""
          tabIndex={0}
          role="grid"
          aria-label="Шестнадцатеричные данные"
          aria-readonly={tab.info.mode === "readOnly"}
          onMouseDown={onMouseDown}
          onMouseMove={onMouseMove}
          onMouseLeave={() => {
            setHover(-1);
            setTip(null);
            clearTimeout(tipTimer.current);
          }}
          onContextMenu={onContextMenu}
          onFocus={() => setFocused(true)}
          onBlur={() => setFocused(false)}
          style={{ ["--fs" as string]: `${fs}px` }}
        >
          <canvas ref={canvasRef} />
          <div className="overlay">
            {fx.map((f) => (
              <div
                key={f.id}
                className={f.kind === "pop" ? "edit-pop" : "flash"}
                style={{ left: f.x, top: f.y, width: f.w, height: f.h }}
              />
            ))}
            {showCursor && visible && (
              <>
                <div
                  className={`cursor ghost${jumping ? " jump" : ""}`}
                  style={{
                    transform: `translate(${ghost.x}px, ${ghost.y}px)`,
                    width: ghost.w,
                    height: ghost.h,
                    transition: scrolling ? "none" : undefined,
                  }}
                />
                <div
                  className={`cursor${jumping ? " jump" : ""}${insertMode ? " ins" : ""}${focused ? "" : " blur"}`}
                  style={{
                    transform: `translate(${main.x}px, ${main.y}px)`,
                    width: main.w,
                    height: main.h,
                    transition: scrolling ? "none" : undefined,
                    fontSize: fs,
                  }}
                >
                  {tab.column === "hex" ? label : textChar}
                  {focused &&
                    tab.column === "hex" &&
                    s.cellView === "bin" &&
                    layout.cellBytes === 1 && (
                      <span className="bitmark" style={{ left: (0.275 + tab.bit) * cw }} />
                    )}
                  {focused &&
                    tab.column === "hex" &&
                    !buffer &&
                    layout.cellBytes === 1 &&
                    s.cellView === "hex" && <span className={`nib${tab.nibble ? " n1" : ""}`} />}
                </div>
              </>
            )}
          </div>
          {tab.info.len === 0 && (
            <div className="empty-doc">
              <b>Пустой файл</b>
              <span>
                {tab.info.mode === "readOnly"
                  ? "Только чтение"
                  : "Начните вводить hex или вставьте данные"}
              </span>
            </div>
          )}
          {tipContent}
          <div className="vh" aria-live="polite">
            {announce}
          </div>
        </div>
        <Minimap
          tab={tab}
          totalRows={totalRows}
          visibleRows={visibleRows}
          topRow={topRow}
          maxTop={maxTop}
          bpr={layout.bpr}
        />
      </div>
    </section>
  );
}

function k(id: string) {
  return displayKeys(keysFor(id));
}

function contextItems(): MenuItem[] {
  const run = (id: string) => () => import("../lib/commands").then((m) => m.runCommand(id));
  return [
    { header: "Выделение" },
    { label: "Копировать", keys: k("edit.copy"), run: run("edit.copy") },
    { label: "Вырезать", keys: k("edit.cut"), run: run("edit.cut") },
    { label: "Вставить", keys: k("edit.paste"), run: run("edit.paste") },
    {
      label: "Копировать как",
      submenu: [
        { label: "Hex-строка", run: run("edit.copyHex") },
        { label: "Массив C", run: run("edit.copyC") },
        { label: "Base64", run: run("edit.copyBase64") },
        { label: "Дамп xxd", run: run("edit.copyXxd") },
        { separator: true },
        {
          label: "Другой формат…",
          keys: k("edit.copyAs"),
          run: () => openDialog({ kind: "copyAs" }),
        },
      ],
    },
    { separator: true },
    { label: "Интерпретировать (инспектор)", run: run("view.inspector") },
    { label: "Хэш выделения", run: run("view.hashes") },
    {
      label: "Преобразовать",
      submenu: [
        { label: "Поменять порядок байт · 16", run: run("edit.swap16") },
        { label: "Поменять порядок байт · 32", run: run("edit.swap32") },
        { label: "Поменять порядок байт · 64", run: run("edit.swap64") },
        { label: "Развернуть", run: run("edit.reverse") },
        { label: "Обнулить", run: run("edit.zero") },
        { label: "Заполнить…", run: run("edit.fill") },
      ],
    },
    { label: "Перейти по указателю", keys: k("nav.follow32"), run: run("nav.follow32") },
    { separator: true },
    { label: "Сохранить выделение как…", run: run("file.saveSelection") },
    { label: "Выделить диапазон…", run: run("select.range") },
  ];
}
