// In-browser stand-in for the Rust backend, used by `vite dev` outside Tauri
// and by end-to-end tests. It keeps one plain byte array per document with a
// linear undo stack — enough to exercise the UI, not a model of the core.
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { mockSources } from "./mockSources";
import { SCRIPT_COMMANDS, scriptMock } from "./scriptMock";

interface MockDoc {
  id: number;
  name: string;
  path: string | null;
  base: Uint8Array;
  data: Uint8Array;
  mode: "readOnly" | "overwrite" | "insert";
  fixed: boolean;
  version: number;
  undo: { data: Uint8Array; label: string; change: Change }[];
  redo: { data: Uint8Array; label: string; change: Change }[];
  saved: Uint8Array;
}

interface Change {
  start: number;
  removed: number;
  inserted: number;
}

const docs = new Map<number, MockDoc>();
let nextId = 1;
let clip: Uint8Array | null = null;
// biome-ignore lint/suspicious/noExplicitAny: test double
let marks: any[] = [];
let nextMark = 1;
let clipText: string | null = null;

function sample(): Uint8Array {
  const out: number[] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, 0x49, 0x48, 0x44, 0x52,
  ];
  out.push(...new TextEncoder().encode("Hexa mock document — Привет, байты! "));
  let x = 12345;
  while (out.length < 64 * 1024) {
    x = (x * 1103515245 + 12345) & 0x7fffffff;
    out.push(out.length % 97 < 8 ? 0 : (x >> 16) & 0xff);
  }
  return Uint8Array.from(out);
}

function info(d: MockDoc) {
  return {
    id: d.id,
    name: d.name,
    path: d.path,
    len: d.data.length,
    baseLen: d.base.length,
    mode: d.mode,
    fixedSize: d.fixed,
    modified: !eq(d.data, d.saved),
    canUndo: d.undo.length > 0,
    canRedo: d.redo.length > 0,
    writeDenied: false,
    version: d.version,
    pieceCount: d.undo.length + 1,
    follow: false,
    autoReload: false,
  };
}

function eq(a: Uint8Array, b: Uint8Array) {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

function err(kind: string, message: string) {
  return Promise.reject({ kind, message });
}

let typing: { id: number; end: number; at: number } | null = null;

function commit(d: MockDoc, data: Uint8Array, label: string, change: Change, coalesce = false) {
  if (d.mode === "readOnly") throw { kind: "readOnly", message: "read-only" };
  if (!(coalesce && d.undo.length)) d.undo.push({ data: d.data, label, change });
  d.redo = [];
  d.data = data;
  d.version++;
  return { change, info: info(d) };
}

function splice(src: Uint8Array, start: number, end: number, ins: ArrayLike<number>): Uint8Array {
  const out = new Uint8Array(src.length - (end - start) + ins.length);
  out.set(src.subarray(0, start));
  out.set(Array.from(ins), start);
  out.set(src.subarray(end), start + ins.length);
  return out;
}

function fillBytes(
  fill: { kind: string; value?: number; bytes?: number[]; seed?: number },
  n: number,
) {
  const out = new Uint8Array(n);
  for (let i = 0; i < n; i++) {
    if (fill.kind === "byte") out[i] = fill.value ?? 0;
    else if (fill.kind === "pattern") out[i] = fill.bytes?.[i % (fill.bytes.length || 1)] ?? 0;
    else out[i] = (i * 2654435761 + (fill.seed ?? 0)) & 0xff;
  }
  return out;
}

function changes(d: MockDoc, start: number, end: number) {
  const out: { start: number; end: number; kind: string }[] = [];
  if (d.data.length === d.base.length) {
    let s = -1;
    for (let i = start; i < Math.min(end, d.data.length); i++) {
      const diff = d.data[i] !== d.base[i];
      if (diff && s < 0) s = i;
      if (!diff && s >= 0) {
        out.push({ start: s, end: i, kind: "modified" });
        s = -1;
      }
    }
    if (s >= 0) out.push({ start: s, end: Math.min(end, d.data.length), kind: "modified" });
  }
  return out;
}

function hexPattern(p: string): (number | null)[] {
  const clean = p.replace(/0x/gi, "").replace(/[\s,]/g, "");
  const out: (number | null)[] = [];
  for (let i = 0; i + 1 < clean.length; i += 2) {
    const pair = clean.slice(i, i + 2);
    out.push(pair.includes("?") ? null : Number.parseInt(pair, 16));
  }
  return out;
}

function compile(q: { kind: string; pattern?: string; text?: string; caseSensitive?: boolean }) {
  if (q.kind === "hex") return hexPattern(q.pattern ?? "");
  if (q.kind === "text") return Array.from(new TextEncoder().encode(q.text ?? ""));
  throw { kind: "invalid", message: "mock supports hex and text search" };
}

function findAll(d: MockDoc, pat: (number | null)[], ci: boolean) {
  const out: { offset: number; len: number }[] = [];
  const lower = (b: number) => (ci && b >= 0x41 && b <= 0x5a ? b + 32 : b);
  outer: for (let i = 0; i + pat.length <= d.data.length; i++) {
    for (let k = 0; k < pat.length; k++) {
      const p = pat[k];
      if (p !== null && lower(d.data[i + k]) !== lower(p)) continue outer;
    }
    out.push({ offset: i, len: pat.length });
  }
  return out;
}

function inspect(bytes: Uint8Array, endian: string) {
  const le = endian === "little";
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const has = (n: number) => bytes.length >= n;
  const row = (ty: string, label: string, value: string | null, size: number) => ({
    ty,
    label,
    value,
    size,
  });
  const b = bytes[0];
  return [
    row("binary", "binary", has(1) ? b.toString(2).padStart(8, "0") : null, 1),
    row("octal", "octal", has(1) ? b.toString(8).padStart(3, "0") : null, 1),
    row("hex", "hex", has(1) ? b.toString(16).toUpperCase().padStart(2, "0") : null, 1),
    row("i8", "int8", has(1) ? String(dv.getInt8(0)) : null, 1),
    row("u8", "uint8", has(1) ? String(b) : null, 1),
    row("i16", "int16", has(2) ? String(dv.getInt16(0, le)) : null, 2),
    row("u16", "uint16", has(2) ? String(dv.getUint16(0, le)) : null, 2),
    row("i32", "int32", has(4) ? String(dv.getInt32(0, le)) : null, 4),
    row("u32", "uint32", has(4) ? String(dv.getUint32(0, le)) : null, 4),
    row("i64", "int64", has(8) ? String(dv.getBigInt64(0, le)) : null, 8),
    row("u64", "uint64", has(8) ? String(dv.getBigUint64(0, le)) : null, 8),
    row("f32", "float32", has(4) ? String(dv.getFloat32(0, le)) : null, 4),
    row("f64", "float64", has(8) ? String(dv.getFloat64(0, le)) : null, 8),
    row("ascii", "ASCII", has(1) && b < 0x80 ? `'${String.fromCharCode(b)}'` : null, 1),
  ];
}

const ENCODINGS = [
  { id: "ascii", name: "ASCII", group: "Unicode", kind: "singleByte" },
  { id: "latin1", name: "Latin-1", group: "Western", kind: "singleByte" },
  { id: "utf-8", name: "UTF-8", group: "Unicode", kind: "utf8" },
  { id: "utf-16le", name: "UTF-16 LE", group: "Unicode", kind: "utf16Le" },
];

function hexText(bytes: Uint8Array) {
  return Array.from(bytes, (b) => b.toString(16).toUpperCase().padStart(2, "0")).join(" ");
}

export function installMock() {
  mockWindows("main");
  const store = (k: string, v?: unknown) => {
    if (v === undefined) return JSON.parse(localStorage.getItem(`hexa-mock-${k}`) ?? "null");
    localStorage.setItem(`hexa-mock-${k}`, JSON.stringify(v));
  };
  mockIPC(
    // biome-ignore lint/suspicious/noExplicitAny: test double
    async (cmd: string, a: any): Promise<any> => {
      const doc = (): MockDoc => {
        const d = docs.get(a.id);
        if (!d) throw { kind: "notFound", message: "no doc" };
        return d;
      };
      if (
        cmd.startsWith("doc_") &&
        !["doc_type", "doc_read", "doc_changes", "doc_info", "doc_history"].includes(cmd)
      )
        typing = null;
      switch (cmd) {
        case "settings_get":
          return store("settings") ?? {};
        case "settings_set":
          return store("settings", a.value);
        case "recent_list":
        case "recovery_list":
        case "app_initial_files":
          return [];
        case "encodings":
          return ENCODINGS;
        case "encoding_table":
          return Array.from({ length: 256 }, (_, i) =>
            a.id === "latin1" || i < 0x80 ? String.fromCharCode(i) : null,
          );
        case "doc_new":
        case "doc_open": {
          const data =
            a.kind?.kind === "bytes"
              ? Uint8Array.from(a.kind.bytes)
              : a.kind?.kind === "sized"
                ? fillBytes(a.kind.fill, a.kind.len)
                : cmd === "doc_open"
                  ? sample()
                  : new Uint8Array();
          const id = nextId++;
          const d: MockDoc = {
            id,
            name: cmd === "doc_open" ? String(a.path).split("/").pop()! : "Untitled",
            path: cmd === "doc_open" ? a.path : null,
            base: cmd === "doc_open" ? data : new Uint8Array(),
            data,
            mode: a.opts?.mode ?? "overwrite",
            fixed: false,
            version: 1,
            undo: [],
            redo: [],
            saved: cmd === "doc_open" ? data : new Uint8Array(),
          };
          docs.set(id, d);
          return info(d);
        }
        case "doc_attach":
        case "doc_info":
          return info(doc());
        case "doc_release":
          docs.delete(a.id);
          return null;
        case "doc_read": {
          const d = doc();
          return Array.from(d.data.subarray(a.offset, a.offset + a.len));
        }
        case "doc_changes":
          return changes(doc(), a.start, a.end);
        case "doc_type": {
          const d = doc();
          const insert = d.mode === "insert" && !a.amend;
          const coalesce =
            typing?.id === d.id && (a.amend ? a.pos < typing.end : a.pos === typing.end);
          const data = insert
            ? splice(d.data, a.pos, a.pos, [a.byte])
            : splice(d.data, a.pos, Math.min(a.pos + 1, d.data.length), [a.byte]);
          const r = commit(
            d,
            data,
            "Typing",
            { start: a.pos, removed: insert ? 0 : 1, inserted: 1 },
            coalesce,
          );
          typing = { id: d.id, end: Math.max(a.pos + 1, typing?.end ?? 0), at: Date.now() };
          return r;
        }
        case "doc_edit": {
          const d = doc();
          const o = a.op;
          switch (o.op) {
            case "overwrite":
              return commit(
                d,
                splice(d.data, o.pos, Math.min(o.pos + o.bytes.length, d.data.length), o.bytes),
                "Overwrite",
                { start: o.pos, removed: o.bytes.length, inserted: o.bytes.length },
              );
            case "insert":
              return commit(d, splice(d.data, o.pos, o.pos, o.bytes), "Insert", {
                start: o.pos,
                removed: 0,
                inserted: o.bytes.length,
              });
            case "delete":
              return commit(d, splice(d.data, o.start, o.end, []), "Delete", {
                start: o.start,
                removed: o.end - o.start,
                inserted: 0,
              });
            case "replace":
              return commit(d, splice(d.data, o.start, o.end, o.bytes), "Replace", {
                start: o.start,
                removed: o.end - o.start,
                inserted: o.bytes.length,
              });
            case "fill":
              return commit(
                d,
                splice(d.data, o.start, o.end, fillBytes(o.fill, o.end - o.start)),
                "Fill",
                { start: o.start, removed: o.end - o.start, inserted: o.end - o.start },
              );
            case "insertFill":
              return commit(
                d,
                splice(d.data, o.pos, o.pos, fillBytes(o.fill, o.len)),
                "Insert bytes",
                { start: o.pos, removed: 0, inserted: o.len },
              );
            case "reverse": {
              const part = d.data.slice(o.start, o.end).reverse();
              return commit(d, splice(d.data, o.start, o.end, part), "Reverse", {
                start: o.start,
                removed: part.length,
                inserted: part.length,
              });
            }
            default:
              return err("unsupported", `mock: ${o.op}`);
          }
        }
        case "doc_undo": {
          const d = doc();
          const e = d.undo.pop();
          if (!e) return { change: null, info: info(d) };
          d.redo.push({ data: d.data, label: e.label, change: e.change });
          d.data = e.data;
          d.version++;
          return {
            change: {
              start: e.change.start,
              removed: e.change.inserted,
              inserted: e.change.removed,
            },
            info: info(d),
          };
        }
        case "doc_redo": {
          const d = doc();
          const e = d.redo.pop();
          if (!e) return { change: null, info: info(d) };
          d.undo.push({ data: d.data, label: e.label, change: e.change });
          d.data = e.data;
          d.version++;
          return { change: e.change, info: info(d) };
        }
        case "doc_history": {
          const d = doc();
          const items: {
            id: number;
            parent: number | null;
            label: string;
            timeMs: number;
            current: boolean;
            saved: boolean;
            onRedoPath: boolean;
          }[] = [
            {
              id: 0,
              parent: null,
              label: "Open",
              timeMs: Date.now(),
              current: d.undo.length === 0,
              saved: true,
              onRedoPath: true,
            },
          ];
          for (const [i, u] of d.undo.entries()) {
            items.push({
              id: i + 1,
              parent: i,
              label: u.label,
              timeMs: Date.now(),
              current: i === d.undo.length - 1,
              saved: false,
              onRedoPath: true,
            });
          }
          return items;
        }
        case "doc_history_goto":
          return { change: null, info: info(doc()) };
        case "doc_set_mode": {
          const d = doc();
          d.mode = a.mode;
          return info(d);
        }
        case "doc_set_fixed": {
          const d = doc();
          d.fixed = a.fixed;
          return info(d);
        }
        case "doc_save":
        case "doc_save_as": {
          const d = doc();
          a.channel?.onmessage?.({ done: d.data.length, total: d.data.length });
          d.saved = d.data;
          d.base = d.data;
          d.version++;
          return { report: { strategy: "atomic", bytesWritten: d.data.length }, info: info(d) };
        }
        case "doc_props": {
          const d = doc();
          return {
            path: d.path,
            name: d.name,
            size: d.data.length,
            diskSize: d.base.length,
            createdMs: null,
            modifiedMs: Date.now(),
            accessedMs: null,
            readonly: false,
            permissions: "644",
            owner: null,
            pieces: 1,
            history: d.undo.length + 1,
            diskChanged: false,
          };
        }
        case "inspect": {
          const d = doc();
          return inspect(d.data.slice(a.pos, a.pos + 16), a.endian);
        }
        case "search_next": {
          const d = doc();
          const all = findAll(d, compile(a.query), !a.query.caseSensitive);
          const hit = a.backwards
            ? [...all].reverse().find((m) => m.offset < a.cursor)
            : all.find((m) => m.offset >= a.cursor);
          return hit ?? (a.wrap ? ((a.backwards ? all[all.length - 1] : all[0]) ?? null) : null);
        }
        case "search_all": {
          const d = doc();
          const ch = a.channel;
          try {
            const all = findAll(
              d,
              compile(a.query),
              a.query.kind === "text" && !a.query.caseSensitive,
            );
            setTimeout(() => {
              ch.onmessage({ type: "batch", matches: all.slice(0, a.max) });
              ch.onmessage({
                type: "done",
                count: Math.min(all.length, a.max),
                truncated: all.length > a.max,
              });
            }, 20);
          } catch (e) {
            setTimeout(() => ch.onmessage({ type: "error", error: e }), 0);
          }
          return 1;
        }
        case "hash_algorithms":
          return [
            { id: "sum8", name: "Sum-8", group: "checksum" },
            { id: "crc:CRC-32/ISO-HDLC", name: "CRC-32/ISO-HDLC", group: "crc" },
            { id: "sha256", name: "SHA-256", group: "crypto" },
          ];
        case "hash_run": {
          const d = doc();
          const part = d.data.subarray(a.start, a.end);
          const sum = part.reduce((s, b) => (s + b) & 0xff, 0);
          setTimeout(
            () =>
              a.channel.onmessage({
                type: "done",
                results: a.algorithms.map((id: string) => ({
                  id,
                  value: `mock-${id}-${sum.toString(16)}`,
                })),
              }),
            50,
          );
          return 2;
        }
        case "hash_quick":
          return "MOCKCRC0";
        case "clip_copy": {
          const d = doc();
          clip = d.data.slice(a.start, a.end);
          clipText = hexText(clip);
          return { text: clipText, len: clip.length, written: false };
        }
        case "copy_as":
          return hexText(doc().data.slice(a.start, a.end));
        case "clip_info":
          return clip ? { len: clip.length, text: clipText } : null;
        case "clip_paste": {
          const d = doc();
          if (!clip) return err("invalid", "empty clipboard");
          const end = a.end ?? (a.overwrite ? Math.min(a.pos + clip.length, d.data.length) : a.pos);
          return commit(d, splice(d.data, a.pos, end, clip), "Paste", {
            start: a.pos,
            removed: end - a.pos,
            inserted: clip.length,
          });
        }
        case "diff_run": {
          const da = docs.get(a.a);
          const db = docs.get(a.b);
          if (!da || !db) throw { kind: "notFound", message: "no doc" };
          const regions = mockDiff(da.data, db.data);
          const kinds = { equal: 0, changed: 0, inserted: 0, deleted: 0 } as Record<string, number>;
          for (const r of regions) kinds[r.kind]++;
          const diffBytes = regions.reduce((n, r) => n + Math.max(r.a.len, r.b.len), 0);
          const total = Math.max(da.data.length, db.data.length) || 1;
          const ks = (k: string) => ({ count: kinds[k], bytesA: 0, bytesB: 0 });
          setTimeout(
            () =>
              a.channel.onmessage({
                type: "done",
                result: {
                  regions,
                  truncated: false,
                  stats: {
                    lenA: da.data.length,
                    lenB: db.data.length,
                    equalBytes: total - diffBytes,
                    matchPercent: (100 * (total - diffBytes)) / total,
                    equal: ks("equal"),
                    changed: ks("changed"),
                    inserted: ks("inserted"),
                    deleted: ks("deleted"),
                    differences: diffBytes,
                  },
                },
              }),
            10,
          );
          return 2;
        }
        case "diff_apply": {
          const to = docs.get(a.to);
          const from = docs.get(a.from);
          if (!to || !from) throw { kind: "notFound", message: "no doc" };
          let data = to.data;
          const rs = [...a.regions].sort((x: MockRegion, y: MockRegion) => y.a.start - x.a.start);
          for (const r of rs)
            data = splice(
              data,
              r.a.start,
              r.a.start + r.a.len,
              from.data.slice(r.b.start, r.b.start + r.b.len),
            );
          return commit(to, data, "Merge regions", {
            start: 0,
            removed: to.data.length,
            inserted: data.length,
          });
        }
        case "xfrm_preview": {
          const d = doc();
          const out = mockTransform(d.data.slice(a.start, a.end), a.recipe.steps);
          return {
            output: [...out.slice(0, a.maxOut)],
            totalLen: out.length,
            truncated: out.length > a.maxOut,
            inputTruncated: false,
          };
        }
        case "xfrm_apply": {
          const d = doc();
          const out = mockTransform(d.data.slice(a.start, a.end), a.recipe.steps);
          return commit(d, splice(d.data, a.start, a.end, out), "Transform", {
            start: a.start,
            removed: a.end - a.start,
            inserted: out.length,
          });
        }
        case "xfrm_invert":
          return { steps: [...a.recipe.steps].reverse() };
        case "xfrm_xor_guess":
          return {
            single: [{ key: "00", score: 0.5, method: "zeroBytes", printableRatio: 0.5 }],
            repeating: [],
            sizes: [],
          };
        case "anl_overview": {
          const d = doc();
          const bytes = d.data.slice(a.start, a.end);
          const counts = new Array(256).fill(0);
          for (const b of bytes) counts[b]++;
          const n = Math.max(1, bytes.length);
          const entropy = -counts.reduce((s, c) => (c ? s + (c / n) * Math.log2(c / n) : s), 0);
          return {
            histogram: {
              counts,
              total: bytes.length,
              distinct: counts.filter(Boolean).length,
              entropy,
              mean: bytes.reduce((s, b) => s + b, 0) / n,
              chiSquare: 0,
              mostCommon: counts.indexOf(Math.max(...counts)),
              leastCommon: null,
            },
            entropy: {
              start: a.start,
              window: 256,
              step: 256,
              values: Array.from({ length: 16 }, () => entropy),
            },
            regions: [
              { start: a.start, end: a.end, kind: "data", detail: null, confidence: 1, entropy },
            ],
          };
        }
        case "anl_strings": {
          const d = doc();
          const out: { offset: number; len: number; encoding: string; text: string }[] = [];
          let s = -1;
          for (let i = a.start; i <= a.end; i++) {
            const b = i < a.end ? d.data[i] : 0;
            const ok = b >= 0x20 && b < 0x7f;
            if (ok && s < 0) s = i;
            if (!ok && s >= 0) {
              if (i - s >= (a.opts.minLen ?? 4))
                out.push({
                  offset: s,
                  len: i - s,
                  encoding: "ascii",
                  text: String.fromCharCode(...d.data.slice(s, i)),
                });
              s = -1;
            }
          }
          return { strings: out, truncated: false };
        }
        case "anl_embedded":
          return [];
        case "re_info":
          return { info: null, error: "not an executable", findings: [] };
        case "re_arches":
          return [{ arch: "x86_64", label: "x86-64", canAssemble: true, hasNop: true }];
        case "re_disasm": {
          const d = doc();
          return Array.from(d.data.slice(a.offset, a.offset + 16)).map((b, i) => ({
            address: a.address + i,
            size: 1,
            bytes: [b],
            mnemonic: ".byte",
            operands: `0x${b.toString(16).padStart(2, "0")}`,
            groups: [],
            branchTarget: null,
            memoryReference: null,
            isData: true,
          }));
        }
        case "re_tools":
          return [];
        case "tpl_library":
          return [
            {
              id: "png",
              name: "PNG",
              description: "Portable Network Graphics",
              extensions: ["png"],
            },
            { id: "zip", name: "ZIP", description: "ZIP archive", extensions: ["zip"] },
          ];
        case "tpl_detect":
          return ["png"];
        case "tpl_apply":
        case "tpl_reapply":
          return mockTemplate(a.key ?? 1);
        case "tpl_children":
          return (MOCK_TPL_CHILDREN[a.node] ?? []).slice(a.start, a.end);
        case "tpl_node_at": {
          const hitNode = a.offset < 8 ? 2 : a.offset < 12 ? 4 : a.offset < 16 ? 5 : null;
          return hitNode === null
            ? null
            : { path: hitNode === 2 ? [1, 2] : [1, 3, hitNode], elementIndex: null };
        }
        case "tpl_ranges_at":
          return a.offset < 8
            ? [
                [0, 8],
                [0, 33],
              ]
            : [
                [8, 12],
                [8, 33],
                [0, 33],
              ];
        case "tpl_diagnostics":
          return /struct\s*\{/.test(a.text) && !/\}/.test(a.text)
            ? [
                {
                  line: 1,
                  col: 1,
                  endLine: 1,
                  endCol: 2,
                  severity: "error",
                  message: "expected `}`",
                },
              ]
            : [];
        case "tpl_suggest":
          return [{ ty: "u32le", value: "1196314761", confidence: 0.4, reason: "mock" }];
        case "tpl_remove":
          return null;
        case "app_info":
          return { version: "0.1.0", portable: false, configDir: "/mock" };
        case "app_print":
          return null;
        case "files_search":
          setTimeout(
            () => a.channel.onmessage({ type: "done", files: 0, hits: 0, truncated: false }),
            10,
          );
          return 3;
        case "find_runs": {
          const d = doc();
          const runs: { start: number; end: number; byte: number }[] = [];
          let i = 0;
          while (i < d.data.length && runs.length < (a.max ?? 1000)) {
            let j = i + 1;
            while (j < d.data.length && d.data[j] === d.data[i]) j++;
            if (j - i >= a.minLen) runs.push({ start: i, end: j, byte: d.data[i] });
            i = j;
          }
          return runs;
        }
        case "marks_list":
          return marks.filter((m) => m.doc === a.id).map(({ doc: _, ...m }) => m);
        case "marks_add": {
          const id = nextMark++;
          marks.push({ ...a.mark, id, doc: a.id });
          return id;
        }
        case "marks_update": {
          const i = marks.findIndex((m) => m.id === a.mark.id);
          if (i >= 0) marks[i] = { ...a.mark, doc: a.id };
          return i >= 0;
        }
        case "marks_remove": {
          const n = marks.length;
          marks = marks.filter((m) => m.id !== a.mark);
          return n !== marks.length;
        }
        case "marks_set":
          marks = marks
            .filter((m) => m.doc !== a.id)
            .concat(a.marks.map((m: object) => ({ ...m, id: nextMark++, doc: a.id })));
          return marks.filter((m) => m.doc === a.id);
        case "inspect_ext":
          return [];
        case "read_text_file":
          return "";
        case "write_text_file":
          return null;
        case "clip_paste_auto": {
          // The mock has no system clipboard formats: same-window copies only.
          const d = doc();
          if (!clip || clipText === null) return null;
          const end = a.end ?? (a.overwrite ? Math.min(a.pos + clip.length, d.data.length) : a.pos);
          return commit(d, splice(d.data, a.pos, end, clip), "Paste", {
            start: a.pos,
            removed: end - a.pos,
            inserted: clip.length,
          });
        }
        case "clip_parse":
          return [
            {
              kind: "text",
              label: "Text (UTF-8)",
              bytes: Array.from(new TextEncoder().encode(a.text)),
            },
          ];
        case "job_cancel":
        case "recent_pin":
        case "recent_remove":
          return null;
        case "plugin:clipboard-manager|write_text":
          clipText = a.text ?? a.data ?? null;
          return null;
        case "plugin:clipboard-manager|read_text":
          return clipText;
        default: {
          const sources = mockSources(cmd, a);
          if (sources !== undefined) return sources;
          if (SCRIPT_COMMANDS.has(cmd))
            return scriptMock(cmd, a, (id) => docs.get(id)?.data ?? null);
          if (cmd.startsWith("plugin:")) return null;
          console.warn("mock: unhandled", cmd, a);
          return null;
        }
      }
    },
    { shouldMockEvents: true },
  );
}

/** Opens a sample document so the editor is visible right away. */
export async function openMockSample() {
  const { openPaths } = await import("../state/actions");
  await openPaths(["/mock/sample.png"]);
}

interface MockRegion {
  a: { start: number; len: number };
  b: { start: number; len: number };
  kind: string;
}

/** Byte-wise comparison: runs of differing offsets plus the longer tail. */
function mockDiff(x: Uint8Array, y: Uint8Array): MockRegion[] {
  const out: MockRegion[] = [];
  const n = Math.min(x.length, y.length);
  let i = 0;
  while (i < n) {
    if (x[i] === y[i]) {
      i++;
      continue;
    }
    const s = i;
    while (i < n && x[i] !== y[i]) i++;
    out.push({ a: { start: s, len: i - s }, b: { start: s, len: i - s }, kind: "changed" });
  }
  if (x.length > n)
    out.push({ a: { start: n, len: x.length - n }, b: { start: n, len: 0 }, kind: "deleted" });
  if (y.length > n)
    out.push({ a: { start: n, len: 0 }, b: { start: n, len: y.length - n }, kind: "inserted" });
  return out;
}

/** The few recipe steps the browser mock understands. */
function mockTransform(
  input: Uint8Array,
  steps: { kind: string; [k: string]: unknown }[],
): Uint8Array {
  let data = Uint8Array.from(input);
  for (const st of steps) {
    if (st.kind === "not") data = data.map((b) => ~b & 0xff);
    else if (st.kind === "reverse") data = data.reverse();
    else if (st.kind === "xorKey") {
      const key = (String(st.key).match(/../g) ?? ["00"]).map((h) => Number.parseInt(h, 16));
      data = data.map((b, i) => b ^ key[i % key.length]);
    } else if (st.kind === "bitwise" && st.op === "xor") {
      const v = Number(st.operand) & 0xff;
      data = data.map((b) => b ^ v);
    } else throw { kind: "transform", message: `mock: ${st.kind} is not supported` };
  }
  return data;
}

function tplNode(
  id: number,
  parent: number | null,
  name: string,
  typeName: string,
  kind: string,
  offset: number,
  size: number,
  value: string | null,
  children: number[] = [],
) {
  return {
    id,
    parent,
    name,
    displayName: null,
    typeName,
    kind,
    offset,
    size,
    bitOffset: null,
    bitSize: null,
    value,
    children,
    comment: null,
    error: null,
    enumName: null,
    isArray: false,
    len: 0,
    elemIndex: null,
    hidden: false,
    lazy: false,
    childCount: children.length,
    color: { palette: id % 6 },
  };
}

/** A fixed PNG-like tree for the mock structure panel. */
const MOCK_TPL_CHILDREN: Record<number, ReturnType<typeof tplNode>[]> = {
  1: [
    tplNode(2, 1, "signature", "u8[8]", "array", 0, 8, "89 50 4E 47 0D 0A 1A 0A"),
    tplNode(3, 1, "ihdr", "Chunk", "struct", 8, 25, null, [4, 5]),
  ],
  3: [
    tplNode(4, 3, "length", "u32", "primitive", 8, 4, "13"),
    tplNode(5, 3, "type", "char[4]", "string", 12, 4, '"IHDR"'),
  ],
};

function mockTemplate(key: number) {
  return {
    key,
    name: "PNG",
    roots: [tplNode(1, null, "png", "PNG", "struct", 0, 33, null, [2, 3])],
    errors: [],
    log: [],
    truncated: false,
    nodeCount: 5,
    highlights: [
      { start: 0, end: 8, color: "#2F6BF0", node: 2 },
      { start: 8, end: 12, color: "#1F9D74", node: 4 },
      { start: 12, end: 16, color: "#E8890C", node: 5 },
    ],
  };
}
