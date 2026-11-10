import { describe, expect, it } from "vitest";
import { detectFormat } from "./detect";
import { evalExpr } from "./expr";
import { byteKind, fmtOffset, fmtSize, hexN, offsetWidth, parseHexBytes } from "./format";

const ctx = { cursor: 0x100, length: 0x1000, bytesPerRow: 16, labels: { header: 0x40 } };

describe("goto expressions", () => {
  it("parses hex by default and decimal with suffix", () => {
    expect(evalExpr("1F", ctx).value).toBe(0x1f);
    expect(evalExpr("0x1f", ctx).value).toBe(0x1f);
    expect(evalExpr("128d", ctx).value).toBe(128);
    expect(evalExpr("0d10", ctx).value).toBe(10);
  });
  it("supports names, labels and arithmetic", () => {
    expect(evalExpr("end-0x10", ctx).value).toBe(0xff0);
    expect(evalExpr("cursor+4*2", ctx).value).toBe(0x108);
    expect(evalExpr("header + 4", ctx).value).toBe(0x44);
    expect(evalExpr("(10+10)×2", ctx).value).toBe(0x40);
    expect(evalExpr("beef", ctx).value).toBe(0xbeef);
  });
  it("handles relative, from-end, percent and units", () => {
    expect(evalExpr("+10", ctx)).toEqual({ value: 0x110, kind: "relative" });
    expect(evalExpr("-10", ctx).value).toBe(0xf0);
    expect(evalExpr("^10", ctx)).toEqual({ value: 0xff0, kind: "fromEnd" });
    expect(evalExpr("50%", ctx)).toEqual({ value: 0x800, kind: "percent" });
    expect(evalExpr("s:2", ctx).value).toBe(1024);
    expect(evalExpr("r:3", ctx).value).toBe(48);
    expect(evalExpr("p:1", ctx).value).toBe(4096);
  });
  it("reports errors", () => {
    expect(() => evalExpr("", ctx)).toThrow();
    expect(() => evalExpr("(1+2", ctx)).toThrow();
    expect(() => evalExpr("nope", ctx)).toThrow(/nope/);
    expect(() => evalExpr("4/0", ctx)).toThrow();
  });
});

describe("formatting", () => {
  it("formats numbers", () => {
    expect(hexN(255, 4)).toBe("00FF");
    expect(fmtSize(512)).toBe("512 Б");
    expect(fmtSize(1536)).toBe("1,5 КБ");
    expect(fmtOffset(0x21, "hex", 8)).toEqual({ z: "000000", v: "21" });
    expect(fmtOffset(0, "hex", 8)).toEqual({ z: "0000000", v: "0" });
    expect(offsetWidth(0xffffffffff, "hex")).toBe(10);
  });
  it("classifies bytes", () => {
    expect([0, 0x41, 0x0a, 0x80, 0xff].map(byteKind)).toEqual([0, 1, 2, 3, 4]);
  });
  it("parses hex byte strings", () => {
    expect(parseHexBytes("de ad,BE:ef")).toEqual([0xde, 0xad, 0xbe, 0xef]);
    expect(parseHexBytes("abc")).toBeNull();
  });
});

describe("format detection", () => {
  it("detects by magic and falls back to extension", () => {
    expect(detectFormat([0x89, 0x50, 0x4e, 0x47, 0x0d]).badge).toBe("PNG");
    expect(detectFormat([0x4d, 0x5a, 0x90]).badge).toBe("PE");
    expect(detectFormat([1, 2, 3], "fw.img").badge).toBe("IMG");
    expect(detectFormat([1, 2, 3]).badge).toBe("BIN");
  });
});
