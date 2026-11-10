// Structure template commands (STRC-01…23, SRCH-11, DIFF-10, NAV-12).
import { invoke } from "@tauri-apps/api/core";
import type { EditResult } from "./api";

export type TplSource =
  | { kind: "library"; id: string }
  | { kind: "text"; text: string; dialect?: string | null }
  | { kind: "file"; path: string };

export interface TplNode {
  id: number;
  parent: number | null;
  name: string;
  displayName: string | null;
  typeName: string;
  kind: string;
  offset: number;
  size: number;
  bitOffset: number | null;
  bitSize: number | null;
  value: string | null;
  children: number[];
  comment: string | null;
  error: string | null;
  enumName: string | null;
  isArray: boolean;
  len: number;
  elemIndex: number | null;
  hidden: boolean;
  lazy: boolean;
  childCount: number;
  color: { palette: number } | { rgb: string } | null;
}

export interface EvalMessage {
  message: string;
  line: number;
  col: number;
  offset: number;
  node: number | null;
  warning: boolean;
}

export interface Applied {
  key: number;
  name: string;
  roots: TplNode[];
  errors: EvalMessage[];
  log: string[];
  truncated: boolean;
  nodeCount: number;
  highlights: { start: number; end: number; color: string; node: number }[];
}

export interface Diagnostic {
  line: number;
  col: number;
  endLine: number;
  endCol: number;
  severity: "error" | "warning" | "info" | "hint";
  message: string;
}

export interface FieldChange {
  path: string;
  kind: "changed" | "added" | "removed";
  old: string | null;
  new: string | null;
  oldOffset: number | null;
  newOffset: number | null;
}

export const tpl = {
  library: () =>
    invoke<{ id: string; name: string; description: string; extensions: string[] }[]>(
      "tpl_library",
    ),
  detect: (id: number) => invoke<string[]>("tpl_detect", { id }),
  apply: (
    id: number,
    source: TplSource,
    args: { offset?: number; start?: number; end?: number; rootType?: string } = {},
  ) => invoke<Applied>("tpl_apply", { id, source, args }),
  reapply: (key: number) => invoke<Applied>("tpl_reapply", { key }),
  remove: (key: number) => invoke<void>("tpl_remove", { key }),
  children: (key: number, node: number, start: number, end: number) =>
    invoke<TplNode[]>("tpl_children", { key, node, start, end }),
  expand: (key: number, array: number, index: number) =>
    invoke<{ key: number; nodes: TplNode[] }>("tpl_expand", { key, array, index }),
  nodeAt: (key: number, offset: number) =>
    invoke<{ path: number[]; elementIndex: number | null } | null>("tpl_node_at", { key, offset }),
  rangesAt: (key: number, offset: number) =>
    invoke<[number, number][]>("tpl_ranges_at", { key, offset }),
  setValue: (key: number, node: number, elem: number | null, text: string) =>
    invoke<EditResult>("tpl_set_value", { key, node, elem, text }),
  filter: (key: number, query: string) => invoke<number[]>("tpl_filter", { key, query }),
  export: (key: number, format: "json" | "csv" | "xml", path: string) =>
    invoke<void>("tpl_export", { key, format, path }),
  diff: (a: number, b: number) => invoke<FieldChange[]>("tpl_diff", { a, b }),
  scan: (id: number, source: TplSource, typeName: string, start: number, end: number, step = 1) =>
    invoke<number[]>("tpl_scan", { id, source, typeName, start, end, step }),
  diagnostics: (text: string, dialect: string | null) =>
    invoke<Diagnostic[]>("tpl_diagnostics", { text, dialect }),
  librarySource: (id: string) => invoke<string>("tpl_library_source", { id }),
  suggest: (id: number, offset: number) =>
    invoke<{ ty: string; value: string; confidence: number; reason: string }[]>("tpl_suggest", {
      id,
      offset,
    }),
  generate: (source: TplSource, target: string, root: string | null) =>
    invoke<string>("tpl_generate", { source, target, root }),
};
