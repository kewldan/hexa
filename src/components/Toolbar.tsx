// Window toolbar with tabs (FILE-09) and quick actions.
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useState } from "react";
import { cacheFor } from "../grid/cache";
import { keysFor, runCommand } from "../lib/commands";
import { detectFormat } from "../lib/detect";
import { displayKeys, isMac } from "../lib/keys";
import { startPointerDrag } from "../lib/pointerDrag";
import * as A from "../state/actions";
import { useSettings } from "../state/settings";
import { type Tab, useTabs } from "../state/tabs";
import { useUi } from "../state/ui";
import { Icon } from "./icons";

function badge(t: Tab): string {
  const c = cacheFor(t.id);
  const head = [0, 1, 2, 3, 4, 5, 6, 7].map((i) => c.byte(i) ?? -1);
  return detectFormat(head, t.info.name).badge;
}

export function Toolbar({ start }: { start: boolean }) {
  const { tabs, active, activate, move } = useTabs();
  const s = useSettings((st) => st.s);
  const set = useSettings((st) => st.set);
  const find = useUi((u) => u.find);
  const [dragId, setDragId] = useState<number | null>(null);
  const [dropBefore, setDropBefore] = useState<number | null>(null);

  const startDrag = (e: React.MouseEvent) => {
    if (e.button === 0 && e.target === e.currentTarget)
      getCurrentWindow()
        .startDragging()
        .catch(() => {});
  };

  const themeIcon =
    s.theme === "dark" ? <Icon.moon /> : s.theme === "contrast" ? <Icon.contrast /> : <Icon.sun />;

  return (
    <header className="tb" data-tauri-drag-region="" onMouseDown={startDrag}>
      {start ? (
        <span className="ttl">Hexa</span>
      ) : (
        <>
          <button
            type="button"
            className={`ib${s.layout.leftOpen ? " on" : ""}`}
            aria-label="Боковая панель"
            title={`Боковая панель  ${displayKeys(keysFor("view.left"))}`}
            onClick={() => runCommand("view.left")}
          >
            <Icon.sidebarL />
          </button>
          <nav className="tabs" aria-label="Открытые файлы">
            {tabs.map((t) => (
              <div
                key={t.id}
                className={`tab${t.id === active ? " on" : ""}${t.info.modified ? " mod" : ""}${dragId === t.id ? " dragging" : ""}${dropBefore === t.id && dragId !== t.id ? " drop-before" : ""}`}
                data-tab-id={t.id}
                onPointerDown={(e) => {
                  if ((e.target as HTMLElement).closest(".tab-x")) return;
                  startPointerDrag(e, {
                    onStart: () => setDragId(t.id),
                    onMove: (_ev, over) => {
                      const el = over?.closest<HTMLElement>("[data-tab-id]");
                      setDropBefore(el ? Number(el.dataset.tabId) : null);
                    },
                    onDrop: (_ev, over, outside) => {
                      const el = over?.closest<HTMLElement>("[data-tab-id]");
                      if (outside) A.tearOff(t.id);
                      else if (el) move(t.id, Number(el.dataset.tabId));
                      else if (over?.closest(".tabs")) move(t.id, null);
                      setDragId(null);
                      setDropBefore(null);
                    },
                    onCancel: () => {
                      setDragId(null);
                      setDropBefore(null);
                    },
                  });
                }}
                onAuxClick={(e) => {
                  if (e.button === 1) A.closeTab(t.id);
                }}
              >
                <button
                  type="button"
                  className="tab-b"
                  onClick={() => activate(t.id)}
                  title={t.info.path ?? t.info.name}
                >
                  <span className="badge">{badge(t)}</span>
                  <span className="tn">{t.info.name}</span>
                </button>
                <button
                  type="button"
                  className="tab-x"
                  aria-label="Закрыть вкладку"
                  onClick={() => A.closeTab(t.id)}
                >
                  <span className="dot" />
                  <Icon.close />
                </button>
              </div>
            ))}
            <button
              type="button"
              className="ib"
              aria-label="Открыть файл"
              title={`Открыть файл  ${displayKeys(keysFor("file.open"))}`}
              onClick={() => A.openWithDialog()}
            >
              <Icon.plus />
            </button>
          </nav>
        </>
      )}
      <div className="grow" data-tauri-drag-region="" onMouseDown={startDrag} />
      <button type="button" className="cmd" onClick={() => runCommand("view.palette")}>
        <Icon.search />
        <span className="lbl">Команды и поиск</span>
        <span className="kbd">{displayKeys(keysFor("view.palette"))}</span>
      </button>
      {!start && (
        <>
          <button
            type="button"
            className="ib"
            aria-label="Перейти к смещению"
            title={`Перейти к смещению  ${displayKeys(keysFor("nav.goto"))}`}
            onClick={() => runCommand("nav.goto")}
          >
            <Icon.goto />
          </button>
          <button
            type="button"
            className={`ib${find ? " on" : ""}`}
            aria-label="Найти"
            title={`Найти  ${displayKeys(keysFor("find.open"))}`}
            onClick={() => (find ? useUi.getState().set({ find: false }) : runCommand("find.open"))}
          >
            <Icon.find />
          </button>
        </>
      )}
      <span className="vsep" />
      <button
        type="button"
        className="ib"
        aria-label="Переключить тему"
        title={`Тема: ${s.theme}`}
        onClick={() => {
          const order = ["system", "light", "dark", "contrast"] as const;
          set({ theme: order[(order.indexOf(s.theme) + 1) % order.length] });
        }}
      >
        {themeIcon}
      </button>
      {!start && (
        <button
          type="button"
          className={`ib${s.layout.rightOpen ? " on" : ""}`}
          aria-label="Инспектор"
          title={`Инспектор  ${displayKeys(keysFor("view.right"))}`}
          onClick={() => runCommand("view.right")}
        >
          <Icon.sidebarR />
        </button>
      )}
      <button
        type="button"
        className="ib"
        aria-label="Настройки"
        title={`Настройки  ${displayKeys(keysFor("view.settings"))}`}
        onClick={() => runCommand("view.settings")}
      >
        <Icon.gear />
      </button>
      {!isMac && <span style={{ width: 4 }} />}
    </header>
  );
}
