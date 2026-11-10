//! Saving (CORE-12, CORE-14…19).
//!
//! * **Atomic**: the document is streamed into a temporary file in the same
//!   directory, synced, given the original's permissions/owner/xattrs and
//!   renamed over the original.
//! * **In place**: only changed regions are written, ordered by a dependency
//!   graph so no region is overwritten while another one still has to read it.
//!   Needs no free disk space beyond an optional journal of overwritten bytes.
//!   The journal makes a crash recoverable (CORE-16) and keeps undo working
//!   after the save (CORE-12).

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::document::{Document, EditMode, FileStamp, ShareMode, file_provider};
use crate::error::{Error, Result};
use crate::piece::{Piece, Source};
use crate::provider::{
    FileOpenOptions, FileProvider, Overlay, Provider, read_exact_at, write_all_at,
};
use crate::task::{Cancel, Progress};
use crate::tree::Tree;

const CHUNK: u64 = 1 << 20;
const JOURNAL_MAGIC: &[u8; 8] = b"HXJRNL01";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SaveStrategy {
    /// In place when the length is unchanged, atomic otherwise; falls back to
    /// in place when the disk is full.
    #[default]
    Auto,
    Atomic,
    InPlace,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SaveOptions {
    pub strategy: SaveStrategy,
    /// Copy the original to `<name>.bak` first (CORE-17).
    pub backup: bool,
    /// Journal overwritten bytes during in-place saves (CORE-16).
    pub journal: bool,
    /// Save even when the file changed on disk (CORE-19).
    pub force: bool,
}

impl Default for SaveOptions {
    fn default() -> Self {
        Self {
            strategy: SaveStrategy::Auto,
            backup: false,
            journal: true,
            force: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveReport {
    pub strategy: SaveStrategy,
    pub bytes_written: u64,
}

impl Document {
    /// Saves to the document's own path.
    pub fn save(
        &mut self,
        opts: &SaveOptions,
        progress: Progress<'_>,
        cancel: &Cancel,
    ) -> Result<SaveReport> {
        let path = self.path.clone().ok_or(Error::NoPath)?;
        self.check_disk_stamp(&path, opts.force)?;
        if opts.backup && path.exists() {
            fs::copy(&path, backup_path(&path))?;
        }
        // A file rewritten by someone else no longer matches our base pieces
        // byte for byte, so only a full rewrite is safe.
        let changed_on_disk = self.disk_changed();
        let in_place_possible = !changed_on_disk
            && file_provider(&self.base)
                .is_some_and(|fp| fp.capabilities().writable && fp.path() == path);
        let strategy = match opts.strategy {
            SaveStrategy::InPlace if !in_place_possible => {
                return Err(Error::invalid("in-place save needs a writable file"));
            }
            SaveStrategy::Auto if in_place_possible && self.len() == self.base.len() => {
                SaveStrategy::InPlace
            }
            SaveStrategy::Auto => SaveStrategy::Atomic,
            s => s,
        };
        let report = match strategy {
            SaveStrategy::InPlace => self.save_in_place(&path, opts, progress, cancel)?,
            _ => match self.save_atomic(&path, progress, cancel) {
                Err(Error::Io(e))
                    if opts.strategy == SaveStrategy::Auto
                        && in_place_possible
                        && e.kind() == io::ErrorKind::StorageFull =>
                {
                    self.save_in_place(&path, opts, progress, cancel)?
                }
                r => r?,
            },
        };
        Ok(report)
    }

    /// Writes to `path` and makes it the document's file (CORE-18).
    pub fn save_as(&mut self, path: &Path, progress: Progress<'_>, cancel: &Cancel) -> Result<()> {
        self.save_atomic(path, progress, cancel)?;
        self.name = crate::provider::display_name(path);
        Ok(())
    }

    /// Writes a copy; the document keeps its current file (CORE-18).
    pub fn save_copy(&self, path: &Path, progress: Progress<'_>, cancel: &Cancel) -> Result<()> {
        let tree = self.snapshot();
        atomic_write(path, &tree, None, progress, cancel)?;
        Ok(())
    }

    /// Writes `[start, end)` to a file (CORE-18, EDIT-06).
    pub fn save_range(
        &self,
        start: u64,
        end: u64,
        path: &Path,
        progress: Progress<'_>,
        cancel: &Cancel,
    ) -> Result<()> {
        let tree = self.copy(start, end)?;
        atomic_write(path, &tree, None, progress, cancel)?;
        Ok(())
    }

    /// Whether the file on disk differs from what was loaded (CORE-19, FILE-06).
    pub fn disk_changed(&self) -> bool {
        match (&self.path, &self.stamp) {
            (Some(p), Some(s)) => FileStamp::read(p).map_or(true, |now| now != *s),
            _ => false,
        }
    }

    /// Accepts the current on-disk state as known (after "keep mine").
    pub fn acknowledge_disk_state(&mut self) {
        if let Some(p) = &self.path {
            self.stamp = FileStamp::read(p).ok();
        }
    }

    fn check_disk_stamp(&self, path: &Path, force: bool) -> Result<()> {
        if force {
            return Ok(());
        }
        if let Some(stamp) = self.stamp {
            match FileStamp::read(path) {
                Ok(now) if now == stamp => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                _ => return Err(Error::ExternallyModified(path.to_path_buf())),
            }
        }
        Ok(())
    }

    fn save_atomic(
        &mut self,
        path: &Path,
        progress: Progress<'_>,
        cancel: &Cancel,
    ) -> Result<SaveReport> {
        let tree = self.snapshot();
        let meta_from = self.path.as_deref().filter(|p| p.exists());
        let written = match atomic_write(path, &tree, meta_from, progress, cancel) {
            Ok(n) => n,
            Err(Error::Io(e))
                if cfg!(windows)
                    && e.kind() == io::ErrorKind::PermissionDenied
                    && self.path.as_deref() == Some(path) =>
            {
                // Some Windows setups refuse replacing a file that is open.
                // Write through instead; history before this point is lost
                // because the old bytes are gone.
                let n = overwrite_whole(path, &tree, progress, cancel)?;
                self.history.collapse();
                n
            }
            Err(e) => return Err(e),
        };
        self.rebase_on_file(path)?;
        Ok(SaveReport {
            strategy: SaveStrategy::Atomic,
            bytes_written: written,
        })
    }

    /// Re-points the document at `path`, which now holds exactly the current
    /// content. Older history entries keep reading the previous provider.
    fn rebase_on_file(&mut self, path: &Path) -> Result<()> {
        let write = self.mode() != EditMode::ReadOnly;
        let fp = FileProvider::open(
            path,
            FileOpenOptions {
                write,
                ..FileOpenOptions::default()
            },
        )
        .or_else(|_| FileProvider::open(path, FileOpenOptions::default()))?;
        lock(fp.file(), self.share);
        let base: Arc<dyn Provider> = Arc::new(fp);
        let len = base.len();
        let tree = if len == 0 {
            Tree::new()
        } else {
            Tree::from_piece(Piece::provider(base.clone(), 0, len))
        };
        self.base = base;
        self.history.rebase_current(tree);
        self.history.mark_saved();
        self.break_typing();
        self.path = Some(path.to_path_buf());
        self.stamp = FileStamp::read(path).ok();
        Ok(())
    }

    fn save_in_place(
        &mut self,
        path: &Path,
        opts: &SaveOptions,
        progress: Progress<'_>,
        cancel: &Cancel,
    ) -> Result<SaveReport> {
        let base = self.base.clone();
        let fp = file_provider(&base).ok_or(Error::Unsupported)?;
        let file = fp.file();
        let tree = self.snapshot();
        let old_len = base.len();
        let new_len = tree.len();

        let ops = plan_ops(&tree, &base);
        let steps = order_ops(&ops);
        let total: u64 = ops.iter().map(|o| o.len).sum();

        let jpath = journal_path(path);
        let mut journal = if opts.journal {
            let mut j = File::create(&jpath)?;
            j.write_all(JOURNAL_MAGIC)?;
            j.write_all(&old_len.to_le_bytes())?;
            j.sync_all()?;
            Some(JournalWriter {
                file: j,
                pos: 16,
                records: Vec::new(),
            })
        } else {
            None
        };

        let result = (|| -> Result<u64> {
            if new_len > old_len {
                file.set_len(new_len)?;
            }
            let mut held: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
            let mut done = 0u64;
            for step in &steps {
                cancel.check()?;
                match *step {
                    Step::Materialize(i) => {
                        held.insert(i, read_op(&ops[i], &base)?);
                    }
                    Step::Write(i) => {
                        let op = &ops[i];
                        let data = match held.remove(&i) {
                            Some(d) => d,
                            None => read_op(op, &base)?,
                        };
                        if let Some(j) = journal.as_mut() {
                            let old_end = op.target.saturating_add(op.len).min(old_len);
                            if op.target < old_end {
                                j.record(file, op.target, old_end - op.target)?;
                            }
                        }
                        write_all_at(file, op.target, &data)?;
                        done += op.len;
                        progress(done, total);
                    }
                }
            }
            if new_len < old_len {
                if let Some(j) = journal.as_mut() {
                    let mut at = new_len;
                    while at < old_len {
                        let n = (old_len - at).min(CHUNK);
                        j.record(file, at, n)?;
                        at += n;
                    }
                }
                file.set_len(new_len)?;
            }
            file.sync_all()?;
            Ok(done)
        })();

        let written = match result {
            Ok(n) => n,
            Err(e) => {
                // Roll back what was written so the file stays consistent.
                if journal.is_some() {
                    drop(journal);
                    let _ = rollback_journal(path);
                } else {
                    let _ = fs::remove_file(&jpath);
                }
                base.invalidate();
                return Err(e);
            }
        };

        // Old history entries keep seeing the pre-save bytes through an overlay.
        let overlay = if let Some(j) = journal {
            let records = j.records;
            // `File` has no destructor on wasm32, where `std::fs` is a stub.
            #[cfg_attr(target_family = "wasm", allow(clippy::drop_non_drop))]
            drop(j.file);
            let kept = std::env::temp_dir().join(format!(
                "hexa-undo-{}-{}.journal",
                std::process::id(),
                unique_suffix()
            ));
            if fs::rename(&jpath, &kept).is_err() {
                fs::copy(&jpath, &kept)?;
                fs::remove_file(&jpath)?;
            }
            let jf = File::open(&kept)?;
            // The handle keeps the data readable; unlink the name where the
            // OS allows it.
            let _ = fs::remove_file(&kept);
            Some(Overlay::new(jf, first_write_wins(&records)))
        } else {
            self.history.collapse();
            None
        };
        self.rebase_on_file(path)?;
        if let Some(overlay) = overlay {
            fp.set_overlay(Arc::new(overlay), self.base.clone());
        }
        Ok(SaveReport {
            strategy: SaveStrategy::InPlace,
            bytes_written: written,
        })
    }
}

fn lock(file: &File, share: ShareMode) {
    let _ = match share {
        ShareMode::Shared => Ok(()),
        ShareMode::DenyWrite => file.try_lock_shared(),
        ShareMode::Exclusive => file.try_lock(),
    };
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
}

pub fn backup_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".bak");
    PathBuf::from(s)
}

/// Sidecar file holding the in-place save journal of `path`.
pub fn journal_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.hexa-journal"))
}

/// Whether an interrupted in-place save left a journal behind.
pub fn has_pending_journal(path: &Path) -> bool {
    journal_path(path).exists()
}

/// Restores the file to its state before the interrupted save and removes
/// the journal.
pub fn rollback_journal(path: &Path) -> Result<()> {
    let jpath = journal_path(path);
    let mut j = File::open(&jpath)?;
    let mut header = [0u8; 16];
    j.read_exact(&mut header)?;
    if &header[..8] != JOURNAL_MAGIC {
        return Err(Error::invalid("not a Hexa journal"));
    }
    let old_len = u64::from_le_bytes(header[8..16].try_into().expect("8 bytes"));
    let mut records = Vec::new();
    let mut pos = 16u64;
    let jlen = j.metadata()?.len();
    loop {
        let mut rec = [0u8; 16];
        if pos + 16 > jlen {
            break;
        }
        read_exact_at(&j, pos, &mut rec)?;
        let target = u64::from_le_bytes(rec[..8].try_into().expect("8 bytes"));
        let len = u64::from_le_bytes(rec[8..].try_into().expect("8 bytes"));
        if pos + 16 + len > jlen {
            break; // torn record: its data write never happened
        }
        records.push((target, len, pos + 16));
        pos += 16 + len;
    }
    let f = OpenOptions::new().read(true).write(true).open(path)?;
    if f.metadata()?.len() < old_len {
        f.set_len(old_len)?;
    }
    let mut buf = Vec::new();
    for &(target, len, jpos) in records.iter().rev() {
        buf.resize(len as usize, 0);
        read_exact_at(&j, jpos, &mut buf)?;
        write_all_at(&f, target, &buf)?;
    }
    f.set_len(old_len)?;
    f.sync_all()?;
    #[cfg_attr(target_family = "wasm", allow(clippy::drop_non_drop))]
    drop(j);
    fs::remove_file(&jpath)?;
    Ok(())
}

/// Forgets a pending journal, keeping the file as it is.
pub fn discard_journal(path: &Path) -> Result<()> {
    fs::remove_file(journal_path(path))?;
    Ok(())
}

struct JournalWriter {
    file: File,
    pos: u64,
    records: Vec<(u64, u64, u64)>,
}

impl JournalWriter {
    /// Appends the current bytes of `[target, target + len)` and syncs.
    fn record(&mut self, src: &File, target: u64, len: u64) -> io::Result<()> {
        let mut data = vec![0u8; len as usize];
        read_exact_at(src, target, &mut data)?;
        let mut rec = Vec::with_capacity(16 + data.len());
        rec.extend_from_slice(&target.to_le_bytes());
        rec.extend_from_slice(&len.to_le_bytes());
        rec.extend_from_slice(&data);
        write_all_at(&self.file, self.pos, &rec)?;
        self.file.sync_data()?;
        self.records.push((target, len, self.pos + 16));
        self.pos += rec.len() as u64;
        Ok(())
    }
}

/// Maps each byte to its oldest journaled value.
fn first_write_wins(records: &[(u64, u64, u64)]) -> BTreeMap<u64, (u64, u64)> {
    let mut map: BTreeMap<u64, (u64, u64)> = BTreeMap::new();
    for &(start, len, jpos) in records {
        let end = start + len;
        let mut cursor = start;
        let covered: Vec<(u64, u64)> = map
            .range(..end)
            .filter(|(s, (e, _))| *e > start && **s < end)
            .map(|(s, (e, _))| (*s, *e))
            .collect();
        let mut gaps = Vec::new();
        for (s, e) in covered {
            if s > cursor {
                gaps.push((cursor, s));
            }
            cursor = cursor.max(e);
        }
        if cursor < end {
            gaps.push((cursor, end));
        }
        for (s, e) in gaps {
            map.insert(s, (e, jpos + (s - start)));
        }
    }
    map
}

// ---------------------------------------------------------------------------
// In-place planning
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Op {
    target: u64,
    len: u64,
    piece: Piece,
    /// Base range the op reads, if it reads the file being written.
    reads: Option<(u64, u64)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Materialize(usize),
    Write(usize),
}

fn plan_ops(tree: &Tree, base: &Arc<dyn Provider>) -> Vec<Op> {
    let mut ops = Vec::new();
    for (pos, p) in tree.pieces(0, tree.len()) {
        let from_base = p.source.is_provider(base);
        if from_base && p.offset == pos {
            continue;
        }
        let mut at = 0;
        while at < p.len {
            let n = (p.len - at).min(CHUNK);
            let piece = p.slice(at, n);
            let reads = from_base.then_some((piece.offset, piece.offset + n));
            ops.push(Op {
                target: pos + at,
                len: n,
                piece,
                reads,
            });
            at += n;
        }
    }
    ops
}

/// Orders writes so that every op reading a base region runs before any op
/// overwriting it; cycles are broken by holding one op's data in memory.
fn order_ops(ops: &[Op]) -> Vec<Step> {
    let n = ops.len();
    // Readers sorted by source start for overlap queries.
    let mut readers: Vec<(u64, u64, usize)> = ops
        .iter()
        .enumerate()
        .filter_map(|(i, o)| o.reads.map(|(s, e)| (s, e, i)))
        .collect();
    readers.sort_unstable();
    let max_len = readers.iter().map(|r| r.1 - r.0).max().unwrap_or(0);

    let mut out_edges: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut indeg = vec![0usize; n];
    for (b, op) in ops.iter().enumerate() {
        let (ts, te) = (op.target, op.target + op.len);
        let from = readers.partition_point(|r| r.0 < ts.saturating_sub(max_len));
        for &(s, e, a) in &readers[from..] {
            if s >= te {
                break;
            }
            if e > ts && a != b {
                // a reads what b overwrites: a before b.
                out_edges[a].push(b);
                indeg[b] += 1;
            }
        }
    }

    let mut steps = Vec::with_capacity(n);
    let mut done = vec![false; n];
    let mut detached = vec![false; n];
    let mut ready: Vec<usize> = (0..n).filter(|&i| indeg[i] == 0).collect();
    let mut remaining = n;
    let mut scan = 0;
    while remaining > 0 {
        if let Some(i) = ready.pop() {
            if done[i] {
                continue;
            }
            done[i] = true;
            remaining -= 1;
            steps.push(Step::Write(i));
            if !detached[i] {
                for &b in &out_edges[i] {
                    indeg[b] -= 1;
                    if indeg[b] == 0 {
                        ready.push(b);
                    }
                }
            }
            continue;
        }
        // Cycle: hold the data of some pending op in memory, which removes
        // its reads from the graph.
        while done[scan] || detached[scan] {
            scan += 1;
        }
        let i = scan;
        detached[i] = true;
        steps.push(Step::Materialize(i));
        for &b in &out_edges[i] {
            indeg[b] -= 1;
            if indeg[b] == 0 {
                ready.push(b);
            }
        }
        if indeg[i] == 0 {
            ready.push(i);
        }
    }
    steps
}

fn read_op(op: &Op, base: &Arc<dyn Provider>) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; op.len as usize];
    match (&op.piece.source, op.reads) {
        (Source::Provider(_), Some((s, _))) => {
            // Read straight from the file: the cache may hold stale pages.
            let fp = file_provider(base).ok_or(Error::Unsupported)?;
            read_exact_at(fp.file(), s, &mut buf)?;
        }
        _ => op.piece.read(0, &mut buf)?,
    }
    Ok(buf)
}

// ---------------------------------------------------------------------------
// Streaming writes
// ---------------------------------------------------------------------------

fn write_tree(out: &mut File, tree: &Tree, progress: Progress<'_>, cancel: &Cancel) -> Result<u64> {
    let total = tree.len();
    let mut buf = vec![0u8; CHUNK as usize];
    let mut at = 0;
    while at < total {
        cancel.check()?;
        let n = (total - at).min(CHUNK) as usize;
        tree.read(at, &mut buf[..n])?;
        out.write_all(&buf[..n])?;
        at += n as u64;
        progress(at, total);
    }
    Ok(total)
}

fn overwrite_whole(
    path: &Path,
    tree: &Tree,
    progress: Progress<'_>,
    cancel: &Cancel,
) -> Result<u64> {
    // Materialise first: the tree may read the very file we truncate.
    let tmp = tempfile_in(path);
    let n = {
        let mut f = File::create(&tmp.0)?;
        let n = write_tree(&mut f, tree, progress, cancel)?;
        f.sync_all()?;
        n
    };
    fs::copy(&tmp.0, path)?;
    Ok(n)
}

struct TempGuard(PathBuf, bool);

impl Drop for TempGuard {
    fn drop(&mut self) {
        if !self.1 {
            let _ = fs::remove_file(&self.0);
        }
    }
}

fn tempfile_in(target: &Path) -> TempGuard {
    let dir = target
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    TempGuard(
        dir.join(format!(
            ".{name}.{}-{}.hexa-tmp",
            std::process::id(),
            unique_suffix()
        )),
        false,
    )
}

/// Streams `tree` into `path` through a temp file and an atomic rename
/// (CORE-14). Metadata is copied from `meta_from` when given.
fn atomic_write(
    path: &Path,
    tree: &Tree,
    meta_from: Option<&Path>,
    progress: Progress<'_>,
    cancel: &Cancel,
) -> Result<u64> {
    let mut tmp = tempfile_in(path);
    let n = {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp.0)?;
        let n = write_tree(&mut f, tree, progress, cancel)?;
        f.sync_all()?;
        n
    };
    if let Some(src) = meta_from {
        copy_metadata(src, &tmp.0);
    }
    fs::rename(&tmp.0, path)?;
    tmp.1 = true;
    sync_dir(path);
    Ok(n)
}

fn copy_metadata(src: &Path, dst: &Path) {
    let Ok(meta) = fs::metadata(src) else { return };
    let _ = fs::set_permissions(dst, meta.permissions());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let _ = std::os::unix::fs::chown(dst, Some(meta.uid()), Some(meta.gid()));
        // Extended attributes, including the macOS resource fork (FILE-17).
        if let Ok(names) = xattr::list(src) {
            for name in names {
                if let Ok(Some(value)) = xattr::get(src, &name) {
                    let _ = xattr::set(dst, &name, &value);
                }
            }
        }
    }
}

fn sync_dir(path: &Path) {
    #[cfg(unix)]
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty())
        && let Ok(d) = File::open(dir)
    {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_write_wins_keeps_oldest_bytes() {
        let map = first_write_wins(&[(10, 10, 100), (5, 10, 200), (0, 30, 300)]);
        let v: Vec<_> = map.into_iter().collect();
        assert_eq!(
            v,
            vec![
                (0, (5, 300)),
                (5, (10, 200)),
                (10, (20, 100)),
                (20, (30, 320)),
            ]
        );
    }
}
