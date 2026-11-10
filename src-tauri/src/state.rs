//! Shared application state: open documents, jobs, the internal clipboard.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use hexa_core::provider::Provider;
use hexa_core::{Cancel, Document, EditMode, Tree};
use parking_lot::{Mutex, RwLock};
use serde::Serialize;

use crate::error::{CmdError, CmdResult};

/// Lock-free view of a document for readers (the grid keeps reading while a
/// long save holds the document lock).
#[derive(Debug)]
pub struct View {
    pub tree: Tree,
    pub base: Arc<dyn Provider>,
    pub version: u64,
}

#[derive(Debug)]
pub struct DocEntry {
    pub id: u64,
    pub doc: Mutex<Document>,
    pub view: RwLock<View>,
    /// Windows that show this document (FILE-09).
    pub refs: AtomicUsize,
    /// Follow growth of the file (tail -f, FILE-07).
    pub follow: AtomicBool,
    /// Reload silently when the file changes and there are no edits.
    pub auto_reload: AtomicBool,
    pub saving: AtomicBool,
    /// Version last written to the recovery journal.
    pub recovered_version: AtomicU64,
}

impl DocEntry {
    pub fn new(id: u64, doc: Document) -> Self {
        let view = View {
            tree: doc.snapshot(),
            base: doc.base().clone(),
            version: 1,
        };
        Self {
            id,
            doc: Mutex::new(doc),
            view: RwLock::new(view),
            refs: AtomicUsize::new(0),
            follow: AtomicBool::new(false),
            auto_reload: AtomicBool::new(false),
            saving: AtomicBool::new(false),
            recovered_version: AtomicU64::new(0),
        }
    }

    /// Publishes the document's current state to readers.
    pub fn publish(&self, doc: &Document) -> u64 {
        let mut v = self.view.write();
        v.tree = doc.snapshot();
        v.base = doc.base().clone();
        v.version += 1;
        v.version
    }

    pub fn version(&self) -> u64 {
        self.view.read().version
    }

    pub fn info(&self, doc: &Document) -> DocInfo {
        DocInfo {
            id: self.id,
            name: doc.name().to_owned(),
            path: doc.path().map(|p| p.to_string_lossy().into_owned()),
            len: doc.len(),
            base_len: doc.base().len(),
            mode: doc.mode(),
            fixed_size: doc.fixed_size(),
            modified: doc.is_modified(),
            can_undo: doc.can_undo(),
            can_redo: doc.can_redo(),
            write_denied: doc.write_denied(),
            version: self.version(),
            piece_count: doc.piece_count(),
            follow: self.follow.load(Ordering::Relaxed),
            auto_reload: self.auto_reload.load(Ordering::Relaxed),
        }
    }
}

/// Everything the UI needs to know about a document.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocInfo {
    pub id: u64,
    pub name: String,
    pub path: Option<String>,
    pub len: u64,
    pub base_len: u64,
    pub mode: EditMode,
    pub fixed_size: bool,
    pub modified: bool,
    pub can_undo: bool,
    pub can_redo: bool,
    pub write_denied: bool,
    pub version: u64,
    pub piece_count: u64,
    pub follow: bool,
    pub auto_reload: bool,
}

/// Bytes copied inside Hexa: kept as pieces, so copying a gigabyte is free
/// (CORE-04). `text` is what went to the system clipboard, to recognise it.
#[derive(Debug, Clone)]
pub struct InternalClip {
    pub tree: Tree,
    pub text: Option<String>,
}

#[derive(Debug, Default)]
pub struct AppState {
    pub docs: RwLock<HashMap<u64, Arc<DocEntry>>>,
    next_id: AtomicU64,
    pub clip: Mutex<Option<InternalClip>>,
    pub jobs: Mutex<HashMap<u64, Cancel>>,
    next_job: AtomicU64,
    pub initial_files: Mutex<Vec<PathBuf>>,
    pub config_dir: RwLock<PathBuf>,
}

impl AppState {
    pub fn add(&self, doc: Document) -> Arc<DocEntry> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let entry = Arc::new(DocEntry::new(id, doc));
        self.docs.write().insert(id, entry.clone());
        entry
    }

    pub fn get(&self, id: u64) -> CmdResult<Arc<DocEntry>> {
        self.docs
            .read()
            .get(&id)
            .cloned()
            .ok_or_else(|| CmdError::new("notFound", format!("document {id} is not open")))
    }

    pub fn remove(&self, id: u64) -> Option<Arc<DocEntry>> {
        self.docs.write().remove(&id)
    }

    pub fn all(&self) -> Vec<Arc<DocEntry>> {
        self.docs.read().values().cloned().collect()
    }

    pub fn start_job(&self) -> (u64, Cancel) {
        let id = self.next_job.fetch_add(1, Ordering::Relaxed) + 1;
        let cancel = Cancel::new();
        self.jobs.lock().insert(id, cancel.clone());
        (id, cancel)
    }

    pub fn end_job(&self, id: u64) {
        self.jobs.lock().remove(&id);
    }

    pub fn cancel_job(&self, id: u64) {
        if let Some(c) = self.jobs.lock().get(&id) {
            c.cancel();
        }
    }
}
