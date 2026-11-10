//! Sandboxed WebAssembly plugins executed with `wasmi` (SCR-04).
//!
//! See the crate documentation for the ABI.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use wasmi::{
    Caller, Config, Engine, ExternType, Func, Linker, Memory, Module, ResumableCall, Store,
    StoreLimits, StoreLimitsBuilder, Val, ValType,
};

use crate::sandbox::CancelToken;

/// File name of the manifest inside a plugin directory.
pub const PLUGIN_MANIFEST_FILE: &str = "manifest.json";
/// Default module file name inside a plugin directory.
pub const DEFAULT_PLUGIN_MODULE: &str = "plugin.wasm";
/// Import module names under which `hexa_log` is provided.
pub const HOST_IMPORT_MODULES: &[&str] = &["hexa", "env"];
/// Size of a WebAssembly page.
pub const WASM_PAGE_SIZE: u64 = 65_536;

const FUEL_SLICE: u64 = 1_000_000;
const MAX_LOG_ENTRIES: usize = 1_000;
const MAX_LOG_BYTES: usize = 64 * 1024;

/// What a plugin provides (SCR-04). Each kind maps to one export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PluginKind {
    /// `transform(ptr, len) -> u64`: bytes in, bytes out.
    Transform,
    /// `inspect(ptr, len) -> u64`: bytes in, JSON `[{label, value}]` out.
    Inspector,
    /// `format(ptr, len) -> u64`: bytes in, UTF-8 text out.
    Formatter,
    /// `provide(ptr, len) -> u64`: UTF-8 request in, document bytes out.
    Provider,
}

impl PluginKind {
    /// All kinds.
    pub const ALL: [Self; 4] = [
        Self::Transform,
        Self::Inspector,
        Self::Formatter,
        Self::Provider,
    ];

    /// Name of the export implementing this kind.
    pub const fn export_name(self) -> &'static str {
        match self {
            Self::Transform => "transform",
            Self::Inspector => "inspect",
            Self::Formatter => "format",
            Self::Provider => "provide",
        }
    }
}

/// `manifest.json` of a plugin (SCR-04).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginManifest {
    /// Unique id: ASCII letters, digits, `.`, `_`, `-`; not starting with `.`.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Version string (free-form, semver recommended).
    pub version: String,
    /// Short description.
    #[serde(default)]
    pub description: String,
    /// Provided capabilities; at least one.
    pub kinds: Vec<PluginKind>,
    /// Module file name inside the plugin directory.
    #[serde(default = "default_module")]
    pub module: String,
}

fn default_module() -> String {
    DEFAULT_PLUGIN_MODULE.to_owned()
}

/// Whether `id` is a safe plugin/template id (also usable as a directory name).
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !id.starts_with('.')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

impl PluginManifest {
    /// Parses and validates a manifest.
    pub fn from_json(json: &str) -> Result<Self, PluginError> {
        let manifest: Self =
            serde_json::from_str(json).map_err(|e| PluginError::Manifest(e.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Pretty-printed JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// Checks id, kinds and module file name.
    pub fn validate(&self) -> Result<(), PluginError> {
        if !is_valid_id(&self.id) {
            return Err(PluginError::Manifest(format!(
                "invalid plugin id {:?}",
                self.id
            )));
        }
        if self.name.trim().is_empty() {
            return Err(PluginError::Manifest("plugin name is empty".to_owned()));
        }
        if self.kinds.is_empty() {
            return Err(PluginError::Manifest("plugin declares no kinds".to_owned()));
        }
        if !is_valid_id(&self.module) {
            return Err(PluginError::Manifest(format!(
                "invalid module file name {:?}",
                self.module
            )));
        }
        Ok(())
    }
}

/// Execution limits of one plugin call (SCR-04).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginLimits {
    /// Fuel (≈ executed instructions) per call, including instantiation.
    pub fuel: u64,
    /// Maximum linear memory in 64 KiB pages.
    pub max_memory_pages: u32,
    /// Wall-clock limit per call.
    pub timeout: Duration,
}

impl Default for PluginLimits {
    /// 2·10⁹ fuel, 2048 pages (128 MiB), 10 seconds.
    fn default() -> Self {
        Self {
            fuel: 2_000_000_000,
            max_memory_pages: 2048,
            timeout: Duration::from_secs(10),
        }
    }
}

/// One row produced by an inspector plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectRow {
    /// Row label.
    pub label: String,
    /// Formatted value.
    pub value: String,
}

/// Output of a plugin call plus diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginCall<T> {
    /// The call's result.
    pub output: T,
    /// Messages passed to `hexa_log`.
    pub logs: Vec<String>,
    /// Fuel consumed (instantiation included).
    pub fuel_used: u64,
}

/// Plugin loading and execution errors (SCR-04).
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    /// File system error.
    #[error("{path}: {source}")]
    Io {
        /// Offending path.
        path: PathBuf,
        /// Underlying error.
        source: std::io::Error,
    },
    /// Invalid manifest.
    #[error("invalid plugin manifest: {0}")]
    Manifest(String),
    /// The module failed to parse/validate or lacks a required export.
    #[error("invalid plugin module: {0}")]
    InvalidModule(String),
    /// The plugin does not declare the requested kind.
    #[error("plugin does not provide {0:?}")]
    UnsupportedKind(PluginKind),
    /// Instantiation failed (imports, memory limit, start function).
    #[error("plugin instantiation failed: {0}")]
    Instantiate(String),
    /// The fuel budget ran out.
    #[error("plugin ran out of fuel ({fuel})")]
    FuelExhausted {
        /// Configured fuel.
        fuel: u64,
    },
    /// The wall-clock limit was hit.
    #[error("plugin timed out after {millis} ms")]
    Timeout {
        /// Configured limit in milliseconds.
        millis: u64,
    },
    /// The cancellation flag was raised.
    #[error("plugin call cancelled")]
    Cancelled,
    /// The plugin trapped.
    #[error("plugin trapped: {0}")]
    Trap(String),
    /// The plugin returned an out-of-bounds or malformed result.
    #[error("plugin returned invalid output: {0}")]
    BadOutput(String),
}

struct HostState {
    limits: StoreLimits,
    logs: Vec<String>,
    log_bytes: usize,
}

/// A validated plugin module; each call runs in a fresh instance (SCR-04).
pub struct Plugin {
    manifest: PluginManifest,
    engine: Engine,
    module: Module,
    limits: PluginLimits,
    cancel: CancelToken,
    dir: Option<PathBuf>,
}

impl std::fmt::Debug for Plugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Plugin")
            .field("manifest", &self.manifest)
            .field("limits", &self.limits)
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

fn expect_func(
    module: &Module,
    name: &str,
    params: &[ValType],
    results: &[ValType],
) -> Result<(), PluginError> {
    let export = module
        .exports()
        .find(|e| e.name() == name)
        .ok_or_else(|| PluginError::InvalidModule(format!("missing export `{name}`")))?;
    match export.ty() {
        ExternType::Func(ty) if ty.params() == params && ty.results() == results => Ok(()),
        _ => Err(PluginError::InvalidModule(format!(
            "export `{name}` must be a function {params:?} -> {results:?}"
        ))),
    }
}

impl Plugin {
    /// Validates `wasm` against `manifest` (exports for every declared kind).
    pub fn from_bytes(
        manifest: PluginManifest,
        wasm: &[u8],
        limits: PluginLimits,
    ) -> Result<Self, PluginError> {
        manifest.validate()?;
        let mut config = Config::default();
        config.consume_fuel(true);
        let engine = Engine::new(&config);
        let module =
            Module::new(&engine, wasm).map_err(|e| PluginError::InvalidModule(e.to_string()))?;
        let has_memory = module
            .exports()
            .any(|e| e.name() == "memory" && matches!(e.ty(), ExternType::Memory(_)));
        if !has_memory {
            return Err(PluginError::InvalidModule(
                "missing exported `memory`".to_owned(),
            ));
        }
        expect_func(&module, "hexa_alloc", &[ValType::I32], &[ValType::I32])?;
        expect_func(&module, "hexa_free", &[ValType::I32, ValType::I32], &[])?;
        for kind in &manifest.kinds {
            expect_func(
                &module,
                kind.export_name(),
                &[ValType::I32, ValType::I32],
                &[ValType::I64],
            )?;
        }
        for import in module.imports() {
            let known =
                HOST_IMPORT_MODULES.contains(&import.module()) && import.name() == "hexa_log";
            if !known {
                return Err(PluginError::InvalidModule(format!(
                    "unsupported import `{}`.`{}`",
                    import.module(),
                    import.name()
                )));
            }
        }
        Ok(Self {
            manifest,
            engine,
            module,
            limits,
            cancel: CancelToken::new(),
            dir: None,
        })
    }

    /// Loads `<dir>/manifest.json` and the module it names.
    pub fn load_dir(dir: &Path, limits: PluginLimits) -> Result<Self, PluginError> {
        let manifest_path = dir.join(PLUGIN_MANIFEST_FILE);
        let text = std::fs::read_to_string(&manifest_path).map_err(|source| PluginError::Io {
            path: manifest_path,
            source,
        })?;
        let manifest = PluginManifest::from_json(&text)?;
        let module_path = dir.join(&manifest.module);
        let wasm = std::fs::read(&module_path).map_err(|source| PluginError::Io {
            path: module_path,
            source,
        })?;
        let mut plugin = Self::from_bytes(manifest, &wasm, limits)?;
        plugin.dir = Some(dir.to_owned());
        Ok(plugin)
    }

    /// The manifest.
    pub fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    /// Directory the plugin was loaded from.
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// Current limits.
    pub fn limits(&self) -> PluginLimits {
        self.limits
    }

    /// Replaces the limits.
    pub fn set_limits(&mut self, limits: PluginLimits) {
        self.limits = limits;
    }

    /// The cancellation flag checked between fuel slices.
    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }

    /// Whether the plugin declares `kind`.
    pub fn provides(&self, kind: PluginKind) -> bool {
        self.manifest.kinds.contains(&kind)
    }

    /// Runs a transform plugin.
    pub fn transform(&self, input: &[u8]) -> Result<PluginCall<Vec<u8>>, PluginError> {
        self.invoke(PluginKind::Transform, input)
    }

    /// Runs an inspector plugin.
    pub fn inspect(&self, input: &[u8]) -> Result<PluginCall<Vec<InspectRow>>, PluginError> {
        let call = self.invoke(PluginKind::Inspector, input)?;
        let rows = serde_json::from_slice(&call.output)
            .map_err(|e| PluginError::BadOutput(format!("inspector JSON: {e}")))?;
        Ok(PluginCall {
            output: rows,
            logs: call.logs,
            fuel_used: call.fuel_used,
        })
    }

    /// Runs a formatter plugin.
    pub fn format(&self, input: &[u8]) -> Result<PluginCall<String>, PluginError> {
        let call = self.invoke(PluginKind::Formatter, input)?;
        let text = String::from_utf8(call.output)
            .map_err(|e| PluginError::BadOutput(format!("formatter text is not UTF-8: {e}")))?;
        Ok(PluginCall {
            output: text,
            logs: call.logs,
            fuel_used: call.fuel_used,
        })
    }

    /// Runs a provider plugin with a UTF-8 request (e.g. a URI).
    pub fn provide(&self, request: &str) -> Result<PluginCall<Vec<u8>>, PluginError> {
        self.invoke(PluginKind::Provider, request.as_bytes())
    }

    /// Runs the export of `kind` on `input` in a fresh instance.
    pub fn invoke(
        &self,
        kind: PluginKind,
        input: &[u8],
    ) -> Result<PluginCall<Vec<u8>>, PluginError> {
        if !self.provides(kind) {
            return Err(PluginError::UnsupportedKind(kind));
        }
        let input_len = u32::try_from(input.len())
            .map_err(|_| PluginError::BadOutput("input larger than 4 GiB".to_owned()))?;
        let max_bytes = u64::from(self.limits.max_memory_pages) * WASM_PAGE_SIZE;
        let state = HostState {
            limits: StoreLimitsBuilder::new()
                .memory_size(usize::try_from(max_bytes).unwrap_or(usize::MAX))
                .memories(1)
                .instances(1)
                .tables(4)
                .build(),
            logs: Vec::new(),
            log_bytes: 0,
        };
        let mut store = Store::new(&self.engine, state);
        store.limiter(|s| &mut s.limits);
        let mut linker = Linker::<HostState>::new(&self.engine);
        for module in HOST_IMPORT_MODULES {
            linker
                .func_wrap(module, "hexa_log", host_log)
                .map_err(|e| PluginError::Instantiate(e.to_string()))?;
        }

        let started = Instant::now();
        store
            .set_fuel(self.limits.fuel)
            .map_err(|e| PluginError::Instantiate(e.to_string()))?;
        let instance = linker
            .instantiate_and_start(&mut store, &self.module)
            .map_err(|e| {
                if store.get_fuel().is_ok_and(|f| f == 0) {
                    PluginError::FuelExhausted {
                        fuel: self.limits.fuel,
                    }
                } else {
                    PluginError::Instantiate(e.to_string())
                }
            })?;
        let used = self.limits.fuel - store.get_fuel().unwrap_or(0);
        let mut runner = Runner {
            store,
            used,
            limits: self.limits,
            started,
            cancel: &self.cancel,
        };

        let get = |runner: &Runner<'_>, name: &str| {
            instance
                .get_func(&runner.store, name)
                .ok_or_else(|| PluginError::InvalidModule(format!("missing export `{name}`")))
        };
        let memory = instance
            .get_memory(&runner.store, "memory")
            .ok_or_else(|| PluginError::InvalidModule("missing exported `memory`".to_owned()))?;
        let alloc = get(&runner, "hexa_alloc")?;
        let free = get(&runner, "hexa_free")?;
        let entry = get(&runner, kind.export_name())?;

        let mut out = [Val::I32(0)];
        runner.call(&alloc, &[Val::I32(input_len as i32)], &mut out)?;
        let in_ptr = out[0].i32().unwrap_or(0) as u32;
        write_memory(&memory, &mut runner.store, in_ptr, input)?;

        let mut result = [Val::I64(0)];
        runner.call(
            &entry,
            &[Val::I32(in_ptr as i32), Val::I32(input_len as i32)],
            &mut result,
        )?;
        let packed = result[0].i64().unwrap_or(0) as u64;
        let out_ptr = (packed >> 32) as u32;
        let out_len = packed as u32;
        let output = read_memory(&memory, &runner.store, out_ptr, out_len)?;

        runner.call(
            &free,
            &[Val::I32(in_ptr as i32), Val::I32(input_len as i32)],
            &mut [],
        )?;
        if out_len > 0 && out_ptr != in_ptr {
            runner.call(
                &free,
                &[Val::I32(out_ptr as i32), Val::I32(out_len as i32)],
                &mut [],
            )?;
        }
        let fuel_used = runner.used;
        let logs = std::mem::take(&mut runner.store.data_mut().logs);
        Ok(PluginCall {
            output,
            logs,
            fuel_used,
        })
    }
}

fn host_log(caller: Caller<'_, HostState>, ptr: u32, len: u32) -> Result<(), wasmi::Error> {
    let memory = caller
        .get_export("memory")
        .and_then(wasmi::Extern::into_memory)
        .ok_or_else(|| wasmi::Error::new("hexa_log: plugin has no exported memory"))?;
    let mut caller = caller;
    let state = caller.data();
    if state.logs.len() >= MAX_LOG_ENTRIES || state.log_bytes >= MAX_LOG_BYTES {
        return Ok(());
    }
    let take = (len as usize).min(MAX_LOG_BYTES - state.log_bytes);
    let mut buf = vec![0; take];
    memory
        .read(&caller, ptr as usize, &mut buf)
        .map_err(|e| wasmi::Error::new(format!("hexa_log: {e}")))?;
    let state = caller.data_mut();
    state.log_bytes += buf.len();
    state.logs.push(String::from_utf8_lossy(&buf).into_owned());
    Ok(())
}

fn write_memory(
    memory: &Memory,
    store: &mut Store<HostState>,
    ptr: u32,
    data: &[u8],
) -> Result<(), PluginError> {
    memory
        .write(store, ptr as usize, data)
        .map_err(|e| PluginError::BadOutput(format!("hexa_alloc returned invalid pointer: {e}")))
}

fn read_memory(
    memory: &Memory,
    store: &Store<HostState>,
    ptr: u32,
    len: u32,
) -> Result<Vec<u8>, PluginError> {
    let mut buf = vec![0; len as usize];
    memory
        .read(store, ptr as usize, &mut buf)
        .map_err(|e| PluginError::BadOutput(format!("result {ptr}+{len} out of bounds: {e}")))?;
    Ok(buf)
}

/// Executes calls in fuel slices, checking timeout and cancellation between
/// slices.
struct Runner<'a> {
    store: Store<HostState>,
    used: u64,
    limits: PluginLimits,
    started: Instant,
    cancel: &'a CancelToken,
}

impl Runner<'_> {
    fn next_slice(&self, required: u64) -> Result<u64, PluginError> {
        let remaining = self.limits.fuel.saturating_sub(self.used);
        if remaining == 0 || remaining < required {
            return Err(PluginError::FuelExhausted {
                fuel: self.limits.fuel,
            });
        }
        Ok(FUEL_SLICE.max(required).min(remaining))
    }

    fn check(&self) -> Result<(), PluginError> {
        if self.cancel.is_cancelled() {
            return Err(PluginError::Cancelled);
        }
        if self.started.elapsed() > self.limits.timeout {
            return Err(PluginError::Timeout {
                millis: u64::try_from(self.limits.timeout.as_millis()).unwrap_or(u64::MAX),
            });
        }
        Ok(())
    }

    fn set_fuel(&mut self, fuel: u64) -> Result<(), PluginError> {
        self.store
            .set_fuel(fuel)
            .map_err(|e| PluginError::Trap(e.to_string()))
    }

    fn account(&mut self, given: u64) {
        let left = self.store.get_fuel().unwrap_or(0);
        self.used += given.saturating_sub(left);
    }

    fn call(&mut self, func: &Func, args: &[Val], results: &mut [Val]) -> Result<(), PluginError> {
        self.check()?;
        let mut given = self.next_slice(0)?;
        self.set_fuel(given)?;
        let mut state = func
            .call_resumable(&mut self.store, args, results)
            .map_err(|e| PluginError::Trap(e.to_string()))?;
        loop {
            self.account(given);
            match state {
                ResumableCall::Finished => return Ok(()),
                ResumableCall::HostTrap(trap) => {
                    return Err(PluginError::Trap(trap.into_host_error().to_string()));
                }
                ResumableCall::OutOfFuel(pending) => {
                    self.check()?;
                    given = self.next_slice(pending.required_fuel())?;
                    self.set_fuel(given)?;
                    state = pending
                        .resume(&mut self.store, results)
                        .map_err(|e| PluginError::Trap(e.to_string()))?;
                }
            }
        }
    }
}

/// A failed plugin directory during [`PluginRegistry::load_dir`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginLoadFailure {
    /// Plugin directory.
    pub path: PathBuf,
    /// Error message.
    pub error: String,
}

/// Plugins loaded from a directory of plugin directories (SCR-04).
#[derive(Debug, Default)]
pub struct PluginRegistry {
    plugins: Vec<Plugin>,
}

impl PluginRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Loads every `<root>/<name>/manifest.json`. Broken plugins and duplicate
    /// ids are reported, not fatal. A missing `root` yields an empty registry.
    pub fn load_dir(root: &Path, limits: PluginLimits) -> (Self, Vec<PluginLoadFailure>) {
        let mut registry = Self::new();
        let mut failures = Vec::new();
        let entries = match std::fs::read_dir(root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (registry, failures),
            Err(e) => {
                failures.push(PluginLoadFailure {
                    path: root.to_owned(),
                    error: e.to_string(),
                });
                return (registry, failures);
            }
        };
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.is_dir()
                    && !p
                        .file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with('.'))
            })
            .collect();
        dirs.sort();
        for dir in dirs {
            match Plugin::load_dir(&dir, limits) {
                Ok(plugin) => {
                    if let Err(error) = registry.insert(plugin) {
                        failures.push(PluginLoadFailure {
                            path: dir,
                            error: error.to_string(),
                        });
                    }
                }
                Err(error) => failures.push(PluginLoadFailure {
                    path: dir,
                    error: error.to_string(),
                }),
            }
        }
        (registry, failures)
    }

    /// Adds a plugin; fails on a duplicate id.
    pub fn insert(&mut self, plugin: Plugin) -> Result<(), PluginError> {
        if self.get(&plugin.manifest.id).is_some() {
            return Err(PluginError::Manifest(format!(
                "duplicate plugin id {:?}",
                plugin.manifest.id
            )));
        }
        let pos = self
            .plugins
            .partition_point(|p| p.manifest.id < plugin.manifest.id);
        self.plugins.insert(pos, plugin);
        Ok(())
    }

    /// Removes a plugin by id.
    pub fn remove(&mut self, id: &str) -> Option<Plugin> {
        let pos = self.plugins.iter().position(|p| p.manifest.id == id)?;
        Some(self.plugins.remove(pos))
    }

    /// Plugin by id.
    pub fn get(&self, id: &str) -> Option<&Plugin> {
        self.plugins.iter().find(|p| p.manifest.id == id)
    }

    /// All plugins sorted by id.
    pub fn plugins(&self) -> &[Plugin] {
        &self.plugins
    }

    /// Plugins providing `kind`.
    pub fn by_kind(&self, kind: PluginKind) -> impl Iterator<Item = &Plugin> {
        self.plugins.iter().filter(move |p| p.provides(kind))
    }

    /// Manifests of all plugins (for the UI).
    pub fn manifests(&self) -> Vec<PluginManifest> {
        self.plugins.iter().map(|p| p.manifest.clone()).collect()
    }
}
