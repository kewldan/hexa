# Hexa — notes for working in this repo

Cross-platform hex editor: Rust crates for everything that touches data, a thin
Tauri 2 shell, a React 19 + TypeScript UI. See README.md for the feature list
and architecture diagram.

## Layout

- `crates/hexa-core` — piece tree, history, saving, search, inspector. Every
  other crate builds on it. No UI dependencies; also compiles to wasm32.
- `crates/hexa-{diff,transform,analysis,template,re,sys,proc,net,script}` —
  feature crates, each tested on its own. `hexa-cli` and `hexa-wasm` reuse them.
- `src-tauri/src/commands/*.rs` — the only place crates meet the UI. One file
  per area; register new commands in `src-tauri/src/lib.rs` `invoke_handler`
  and any state with `.manage(...)`.
- `src/ipc/*.ts` — typed `invoke` wrappers (`api.ts` for core, one file per
  area for the rest). `src/dev/mock.ts` (+ `mockSources.ts`, `scriptMock.ts`)
  is the browser mock backend used by `bun run dev` and Playwright.
- `src/components/panels/*` — side panels; register a panel in
  `src/components/Sidebar.tsx` (`TITLES`, `ICONS`, `PANELS`) and `PanelId` /
  `DEFAULT_LAYOUT` in `src/state/settings.ts`.
- Dialogs: add a member to the `Dialog` union in `src/state/ui.ts` and a case in
  `src/components/dialogs/Dialogs.tsx`. Commands and default shortcuts live in
  `src/lib/commands.ts`.
- Grid highlight layers come from `src/state/overlays.ts`
  (`useOverlays.getState().set(tabId, layer, regions)`).

## Checks (run before every commit; CI runs the same)

```sh
bunx biome ci . && bunx tsc --noEmit
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
bunx vitest run
bunx playwright test
```

- Also run clippy for `--target x86_64-unknown-linux-gnu` on crates without C
  dependencies (capstone, Lua, ring can't cross-compile from macOS).
- Don't wait for CI; verify locally and push.
- Chain checks with `&&` — a failed `cargo fmt --check` followed by `;` has
  let unformatted code through before.
- When several Playwright runs may overlap, pass
  `--trace off --output <own dir>`; runs share `test-results/` otherwise.

## Rules that bit us

- **Cargo.lock must match the committed manifests**: CI uses `--locked`. If the
  working tree has extra crates or dependencies that aren't committed, commit a
  lockfile resolved for exactly what is staged (resolve it in a temporary
  worktree with `cargo metadata --offline`), not the working-tree one.
- **Every Russian UI string needs an English entry** in `src/i18n/en.json`.
  `bun scripts/i18n-extract.ts` lists missing keys and a Vitest test fails on
  them. Template literals become `{placeholder}` patterns. Put
  `// i18n-ignore` above literals that aren't UI text (sample code, etc.).
- **Every new Tauri command needs a mock** in `src/dev/mock.ts` if the UI calls
  it on mount, otherwise the browser build and e2e tests break.
- **Zustand selectors must return stable values.** `useStore((s) => s.x ?? [])`
  creates a new array on every call and loops React into "Maximum update
  depth exceeded"; use a module-level empty constant.
- **Inline styles beat classes.** A sidebar once stayed 272 px wide while
  "closed" because `style={{ width }}` overrode `.shut { width: 0 }`.
- Biome: no array-index keys without a
  `// biome-ignore lint/suspicious/noArrayIndexKey: reason` right before the
  attribute; `<label>` must wrap its control; `useExhaustiveDependencies` is off
  on purpose (Biome's autofix broke effects).
- The page cache keeps stale pages visible until refetched — don't "fix" edits
  by clearing it, that brings back a blank frame after every keystroke.
- Offsets in the UI are JS numbers (exact to 2⁵³); keep 64-bit math in Rust.

## Style

- Rust: edition 2024, MSRV 1.95, clippy pedantic (see `[workspace.lints]`).
  `unsafe` is denied except in `hexa-sys` and `hexa-proc`, with a `SAFETY`
  comment on every block.
- TypeScript/CSS: Biome formatting (2 spaces, double quotes, 100 columns).
  Reuse the CSS classes in `src/styles/app.css` (`.ph .cap .tag .chip .btn .ib
  .li .list .empty .note .row-f .inp .sel-input .kv .chk .mono`); put
  feature styles in their own file under `src/styles/`.
- UI copy is Russian in code; English comes from `en.json`.

## Git and releases

- Work and commit on `main`; no feature branches.
- Conventional Commits: `feat(scope): …`, `fix: …`, `refactor: …`, `test: …`,
  `docs: …`, `chore: …`, `ci: …`. No AI attribution in commits, PRs or files.
- Release: bump `version` in `Cargo.toml` (workspace), `package.json`,
  `src-tauri/tauri.conf.json`, `crates/hexa-wasm/package.json` and
  `editors/vscode/package.json`, commit `chore(release): vX.Y.Z`, then push the
  tag `vX.Y.Z`. `.github/workflows/release.yml` builds the installers and the
  CLI on all three platforms and publishes the GitHub release.
- README media: screenshots in `docs/screenshots/` and `docs/demo.gif` are
  recorded with Playwright against `bun run dev` (mock backend), dark theme.
