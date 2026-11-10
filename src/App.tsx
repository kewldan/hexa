import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useEffect, useRef } from "react";
import { Banner } from "./components/Banner";
import { Dialogs } from "./components/dialogs/Dialogs";
import { EditorArea } from "./components/EditorArea";
import { FindBar } from "./components/FindBar";
import { ContextMenu, GotoDialog, Palette, ToastView } from "./components/Overlays";
import { Sidebar } from "./components/Sidebar";
import { StartScreen } from "./components/StartScreen";
import { StatusBar } from "./components/StatusBar";
import { Toolbar } from "./components/Toolbar";
import { api } from "./ipc/api";
import { resolve, runCommand } from "./lib/commands";
import { chordOf, isMac } from "./lib/keys";
import { useVim, vimKey } from "./lib/vim";
import * as A from "./state/actions";
import { useSettings } from "./state/settings";
import { activeTab, useActiveTab, useTabs } from "./state/tabs";
import { confirm, toast, useUi } from "./state/ui";

function useTheme() {
  const s = useSettings((st) => st.s);
  useEffect(() => {
    const root = document.documentElement;
    const apply = () => {
      const dark = window.matchMedia("(prefers-color-scheme: dark)").matches;
      const theme = s.theme === "system" ? (dark ? "dark" : "light") : s.theme;
      root.dataset.theme = theme;
      root.style.cssText = "";
      root.style.setProperty("--acc", s.accent);
      for (const [k, v] of Object.entries(s.themeOverrides)) root.style.setProperty(k, v);
      root.dataset.motion = s.reduceMotion ? "reduce" : "full";
      root.dataset.cvd = s.colorBlind ? "1" : "0";
      root.style.setProperty("--ui-scale", String(s.uiScale));
    };
    apply();
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    mq.addEventListener("change", apply);
    return () => mq.removeEventListener("change", apply);
  }, [s.theme, s.accent, s.themeOverrides, s.reduceMotion, s.colorBlind, s.uiScale]);
}

/** Global keyboard: chords (with Emacs-style sequences), vim, then typing. */
function useKeyboard() {
  const seq = useRef<string[]>([]);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented || e.isComposing) return;
      const ui = useUi.getState();
      if (ui.dialog || ui.palette || ui.goto || ui.menu) return;
      const target = e.target as HTMLElement;
      const inField = target.closest("input, textarea, select, [contenteditable]") !== null;
      const t = activeTab();
      const gridFocused = target.closest("[data-hx-grid]") !== null || target === document.body;

      if (e.key === "Escape") {
        if (A.cellBuffer.text) {
          A.cancelCellBuffer();
          return;
        }
        if (ui.find && !inField) ui.set({ find: false });
      }
      if (useSettings.getState().s.keymap === "vim" && t && gridFocused && !inField && vimKey(e)) {
        e.preventDefault();
        return;
      }
      const chord = chordOf(e);
      if (!chord) return;
      const full = [...seq.current, chord].join(" ");
      const r = resolve(full);
      if (r.prefix && !r.id) {
        seq.current.push(chord);
        e.preventDefault();
        return;
      }
      seq.current = [];
      // Plain keys inside inputs belong to the input.
      const plain = !/^(Mod|Ctrl|Alt|Meta)\+/.test(chord) && !/^F\d+$/.test(chord);
      if (r.id && !(inField && plain)) {
        e.preventDefault();
        runCommand(r.id);
        return;
      }
      if (inField || !t || !gridFocused) return;
      if (e.ctrlKey || e.metaKey || (e.altKey && !isMac)) return;
      // Typing (EDIT-02).
      if (useSettings.getState().s.keymap === "vim" && useVim.getState().mode === "normal") return;
      if (e.key.length !== 1) {
        if (e.key === "Enter" && A.cellBuffer.text) {
          A.commitCellBuffer();
          e.preventDefault();
        }
        return;
      }
      e.preventDefault();
      if (t.column === "hex") {
        const n = Number.parseInt(e.key, 16);
        const s = useSettings.getState().s;
        if (s.cellView === "bin" && s.cellWidth === 1 && (e.key === "0" || e.key === "1"))
          A.typeBit(e.key === "1" ? 1 : 0);
        else if (s.cellView === "char") A.typeText(e.key);
        else if (s.cellView === "float")
          toast("Числа с плавающей точкой правятся в инспекторе", true);
        else if (s.cellView !== "hex" || s.cellWidth !== 1) A.typeCellDigit(e.key);
        else if (Number.isFinite(n) && /^[0-9a-f]$/i.test(e.key)) A.typeHexNibble(n);
      } else {
        A.typeText(e.key);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
}

function useBackendEvents() {
  useEffect(() => {
    const offs: Promise<() => void>[] = [];
    offs.push(listen<string[]>("hexa://open-files", (e) => A.openPaths(e.payload)));
    offs.push(
      listen<{ id: number }>("hexa://disk-changed", async (e) => {
        const t = useTabs.getState().tabs.find((x) => x.id === e.payload.id);
        if (!t) return;
        useTabs.getState().update(t.id, {
          banner: {
            kind: "diskChanged",
            title: `${t.info.name} изменён на диске`,
            sub: t.info.modified
              ? "У вас есть несохранённые правки."
              : "Другая программа изменила файл.",
          },
        });
      }),
    );
    offs.push(
      listen<{ id: number }>("hexa://doc-updated", async (e) => {
        const t = useTabs.getState().tabs.find((x) => x.id === e.payload.id);
        if (!t) return;
        const info = await api.info(t.id);
        A.applyInfo(info);
        if (info.follow) {
          A.scrollIntoView(t.id, info.len, false);
          useTabs.getState().update(t.id, { cursor: info.len, anchor: info.len });
        }
      }),
    );
    // Files dropped onto the window (FILE-01).
    offs.push(
      getCurrentWebview().onDragDropEvent((e) => {
        const ui = useUi.getState();
        if (e.payload.type === "enter" || e.payload.type === "over") ui.set({ dragOver: true });
        else if (e.payload.type === "leave") ui.set({ dragOver: false });
        else if (e.payload.type === "drop") {
          ui.set({ dragOver: false });
          A.openPaths(e.payload.paths);
        }
      }),
    );
    // Unsaved changes on window close.
    offs.push(
      getCurrentWindow().onCloseRequested(async (ev) => {
        const tabs = useTabs.getState().tabs;
        const dirty = tabs.filter((t) => t.info.modified);
        const release = () => Promise.all(tabs.map((t) => api.release(t.id).catch(() => {})));
        if (dirty.length === 0 || !useSettings.getState().s.confirmClose) {
          await release();
          return;
        }
        ev.preventDefault();
        confirm(
          "Закрыть окно?",
          `Несохранённые правки в ${dirty.map((t) => `«${t.info.name}»`).join(", ")} будут потеряны.`,
          [
            { label: "Отмена", run: () => {} },
            {
              label: "Закрыть без сохранения",
              danger: true,
              run: async () => {
                await release();
                await getCurrentWindow().destroy();
              },
            },
          ],
        );
      }),
    );
    return () => {
      for (const o of offs) o.then((f) => f());
    };
  }, []);
}

export function App() {
  const loaded = useSettings((s) => s.loaded);
  const tab = useActiveTab();
  const ui = useUi();
  const layout = useSettings((s) => s.s.layout);
  useTheme();
  useKeyboard();
  useBackendEvents();

  useEffect(() => {
    (async () => {
      await useSettings.getState().load();
      await A.loadEncodings().catch(() => {});
      // Documents handed over by another window (tab tear-off).
      const docs = new URLSearchParams(location.search).get("docs");
      for (const id of docs?.split(",").map(Number).filter(Number.isFinite) ?? []) {
        try {
          await A.adopt(await api.info(id));
        } catch (e) {
          A.report(e);
        }
      }
      const files = await api.initialFiles().catch(() => []);
      if (files.length) await A.openPaths(files);
      const rec = await api.recoveryList().catch(() => []);
      if (rec.length && !docs)
        toast(`Есть несохранённые правки от прошлого запуска — см. стартовый экран`);
    })();
  }, []);

  useEffect(() => {
    getCurrentWindow()
      .setTitle(tab ? `${tab.info.modified ? "• " : ""}${tab.info.name} — Hexa` : "Hexa")
      .catch(() => {});
  }, [tab?.info.name, tab?.info.modified]);

  // Focus the grid when switching tabs.
  useEffect(() => {
    if (tab) {
      setTimeout(() => document.querySelector<HTMLElement>("[data-hx-grid]")?.focus(), 0);
      void A.refreshMarks(tab.id);
    }
  }, [tab?.id]);

  if (!loaded) return <div className="app" />;
  return (
    <div className={`app${isMac ? " mac" : ""}`}>
      <Toolbar start={!tab} />
      {tab ? (
        <>
          <Banner key={`b${tab.id}`} tab={tab} />
          {ui.find && <FindBar key={`f${tab.id}`} tab={tab} />}
          <main className="body">
            {layout.left.length > 0 && <Sidebar side="left" tab={tab} />}
            <EditorArea tab={tab} />
            {layout.right.length > 0 && <Sidebar side="right" tab={tab} />}
          </main>
          <StatusBar tab={tab} />
        </>
      ) : (
        <StartScreen />
      )}
      {ui.palette && <Palette />}
      {ui.goto && tab && <GotoDialog />}
      <ContextMenu />
      <Dialogs />
      <ToastView />
      {ui.dragOver && tab && <div className="drag-veil">Отпустите, чтобы открыть</div>}
    </div>
  );
}
