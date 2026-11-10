import { describe, expect, it } from "vitest";
import { calc, views } from "./calc";

describe("calculator", () => {
  it("evaluates mixed radices with precedence", () => {
    expect(calc("0x10 + 2 * 3")).toBe(22n);
    expect(calc("(1 << 4) | 0b11")).toBe(19n);
    expect(calc("2 ** 3 ** 2")).toBe(512n);
    expect(calc("~0 & 0xFF")).toBe(255n);
    expect(calc("0FFh - 0o17")).toBe(240n);
    expect(calc("-5 % 3")).toBe(-2n);
  });
  it("reports errors", () => {
    expect(() => calc("1 / 0")).toThrow();
    expect(() => calc("(1 + 2")).toThrow();
    expect(() => calc("1 $ 2")).toThrow();
  });
  it("shows two's complement views", () => {
    const v = views(-1n);
    expect(v.i8).toBe("-1");
    expect(v.u64).toBe("0xFFFFFFFFFFFFFFFF");
    expect(views(0x41424344n).chars).toBe("ABCD");
  });
});

import { normalizeBinding } from "./keys";

describe("key bindings", () => {
  it("normalises modifier order", () => {
    expect(normalizeBinding("Shift+Mod+S")).toBe("Mod+Shift+S");
    expect(normalizeBinding("Alt+Mod+K")).toBe("Mod+Alt+K");
    expect(normalizeBinding("Mod+K Mod+S")).toBe("Mod+K Mod+S");
  });
});
