// User settings (UI-11), persisted as JSON by the Rust side.
import { create } from "zustand";
import { api, type EditMode, type Endian, type SaveOptions, type ShareMode } from "../ipc/api";

export type Theme = "system" | "light" | "dark" | "contrast";
export type Density = "compact" | "regular" | "comfortable";
export type CellView = "hex" | "dec" | "oct" | "bin" | "signed" | "hexii" | "float" | "char";
export type Column = "offset" | "hex" | "text";
export type NonPrintable = "dot" | "pictures" | "blank";
export type PanelId = "history" | "hashes" | "results" | "inspector";
export type KeymapPreset = "hexa" | "hxd" | "010" | "vim" | "emacs";

export interface Layout {
  left: PanelId[];
  right: PanelId[];
  leftOpen: boolean;
  rightOpen: boolean;
  leftWidth: number;
  rightWidth: number;
  leftActive: PanelId | null;
  rightActive: PanelId | null;
}

export interface Settings {
  theme: Theme;
  accent: string;
  density: Density;
  fontSize: number;
  lineSpacing: number;
  fontFamily: string;
  reduceMotion: boolean;
  colorBlind: boolean;
  themeOverrides: Record<string, string>;
  /** Fixed count, or "auto" to fit the window, or "pow2" to fit with powers of two. */
  bytesPerRow: number | "auto" | "pow2";
  group: 1 | 2 | 4 | 8 | 16;
  cellView: CellView;
  /** Bytes per cell for dec/oct/signed views (DISP-05). */
  cellWidth: 1 | 2 | 4 | 8;
  cellEndian: Endian;
  lowercase: boolean;
  offsetBase: "hex" | "dec" | "oct";
  baseAddress: number;
  offsetRelative: boolean;
  columns: Column[];
  showColHeader: boolean;
  encoding: string;
  nonPrintable: NonPrintable;
  colorBytes: boolean;
  highlightChanges: boolean;
  highlightRow: boolean;
  highlightColumn: boolean;
  zebra: boolean;
  highlightSame: boolean;
  defaultMode: EditMode;
  save: SaveOptions;
  share: ShareMode;
  loadInMemoryMb: number;
  keymap: KeymapPreset;
  keyOverrides: Record<string, string[]>;
  inspectorEndian: Endian;
  layout: Layout;
  layouts: Record<string, Layout>;
  hashAlgos: string[];
  searchEncodings: string[];
  confirmClose: boolean;
  zoom: number;
}

export const DEFAULT_LAYOUT: Layout = {
  left: ["history", "hashes", "results"],
  right: ["inspector"],
  leftOpen: true,
  rightOpen: true,
  leftWidth: 272,
  rightWidth: 292,
  leftActive: "history",
  rightActive: "inspector",
};

export const DEFAULTS: Settings = {
  theme: "system",
  accent: "#2F6BF0",
  density: "regular",
  fontSize: 12.5,
  lineSpacing: 1,
  fontFamily: "",
  reduceMotion: false,
  colorBlind: false,
  themeOverrides: {},
  bytesPerRow: 16,
  group: 8,
  cellView: "hex",
  cellWidth: 1,
  cellEndian: "little",
  lowercase: false,
  offsetBase: "hex",
  baseAddress: 0,
  offsetRelative: false,
  columns: ["offset", "hex", "text"],
  showColHeader: true,
  encoding: "ascii",
  nonPrintable: "dot",
  colorBytes: true,
  highlightChanges: true,
  highlightRow: true,
  highlightColumn: true,
  zebra: false,
  highlightSame: false,
  defaultMode: "overwrite",
  save: { strategy: "auto", backup: false, journal: true, force: false },
  share: "shared",
  loadInMemoryMb: 0,
  keymap: "hexa",
  keyOverrides: {},
  inspectorEndian: "little",
  layout: DEFAULT_LAYOUT,
  layouts: {},
  hashAlgos: ["crc:CRC-32/ISO-HDLC", "md5", "sha1", "sha256"],
  searchEncodings: ["ascii"],
  confirmClose: true,
  zoom: 1,
};

interface SettingsStore {
  s: Settings;
  loaded: boolean;
  load: () => Promise<void>;
  set: (patch: Partial<Settings>) => void;
  setLayout: (patch: Partial<Layout>) => void;
  reset: () => void;
}

let saveTimer: ReturnType<typeof setTimeout> | undefined;
function persist(s: Settings) {
  clearTimeout(saveTimer);
  saveTimer = setTimeout(() => {
    api.settingsSet(s).catch(() => {});
  }, 300);
}

export const useSettings = create<SettingsStore>((set, get) => ({
  s: DEFAULTS,
  loaded: false,
  load: async () => {
    try {
      const raw = (await api.settingsGet()) as Partial<Settings>;
      const s = {
        ...DEFAULTS,
        ...raw,
        save: { ...DEFAULTS.save, ...(raw.save ?? {}) },
        layout: { ...DEFAULT_LAYOUT, ...(raw.layout ?? {}) },
      };
      set({ s, loaded: true });
    } catch {
      set({ loaded: true });
    }
  },
  set: (patch) => {
    const s = { ...get().s, ...patch };
    set({ s });
    persist(s);
  },
  setLayout: (patch) => {
    const s = { ...get().s, layout: { ...get().s.layout, ...patch } };
    set({ s });
    persist(s);
  },
  reset: () => {
    set({ s: DEFAULTS });
    persist(DEFAULTS);
  },
}));

export const settings = () => useSettings.getState().s;

/** Row height in px for the density (rowh from the mockup), scaled by spacing. */
export function rowHeight(s: Settings): number {
  const base = s.density === "compact" ? 19 : s.density === "comfortable" ? 26 : 22;
  return Math.round(base * s.lineSpacing * (s.fontSize / 12.5) * s.zoom);
}

export function fontSize(s: Settings): number {
  const d = s.density === "compact" ? -0.5 : s.density === "comfortable" ? 1 : 0;
  return (s.fontSize + d) * s.zoom;
}
