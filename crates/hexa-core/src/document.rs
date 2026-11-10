//! An editable document: piece tree + history + edit modes.

use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;

use crate::error::{Error, Result};
use crate::history::{Change, History, HistoryItem};
use crate::piece::{Piece, Source};
use crate::provider::{FileOpenOptions, FileProvider, MemoryProvider, Provider};
use crate::tree::Tree;

/// Edit mode of a document (EDIT-01, FILE-04).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EditMode {
    ReadOnly,
    Overwrite,
    Insert,
}

/// How other processes may access an opened file (FILE-08).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ShareMode {
    /// No lock: others may read and write.
    #[default]
    Shared,
    /// Shared lock: others may read but not take an exclusive lock.
    DenyWrite,
    /// Exclusive lock.
    Exclusive,
}

/// Options for [`Document::open`].
#[derive(Debug, Clone)]
pub struct OpenOptions {
    pub mode: EditMode,
    /// Forbid length changes (CORE-20). `None` = automatic.
    pub fixed_size: Option<bool>,
    /// Read the whole file into RAM instead of paging it (CORE-06).
    pub load_in_memory: bool,
    pub cache_budget: usize,
    pub share: ShareMode,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self {
            mode: EditMode::Overwrite,
            fixed_size: None,
            load_in_memory: false,
            cache_budget: crate::provider::DEFAULT_CACHE_BUDGET,
            share: ShareMode::Shared,
        }
    }
}

/// How newly created or filled bytes are generated (EDIT-05, CORE-07).
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Fill {
    Byte {
        value: u8,
    },
    Pattern {
        bytes: Vec<u8>,
    },
    Random {
        seed: u64,
    },
    /// Increasing sequence of `width`-byte little/big endian integers.
    Sequence {
        start: u64,
        step: u64,
        width: u8,
        big_endian: bool,
    },
}

/// Size and mtime of the file at the time it was read (CORE-19).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStamp {
    pub len: u64,
    pub modified: Option<SystemTime>,
}

impl FileStamp {
    pub fn of(meta: &Metadata) -> Self {
        Self {
            len: meta.len(),
            modified: meta.modified().ok(),
        }
    }

    pub fn read(path: &Path) -> std::io::Result<Self> {
        Ok(Self::of(&std::fs::metadata(path)?))
    }
}

/// Kind of a highlighted change span (DISP-15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ChangeKind {
    Modified,
    Inserted,
    /// Bytes were deleted at `start` (zero-length marker).
    Deleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeSpan {
    pub start: u64,
    pub end: u64,
    pub kind: ChangeKind,
}

#[derive(Debug)]
struct TypingRun {
    entry: usize,
    start: u64,
    bytes: Vec<u8>,
    insert: bool,
    base_tree: Tree,
    last: Instant,
}

const TYPING_GAP: Duration = Duration::from_secs(3);
/// Largest selection that may be materialised in memory by a transform.
pub const MATERIALIZE_LIMIT: u64 = 512 * 1024 * 1024;

/// An open document.
#[derive(Debug)]
pub struct Document {
    pub(crate) base: Arc<dyn Provider>,
    pub(crate) history: History,
    mode: EditMode,
    pub(crate) fixed_size: bool,
    pub(crate) path: Option<PathBuf>,
    pub(crate) stamp: Option<FileStamp>,
    pub(crate) write_denied: bool,
    pub(crate) share: ShareMode,
    pub(crate) name: String,
    typing: Option<TypingRun>,
    tx: Option<(String, Tree, Option<Change>)>,
    marks: Vec<crate::marks::Mark>,
    next_mark: u64,
}

impl Document {
    fn with_base(base: Arc<dyn Provider>, name: String) -> Self {
        let len = base.len();
        let tree = if len == 0 {
            Tree::new()
        } else {
            Tree::from_piece(Piece::provider(base.clone(), 0, len))
        };
        Self {
            base,
            history: History::new(tree),
            mode: EditMode::Overwrite,
            fixed_size: false,
            path: None,
            stamp: None,
            write_denied: false,
            share: ShareMode::Shared,
            name,
            typing: None,
            tx: None,
            marks: Vec::new(),
            next_mark: 1,
        }
    }

    /// A new empty buffer (FILE-03).
    pub fn new_empty() -> Self {
        Self::from_bytes("Untitled", Vec::new())
    }

    pub fn from_bytes(name: impl Into<String>, data: Vec<u8>) -> Self {
        let name = name.into();
        Self::with_base(Arc::new(MemoryProvider::new(name.clone(), data)), name)
    }

    /// A new buffer of `len` bytes generated by `fill`, without materialising it.
    pub fn new_sized(len: u64, fill: &Fill) -> Result<Self> {
        let mut doc = Self::new_empty();
        if len > 0 {
            let tree = fill_tree(len, 0, fill)?;
            doc.history = History::new(tree);
            // The new content is unsaved.
            doc.history.commit(
                doc.history.tree().clone(),
                "New",
                Change {
                    start: 0,
                    removed: 0,
                    inserted: len,
                },
            );
        }
        Ok(doc)
    }

    /// Opens a file lazily (FILE-02, FILE-04, FILE-05, FILE-08).
    pub fn open(path: &Path, opts: &OpenOptions) -> Result<Self> {
        let meta = std::fs::metadata(path)?;
        if meta.is_dir() {
            return Err(Error::invalid(format!("{} is a directory", path.display())));
        }
        let want_write = opts.mode != EditMode::ReadOnly;
        let mut write_denied = false;
        let base: Arc<dyn Provider> = if opts.load_in_memory {
            write_denied = want_write && meta.permissions().readonly();
            Arc::new(MemoryProvider::from_file(path)?)
        } else {
            let fopts = FileOpenOptions {
                write: want_write,
                cache_budget: opts.cache_budget,
                ..FileOpenOptions::default()
            };
            match FileProvider::open(path, fopts) {
                Ok(p) => {
                    lock(p.file(), opts.share)?;
                    Arc::new(p)
                }
                Err(e) if want_write && e.kind() == std::io::ErrorKind::PermissionDenied => {
                    write_denied = true;
                    let p = FileProvider::open(
                        path,
                        FileOpenOptions {
                            write: false,
                            ..fopts
                        },
                    )?;
                    lock(p.file(), opts.share)?;
                    Arc::new(p)
                }
                Err(e) => return Err(e.into()),
            }
        };
        let mut doc = Self::with_base(base, crate::provider::display_name(path));
        doc.path = Some(path.to_path_buf());
        doc.stamp = Some(FileStamp::of(&meta));
        doc.write_denied = write_denied;
        doc.share = opts.share;
        doc.mode = if write_denied {
            EditMode::ReadOnly
        } else {
            opts.mode
        };
        doc.fixed_size = opts.fixed_size.unwrap_or(false);
        Ok(doc)
    }

    /// Wraps an arbitrary provider (disks, processes, windows …).
    pub fn from_provider(provider: Arc<dyn Provider>, fixed_size: bool) -> Self {
        let name = provider.name();
        let writable = provider.capabilities().writable;
        let mut doc = Self::with_base(provider, name);
        doc.fixed_size = fixed_size;
        if !writable {
            doc.mode = EditMode::ReadOnly;
        }
        doc
    }

    // ----- accessors ------------------------------------------------------

    pub fn len(&self) -> u64 {
        self.tree().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn mode(&self) -> EditMode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: EditMode) -> Result<()> {
        if mode != EditMode::ReadOnly && self.write_denied {
            return Err(Error::ReadOnly);
        }
        self.break_typing();
        self.mode = mode;
        Ok(())
    }

    pub fn fixed_size(&self) -> bool {
        self.fixed_size
    }

    pub fn set_fixed_size(&mut self, fixed: bool) {
        self.fixed_size = fixed;
    }

    /// Whether write access to the file was refused by the OS (FILE-05).
    pub fn write_denied(&self) -> bool {
        self.write_denied
    }

    pub fn is_modified(&self) -> bool {
        self.history.is_modified()
    }

    pub fn base(&self) -> &Arc<dyn Provider> {
        &self.base
    }

    fn tree(&self) -> &Tree {
        self.tx
            .as_ref()
            .map_or_else(|| self.history.tree(), |(_, t, _)| t)
    }

    /// An immutable snapshot for background jobs (CORE-08).
    pub fn snapshot(&self) -> Tree {
        self.tree().clone()
    }

    pub fn read(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        Ok(self.tree().read(offset, buf)?)
    }

    pub fn read_vec(&self, offset: u64, max: usize) -> Result<Vec<u8>> {
        Ok(self.tree().read_vec(offset, max)?)
    }

    pub fn piece_count(&self) -> u64 {
        self.tree().piece_count()
    }

    // ----- history --------------------------------------------------------

    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    pub fn undo(&mut self) -> Option<Change> {
        self.break_typing();
        let c = self.history.undo().map(Change::inverse);
        if let Some(c) = c {
            crate::marks::map_marks(&mut self.marks, c);
        }
        c
    }

    pub fn redo(&mut self) -> Option<Change> {
        self.break_typing();
        let c = self.history.redo();
        if let Some(c) = c {
            crate::marks::map_marks(&mut self.marks, c);
        }
        c
    }

    pub fn history(&self) -> Vec<HistoryItem> {
        self.history.items()
    }

    pub fn history_goto(&mut self, id: usize) -> bool {
        self.break_typing();
        match self.history.goto(id) {
            Some(changes) => {
                for c in changes {
                    crate::marks::map_marks(&mut self.marks, c);
                }
                true
            }
            None => false,
        }
    }

    /// Applies planned replacements as one undoable step (SRCH-20).
    pub fn apply_replacements(&mut self, plan: &[crate::search::Planned]) -> Result<Change> {
        if plan.iter().any(|p| p.new.len() as u64 != p.len) && self.fixed_size {
            return Err(Error::FixedSize);
        }
        let mut sorted: Vec<&crate::search::Planned> = plan.iter().collect();
        sorted.sort_by_key(|p| std::cmp::Reverse(p.offset));
        let start = sorted.iter().map(|p| p.offset).min().unwrap_or(0);
        let end = sorted.iter().map(|p| p.offset + p.len).max().unwrap_or(0);
        let delta: i128 = sorted
            .iter()
            .map(|p| p.new.len() as i128 - i128::from(p.len))
            .sum();
        self.transaction("Replace all", |d| {
            for p in sorted {
                d.replace(p.offset, p.offset + p.len, &p.new)?;
            }
            Ok(())
        })?;
        Ok(Change {
            start,
            removed: end - start,
            inserted: (i128::from(end - start) + delta) as u64,
        })
    }

    // ----- marks (NAV-14…18) ------------------------------------------------

    pub fn marks(&self) -> &[crate::marks::Mark] {
        &self.marks
    }

    /// Adds a mark and returns its id.
    pub fn add_mark(&mut self, mut mark: crate::marks::Mark) -> u64 {
        mark.id = self.next_mark;
        self.next_mark += 1;
        self.marks.push(mark);
        self.next_mark - 1
    }

    /// Replaces a mark with the same id; returns false if it does not exist.
    pub fn update_mark(&mut self, mark: crate::marks::Mark) -> bool {
        match self.marks.iter_mut().find(|m| m.id == mark.id) {
            Some(m) => {
                *m = mark;
                true
            }
            None => false,
        }
    }

    pub fn remove_mark(&mut self, id: u64) -> bool {
        let n = self.marks.len();
        self.marks.retain(|m| m.id != id);
        n != self.marks.len()
    }

    /// Replaces all marks (import, workspace restore); ids are reassigned.
    pub fn set_marks(&mut self, marks: Vec<crate::marks::Mark>) {
        self.marks.clear();
        for m in marks {
            self.add_mark(m);
        }
    }

    /// Undoes one earlier edit while keeping everything done after it
    /// (CORE-13). The bytes the edit inserted are tracked through later
    /// edits; if a later edit touched them the revert is refused.
    pub fn revert_entry(&mut self, id: usize) -> Result<Change> {
        self.check_writable()?;
        if self.tx.is_some() {
            return Err(Error::invalid("revert inside a transaction"));
        }
        self.break_typing();
        let (before, change) = self
            .history
            .entry_change(id)
            .ok_or_else(|| Error::invalid("this history entry cannot be reverted"))?;
        let later = self
            .history
            .changes_after(id)
            .ok_or_else(|| Error::invalid("the entry is not on the current branch"))?;
        let (a, b) = map_range(change.start, change.start + change.inserted, &later)
            .ok_or_else(|| Error::invalid("later edits changed the same bytes"))?;
        let restore = before.slice(change.start, change.start + change.removed);
        if self.fixed_size && restore.len() != b - a {
            return Err(Error::FixedSize);
        }
        let tree = self.tree().replace(a, b, &restore);
        Ok(self.apply(
            tree,
            &format!("Revert #{id}"),
            Change {
                start: a,
                removed: b - a,
                inserted: restore.len(),
            },
        ))
    }

    /// Ends coalescing so the next keystroke starts a new history entry.
    pub fn break_typing(&mut self) {
        self.typing = None;
    }

    /// Runs `f` as one undoable step (CORE-11).
    pub fn transaction<T>(
        &mut self,
        label: &str,
        f: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<T> {
        if self.tx.is_some() {
            return f(self);
        }
        self.break_typing();
        self.tx = Some((label.to_owned(), self.history.tree().clone(), None));
        let out = f(self);
        let (label, tree, change) = self.tx.take().expect("transaction state");
        match out {
            Ok(v) => {
                if let Some(change) = change {
                    self.history.commit(tree, label, change);
                }
                Ok(v)
            }
            Err(e) => Err(e),
        }
    }

    fn apply(&mut self, tree: Tree, label: &str, change: Change) -> Change {
        self.typing = None;
        crate::marks::map_marks(&mut self.marks, change);
        if let Some((_, t, c)) = &mut self.tx {
            *t = tree;
            *c = Some(match *c {
                None => change,
                Some(prev) => merge_changes(prev, change),
            });
        } else {
            self.history.commit(tree, label, change);
        }
        change
    }

    // ----- checks ---------------------------------------------------------

    fn check_writable(&self) -> Result<()> {
        if self.mode == EditMode::ReadOnly {
            Err(Error::ReadOnly)
        } else {
            Ok(())
        }
    }

    fn check_resizable(&self) -> Result<()> {
        self.check_writable()?;
        if self.fixed_size {
            Err(Error::FixedSize)
        } else {
            Ok(())
        }
    }

    fn check_range(&self, start: u64, end: u64) -> Result<()> {
        let len = self.len();
        if start > end || end > len {
            Err(Error::OutOfRange { start, end, len })
        } else {
            Ok(())
        }
    }

    // ----- typing (EDIT-02, CORE-10) ---------------------------------------

    /// Types one byte at `pos` according to the current mode.
    ///
    /// With `amend`, the byte at `pos` is replaced even in insert mode (second
    /// nibble of a hex digit pair). Consecutive keystrokes coalesce into one
    /// history entry.
    pub fn type_byte(&mut self, pos: u64, byte: u8, amend: bool) -> Result<Change> {
        self.check_writable()?;
        if self.tx.is_some() {
            return Err(Error::invalid("typing inside a transaction"));
        }
        let insert = self.mode == EditMode::Insert && !amend;
        let len = self.len();
        if pos > len || (amend && pos >= len) {
            return Err(Error::OutOfRange {
                start: pos,
                end: pos + 1,
                len,
            });
        }
        if self.fixed_size && (insert || pos == len) {
            return Err(Error::FixedSize);
        }

        if let Some(run) = &mut self.typing {
            let run_end = run.start + run.bytes.len() as u64;
            let fresh = run.last.elapsed() < TYPING_GAP && self.history.can_amend(run.entry);
            let continues = if amend {
                pos >= run.start && pos < run_end
            } else {
                pos == run_end && run.insert == insert
            };
            if fresh && continues {
                let step = if amend {
                    Change {
                        start: pos,
                        removed: 1,
                        inserted: 1,
                    }
                } else if run.insert || pos >= len {
                    Change {
                        start: pos,
                        removed: 0,
                        inserted: 1,
                    }
                } else {
                    Change {
                        start: pos,
                        removed: 1,
                        inserted: 1,
                    }
                };
                crate::marks::map_marks(&mut self.marks, step);
                if amend {
                    run.bytes[(pos - run.start) as usize] = byte;
                } else {
                    run.bytes.push(byte);
                }
                run.last = Instant::now();
                let (tree, change) = run_tree(run);
                self.history.amend(tree, change);
                return Ok(change);
            }
        }

        let base_tree = self.history.tree().clone();
        let first = if insert || pos >= len {
            Change {
                start: pos,
                removed: 0,
                inserted: 1,
            }
        } else {
            Change {
                start: pos,
                removed: 1,
                inserted: 1,
            }
        };
        crate::marks::map_marks(&mut self.marks, first);
        let mut run = TypingRun {
            entry: 0,
            start: pos,
            bytes: vec![byte],
            insert,
            base_tree,
            last: Instant::now(),
        };
        let (tree, change) = run_tree(&run);
        run.entry = self.history.commit(tree, "Typing", change);
        self.typing = Some(run);
        Ok(change)
    }

    // ----- generic edits (EDIT-03…09) ---------------------------------------

    pub fn overwrite(&mut self, pos: u64, data: &[u8]) -> Result<Change> {
        self.overwrite_tree(pos, &bytes_tree(data), "Overwrite")
    }

    pub fn insert(&mut self, pos: u64, data: &[u8]) -> Result<Change> {
        self.insert_tree(pos, &bytes_tree(data), "Insert")
    }

    pub fn delete(&mut self, start: u64, end: u64) -> Result<Change> {
        self.check_resizable()?;
        self.check_range(start, end)?;
        let tree = self.tree().remove(start, end);
        Ok(self.apply(
            tree,
            "Delete",
            Change {
                start,
                removed: end - start,
                inserted: 0,
            },
        ))
    }

    pub fn replace(&mut self, start: u64, end: u64, data: &[u8]) -> Result<Change> {
        self.replace_tree(start, end, &bytes_tree(data), "Replace")
    }

    /// Replaces `[start, end)` with the bytes of `other`; length may change.
    pub fn replace_tree(
        &mut self,
        start: u64,
        end: u64,
        other: &Tree,
        label: &str,
    ) -> Result<Change> {
        self.check_writable()?;
        self.check_range(start, end)?;
        if other.len() != end - start && self.fixed_size {
            return Err(Error::FixedSize);
        }
        let tree = self.tree().replace(start, end, other);
        Ok(self.apply(
            tree,
            label,
            Change {
                start,
                removed: end - start,
                inserted: other.len(),
            },
        ))
    }

    /// Copies a range without copying bytes (CORE-04).
    pub fn copy(&self, start: u64, end: u64) -> Result<Tree> {
        self.check_range(start, end)?;
        Ok(self.tree().slice(start, end))
    }

    /// Paste-insert (EDIT-04).
    pub fn insert_tree(&mut self, pos: u64, other: &Tree, label: &str) -> Result<Change> {
        self.check_resizable()?;
        self.check_range(pos, pos)?;
        let tree = self.tree().insert(pos, other);
        Ok(self.apply(
            tree,
            label,
            Change {
                start: pos,
                removed: 0,
                inserted: other.len(),
            },
        ))
    }

    /// Paste-write (EDIT-04): overwrites from `pos`, extending the document
    /// when needed (truncated instead when the size is fixed).
    pub fn overwrite_tree(&mut self, pos: u64, other: &Tree, label: &str) -> Result<Change> {
        self.check_writable()?;
        let len = self.len();
        self.check_range(pos, pos)?;
        let mut other = other.clone();
        if pos + other.len() > len && self.fixed_size {
            other = other.slice(0, len - pos);
        }
        let end = (pos + other.len()).min(len);
        let tree = self.tree().replace(pos, end, &other);
        Ok(self.apply(
            tree,
            label,
            Change {
                start: pos,
                removed: end - pos,
                inserted: other.len(),
            },
        ))
    }

    /// Fills `[start, end)` (EDIT-05) without materialising the bytes.
    pub fn fill(&mut self, start: u64, end: u64, fill: &Fill) -> Result<Change> {
        self.check_writable()?;
        self.check_range(start, end)?;
        let t = fill_tree(end - start, start, fill)?;
        self.replace_tree(start, end, &t, "Fill")
    }

    /// Inserts `len` generated bytes (EDIT-06).
    pub fn insert_fill(&mut self, pos: u64, len: u64, fill: &Fill) -> Result<Change> {
        let t = fill_tree(len, 0, fill)?;
        self.insert_tree(pos, &t, "Insert bytes")
    }

    /// Inserts the content of a file without reading it (EDIT-06).
    pub fn insert_file(&mut self, pos: u64, path: &Path) -> Result<Change> {
        let p: Arc<dyn Provider> = Arc::new(FileProvider::open(path, FileOpenOptions::default())?);
        let len = p.len();
        let t = if len == 0 {
            Tree::new()
        } else {
            Tree::from_piece(Piece::provider(p, 0, len))
        };
        self.insert_tree(pos, &t, "Insert file")
    }

    /// Truncates or extends to `new_len` (EDIT-07).
    pub fn resize(&mut self, new_len: u64, fill: &Fill) -> Result<Change> {
        let len = self.len();
        match new_len.cmp(&len) {
            std::cmp::Ordering::Less => self.delete(new_len, len),
            std::cmp::Ordering::Equal => Ok(Change {
                start: len,
                removed: 0,
                inserted: 0,
            }),
            std::cmp::Ordering::Greater => {
                let t = fill_tree(new_len - len, 0, fill)?;
                self.insert_tree(len, &t, "Extend")
            }
        }
    }

    /// Rewrites a range through `f` (byte swap, reverse … EDIT-09).
    pub fn transform(
        &mut self,
        start: u64,
        end: u64,
        label: &str,
        f: impl FnOnce(&mut Vec<u8>) -> Result<()>,
    ) -> Result<Change> {
        self.check_writable()?;
        self.check_range(start, end)?;
        let len = end - start;
        if len > MATERIALIZE_LIMIT {
            return Err(Error::TooLarge {
                len,
                limit: MATERIALIZE_LIMIT,
            });
        }
        let mut buf = vec![0; len as usize];
        self.read(start, &mut buf)?;
        f(&mut buf)?;
        if buf.len() as u64 != len {
            self.check_resizable()?;
        }
        self.replace_tree(start, end, &bytes_tree(&buf), label)
    }

    /// Swaps byte order of every `width`-byte group (EDIT-09).
    pub fn byte_swap(&mut self, start: u64, end: u64, width: usize) -> Result<Change> {
        if !matches!(width, 2 | 4 | 8) || !(end - start).is_multiple_of(width as u64) {
            return Err(Error::invalid(
                "selection length must be a multiple of the width",
            ));
        }
        self.transform(start, end, "Byte swap", |b| {
            for chunk in b.chunks_exact_mut(width) {
                chunk.reverse();
            }
            Ok(())
        })
    }

    pub fn reverse(&mut self, start: u64, end: u64) -> Result<Change> {
        self.transform(start, end, "Reverse", |b| {
            b.reverse();
            Ok(())
        })
    }

    /// Moves `[start, end)` so it begins at `to` in the original coordinates.
    pub fn move_block(&mut self, start: u64, end: u64, to: u64) -> Result<Change> {
        self.check_writable()?;
        self.check_range(start, end)?;
        self.check_range(to, to)?;
        if to >= start && to <= end {
            return Ok(Change {
                start,
                removed: 0,
                inserted: 0,
            });
        }
        let block = self.tree().slice(start, end);
        let removed = self.tree().remove(start, end);
        let at = if to > end { to - (end - start) } else { to };
        let tree = removed.insert(at, &block);
        let lo = start.min(at);
        let hi = end.max(at + (end - start));
        Ok(self.apply(
            tree,
            "Move block",
            Change {
                start: lo,
                removed: hi - lo,
                inserted: hi - lo,
            },
        ))
    }

    // ----- change highlighting (DISP-15) -----------------------------------

    /// Changed spans intersecting `[start, end)`, including deletion markers.
    ///
    /// A byte is unchanged when it still comes from the base provider. Runs of
    /// other bytes are classified by comparing them with the gap they leave in
    /// the base: as long as the gap is, they count as modified, the rest as
    /// inserted; a gap longer than the run marks a deletion.
    pub fn changes_in(&self, start: u64, end: u64) -> Vec<ChangeSpan> {
        changes_in(self.tree(), &self.base, start, end)
    }

    // ----- external growth (FILE-07) ---------------------------------------

    /// Appends bytes that were added to the end of the file on disk.
    /// Only valid for unmodified documents; returns the number of new bytes.
    pub fn follow_growth(&mut self) -> Result<u64> {
        let Some(fp) = file_provider(&self.base) else {
            return Ok(0);
        };
        if self.is_modified() {
            return Ok(0);
        }
        let old = self.len();
        let new = fp.refresh_len()?;
        if new <= old {
            return Ok(0);
        }
        crate::marks::map_marks(
            &mut self.marks,
            Change {
                start: old,
                removed: 0,
                inserted: new - old,
            },
        );
        let tail = Tree::from_piece(Piece::provider(self.base.clone(), old, new - old));
        let tree = self.tree().concat(&tail);
        self.history.collapse();
        self.history.set_current_tree(tree);
        if let Some(path) = &self.path {
            self.stamp = FileStamp::read(path).ok();
        }
        Ok(new - old)
    }
}

/// Changed spans of `tree` relative to `base` (see [`Document::changes_in`]).
pub fn changes_in(tree: &Tree, base: &Arc<dyn Provider>, start: u64, end: u64) -> Vec<ChangeSpan> {
    const MARGIN: u64 = 1 << 20;
    let total = tree.len();
    let lo = start.saturating_sub(MARGIN);
    let hi = end.saturating_add(MARGIN).min(total);
    let pieces = tree.pieces(lo, hi);
    let is_base = |p: &Piece| p.source.is_provider(base);
    let mut out = Vec::new();
    let mut prev_end: Option<u64> = (lo == 0).then_some(0);
    let mut after_run = false;
    let mut last_was_base = false;
    let mut i = 0;
    while i < pieces.len() {
        let (pos, p) = &pieces[i];
        if is_base(p) {
            if !after_run && prev_end.is_some_and(|pe| p.offset > pe) {
                out.push(span(*pos, *pos, ChangeKind::Deleted));
            }
            prev_end = Some(p.offset + p.len);
            after_run = false;
            last_was_base = true;
            i += 1;
            continue;
        }
        let run_start = *pos;
        let mut j = i;
        while j < pieces.len() && !is_base(&pieces[j].1) {
            j += 1;
        }
        let run_end = pieces.get(j).map_or(hi, |(p, _)| *p);
        let run_len = run_end - run_start;
        let next_start = match pieces.get(j) {
            Some((_, b)) => Some(b.offset),
            None if hi == total => Some(base.len()),
            None => None,
        };
        let gap = match (prev_end, next_start) {
            (Some(a), Some(b)) if b >= a => b - a,
            _ => run_len,
        };
        let modified = gap.min(run_len);
        if modified > 0 {
            out.push(span(run_start, run_start + modified, ChangeKind::Modified));
        }
        if modified < run_len {
            out.push(span(run_start + modified, run_end, ChangeKind::Inserted));
        }
        if gap > run_len {
            out.push(span(run_end, run_end, ChangeKind::Deleted));
        }
        after_run = true;
        last_was_base = false;
        i = j;
    }
    if last_was_base && hi == total && prev_end.is_some_and(|pe| pe < base.len()) {
        out.push(span(total, total, ChangeKind::Deleted));
    }
    if pieces.is_empty() && total == 0 && !base.is_empty() {
        out.push(span(0, 0, ChangeKind::Deleted));
    }
    out.retain(|s| {
        if s.kind == ChangeKind::Deleted {
            s.start >= start && s.start <= end
        } else {
            s.end > start && s.start < end
        }
    });
    out
}

fn span(start: u64, end: u64, kind: ChangeKind) -> ChangeSpan {
    ChangeSpan { start, end, kind }
}

pub(crate) fn file_provider(p: &Arc<dyn Provider>) -> Option<&FileProvider> {
    (p.as_ref() as &dyn std::any::Any).downcast_ref::<FileProvider>()
}

fn lock(file: &std::fs::File, share: ShareMode) -> Result<()> {
    let res = match share {
        ShareMode::Shared => return Ok(()),
        ShareMode::DenyWrite => file.try_lock_shared(),
        ShareMode::Exclusive => file.try_lock(),
    };
    res.map_err(|e| match e {
        std::fs::TryLockError::WouldBlock => Error::invalid("file is locked by another process"),
        std::fs::TryLockError::Error(e) => Error::Io(e),
    })
}

/// Follows `[a, b)` through later changes; `None` when one overlaps it.
pub(crate) fn map_range(mut a: u64, mut b: u64, later: &[Change]) -> Option<(u64, u64)> {
    for c in later {
        let c_end = c.start + c.removed;
        if c_end <= a && !(c.removed == 0 && c.start == a && a < b) {
            let delta = i128::from(c.inserted) - i128::from(c.removed);
            a = (i128::from(a) + delta) as u64;
            b = (i128::from(b) + delta) as u64;
        } else if b <= c.start {
            // Entirely after our range: nothing moves.
        } else {
            return None;
        }
    }
    Some((a, b))
}

fn bytes_tree(data: &[u8]) -> Tree {
    if data.is_empty() {
        Tree::new()
    } else {
        Tree::from_piece(Piece::bytes(data.to_vec()))
    }
}

fn run_tree(run: &TypingRun) -> (Tree, Change) {
    let n = run.bytes.len() as u64;
    let piece = Tree::from_piece(Piece::bytes(run.bytes.clone()));
    if run.insert {
        (
            run.base_tree.insert(run.start, &piece),
            Change {
                start: run.start,
                removed: 0,
                inserted: n,
            },
        )
    } else {
        let end = (run.start + n).min(run.base_tree.len());
        (
            run.base_tree.replace(run.start, end, &piece),
            Change {
                start: run.start,
                removed: end - run.start,
                inserted: n,
            },
        )
    }
}

/// Union of two consecutive changes, conservatively.
fn merge_changes(a: Change, b: Change) -> Change {
    let a_end_new = a.start + a.inserted;
    let start = a.start.min(b.start);
    let end_after = (i128::from(a_end_new.max(b.start + b.removed)) + i128::from(b.inserted)
        - i128::from(b.removed))
    .max(i128::from(b.start + b.inserted)) as u64;
    let total_delta = i128::from(a.inserted) - i128::from(a.removed) + i128::from(b.inserted)
        - i128::from(b.removed);
    let inserted = end_after - start;
    let removed = (i128::from(inserted) - total_delta).max(0) as u64;
    Change {
        start,
        removed,
        inserted,
    }
}

/// Builds a lazily generated run of `len` bytes. `phase` is the absolute
/// position, so patterns stay aligned when filling a range.
pub(crate) fn fill_tree(len: u64, phase: u64, fill: &Fill) -> Result<Tree> {
    if len == 0 {
        return Ok(Tree::new());
    }
    let piece = match fill {
        Fill::Byte { value } => Piece::fill(vec![*value], len),
        Fill::Pattern { bytes } => {
            if bytes.is_empty() {
                return Err(Error::invalid("empty fill pattern"));
            }
            let _ = phase; // patterns start at the beginning of the range
            Piece::fill(bytes.clone(), len)
        }
        Fill::Random { seed } => Piece::random(*seed, len),
        Fill::Sequence {
            start,
            step,
            width,
            big_endian,
        } => {
            if !matches!(width, 1 | 2 | 4 | 8) {
                return Err(Error::invalid("sequence width must be 1, 2, 4 or 8"));
            }
            if len > MATERIALIZE_LIMIT {
                return Err(Error::TooLarge {
                    len,
                    limit: MATERIALIZE_LIMIT,
                });
            }
            let w = usize::from(*width);
            let mut out = Vec::with_capacity(len as usize);
            let mut v = *start;
            while (out.len() as u64) < len {
                let bytes = if *big_endian {
                    v.to_be_bytes()[8 - w..].to_vec()
                } else {
                    v.to_le_bytes()[..w].to_vec()
                };
                out.extend_from_slice(&bytes);
                v = v.wrapping_add(*step);
            }
            out.truncate(len as usize);
            Piece::bytes(out)
        }
    };
    Ok(Tree::from_piece(piece))
}

/// Source of the piece covering `pos`, for diagnostics and tests.
pub fn source_at(tree: &Tree, pos: u64) -> Option<Source> {
    tree.pieces(pos, pos + 1)
        .first()
        .map(|(_, p)| p.source.clone())
}
