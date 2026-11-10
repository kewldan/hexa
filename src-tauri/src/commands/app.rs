//! Settings, recent files, windows, workspaces and crash recovery.

use std::path::PathBuf;

use tauri::{AppHandle, State};

use crate::error::{CmdError, CmdResult};
use crate::recent::{self, RecentItem};
use crate::recovery::{self, RecoveryItem};
use crate::state::{AppState, DocInfo};

#[tauri::command]
pub fn settings_get(state: State<'_, AppState>) -> serde_json::Value {
    recent::load_settings(&state)
}

#[tauri::command]
pub fn settings_set(state: State<'_, AppState>, value: serde_json::Value) -> CmdResult<()> {
    Ok(recent::store_settings(&state, &value)?)
}

#[tauri::command]
pub fn recent_list(state: State<'_, AppState>) -> Vec<RecentItem> {
    recent::list(&state)
}

#[tauri::command]
pub fn recent_pin(state: State<'_, AppState>, path: PathBuf, pinned: bool) {
    recent::set_pinned(&state, &path, pinned);
}

#[tauri::command]
pub fn recent_remove(state: State<'_, AppState>, path: PathBuf) {
    recent::remove(&state, &path);
}

/// Files passed on the command line or through "Open with" before the
/// window was ready (FILE-01).
#[tauri::command]
pub fn app_initial_files(state: State<'_, AppState>) -> Vec<String> {
    std::mem::take(&mut *state.initial_files.lock())
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

/// Opens another window, optionally taking over documents (FILE-09).
#[tauri::command]
pub fn window_new(app: AppHandle, docs: Option<Vec<u64>>) -> CmdResult<()> {
    let query = docs
        .filter(|d| !d.is_empty())
        .map(|d| {
            format!(
                "?docs={}",
                d.iter().map(u64::to_string).collect::<Vec<_>>().join(",")
            )
        })
        .unwrap_or_default();
    crate::create_window(&app, &query)?;
    Ok(())
}

/// Writes a workspace sidecar file (FILE-11). The content is produced by the UI.
#[tauri::command]
pub fn workspace_save(path: PathBuf, content: String) -> CmdResult<()> {
    if serde_json::from_str::<serde_json::Value>(&content).is_err() {
        return Err(CmdError::invalid("workspace must be JSON"));
    }
    let tmp = path.with_extension("hexa-ws.tmp");
    std::fs::write(&tmp, content)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

#[tauri::command]
pub fn workspace_load(path: PathBuf) -> CmdResult<String> {
    let content = std::fs::read_to_string(path)?;
    if serde_json::from_str::<serde_json::Value>(&content).is_err() {
        return Err(CmdError::invalid("not a workspace file"));
    }
    Ok(content)
}

#[tauri::command]
pub fn recovery_list(state: State<'_, AppState>) -> Vec<RecoveryItem> {
    recovery::list(&state)
}

#[tauri::command]
pub fn recovery_restore(
    app: AppHandle,
    state: State<'_, AppState>,
    key: String,
) -> CmdResult<DocInfo> {
    let doc = recovery::restore(&state, &key)?;
    let path = doc.path().map(PathBuf::from);
    let entry = state.add(doc);
    if let Some(p) = path {
        crate::watch::watch(&app, entry.id, &p);
    }
    let doc = entry.doc.lock();
    Ok(entry.info(&doc))
}

#[tauri::command]
pub fn recovery_discard(state: State<'_, AppState>, key: String) {
    recovery::discard(&state, &key);
}

/// Reads a small text file chosen by the user (tables, bookmarks, templates).
#[tauri::command]
pub fn read_text_file(path: PathBuf) -> CmdResult<String> {
    let meta = std::fs::metadata(&path)?;
    if meta.len() > 16 << 20 {
        return Err(CmdError::invalid("file is too large for a text import"));
    }
    Ok(std::fs::read_to_string(path)?)
}

/// Writes a text export chosen by the user.
#[tauri::command]
pub fn write_text_file(path: PathBuf, content: String) -> CmdResult<()> {
    std::fs::write(path, content)?;
    Ok(())
}
