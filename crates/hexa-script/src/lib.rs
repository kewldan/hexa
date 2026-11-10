//! Scripting, macros, WASM plugins, plugin catalog and the local API of Hexa.
//!
//! The application owns documents; everything here reaches them only through
//! [`ScriptHost`].
//!
//! | Spec   | Module / type |
//! |--------|---------------|
//! | SCR-01 | [`ScriptEngine`], [`ScriptLimits`], [`CancelToken`] |
//! | SCR-02 | [`Repl`] |
//! | SCR-03 | [`Macro`], [`MacroRecorder`], [`Macro::to_lua`] |
//! | SRCH-08| [`PredicateSearch`] |
//! | SCR-04 | [`Plugin`], [`PluginRegistry`], [`PluginManifest`] |
//! | SCR-05 | [`Catalog`] |
//! | SCR-09 | [`LocalApi`], [`ApiRequest`], [`ApiResponse`] |
//!
//! # Lua sandbox (SCR-01)
//!
//! Lua 5.4 with `string`, `table`, `math`, `utf8`, `coroutine` and the base
//! library minus `dofile`, `loadfile` and `string.dump`; `load` accepts text
//! chunks only; `collectgarbage` only `collect`/`count`/`step`. `io`, `os`,
//! `package`, `require` and `debug` are absent. Each run is bounded by a heap
//! limit, an instruction budget (checked every [`HOOK_INTERVAL`]
//! instructions), a wall-clock timeout and a [`CancelToken`].
//!
//! # Lua API reference (`hexa` table)
//!
//! Offsets are 0-based integers, byte data are Lua strings, ranges are tables
//! `{offset = n, len = n}`. The trailing `doc` argument is optional and
//! defaults to the active document. Host failures raise Lua errors.
//!
//! | Function | Returns |
//! |----------|---------|
//! | `hexa.documents()` | array of `{id, name, path, len, read_only, modified}` |
//! | `hexa.active()` | active document id or `nil` |
//! | `hexa.open(path [, read_only])` | new document id |
//! | `hexa.len([doc])` | length |
//! | `hexa.read(offset, len [, doc])` | byte string (short at end) |
//! | `hexa.write(offset, bytes [, doc])` | overwrite |
//! | `hexa.insert(offset, bytes [, doc])` | insert |
//! | `hexa.delete(offset, len [, doc])` | delete |
//! | `hexa.replace(offset, len, bytes [, doc])` | replace |
//! | `hexa.selection([doc])` | range or `nil` |
//! | `hexa.select(offset, len [, doc])` / `hexa.clear_selection([doc])` | |
//! | `hexa.cursor([doc])` / `hexa.set_cursor(offset [, doc])` | caret |
//! | `hexa.go_to(offset [, doc])` | move caret and scroll (`goto` is a Lua keyword) |
//! | `hexa.find_next(query [, from [, doc]])` | range or `nil`; `from` defaults to the caret |
//! | `hexa.find_all(query [, limit [, doc]])` | array of ranges |
//! | `hexa.hash(algorithm [, offset [, len [, doc]]])` | hex digest |
//! | `hexa.inspect(offset, type [, doc])` | formatted value |
//! | `hexa.template(name [, offset [, doc]])` | parsed tree as Lua tables |
//! | `hexa.transform(recipe [, offset [, len [, doc]]])` | output bytes (document unchanged) |
//! | `hexa.undo([doc])` / `hexa.redo([doc])` | boolean |
//! | `hexa.save([doc])` | |
//! | `hexa.command(id [, args])` | runs an application command |
//! | `hexa.log(message [, level])` | level `debug`/`info`/`warn`/`error` |
//! | `print(...)` / `hexa.print(...)` | captured output, also sent to the host |
//! | `hexa.prompt(message [, default [, title]])` | string or `nil` |
//! | `hexa.confirm(message [, title])` | boolean |
//! | `hexa.message(message [, title])` | |
//! | `hexa.hex(bytes)` / `hexa.unhex(text)` | hex conversion |
//! | `hexa.array([t])` | marks a table as a JSON array |
//! | `hexa.null` / `hexa.version` | JSON null sentinel / crate version |
//!
//! A `query` is a text string, `{text = "...", ignore_case = true}` or
//! `{hex = "DE AD"}`. A `recipe` is a JSON string or a table. When `hash` or
//! `transform` get no offset they use the selection, else the whole document.
//!
//! # WASM plugin ABI (SCR-04)
//!
//! A plugin directory holds `manifest.json` ([`PluginManifest`]: `id`,
//! `name`, `version`, `description`, `kinds`, `module`) and the module
//! (default `plugin.wasm`). The module must export:
//!
//! * `memory` — linear memory;
//! * `hexa_alloc(len: i32) -> i32` — returns a buffer of `len` bytes;
//! * `hexa_free(ptr: i32, len: i32)` — releases a buffer;
//! * per declared kind, `(ptr: i32, len: i32) -> i64` returning
//!   `(out_ptr << 32) | out_len`:
//!   * `transform` — bytes → bytes;
//!   * `inspect` — bytes → UTF-8 JSON `[{"label": "...", "value": "..."}]`;
//!   * `format` — bytes → UTF-8 text;
//!   * `provide` — UTF-8 request (e.g. a URI) → document bytes.
//!
//! It may import `hexa_log(ptr: i32, len: i32)` from module `hexa` (or `env`)
//! to log UTF-8 text; no other imports are allowed. Per call the host creates
//! a fresh instance, calls `hexa_alloc(input_len)`, copies the input, calls
//! the kind export, copies the output, then calls `hexa_free` for the input
//! and, if distinct and non-empty, for the output. Each call is limited by
//! fuel, a memory page cap and a timeout ([`PluginLimits`]) and can be
//! cancelled ([`Plugin::cancel_token`]).

mod catalog;
mod engine;
mod error;
mod host;
mod local_api;
mod lua_api;
mod macros;
mod plugin;
mod predicate;
mod repl;
mod sandbox;

pub use catalog::{
    CATALOG_SCHEMA_VERSION, Catalog, CatalogError, CatalogPlugin, CatalogTemplate, sha256_hex,
    verify_sha256,
};
pub use engine::{MAX_OUTPUT_BYTES, ScriptEngine, ScriptOutput, format_value};
pub use error::ScriptError;
pub use host::{
    ByteRange, DocId, DocumentInfo, FindQuery, HostResult, LogLevel, QueryKind, ScriptHost,
    parse_hex,
};
pub use local_api::{
    API_VERSION, ApiCall, ApiConfig, ApiError, ApiErrorCode, ApiRequest, ApiResponse, LocalApi,
    WriteMode,
};
pub use lua_api::{API_NAMES, DEFAULT_FIND_LIMIT};
pub use macros::{MACRO_COMMAND_PREFIX, Macro, MacroError, MacroRecorder, MacroStep};
pub use plugin::{
    DEFAULT_PLUGIN_MODULE, HOST_IMPORT_MODULES, InspectRow, PLUGIN_MANIFEST_FILE, Plugin,
    PluginCall, PluginError, PluginKind, PluginLimits, PluginLoadFailure, PluginManifest,
    PluginRegistry, WASM_PAGE_SIZE, is_valid_id,
};
pub use predicate::{PredicateOptions, PredicateSearch};
pub use repl::{REPL_CHUNK, Repl, ReplResponse};
pub use sandbox::{CancelToken, HOOK_INTERVAL, ScriptLimits};
