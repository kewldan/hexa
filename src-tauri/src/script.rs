//! Scripting glue: a [`ScriptHost`] over the open documents, per-window
//! REPLs, WASM plugins and the local HTTP API (SCR-01…05, SCR-09, SRCH-08).
//!
//! The UI owns the cursor, the selection and the active tab. They are passed
//! in with every run as a [`UiContext`]; changes made by a script go back to
//! the window as `hexa://script-ui` events ([`UiEvent`]).

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use hexa_core::inspector::{self, Endian, InspectType};
use hexa_core::search::{self, Query, Scope};
use hexa_core::{Cancel, Document, EditMode, OpenOptions};
use hexa_script::{
    ApiConfig, ByteRange, CancelToken, DocId, DocumentInfo, FindQuery, HostResult, LocalApi,
    LogLevel, PluginLoadFailure, PluginManifest, PluginRegistry, QueryKind, Repl, ScriptError,
    ScriptHost, ScriptLimits,
};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::commands::doc::mutate;
use crate::error::{CmdError, CmdResult};
use crate::state::{AppState, DocInfo};

/// Event name of [`UiEvent`]s.
pub const UI_EVENT: &str = "hexa://script-ui";
/// Largest single `hexa.read` / `hexa.transform` input.
const MAX_SCRIPT_READ: u64 = 256 << 20;
/// Minimum interval between `docChanged` notifications during a run.
const DIRTY_FLUSH: Duration = Duration::from_millis(120);
/// Default port of the local API (SCR-09).
pub const DEFAULT_API_PORT: u16 = 4392;
/// Largest HTTP request body of the local API.
const MAX_API_BODY: usize = 64 << 20;

// ---------------------------------------------------------------- context & events

/// A half-open byte range as the UI sends it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Span {
    pub start: u64,
    pub end: u64,
}

/// Caret and selection of one tab.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DocCtx {
    pub id: u64,
    pub cursor: u64,
    pub selection: Option<Span>,
}

/// UI-owned state handed to a script run.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UiContext {
    pub active: Option<u64>,
    pub docs: Vec<DocCtx>,
}

/// Something the window must apply after a script asked for it.
#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum UiEvent {
    /// Bytes changed: refresh the tab.
    DocChanged { doc: u64 },
    /// Move the caret (collapses the selection).
    Cursor { doc: u64, offset: u64, scroll: bool },
    /// Select a range or clear the selection.
    Select { doc: u64, selection: Option<Span> },
    /// Run an application command (`hexa.command`, macro steps).
    Command { id: String, args: serde_json::Value },
    /// Show a document the script opened.
    Adopt { info: DocInfo },
    /// Ask for a line of text; answer with `script_prompt_reply`.
    Prompt {
        req: u64,
        title: String,
        message: String,
        default: String,
    },
}

#[derive(Clone, Serialize)]
struct UiEnvelope {
    /// Target window label; `None` for every window.
    window: Option<String>,
    #[serde(flatten)]
    event: UiEvent,
}

/// Streamed to the console while a script runs.
#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ScriptEvent {
    Print {
        text: String,
    },
    Log {
        level: LogLevel,
        message: String,
    },
    Done {
        values: Vec<String>,
        error: Option<ScriptError>,
        incomplete: bool,
        millis: u64,
    },
}

pub type Sink = Arc<dyn Fn(ScriptEvent) + Send + Sync>;

// ---------------------------------------------------------------- prompts

/// Pending `hexa.prompt` requests answered by the UI.
#[derive(Debug, Default)]
pub struct Prompts {
    next: AtomicU64,
    pending: Mutex<HashMap<u64, mpsc::Sender<Option<String>>>>,
}

impl Prompts {
    fn open(&self) -> (u64, mpsc::Receiver<Option<String>>) {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = mpsc::channel();
        self.pending.lock().insert(id, tx);
        (id, rx)
    }

    /// Delivers the answer of a prompt; `false` if nobody waits for it.
    pub fn reply(&self, id: u64, value: Option<String>) -> bool {
        self.pending
            .lock()
            .remove(&id)
            .is_some_and(|tx| tx.send(value).is_ok())
    }

    fn forget(&self, id: u64) {
        self.pending.lock().remove(&id);
    }
}

// ---------------------------------------------------------------- bridge

/// What the host needs from the application; a trait so the glue can be
/// tested without a Tauri runtime.
pub trait Bridge: Send + Sync {
    fn state(&self) -> &AppState;
    fn emit(&self, window: Option<&str>, event: &UiEvent);
    /// Registers a freshly opened document and returns its info.
    fn adopt(&self, doc: Document) -> DocInfo;
    fn confirm(&self, title: &str, message: &str) -> bool;
    fn message(&self, title: &str, message: &str);
    fn prompts(&self) -> &Prompts;
}

/// The real bridge backed by the Tauri app.
pub struct TauriBridge {
    app: AppHandle,
}

impl std::fmt::Debug for TauriBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TauriBridge").finish_non_exhaustive()
    }
}

impl TauriBridge {
    pub fn new(app: AppHandle) -> Arc<Self> {
        Arc::new(Self { app })
    }
}

impl Bridge for TauriBridge {
    fn state(&self) -> &AppState {
        self.app.state::<AppState>().inner()
    }

    fn emit(&self, window: Option<&str>, event: &UiEvent) {
        let _ = self.app.emit(
            UI_EVENT,
            UiEnvelope {
                window: window.map(str::to_owned),
                event: event.clone(),
            },
        );
    }

    fn adopt(&self, doc: Document) -> DocInfo {
        crate::commands::doc::opened_doc(&self.app, self.state(), doc)
    }

    fn confirm(&self, title: &str, message: &str) -> bool {
        use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
        self.app
            .dialog()
            .message(message)
            .title(title)
            .kind(MessageDialogKind::Info)
            .buttons(MessageDialogButtons::OkCancel)
            .blocking_show()
    }

    fn message(&self, title: &str, message: &str) {
        use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
        self.app
            .dialog()
            .message(message)
            .title(title)
            .kind(MessageDialogKind::Info)
            .blocking_show();
    }

    fn prompts(&self) -> &Prompts {
        &self.app.state::<ScriptState>().inner().prompts
    }
}

// ---------------------------------------------------------------- host

#[derive(Debug, Default)]
struct Ui {
    active: Option<u64>,
    cursor: HashMap<u64, u64>,
    selection: HashMap<u64, Span>,
}

impl From<UiContext> for Ui {
    fn from(ctx: UiContext) -> Self {
        let mut ui = Self {
            active: ctx.active,
            ..Self::default()
        };
        for d in ctx.docs {
            ui.cursor.insert(d.id, d.cursor);
            if let Some(s) = d.selection.filter(|s| s.end > s.start) {
                ui.selection.insert(d.id, s);
            }
        }
        ui
    }
}

/// [`ScriptHost`] of one window (or of the local API).
pub struct AppHost {
    bridge: Arc<dyn Bridge>,
    window: Option<String>,
    ui: Mutex<Ui>,
    sink: Mutex<Option<Sink>>,
    cancel: Mutex<Option<CancelToken>>,
    dirty: Mutex<(HashSet<u64>, Instant)>,
}

impl std::fmt::Debug for AppHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppHost")
            .field("window", &self.window)
            .finish_non_exhaustive()
    }
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn cmd_err(e: CmdError) -> String {
    e.message
}

impl AppHost {
    pub fn new(bridge: Arc<dyn Bridge>, window: Option<String>) -> Arc<Self> {
        Arc::new(Self {
            bridge,
            window,
            ui: Mutex::new(Ui::default()),
            sink: Mutex::new(None),
            cancel: Mutex::new(None),
            dirty: Mutex::new((HashSet::new(), Instant::now())),
        })
    }

    /// Prepares a run: UI state, output sink and cancellation flag.
    pub fn begin(&self, ctx: UiContext, sink: Option<Sink>, cancel: Option<CancelToken>) {
        *self.ui.lock() = ctx.into();
        *self.sink.lock() = sink;
        *self.cancel.lock() = cancel;
    }

    /// Ends a run: notifies the UI about edited documents.
    pub fn finish(&self) {
        self.flush();
        *self.sink.lock() = None;
        *self.cancel.lock() = None;
    }

    fn state(&self) -> &AppState {
        self.bridge.state()
    }

    fn emit(&self, event: &UiEvent) {
        self.bridge.emit(self.window.as_deref(), event);
    }

    fn send(&self, event: ScriptEvent) {
        let sink = self.sink.lock().clone();
        if let Some(sink) = sink {
            sink(event);
        }
    }

    fn mark_dirty(&self, doc: u64) {
        let flush = {
            let mut d = self.dirty.lock();
            d.0.insert(doc);
            d.1.elapsed() >= DIRTY_FLUSH
        };
        if flush {
            self.flush();
        }
    }

    /// Sends `docChanged` for every document edited since the last flush.
    pub fn flush(&self) {
        let docs: Vec<u64> = {
            let mut d = self.dirty.lock();
            d.1 = Instant::now();
            d.0.drain().collect()
        };
        for doc in docs {
            // Every window showing the document refreshes.
            self.bridge.emit(None, &UiEvent::DocChanged { doc });
        }
    }

    fn tree(&self, doc: DocId) -> HostResult<hexa_core::Tree> {
        let entry = self.state().get(doc).map_err(cmd_err)?;
        let tree = entry.view.read().tree.clone();
        Ok(tree)
    }

    fn read_range(&self, doc: DocId, range: ByteRange) -> HostResult<Vec<u8>> {
        let tree = self.tree(doc)?;
        if range.offset > tree.len() {
            return Err(format!("смещение {} за концом файла", range.offset));
        }
        let len = range.len.min(tree.len() - range.offset);
        if len > MAX_SCRIPT_READ {
            return Err(format!("диапазон больше {} МиБ", MAX_SCRIPT_READ >> 20));
        }
        tree.read_vec(range.offset, len as usize).map_err(err)
    }

    fn edit(
        &self,
        doc: DocId,
        f: impl FnOnce(&mut Document) -> hexa_core::Result<hexa_core::Change>,
    ) -> HostResult<()> {
        mutate(self.state(), doc, |d| f(d).map(Some)).map_err(cmd_err)?;
        self.mark_dirty(doc);
        Ok(())
    }

    fn check_doc(&self, doc: DocId) -> HostResult<u64> {
        self.len(doc)
    }

    fn token(&self) -> Option<CancelToken> {
        self.cancel.lock().clone()
    }
}

fn compile_query(query: &FindQuery) -> HostResult<search::Matcher> {
    let q = match query.kind {
        QueryKind::Hex => Query::Hex {
            pattern: query.pattern.clone(),
        },
        QueryKind::Text => Query::Text {
            text: query.pattern.clone(),
            encodings: vec!["utf-8".to_owned()],
            case_sensitive: !query.ignore_case,
        },
    };
    search::compile(&q).map_err(err)
}

/// `"u32le"`, `"f64be"`, `"uint16"`, `"ascii"` → inspector type and order.
fn parse_value_type(text: &str) -> HostResult<(InspectType, Endian)> {
    let lower = text.trim().to_ascii_lowercase();
    let (name, endian) = if let Some(n) = lower.strip_suffix("le") {
        (n, Endian::Little)
    } else if let Some(n) = lower.strip_suffix("be") {
        (n, Endian::Big)
    } else {
        (lower.as_str(), Endian::Little)
    };
    let name = name.trim_end_matches(['_', '-']);
    if let Ok(ty) = serde_json::from_value::<InspectType>(serde_json::Value::String(name.into())) {
        return Ok((ty, endian));
    }
    InspectType::ALL
        .iter()
        .find(|t| t.label().eq_ignore_ascii_case(name))
        .map(|&t| (t, endian))
        .ok_or_else(|| format!("неизвестный тип {text:?}"))
}

/// Short aliases accepted by `hexa.hash`.
fn hash_id(algorithm: &str) -> String {
    match algorithm.to_ascii_lowercase().as_str() {
        "crc32" | "crc-32" => "crc:CRC-32/ISO-HDLC".to_owned(),
        "crc16" | "crc-16" => "crc:CRC-16/ARC".to_owned(),
        "crc64" | "crc-64" => "crc:CRC-64/XZ".to_owned(),
        "sha-1" => "sha1".to_owned(),
        "sha-256" => "sha256".to_owned(),
        "sha-512" => "sha512".to_owned(),
        _ => algorithm.to_owned(),
    }
}

impl ScriptHost for AppHost {
    fn documents(&self) -> Vec<DocumentInfo> {
        let mut out: Vec<DocumentInfo> = self
            .state()
            .all()
            .into_iter()
            .map(|e| {
                let d = e.doc.lock();
                DocumentInfo {
                    id: e.id,
                    name: d.name().to_owned(),
                    path: d.path().map(|p| p.to_string_lossy().into_owned()),
                    len: d.len(),
                    read_only: d.mode() == EditMode::ReadOnly,
                    modified: d.is_modified(),
                }
            })
            .collect();
        out.sort_by_key(|d| d.id);
        out
    }

    fn active_document(&self) -> Option<DocId> {
        self.ui.lock().active
    }

    fn open_file(&self, path: &str, read_only: bool) -> HostResult<DocId> {
        let doc = Document::open(
            Path::new(path),
            &OpenOptions {
                mode: if read_only {
                    EditMode::ReadOnly
                } else {
                    EditMode::Overwrite
                },
                ..OpenOptions::default()
            },
        )
        .map_err(err)?;
        let info = self.bridge.adopt(doc);
        let id = info.id;
        self.emit(&UiEvent::Adopt { info });
        Ok(id)
    }

    fn len(&self, doc: DocId) -> HostResult<u64> {
        Ok(self.tree(doc)?.len())
    }

    fn read(&self, doc: DocId, offset: u64, len: u64) -> HostResult<Vec<u8>> {
        self.read_range(doc, ByteRange::new(offset, len))
    }

    fn write(&self, doc: DocId, offset: u64, data: &[u8]) -> HostResult<()> {
        if data.is_empty() {
            return Ok(());
        }
        self.edit(doc, |d| d.overwrite(offset, data))
    }

    fn insert(&self, doc: DocId, offset: u64, data: &[u8]) -> HostResult<()> {
        if data.is_empty() {
            return Ok(());
        }
        self.edit(doc, |d| d.insert(offset, data))
    }

    fn delete(&self, doc: DocId, offset: u64, len: u64) -> HostResult<()> {
        if len == 0 {
            return Ok(());
        }
        self.edit(doc, |d| d.delete(offset, offset.saturating_add(len)))
    }

    fn replace(&self, doc: DocId, offset: u64, len: u64, data: &[u8]) -> HostResult<()> {
        self.edit(doc, |d| d.replace(offset, offset.saturating_add(len), data))
    }

    fn selection(&self, doc: DocId) -> HostResult<Option<ByteRange>> {
        self.check_doc(doc)?;
        Ok(self
            .ui
            .lock()
            .selection
            .get(&doc)
            .map(|s| ByteRange::new(s.start, s.end - s.start)))
    }

    fn set_selection(&self, doc: DocId, selection: Option<ByteRange>) -> HostResult<()> {
        let len = self.check_doc(doc)?;
        let span = selection
            .map(|r| Span {
                start: r.offset.min(len),
                end: r.end().min(len),
            })
            .filter(|s| s.end > s.start);
        {
            let mut ui = self.ui.lock();
            match span {
                Some(s) => {
                    ui.selection.insert(doc, s);
                    ui.cursor.insert(doc, s.end - 1);
                }
                None => {
                    ui.selection.remove(&doc);
                }
            }
        }
        self.flush();
        self.emit(&UiEvent::Select {
            doc,
            selection: span,
        });
        Ok(())
    }

    fn cursor(&self, doc: DocId) -> HostResult<u64> {
        self.check_doc(doc)?;
        Ok(self.ui.lock().cursor.get(&doc).copied().unwrap_or(0))
    }

    fn set_cursor(&self, doc: DocId, offset: u64) -> HostResult<()> {
        let offset = offset.min(self.check_doc(doc)?);
        {
            let mut ui = self.ui.lock();
            ui.cursor.insert(doc, offset);
            ui.selection.remove(&doc);
        }
        self.flush();
        self.emit(&UiEvent::Cursor {
            doc,
            offset,
            scroll: false,
        });
        Ok(())
    }

    fn goto(&self, doc: DocId, offset: u64) -> HostResult<()> {
        let offset = offset.min(self.check_doc(doc)?);
        {
            let mut ui = self.ui.lock();
            ui.cursor.insert(doc, offset);
            ui.selection.remove(&doc);
        }
        self.flush();
        self.emit(&UiEvent::Cursor {
            doc,
            offset,
            scroll: true,
        });
        Ok(())
    }

    fn find_next(&self, doc: DocId, query: &FindQuery, from: u64) -> HostResult<Option<ByteRange>> {
        let tree = self.tree(doc)?;
        let matcher = compile_query(query)?;
        let scope = Scope::whole(&tree);
        let m = search::find_next(&tree, &matcher, scope, from, false, false, &Cancel::new())
            .map_err(err)?;
        Ok(m.map(|m| ByteRange::new(m.offset, m.len)))
    }

    fn find_all(&self, doc: DocId, query: &FindQuery, limit: usize) -> HostResult<Vec<ByteRange>> {
        let tree = self.tree(doc)?;
        let matcher = compile_query(query)?;
        let scope = Scope::whole(&tree);
        let mut out = Vec::new();
        search::find_all(
            &tree,
            &matcher,
            scope,
            limit,
            &Cancel::new(),
            &hexa_core::task::no_progress,
            |m| out.push(ByteRange::new(m.offset, m.len)),
        )
        .map_err(err)?;
        Ok(out)
    }

    fn hash(&self, doc: DocId, range: ByteRange, algorithm: &str) -> HostResult<String> {
        let tree = self.tree(doc)?;
        let r = hexa_core::hash::hash_range(
            &tree,
            range.offset,
            range.end(),
            &[hash_id(algorithm)],
            None,
            &Cancel::new(),
            &hexa_core::task::no_progress,
        )
        .map_err(err)?;
        r.into_iter()
            .next()
            .map(|h| h.value.to_ascii_lowercase())
            .ok_or_else(|| format!("неизвестный алгоритм {algorithm:?}"))
    }

    fn inspect(&self, doc: DocId, offset: u64, value_type: &str) -> HostResult<String> {
        let (ty, endian) = parse_value_type(value_type)?;
        let bytes = self.read_range(doc, ByteRange::new(offset, 16))?;
        inspector::decode(ty, &bytes, endian)
            .0
            .ok_or_else(|| format!("нет значения {value_type} по смещению {offset}"))
    }

    fn apply_template(&self, _doc: DocId, name: &str, _offset: u64) -> HostResult<String> {
        Err(format!(
            "шаблоны структур пока недоступны из скриптов (шаблон {name:?})"
        ))
    }

    fn transform(&self, doc: DocId, recipe_json: &str, range: ByteRange) -> HostResult<Vec<u8>> {
        let recipe: hexa_transform::Recipe =
            serde_json::from_str(recipe_json).map_err(|e| format!("рецепт: {e}"))?;
        let data = self.read_range(doc, range)?;
        hexa_transform::apply(&recipe, &data).map_err(err)
    }

    fn undo(&self, doc: DocId) -> HostResult<bool> {
        let r = mutate(self.state(), doc, |d| Ok(d.undo())).map_err(cmd_err)?;
        self.mark_dirty(doc);
        Ok(r.change.is_some())
    }

    fn redo(&self, doc: DocId) -> HostResult<bool> {
        let r = mutate(self.state(), doc, |d| Ok(d.redo())).map_err(cmd_err)?;
        self.mark_dirty(doc);
        Ok(r.change.is_some())
    }

    fn save(&self, doc: DocId) -> HostResult<()> {
        let entry = self.state().get(doc).map_err(cmd_err)?;
        if entry.saving.swap(true, Ordering::AcqRel) {
            return Err("файл уже сохраняется".to_owned());
        }
        let result = {
            let mut d = entry.doc.lock();
            let r = d.save(
                &hexa_core::save::SaveOptions::default(),
                &hexa_core::task::no_progress,
                &Cancel::new(),
            );
            entry.publish(&d);
            r
        };
        entry.saving.store(false, Ordering::Release);
        self.mark_dirty(doc);
        result.map(|_| ()).map_err(err)
    }

    fn run_command(&self, command: &str, args: &serde_json::Value) -> HostResult<()> {
        if command.trim().is_empty() {
            return Err("пустой id команды".to_owned());
        }
        self.flush();
        self.emit(&UiEvent::Command {
            id: command.to_owned(),
            args: args.clone(),
        });
        Ok(())
    }

    fn log(&self, level: LogLevel, message: &str) {
        self.send(ScriptEvent::Log {
            level,
            message: message.to_owned(),
        });
    }

    fn print(&self, text: &str) {
        self.send(ScriptEvent::Print {
            text: text.to_owned(),
        });
    }

    fn prompt(&self, title: &str, message: &str, default: &str) -> Option<String> {
        self.flush();
        let prompts = self.bridge.prompts();
        let (req, rx) = prompts.open();
        self.emit(&UiEvent::Prompt {
            req,
            title: title.to_owned(),
            message: message.to_owned(),
            default: default.to_owned(),
        });
        let token = self.token();
        loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(v) => return v,
                Err(mpsc::RecvTimeoutError::Disconnected) => return None,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if token.as_ref().is_some_and(CancelToken::is_cancelled) {
                        prompts.forget(req);
                        return None;
                    }
                }
            }
        }
    }

    fn confirm(&self, title: &str, message: &str) -> bool {
        self.flush();
        self.bridge.confirm(title, message)
    }

    fn message(&self, title: &str, message: &str) {
        self.flush();
        self.bridge.message(title, message);
    }
}

// ---------------------------------------------------------------- cancellation

/// Forwards a job's [`Cancel`] to a Lua/WASM [`CancelToken`] while alive.
#[derive(Debug)]
pub struct CancelBridge {
    done: Arc<AtomicBool>,
}

impl CancelBridge {
    pub fn spawn(cancel: Cancel, token: CancelToken) -> Self {
        let done = Arc::new(AtomicBool::new(false));
        let flag = done.clone();
        std::thread::spawn(move || {
            while !flag.load(Ordering::Acquire) {
                if cancel.is_cancelled() {
                    token.cancel();
                    return;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        });
        Self { done }
    }
}

impl Drop for CancelBridge {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Release);
    }
}

// ---------------------------------------------------------------- state

/// REPL of one window; also runs script files so their globals stay
/// available in the console (SCR-01, SCR-02).
pub struct WindowRepl {
    pub host: Arc<AppHost>,
    pub repl: Mutex<Repl>,
}

impl std::fmt::Debug for WindowRepl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowRepl")
            .field("host", &self.host)
            .finish_non_exhaustive()
    }
}

/// Limits of console input (SCR-02).
pub fn repl_limits() -> ScriptLimits {
    ScriptLimits::default()
}

/// Limits of script files: no instruction budget, Stop cancels them (SCR-01).
pub fn file_limits() -> ScriptLimits {
    ScriptLimits {
        instruction_budget: None,
        timeout: Some(Duration::from_mins(30)),
        ..ScriptLimits::default()
    }
}

/// Loaded plugins plus the directories that failed to load (SCR-04).
#[derive(Debug, Default)]
pub struct PluginSet {
    pub registry: PluginRegistry,
    pub failures: Vec<PluginLoadFailure>,
}

/// Plugins as shown in the settings.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginListing {
    pub dir: String,
    pub plugins: Vec<PluginManifest>,
    pub failures: Vec<PluginLoadFailure>,
}

/// Scripting state shared by all windows.
#[derive(Default)]
pub struct ScriptState {
    pub prompts: Prompts,
    repls: Mutex<HashMap<String, Arc<WindowRepl>>>,
    /// Script jobs that are running (for "stop all").
    pub running: Mutex<HashSet<u64>>,
    /// Last UI context pushed by a window, used by the local API.
    context: Mutex<Option<(String, UiContext)>>,
    pub plugins: RwLock<Option<PluginSet>>,
    api: Mutex<Option<ApiServer>>,
    api_error: Mutex<Option<String>>,
}

impl std::fmt::Debug for ScriptState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptState").finish_non_exhaustive()
    }
}

impl ScriptState {
    /// The REPL of `window`, created on first use.
    pub fn repl_for(&self, app: &AppHandle, window: &str) -> CmdResult<Arc<WindowRepl>> {
        if let Some(r) = self.repls.lock().get(window) {
            return Ok(r.clone());
        }
        let host = AppHost::new(TauriBridge::new(app.clone()), Some(window.to_owned()));
        let repl = Repl::new(host.clone(), repl_limits())
            .map_err(|e| CmdError::new("script", e.to_string()))?;
        let r = Arc::new(WindowRepl {
            host,
            repl: Mutex::new(repl),
        });
        self.repls.lock().insert(window.to_owned(), r.clone());
        Ok(r)
    }

    /// Drops the REPL of `window` (fresh globals next time).
    pub fn reset_repl(&self, window: &str) {
        self.repls.lock().remove(window);
    }

    pub fn set_context(&self, window: &str, ctx: UiContext) {
        *self.context.lock() = Some((window.to_owned(), ctx));
    }

    fn last_context(&self) -> (Option<String>, UiContext) {
        match self.context.lock().clone() {
            Some((w, c)) => (Some(w), c),
            None => (None, UiContext::default()),
        }
    }
}

// ---------------------------------------------------------------- plugins (SCR-04, SCR-05)

pub fn plugins_dir(state: &AppState) -> PathBuf {
    state.config_dir.read().join("plugins")
}

pub fn templates_dir(state: &AppState) -> PathBuf {
    state.config_dir.read().join("templates")
}

/// (Re)loads every plugin from the plugins directory.
pub fn reload_plugins(state: &AppState, scripts: &ScriptState) -> PluginListing {
    let dir = plugins_dir(state);
    let (registry, failures) = PluginRegistry::load_dir(&dir, hexa_script::PluginLimits::default());
    *scripts.plugins.write() = Some(PluginSet { registry, failures });
    listing(state, scripts)
}

/// Current plugin list, loading on first use.
pub fn listing(state: &AppState, scripts: &ScriptState) -> PluginListing {
    if scripts.plugins.read().is_none() {
        return reload_plugins(state, scripts);
    }
    let guard = scripts.plugins.read();
    let set = guard.as_ref();
    PluginListing {
        dir: plugins_dir(state).to_string_lossy().into_owned(),
        plugins: set.map(|s| s.registry.manifests()).unwrap_or_default(),
        failures: set.map(|s| s.failures.clone()).unwrap_or_default(),
    }
}

// ---------------------------------------------------------------- local API (SCR-09)

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiFile {
    port: u16,
    token: String,
}

/// State of the local API as shown in the settings.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiStatus {
    pub running: bool,
    pub port: u16,
    pub token: String,
    pub url: String,
    pub error: Option<String>,
}

struct ApiServer {
    port: u16,
    stop: Arc<AtomicBool>,
    api: Arc<RwLock<LocalApi>>,
    thread: Option<JoinHandle<()>>,
}

impl ApiServer {
    fn shutdown(mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn api_file(state: &AppState) -> PathBuf {
    state.config_dir.read().join("api.json")
}

/// A fresh random bearer token (256 bits, hex).
pub fn new_token() -> CmdResult<String> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|e| CmdError::new("internal", e.to_string()))?;
    Ok(b.iter().fold(String::with_capacity(64), |mut s, x| {
        let _ = write!(s, "{x:02x}");
        s
    }))
}

fn load_api_config(state: &AppState) -> CmdResult<ApiFile> {
    let path = api_file(state);
    if let Ok(text) = std::fs::read_to_string(&path)
        && let Ok(cfg) = serde_json::from_str::<ApiFile>(&text)
        && !cfg.token.is_empty()
    {
        return Ok(cfg);
    }
    let cfg = ApiFile {
        port: DEFAULT_API_PORT,
        token: new_token()?,
    };
    save_api_config(state, &cfg)?;
    Ok(cfg)
}

fn save_api_config(state: &AppState, cfg: &ApiFile) -> CmdResult<()> {
    let path = api_file(state);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(cfg).map_err(|e| CmdError::invalid(e.to_string()))?;
    std::fs::write(&path, json)?;
    Ok(())
}

pub fn api_status(state: &AppState, scripts: &ScriptState) -> CmdResult<ApiStatus> {
    let cfg = load_api_config(state)?;
    let server = scripts.api.lock();
    let port = server.as_ref().map_or(cfg.port, |s| s.port);
    Ok(ApiStatus {
        running: server.is_some(),
        port,
        token: cfg.token,
        url: format!("http://127.0.0.1:{port}/"),
        error: scripts.api_error.lock().clone(),
    })
}

/// Starts (or restarts on another port) the local API server.
pub fn api_start(app: &AppHandle, port: Option<u16>) -> CmdResult<ApiStatus> {
    let state = app.state::<AppState>();
    let scripts = app.state::<ScriptState>();
    let mut cfg = load_api_config(&state)?;
    if let Some(p) = port
        && p != cfg.port
    {
        cfg.port = p;
        save_api_config(&state, &cfg)?;
    }
    {
        let mut slot = scripts.api.lock();
        if slot.as_ref().is_some_and(|s| s.port == cfg.port) {
            drop(slot);
            return api_status(&state, &scripts);
        }
        if let Some(old) = slot.take() {
            old.shutdown();
        }
        match spawn_server(app.clone(), cfg.port, &cfg.token) {
            Ok(server) => {
                *slot = Some(server);
                *scripts.api_error.lock() = None;
            }
            Err(e) => {
                *scripts.api_error.lock() = Some(e.clone());
                return Err(CmdError::new("io", e));
            }
        }
    }
    api_status(&state, &scripts)
}

/// Changes the port; a running server moves to it.
pub fn api_set_port(app: &AppHandle, port: u16) -> CmdResult<ApiStatus> {
    let state = app.state::<AppState>();
    let scripts = app.state::<ScriptState>();
    if scripts.api.lock().is_some() {
        return api_start(app, Some(port));
    }
    let mut cfg = load_api_config(&state)?;
    cfg.port = port;
    save_api_config(&state, &cfg)?;
    api_status(&state, &scripts)
}

pub fn api_stop(app: &AppHandle) -> CmdResult<ApiStatus> {
    let state = app.state::<AppState>();
    let scripts = app.state::<ScriptState>();
    let old = scripts.api.lock().take();
    if let Some(old) = old {
        old.shutdown();
    }
    *scripts.api_error.lock() = None;
    api_status(&state, &scripts)
}

pub fn api_regenerate(app: &AppHandle) -> CmdResult<ApiStatus> {
    let state = app.state::<AppState>();
    let scripts = app.state::<ScriptState>();
    let mut cfg = load_api_config(&state)?;
    cfg.token = new_token()?;
    save_api_config(&state, &cfg)?;
    if let Some(server) = scripts.api.lock().as_ref() {
        *server.api.write() = LocalApi::new(ApiConfig::new(cfg.token.clone()));
    }
    api_status(&state, &scripts)
}

fn spawn_server(app: AppHandle, port: u16, token: &str) -> Result<ApiServer, String> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .map_err(|e| format!("не удалось занять 127.0.0.1:{port}: {e}"))?;
    let port = listener.local_addr().map_err(err)?.port();
    listener.set_nonblocking(true).map_err(err)?;
    let stop = Arc::new(AtomicBool::new(false));
    let api = Arc::new(RwLock::new(LocalApi::new(ApiConfig::new(token))));
    let thread = {
        let stop = stop.clone();
        let api = api.clone();
        std::thread::spawn(move || {
            let bridge: Arc<dyn Bridge> = TauriBridge::new(app.clone());
            while !stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let api = api.read().clone();
                        let bridge = bridge.clone();
                        let app = app.clone();
                        std::thread::spawn(move || {
                            let (window, ctx) = app.state::<ScriptState>().last_context();
                            let host = AppHost::new(bridge, window);
                            host.begin(ctx, None, None);
                            serve_connection(stream, &api, &host);
                            host.finish();
                        });
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(40));
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(200)),
                }
            }
        })
    };
    Ok(ApiServer {
        port,
        stop,
        api,
        thread: Some(thread),
    })
}

/// A parsed HTTP/1.1 request.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: String,
    pub path: String,
    pub host: Option<String>,
    pub authorization: Option<String>,
    pub origin: Option<String>,
    pub body: Vec<u8>,
}

/// Reads one request; the error is the HTTP status to answer with.
pub fn read_request(reader: &mut impl BufRead) -> Result<HttpRequest, (u16, &'static str)> {
    fn read_line(reader: &mut impl BufRead, line: &mut String) -> Result<(), (u16, &'static str)> {
        line.clear();
        let n = (&mut *reader)
            .take(16 * 1024)
            .read_line(line)
            .map_err(|_| (400, "bad request"))?;
        if n == 0 || !line.ends_with('\n') {
            return Err((400, "bad request"));
        }
        Ok(())
    }
    let mut line = String::new();
    read_line(reader, &mut line)?;
    let mut parts = line.split_whitespace();
    let mut req = HttpRequest {
        method: parts.next().ok_or((400, "bad request"))?.to_owned(),
        path: parts.next().ok_or((400, "bad request"))?.to_owned(),
        ..HttpRequest::default()
    };
    let mut content_length = 0usize;
    for _ in 0..128 {
        read_line(reader, &mut line)?;
        let l = line.trim_end();
        if l.is_empty() {
            if content_length > MAX_API_BODY {
                return Err((413, "request too large"));
            }
            let mut body = vec![0; content_length];
            reader
                .read_exact(&mut body)
                .map_err(|_| (400, "bad request"))?;
            req.body = body;
            return Ok(req);
        }
        let Some((name, value)) = l.split_once(':') else {
            return Err((400, "bad request"));
        };
        let value = value.trim().to_owned();
        match name.trim().to_ascii_lowercase().as_str() {
            "content-length" => {
                content_length = value.parse().map_err(|_| (400, "bad request"))?;
            }
            "authorization" => req.authorization = Some(value),
            "host" => req.host = Some(value),
            "origin" => req.origin = Some(value),
            "transfer-encoding" => return Err((501, "chunked bodies are not supported")),
            _ => {}
        }
    }
    Err((431, "too many headers"))
}

/// Only requests addressed to the loopback name are served (DNS rebinding).
fn local_host(host: Option<&str>) -> bool {
    let Some(host) = host else { return false };
    let name = host
        .rsplit_once(':')
        .filter(|(_, p)| p.bytes().all(|b| b.is_ascii_digit()))
        .map_or(host, |(n, _)| n);
    matches!(name, "127.0.0.1" | "localhost" | "[::1]")
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        422 => "Unprocessable Entity",
        431 => "Request Header Fields Too Large",
        501 => "Not Implemented",
        _ => "Error",
    }
}

fn write_response(stream: &mut impl Write, status: u16, body: &str) {
    let head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        reason(status),
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}

fn error_body(message: &str) -> String {
    serde_json::json!({ "ok": false, "error": { "code": "badRequest", "message": message } })
        .to_string()
}

/// Answers one HTTP request with the JSON API.
pub fn handle_http(req: &HttpRequest, api: &LocalApi, host: &Arc<dyn ScriptHost>) -> (u16, String) {
    if !local_host(req.host.as_deref()) || req.origin.is_some() {
        return (
            403,
            error_body("only local, non-browser clients are allowed"),
        );
    }
    match req.method.as_str() {
        "GET" if req.path == "/" => (
            200,
            serde_json::json!({ "name": "Hexa", "apiVersion": hexa_script::API_VERSION })
                .to_string(),
        ),
        "POST" => {
            let Ok(body) = std::str::from_utf8(&req.body) else {
                return (400, error_body("body is not UTF-8"));
            };
            let r = api.handle_request(req.authorization.as_deref(), body, host);
            (r.http_status(), r.to_json())
        }
        _ => (405, error_body("use POST with a JSON request")),
    }
}

fn serve_connection(stream: TcpStream, api: &LocalApi, host: &Arc<AppHost>) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(15)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(15)));
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    let host: Arc<dyn ScriptHost> = host.clone();
    let (status, body) = match read_request(&mut reader) {
        Ok(req) => handle_http(&req, api, &host),
        Err((status, msg)) => (status, error_body(msg)),
    };
    write_response(&mut writer, status, &body);
    let _ = writer.shutdown(Shutdown::Both);
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use hexa_core::Tree;
    use hexa_core::piece::Piece;
    use hexa_script::ScriptEngine;

    struct TestBridge {
        state: AppState,
        events: Mutex<Vec<UiEvent>>,
        prompts: Prompts,
    }

    impl Bridge for TestBridge {
        fn state(&self) -> &AppState {
            &self.state
        }
        fn emit(&self, _window: Option<&str>, event: &UiEvent) {
            self.events.lock().push(event.clone());
        }
        fn adopt(&self, doc: Document) -> DocInfo {
            let entry = self.state.add(doc);
            let d = entry.doc.lock();
            entry.info(&d)
        }
        fn confirm(&self, _title: &str, _message: &str) -> bool {
            true
        }
        fn message(&self, _title: &str, _message: &str) {}
        fn prompts(&self) -> &Prompts {
            &self.prompts
        }
    }

    fn setup(bytes: &[u8]) -> (Arc<TestBridge>, Arc<AppHost>, u64) {
        let bridge = Arc::new(TestBridge {
            state: AppState::default(),
            events: Mutex::new(Vec::new()),
            prompts: Prompts::default(),
        });
        let doc = Document::from_tree("t.bin", Tree::from_piece(Piece::bytes(bytes.to_vec())));
        let id = bridge.state.add(doc).id;
        let host = AppHost::new(bridge.clone(), Some("main".into()));
        host.begin(
            UiContext {
                active: Some(id),
                docs: vec![DocCtx {
                    id,
                    cursor: 2,
                    selection: Some(Span { start: 1, end: 3 }),
                }],
            },
            None,
            None,
        );
        (bridge, host, id)
    }

    fn run(host: &Arc<AppHost>, src: &str) -> Vec<String> {
        let engine = ScriptEngine::new(host.clone(), ScriptLimits::default()).unwrap();
        let out = engine.run(src, "test");
        host.finish();
        assert!(out.error.is_none(), "{:?}", out.error);
        out.values
    }

    #[test]
    fn reads_edits_and_reports_changes() {
        let (bridge, host, id) = setup(b"Hello, world");
        let v = run(
            &host,
            r#"
            local s = hexa.selection()
            hexa.write(0, "J")
            hexa.insert(hexa.len(), "!")
            return hexa.read(0, 5), s.offset, s.len, hexa.cursor(), hexa.len()
            "#,
        );
        assert_eq!(v, ["\"Jello\"", "1", "2", "2", "13"]);
        let entry = bridge.state.get(id).unwrap();
        assert_eq!(
            entry.view.read().tree.read_vec(0, 64).unwrap(),
            b"Jello, world!"
        );
        let events = bridge.events.lock();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, UiEvent::DocChanged { doc } if *doc == id))
        );
    }

    #[test]
    fn search_hash_inspect_and_ui_events() {
        let (bridge, host, id) = setup(b"abcabc\x01\x00\x00\x00");
        let v = run(
            &host,
            r#"
            local all = hexa.find_all("bc")
            local n = hexa.find_next({hex = "61 62"}, 1)
            hexa.go_to(4)
            hexa.select(1, 2)
            hexa.command("view.palette")
            return #all, n.offset, hexa.inspect(6, "u32le"), hexa.hash("crc32", 0, 3)
            "#,
        );
        assert_eq!(v, ["2", "3", "\"1\"", "\"352441c2\""]);
        let events = bridge.events.lock();
        assert!(events.iter().any(|e| matches!(
            e,
            UiEvent::Cursor { doc, offset: 4, scroll: true } if *doc == id
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            UiEvent::Select {
                selection: Some(Span { start: 1, end: 3 }),
                ..
            }
        )));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, UiEvent::Command { id, .. } if id == "view.palette"))
        );
    }

    #[test]
    fn undo_transform_and_errors() {
        let (_bridge, host, _id) = setup(b"\x01\x02");
        let v = run(
            &host,
            r#"
            hexa.write(0, "\255")
            local undone = hexa.undo()
            local x = hexa.transform('{"steps":[{"kind":"not"}]}', 0, 2)
            local ok = pcall(hexa.template, "png")
            return undone, hexa.hex(hexa.read(0, 2)), hexa.hex(x), ok
            "#,
        );
        assert_eq!(v[0], "true");
        assert_eq!(v[1].to_ascii_lowercase(), "\"0102\"");
        assert_eq!(v[2].to_ascii_lowercase(), "\"fefd\"");
        assert_eq!(v[3], "false");
    }

    #[test]
    fn prompt_waits_for_the_reply() {
        let (bridge, host, _id) = setup(b"");
        let b = bridge.clone();
        let t = std::thread::spawn(move || {
            for _ in 0..200 {
                let req = b.events.lock().iter().find_map(|e| match e {
                    UiEvent::Prompt { req, .. } => Some(*req),
                    _ => None,
                });
                if let Some(req) = req {
                    assert!(b.prompts.reply(req, Some("42".into())));
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("no prompt");
        });
        let v = run(&host, r#"return hexa.prompt("Число?")"#);
        t.join().unwrap();
        assert_eq!(v, ["\"42\""]);
    }

    #[test]
    fn value_types() {
        assert_eq!(
            parse_value_type("u32le").unwrap(),
            (InspectType::U32, Endian::Little)
        );
        assert_eq!(
            parse_value_type("f64be").unwrap(),
            (InspectType::F64, Endian::Big)
        );
        assert_eq!(parse_value_type("uint16").unwrap().0, InspectType::U16);
        assert!(parse_value_type("nope").is_err());
    }

    #[test]
    fn http_requests_are_parsed_and_guarded() {
        let raw = b"POST / HTTP/1.1\r\nHost: 127.0.0.1:4392\r\nAuthorization: Bearer t0k\r\nContent-Length: 5\r\n\r\nhello";
        let req = read_request(&mut &raw[..]).unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(req.authorization.as_deref(), Some("Bearer t0k"));
        assert_eq!(req.body, b"hello");
        assert!(local_host(Some("localhost:1")));
        assert!(!local_host(Some("evil.example:4392")));

        let (_bridge, host, _id) = setup(b"xyz");
        let host: Arc<dyn ScriptHost> = host;
        let api = LocalApi::new(ApiConfig::new("t0k"));
        let mut req = HttpRequest {
            method: "POST".into(),
            path: "/".into(),
            host: Some("127.0.0.1".into()),
            authorization: Some("Bearer t0k".into()),
            body: br#"{"id":1,"method":"readRange","params":{"offset":0,"len":3}}"#.to_vec(),
            ..HttpRequest::default()
        };
        let (status, body) = handle_http(&req, &api, &host);
        assert_eq!(status, 200, "{body}");
        assert!(body.contains("78797a"), "{body}");
        req.authorization = Some("Bearer nope".into());
        assert_eq!(handle_http(&req, &api, &host).0, 401);
        req.origin = Some("https://evil.example".into());
        assert_eq!(handle_http(&req, &api, &host).0, 403);
    }
}
