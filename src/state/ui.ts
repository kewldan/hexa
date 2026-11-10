// Overlays, dialogs, toasts and progress.
import { create } from "zustand";

export type Dialog =
  | { kind: "newFile" }
  | { kind: "fill" }
  | { kind: "insertBytes" }
  | { kind: "resize" }
  | { kind: "selectRange" }
  | { kind: "copyAs" }
  | { kind: "paste"; text: string }
  | { kind: "settings"; section?: string }
  | { kind: "props" }
  | { kind: "followPointer" }
  | { kind: "moveBlock" }
  | { kind: "pickTab"; mode?: "side" | "diff" }
  | { kind: "merge3" }
  | { kind: "patchCreate" }
  | { kind: "patchApply"; path?: string }
  | { kind: "replacePreview"; plan: import("../ipc/api").Planned[] }
  | { kind: "mark"; mark: import("../ipc/api").Mark }
  | { kind: "confirm"; title: string; body: string; actions: ConfirmAction[] }
  | { kind: "recovery" }
  | { kind: "about" };

export interface ConfirmAction {
  label: string;
  primary?: boolean;
  danger?: boolean;
  run: () => void | Promise<void>;
}

export interface MenuItem {
  label?: string;
  keys?: string;
  header?: string;
  separator?: boolean;
  disabled?: boolean;
  run?: () => void;
  submenu?: MenuItem[];
}

export interface Toast {
  text: string;
  error?: boolean;
  seq: number;
}

interface UiStore {
  palette: boolean;
  paletteQuery: string;
  goto: boolean;
  find: boolean;
  findFocus: number;
  dialog: Dialog | null;
  menu: { x: number; y: number; items: MenuItem[] } | null;
  toast: Toast | null;
  progress: { label: string; done: number; total: number; job?: number } | null;
  dragOver: boolean;
  /** Two tabs shown side by side (DISP-21, DIFF-06). */
  compare: { a: number; b: number; sync: boolean } | null;
  set: (patch: Partial<Omit<UiStore, "set">>) => void;
}

export const useUi = create<UiStore>((set) => ({
  palette: false,
  paletteQuery: "",
  goto: false,
  find: false,
  findFocus: 0,
  dialog: null,
  menu: null,
  toast: null,
  progress: null,
  dragOver: false,
  compare: null,
  set: (patch) => set(patch),
}));

let toastSeq = 0;
let toastTimer: ReturnType<typeof setTimeout> | undefined;

export function toast(text: string, error = false) {
  clearTimeout(toastTimer);
  useUi.getState().set({ toast: { text, error, seq: ++toastSeq } });
  toastTimer = setTimeout(() => useUi.getState().set({ toast: null }), error ? 4200 : 2200);
}

export function openDialog(d: Dialog) {
  useUi.getState().set({ dialog: d, menu: null, palette: false, goto: false });
}

export function closeDialog() {
  useUi.getState().set({ dialog: null });
}

export function confirm(title: string, body: string, actions: ConfirmAction[]) {
  openDialog({ kind: "confirm", title, body, actions });
}
