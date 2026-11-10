// Integer calculator with arbitrary precision (UI-09): hex/dec/oct/bin
// literals, + - * / % ** << >> & | ^ ~ and parentheses.

type Tok = { k: "num"; v: bigint } | { k: "op"; v: string };

const OPS = ["**", "<<", ">>", "+", "-", "*", "/", "%", "&", "|", "^", "~", "(", ")"];

function lex(src: string): Tok[] {
  const out: Tok[] = [];
  let i = 0;
  const s = src.replace(/_/g, "");
  while (i < s.length) {
    const c = s[i];
    if (/\s/.test(c)) {
      i++;
      continue;
    }
    const op = OPS.find((o) => s.startsWith(o, i));
    if (op) {
      out.push({ k: "op", v: op });
      i += op.length;
      continue;
    }
    const m =
      /^0x[0-9a-f]+/i.exec(s.slice(i)) ??
      /^0b[01]+/i.exec(s.slice(i)) ??
      /^0o[0-7]+/i.exec(s.slice(i)) ??
      /^[0-9][0-9a-f]*h\b/i.exec(s.slice(i)) ??
      /^[0-9]+/.exec(s.slice(i));
    if (!m) throw new Error(`непонятный символ «${c}»`);
    const t = m[0];
    const v = /h$/i.test(t)
      ? BigInt(`0x${t.slice(0, -1)}`)
      : BigInt(t.toLowerCase().replace(/^0o/, "0o"));
    out.push({ k: "num", v });
    i += t.length;
  }
  return out;
}

const BIN: Record<string, [number, (a: bigint, b: bigint) => bigint]> = {
  "|": [1, (a, b) => a | b],
  "^": [2, (a, b) => a ^ b],
  "&": [3, (a, b) => a & b],
  "<<": [4, (a, b) => a << b],
  ">>": [4, (a, b) => a >> b],
  "+": [5, (a, b) => a + b],
  "-": [5, (a, b) => a - b],
  "*": [6, (a, b) => a * b],
  "/": [
    6,
    (a, b) => {
      if (b === 0n) throw new Error("деление на ноль");
      return a / b;
    },
  ],
  "%": [
    6,
    (a, b) => {
      if (b === 0n) throw new Error("деление на ноль");
      return a % b;
    },
  ],
  "**": [
    7,
    (a, b) => {
      if (b < 0n || b > 4096n) throw new Error("степень вне диапазона");
      return a ** b;
    },
  ],
};

export function calc(src: string): bigint {
  const toks = lex(src);
  if (!toks.length) throw new Error("пустое выражение");
  let p = 0;
  const peek = () => toks[p];
  const unary = (): bigint => {
    const t = toks[p++];
    if (!t) throw new Error("выражение оборвано");
    if (t.k === "num") return t.v;
    if (t.v === "-") return -unary();
    if (t.v === "+") return unary();
    if (t.v === "~") return ~unary();
    if (t.v === "(") {
      const v = expr(0);
      if (toks[p++]?.v !== ")") throw new Error("нет закрывающей скобки");
      return v;
    }
    throw new Error(`неожиданный «${t.v}»`);
  };
  const expr = (min: number): bigint => {
    let lhs = unary();
    for (;;) {
      const t = peek();
      if (t?.k !== "op" || !BIN[t.v]) break;
      const [prec, f] = BIN[t.v];
      if (prec < min) break;
      p++;
      // ** is right-associative.
      const rhs = expr(t.v === "**" ? prec : prec + 1);
      lhs = f(lhs, rhs);
    }
    return lhs;
  };
  const v = expr(0);
  if (p < toks.length) throw new Error(`лишнее «${(toks[p] as { v: unknown }).v}»`);
  return v;
}

/** Two's-complement bits of `v` truncated to `bits`. */
export function wrap(v: bigint, bits: number): bigint {
  return BigInt.asUintN(bits, v);
}

export function views(v: bigint) {
  const u64 = wrap(v, 64);
  const buf = new DataView(new ArrayBuffer(8));
  buf.setBigUint64(0, u64);
  const f32 = buf.getFloat32(4);
  const f64 = buf.getFloat64(0);
  const chars = Array.from({ length: 8 }, (_, i) => buf.getUint8(i))
    .filter((b, i, a) => b !== 0 || a.slice(0, i).some(Boolean))
    .map((b) => (b >= 0x20 && b < 0x7f ? String.fromCharCode(b) : "·"))
    .join("");
  return {
    dec: v.toString(10),
    hex: `${v < 0n ? "-" : ""}0x${(v < 0n ? -v : v).toString(16).toUpperCase()}`,
    oct: `${v < 0n ? "-" : ""}0o${(v < 0n ? -v : v).toString(8)}`,
    bin: (v < 0n ? -v : v).toString(2).replace(/\B(?=(\d{4})+(?!\d))/g, " "),
    u64: `0x${u64.toString(16).toUpperCase().padStart(16, "0")}`,
    i8: BigInt.asIntN(8, v).toString(),
    i16: BigInt.asIntN(16, v).toString(),
    i32: BigInt.asIntN(32, v).toString(),
    i64: BigInt.asIntN(64, v).toString(),
    f32: Number.isFinite(f32) ? f32.toPrecision(9) : String(f32),
    f64: Number.isFinite(f64) ? f64.toPrecision(17) : String(f64),
    chars,
  };
}
