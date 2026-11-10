// Open tabs of this window and their view state.
import { create } from "zustand";
import type { DocInfo, Match } from "../ipc/api";

export type GridColumn = "hex" | "text";

export interface Banner {
  kind: "diskChanged" | "writeDenied" | "journal" | "info";
  title: string;
  sub?: string;
}

export interface SearchResults {
  matches: Match[];
  current: number;
  running: boolean;
  done: number;
  total: number;
  truncated: boolean;
  job: number | null;
  label: string;
}

export interface Tab {
  id: number;
  info: DocInfo;
  cursor: number;
  anchor: number;
  /** Second nibble of the byte is being typed. */
  nibble: 0 | 1;
  column: GridColumn;
  topRow: number;
  scrollX: number;
  back: number[];
  forward: number[];
  banner: Banner | null;
  results: SearchResults | null;
  /** Bumped to make the cursor animate with a spring (jumps). */
  jumpSeq: number;
  flash: { start: number; end: number; seq: number } | null;
}

interface TabsStore {
  tabs: Tab[];
  active: number | null;
  add: (info: DocInfo, activate?: boolean) => void;
  remove: (id: number) => void;
  activate: (id: number) => void;
  move: (id: number, before: number | null) => void;
  update: (id: number, patch: Partial<Tab> | ((t: Tab) => Partial<Tab>)) => void;
  setInfo: (info: DocInfo) => void;
}

export function newTab(info: DocInfo): Tab {
  return {
    id: info.id,
    info,
    cursor: 0,
    anchor: 0,
    nibble: 0,
    column: "hex",
    topRow: 0,
    scrollX: 0,
    back: [],
    forward: [],
    banner: info.writeDenied
      ? {
          kind: "writeDenied",
          title: `${info.name} открыт только для чтения`,
          sub: "Нет прав на запись. Можно сохранить через запрос прав администратора.",
        }
      : null,
    results: null,
    jumpSeq: 0,
    flash: null,
  };
}

export const useTabs = create<TabsStore>((set) => ({
  tabs: [],
  active: null,
  add: (info, activate = true) =>
    set((st) => {
      if (st.tabs.some((t) => t.id === info.id)) return { active: activate ? info.id : st.active };
      return { tabs: [...st.tabs, newTab(info)], active: activate ? info.id : st.active };
    }),
  remove: (id) =>
    set((st) => {
      const idx = st.tabs.findIndex((t) => t.id === id);
      const tabs = st.tabs.filter((t) => t.id !== id);
      let active = st.active;
      if (active === id) active = tabs[Math.min(idx, tabs.length - 1)]?.id ?? null;
      return { tabs, active };
    }),
  activate: (id) => set({ active: id }),
  move: (id, before) =>
    set((st) => {
      const tab = st.tabs.find((t) => t.id === id);
      if (!tab || id === before) return {};
      const rest = st.tabs.filter((t) => t.id !== id);
      const at = before === null ? rest.length : rest.findIndex((t) => t.id === before);
      rest.splice(at < 0 ? rest.length : at, 0, tab);
      return { tabs: rest };
    }),
  update: (id, patch) =>
    set((st) => ({
      tabs: st.tabs.map((t) =>
        t.id === id ? { ...t, ...(typeof patch === "function" ? patch(t) : patch) } : t,
      ),
    })),
  setInfo: (info) =>
    set((st) => ({
      tabs: st.tabs.map((t) => {
        if (t.id !== info.id) return t;
        const max = info.len;
        return {
          ...t,
          info,
          cursor: Math.min(t.cursor, max),
          anchor: Math.min(t.anchor, max),
        };
      }),
    })),
}));

export function activeTab(): Tab | undefined {
  const st = useTabs.getState();
  return st.tabs.find((t) => t.id === st.active);
}

export function useActiveTab(): Tab | undefined {
  return useTabs((st) => st.tabs.find((t) => t.id === st.active));
}

/**
 * Selected bytes as a half-open range. The selection runs from the anchor to
 * the cursor inclusive; equal anchor and cursor mean "no selection".
 */
export function selection(t: Pick<Tab, "cursor" | "anchor" | "info">): {
  start: number;
  end: number;
} {
  if (t.anchor === t.cursor) return { start: t.cursor, end: t.cursor };
  const start = Math.min(t.cursor, t.anchor);
  const end = Math.min(Math.max(t.cursor, t.anchor) + 1, t.info.len);
  return { start, end };
}

export function hasSelection(t: Pick<Tab, "cursor" | "anchor" | "info">): boolean {
  const s = selection(t);
  return s.end > s.start;
}

/** The selection, or the byte under the cursor when nothing is selected. */
export function selectionOrByte(t: Pick<Tab, "cursor" | "anchor" | "info">): {
  start: number;
  end: number;
} {
  const s = selection(t);
  if (s.end > s.start) return s;
  return { start: t.cursor, end: Math.min(t.cursor + 1, t.info.len) };
}
