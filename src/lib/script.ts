// Lua console, scripts, macros, plugins and predicate search on the UI side
// (SCR-01…05, SCR-09, SRCH-08). The state lives outside components so the
// console survives panel switches.
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { create } from "zustand";
import { api, isCmdError, type Match, type Scope } from "../ipc/api";
import {
  type Macro,
  type PluginListing,
  type ScriptEvent,
  scriptApi,
  scriptErrorText,
  UI_EVENT,
  type UiContext,
  type UiEvent,
} from "../ipc/script";
import * as A from "../state/actions";
import { useSettings } from "../state/settings";
import { activeTab, hasSelection, selection, type Tab, useTabs } from "../state/tabs";
import { toast } from "../state/ui";
import { commandById, commandHooks, runCommand } from "./commands";

export type LineKind = "in" | "out" | "value" | "error" | "info" | "warn" | "debug";

export interface ConsoleLine {
  id: number;
  kind: LineKind;
  text: string;
}

export type ConsoleView = "repl" | "script" | "macro" | "plugins";

export interface PendingPrompt {
  req: number;
  title: string;
  message: string;
  default: string;
}

interface ConsoleStore {
  lines: ConsoleLine[];
  history: string[];
  running: boolean;
  job: number | null;
  /** The REPL waits for the rest of a statement. */
  pending: boolean;
  source: string;
  scriptName: string;
  view: ConsoleView;
  recording: Macro | null;
  lastMacro: Macro | null;
  prompt: PendingPrompt | null;
  plugins: PluginListing | null;
  /** Bytes handed to a Lua search predicate per offset (SRCH-08). */
  luaWindow: number;
  set: (patch: Partial<Omit<ConsoleStore, "set">>) => void;
}

const MAX_LINES = 2000;
const HISTORY_KEY = "hexa-console-history";
const SOURCE_KEY = "hexa-console-source";

function loadLocal<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key);
    return raw ? (JSON.parse(raw) as T) : fallback;
  } catch {
    return fallback;
  }
}

function saveLocal(key: string, value: unknown) {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    /* storage unavailable */
  }
}

const DEFAULT_SOURCE = `-- Пример: XOR выделения с 0x5A
local sel = hexa.selection()
if not sel then return "Выделите байты" end
local bytes = hexa.read(sel.offset, sel.len)
local out = bytes:gsub(".", function(c) return string.char(c:byte() ~ 0x5A) end)
hexa.write(sel.offset, out)
print(("Изменено %d байт"):format(sel.len))
`;

export const useConsole = create<ConsoleStore>((set) => ({
  lines: [],
  history: loadLocal<string[]>(HISTORY_KEY, []),
  running: false,
  job: null,
  pending: false,
  source: loadLocal<string>(SOURCE_KEY, DEFAULT_SOURCE),
  scriptName: "script",
  view: "repl",
  recording: null,
  lastMacro: null,
  prompt: null,
  plugins: null,
  luaWindow: 16,
  set: (patch) => set(patch),
}));

const con = () => useConsole.getState();
let lineSeq = 0;

export function print(kind: LineKind, text: string) {
  const add = text.split("\n").map((t) => ({ id: ++lineSeq, kind, text: t }));
  const lines = con().lines.concat(add);
  con().set({ lines: lines.length > MAX_LINES ? lines.slice(-MAX_LINES) : lines });
}

export function clearConsole() {
  con().set({ lines: [] });
}

export function setSource(source: string, name?: string) {
  con().set({ source, ...(name ? { scriptName: name } : {}) });
  saveLocal(SOURCE_KEY, source);
}

/** Cursor, selection and active tab of this window for a script run. */
export function uiContext(): UiContext {
  const st = useTabs.getState();
  return {
    active: st.active,
    docs: st.tabs.map((t) => ({
      id: t.id,
      cursor: t.cursor,
      selection: hasSelection(t) ? selection(t) : null,
    })),
  };
}

function errorOf(e: unknown): string {
  if (isCmdError(e)) return e.message;
  return e instanceof Error ? e.message : String(e);
}

function onScriptEvent(e: ScriptEvent, done: (ok: boolean) => void) {
  if (e.type === "print") print("out", e.text);
  else if (e.type === "log")
    print(e.level === "error" ? "error" : e.level === "warn" ? "warn" : e.level, e.message);
  else {
    con().set({ running: false, job: null, pending: e.incomplete });
    if (e.error) print("error", scriptErrorText(e.error));
    else for (const v of e.values) print("value", v);
    done(!e.error);
  }
}

function start(launch: (onEvent: (e: ScriptEvent) => void) => Promise<number>): Promise<boolean> {
  return new Promise((resolve) => {
    con().set({ running: true });
    launch((e) => onScriptEvent(e, resolve))
      .then((job) => {
        if (con().running) con().set({ job });
      })
      .catch((e) => {
        con().set({ running: false, job: null });
        print("error", errorOf(e));
        resolve(false);
      });
  });
}

/** Evaluates one console entry (SCR-02). */
export function evalLine(line: string): Promise<boolean> {
  const c = con();
  print("in", `${c.pending ? "»" : "›"} ${line}`);
  if (line.trim()) {
    const history = [line, ...c.history.filter((h) => h !== line)].slice(0, 200);
    c.set({ history });
    saveLocal(HISTORY_KEY, history);
  }
  return start((onEvent) => scriptApi.replEval(line, uiContext(), onEvent));
}

/** Runs a whole script on the console's engine (SCR-01). */
export function runScript(source: string, name = con().scriptName): Promise<boolean> {
  if (con().running) {
    toast("Скрипт уже выполняется", true);
    return Promise.resolve(false);
  }
  print("info", `▶ ${name}`);
  return start((onEvent) => scriptApi.run(source, name, uiContext(), onEvent));
}

export async function stopScript() {
  await scriptApi.cancel(con().job).catch(A.report);
}

export async function resetRepl() {
  await scriptApi.replReset().catch(A.report);
  con().set({ pending: false });
  print("info", "Состояние Lua сброшено");
}

export async function runScriptFile() {
  const { open } = await import("@tauri-apps/plugin-dialog");
  const path = await open({
    multiple: false,
    filters: [{ name: "Lua", extensions: ["lua", "txt"] }],
  });
  if (typeof path !== "string") return;
  try {
    const source = await api.readTextFile(path);
    const name = path.split(/[\\/]/).pop() ?? "script.lua";
    showConsole();
    await runScript(source, name);
  } catch (e) {
    A.report(e);
  }
}

export async function openScriptFile() {
  const { open } = await import("@tauri-apps/plugin-dialog");
  const path = await open({
    multiple: false,
    filters: [{ name: "Lua", extensions: ["lua", "txt"] }],
  });
  if (typeof path !== "string") return;
  try {
    setSource(await api.readTextFile(path), path.split(/[\\/]/).pop());
  } catch (e) {
    A.report(e);
  }
}

export async function saveScriptFile() {
  const { save } = await import("@tauri-apps/plugin-dialog");
  const path = await save({
    defaultPath: `${con().scriptName.replace(/\.lua$/, "")}.lua`,
    filters: [{ name: "Lua", extensions: ["lua"] }],
  });
  if (!path) return;
  await api.writeTextFile(path, con().source).catch(A.report);
}

function showConsole(view: ConsoleView = "repl") {
  con().set({ view });
  runCommand("script.console");
}

/** Tab completion: the new input and the candidates to show. */
export async function completeInput(
  text: string,
  caret: number,
): Promise<{ text: string; caret: number; options: string[] }> {
  const before = text.slice(0, caret);
  const token = /[A-Za-z0-9_.]*$/.exec(before)?.[0] ?? "";
  let options: string[] = [];
  try {
    options = await scriptApi.complete(before);
  } catch {
    options = [];
  }
  if (options.length === 0) return { text, caret, options };
  let common = options[0];
  for (const o of options) {
    let i = 0;
    while (i < common.length && i < o.length && common[i] === o[i]) i++;
    common = common.slice(0, i);
  }
  if (common.length <= token.length) return { text, caret, options };
  const head = before.slice(0, before.length - token.length) + common;
  return { text: head + text.slice(caret), caret: head.length, options };
}

// ---------------------------------------------------------------- macros (SCR-03)

export function toggleRecording() {
  const c = con();
  if (c.recording) {
    const m = c.recording;
    c.set({ recording: null, lastMacro: m.steps.length ? m : c.lastMacro });
    toast(
      m.steps.length
        ? `Макрос записан: ${m.steps.length} шаг(ов)`
        : "Запись остановлена, шагов нет",
    );
  } else {
    c.set({ recording: { name: `Макрос ${new Date().toLocaleTimeString()}`, steps: [] } });
    toast("Запись макроса… Повторное нажатие остановит запись");
  }
}

function recordCommand(id: string) {
  const r = con().recording;
  if (!r || id.startsWith("macro.") || id.startsWith("script.")) return;
  con().set({ recording: { ...r, steps: [...r.steps, { command: id, args: null }] } });
}

/** Replays macro steps in order, waiting for each command (SCR-03). */
export async function replayMacro(m: Macro) {
  for (const [i, step] of m.steps.entries()) {
    const c = commandById(step.command);
    if (!c) {
      toast(`Шаг ${i + 1}: неизвестная команда ${step.command}`, true);
      return;
    }
    if (c.doc && !activeTab()) {
      toast(`Шаг ${i + 1}: нет открытого документа`, true);
      return;
    }
    try {
      await c.run();
    } catch (e) {
      A.report(e);
      return;
    }
  }
  toast(`Макрос выполнен: ${m.steps.length} шаг(ов)`);
}

export function replayLastMacro() {
  const m = con().lastMacro;
  if (!m) return toast("Нет записанного макроса", true);
  return replayMacro(m);
}

export async function saveMacro() {
  const m = con().lastMacro;
  if (!m) return toast("Нет записанного макроса", true);
  const { save } = await import("@tauri-apps/plugin-dialog");
  const path = await save({
    defaultPath: "macro.json",
    filters: [{ name: "Макрос Hexa", extensions: ["json"] }],
  });
  if (!path) return;
  await scriptApi.macroSave(path, m).then(() => toast("Макрос сохранён"), A.report);
}

export async function loadMacro() {
  const { open } = await import("@tauri-apps/plugin-dialog");
  const path = await open({
    multiple: false,
    filters: [{ name: "Макрос Hexa", extensions: ["json"] }],
  });
  if (typeof path !== "string") return;
  try {
    con().set({ lastMacro: await scriptApi.macroLoad(path) });
    toast("Макрос загружен");
  } catch (e) {
    A.report(e);
  }
}

/** Puts the Lua equivalent of the last macro into the script editor. */
export async function macroToLua() {
  const m = con().lastMacro;
  if (!m) return toast("Нет записанного макроса", true);
  try {
    setSource(await scriptApi.macroToLua(m), "macro.lua");
    con().set({ view: "script" });
  } catch (e) {
    A.report(e);
  }
}

// ---------------------------------------------------------------- plugins (SCR-04)

export async function refreshPlugins(reload = false) {
  try {
    const plugins = reload ? await scriptApi.pluginsReload() : await scriptApi.pluginsList();
    con().set({ plugins });
    return plugins;
  } catch (e) {
    A.report(e);
    return null;
  }
}

export async function reloadPlugins() {
  const p = await refreshPlugins(true);
  if (p)
    toast(
      `Плагинов: ${p.plugins.length}${p.failures.length ? `, с ошибками: ${p.failures.length}` : ""}`,
      p.failures.length > 0,
    );
}

// ---------------------------------------------------------------- predicate search (SRCH-08)

let lastLuaKey = "";

function patchTab(id: number, p: Partial<Tab> | ((t: Tab) => Partial<Tab>)) {
  useTabs.getState().update(id, p);
}

/** Streams the offsets where a Lua predicate holds into the results panel. */
export async function luaFindAll(tab: Tab, source: string, scope: Scope | null) {
  const prev = tab.results?.job;
  if (prev) api.cancelJob(prev).catch(() => {});
  const start = scope?.start ?? 0;
  const end = scope?.end ?? tab.info.len;
  lastLuaKey = JSON.stringify([tab.id, source, scope, con().luaWindow]);
  patchTab(tab.id, {
    results: {
      matches: [],
      current: -1,
      running: true,
      done: 0,
      total: end - start,
      truncated: false,
      job: null,
      label: "Lua",
    },
  });
  let pending: Match[] = [];
  const flush = () => {
    const add = pending;
    pending = [];
    if (add.length)
      patchTab(tab.id, (cur) =>
        cur.results
          ? { results: { ...cur.results, matches: cur.results.matches.concat(add) } }
          : {},
      );
  };
  try {
    const job = await scriptApi.predicateSearch(
      tab.id,
      source,
      { window: con().luaWindow, align: scope?.align ?? 1, start, end, max: 100_000 },
      (e) => {
        if (e.type === "batch") {
          pending = pending.concat(e.matches);
          if (pending.length > 2000) flush();
        } else if (e.type === "progress") {
          flush();
          patchTab(tab.id, (cur) =>
            cur.results ? { results: { ...cur.results, done: e.done, total: e.total } } : {},
          );
        } else if (e.type === "done") {
          flush();
          patchTab(tab.id, (cur) =>
            cur.results
              ? { results: { ...cur.results, running: false, truncated: e.truncated, job: null } }
              : {},
          );
          const t = useTabs.getState().tabs.find((x) => x.id === tab.id);
          const ms = t?.results?.matches ?? [];
          if (ms.length) A.showMatch(ms[A.lowerBound(ms, t?.cursor ?? 0)] ?? ms[0]);
        } else {
          flush();
          patchTab(tab.id, (cur) =>
            cur.results ? { results: { ...cur.results, running: false, job: null } } : {},
          );
          lastLuaKey = "";
          A.report(e.error);
        }
      },
    );
    patchTab(tab.id, (cur) => (cur.results?.running ? { results: { ...cur.results, job } } : {}));
  } catch (e) {
    patchTab(tab.id, { results: null });
    lastLuaKey = "";
    A.report(e);
  }
}

/** Enter in the find bar: search with a new predicate, else step through results. */
export function luaFindEnter(tab: Tab, source: string, scope: Scope | null, backwards: boolean) {
  if (!source.trim()) return;
  const key = JSON.stringify([tab.id, source, scope, con().luaWindow]);
  if (key === lastLuaKey && tab.results && !tab.results.running && tab.results.label === "Lua") {
    void A.stepResult(backwards ? -1 : 1);
    return;
  }
  void luaFindAll(tab, source, scope);
}

// ---------------------------------------------------------------- UI events and the local API

let windowLabel: string | null = null;
function myLabel(): string {
  if (windowLabel === null) {
    try {
      windowLabel = getCurrentWindow().label;
    } catch {
      windowLabel = "main";
    }
  }
  return windowLabel;
}

async function handleUiEvent(ev: UiEvent) {
  if (ev.window && ev.window !== myLabel()) return;
  const tabs = useTabs.getState();
  const has = (id: number) => tabs.tabs.some((t) => t.id === id);
  switch (ev.type) {
    case "docChanged": {
      if (!has(ev.doc)) return;
      const info = await api.info(ev.doc);
      A.applyResult(ev.doc, { change: null, info });
      return;
    }
    case "cursor": {
      if (!has(ev.doc)) return;
      patchTab(ev.doc, { cursor: ev.offset, anchor: ev.offset, nibble: 0 });
      if (ev.scroll && tabs.active === ev.doc) A.scrollIntoView(ev.doc, ev.offset, true);
      return;
    }
    case "select": {
      if (!has(ev.doc)) return;
      const s = ev.selection;
      if (s && s.end > s.start) {
        patchTab(ev.doc, { anchor: s.start, cursor: s.end - 1, nibble: 0 });
        if (tabs.active === ev.doc) A.scrollIntoView(ev.doc, s.start, true);
      } else patchTab(ev.doc, (t) => ({ anchor: t.cursor }));
      return;
    }
    case "command":
      if (!runCommand(ev.id)) print("warn", `Команда ${ev.id} недоступна`);
      return;
    case "adopt":
      await A.adopt(ev.info);
      return;
    case "prompt":
      con().set({
        prompt: { req: ev.req, title: ev.title, message: ev.message, default: ev.default },
      });
      return;
  }
}

export function answerPrompt(value: string | null) {
  const p = con().prompt;
  if (!p) return;
  con().set({ prompt: null });
  scriptApi.promptReply(p.req, value).catch(() => {});
}

/** Listens for script requests, records macros and keeps the local API in
 * sync with the `apiEnabled` setting. Returns the cleanup. */
export function installScriptBridge(): () => void {
  let queue: Promise<void> = Promise.resolve();
  const off = listen<UiEvent>(UI_EVENT, (e) => {
    queue = queue.then(() => handleUiEvent(e.payload)).catch(() => {});
  });
  commandHooks.onRun = recordCommand;

  // Local API (SCR-09): follows the setting; the UI context feeds API calls.
  let apiOn = false;
  let ctxTimer: ReturnType<typeof setTimeout> | undefined;
  const pushContext = () => {
    clearTimeout(ctxTimer);
    ctxTimer = setTimeout(() => scriptApi.setContext(uiContext()).catch(() => {}), 150);
  };
  const syncApi = () => {
    const st = useSettings.getState();
    if (!st.loaded || st.s.apiEnabled === apiOn) return;
    apiOn = st.s.apiEnabled;
    if (apiOn) {
      pushContext();
      scriptApi.apiStart().catch((e) => toast(`Локальный API: ${errorOf(e)}`, true));
    } else scriptApi.apiStop().catch(() => {});
  };
  syncApi();
  const offSettings = useSettings.subscribe(syncApi);
  const offTabs = useTabs.subscribe(() => {
    if (apiOn) pushContext();
  });

  return () => {
    off.then((f) => f()).catch(() => {});
    if (commandHooks.onRun === recordCommand) commandHooks.onRun = undefined;
    offSettings();
    offTabs();
    clearTimeout(ctxTimer);
  };
}
