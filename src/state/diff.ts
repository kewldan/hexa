// Comparison of two open documents (DIFF-01…11): runs the Rust diff in the
// background, mirrors regions into the grid overlay of both tabs and keeps
// the current region for navigation and merging.
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { create } from "zustand";
import {
  api,
  type DiffOptions,
  type DiffRegion,
  type DiffResult,
  type RegionKind,
} from "../ipc/api";
import * as A from "./actions";
import { useOverlays } from "./overlays";
import { settings, useSettings } from "./settings";
import { useTabs } from "./tabs";
import { toast, useUi } from "./ui";

export const DIFF_COLORS: Record<RegionKind, string> = {
  changed: "#E8890C",
  inserted: "#1F9D74",
  deleted: "#DC2626",
  equal: "#6B6B73",
};

export const KIND_LABEL: Record<RegionKind, string> = {
  changed: "изменено",
  inserted: "вставлено",
  deleted: "удалено",
  equal: "совпадает",
};

/** Regions drawn in the grid; the list and statistics keep all of them. */
const MAX_OVERLAY = 200_000;

interface DiffState {
  a: number | null;
  b: number | null;
  result: DiffResult | null;
  running: boolean;
  progress: number;
  job: number | null;
  current: number;
  opts: DiffOptions;
  set: (p: Partial<DiffState>) => void;
}

export const useDiff = create<DiffState>((set) => ({
  a: null,
  b: null,
  result: null,
  running: false,
  progress: 0,
  job: null,
  current: -1,
  opts: { mode: "shiftAware", mergeGap: 0 },
  set: (p) => set(p),
}));

function paint(a: number, b: number, regions: DiffRegion[]) {
  const o = useOverlays.getState();
  const shown = regions.slice(0, MAX_OVERLAY);
  const side = (pick: (r: DiffRegion) => { start: number; len: number }) =>
    shown
      .filter((r) => pick(r).len > 0)
      .map((r) => ({
        start: pick(r).start,
        end: pick(r).start + pick(r).len,
        color: DIFF_COLORS[r.kind],
        label: `Отличие: ${KIND_LABEL[r.kind]}`,
      }));
  o.set(
    a,
    "diff",
    side((r) => r.a),
  );
  o.set(
    b,
    "diff",
    side((r) => r.b),
  );
}

/** Compares tab `a` with tab `b` and shows them side by side. */
export async function startDiff(a: number, b: number) {
  const d = useDiff.getState();
  if (d.job !== null) void api.cancelJob(d.job).catch(() => {});
  if (d.a !== null) useOverlays.getState().clear(d.a, "diff");
  if (d.b !== null) useOverlays.getState().clear(d.b, "diff");
  useUi.getState().set({ compare: { a, b, sync: true } });
  const s = settings();
  if (!s.layers.diff) useSettings.getState().set({ layers: { ...s.layers, diff: true } });
  d.set({ a, b, result: null, running: true, progress: 0, current: -1, job: null });
  try {
    const job = await api.diffRun(a, b, useDiff.getState().opts, {}, (e) => {
      if (e.type === "progress") {
        useDiff.getState().set({ progress: e.total ? e.done / e.total : 0 });
      } else if (e.type === "done") {
        useDiff.getState().set({ result: e.result, running: false, job: null });
        paint(a, b, e.result.regions);
        if (e.result.regions.length === 0) toast("Файлы совпадают");
      } else {
        useDiff.getState().set({ running: false, job: null });
        A.report(e.error);
      }
    });
    if (useDiff.getState().running) useDiff.getState().set({ job });
  } catch (e) {
    useDiff.getState().set({ running: false });
    A.report(e);
  }
}

export function stopDiff() {
  const d = useDiff.getState();
  if (d.job !== null) void api.cancelJob(d.job).catch(() => {});
  if (d.a !== null) useOverlays.getState().clear(d.a, "diff");
  if (d.b !== null) useOverlays.getState().clear(d.b, "diff");
  d.set({ a: null, b: null, result: null, running: false, job: null, current: -1 });
}

/** Re-runs the comparison after an edit or an option change. */
export function rerunDiff() {
  const { a, b } = useDiff.getState();
  if (a !== null && b !== null) void startDiff(a, b);
}

export function gotoRegion(i: number) {
  const { a, b, result } = useDiff.getState();
  if (a === null || b === null || !result || !result.regions.length) return;
  const n = result.regions.length;
  const idx = ((i % n) + n) % n;
  const r = result.regions[idx];
  useDiff.getState().set({ current: idx });
  const tabs = useTabs.getState();
  const focus = (id: number, s: { start: number; len: number }) => {
    const end = s.len > 0 ? s.start + s.len - 1 : s.start;
    tabs.update(id, { anchor: s.start, cursor: end, nibble: 0 });
    A.scrollIntoView(id, s.start, true);
  };
  focus(a, r.a);
  focus(b, r.b);
}

export function nextRegion(dir: 1 | -1) {
  const { result, current, a } = useDiff.getState();
  if (!result?.regions.length) return toast(result ? "Отличий нет" : "Сравнение не запущено");
  if (current < 0) {
    // Start from the cursor of the left document.
    const cur = useTabs.getState().tabs.find((t) => t.id === a)?.cursor ?? 0;
    const i = result.regions.findIndex((r) => r.a.start + r.a.len > cur);
    return gotoRegion(dir > 0 ? Math.max(0, i) : (i < 0 ? result.regions.length : i) - 1);
  }
  gotoRegion(current + dir);
}

/** Copies the current region from one side to the other (DIFF-09). */
export async function applyRegion(toLeft: boolean, all = false) {
  const { a, b, result, current } = useDiff.getState();
  if (a === null || b === null || !result) return;
  const regions = all ? result.regions : current >= 0 ? [result.regions[current]] : [];
  if (!regions.length) return toast("Выберите отличие");
  try {
    const swapped = regions.map((r) => ({ a: r.b, b: r.a, kind: r.kind }));
    const [to, from, list] = toLeft ? [a, b, regions] : [b, a, swapped];
    const r = await api.diffApply(to, from, list);
    A.applyResult(to, r);
    rerunDiff();
  } catch (e) {
    A.report(e);
  }
}

export async function exportDiff() {
  const { result, a } = useDiff.getState();
  if (!result) return;
  const name = useTabs.getState().tabs.find((t) => t.id === a)?.info.name ?? "diff";
  const path = await saveDialog({
    defaultPath: `${name}.diff.txt`,
    filters: [
      { name: "Текст", extensions: ["txt"] },
      { name: "CSV", extensions: ["csv"] },
      { name: "JSON", extensions: ["json"] },
    ],
  });
  if (!path) return;
  const ext = path.split(".").pop()?.toLowerCase();
  const format = ext === "csv" ? "csv" : ext === "json" ? "json" : "text";
  try {
    await api.diffExport(result, format, path);
    toast("Отчёт сохранён");
  } catch (e) {
    A.report(e);
  }
}
