// Typed wrappers around the scripting commands of `src-tauri` (SCR-01…05, SCR-09, SRCH-08).
import { Channel, invoke } from "@tauri-apps/api/core";
import type { DocInfo, EditResult, SearchEvent } from "./api";

export type LogLevel = "debug" | "info" | "warn" | "error";

export type ScriptError =
  | { kind: "syntax"; message: string; line: number | null; incomplete: boolean }
  | { kind: "runtime"; message: string; line: number | null }
  | { kind: "cancelled" }
  | { kind: "budgetExceeded"; budget: number }
  | { kind: "timeout"; millis: number }
  | { kind: "memoryLimit"; limit: number }
  | { kind: "invalidInput"; message: string }
  | { kind: "internal"; message: string };

export type ScriptEvent =
  | { type: "print"; text: string }
  | { type: "log"; level: LogLevel; message: string }
  | {
      type: "done";
      values: string[];
      error: ScriptError | null;
      incomplete: boolean;
      millis: number;
    };

export interface Span {
  start: number;
  end: number;
}

/** UI-owned state handed to every run. */
export interface UiContext {
  active: number | null;
  docs: { id: number; cursor: number; selection: Span | null }[];
}

/** Requests from a running script to the window (`hexa://script-ui`). */
export type UiEvent = { window: string | null } & (
  | { type: "docChanged"; doc: number }
  | { type: "cursor"; doc: number; offset: number; scroll: boolean }
  | { type: "select"; doc: number; selection: Span | null }
  | { type: "command"; id: string; args: unknown }
  | { type: "adopt"; info: DocInfo }
  | { type: "prompt"; req: number; title: string; message: string; default: string }
);

export const UI_EVENT = "hexa://script-ui";

export interface MacroStep {
  command: string;
  args: unknown;
}

export interface Macro {
  name: string;
  description?: string;
  steps: MacroStep[];
}

export type PluginKind = "transform" | "inspector" | "formatter" | "provider";

export interface PluginManifest {
  id: string;
  name: string;
  version: string;
  description: string;
  kinds: PluginKind[];
  module: string;
}

export interface PluginListing {
  dir: string;
  plugins: PluginManifest[];
  failures: { path: string; error: string }[];
}

export type PluginOutput =
  | { kind: "bytes"; len: number; preview: number[]; logs: string[] }
  | { kind: "rows"; rows: { label: string; value: string }[]; logs: string[] }
  | { kind: "text"; text: string; logs: string[] }
  | { kind: "doc"; info: DocInfo; logs: string[] }
  | { kind: "edit"; result: EditResult; logs: string[] };

export interface CatalogEntry {
  id: string;
  name: string;
  version: string;
  description: string;
  url: string;
  sha256: string;
  kinds?: PluginKind[];
}

export interface Catalog {
  schemaVersion: number;
  plugins: CatalogEntry[];
  templates: CatalogEntry[];
}

export interface ApiStatus {
  running: boolean;
  port: number;
  token: string;
  url: string;
  error: string | null;
}

export interface PredicateOpts {
  window?: number;
  align?: number;
  start?: number;
  end?: number;
  max?: number;
}

function channel<T>(onEvent: (e: T) => void): Channel<T> {
  const ch = new Channel<T>();
  ch.onmessage = onEvent;
  return ch;
}

export const scriptApi = {
  run: (source: string, name: string, ctx: UiContext, onEvent: (e: ScriptEvent) => void) =>
    invoke<number>("script_run", { source, name, ctx, channel: channel(onEvent) }),
  replEval: (line: string, ctx: UiContext, onEvent: (e: ScriptEvent) => void) =>
    invoke<number>("script_repl_eval", { line, ctx, channel: channel(onEvent) }),
  complete: (input: string) => invoke<string[]>("script_complete", { input }),
  replReset: () => invoke<void>("script_repl_reset"),
  cancel: (job: number | null = null) => invoke<void>("script_cancel", { job }),
  promptReply: (req: number, value: string | null) =>
    invoke<void>("script_prompt_reply", { req, value }),
  setContext: (ctx: UiContext) => invoke<void>("script_set_context", { ctx }),
  macroSave: (path: string, mac: Macro) => invoke<void>("macro_save", { path, mac }),
  macroLoad: (path: string) => invoke<Macro>("macro_load", { path }),
  macroToLua: (mac: Macro) => invoke<string>("macro_to_lua", { mac }),
  pluginsList: () => invoke<PluginListing>("plugins_list"),
  pluginsReload: () => invoke<PluginListing>("plugins_reload"),
  pluginsOpenDir: () => invoke<string>("plugins_open_dir"),
  pluginsRun: (args: {
    plugin: string;
    kind: PluginKind;
    id: number | null;
    start?: number;
    end?: number;
    request?: string;
    target?: "preview" | "newDoc" | "replace";
  }) => invoke<PluginOutput>("plugins_run", args),
  catalogRead: (path: string) => invoke<Catalog>("catalog_read", { path }),
  catalogInstall: (path: string, id: string) =>
    invoke<PluginListing>("catalog_install", { path, id }),
  apiStart: (port: number | null = null) => invoke<ApiStatus>("api_server_start", { port }),
  apiStop: () => invoke<ApiStatus>("api_server_stop"),
  apiSetPort: (port: number) => invoke<ApiStatus>("api_server_set_port", { port }),
  apiStatus: () => invoke<ApiStatus>("api_server_status"),
  apiRegenerate: () => invoke<ApiStatus>("api_server_regenerate_token"),
  predicateSearch: (
    id: number,
    source: string,
    opts: PredicateOpts,
    onEvent: (e: SearchEvent) => void,
  ) => invoke<number>("predicate_search", { id, source, opts, channel: channel(onEvent) }),
};

/** Human-readable text of a script failure. */
export function scriptErrorText(e: ScriptError): string {
  const at = (line: number | null) => (line ? ` (строка ${line})` : "");
  switch (e.kind) {
    case "syntax":
      return `Синтаксическая ошибка${at(e.line)}: ${e.message}`;
    case "runtime":
      return `Ошибка${at(e.line)}: ${e.message}`;
    case "cancelled":
      return "Скрипт остановлен";
    case "budgetExceeded":
      return `Превышен лимит инструкций (${e.budget})`;
    case "timeout":
      return `Превышено время выполнения (${Math.round(e.millis / 1000)} с)`;
    case "memoryLimit":
      return `Превышен лимит памяти (${Math.round(e.limit / 1048576)} МиБ)`;
    case "invalidInput":
    case "internal":
      return e.message;
  }
}
