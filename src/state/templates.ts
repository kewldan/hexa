// Templates applied to each tab (STRC-08): results, the grid layer and
// smart selection over field boundaries (NAV-12).
import { create } from "zustand";
import { type Applied, type TplSource, tpl } from "../ipc/template";
import * as A from "./actions";
import { useOverlays } from "./overlays";
import { activeTab, selection, useTabs } from "./tabs";
import { toast } from "./ui";

export interface TabTemplate extends Applied {
  source: TplSource;
}

interface TplStore {
  byTab: Record<number, TabTemplate[]>;
  /** Template whose tree the panel shows. */
  current: Record<number, number>;
  set: (tab: number, list: TabTemplate[], current?: number) => void;
}

export const useTpl = create<TplStore>((set) => ({
  byTab: {},
  current: {},
  set: (tab, list, current) =>
    set((s) => ({
      byTab: { ...s.byTab, [tab]: list },
      current: { ...s.current, [tab]: current ?? s.current[tab] ?? list[0]?.key ?? 0 },
    })),
}));

/** Draws every applied template's fields as the `structure` layer. */
function paint(tab: number) {
  const list = useTpl.getState().byTab[tab] ?? [];
  const regions = list.flatMap((t) =>
    t.highlights.map((h) => ({ start: h.start, end: h.end, color: `${h.color}40`, label: t.name })),
  );
  useOverlays.getState().set(tab, "structure", regions);
}

export async function applyTemplate(
  tab: number,
  source: TplSource,
  args: { offset?: number; start?: number; end?: number; rootType?: string } = {},
) {
  try {
    const r = await tpl.apply(tab, source, args);
    const list = [...(useTpl.getState().byTab[tab] ?? []), { ...r, source }];
    useTpl.getState().set(tab, list, r.key);
    paint(tab);
    if (r.errors.some((e) => !e.warning)) toast(`${r.name}: ${r.errors[0].message}`, true);
    return r;
  } catch (e) {
    A.report(e);
    return null;
  }
}

export async function removeTemplate(tab: number, key: number) {
  await tpl.remove(key).catch(() => {});
  const list = (useTpl.getState().byTab[tab] ?? []).filter((t) => t.key !== key);
  useTpl.getState().set(tab, list, list[0]?.key ?? 0);
  paint(tab);
}

/** Re-evaluates every template of a tab after an edit (STRC-11). */
export async function reapplyAll(tab: number) {
  const list = useTpl.getState().byTab[tab] ?? [];
  if (!list.length) return;
  const next = await Promise.all(
    list.map(async (t) => {
      try {
        return { ...(await tpl.reapply(t.key)), source: t.source };
      } catch {
        return t;
      }
    }),
  );
  useTpl.getState().set(tab, next);
  paint(tab);
}

/** Detects the format and applies the best library template (STRC-09). */
const autoRunning = new Set<number>();

export async function autoApply(tab: number) {
  if ((useTpl.getState().byTab[tab] ?? []).length || autoRunning.has(tab)) return;
  autoRunning.add(tab);
  try {
    const ids = await tpl.detect(tab);
    if (ids[0]) await applyTemplate(tab, { kind: "library", id: ids[0] });
  } catch {
    // Detection is best effort.
  } finally {
    autoRunning.delete(tab);
  }
}

const shrinkStack: Record<number, { start: number; end: number }[]> = {};

/** Grows the selection to the enclosing field, then struct, … (NAV-12). */
export async function expandSelection() {
  const t = activeTab();
  if (!t) return;
  const key = useTpl.getState().current[t.id];
  if (!key) return toast("Сначала примените шаблон структуры");
  const sel = selection(t);
  const cur = { start: sel.start, end: Math.max(sel.end, sel.start + 1) };
  const ranges = await tpl.rangesAt(key, cur.start);
  const next = ranges.find(
    ([s, e]) => s <= cur.start && e >= cur.end && (s < cur.start || e > cur.end),
  );
  if (!next) return;
  shrinkStack[t.id] = [...(shrinkStack[t.id] ?? []), cur];
  A.selectRange(next[0], next[1]);
}

export function shrinkSelection() {
  const t = activeTab();
  if (!t) return;
  const prev = shrinkStack[t.id]?.pop();
  if (prev) A.selectRange(prev.start, prev.end);
}

// Forget results of closed tabs.
useTabs.subscribe((s, prev) => {
  if (s.tabs.length >= prev.tabs.length) return;
  const open = new Set(s.tabs.map((t) => t.id));
  for (const id of Object.keys(useTpl.getState().byTab).map(Number))
    if (!open.has(id))
      for (const t of useTpl.getState().byTab[id] ?? []) void tpl.remove(t.key).catch(() => {});
});
