//! Search, hashes, clipboard formats, encodings and the inspector catalogue.

use hexa_core::encoding::{self, EncodingInfo};
use hexa_core::format::{self, CopyFormat, PasteCandidate};
use hexa_core::hash::{self, AlgoInfo, CrcParams, CrcPreset, HashResult};
use hexa_core::search::{self, Match, Query, Scope};
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State};

use super::doc::{EditResult, mutate};
use crate::clipboard;
use crate::error::{CmdError, CmdResult};
use crate::state::{AppState, InternalClip};
use hexa_core::Tree;
use hexa_core::piece::Piece;
use std::sync::atomic::Ordering;

/// Largest selection copied to the system clipboard as text.
const MAX_CLIPBOARD_TEXT: u64 = 16 << 20;

#[tauri::command]
pub fn job_cancel(state: State<'_, AppState>, job: u64) {
    state.cancel_job(job);
}

/// Next/previous match (SRCH-13, SRCH-15). Cancellable through `job`.
#[tauri::command]
pub async fn search_next(
    state: State<'_, AppState>,
    id: u64,
    query: Query,
    scope: Option<Scope>,
    cursor: u64,
    backwards: bool,
    wrap: bool,
) -> CmdResult<Option<Match>> {
    let state = state.inner();
    let entry = state.get(id)?;
    let tree = entry.view.read().tree.clone();
    let matcher = search::compile(&query)?;
    let (job, cancel) = state.start_job();
    let scope = scope.unwrap_or_else(|| Scope::whole(&tree));
    let r = tauri::async_runtime::spawn_blocking(move || {
        search::find_next(&tree, &matcher, scope, cursor, backwards, wrap, &cancel)
    })
    .await
    .map_err(|e| CmdError::new("internal", e.to_string()))?;
    state.end_job(job);
    Ok(r?)
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SearchEvent {
    Batch { matches: Vec<Match> },
    Progress { done: u64, total: u64 },
    Done { count: usize, truncated: bool },
    Error { error: CmdError },
}

/// Streams every match to the frontend in batches (SRCH-16, SRCH-17).
#[tauri::command]
pub fn search_all(
    app: AppHandle,
    state: State<'_, AppState>,
    id: u64,
    query: Query,
    scope: Option<Scope>,
    max: Option<usize>,
    channel: Channel<SearchEvent>,
) -> CmdResult<u64> {
    let entry = state.get(id)?;
    let tree = entry.view.read().tree.clone();
    let matcher = search::compile(&query)?;
    let scope = scope.unwrap_or_else(|| Scope::whole(&tree));
    let max = max.unwrap_or(100_000);
    let (job, cancel) = state.start_job();
    std::thread::spawn(move || {
        let mut batch = Vec::new();
        let mut last_flush = std::time::Instant::now();
        let progress = |done: u64, total: u64| {
            let _ = channel.send(SearchEvent::Progress { done, total });
        };
        let r = search::find_all(&tree, &matcher, scope, max, &cancel, &progress, |m| {
            batch.push(m);
            if batch.len() >= 2000 || last_flush.elapsed().as_millis() > 80 {
                let _ = channel.send(SearchEvent::Batch {
                    matches: std::mem::take(&mut batch),
                });
                last_flush = std::time::Instant::now();
            }
        });
        if !batch.is_empty() {
            let _ = channel.send(SearchEvent::Batch { matches: batch });
        }
        let _ = channel.send(match r {
            Ok(count) => SearchEvent::Done {
                count,
                truncated: count >= max,
            },
            Err(e) => SearchEvent::Error { error: e.into() },
        });
        app.state::<AppState>().end_job(job);
    });
    Ok(job)
}

#[tauri::command]
pub fn hash_algorithms() -> Vec<AlgoInfo> {
    hash::algorithms()
}

#[tauri::command]
pub fn crc_presets() -> Vec<CrcPreset> {
    hash::CRC_PRESETS.to_vec()
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum HashEvent {
    Progress { done: u64, total: u64 },
    Done { results: Vec<HashResult> },
    Error { error: CmdError },
}

/// Hashes a range in the background (ANL-01, ANL-02).
#[tauri::command]
pub fn hash_run(
    app: AppHandle,
    state: State<'_, AppState>,
    id: u64,
    start: u64,
    end: u64,
    algorithms: Vec<String>,
    custom: Option<CrcParams>,
    channel: Channel<HashEvent>,
) -> CmdResult<u64> {
    let entry = state.get(id)?;
    let tree = entry.view.read().tree.clone();
    let (job, cancel) = state.start_job();
    std::thread::spawn(move || {
        let last = std::sync::atomic::AtomicU64::new(0);
        let progress = |done: u64, total: u64| {
            if done >= last.load(std::sync::atomic::Ordering::Relaxed) + (16 << 20) || done == total
            {
                last.store(done, std::sync::atomic::Ordering::Relaxed);
                let _ = channel.send(HashEvent::Progress { done, total });
            }
        };
        let r = hash::hash_range(&tree, start, end, &algorithms, custom, &cancel, &progress);
        let _ = channel.send(match r {
            Ok(results) => HashEvent::Done { results },
            Err(e) => HashEvent::Error { error: e.into() },
        });
        app.state::<AppState>().end_job(job);
    });
    Ok(job)
}

/// Quick hash for the status bar (ANL-06 preview); small ranges only.
#[tauri::command]
pub fn hash_quick(
    state: State<'_, AppState>,
    id: u64,
    start: u64,
    end: u64,
    algorithm: String,
) -> CmdResult<Option<String>> {
    if end.saturating_sub(start) > 64 << 20 {
        return Ok(None);
    }
    let entry = state.get(id)?;
    let tree = entry.view.read().tree.clone();
    let r = hash::hash_range(
        &tree,
        start,
        end,
        &[algorithm],
        None,
        &hexa_core::Cancel::new(),
        &hexa_core::task::no_progress,
    )?;
    Ok(r.into_iter().next().map(|h| h.value))
}

#[tauri::command]
pub fn encodings() -> Vec<EncodingInfo> {
    encoding::ENCODINGS.to_vec()
}

/// Byte → character table of a single-byte encoding (DISP-09).
#[tauri::command]
pub fn encoding_table(id: String) -> CmdResult<Vec<Option<String>>> {
    Ok(encoding::table(&id)?
        .into_iter()
        .map(|c| c.map(|c| c.to_string()))
        .collect())
}

#[tauri::command]
pub fn encode_text(text: String, encoding: String) -> CmdResult<Vec<u8>> {
    Ok(encoding::encode(&text, &encoding)?)
}

#[tauri::command]
pub fn decode_bytes(bytes: Vec<u8>, encoding: String) -> CmdResult<String> {
    Ok(encoding::decode(&bytes, &encoding)?)
}

#[tauri::command]
pub fn parse_hex(text: String) -> CmdResult<Vec<u8>> {
    Ok(format::parse_hex_bytes(&text)?)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyResult {
    /// Text for the system clipboard; `None` when the selection is too big.
    pub text: Option<String>,
    pub len: u64,
    /// Whether the backend already wrote the system clipboard (text + bytes).
    pub written: bool,
}

/// Copies a selection: the pieces go to the internal clipboard (no bytes are
/// copied), text and raw bytes go to the system clipboard (EDIT-10…17).
#[tauri::command]
pub fn clip_copy(
    state: State<'_, AppState>,
    id: u64,
    start: u64,
    end: u64,
    format: Option<CopyFormat>,
) -> CmdResult<CopyResult> {
    let entry = state.get(id)?;
    let tree = entry.view.read().tree.clone();
    if start > end || end > tree.len() {
        return Err(CmdError::new(
            "outOfRange",
            "selection is outside of the document",
        ));
    }
    let slice = tree.slice(start, end);
    let len = end - start;
    let bytes = if len <= MAX_CLIPBOARD_TEXT.max(clipboard::MAX_RAW) {
        Some(
            slice
                .read_vec(0, len as usize)
                .map_err(|e| CmdError::from(hexa_core::Error::from(e)))?,
        )
    } else {
        None
    };
    let text = bytes
        .as_ref()
        .filter(|_| len <= MAX_CLIPBOARD_TEXT)
        .map(|b| format::copy_as(b, &format.unwrap_or(CopyFormat::HexSpaced)));
    let seq = state.clip_seq.fetch_add(1, Ordering::Relaxed) + 1;
    let raw = bytes.as_deref().filter(|_| len <= clipboard::MAX_RAW);
    let payload = clipboard::encode(std::process::id(), seq, len, raw);
    let shown = text
        .clone()
        .unwrap_or_else(|| format!("[Hexa: {len} bytes, too large to copy as text]"));
    let written = clipboard::write(&shown, payload);
    *state.clip.lock() = Some(InternalClip {
        tree: slice,
        text: text.clone(),
        seq,
    });
    Ok(CopyResult { text, len, written })
}

/// Pastes what Hexa put on the system clipboard: the internal pieces when
/// this instance copied them, raw bytes from another instance otherwise.
/// `None` means the clipboard holds no Hexa data (the UI parses its text).
#[tauri::command]
pub fn clip_paste_auto(
    state: State<'_, AppState>,
    id: u64,
    pos: u64,
    end: Option<u64>,
    overwrite: bool,
) -> CmdResult<Option<EditResult>> {
    let Some(p) = clipboard::read() else {
        return Ok(None);
    };
    let internal = state.clip.lock().clone();
    let tree = match internal {
        Some(c) if p.pid == std::process::id() && p.seq == c.seq => c.tree,
        _ => match p.bytes {
            Some(b) if b.is_empty() => Tree::new(),
            Some(b) => Tree::from_piece(Piece::bytes(b)),
            None => {
                return Err(CmdError::invalid(format!(
                    "another Hexa window copied {} bytes — too many to pass between programs",
                    p.len
                )));
            }
        },
    };
    paste_tree(&state, id, pos, end, overwrite, &tree).map(Some)
}

fn paste_tree(
    state: &AppState,
    id: u64,
    pos: u64,
    end: Option<u64>,
    overwrite: bool,
    tree: &Tree,
) -> CmdResult<EditResult> {
    mutate(state, id, |d| {
        let c = match end {
            Some(e) if e > pos => d.replace_tree(pos, e, tree, "Paste")?,
            _ if overwrite => d.overwrite_tree(pos, tree, "Paste")?,
            _ => d.insert_tree(pos, tree, "Paste")?,
        };
        Ok(Some(c))
    })
}

/// Formats a selection without touching the internal clipboard.
#[tauri::command]
pub fn copy_as(
    state: State<'_, AppState>,
    id: u64,
    start: u64,
    end: u64,
    format: CopyFormat,
) -> CmdResult<String> {
    if end.saturating_sub(start) > MAX_CLIPBOARD_TEXT {
        return Err(CmdError::new(
            "tooLarge",
            "selection is too large to copy as text",
        ));
    }
    let entry = state.get(id)?;
    let tree = entry.view.read().tree.clone();
    let bytes = tree
        .slice(start, end)
        .read_vec(0, usize::MAX)
        .map_err(|e| CmdError::from(hexa_core::Error::from(e)))?;
    Ok(format::copy_as(&bytes, &format))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipInfo {
    pub len: u64,
    pub text: Option<String>,
}

/// The internal clipboard, so the frontend can tell whether the system
/// clipboard still holds what Hexa put there.
#[tauri::command]
pub fn clip_info(state: State<'_, AppState>) -> Option<ClipInfo> {
    state.clip.lock().as_ref().map(|c| ClipInfo {
        len: c.tree.len(),
        text: c.text.clone(),
    })
}

/// Pastes the internal clipboard (EDIT-04): replace a selection, insert, or
/// overwrite starting at `pos`.
#[tauri::command]
pub fn clip_paste(
    state: State<'_, AppState>,
    id: u64,
    pos: u64,
    end: Option<u64>,
    overwrite: bool,
) -> CmdResult<EditResult> {
    let clip = state
        .clip
        .lock()
        .clone()
        .ok_or_else(|| CmdError::invalid("the clipboard is empty"))?;
    paste_tree(&state, id, pos, end, overwrite, &clip.tree)
}

/// Interpretations of clipboard text (EDIT-16).
#[tauri::command]
pub fn clip_parse(text: String) -> Vec<PasteCandidate> {
    format::parse_clipboard(&text)
}
