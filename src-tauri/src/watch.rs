//! Watching opened files for external changes (FILE-06, FILE-07).
//!
//! The parent directory is watched rather than the file, so atomic saves by
//! other programs (write temp + rename) are noticed too.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;

use hexa_core::{Document, EditMode, OpenOptions};
use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};
use parking_lot::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::state::AppState;

#[derive(Default)]
pub struct Watcher {
    inner: Mutex<Option<RecommendedWatcher>>,
    /// file → documents showing it
    files: Mutex<HashMap<PathBuf, HashSet<u64>>>,
    /// directory → number of watched files in it
    dirs: Mutex<HashMap<PathBuf, usize>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocEvent {
    pub id: u64,
}

fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

pub fn init(app: &AppHandle) {
    let (tx, rx) = mpsc::channel::<PathBuf>();
    let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res {
            for p in ev.paths {
                let _ = tx.send(p);
            }
        }
    });
    let w = app.state::<Watcher>();
    match watcher {
        Ok(watcher) => *w.inner.lock() = Some(watcher),
        Err(e) => eprintln!("file watching unavailable: {e}"),
    }
    let app = app.clone();
    std::thread::spawn(move || {
        while let Ok(first) = rx.recv() {
            // Debounce bursts of events.
            let mut paths = HashSet::from([first]);
            while let Ok(p) = rx.recv_timeout(Duration::from_millis(150)) {
                paths.insert(p);
            }
            let ids: HashSet<u64> = {
                let files = app.state::<Watcher>().files.lock().clone();
                paths
                    .iter()
                    .filter_map(|p| files.get(&canonical(p)).or_else(|| files.get(p)))
                    .flatten()
                    .copied()
                    .collect()
            };
            for id in ids {
                handle(&app, id);
            }
        }
    });
}

fn handle(app: &AppHandle, id: u64) {
    let state = app.state::<AppState>();
    let Ok(entry) = state.get(id) else { return };
    if entry.saving.load(Ordering::Acquire) {
        return;
    }
    let mut doc = entry.doc.lock();
    if entry.follow.load(Ordering::Relaxed)
        && !doc.is_modified()
        && let Ok(n) = doc.follow_growth()
        && n > 0
    {
        entry.publish(&doc);
        let _ = app.emit("hexa://doc-updated", DocEvent { id });
        return;
    }
    if !doc.disk_changed() {
        return;
    }
    if entry.auto_reload.load(Ordering::Relaxed)
        && !doc.is_modified()
        && let Some(path) = doc.path().map(PathBuf::from)
    {
        let mode = doc.mode();
        if let Ok(fresh) = Document::open(
            &path,
            &OpenOptions {
                mode: if mode == EditMode::ReadOnly {
                    mode
                } else {
                    EditMode::Overwrite
                },
                ..OpenOptions::default()
            },
        ) {
            *doc = fresh;
            entry.publish(&doc);
            let _ = app.emit("hexa://doc-updated", DocEvent { id });
            return;
        }
    }
    let _ = app.emit("hexa://disk-changed", DocEvent { id });
}

pub fn watch(app: &AppHandle, id: u64, path: &Path) {
    let w = app.state::<Watcher>();
    let file = canonical(path);
    let Some(dir) = file.parent().map(Path::to_path_buf) else {
        return;
    };
    w.files.lock().entry(file).or_default().insert(id);
    let mut dirs = w.dirs.lock();
    let count = dirs.entry(dir.clone()).or_insert(0);
    if *count == 0
        && let Some(watcher) = w.inner.lock().as_mut()
    {
        let _ = watcher.watch(&dir, RecursiveMode::NonRecursive);
    }
    *count += 1;
}

pub fn unwatch(app: &AppHandle, id: u64) {
    let w = app.state::<Watcher>();
    let mut emptied = Vec::new();
    {
        let mut files = w.files.lock();
        for (path, ids) in files.iter_mut() {
            if ids.remove(&id) && ids.is_empty() {
                emptied.push(path.clone());
            }
        }
        for p in &emptied {
            files.remove(p);
        }
    }
    let mut dirs = w.dirs.lock();
    for p in emptied {
        let Some(dir) = p.parent().map(Path::to_path_buf) else {
            continue;
        };
        if let Some(c) = dirs.get_mut(&dir) {
            *c -= 1;
            if *c == 0 {
                dirs.remove(&dir);
                if let Some(watcher) = w.inner.lock().as_mut() {
                    let _ = watcher.unwatch(&dir);
                }
            }
        }
    }
}

pub fn rewatch(app: &AppHandle, id: u64, path: &Path) {
    unwatch(app, id);
    watch(app, id, path);
}
