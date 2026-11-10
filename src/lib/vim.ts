// Vim-style navigation and editing for the grid (UI-04), in the spirit of bvi.
import { create } from "zustand";
import * as A from "../state/actions";
import { activeTab, selection } from "../state/tabs";
import { useUi } from "../state/ui";

export type VimMode = "normal" | "insert" | "replace" | "visual";

interface VimStore {
  mode: VimMode;
  count: string;
  pending: string;
  set: (p: Partial<Omit<VimStore, "set">>) => void;
}

export const useVim = create<VimStore>((set) => ({
  mode: "normal",
  count: "",
  pending: "",
  set: (p) => set(p),
}));

const v = () => useVim.getState();

function count(): number {
  const n = Number.parseInt(v().count || "1", 10);
  v().set({ count: "" });
  return Number.isFinite(n) && n > 0 ? n : 1;
}

function enter(mode: VimMode) {
  v().set({ mode, count: "", pending: "" });
  if (mode === "insert") A.setMode("insert");
  if (mode === "replace") A.setMode("overwrite");
}

/**
 * Handles a key in vim mode. Returns true when the key was consumed.
 * Insert/replace modes fall through to normal typing except for Escape.
 */
export function vimKey(e: KeyboardEvent): boolean {
  const st = v();
  const t = activeTab();
  if (!t) return false;
  if (e.key === "Escape") {
    A.commitCellBuffer();
    if (st.mode === "visual") A.setCursor(t.cursor);
    enter("normal");
    return true;
  }
  if (st.mode === "insert" || st.mode === "replace") return false;
  if (e.metaKey || (e.altKey && e.key.length > 1)) return false;
  const visual = st.mode === "visual";
  const k = e.key;

  if (st.pending === "r") {
    if (/^[0-9a-f]$/i.test(k)) {
      const digits = `${st.count}${k}`;
      if (digits.length === 2) {
        v().set({ pending: "", count: "" });
        A.edit({ op: "overwrite", pos: t.cursor, bytes: [Number.parseInt(digits, 16)] }, t.cursor);
      } else {
        v().set({ count: digits });
      }
    } else {
      v().set({ pending: "", count: "" });
    }
    return true;
  }
  if (st.pending === "g") {
    v().set({ pending: "" });
    if (k === "g")
      A.setCursor(st.count ? Number.parseInt(st.count, 16) : 0, visual, { jump: true });
    v().set({ count: "" });
    return true;
  }
  if (st.pending === "d") {
    v().set({ pending: "" });
    if (k === "d") {
      A.selectRow();
      A.deleteBytes(false);
    }
    return true;
  }
  if (/^[0-9]$/.test(k) && !(k === "0" && st.count === "")) {
    v().set({ count: st.count + k });
    return true;
  }
  if (e.ctrlKey) {
    if (k === "f") A.moveRows(A.viewport.rows - 1, visual);
    else if (k === "b") A.moveRows(-(A.viewport.rows - 1), visual);
    else if (k === "d") A.moveRows(Math.floor(A.viewport.rows / 2), visual);
    else if (k === "u") A.moveRows(-Math.floor(A.viewport.rows / 2), visual);
    else if (k === "r") A.redo();
    else if (k === "o") A.navBack();
    else if (k === "i") A.navForward();
    else return false;
    return true;
  }
  const group = 4;
  switch (k) {
    case "h":
    case "ArrowLeft":
      A.moveBy(-count(), visual);
      return true;
    case "l":
    case "ArrowRight":
      A.moveBy(count(), visual);
      return true;
    case "j":
    case "ArrowDown":
      A.moveRows(count(), visual);
      return true;
    case "k":
    case "ArrowUp":
      A.moveRows(-count(), visual);
      return true;
    case "w":
      A.moveBy(group * count() - (t.cursor % group), visual);
      return true;
    case "b":
      A.moveBy(-(t.cursor % group || group) - group * (count() - 1), visual);
      return true;
    case "0":
    case "^":
      A.moveHome(visual);
      return true;
    case "$":
      A.moveEnd(visual);
      return true;
    case "G":
      if (st.count) A.setCursor(Number.parseInt(st.count, 16), visual, { jump: true });
      else A.moveEnd(visual, true);
      v().set({ count: "" });
      return true;
    case "g":
      v().set({ pending: "g" });
      return true;
    case "x":
      if (visual) {
        A.deleteBytes(false);
        enter("normal");
      } else {
        A.selectRange(t.cursor, t.cursor + count());
        A.deleteBytes(false);
      }
      return true;
    case "r":
      v().set({ pending: "r", count: "" });
      return true;
    case "d":
      if (visual) {
        A.deleteBytes(false);
        enter("normal");
      } else v().set({ pending: "d" });
      return true;
    case "y":
      A.copy();
      if (visual) {
        A.setCursor(selection(t).start);
        enter("normal");
      }
      return true;
    case "p":
      A.setCursor(t.cursor + 1);
      A.paste("insert");
      return true;
    case "P":
      A.paste("insert");
      return true;
    case "u":
      A.undo();
      return true;
    case "v":
      if (visual) {
        A.setCursor(t.cursor);
        enter("normal");
      } else v().set({ mode: "visual" });
      return true;
    case "i":
      enter("insert");
      return true;
    case "a":
      A.setCursor(t.cursor + 1);
      enter("insert");
      return true;
    case "R":
      enter("replace");
      return true;
    case "/":
      useUi.getState().set({ find: true, findFocus: useUi.getState().findFocus + 1 });
      return true;
    case "n":
      A.stepResult(1);
      return true;
    case "N":
      A.stepResult(-1);
      return true;
    case ":":
      useUi.getState().set({ goto: true });
      return true;
    case "Tab":
      A.switchColumn();
      return true;
    default:
      // Swallow printable keys so they do not edit in normal mode.
      return k.length === 1;
  }
}
