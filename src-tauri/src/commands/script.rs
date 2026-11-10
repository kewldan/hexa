//! Lua console and scripts, macros, plugins, the plugin catalog, the local
//! API and predicate search (SCR-01…05, SCR-09, SRCH-08).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use hexa_core::piece::Piece;
use hexa_core::search::Match;
use hexa_core::{Document, Tree};
use hexa_script::{Catalog, Macro, PluginKind, PredicateOptions, PredicateSearch, ScriptLimits};
use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State, Window};

use super::doc::{EditResult, mutate, opened_doc};
use super::tools::SearchEvent;
use crate::error::{CmdError, CmdResult};
use crate::script::{
    self, ApiStatus, CancelBridge, PluginListing, ScriptEvent, ScriptState, Sink, UiContext,
    WindowRepl,
};
use crate::state::{AppState, DocInfo};

fn script_err(e: impl std::fmt::Display) -> CmdError {
    CmdError::new("script", e.to_string())
}

fn sink(channel: Channel<ScriptEvent>) -> Sink {
    Arc::new(move |e| {
        let _ = channel.send(e);
    })
}

enum Run {
    File { source: String, name: String },
    Line(String),
}

/// Runs a chunk on the window's REPL engine in the background; output is
/// streamed to `channel`, the job is cancellable with `job_cancel`.
fn spawn_run(
    app: AppHandle,
    repl: Arc<WindowRepl>,
    run: Run,
    ctx: UiContext,
    channel: Channel<ScriptEvent>,
) -> u64 {
    let state = app.state::<AppState>();
    let (job, cancel) = state.start_job();
    app.state::<ScriptState>().running.lock().insert(job);
    std::thread::spawn(move || {
        let t0 = Instant::now();
        let (values, error, incomplete) = {
            let mut guard = repl.repl.lock();
            let token = guard.engine().cancel_token();
            token.reset();
            let _bridge = CancelBridge::spawn(cancel, token.clone());
            repl.host
                .begin(ctx, Some(sink(channel.clone())), Some(token));
            let r = match run {
                Run::File { source, name } => {
                    let _ = guard.engine().set_limits(script::file_limits());
                    let out = guard.engine().run(&source, &name);
                    (out.values, out.error, false)
                }
                Run::Line(line) => {
                    let _ = guard.engine().set_limits(script::repl_limits());
                    let out = guard.eval(&line);
                    (out.values, out.error, out.incomplete)
                }
            };
            repl.host.finish();
            r
        };
        let _ = channel.send(ScriptEvent::Done {
            values,
            error,
            incomplete,
            millis: u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX),
        });
        app.state::<ScriptState>().running.lock().remove(&job);
        app.state::<AppState>().end_job(job);
    });
    job
}

/// Runs a script (SCR-01). Globals stay available in the console.
#[tauri::command]
pub fn script_run(
    app: AppHandle,
    window: Window,
    scripts: State<'_, ScriptState>,
    source: String,
    name: Option<String>,
    ctx: Option<UiContext>,
    channel: Channel<ScriptEvent>,
) -> CmdResult<u64> {
    let repl = scripts.repl_for(&app, window.label())?;
    let name = name.unwrap_or_else(|| "script".to_owned());
    Ok(spawn_run(
        app,
        repl,
        Run::File { source, name },
        ctx.unwrap_or_default(),
        channel,
    ))
}

/// Evaluates one console line (SCR-02); `incomplete` asks for more input.
#[tauri::command]
pub fn script_repl_eval(
    app: AppHandle,
    window: Window,
    scripts: State<'_, ScriptState>,
    line: String,
    ctx: Option<UiContext>,
    channel: Channel<ScriptEvent>,
) -> CmdResult<u64> {
    let repl = scripts.repl_for(&app, window.label())?;
    Ok(spawn_run(
        app,
        repl,
        Run::Line(line),
        ctx.unwrap_or_default(),
        channel,
    ))
}

/// Tab completion of globals and table fields (SCR-02).
#[tauri::command]
pub fn script_complete(
    app: AppHandle,
    window: Window,
    scripts: State<'_, ScriptState>,
    input: String,
) -> CmdResult<Vec<String>> {
    let repl = scripts.repl_for(&app, window.label())?;
    // A running script holds the lock; completion is best effort.
    Ok(repl
        .repl
        .try_lock()
        .map(|r| r.complete(&input))
        .unwrap_or_default())
}

/// Forgets console globals and a pending multi-line input.
#[tauri::command]
pub fn script_repl_reset(window: Window, scripts: State<'_, ScriptState>) {
    scripts.reset_repl(window.label());
}

/// Stops one script job, or all of them.
#[tauri::command]
pub fn script_cancel(
    state: State<'_, AppState>,
    scripts: State<'_, ScriptState>,
    job: Option<u64>,
) {
    if let Some(job) = job {
        state.cancel_job(job);
    } else {
        let jobs: Vec<u64> = scripts.running.lock().iter().copied().collect();
        for job in jobs {
            state.cancel_job(job);
        }
    }
}

/// Answer to a `hexa.prompt` (`None` = cancelled).
#[tauri::command]
pub fn script_prompt_reply(scripts: State<'_, ScriptState>, req: u64, value: Option<String>) {
    scripts.prompts.reply(req, value);
}

/// UI state used by local API calls (SCR-09).
#[tauri::command]
pub fn script_set_context(window: Window, scripts: State<'_, ScriptState>, ctx: UiContext) {
    scripts.set_context(window.label(), ctx);
}

// ---------------------------------------------------------------- macros (SCR-03)

#[tauri::command]
pub fn macro_save(path: PathBuf, mac: Macro) -> CmdResult<()> {
    mac.save(&path).map_err(script_err)
}

#[tauri::command]
pub fn macro_load(path: PathBuf) -> CmdResult<Macro> {
    Macro::load(&path).map_err(script_err)
}

#[tauri::command]
pub fn macro_to_lua(mac: Macro) -> String {
    mac.to_lua()
}

// ---------------------------------------------------------------- plugins (SCR-04)

#[tauri::command]
pub fn plugins_list(state: State<'_, AppState>, scripts: State<'_, ScriptState>) -> PluginListing {
    script::listing(&state, &scripts)
}

#[tauri::command]
pub fn plugins_reload(
    state: State<'_, AppState>,
    scripts: State<'_, ScriptState>,
) -> PluginListing {
    script::reload_plugins(&state, &scripts)
}

/// Opens the plugins folder in the file manager (creating it).
#[tauri::command]
pub fn plugins_open_dir(app: AppHandle, state: State<'_, AppState>) -> CmdResult<String> {
    use tauri_plugin_opener::OpenerExt;
    let dir = script::plugins_dir(&state);
    std::fs::create_dir_all(&dir)?;
    let path = dir.to_string_lossy().into_owned();
    app.opener()
        .open_path(path.clone(), None::<&str>)
        .map_err(|e| CmdError::new("io", e.to_string()))?;
    Ok(path)
}

/// Where a transform plugin's output goes.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PluginTarget {
    #[default]
    Preview,
    NewDoc,
    Replace,
}

#[derive(Debug, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PluginOutput {
    Bytes {
        len: usize,
        preview: Vec<u8>,
        logs: Vec<String>,
    },
    Rows {
        rows: Vec<hexa_script::InspectRow>,
        logs: Vec<String>,
    },
    Text {
        text: String,
        logs: Vec<String>,
    },
    Doc {
        info: DocInfo,
        logs: Vec<String>,
    },
    Edit {
        result: EditResult,
        logs: Vec<String>,
    },
}

const MAX_PLUGIN_INPUT: u64 = 256 << 20;

fn read_tree(state: &AppState, id: u64, start: u64, end: u64) -> CmdResult<Vec<u8>> {
    let tree = state.get(id)?.view.read().tree.clone();
    let end = end.min(tree.len());
    if end < start {
        return Err(CmdError::invalid("range out of bounds"));
    }
    if end - start > MAX_PLUGIN_INPUT {
        return Err(CmdError::invalid("диапазон больше 256 МиБ"));
    }
    Ok(tree.read_vec(start, (end - start) as usize)?)
}

/// Runs a plugin on a range of a document (SCR-04).
#[tauri::command]
pub async fn plugins_run(
    app: AppHandle,
    plugin: String,
    kind: PluginKind,
    id: Option<u64>,
    start: Option<u64>,
    end: Option<u64>,
    request: Option<String>,
    target: Option<PluginTarget>,
) -> CmdResult<PluginOutput> {
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = handle.state::<AppState>();
        let scripts = handle.state::<ScriptState>();
        if scripts.plugins.read().is_none() {
            script::reload_plugins(&state, &scripts);
        }
        let input = match (kind, id) {
            (PluginKind::Provider, _) => request.unwrap_or_default().into_bytes(),
            (_, Some(id)) => read_tree(&state, id, start.unwrap_or(0), end.unwrap_or(u64::MAX))?,
            (_, None) => return Err(CmdError::invalid("нужен документ")),
        };
        let guard = scripts.plugins.read();
        let p = guard
            .as_ref()
            .and_then(|s| s.registry.get(&plugin))
            .ok_or_else(|| CmdError::new("notFound", format!("плагин {plugin} не найден")))?;
        let plugin_err = |e: hexa_script::PluginError| CmdError::new("plugin", e.to_string());
        Ok(match kind {
            PluginKind::Transform => {
                let call = p.transform(&input).map_err(plugin_err)?;
                match (target.unwrap_or_default(), id) {
                    (PluginTarget::Replace, Some(id)) => {
                        let s = start.unwrap_or(0);
                        let e = s + input.len() as u64;
                        let result =
                            mutate(&state, id, |d| d.replace(s, e, &call.output).map(Some))?;
                        PluginOutput::Edit {
                            result,
                            logs: call.logs,
                        }
                    }
                    (PluginTarget::NewDoc, _) => PluginOutput::Doc {
                        info: new_doc(&handle, &state, p.manifest().name.clone(), call.output),
                        logs: call.logs,
                    },
                    _ => PluginOutput::Bytes {
                        len: call.output.len(),
                        preview: call.output[..call.output.len().min(4096)].to_vec(),
                        logs: call.logs,
                    },
                }
            }
            PluginKind::Inspector => {
                let call = p.inspect(&input).map_err(plugin_err)?;
                PluginOutput::Rows {
                    rows: call.output,
                    logs: call.logs,
                }
            }
            PluginKind::Formatter => {
                let call = p.format(&input).map_err(plugin_err)?;
                PluginOutput::Text {
                    text: call.output,
                    logs: call.logs,
                }
            }
            PluginKind::Provider => {
                let req = String::from_utf8_lossy(&input).into_owned();
                let call = p.provide(&req).map_err(plugin_err)?;
                PluginOutput::Doc {
                    info: new_doc(&handle, &state, p.manifest().name.clone(), call.output),
                    logs: call.logs,
                }
            }
        })
    })
    .await
    .map_err(|e| CmdError::new("internal", e.to_string()))?
}

fn new_doc(app: &AppHandle, state: &AppState, name: String, bytes: Vec<u8>) -> DocInfo {
    let tree = if bytes.is_empty() {
        Tree::new()
    } else {
        Tree::from_piece(Piece::bytes(bytes))
    };
    opened_doc(app, state, Document::from_tree(name, tree))
}

// ---------------------------------------------------------------- catalog (SCR-05)

/// Reads a catalog JSON file.
#[tauri::command]
pub fn catalog_read(path: PathBuf) -> CmdResult<Catalog> {
    let text = std::fs::read_to_string(&path)?;
    Catalog::from_json(&text).map_err(script_err)
}

/// Reads an entry's file: a path relative to the catalog, absolute, or
/// `file://`. Network downloads are not done here.
fn entry_bytes(catalog: &Path, url: &str) -> CmdResult<Vec<u8>> {
    if url.starts_with("http://") || url.starts_with("https://") {
        return Err(CmdError::new(
            "unsupported",
            "загрузка из сети не поддерживается: скачайте файл рядом с каталогом и укажите относительный путь",
        ));
    }
    let raw = url.strip_prefix("file://").unwrap_or(url);
    let p = Path::new(raw);
    let p = if p.is_absolute() {
        p.to_path_buf()
    } else {
        catalog.parent().unwrap_or(Path::new(".")).join(p)
    };
    Ok(std::fs::read(p)?)
}

/// Installs a plugin or template from a local catalog after checking its
/// SHA-256 (SCR-05).
#[tauri::command]
pub fn catalog_install(
    state: State<'_, AppState>,
    scripts: State<'_, ScriptState>,
    path: PathBuf,
    id: String,
) -> CmdResult<PluginListing> {
    let catalog = Catalog::from_json(&std::fs::read_to_string(&path)?).map_err(script_err)?;
    if let Some(p) = catalog.plugin(&id) {
        let bytes = entry_bytes(&path, &p.url)?;
        catalog
            .install_plugin(&id, &bytes, &script::plugins_dir(&state))
            .map_err(script_err)?;
    } else if let Some(t) = catalog.template(&id) {
        let bytes = entry_bytes(&path, &t.url)?;
        catalog
            .install_template(&id, &bytes, &script::templates_dir(&state))
            .map_err(script_err)?;
    } else {
        return Err(CmdError::new("notFound", format!("в каталоге нет {id}")));
    }
    Ok(script::reload_plugins(&state, &scripts))
}

// ---------------------------------------------------------------- local API (SCR-09)

#[tauri::command]
pub fn api_server_start(app: AppHandle, port: Option<u16>) -> CmdResult<ApiStatus> {
    script::api_start(&app, port)
}

#[tauri::command]
pub fn api_server_set_port(app: AppHandle, port: u16) -> CmdResult<ApiStatus> {
    script::api_set_port(&app, port)
}

#[tauri::command]
pub fn api_server_stop(app: AppHandle) -> CmdResult<ApiStatus> {
    script::api_stop(&app)
}

#[tauri::command]
pub fn api_server_status(
    state: State<'_, AppState>,
    scripts: State<'_, ScriptState>,
) -> CmdResult<ApiStatus> {
    script::api_status(&state, &scripts)
}

#[tauri::command]
pub fn api_server_regenerate_token(app: AppHandle) -> CmdResult<ApiStatus> {
    script::api_regenerate(&app)
}

// ---------------------------------------------------------------- predicate search (SRCH-08)

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PredicateOpts {
    /// Bytes passed to the predicate per offset.
    pub window: Option<usize>,
    pub align: Option<u64>,
    pub start: Option<u64>,
    pub end: Option<u64>,
    pub max: Option<usize>,
}

const PREDICATE_CHUNK: usize = 256 << 10;

/// Streams the offsets where a Lua predicate `function(offset, bytes)`
/// returns true, like `search_all` (SRCH-08).
#[tauri::command]
pub fn predicate_search(
    app: AppHandle,
    state: State<'_, AppState>,
    id: u64,
    source: String,
    opts: Option<PredicateOpts>,
    channel: Channel<SearchEvent>,
) -> CmdResult<u64> {
    let tree = state.get(id)?.view.read().tree.clone();
    let opts = opts.unwrap_or_default();
    let len = tree.len();
    let start = opts.start.unwrap_or(0).min(len);
    let end = opts.end.unwrap_or(len).clamp(start, len);
    let max = opts.max.unwrap_or(100_000);
    let options = PredicateOptions {
        window: opts.window.unwrap_or(16).clamp(1, 1 << 16),
        alignment: opts.align.unwrap_or(1).max(1),
        max_matches: max,
        limits: ScriptLimits {
            instruction_budget: None,
            // Counted over the whole scan; Stop cancels earlier.
            timeout: Some(Duration::from_hours(1)),
            ..ScriptLimits::default()
        },
    };
    let (job, cancel) = state.start_job();
    std::thread::spawn(move || {
        let send_err = |e: CmdError| {
            let _ = channel.send(SearchEvent::Error { error: e });
        };
        match PredicateSearch::compile(&source, options) {
            Err(e) => send_err(script_err(e)),
            Ok(mut search) => {
                let _bridge = CancelBridge::spawn(cancel.clone(), search.cancel_token());
                let mut count = 0usize;
                let mut offset = start;
                let mut last = Instant::now();
                let send = |found: Vec<u64>, count: &mut usize| {
                    if found.is_empty() {
                        return;
                    }
                    *count += found.len();
                    let matches = found
                        .into_iter()
                        .map(|offset| Match { offset, len: 1 })
                        .collect();
                    let _ = channel.send(SearchEvent::Batch { matches });
                };
                let mut failed = false;
                while offset < end && !search.is_limit_reached() {
                    if cancel.is_cancelled() {
                        send_err(CmdError::new("cancelled", "cancelled"));
                        failed = true;
                        break;
                    }
                    let n = ((end - offset) as usize).min(PREDICATE_CHUNK);
                    let data = match tree.read_vec(offset, n) {
                        Ok(d) if !d.is_empty() => d,
                        Ok(_) => break,
                        Err(e) => {
                            send_err(hexa_core::Error::from(e).into());
                            failed = true;
                            break;
                        }
                    };
                    match search.feed(offset, &data) {
                        Ok(found) => send(found, &mut count),
                        Err(e) => {
                            send_err(script_err(e));
                            failed = true;
                            break;
                        }
                    }
                    offset += data.len() as u64;
                    if last.elapsed() > Duration::from_millis(80) {
                        last = Instant::now();
                        let _ = channel.send(SearchEvent::Progress {
                            done: offset - start,
                            total: end - start,
                        });
                    }
                }
                if !failed {
                    match search.finish() {
                        Ok(found) => {
                            send(found, &mut count);
                            let _ = channel.send(SearchEvent::Done {
                                count,
                                truncated: search.is_limit_reached(),
                            });
                        }
                        Err(e) => send_err(script_err(e)),
                    }
                }
            }
        }
        app.state::<AppState>().end_job(job);
    });
    Ok(job)
}
