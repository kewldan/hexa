//! Recent and pinned files (FILE-01) and persisted settings (UI-11).

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::state::AppState;

const MAX_RECENT: usize = 40;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentEntry {
    pub path: PathBuf,
    pub pinned: bool,
    pub opened_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentItem {
    pub path: String,
    pub name: String,
    pub dir: String,
    pub size: Option<u64>,
    pub opened_ms: u64,
    pub pinned: bool,
    pub exists: bool,
    /// First bytes, for format badges on the start screen.
    pub head: Vec<u8>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

pub fn config_file(state: &AppState, name: &str) -> PathBuf {
    state.config_dir.read().join(name)
}

fn load(state: &AppState) -> Vec<RecentEntry> {
    std::fs::read(config_file(state, "recent.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn store(state: &AppState, list: &[RecentEntry]) {
    let path = config_file(state, "recent.json");
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_vec_pretty(list) {
        let _ = std::fs::write(path, json);
    }
}

pub fn touch(state: &AppState, path: &Path) {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut list = load(state);
    let pinned = list.iter().any(|e| e.path == path && e.pinned);
    list.retain(|e| e.path != path);
    list.insert(
        0,
        RecentEntry {
            path,
            pinned,
            opened_ms: now_ms(),
        },
    );
    let mut unpinned = 0;
    list.retain(|e| {
        if e.pinned {
            return true;
        }
        unpinned += 1;
        unpinned <= MAX_RECENT
    });
    store(state, &list);
}

pub fn list(state: &AppState) -> Vec<RecentItem> {
    load(state)
        .into_iter()
        .map(|e| {
            let meta = std::fs::metadata(&e.path).ok();
            let head = std::fs::File::open(&e.path)
                .ok()
                .and_then(|mut f| {
                    use std::io::Read;
                    let mut b = vec![0u8; 16];
                    let n = f.read(&mut b).ok()?;
                    b.truncate(n);
                    Some(b)
                })
                .unwrap_or_default();
            RecentItem {
                name: e
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                dir: e
                    .path
                    .parent()
                    .map(|d| d.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                path: e.path.to_string_lossy().into_owned(),
                size: meta.as_ref().map(std::fs::Metadata::len),
                exists: meta.is_some(),
                opened_ms: e.opened_ms,
                pinned: e.pinned,
                head,
            }
        })
        .collect()
}

pub fn set_pinned(state: &AppState, path: &Path, pinned: bool) {
    let mut list = load(state);
    for e in &mut list {
        if e.path == path {
            e.pinned = pinned;
        }
    }
    store(state, &list);
}

pub fn remove(state: &AppState, path: &Path) {
    let mut list = load(state);
    list.retain(|e| e.path != path);
    store(state, &list);
}

pub fn load_settings(state: &AppState) -> serde_json::Value {
    std::fs::read(config_file(state, "settings.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}

pub fn store_settings(state: &AppState, value: &serde_json::Value) -> std::io::Result<()> {
    let path = config_file(state, "settings.json");
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    std::fs::rename(tmp, path)
}
