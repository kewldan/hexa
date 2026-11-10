import { describe, expect, it } from "vitest";
import type { EncodingInfo } from "../ipc/api";
import { computeLayout, hitHexCell, hitTextByte } from "./layout";
import { encodeChar, TextDecoder2 } from "./text";

const s = {
  bytesPerRow: 16 as const,
  group: 8 as const,
  cellView: "hex" as const,
  cellWidth: 1 as const,
  columns: ["offset", "hex", "text"] as ("offset" | "hex" | "text")[],
  offsetBase: "hex" as const,
};

describe("layout", () => {
  it("places cells, groups and hits them back", () => {
    const l = computeLayout({ s, cw: 8, rowH: 22, width: 2000, offsetDigits: 8 });
    expect(l.bpr).toBe(16);
    expect(l.cellX.length).toBe(16);
    // The group gap is wider than the normal gap.
    expect(l.cellX[8] - l.cellX[7]).toBeGreaterThan(l.cellX[1] - l.cellX[0]);
    for (let i = 0; i < 16; i++) expect(hitHexCell(l, l.hexX + l.cellX[i] + 3)).toBe(i);
    expect(hitTextByte(l, l.textX + l.textCharW * 5 + 1)).toBe(5);
    expect(hitHexCell(l, 2)).toBe(-1);
  });
  it("fits automatic widths", () => {
    const narrow = computeLayout({
      s: { ...s, bytesPerRow: "auto" },
      cw: 8,
      rowH: 22,
      width: 500,
      offsetDigits: 8,
    });
    const wide = computeLayout({
      s: { ...s, bytesPerRow: "auto" },
      cw: 8,
      rowH: 22,
      width: 1400,
      offsetDigits: 8,
    });
    expect(narrow.bpr % 8).toBe(0);
    expect(wide.bpr).toBeGreaterThan(narrow.bpr);
    expect(wide.totalW).toBeLessThanOrEqual(1400);
    const p2 = computeLayout({
      s: { ...s, bytesPerRow: "pow2", group: 1 },
      cw: 8,
      rowH: 22,
      width: 900,
      offsetDigits: 8,
    });
    expect(Math.log2(p2.bpr) % 1).toBe(0);
  });
  it("supports multi-byte cells", () => {
    const l = computeLayout({
      s: { ...s, cellWidth: 4, cellView: "dec" },
      cw: 8,
      rowH: 22,
      width: 2000,
      offsetDigits: 8,
    });
    expect(l.cellX.length).toBe(4);
    expect(l.cellChars).toBe(10);
  });
});

const enc = (kind: EncodingInfo["kind"]): EncodingInfo => ({
  id: kind,
  name: kind,
  group: "",
  kind,
});

describe("text decoding", () => {
  const bytes = (arr: number[]) => (o: number) => arr[o];
  it("aligns multi-byte UTF-8 characters to their first byte", () => {
    const d = new TextDecoder2(enc("utf8"), null, "dot");
    const data = [0x41, 0xd0, 0x96, 0x0a, 0x96];
    const g = data.map((_, i) => d.glyph(bytes(data), i));
    expect(g.map((x) => x.ch)).toEqual(["A", "Ж", "", "·", "·"]);
  });
  it("decodes UTF-16 with surrogates and control pictures", () => {
    const d = new TextDecoder2(enc("utf16Le"), null, "pictures");
    const data = [0x3d, 0xd8, 0x00, 0xde, 0x0a, 0x00];
    const g = data.map((_, i) => d.glyph(bytes(data), i).ch);
    expect(g).toEqual(["😀", "", "", "", "␊", ""]);
  });
  it("uses single-byte tables", () => {
    const table: (string | null)[] = Array.from({ length: 256 }, (_, i) =>
      i < 0x80 ? String.fromCharCode(i) : null,
    );
    table[0xc0] = "А";
    const d = new TextDecoder2(enc("singleByte"), table, "dot");
    expect(d.glyph(bytes([0xc0]), 0)).toEqual({ ch: "А", dim: false });
    expect(d.glyph(bytes([0x81]), 0).dim).toBe(true);
    expect(encodeChar("А", enc("singleByte"), table)).toEqual([0xc0]);
    expect(encodeChar("Ж", enc("utf16Be"), null)).toEqual([0x04, 0x16]);
  });
});
