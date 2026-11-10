import { describe, expect, it, vi } from "vitest";

let data = new Uint8Array(8192).map((_, i) => i & 0xff);
vi.mock("../ipc/api", () => ({
  api: {
    read: vi.fn(async (_id: number, off: number, len: number) => data.slice(off, off + len)),
    changes: vi.fn(async () => []),
  },
}));

const { DocCache } = await import("./cache");

const tick = () => new Promise((r) => setTimeout(r, 0));

describe("page cache", () => {
  it("keeps showing old bytes until an edited page is refetched", async () => {
    const c = new DocCache(1);
    c.sync(data.length, 1);
    c.ensure(0, 4096);
    await tick();
    expect(c.byte(10)).toBe(10);
    // Overwrite byte 10 in the "document", then report the edit.
    data = data.slice();
    data[10] = 0xee;
    c.applyChange({ start: 10, removed: 1, inserted: 1 }, data.length, 2);
    // No blank frame: the stale page is still drawn.
    expect(c.byte(10)).toBe(10);
    c.ensure(0, 4096);
    await tick();
    expect(c.byte(10)).toBe(0xee);
  });

  it("keeps bytes on screen across a full refresh", async () => {
    const c = new DocCache(2);
    c.sync(data.length, 1);
    c.ensure(0, 8192);
    await tick();
    c.sync(data.length, 5);
    expect(c.byte(5000)).toBe(data[5000]);
  });
});
