// Clipboard history (EDIT-18): the last copies made in this session.
import { create } from "zustand";

export interface ClipEntry {
  id: number;
  text: string;
  len: number;
  source: string;
  time: number;
}

const MAX = 20;
/** Copies larger than this keep only a preview and cannot be pasted back. */
const MAX_TEXT = 4 << 20;
let seq = 0;

interface ClipHist {
  entries: ClipEntry[];
  push: (text: string | null, len: number, source: string) => void;
  remove: (id: number) => void;
  clear: () => void;
}

export const useClipHist = create<ClipHist>((set) => ({
  entries: [],
  push: (text, len, source) => {
    if (text === null || text.length > MAX_TEXT) return;
    set((s) => ({
      entries: [
        { id: ++seq, text, len, source, time: Date.now() },
        ...s.entries.filter((e) => e.text !== text),
      ].slice(0, MAX),
    }));
  },
  remove: (id) => set((s) => ({ entries: s.entries.filter((e) => e.id !== id) })),
  clear: () => set({ entries: [] }),
}));
