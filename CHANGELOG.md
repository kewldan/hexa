# Changelog

All notable changes to Hexa. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/).

## [1.0.0] — 2026-11-10

The first release: a cross-platform hex editor for files of any size, disks,
process memory and remote sources.

### Editor

- Files of any size open instantly: only visible pages are read and edits live in a persistent piece tree.
- History is a tree with branches, reverting a single edit, and undo after saving.
- Atomic or in-place saving with a journal that rolls back after a crash.
- Canvas grid with hex, decimal, octal, binary, signed, float, char and HexII cells, split panes, synced side-by-side tabs, folds and collapsed repeats.
- Data inspector for integers, floats (up to float128), fixed point, varints, dates, GUIDs, network types, colours and the instruction under the cursor; watches and editing.
- Bookmarks, labels and Markdown annotations that move with their bytes.
- Search for hex with wildcards, text in several encodings, regex, numbers, typed sequences and Lua predicates; replace with preview; find in files and open tabs; YARA rules.
- Copy as C, Python, Rust, Go, Base64, HTML and more; clipboard history.

### Structures

- ImHex-style template language with Kaitai, 010 Editor, C header, DWARF and PDB importers and 28 built-in formats.
- Tree synced with the grid, in-place editing, filter, JSON/CSV/XML export, field-level diff, valid-offset scan, smart selection.
- Template editor with live diagnostics, an LSP and DAP server and a VS Code extension.
- Export to `.ksy`, C headers and Python, Go, TypeScript and Rust parsers.

### Analysis and transformations

- Entropy curve, data-class regions, histograms, Hilbert and digraph maps, numeric plots, raw bitmaps, image previews, strings and embedded files.
- 100+ hashes and checksums, including ssdeep and TLSH, block hashes and hash range search.
- Transformation recipes (bit ops, arithmetic, codecs, compression, ciphers) with live preview, write-back and XOR key guessing.

### Compare and patch

- Shift-aware and byte-wise diff, N-way comparison and three-way merge.
- IPS, UPS, BPS, VCDIFF and bsdiff patches; Intel HEX, S-Record, TI-TXT, Tektronix and text dump import and export.

### Reverse engineering

- PE, ELF and Mach-O structure, compiler and packer detection, disassembly for every Capstone architecture plus WebAssembly.
- Inline assembly patches with NOP padding, cross-references, patch lists that port to new versions, PE checksum repair, external tools.

### Beyond files

- Raw disks, partitions, FAT/exFAT/ext/NTFS/ISO browsing, disk images (VHD, VHDX, VMDK, QCOW2, DMG, E01), cloning, wiping, RAID and guarded write-back.
- Process memory: regions, value scanner, freezing, pointer scan, dumps and GDB remote targets.
- HTTP(S), S3 and SFTP sources, network capture, files inside archives.

### Automation

- Sandboxed Lua console, macros, WASM plugins and a token-protected local HTTP API.
- `hexa-cli` with JSON output and CI exit codes; the core as a WebAssembly npm package.

### Interface

- Command palette with fuzzy search and wrong-layout correction, Vim and Emacs keymaps, themes, native macOS menu, portable mode.
- English and Russian, screen reader announcements and colour-blind-safe highlights.

### Install notes

- macOS builds are not notarized: after moving Hexa to Applications, open it once from **System Settings → Privacy & Security → Open Anyway**, or run `xattr -dr com.apple.quarantine /Applications/Hexa.app`.
- Raw disks need administrator rights; processes need the usual debugger permission.
