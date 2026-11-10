// Goto expressions (NAV-01, NAV-02).
//
// Numbers are hex by default (`1F`, `0x1F`); decimal needs a `d` suffix or
// `0d` prefix (`128d`). Names: `end`/`len` (document length), `cursor`/`pos`,
// `sel`/`selend`, plus labels supplied by the caller. Operators: + - * / % ×
// and parentheses. Forms:
//   `+N` / `-N`   relative to the cursor
//   `^N`          N bytes before the end
//   `50%`         percent of the file
//   `s:N`         sector N (sector size from context, default 512)
//   `p:N`         page N (4096 bytes)
//   `r:N`         row N (bytes per row from context)
//   `SSSS:OOOO`   segment:offset (segment size from context, default 64 KiB)

export interface ExprContext {
  cursor: number;
  length: number;
  selStart?: number;
  selEnd?: number;
  bytesPerRow?: number;
  sectorSize?: number;
  /** Segment size for `SSSS:OOOO` input (default 64 KiB). */
  segmentSize?: number;
  labels?: Record<string, number>;
}

export interface ExprResult {
  value: number;
  /** Human description of how the input was interpreted. */
  kind: "absolute" | "relative" | "fromEnd" | "percent" | "sector" | "page" | "row";
}

type Tok =
  | { t: "num"; v: number }
  | { t: "op"; v: string }
  | { t: "lp" }
  | { t: "rp" }
  | { t: "id"; v: string };

function tokenize(src: string): Tok[] {
  const out: Tok[] = [];
  let i = 0;
  while (i < src.length) {
    const c = src[i];
    if (/\s/.test(c)) {
      i++;
      continue;
    }
    if ("+-*/%".includes(c) || c === "×" || c === "·") {
      out.push({ t: "op", v: c === "×" || c === "·" ? "*" : c });
      i++;
      continue;
    }
    if (c === "(") {
      out.push({ t: "lp" });
      i++;
      continue;
    }
    if (c === ")") {
      out.push({ t: "rp" });
      i++;
      continue;
    }
    const m = /^(0x[0-9a-f_]+|0d[0-9_]+|0b[01_]+|0o[0-7_]+|[a-z_$][\w.$]*|[0-9][0-9a-f_]*)/i.exec(
      src.slice(i),
    );
    if (!m) throw new Error(`Непонятный символ «${c}»`);
    const w = m[0];
    i += w.length;
    const lw = w.toLowerCase().replaceAll("_", "");
    if (lw.startsWith("0x")) out.push({ t: "num", v: Number.parseInt(lw.slice(2), 16) });
    else if (lw.startsWith("0d")) out.push({ t: "num", v: Number.parseInt(lw.slice(2), 10) });
    else if (lw.startsWith("0b")) out.push({ t: "num", v: Number.parseInt(lw.slice(2), 2) });
    else if (lw.startsWith("0o")) out.push({ t: "num", v: Number.parseInt(lw.slice(2), 8) });
    else if (/^[0-9]+d$/.test(lw)) out.push({ t: "num", v: Number.parseInt(lw.slice(0, -1), 10) });
    else if (/^[0-9][0-9a-f]*$/.test(lw)) out.push({ t: "num", v: Number.parseInt(lw, 16) });
    else if (/^[0-9]/.test(lw)) throw new Error(`Непонятное число «${w}»`);
    else out.push({ t: "id", v: w });
  }
  return out;
}

function evaluate(toks: Tok[], ctx: ExprContext): number {
  let i = 0;
  const peek = () => toks[i];
  const name = (n: string): number => {
    const l = n.toLowerCase();
    if (l === "end" || l === "len") return ctx.length;
    if (l === "cursor" || l === "pos" || l === "here") return ctx.cursor;
    if (l === "sel") return ctx.selStart ?? ctx.cursor;
    if (l === "selend") return ctx.selEnd ?? ctx.cursor;
    const label = ctx.labels?.[n] ?? ctx.labels?.[l];
    if (label !== undefined) return label;
    // Hex digits that look like a word (e.g. "beef").
    if (/^[0-9a-f]+$/i.test(n)) return Number.parseInt(n, 16);
    throw new Error(`Неизвестное имя «${n}»`);
  };
  const primary = (): number => {
    const t = toks[i++];
    if (!t) throw new Error("Выражение оборвалось");
    if (t.t === "num") return t.v;
    if (t.t === "id") return name(t.v);
    if (t.t === "lp") {
      const v = sum();
      if (toks[i++]?.t !== "rp") throw new Error("Не хватает «)»");
      return v;
    }
    if (t.t === "op" && t.v === "-") return -primary();
    if (t.t === "op" && t.v === "+") return primary();
    throw new Error("Ожидалось число");
  };
  const product = (): number => {
    let v = primary();
    for (let t = peek(); t?.t === "op" && "*/%".includes(t.v); t = peek()) {
      i++;
      const r = primary();
      if (t.v === "*") v *= r;
      else if (r === 0) throw new Error("Деление на ноль");
      else if (t.v === "/") v = Math.trunc(v / r);
      else v %= r;
    }
    return v;
  };
  const sum = (): number => {
    let v = product();
    for (let t = peek(); t?.t === "op" && "+-".includes(t.v); t = peek()) {
      i++;
      const r = product();
      v = t.v === "+" ? v + r : v - r;
    }
    return v;
  };
  const v = sum();
  if (i < toks.length) throw new Error("Лишние символы в конце");
  return v;
}

export function evalExpr(input: string, ctx: ExprContext): ExprResult {
  let s = input.trim();
  if (!s) throw new Error("Пусто");
  const seg = /^([0-9a-f]{1,8}):([0-9a-f]{1,8})$/i.exec(s);
  if (seg) {
    const size = ctx.segmentSize ?? 0x10000;
    return {
      value: Number.parseInt(seg[1], 16) * size + Number.parseInt(seg[2], 16),
      kind: "absolute",
    };
  }
  const pref = /^([spr]):\s*(.+)$/i.exec(s);
  if (pref) {
    const n = evaluate(tokenize(pref[2]), ctx);
    const k = pref[1].toLowerCase();
    const unit = k === "s" ? (ctx.sectorSize ?? 512) : k === "p" ? 4096 : (ctx.bytesPerRow ?? 16);
    return { value: n * unit, kind: k === "s" ? "sector" : k === "p" ? "page" : "row" };
  }
  if (s.endsWith("%")) {
    const p = Number.parseFloat(s.slice(0, -1).replace(",", "."));
    if (!Number.isFinite(p)) throw new Error("Ожидался процент");
    return {
      value: Math.floor((ctx.length * Math.min(Math.max(p, 0), 100)) / 100),
      kind: "percent",
    };
  }
  if (s.startsWith("^")) {
    const n = evaluate(tokenize(s.slice(1)), ctx);
    return { value: ctx.length - n, kind: "fromEnd" };
  }
  let kind: ExprResult["kind"] = "absolute";
  if (s.startsWith("+") || s.startsWith("-")) {
    s = `cursor${s}`;
    kind = "relative";
  }
  return { value: evaluate(tokenize(s), ctx), kind };
}
