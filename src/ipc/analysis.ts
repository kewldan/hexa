// Analysis commands (ANL-03…17, IO-01…04, SRCH-10).
import { invoke } from "@tauri-apps/api/core";
import type { DocInfo } from "./api";

export interface Histogram {
  counts: number[];
  total: number;
  distinct: number;
  entropy: number;
  mean: number;
  chiSquare: number;
  mostCommon: number | null;
  leastCommon: number | null;
}
export interface EntropySeries {
  start: number;
  window: number;
  step: number;
  values: number[];
}
export type RegionKind = "zero" | "padding" | "text" | "code" | "compressed" | "encrypted" | "data";
export interface AnlRegion {
  start: number;
  end: number;
  kind: RegionKind;
  detail: string | null;
  confidence: number;
  entropy: number;
}
export interface Overview {
  histogram: Histogram;
  entropy: EntropySeries;
  regions: AnlRegion[];
}
export interface Digraph {
  size: number;
  cell: number;
  counts: number[];
  max: number;
  pairs: number;
}
export interface HilbertMap {
  order: number;
  side: number;
  start: number;
  bytesPerCell: number;
  cells: number[];
}
export type StringEncoding = "ascii" | "utf8" | "utf16Le" | "utf16Be";
export interface StringsOptions {
  encodings?: StringEncoding[];
  minLen?: number;
  maxResults?: number;
  contains?: string | null;
  regex?: string | null;
}
export interface FoundString {
  offset: number;
  len: number;
  encoding: StringEncoding;
  text: string;
}
export interface Embedded {
  offset: number;
  lenGuess: number | null;
  kind: string;
  description: string;
}
export type NumType = "u8" | "i8" | "u16" | "i16" | "u32" | "i32" | "u64" | "i64" | "f32" | "f64";
export interface PlotSeries {
  count: number;
  buckets: { first: number; count: number; min: number | null; max: number | null }[];
  min: number | null;
  max: number | null;
  mean: number | null;
  nonFinite: number;
}
export type PixelFormat =
  | "rgb565"
  | "rgb555"
  | "rgb888"
  | "bgr888"
  | "rgba8888"
  | "bgra8888"
  | "argb8888"
  | "gray1"
  | "gray2"
  | "gray4"
  | "gray8"
  | "indexed8";
export interface BitmapOptions {
  offset: number;
  width: number;
  height: number | null;
  stride: number | null;
  format: PixelFormat;
  bigEndian: boolean;
  lsbFirst: boolean;
  flipVertical: boolean;
  palette: {
    offset: number;
    format: "rgb888" | "bgr888" | "rgba8888" | "bgra8888";
    entries: number;
    ignoreAlpha: boolean;
  } | null;
}
export interface Rgba {
  width: number;
  height: number;
  rgba: Uint8ClampedArray;
}
export type FirmwareFormat = "intelHex" | "sRecord" | "tiTxt" | "tekHex";
export type DumpFormat =
  | { kind: "xxd" }
  | { kind: "hexdumpC"; squeeze: boolean }
  | { kind: "html" }
  | { kind: "rtf" }
  | { kind: "markdown"; width: number }
  | { kind: "csv"; width: number }
  | { kind: "cHeader"; name: string };
export interface Imported {
  info: DocInfo;
  base: number;
  segments: number;
  errors: { line: number; message: string }[];
  warnings: { line: number; message: string }[];
  format: string;
}

function toRgba(buf: ArrayBuffer): Rgba {
  const v = new DataView(buf);
  return {
    width: v.getUint32(0, true),
    height: v.getUint32(4, true),
    rgba: new Uint8ClampedArray(buf, 8),
  };
}

export interface YaraReport {
  matches: {
    rule: string;
    namespace: string;
    tags: string[];
    strings: { identifier: string; offset: number; len: number }[];
  }[];
  warnings: string[];
}

export const anl = {
  yara: (id: number, rules: string, start: number, end: number) =>
    invoke<YaraReport>("yara_scan", { id, rules, start, end }),
  overview: (id: number, start: number, end: number, points = 512) =>
    invoke<Overview>("anl_overview", { id, start, end, points }),
  digraph: (id: number, start: number, end: number, size = 256) =>
    invoke<Digraph>("anl_digraph", { id, start, end, size }),
  hilbert: (id: number, start: number, end: number, order = 8) =>
    invoke<HilbertMap>("anl_hilbert", { id, start, end, order }),
  strings: (id: number, start: number, end: number, opts: StringsOptions) =>
    invoke<{ strings: FoundString[]; truncated: boolean }>("anl_strings", { id, start, end, opts }),
  embedded: (id: number, start: number, end: number) =>
    invoke<Embedded[]>("anl_embedded", { id, start, end }),
  plot: (
    id: number,
    start: number,
    end: number,
    opts: { numType: NumType; bigEndian: boolean; buckets: number; stride: number | null },
  ) => invoke<PlotSeries>("anl_plot", { id, start, end, opts }),
  bitmap: async (id: number, opts: BitmapOptions) =>
    toRgba(await invoke<ArrayBuffer>("anl_bitmap", { id, opts })),
  image: async (id: number, start: number, end: number) =>
    toRgba(await invoke<ArrayBuffer>("anl_image", { id, start, end })),
  fuzzyCompare: (a: number, b: number) =>
    invoke<{
      ssdeep: [string, string];
      tlsh: [string | null, string | null];
      score: number | null;
      distance: number | null;
    }>("anl_fuzzy_compare", { a, b }),
  fwImport: (path: string, fill = 0xff) =>
    invoke<Imported>("fw_import", { path, format: null, fill }),
  fwExport: (
    id: number,
    start: number,
    end: number,
    base: number,
    format: FirmwareFormat,
    path: string,
  ) => invoke<void>("fw_export", { id, start, end, base, format, opts: null, path }),
  dumpExport: (id: number, start: number, end: number, format: DumpFormat, path: string) =>
    invoke<void>("dump_export", { id, start, end, format, path }),
  dumpImport: (path: string) => invoke<Imported>("dump_import", { path, format: null }),
};
