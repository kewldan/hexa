// Dockable side panels (UI-01): drag a panel tab to the other side, resize, persist.
import { useState } from "react";
import { startPointerDrag } from "../lib/pointerDrag";
import { type PanelId, useSettings } from "../state/settings";
import type { Tab } from "../state/tabs";
import { Icon } from "./icons";
import { AnalysisPanel } from "./panels/AnalysisPanel";
import { ConsolePanel } from "./panels/ConsolePanel";
import { DisasmPanel } from "./panels/DisasmPanel";
import { HashPanel } from "./panels/HashPanel";
import { HistoryPanel } from "./panels/HistoryPanel";
import { InspectorPanel } from "./panels/InspectorPanel";
import { MarksPanel } from "./panels/MarksPanel";
import { ResultsPanel } from "./panels/ResultsPanel";
import { StringsPanel } from "./panels/StringsPanel";
import { StructurePanel } from "./panels/StructurePanel";
import { TransformPanel } from "./panels/TransformPanel";

export const TITLES: Record<PanelId, string> = {
  history: "История",
  hashes: "Хэши",
  results: "Поиск",
  inspector: "Инспектор",
  marks: "Закладки",
  structure: "Структура",
  analysis: "Анализ",
  transform: "Преобразования",
  disasm: "Код",
  strings: "Строки",
  console: "Консоль",
};

const ICONS: Record<PanelId, () => React.ReactElement> = {
  history: Icon.clock,
  hashes: Icon.hash,
  results: Icon.search,
  inspector: Icon.sidebarR,
  marks: Icon.star,
  structure: Icon.layers,
  analysis: Icon.chart,
  transform: Icon.refresh,
  disasm: Icon.code,
  strings: Icon.list,
  console: Icon.terminal,
};

const PANELS: Record<PanelId, (p: { tab: Tab }) => React.ReactElement | null> = {
  history: HistoryPanel,
  hashes: HashPanel,
  results: ResultsPanel,
  inspector: InspectorPanel,
  marks: MarksPanel,
  structure: StructurePanel,
  analysis: AnalysisPanel,
  transform: TransformPanel,
  disasm: DisasmPanel,
  strings: StringsPanel,
  console: ConsolePanel,
};

/** Moves a panel to a side and shows it there (UI-01). */
export function movePanelTo(side: "left" | "right", id: PanelId) {
  const { s, setLayout } = useSettings.getState();
  const layout = s.layout;
  const panels = side === "left" ? layout.left : layout.right;
  if (panels.includes(id)) return;
  const left = layout.left.filter((p) => p !== id);
  const right = layout.right.filter((p) => p !== id);
  if (side === "left") left.push(id);
  else right.push(id);
  setLayout({
    left,
    right,
    leftActive:
      side === "left" ? id : layout.leftActive === id ? (left[0] ?? null) : layout.leftActive,
    rightActive:
      side === "right" ? id : layout.rightActive === id ? (right[0] ?? null) : layout.rightActive,
    leftOpen: side === "left" ? true : layout.leftOpen,
    rightOpen: side === "right" ? true : layout.rightOpen,
  });
}

export function Sidebar({ side, tab }: { side: "left" | "right"; tab: Tab }) {
  const layout = useSettings((s) => s.s.layout);
  const setLayout = useSettings((s) => s.setLayout);
  const [resizing, setResizing] = useState(false);
  const panels = side === "left" ? layout.left : layout.right;
  const open = side === "left" ? layout.leftOpen : layout.rightOpen;
  const width = side === "left" ? layout.leftWidth : layout.rightWidth;
  const activeId = (side === "left" ? layout.leftActive : layout.rightActive) ?? panels[0] ?? null;
  const active = panels.includes(activeId as PanelId) ? activeId : (panels[0] ?? null);

  const setActive = (id: PanelId) =>
    setLayout(side === "left" ? { leftActive: id } : { rightActive: id });

  const startResize = (e: React.MouseEvent) => {
    e.preventDefault();
    setResizing(true);
    const x0 = e.clientX;
    const w0 = width;
    const move = (ev: MouseEvent) => {
      const d = side === "left" ? ev.clientX - x0 : x0 - ev.clientX;
      const w = Math.max(200, Math.min(560, w0 + d));
      setLayout(side === "left" ? { leftWidth: w } : { rightWidth: w });
    };
    const up = () => {
      setResizing(false);
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  };

  return (
    <aside
      className={`side ${side === "left" ? "l" : "r"}${open && panels.length ? "" : " shut"}${resizing ? " resizing" : ""}`}
      style={{ width }}
      aria-label={side === "left" ? "Левая панель" : "Правая панель"}
    >
      <div className="side-in" style={{ width }}>
        <div className="side-tabs" role="tablist" data-side={side}>
          {panels.map((id) => (
            <button
              type="button"
              key={id}
              role="tab"
              aria-selected={id === active}
              aria-label={TITLES[id]}
              className={`side-tab${id === active ? " on" : ""}`}
              title="Перетащите на другую панель, чтобы переместить"
              onPointerDown={(e) =>
                startPointerDrag(e, {
                  onMove: (_ev, over) => {
                    for (const el of document.querySelectorAll(".side-tabs.drop"))
                      el.classList.remove("drop");
                    const target = over?.closest<HTMLElement>("[data-side]");
                    if (target && target.dataset.side !== side) target.classList.add("drop");
                  },
                  onDrop: (_ev, over) => {
                    for (const el of document.querySelectorAll(".side-tabs.drop"))
                      el.classList.remove("drop");
                    const target = over?.closest<HTMLElement>("[data-side]")?.dataset.side;
                    if (target === "left" || target === "right") movePanelTo(target, id);
                  },
                })
              }
              onClick={() => setActive(id)}
            >
              {panels.length > 3 ? (
                <span className="side-ico" title={TITLES[id]}>
                  {ICONS[id]()}
                </span>
              ) : (
                TITLES[id]
              )}
              {id === "results" && tab.results && tab.results.matches.length > 0 && (
                <span className="cnt">
                  {tab.results.matches.length > 999 ? "999+" : tab.results.matches.length}
                </span>
              )}
            </button>
          ))}
        </div>
        <div className="panel-body" key={active ?? "none"}>
          {active && panels.length > 3 && <div className="panel-title">{TITLES[active]}</div>}
          {active &&
            (() => {
              const P = PANELS[active];
              return <P tab={tab} />;
            })()}
        </div>
      </div>
      <div className={`splitter${resizing ? " on" : ""}`} onMouseDown={startResize} />
    </aside>
  );
}
