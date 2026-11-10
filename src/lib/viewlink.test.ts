import { describe, expect, it } from "vitest";
import { parseView, type ViewState, viewToLink } from "./viewlink";

describe("view links", () => {
  it("round-trips through a link and JSON", () => {
    const v: ViewState = {
      v: 1,
      name: "файл.bin",
      len: 100,
      cursor: 5,
      anchor: 2,
      topRow: 0,
      marks: [{ name: "заголовок", start: 0, end: 4 }],
      folds: [],
    };
    expect(parseView(viewToLink(v))).toEqual(v);
    expect(parseView(JSON.stringify(v))).toEqual(v);
    expect(() => parseView("{}")).toThrow();
  });
});
