//! Opening, reading, editing and saving documents.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use hexa_core::document::{self, ChangeSpan, Fill, ShareMode};
use hexa_core::history::HistoryItem;
use hexa_core::inspector::{self, Endian, InspectType};
use hexa_core::save::{self, SaveOptions, SaveReport};
use hexa_core::{Change, Document, EditMode, OpenOptions};
use serde::{Deserialize, Serialize};
use tauri::ipc::{Channel, Response};
use tauri::{AppHandle, State};

use crate::error::{CmdError, CmdResult};
use crate::state::{AppState, DocEntry, DocInfo};
use crate::{recent, recovery, watch};

/// Largest read the grid may request at once.
const MAX_READ: u64 = 16 << 20;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OpenOpts {
    pub mode: Option<EditMode>,
    pub load_in_memory: bool,
    pub share: ShareMode,
    pub fixed_size: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum NewDoc {
    Empty,
    Sized {
        len: u64,
        fill: Fill,
    },
    Bytes {
        bytes: Vec<u8>,
        name: Option<String>,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditResult {
    pub change: Option<Change>,
    pub info: DocInfo,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum EditOp {
    Overwrite {
        pos: u64,
        bytes: Vec<u8>,
    },
    Insert {
        pos: u64,
        bytes: Vec<u8>,
    },
    Delete {
        start: u64,
        end: u64,
    },
    Replace {
        start: u64,
        end: u64,
        bytes: Vec<u8>,
    },
    Fill {
        start: u64,
        end: u64,
        fill: Fill,
    },
    InsertFill {
        pos: u64,
        len: u64,
        fill: Fill,
    },
    InsertFile {
        pos: u64,
        path: PathBuf,
    },
    Resize {
        len: u64,
        fill: Fill,
    },
    ByteSwap {
        start: u64,
        end: u64,
        width: usize,
    },
    Reverse {
        start: u64,
        end: u64,
    },
    Move {
        start: u64,
        end: u64,
        to: u64,
    },
}

pub(crate) fn with_doc<T>(
    state: &AppState,
    id: u64,
    f: impl FnOnce(&Arc<DocEntry>, &mut Document) -> CmdResult<T>,
) -> CmdResult<T> {
    let entry = state.get(id)?;
    if entry.saving.load(Ordering::Acquire) {
        return Err(CmdError::new("busy", "the document is being saved"));
    }
    let mut doc = entry.doc.lock();
    f(&entry, &mut doc)
}

/// Applies a mutation, publishes the new state and returns the result.
pub(crate) fn mutate(
    state: &AppState,
    id: u64,
    f: impl FnOnce(&mut Document) -> hexa_core::Result<Option<Change>>,
) -> CmdResult<EditResult> {
    with_doc(state, id, |entry, doc| {
        let change = f(doc)?;
        entry.publish(doc);
        Ok(EditResult {
            change,
            info: entry.info(doc),
        })
    })
}

fn opened(app: &AppHandle, state: &AppState, doc: Document) -> DocInfo {
    let path = doc.path().map(PathBuf::from);
    let entry = state.add(doc);
    if let Some(p) = &path {
        watch::watch(app, entry.id, p);
        recent::touch(state, p);
    }
    let doc = entry.doc.lock();
    entry.info(&doc)
}

#[tauri::command]
pub fn doc_open(
    app: AppHandle,
    state: State<'_, AppState>,
    path: PathBuf,
    opts: Option<OpenOpts>,
) -> CmdResult<DocInfo> {
    let opts = opts.unwrap_or_default();
    let doc = Document::open(
        &path,
        &OpenOptions {
            mode: opts.mode.unwrap_or(EditMode::Overwrite),
            fixed_size: opts.fixed_size,
            load_in_memory: opts.load_in_memory,
            share: opts.share,
            ..OpenOptions::default()
        },
    )?;
    Ok(opened(&app, &state, doc))
}

#[tauri::command]
pub fn doc_new(app: AppHandle, state: State<'_, AppState>, kind: NewDoc) -> CmdResult<DocInfo> {
    let doc = match kind {
        NewDoc::Empty => Document::new_empty(),
        NewDoc::Sized { len, fill } => Document::new_sized(len, &fill)?,
        NewDoc::Bytes { bytes, name } => {
            let mut d = Document::new_empty();
            if !bytes.is_empty() {
                d.insert(0, &bytes)?;
            }
            if let Some(name) = name {
                let _ = name; // new buffers are always "Untitled" until saved
            }
            d
        }
    };
    Ok(opened(&app, &state, doc))
}

/// A window starts showing the document.
#[tauri::command]
pub fn doc_attach(state: State<'_, AppState>, id: u64) -> CmdResult<DocInfo> {
    let entry = state.get(id)?;
    entry.refs.fetch_add(1, Ordering::AcqRel);
    let doc = entry.doc.lock();
    Ok(entry.info(&doc))
}

/// A window stops showing the document; the last one closes it.
#[tauri::command]
pub fn doc_release(app: AppHandle, state: State<'_, AppState>, id: u64) -> CmdResult<()> {
    let entry = state.get(id)?;
    let prev = entry.refs.fetch_sub(1, Ordering::AcqRel);
    if prev <= 1 {
        state.remove(id);
        watch::unwatch(&app, id);
        recovery::forget(&state, id);
    }
    Ok(())
}

#[tauri::command]
pub fn doc_info(state: State<'_, AppState>, id: u64) -> CmdResult<DocInfo> {
    let entry = state.get(id)?;
    let doc = entry.doc.lock();
    Ok(entry.info(&doc))
}

/// Raw bytes for the grid, as an `ArrayBuffer` (no JSON).
#[tauri::command]
pub fn doc_read(state: State<'_, AppState>, id: u64, offset: u64, len: u64) -> CmdResult<Response> {
    let entry = state.get(id)?;
    let tree = entry.view.read().tree.clone();
    let n = len.min(MAX_READ).min(tree.len().saturating_sub(offset));
    let mut buf = vec![0u8; n as usize];
    tree.read(offset, &mut buf)
        .map_err(|e| CmdError::from(hexa_core::Error::from(e)))?;
    Ok(Response::new(buf))
}

#[tauri::command]
pub fn doc_changes(
    state: State<'_, AppState>,
    id: u64,
    start: u64,
    end: u64,
) -> CmdResult<Vec<ChangeSpan>> {
    let entry = state.get(id)?;
    let v = entry.view.read();
    Ok(document::changes_in(&v.tree, &v.base, start, end))
}

#[tauri::command]
pub fn doc_type(
    state: State<'_, AppState>,
    id: u64,
    pos: u64,
    byte: u8,
    amend: bool,
) -> CmdResult<EditResult> {
    mutate(&state, id, |d| d.type_byte(pos, byte, amend).map(Some))
}

#[tauri::command]
pub fn doc_edit(state: State<'_, AppState>, id: u64, op: EditOp) -> CmdResult<EditResult> {
    mutate(&state, id, |d| {
        let c = match op {
            EditOp::Overwrite { pos, bytes } => d.overwrite(pos, &bytes)?,
            EditOp::Insert { pos, bytes } => d.insert(pos, &bytes)?,
            EditOp::Delete { start, end } => d.delete(start, end)?,
            EditOp::Replace { start, end, bytes } => d.replace(start, end, &bytes)?,
            EditOp::Fill { start, end, fill } => d.fill(start, end, &fill)?,
            EditOp::InsertFill { pos, len, fill } => d.insert_fill(pos, len, &fill)?,
            EditOp::InsertFile { pos, path } => d.insert_file(pos, &path)?,
            EditOp::Resize { len, fill } => d.resize(len, &fill)?,
            EditOp::ByteSwap { start, end, width } => d.byte_swap(start, end, width)?,
            EditOp::Reverse { start, end } => d.reverse(start, end)?,
            EditOp::Move { start, end, to } => d.move_block(start, end, to)?,
        };
        Ok(Some(c))
    })
}

#[tauri::command]
pub fn doc_undo(state: State<'_, AppState>, id: u64) -> CmdResult<EditResult> {
    mutate(&state, id, |d| Ok(d.undo()))
}

#[tauri::command]
pub fn doc_redo(state: State<'_, AppState>, id: u64) -> CmdResult<EditResult> {
    mutate(&state, id, |d| Ok(d.redo()))
}

#[tauri::command]
pub fn doc_history(state: State<'_, AppState>, id: u64) -> CmdResult<Vec<HistoryItem>> {
    with_doc(&state, id, |_, d| Ok(d.history()))
}

#[tauri::command]
pub fn doc_history_goto(
    state: State<'_, AppState>,
    id: u64,
    entry: usize,
) -> CmdResult<EditResult> {
    mutate(&state, id, |d| {
        d.history_goto(entry);
        Ok(None)
    })
}

/// Reverts one earlier edit, keeping later ones (CORE-13).
#[tauri::command]
pub fn doc_history_revert(
    state: State<'_, AppState>,
    id: u64,
    entry: usize,
) -> CmdResult<EditResult> {
    mutate(&state, id, |d| d.revert_entry(entry).map(Some))
}

#[tauri::command]
pub fn doc_set_mode(state: State<'_, AppState>, id: u64, mode: EditMode) -> CmdResult<DocInfo> {
    Ok(mutate(&state, id, |d| d.set_mode(mode).map(|()| None))?.info)
}

#[tauri::command]
pub fn doc_set_fixed(state: State<'_, AppState>, id: u64, fixed: bool) -> CmdResult<DocInfo> {
    Ok(mutate(&state, id, |d| {
        d.set_fixed_size(fixed);
        Ok(None)
    })?
    .info)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveProgress {
    pub done: u64,
    pub total: u64,
}

fn run_save(
    state: &AppState,
    id: u64,
    channel: Option<Channel<SaveProgress>>,
    f: impl FnOnce(
        &mut Document,
        hexa_core::task::Progress<'_>,
        &hexa_core::Cancel,
    ) -> hexa_core::Result<Option<SaveReport>>,
) -> CmdResult<(Option<SaveReport>, DocInfo)> {
    let entry = state.get(id)?;
    if entry.saving.swap(true, Ordering::AcqRel) {
        return Err(CmdError::new("busy", "already saving"));
    }
    let (job, cancel) = state.start_job();
    let result = {
        let mut doc = entry.doc.lock();
        let last = std::sync::atomic::AtomicU64::new(0);
        let progress = |done: u64, total: u64| {
            // Throttle to ~1% steps.
            let step = (total / 100).max(1 << 20);
            if done == total || done >= last.load(Ordering::Relaxed) + step {
                last.store(done, Ordering::Relaxed);
                if let Some(ch) = &channel {
                    let _ = ch.send(SaveProgress { done, total });
                }
            }
        };
        let r = f(&mut doc, &progress, &cancel);
        entry.publish(&doc);
        r.map(|rep| (rep, entry.info(&doc)))
    };
    state.end_job(job);
    entry.saving.store(false, Ordering::Release);
    let (rep, info) = result?;
    if !info.modified {
        recovery::forget(state, id);
    }
    Ok((rep, info))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveResult {
    pub report: Option<SaveReport>,
    pub info: DocInfo,
}

#[tauri::command]
pub async fn doc_save(
    app: AppHandle,
    state: State<'_, AppState>,
    id: u64,
    opts: Option<SaveOptions>,
    channel: Channel<SaveProgress>,
) -> CmdResult<SaveResult> {
    let opts = opts.unwrap_or_default();
    let state = state.inner();
    let (report, info) = run_save(state, id, Some(channel), |d, p, c| {
        d.save(&opts, p, c).map(Some)
    })?;
    if let Some(p) = &info.path {
        watch::rewatch(&app, id, std::path::Path::new(p));
        recent::touch(state, std::path::Path::new(p));
    }
    Ok(SaveResult { report, info })
}

#[tauri::command]
pub async fn doc_save_as(
    app: AppHandle,
    state: State<'_, AppState>,
    id: u64,
    path: PathBuf,
    channel: Channel<SaveProgress>,
) -> CmdResult<SaveResult> {
    let state = state.inner();
    let (_, info) = run_save(state, id, Some(channel), |d, p, c| {
        d.save_as(&path, p, c).map(|()| None)
    })?;
    watch::unwatch(&app, id);
    watch::watch(&app, id, &path);
    recent::touch(state, &path);
    Ok(SaveResult { report: None, info })
}

#[tauri::command]
pub async fn doc_save_copy(
    state: State<'_, AppState>,
    id: u64,
    path: PathBuf,
    start: Option<u64>,
    end: Option<u64>,
    channel: Channel<SaveProgress>,
) -> CmdResult<()> {
    let state = state.inner();
    run_save(state, id, Some(channel), |d, p, c| match (start, end) {
        (Some(s), Some(e)) => d.save_range(s, e, &path, p, c).map(|()| None),
        _ => d.save_copy(&path, p, c).map(|()| None),
    })?;
    Ok(())
}

/// Saves through a privileged helper when the file is not writable (FILE-05).
#[tauri::command]
pub async fn doc_save_elevated(
    app: AppHandle,
    state: State<'_, AppState>,
    id: u64,
) -> CmdResult<DocInfo> {
    let state = state.inner();
    let entry = state.get(id)?;
    let path = entry
        .doc
        .lock()
        .path()
        .map(PathBuf::from)
        .ok_or_else(|| CmdError::new("noPath", "document has no file"))?;
    let tmp = std::env::temp_dir().join(format!("hexa-elevated-{}-{id}.tmp", std::process::id()));
    run_save(state, id, None, |d, p, c| {
        d.save_copy(&tmp, p, c).map(|()| None)
    })?;
    let res = crate::elevate::copy_privileged(&tmp, &path);
    let _ = std::fs::remove_file(&tmp);
    res?;
    // The file was rewritten underneath us: reopen it.
    reload_entry(&app, &entry)
}

fn reload_entry(app: &AppHandle, entry: &Arc<DocEntry>) -> CmdResult<DocInfo> {
    let mut doc = entry.doc.lock();
    let path = doc
        .path()
        .map(PathBuf::from)
        .ok_or_else(|| CmdError::new("noPath", "document has no file"))?;
    let mode = doc.mode();
    let fresh = Document::open(
        &path,
        &OpenOptions {
            mode: if mode == EditMode::ReadOnly {
                EditMode::ReadOnly
            } else {
                mode
            },
            ..OpenOptions::default()
        },
    )?;
    *doc = fresh;
    entry.publish(&doc);
    watch::rewatch(app, entry.id, &path);
    Ok(entry.info(&doc))
}

/// Re-reads the file from disk, dropping edits (FILE-06 "reload").
#[tauri::command]
pub fn doc_reload(app: AppHandle, state: State<'_, AppState>, id: u64) -> CmdResult<DocInfo> {
    let entry = state.get(id)?;
    reload_entry(&app, &entry)
}

/// Keeps the in-editor state and accepts the new disk state (FILE-06 "keep mine").
#[tauri::command]
pub fn doc_keep_mine(state: State<'_, AppState>, id: u64) -> CmdResult<DocInfo> {
    with_doc(&state, id, |entry, d| {
        d.acknowledge_disk_state();
        Ok(entry.info(d))
    })
}

#[tauri::command]
pub fn doc_set_watch_mode(
    state: State<'_, AppState>,
    id: u64,
    follow: Option<bool>,
    auto_reload: Option<bool>,
) -> CmdResult<DocInfo> {
    let entry = state.get(id)?;
    if let Some(f) = follow {
        entry.follow.store(f, Ordering::Relaxed);
    }
    if let Some(a) = auto_reload {
        entry.auto_reload.store(a, Ordering::Relaxed);
    }
    let doc = entry.doc.lock();
    Ok(entry.info(&doc))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileProps {
    pub path: Option<String>,
    pub name: String,
    pub size: u64,
    pub disk_size: Option<u64>,
    pub created_ms: Option<u64>,
    pub modified_ms: Option<u64>,
    pub accessed_ms: Option<u64>,
    pub readonly: bool,
    pub permissions: Option<String>,
    pub owner: Option<String>,
    pub pieces: u64,
    pub history: usize,
    pub disk_changed: bool,
}

fn ms(t: std::io::Result<std::time::SystemTime>) -> Option<u64> {
    t.ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as u64)
}

/// File properties (FILE-12).
#[tauri::command]
pub fn doc_props(state: State<'_, AppState>, id: u64) -> CmdResult<FileProps> {
    with_doc(&state, id, |_, d| {
        let meta = d.path().and_then(|p| std::fs::metadata(p).ok());
        #[cfg(unix)]
        let (permissions, owner) = {
            use std::os::unix::fs::MetadataExt;
            meta.as_ref().map_or((None, None), |m| {
                (
                    Some(format!("{:o}", m.mode() & 0o7777)),
                    Some(format!("uid {} · gid {}", m.uid(), m.gid())),
                )
            })
        };
        #[cfg(not(unix))]
        let (permissions, owner) = (None, None);
        Ok(FileProps {
            path: d.path().map(|p| p.to_string_lossy().into_owned()),
            name: d.name().to_owned(),
            size: d.len(),
            disk_size: meta.as_ref().map(std::fs::Metadata::len),
            created_ms: meta.as_ref().and_then(|m| ms(m.created())),
            modified_ms: meta.as_ref().and_then(|m| ms(m.modified())),
            accessed_ms: meta.as_ref().and_then(|m| ms(m.accessed())),
            readonly: meta.as_ref().is_some_and(|m| m.permissions().readonly()),
            permissions,
            owner,
            pieces: d.piece_count(),
            history: d.history().len(),
            disk_changed: d.disk_changed(),
        })
    })
}

#[tauri::command]
pub fn journal_pending(path: PathBuf) -> bool {
    save::has_pending_journal(&path)
}

#[tauri::command]
pub fn journal_rollback(path: PathBuf) -> CmdResult<()> {
    Ok(save::rollback_journal(&path)?)
}

#[tauri::command]
pub fn journal_discard(path: PathBuf) -> CmdResult<()> {
    Ok(save::discard_journal(&path)?)
}

#[tauri::command]
pub fn inspect(
    state: State<'_, AppState>,
    id: u64,
    pos: u64,
    endian: Endian,
) -> CmdResult<Vec<inspector::Row>> {
    let entry = state.get(id)?;
    let tree = entry.view.read().tree.clone();
    let bytes = tree
        .read_vec(pos, 16)
        .map_err(|e| CmdError::from(hexa_core::Error::from(e)))?;
    Ok(inspector::inspect(&bytes, endian))
}

/// Writes a value typed into the inspector (INSP-15).
#[tauri::command]
pub fn inspector_set(
    state: State<'_, AppState>,
    id: u64,
    pos: u64,
    ty: InspectType,
    text: String,
    endian: Endian,
) -> CmdResult<EditResult> {
    let bytes = inspector::encode(ty, &text, endian)?;
    mutate(&state, id, |d| {
        let len = d.len();
        if pos + bytes.len() as u64 > len && d.fixed_size() {
            return Err(hexa_core::Error::FixedSize);
        }
        d.overwrite(pos, &bytes).map(Some)
    })
}

// ---------------------------------------------------------------- marks (NAV-14…18)

#[tauri::command]
pub fn marks_list(state: State<'_, AppState>, id: u64) -> CmdResult<Vec<hexa_core::marks::Mark>> {
    let entry = state.get(id)?;
    let doc = entry.doc.lock();
    Ok(doc.marks().to_vec())
}

#[tauri::command]
pub fn marks_add(
    state: State<'_, AppState>,
    id: u64,
    mark: hexa_core::marks::Mark,
) -> CmdResult<u64> {
    let entry = state.get(id)?;
    let mut doc = entry.doc.lock();
    Ok(doc.add_mark(mark))
}

#[tauri::command]
pub fn marks_update(
    state: State<'_, AppState>,
    id: u64,
    mark: hexa_core::marks::Mark,
) -> CmdResult<bool> {
    let entry = state.get(id)?;
    let mut doc = entry.doc.lock();
    Ok(doc.update_mark(mark))
}

#[tauri::command]
pub fn marks_remove(state: State<'_, AppState>, id: u64, mark: u64) -> CmdResult<bool> {
    let entry = state.get(id)?;
    let mut doc = entry.doc.lock();
    Ok(doc.remove_mark(mark))
}

#[tauri::command]
pub fn marks_set(
    state: State<'_, AppState>,
    id: u64,
    marks: Vec<hexa_core::marks::Mark>,
) -> CmdResult<Vec<hexa_core::marks::Mark>> {
    let entry = state.get(id)?;
    let mut doc = entry.doc.lock();
    doc.set_marks(marks);
    Ok(doc.marks().to_vec())
}

/// Extended inspector rows (INSP-04…13, INSP-19 for selections).
#[tauri::command]
pub fn inspect_ext(
    state: State<'_, AppState>,
    id: u64,
    pos: u64,
    len: Option<u64>,
    endian: Endian,
    q_int: Option<u32>,
    q_frac: Option<u32>,
) -> CmdResult<Vec<hexa_core::inspector_ext::ExtRow>> {
    let entry = state.get(id)?;
    let tree = entry.view.read().tree.clone();
    let n = len.unwrap_or(64).min(4096) as usize;
    let bytes = tree
        .read_vec(pos, n)
        .map_err(|e| CmdError::from(hexa_core::Error::from(e)))?;
    Ok(hexa_core::inspector_ext::inspect_ext(
        &bytes,
        endian,
        (q_int.unwrap_or(16), q_frac.unwrap_or(16)),
    ))
}

/// Plans replacements for preview (SRCH-20…22).
#[tauri::command]
pub async fn replace_plan(
    state: State<'_, AppState>,
    id: u64,
    query: hexa_core::search::Query,
    scope: Option<hexa_core::search::Scope>,
    replacement: hexa_core::search::Replacement,
    max: Option<usize>,
) -> CmdResult<Vec<hexa_core::search::Planned>> {
    let entry = state.get(id)?;
    let tree = entry.view.read().tree.clone();
    let matcher = hexa_core::search::compile(&query)?;
    let scope = scope.unwrap_or_else(|| hexa_core::search::Scope::whole(&tree));
    let max = max.unwrap_or(100_000);
    let r = tauri::async_runtime::spawn_blocking(move || {
        hexa_core::search::plan_replacements(
            &tree,
            &matcher,
            scope,
            &replacement,
            max,
            &hexa_core::Cancel::new(),
        )
    })
    .await
    .map_err(|e| CmdError::new("internal", e.to_string()))?;
    Ok(r?)
}

#[tauri::command]
pub fn replace_apply(
    state: State<'_, AppState>,
    id: u64,
    plan: Vec<PlannedIn>,
) -> CmdResult<EditResult> {
    let plan: Vec<hexa_core::search::Planned> = plan
        .into_iter()
        .map(|p| hexa_core::search::Planned {
            offset: p.offset,
            len: p.len,
            old: Vec::new(),
            new: p.new,
        })
        .collect();
    mutate(&state, id, |d| d.apply_replacements(&plan).map(Some))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedIn {
    pub offset: u64,
    pub len: u64,
    pub new: Vec<u8>,
}

/// Long runs of one byte value, for folding repeats (DISP-24).
#[tauri::command]
pub async fn find_runs(
    state: State<'_, AppState>,
    id: u64,
    min_len: u64,
    max: usize,
) -> CmdResult<Vec<hexa_core::search::Run>> {
    let entry = state.get(id)?;
    let tree = entry.view.read().tree.clone();
    let r = tauri::async_runtime::spawn_blocking(move || {
        hexa_core::search::find_runs(&tree, min_len.max(2), max, &hexa_core::Cancel::new())
    })
    .await
    .map_err(|e| CmdError::new("internal", e.to_string()))?;
    Ok(r?)
}
