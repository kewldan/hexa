//! Periodic journal of unsaved edits and restoring it after a crash (FILE-10).

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Duration;

use hexa_core::Document;
use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::state::AppState;

fn dir(state: &AppState) -> PathBuf {
    state.config_dir.read().join("recovery")
}

fn file_for(state: &AppState, id: u64) -> PathBuf {
    dir(state).join(format!("{}-{id}.hxr", std::process::id()))
}

pub fn forget(state: &AppState, id: u64) {
    let _ = std::fs::remove_file(file_for(state, id));
}

/// Writes recovery data for documents that changed since the last pass.
fn pass(state: &AppState) {
    let _ = std::fs::create_dir_all(dir(state));
    for entry in state.all() {
        if entry.saving.load(Ordering::Acquire) {
            continue;
        }
        let version = entry.version();
        if entry.recovered_version.load(Ordering::Relaxed) == version {
            continue;
        }
        let Some(doc) = entry.doc.try_lock() else {
            continue;
        };
        match doc.export_recovery() {
            Ok(Some(data)) => {
                let path = file_for(state, entry.id);
                let tmp = path.with_extension("tmp");
                if std::fs::write(&tmp, data).is_ok() {
                    let _ = std::fs::rename(&tmp, &path);
                }
            }
            Ok(None) => forget(state, entry.id),
            Err(e) => eprintln!("recovery export failed: {e}"),
        }
        entry.recovered_version.store(version, Ordering::Relaxed);
    }
}

pub fn start(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(3));
            pass(&app.state::<AppState>());
        }
    });
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryItem {
    pub key: String,
    pub path: Option<String>,
    pub name: String,
    pub saved_ms: u64,
}

/// Journals left by previous runs.
pub fn list(state: &AppState) -> Vec<RecoveryItem> {
    let own = format!("{}-", std::process::id());
    let Ok(rd) = std::fs::read_dir(dir(state)) else {
        return Vec::new();
    };
    rd.filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "hxr"))
        .filter(|e| !e.file_name().to_string_lossy().starts_with(&own))
        .filter_map(|e| {
            let data = std::fs::read(e.path()).ok()?;
            let (path, name) = Document::peek_recovery(&data).ok()?;
            let saved_ms = e
                .metadata()
                .ok()?
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_millis() as u64;
            Some(RecoveryItem {
                key: e.file_name().to_string_lossy().into_owned(),
                path: path.map(|p| p.to_string_lossy().into_owned()),
                name,
                saved_ms,
            })
        })
        .collect()
}

fn key_path(state: &AppState, key: &str) -> Option<PathBuf> {
    // Keys are plain file names inside the recovery directory.
    (!key.contains(['/', '\\'])
        && std::path::Path::new(key)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("hxr")))
    .then(|| dir(state).join(key))
}

pub fn restore(state: &AppState, key: &str) -> hexa_core::Result<Document> {
    let path = key_path(state, key).ok_or_else(|| hexa_core::Error::Invalid("bad key".into()))?;
    let data = std::fs::read(&path)?;
    let doc = Document::import_recovery(&data)?;
    let _ = std::fs::remove_file(path);
    Ok(doc)
}

pub fn discard(state: &AppState, key: &str) {
    if let Some(p) = key_path(state, key) {
        let _ = std::fs::remove_file(p);
    }
}
