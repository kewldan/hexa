# hexa-wasm

The Hexa hex editor engine compiled to WebAssembly: an editable in-memory
buffer with undo/redo, hex/text/regex search, checksums and hashes, binary
diff, IPS/UPS/BPS/VCDIFF/bsdiff patches and transformation recipes.

## Build

`wasm-pack` is not needed. The `wasm-bindgen` CLI must have exactly the same
version as the `wasm-bindgen` crate pinned in `Cargo.toml` (currently
`0.2.130`):

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.130

cd crates/hexa-wasm
npm run build        # both targets
npm run build:node   # pkg/node (CommonJS, for Node.js)
npm run build:web    # pkg/web  (ES module, for browsers / bundlers)
npm test             # Node.js smoke test against pkg/node
```

The scripts run:

```sh
cargo build --target wasm32-unknown-unknown --release -p hexa-wasm
wasm-bindgen --target nodejs --out-dir pkg/node ../../target/wasm32-unknown-unknown/release/hexa_wasm.wasm
wasm-bindgen --target web    --out-dir pkg/web  ../../target/wasm32-unknown-unknown/release/hexa_wasm.wasm
```

`pkg/` is generated and not committed. `wasm-bindgen` warns that the
`target_features` section is missing because the workspace release profile
strips binaries; the output is fine. Run `wasm-opt -O` on
`hexa_wasm_bg.wasm` for a smaller file.

## Usage

Node.js:

```js
const hexa = require("hexa-wasm");

const doc = hexa.HexDocument.fromBytes(new TextEncoder().encode("hello world"));
doc.overwrite(0, new Uint8Array([0x4a]));    // "Jello world"
doc.insert(5, new TextEncoder().encode(","));
doc.undo();
doc.read(0, 5);                               // Uint8Array
doc.search("hex", "6C 6C ??");                // Float64Array of offsets
doc.toBytes();
doc.free();

hexa.hash(bytes, ["sha256", "crc:CRC-32/ISO-HDLC", "xxh64"]);  // [{ id, value }]
hexa.diff(a, b, { bytewise: false });         // { regions, stats, truncated }
hexa.applyPatch(source, hexa.createPatch("bps", source, target));
hexa.transform(bytes, '[{"kind":"bitwise","op":"xor","operand":"0xFF"}]');
```

Browser (ES module):

```js
import init, { HexDocument, search } from "hexa-wasm/web";
await init();
```

## API

| Export | Description |
|--------|-------------|
| `new HexDocument()`, `HexDocument.fromBytes(bytes)` | Editable buffer (piece tree, copy-on-write). |
| `doc.length`, `doc.len()`, `doc.isEmpty()` | Size in bytes. |
| `doc.read(offset, length)` | Bytes at `offset` (shorter at the end). |
| `doc.overwrite(offset, bytes)`, `doc.insert(offset, bytes)`, `doc.delete(offset, length)`, `doc.replace(offset, length, bytes)` | Edits; each is one undo step. |
| `doc.undo()`, `doc.redo()`, `doc.canUndo`, `doc.canRedo`, `doc.isModified` | History. |
| `doc.toBytes()` | Whole content. |
| `doc.search(kind, pattern, options?)`, `doc.hash(ids)` | Same as the free functions, on the document. |
| `search(bytes, kind, pattern, options?)` | `kind`: `"hex"` (`??`/`4?` wildcards, `{01xx0101}` bit masks), `"text"`, `"regex"`. Options: `encodings`, `caseSensitive`, `unicode`, `max`, `align`. Returns offsets. |
| `hash(bytes, ids)`, `hashAlgorithms()` | Checksums, CRC presets (`crc:<name>`), MD/SHA/SHA-3/BLAKE, xxHash, Murmur3, FNV, CityHash, ssdeep, TLSH. |
| `entropy(bytes)` | Shannon entropy, bits per byte. |
| `diff(a, b, options?)` | Shift-aware (or `bytewise`) diff regions and statistics. |
| `createPatch(format, source, target)`, `applyPatch(source, patch)` | `ips`, `ups`, `bps`, `vcdiff`, `bsdiff`. |
| `transform(bytes, recipeJson)` | `hexa-transform` recipe: bit ops, arithmetic, codecs, compression, ciphers. |
| `version()` | Engine version. |

Offsets are JavaScript numbers; errors are thrown as `Error`. TypeScript
declarations are generated next to the JavaScript files.
