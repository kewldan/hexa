// User settings (UI-11), persisted as JSON by the Rust side.
import { create } from "zustand";
import { api, type EditMode, type Endian, type SaveOptions, type ShareMode } from "../ipc/api";

export type Theme = "system" | "light" | "dark" | "contrast";
export type Density = "compact" | "regular" | "comfortable";
export type CellView = "hex" | "dec" | "oct" | "bin" | "signed" | "hexii" | "float" | "char";
export type Column = "offset" | "hex" | "text";
export type NonPrintable = "dot" | "pictures" | "blank";
export type PanelId =
  | "history"
  | "diff"
  | "hashes"
  | "results"
  | "inspector"
  | "marks"
  | "structure"
  | "analysis"
  | "transform"
  | "disasm"
  | "strings"
  | "console"
  | "disk"
  | "memory";
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
  offsetBase: "hex" | "dec" | "oct" | "seg";
  /** Segment size for the segment:offset form. */
  offsetSegment: number;
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
  /** Interface scale for everything but the grid (UI-18). */
  uiScale: number;
  /** Hidden inspector rows by key (INSP-17). */
  inspectorHidden: string[];
  /** Integer and fraction bits for the Q-format row (INSP-04). */
  qFormat: [number, number];
  /** Inspector tooltip when hovering a byte (INSP-18). */
  hoverInspector: boolean;
  /** User colour maps by byte value (DISP-14). */
  colorRules: { from: number; to: number; color: string }[];
  /** Visibility of highlight layers (DISP-16). */
  layers: { changes: boolean; search: boolean; marks: boolean; structure: boolean; diff: boolean };
  /** Loaded .tbl character tables by name (DISP-12). */
  tblTables: Record<string, string>;
  savedQueries: { label: string; kind: string; text: string }[];
  /** Saved transformation recipes (XFRM-10). */
  recipes: import("../ipc/api").Recipe[];
  /** Architecture of the inspector's instruction row (INSP-14); empty hides it. */
  inspectorArch: string;
  /** Colour-vision-safe highlight colours (UI-18). */
  colorSafe: boolean;
  queryHistory: string[];
  /** Named settings snapshots (UI-11). */
  profiles: Record<string, Partial<Settings>>;
  /** Opt-in update check (UI-15, NFR-11). */
  checkUpdates: boolean;
  /** Announce cursor moves and values for screen readers (UI-17). */
  screenReader: boolean;
  locale: "ru" | "en";
  /** Structure template auto-apply by signature (STRC-09). */
  autoTemplate: boolean;
  /** Local HTTP API for external tools (SCR-09). */
  apiEnabled: boolean;
}

export const DEFAULT_LAYOUT: Layout = {
  left: ["structure", "marks", "history", "hashes", "results", "diff"],
  right: ["inspector", "analysis", "transform", "disasm", "strings", "console", "disk", "memory"],
  leftOpen: true,
  rightOpen: true,
  leftWidth: 272,
  rightWidth: 292,
  leftActive: "structure",
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
  offsetSegment: 0x10000,
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
  uiScale: 1,
  inspectorHidden: [],
  qFormat: [16, 16],
  hoverInspector: false,
  colorRules: [],
  layers: { changes: true, search: true, marks: true, structure: true, diff: true },
  tblTables: {},
  savedQueries: [],
  recipes: [],
  inspectorArch: "",
  colorSafe: false,
  queryHistory: [],
  profiles: {},
  checkUpdates: false,
  screenReader: true,
  locale: "ru",
  autoTemplate: true,
  apiEnabled: false,
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
        layout: migrateLayout({ ...DEFAULT_LAYOUT, ...(raw.layout ?? {}) }),
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

/** Adds panels introduced after the layout was saved to their default side. */
function migrateLayout(l: Layout): Layout {
  const all = new Set([...l.left, ...l.right]);
  const left = [...l.left];
  const right = [...l.right];
  for (const id of DEFAULT_LAYOUT.left) if (!all.has(id)) left.push(id);
  for (const id of DEFAULT_LAYOUT.right) if (!all.has(id)) right.push(id);
  return { ...l, left, right };
}

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
