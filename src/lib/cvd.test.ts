import { describe, expect, it } from "vitest";
import { safeColor } from "./cvd";

describe("colour-vision-safe palette", () => {
  it("separates red and green and keeps greys", () => {
    expect(safeColor("#DC2626")).toBe("#D55E00");
    expect(safeColor("#1F9D74")).toBe("#009E73");
    expect(safeColor("#6B6B73")).toBe("#6B6B73");
    expect(safeColor("rgba(1,2,3,0.5)")).toBe("rgba(1,2,3,0.5)");
  });
});
