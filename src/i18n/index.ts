// Interface localisation (UI-19). The source strings are Russian literals; en.json maps
// each of them (or a pattern with {placeholders}) to English. Rendered text is translated
// in place in the DOM, so components keep their literals and need no wrappers.
import { useEffect } from "react";
import catalog from "./en.json";

export type Locale = "ru" | "en";

const CYRILLIC = /[Ѐ-ӿ]/;
/** `{name}` matches any text; `{#name}` only a number (for keys composed by hand). */
const PLACEHOLDER = /\{(#?[A-Za-z_][A-Za-z0-9_]*)\}/g;

interface Pattern {
  re: RegExp;
  /** Placeholder names per regex group; adjacent placeholders share one group. */
  groups: string[][];
  to: string;
  /** Longest literal piece; a cheap substring test before the regex. */
  probe: string;
  weight: number;
}

export type Translator = (text: string) => string;

/** Builds a translator over a catalog of exact and pattern keys. */
export function createTranslator(entries: Record<string, string>): Translator {
  const exact = new Map<string, string>();
  const patterns: Pattern[] = [];
  for (const [from, to] of Object.entries(entries)) {
    if (!to) continue;
    exact.set(from, to);
    const pieces = from.split(PLACEHOLDER);
    if (pieces.length === 1) continue;
    const groups: string[][] = [];
    let src = "^";
    let probe = "";
    let weight = 0;
    pieces.forEach((p, i) => {
      if (i % 2 === 1) {
        // `{a}{b}` cannot be split by a regex; the split is chosen when matching.
        if (pieces[i - 1] === "" && i > 1) groups[groups.length - 1].push(p);
        else {
          groups.push([p]);
          src += "\u0000";
        }
      } else {
        src += p.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
        weight += p.length;
        if (p.trim().length > probe.length) probe = p.trim();
      }
    });
    const re = src.split("\u0000").reduce((acc, part, i) => {
      if (i === 0) return part;
      const g = groups[i - 1];
      const num = g.length === 1 && g[0][0] === "#";
      return acc + (num ? "([-+]?[\\d\\s.,\u00a0]*\\d[%+…]*)" : "([\\s\\S]*?)") + part;
    }, "");
    patterns.push({ re: new RegExp(`${re}$`), groups, to, probe, weight });
  }
  // The most specific (longest literal text) pattern wins.
  patterns.sort((a, b) => b.weight - a.weight);

  const cache = new Map<string, string | null>();

  const core = (text: string, depth: number): string | null => {
    const hit = exact.get(text);
    if (hit !== undefined) return hit;
    const cached = cache.get(text);
    if (cached !== undefined) return cached;
    let out: string | null = null;
    const norm = text.replace(/\s+/g, " ");
    if (norm !== text) out = exact.get(norm) ?? null;
    if (out === null) {
      for (const p of patterns) {
        if (!norm.includes(p.probe)) continue;
        const m = p.re.exec(norm);
        if (!m) continue;
        const values = new Map<string, string>();
        p.groups.forEach((names, i) => {
          const parts = names.length === 1 ? [m[i + 1]] : split(m[i + 1], names.length, depth);
          names.forEach((name, j) => {
            values.set(name, parts[j]);
          });
        });
        out = p.to.replace(PLACEHOLDER, (all, name: string) => {
          const v = values.get(name);
          if (v === undefined) return all;
          return depth < 2 ? capture(v, depth + 1) : v;
        });
        break;
      }
    }
    // Lists joined at runtime: translate each piece.
    if (out === null && depth < 2 && /( · | — |\n)/.test(text)) {
      const parts = text.split(/( · | — |\n)/);
      const done = parts.map((p, i) => (i % 2 === 1 ? p : capture(p, depth + 1, true)));
      if (done.every((p) => p !== null)) out = done.join("");
    }
    if (cache.size > 4000) cache.clear();
    cache.set(text, out);
    return out;
  };

  /** Splits text for `n` adjacent placeholders so that every Russian piece is known. */
  const split = (text: string, n: number, depth: number): string[] => {
    const known = (s: string) => !CYRILLIC.test(s) || capture(s, depth + 1, true) !== null;
    const go = (s: string, k: number): string[] | null => {
      if (k === 1) return known(s) ? [s] : null;
      for (let i = 0; i <= s.length; i++) {
        const head = s.slice(0, i);
        if (!known(head)) continue;
        const rest = go(s.slice(i), k - 1);
        if (rest) return [head, ...rest];
      }
      return null;
    };
    return go(text, n) ?? [text, ...Array<string>(n - 1).fill("")];
  };

  function capture(v: string, depth: number): string;
  function capture(v: string, depth: number, strict: true): string | null;
  function capture(v: string, depth: number, strict = false): string | null {
    if (!CYRILLIC.test(v)) return v;
    const m = /^(\s*)([\s\S]*?)(\s*)$/.exec(v)!;
    const r = core(m[2], depth);
    if (r === null) return strict ? null : v;
    return m[1] + r + m[3];
  }

  return (text: string) => {
    if (!CYRILLIC.test(text)) return text;
    return capture(text, 0);
  };
}

/** Russian → English; text it does not know is returned unchanged. */
export const translate: Translator = createTranslator(catalog as Record<string, string>);

let current: Locale = "ru";

export function locale(): Locale {
  return current;
}

/** Text in the current locale, for places outside the DOM (canvas, native dialogs). */
export function t(text: string): string {
  return current === "en" ? translate(text) : text;
}

// ------------------------------------------------------------------ DOM

const ATTRS = ["title", "aria-label", "placeholder", "alt", "label"];
/** User data is never translated; editable text keeps its content but not its labels. */
const NO_I18N = "[data-no-i18n]";
const RAW = "textarea, script, style, [contenteditable]:not([contenteditable=false])";
const SKIP = `${NO_I18N}, ${RAW}`;

interface Swap {
  ru: string;
  en: string;
}
const texts = new WeakMap<Text, Swap>();
const attrs = new WeakMap<Element, Map<string, Swap>>();

let observer: MutationObserver | null = null;
let pending = new Set<Node>();
let frame = 0;

function skipped(el: Element | null, selector = SKIP): boolean {
  return !el || el.closest(selector) !== null;
}

function doText(n: Text) {
  const v = n.data;
  const rec = texts.get(n);
  if (rec && v === rec.en) return;
  if (!CYRILLIC.test(v) || skipped(n.parentElement)) {
    if (rec) texts.delete(n);
    return;
  }
  const out = translate(v);
  if (out === v) return;
  texts.set(n, { ru: v, en: out });
  n.data = out;
}

function doAttrs(el: Element) {
  let map = attrs.get(el);
  for (const name of ATTRS) {
    if (name === "label" && el.tagName !== "OPTION" && el.tagName !== "OPTGROUP") continue;
    const v = el.getAttribute(name);
    const rec = map?.get(name);
    if (v === null || (rec && v === rec.en)) continue;
    if (!CYRILLIC.test(v)) {
      map?.delete(name);
      continue;
    }
    const out = translate(v);
    if (out === v) continue;
    if (!map) {
      map = new Map();
      attrs.set(el, map);
    }
    map.set(name, { ru: v, en: out });
    el.setAttribute(name, out);
  }
}

function walk(root: Node) {
  if (root.nodeType === Node.TEXT_NODE) {
    doText(root as Text);
    return;
  }
  if (root.nodeType !== Node.ELEMENT_NODE) return;
  const el = root as Element;
  if (skipped(el, NO_I18N)) return;
  if (skipped(el, RAW)) {
    doAttrs(el);
    return;
  }
  const tw = document.createTreeWalker(el, NodeFilter.SHOW_ELEMENT | NodeFilter.SHOW_TEXT, {
    acceptNode: (n) => {
      if (n.nodeType !== Node.ELEMENT_NODE) return NodeFilter.FILTER_ACCEPT;
      if ((n as Element).matches(NO_I18N)) return NodeFilter.FILTER_REJECT;
      if ((n as Element).matches(RAW)) {
        doAttrs(n as Element);
        return NodeFilter.FILTER_REJECT;
      }
      return NodeFilter.FILTER_ACCEPT;
    },
  });
  for (let n: Node | null = el; n; n = tw.nextNode()) {
    if (n.nodeType === Node.TEXT_NODE) doText(n as Text);
    else doAttrs(n as Element);
  }
}

/** Next animation frame; a timer covers hidden windows, where frames do not run. */
let timer: ReturnType<typeof setTimeout> | undefined;
function schedule() {
  if (frame) return;
  const run = () => {
    if (frame) flushDomTranslation();
  };
  frame = typeof requestAnimationFrame === "function" ? requestAnimationFrame(run) : -1;
  timer = setTimeout(run, 100);
}
function unschedule() {
  if (frame > 0 && typeof cancelAnimationFrame === "function") cancelAnimationFrame(frame);
  clearTimeout(timer);
  frame = 0;
}

/** Applies queued mutations now (normally done once per animation frame). */
export function flushDomTranslation() {
  unschedule();
  if (!observer) return;
  for (const r of observer.takeRecords()) queue(r);
  const nodes = pending;
  pending = new Set();
  for (const n of nodes) if (n.isConnected) walk(n);
  // Our own writes come back as mutations; they need no work.
  observer.takeRecords();
}

function queue(r: MutationRecord) {
  if (r.type === "childList") for (const n of r.addedNodes) pending.add(n);
  else if (r.type === "characterData") pending.add(r.target);
  else if (r.target.nodeType === Node.ELEMENT_NODE && !skipped(r.target as Element, NO_I18N))
    doAttrs(r.target as Element);
}

export function startDomTranslation(root: HTMLElement = document.body) {
  if (observer) return;
  observer = new MutationObserver((records) => {
    for (const r of records) queue(r);
    if (pending.size) schedule();
  });
  observer.observe(root, {
    subtree: true,
    childList: true,
    characterData: true,
    attributes: true,
    attributeFilter: ATTRS,
  });
  walk(root);
  observer.takeRecords();
}

/** Stops translating and puts the original Russian text back. */
export function stopDomTranslation(root: HTMLElement = document.body) {
  if (!observer) return;
  observer.disconnect();
  observer = null;
  unschedule();
  pending = new Set();
  const tw = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  for (let n = tw.nextNode(); n; n = tw.nextNode()) {
    const rec = texts.get(n as Text);
    if (rec && (n as Text).data === rec.en) (n as Text).data = rec.ru;
    texts.delete(n as Text);
  }
  for (const el of root.querySelectorAll(ATTRS.map((a) => `[${a}]`).join(","))) {
    const map = attrs.get(el);
    if (!map) continue;
    for (const [name, rec] of map)
      if (el.getAttribute(name) === rec.en) el.setAttribute(name, rec.ru);
    attrs.delete(el);
  }
}

// ------------------------------------------------------------- native

const DIALOG_FIELDS = new Set([
  "title",
  "name",
  "message",
  "okLabel",
  "cancelLabel",
  "okButtonLabel",
  "cancelButtonLabel",
  "yesButtonLabel",
  "noButtonLabel",
]);

function translateArgs(v: unknown, key = ""): unknown {
  if (typeof v === "string") return DIALOG_FIELDS.has(key) ? translate(v) : v;
  if (Array.isArray(v)) return v.map((x) => translateArgs(x, key));
  if (v && typeof v === "object" && Object.getPrototypeOf(v) === Object.prototype)
    return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, translateArgs(x, k)]));
  return v;
}

type Invoke = (cmd: string, args?: unknown, options?: unknown) => unknown;
let patched: Invoke | null = null;

/** Native file and message dialogs get their titles and filter names translated. */
function patchNativeDialogs() {
  const internals = (window as unknown as { __TAURI_INTERNALS__?: { invoke?: Invoke } })
    .__TAURI_INTERNALS__;
  if (!internals?.invoke || internals.invoke === patched) return;
  const orig = internals.invoke.bind(internals);
  patched = (cmd, args, options) =>
    orig(
      cmd,
      current === "en" && cmd.startsWith("plugin:dialog|") ? translateArgs(args) : args,
      options,
    );
  try {
    internals.invoke = patched;
  } catch {
    /* read-only IPC bridge: dialogs keep their Russian titles */
  }
}

/** Switches the interface language. */
export function setLocale(l: Locale) {
  current = l;
  if (typeof document === "undefined") return;
  document.documentElement.lang = l;
  if (l === "en") {
    patchNativeDialogs();
    startDomTranslation();
  } else {
    stopDomTranslation();
  }
}

/**
 * Keeps the interface in `l`. The locale is switched during render so that
 * formatters (sizes, times) already use it for the re-render that follows.
 */
export function useLocale(l: Locale) {
  current = l;
  useEffect(() => setLocale(l), [l]);
}
