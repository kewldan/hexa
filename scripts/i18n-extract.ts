// Collects the Russian UI strings of src/ for the English resource file (UI-19).
//
//   bun scripts/i18n-extract.ts          lists keys missing from src/i18n/en.json
//   bun scripts/i18n-extract.ts --unused also lists keys no longer found in the code
//   bun scripts/i18n-extract.ts --json   prints the missing keys as a JSON stub
//
// String literals and JSX text become exact keys; template literals and string
// concatenations become pattern keys where each `${expr}` is a `{name}`
// placeholder named after the expression.
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative, sep } from "node:path";
import { parseAst } from "vite";

export const CYRILLIC = /[Ѐ-ӿ]/;

export interface Found {
  key: string;
  file: string;
  line: number;
}

// biome-ignore lint/suspicious/noExplicitAny: ESTree nodes are walked generically.
type Node = any;

const SKIP_DIRS = new Set(["dev", "i18n"]);

/** Source files to scan, relative paths under `root/src`. */
export function sourceFiles(root: string): string[] {
  const out: string[] = [];
  const walk = (dir: string) => {
    for (const name of readdirSync(dir).sort()) {
      const path = join(dir, name);
      if (statSync(path).isDirectory()) {
        if (!(dir === join(root, "src") && SKIP_DIRS.has(name))) walk(path);
      } else if (/\.tsx?$/.test(name) && !/\.test\.tsx?$/.test(name) && !name.endsWith(".d.ts")) {
        out.push(path);
      }
    }
  };
  walk(join(root, "src"));
  return out;
}

/** Whitespace rules of JSX text: lines are trimmed and joined by a space. */
function jsxText(raw: string): string {
  const lines = raw.split(/\r\n|\n|\r/);
  if (lines.length === 1) return raw;
  return lines
    .map((l, i) => {
      let s = l;
      if (i > 0) s = s.trimStart();
      if (i < lines.length - 1) s = s.trimEnd();
      return s;
    })
    .filter((l) => l.length > 0)
    .join(" ");
}

const ENTITIES: Record<string, string> = {
  amp: "&",
  lt: "<",
  gt: ">",
  quot: '"',
  apos: "'",
  nbsp: "\u00a0",
};

/** JSX text and attribute strings are HTML-decoded by the compiler. */
function decodeEntities(s: string): string {
  return s.replace(/&(#x[0-9a-f]+|#\d+|[a-z]+);/gi, (all, e: string) => {
    if (e[0] === "#")
      return String.fromCodePoint(
        e[1] === "x" || e[1] === "X" ? Number.parseInt(e.slice(2), 16) : Number(e.slice(1)),
      );
    return ENTITIES[e] ?? all;
  });
}

export function normalize(s: string): string {
  return s.replace(/\s+/g, " ").trim();
}

function ident(s: string): string {
  const clean = s.replace(/[^A-Za-z0-9_]/g, "");
  return /^[A-Za-z_]/.test(clean) ? clean : "x";
}

/** Stable placeholder name for an interpolated expression. */
function nameOf(e: Node): string {
  switch (e?.type) {
    case "Identifier":
      return ident(e.name);
    case "ThisExpression":
      return "x";
    case "MemberExpression":
      return e.computed ? nameOf(e.object) : ident(e.property.name);
    case "CallExpression": {
      const a = e.arguments[0];
      if (a && /^(Identifier|MemberExpression|CallExpression|BinaryExpression)$/.test(a.type))
        return nameOf(a);
      if (e.callee.type === "MemberExpression") return nameOf(e.callee.object);
      return nameOf(e.callee);
    }
    case "BinaryExpression":
    case "LogicalExpression":
      return nameOf(e.left);
    case "ChainExpression":
    case "ParenthesizedExpression":
    case "TSAsExpression":
    case "TSNonNullExpression":
    case "TSSatisfiesExpression":
    case "AwaitExpression":
      return nameOf(e.expression);
    case "UnaryExpression":
      return nameOf(e.argument);
    case "Literal":
      return typeof e.value === "number" ? "n" : "x";
    default:
      return "x";
  }
}

/** Builds a pattern key from literal pieces and expressions in order. */
function patternKey(parts: (string | Node)[]): string {
  const used = new Map<string, number>();
  let out = "";
  for (const p of parts) {
    if (typeof p === "string") {
      out += p;
      continue;
    }
    const base = nameOf(p);
    const n = (used.get(base) ?? 0) + 1;
    used.set(base, n);
    out += `{${n === 1 ? base : `${base}${n}`}}`;
  }
  return normalize(out);
}

function isStr(n: Node): boolean {
  return n?.type === "Literal" && typeof n.value === "string";
}

/** Flattens a `"a" + x + "b"` chain; null when it is not a string concatenation. */
function concatParts(n: Node): (string | Node)[] | null {
  const parts: (string | Node)[] = [];
  let hasString = false;
  const walk = (e: Node) => {
    if (e.type === "BinaryExpression" && e.operator === "+") {
      walk(e.left);
      walk(e.right);
    } else if (isStr(e)) {
      hasString = true;
      parts.push(e.value);
    } else if (e.type === "TemplateLiteral") {
      hasString = true;
      e.quasis.forEach((q: Node, i: number) => {
        parts.push(q.value.cooked ?? q.value.raw);
        if (i < e.expressions.length) parts.push(e.expressions[i]);
      });
    } else {
      parts.push(e);
    }
  };
  walk(n);
  return hasString ? parts : null;
}

export function extractSource(code: string, file: string): Found[] {
  const out: Found[] = [];
  const lineStarts = [0];
  for (let i = 0; i < code.length; i++) if (code[i] === "\n") lineStarts.push(i + 1);
  const lineOf = (pos: number) => {
    let lo = 0;
    let hi = lineStarts.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (lineStarts[mid] <= pos) lo = mid;
      else hi = mid - 1;
    }
    return lo + 1;
  };
  const lines = code.split("\n");
  // `// i18n-ignore` on the line above (or the same line) skips a string.
  const ignored = (line: number) =>
    lines[line - 1]?.includes("i18n-ignore") || lines[line - 2]?.includes("i18n-ignore");
  const add = (key: string, node: Node) => {
    if (!CYRILLIC.test(key)) return;
    const line = lineOf(node.start ?? 0);
    if (!ignored(line)) out.push({ key, file, line });
  };

  const visit = (n: Node, parent: Node, field: string) => {
    if (!n || typeof n !== "object") return;
    if (Array.isArray(n)) {
      for (const c of n) visit(c, parent, field);
      return;
    }
    if (typeof n.type !== "string") return;
    // Types, regexes, module specifiers and object keys are not UI text.
    if (n.type.startsWith("TS") && /Type|Interface|Declare|Enum/.test(n.type)) return;
    if (n.type === "ImportDeclaration" || n.type === "ExportAllDeclaration") return;
    if (n.type === "Literal") {
      if (typeof n.value === "string" && !n.regex) {
        const keyField =
          field === "key" && (parent?.type === "Property" || parent?.type === "PropertyDefinition");
        const text = parent?.type === "JSXAttribute" ? decodeEntities(n.value) : n.value;
        if (!keyField || parent.computed) add(normalize(text), n);
      }
      return;
    }
    if (n.type === "JSXText") {
      add(normalize(decodeEntities(jsxText(n.value))), n);
      return;
    }
    if (n.type === "TemplateLiteral") {
      if (n.quasis.some((q: Node) => CYRILLIC.test(q.value.cooked ?? q.value.raw))) {
        add(patternKey(concatParts(n)!), n);
      }
      for (const e of n.expressions) visit(e, n, "expressions");
      return;
    }
    if (n.type === "BinaryExpression" && n.operator === "+") {
      const parts = concatParts(n);
      if (parts && parts.some((p) => typeof p === "string" && CYRILLIC.test(p))) {
        add(patternKey(parts), n);
        for (const p of parts) if (typeof p !== "string") visit(p, n, "operand");
        return;
      }
    }
    for (const k of Object.keys(n)) {
      if (k === "type" || k === "start" || k === "end" || k === "range" || k === "loc") continue;
      const v = n[k];
      if (v && typeof v === "object") visit(v, n, k);
    }
  };

  const ast = parseAst(code, { lang: file.endsWith(".tsx") ? "tsx" : "ts" }, file);
  visit(ast, null, "");
  return out;
}

/** Every Russian UI string of the app, keyed by text. */
export function extractAll(root: string): Map<string, Found[]> {
  const all = new Map<string, Found[]>();
  for (const path of sourceFiles(root)) {
    const file = relative(root, path).split(sep).join("/");
    for (const f of extractSource(readFileSync(path, "utf8"), file)) {
      const list = all.get(f.key);
      if (list) list.push(f);
      else all.set(f.key, [f]);
    }
  }
  return all;
}

export function loadCatalog(root: string): Record<string, string> {
  return JSON.parse(readFileSync(join(root, "src/i18n/en.json"), "utf8"));
}

/** Keys found in the code that the catalog lacks (or maps to an empty string). */
export function missingKeys(root: string): Found[] {
  const catalog = loadCatalog(root);
  const out: Found[] = [];
  for (const [key, found] of extractAll(root)) if (!catalog[key]) out.push(found[0]);
  return out;
}

if (process.argv[1]?.endsWith("i18n-extract.ts")) {
  const root = join(import.meta.dirname, "..");
  const missing = missingKeys(root);
  for (const m of missing) console.log(`${m.file}:${m.line}\t${JSON.stringify(m.key)}`);
  if (process.argv.includes("--unused")) {
    const found = extractAll(root);
    for (const k of Object.keys(loadCatalog(root)))
      if (!found.has(k) && !k.includes("{#")) console.log(`unused\t${JSON.stringify(k)}`);
  }
  if (process.argv.includes("--json")) {
    const obj: Record<string, string> = {};
    for (const m of missing) obj[m.key] = "";
    console.log(JSON.stringify(obj, null, 2));
  }
  console.error(`${missing.length} missing of ${extractAll(root).size} strings`);
  process.exit(missing.length ? 1 : 0);
}
