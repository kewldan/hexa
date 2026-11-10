// Smoke test of the Node.js build (`npm run build:node && npm test`).
"use strict";

const assert = require("node:assert/strict");
const hexa = require("../pkg/node/hexa_wasm.js");

const enc = new TextEncoder();
const dec = new TextDecoder();

// HexDocument: edits, undo / redo.
const doc = hexa.HexDocument.fromBytes(enc.encode("hello world"));
assert.equal(doc.length, 11);
doc.overwrite(0, enc.encode("J"));
doc.insert(5, enc.encode(","));
doc.delete(6, 1);
doc.replace(6, 5, enc.encode("there!"));
assert.equal(dec.decode(doc.toBytes()), "Jello,there!");
assert.equal(dec.decode(doc.read(0, 5)), "Jello");
assert.ok(doc.undo());
assert.equal(dec.decode(doc.toBytes()), "Jello,world");
assert.ok(doc.redo());
assert.deepEqual(Array.from(doc.search("text", "there")), [6]);
assert.throws(() => doc.read(-1, 1), /non-negative integer/);
doc.free();

// Search.
const data = enc.encode("xxABCDxxabcdxx");
assert.deepEqual(Array.from(hexa.search(data, "hex", "41 42 ?? 44")), [2]);
assert.deepEqual(Array.from(hexa.search(data, "text", "abcd", { caseSensitive: false })), [2, 8]);
assert.deepEqual(Array.from(hexa.search(data, "regex", "[a-d]{4}")), [8]);
assert.throws(() => hexa.search(data, "hex", "4G"), Error);

// Hashes.
const h = hexa.hash(enc.encode("123456789"), ["crc:CRC-32/ISO-HDLC", "sha256", "xxh64"]);
assert.equal(h[0].value.toLowerCase(), "cbf43926");
assert.equal(h[1].value, "15e2b0d3c33891ebb0f1ef609ec419420c20e320ce94c65fbc8c3312448eb225");
assert.ok(hexa.hashAlgorithms().some((a) => a.id === "ssdeep"));
assert.ok(hexa.entropy(new Uint8Array([0, 1, 2, 3])) > 1.99);

// Diff and patches.
const a = enc.encode("the quick brown fox");
const b = enc.encode("the quick red fox!");
const d = hexa.diff(a, b);
assert.ok(d.stats.differences > 0);
assert.equal(typeof d.regions[0].a.start, "number");
assert.equal(hexa.diff(a, a, { bytewise: true }).regions.length, 0);
for (const f of ["ips", "ups", "bps", "vcdiff", "bsdiff"]) {
  const p = hexa.createPatch(f, a, b);
  assert.deepEqual(Buffer.from(hexa.applyPatch(a, p)), Buffer.from(b), f);
}

// Transform (AES pulls getrandom: exercises the wasm_js backend linkage).
const up = hexa.transform(enc.encode("hello"), '[{"kind":"bitwise","op":"xor","operand":"0x20"}]');
assert.equal(dec.decode(up), "HELLO");

console.log(
  JSON.stringify({
    version: hexa.version(),
    doc: "Jello,there!",
    hexSearch: Array.from(hexa.search(data, "hex", "41 42 ?? 44")),
    crc32: h[0].value,
    sha256: h[1].value,
    diffRegions: d.regions.length,
    transform: dec.decode(up),
  }),
);
console.log("hexa-wasm smoke test: OK");
