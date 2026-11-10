// Browser stand-ins for the scripting commands (see mock.ts). Lua is not
// available here: the console understands arithmetic and `print("…")`, the
// Lua find kind understands `b:byte(1) == N`.

// biome-ignore lint/suspicious/noExplicitAny: test double
type Args = any;
type Read = (id: number) => Uint8Array | null;

const API_NAMES = [
  "array",
  "active",
  "clear_selection",
  "command",
  "confirm",
  "cursor",
  "delete",
  "documents",
  "find_all",
  "find_next",
  "go_to",
  "hash",
  "hex",
  "insert",
  "inspect",
  "len",
  "log",
  "message",
  "open",
  "print",
  "prompt",
  "read",
  "redo",
  "replace",
  "save",
  "select",
  "selection",
  "set_cursor",
  "template",
  "transform",
  "undo",
  "unhex",
  "write",
];
const GLOBALS = ["hexa", "print", "pairs", "ipairs", "string", "table", "math", "tostring"];

const api = { running: false, port: 4392, token: "mock-token-0123456789abcdef" };

export const SCRIPT_COMMANDS = new Set([
  "script_run",
  "script_repl_eval",
  "script_complete",
  "script_repl_reset",
  "script_cancel",
  "script_prompt_reply",
  "script_set_context",
  "macro_save",
  "macro_load",
  "macro_to_lua",
  "plugins_list",
  "plugins_reload",
  "plugins_open_dir",
  "plugins_run",
  "catalog_read",
  "catalog_install",
  "api_server_start",
  "api_server_stop",
  "api_server_status",
  "api_server_set_port",
  "api_server_regenerate_token",
  "predicate_search",
]);

/** `1 + 2 * (3 - 1)`, `0x10 // 3`, `2 ^ 8`; throws on anything else. */
export function arith(src: string): number {
  const toks = src.match(/0x[0-9a-f]+|\d+(?:\.\d+)?|\/\/|[-+*/%^()]|\S/gi) ?? [];
  let i = 0;
  const peek = () => toks[i];
  const next = () => toks[i++];
  const atom = (): number => {
    const t = next();
    if (t === "(") {
      const v = sum();
      if (next() !== ")") throw new Error("ожидается )");
      return v;
    }
    if (t === "-") return -atom();
    if (t !== undefined && /^(0x[0-9a-f]+|\d+(\.\d+)?)$/i.test(t)) return Number(t);
    throw new Error(`неожиданный символ ${t ?? "<eof>"}`);
  };
  const pow = (): number => {
    const b = atom();
    if (peek() !== "^") return b;
    next();
    return b ** pow();
  };
  const prod = (): number => {
    let v = pow();
    while (["*", "/", "//", "%"].includes(peek() ?? "")) {
      const op = next();
      const r = pow();
      v = op === "*" ? v * r : op === "/" ? v / r : op === "//" ? Math.floor(v / r) : v % r;
    }
    return v;
  };
  const sum = (): number => {
    let v = prod();
    while (peek() === "+" || peek() === "-") v = next() === "+" ? v + prod() : v - prod();
    return v;
  };
  const v = sum();
  if (i < toks.length) throw new Error(`неожиданный символ ${toks[i]}`);
  return v;
}

function luaNumber(v: number, float: boolean): string {
  if (!float && Number.isInteger(v)) return String(v);
  return Number.isInteger(v) ? v.toFixed(1) : String(v);
}

function evalMock(line: string) {
  const src = line
    .trim()
    .replace(/^=/, "")
    .replace(/^return\s+/, "");
  if (/\b(do|then|function\s*\([^)]*\))\s*$/.test(src))
    return { values: [], prints: [], incomplete: true, error: null };
  const p = /^print\((["'])(.*)\1\)$/.exec(src);
  if (p) return { values: [], prints: [p[2]], incomplete: false, error: null };
  if (!src) return { values: [], prints: [], incomplete: false, error: null };
  try {
    const float = /\/(?!\/)|\^|\./.test(src.replace(/0x[0-9a-f]+/gi, ""));
    return {
      values: [luaNumber(arith(src), float)],
      prints: [],
      incomplete: false,
      error: null,
    };
  } catch (e) {
    return {
      values: [],
      prints: [],
      incomplete: false,
      error: { kind: "runtime", message: `mock: ${(e as Error).message}`, line: 1 },
    };
  }
}

const store = (k: string, v?: unknown) => {
  if (v === undefined) return JSON.parse(localStorage.getItem(`hexa-mock-${k}`) ?? "null");
  localStorage.setItem(`hexa-mock-${k}`, JSON.stringify(v));
};

const status = () => ({
  ...api,
  url: `http://127.0.0.1:${api.port}/`,
  error: null,
});

export function scriptMock(cmd: string, a: Args, read: Read): unknown {
  switch (cmd) {
    case "script_repl_eval": {
      const r = evalMock(a.line);
      setTimeout(() => {
        for (const text of r.prints) a.channel.onmessage({ type: "print", text });
        a.channel.onmessage({
          type: "done",
          values: r.values,
          error: r.error,
          incomplete: r.incomplete,
          millis: 0,
        });
      }, 10);
      return 1;
    }
    case "script_run":
      setTimeout(() => {
        a.channel.onmessage({ type: "print", text: "mock: Lua выполняется только в приложении" });
        a.channel.onmessage({
          type: "done",
          values: [],
          error: null,
          incomplete: false,
          millis: 0,
        });
      }, 10);
      return 1;
    case "script_complete": {
      const token = /[A-Za-z0-9_.]*$/.exec(a.input)?.[0] ?? "";
      if (token.startsWith("hexa."))
        return API_NAMES.filter((n) => n.startsWith(token.slice(5))).map((n) => `hexa.${n}`);
      return GLOBALS.filter((g) => g.startsWith(token));
    }
    case "macro_to_lua": {
      const m = a.mac;
      return `-- Macro: ${m.name}\n${m.steps
        .map((s: { command: string }) => `hexa.command(${JSON.stringify(s.command)})`)
        .join("\n")}\n`;
    }
    case "macro_save":
      store(`macro:${a.path}`, a.mac);
      return null;
    case "macro_load":
      return store(`macro:${a.path}`) ?? Promise.reject({ kind: "notFound", message: a.path });
    case "plugins_list":
    case "plugins_reload":
      return { dir: "/mock/config/plugins", plugins: [], failures: [] };
    case "plugins_open_dir":
      return "/mock/config/plugins";
    case "plugins_run":
    case "catalog_read":
    case "catalog_install":
      return Promise.reject({ kind: "unsupported", message: "mock: плагины недоступны" });
    case "api_server_start":
      api.running = true;
      if (a.port) api.port = a.port;
      return status();
    case "api_server_stop":
      api.running = false;
      return status();
    case "api_server_set_port":
      api.port = a.port;
      return status();
    case "api_server_regenerate_token":
      api.token = `mock-${Math.random().toString(16).slice(2)}`;
      return status();
    case "api_server_status":
      return status();
    case "predicate_search": {
      const data = read(a.id);
      const m = /b:byte\(1\)\s*==\s*(0x[0-9a-f]+|\d+)/i.exec(a.source);
      setTimeout(() => {
        if (!data || !m) {
          a.channel.onmessage({
            type: "error",
            error: { kind: "script", message: "mock: поддерживается только b:byte(1) == N" },
          });
          return;
        }
        const want = Number(m[1]);
        const align = a.opts?.align ?? 1;
        const matches = [];
        for (let i = a.opts?.start ?? 0; i < (a.opts?.end ?? data.length); i++)
          if (data[i] === want && i % align === 0) matches.push({ offset: i, len: 1 });
        a.channel.onmessage({ type: "batch", matches });
        a.channel.onmessage({ type: "done", count: matches.length, truncated: false });
      }, 10);
      return 2;
    }
    default:
      return null;
  }
}
