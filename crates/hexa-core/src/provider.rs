//! Data providers (CORE-01…03, CORE-06).
//!
//! A provider is a random-access byte source. The document never mutates a
//! provider while editing: all edits live in the piece tree. Providers are only
//! written to by the save machinery.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use lru::LruCache;
use parking_lot::{Mutex, RwLock};
use serde::Serialize;

/// What a provider can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub readable: bool,
    pub writable: bool,
    pub resizable: bool,
    /// Sector size when I/O must be sector aligned (disks).
    pub sector_size: Option<u32>,
    /// Content can change underneath us (process memory, growing files).
    pub live: bool,
}

impl Capabilities {
    pub const READ_ONLY: Self = Self {
        readable: true,
        writable: false,
        resizable: false,
        sector_size: None,
        live: false,
    };
}

/// A random-access byte source (CORE-01).
pub trait Provider: std::any::Any + Send + Sync + fmt::Debug {
    /// Current length in bytes.
    fn len(&self) -> u64;

    /// Whether the provider holds no bytes.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Fills `buf` with the bytes at `offset`. Reading past the end is an error.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()>;

    fn capabilities(&self) -> Capabilities;

    /// Human readable name (file name, "Untitled", "PID 42" …).
    fn name(&self) -> String;

    fn write_at(&self, _offset: u64, _data: &[u8]) -> io::Result<()> {
        Err(unsupported())
    }

    fn resize(&self, _len: u64) -> io::Result<()> {
        Err(unsupported())
    }

    fn insert(&self, _offset: u64, _data: &[u8]) -> io::Result<()> {
        Err(unsupported())
    }

    fn remove(&self, _offset: u64, _len: u64) -> io::Result<()> {
        Err(unsupported())
    }

    fn flush(&self) -> io::Result<()> {
        Ok(())
    }

    /// Drops cached pages; the next reads go to the backing store.
    fn invalidate(&self) {}
}

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "operation not supported by provider",
    )
}

fn check_range(offset: u64, len: usize, total: u64) -> io::Result<()> {
    let end = offset.checked_add(len as u64);
    match end {
        Some(end) if end <= total => Ok(()),
        _ => Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("read {offset}+{len} past end {total}"),
        )),
    }
}

// ---------------------------------------------------------------------------
// Memory provider
// ---------------------------------------------------------------------------

/// An in-memory buffer (new documents, "load fully into RAM" mode).
pub struct MemoryProvider {
    name: String,
    data: RwLock<Vec<u8>>,
}

impl fmt::Debug for MemoryProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryProvider")
            .field("name", &self.name)
            .field("len", &self.data.read().len())
            .finish()
    }
}

impl MemoryProvider {
    pub fn new(name: impl Into<String>, data: Vec<u8>) -> Self {
        Self {
            name: name.into(),
            data: RwLock::new(data),
        }
    }

    /// Loads a whole file into memory.
    pub fn from_file(path: &Path) -> io::Result<Self> {
        let data = std::fs::read(path)?;
        Ok(Self::new(display_name(path), data))
    }
}

impl Provider for MemoryProvider {
    fn len(&self) -> u64 {
        self.data.read().len() as u64
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let data = self.data.read();
        check_range(offset, buf.len(), data.len() as u64)?;
        let start = offset as usize;
        buf.copy_from_slice(&data[start..start + buf.len()]);
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            readable: true,
            writable: true,
            resizable: true,
            sector_size: None,
            live: false,
        }
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> io::Result<()> {
        let mut buf = self.data.write();
        let end = offset as usize + data.len();
        if end > buf.len() {
            buf.resize(end, 0);
        }
        buf[offset as usize..end].copy_from_slice(data);
        Ok(())
    }

    fn resize(&self, len: u64) -> io::Result<()> {
        self.data.write().resize(len as usize, 0);
        Ok(())
    }

    fn insert(&self, offset: u64, data: &[u8]) -> io::Result<()> {
        let mut buf = self.data.write();
        check_range(offset, 0, buf.len() as u64)?;
        let at = offset as usize;
        buf.splice(at..at, data.iter().copied());
        Ok(())
    }

    fn remove(&self, offset: u64, len: u64) -> io::Result<()> {
        let mut buf = self.data.write();
        check_range(offset, len as usize, buf.len() as u64)?;
        let at = offset as usize;
        buf.drain(at..at + len as usize);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// File provider with a paged LRU cache
// ---------------------------------------------------------------------------

/// Default page size of the read cache (CORE-06).
pub const DEFAULT_PAGE_SIZE: usize = 64 * 1024;
/// Default memory budget of the read cache.
pub const DEFAULT_CACHE_BUDGET: usize = 128 * 1024 * 1024;

/// Pre-save bytes of regions overwritten by an in-place save. Installed on the
/// provider that old history snapshots still reference, so undo keeps working
/// after an in-place save (CORE-12 + CORE-15).
#[derive(Debug)]
pub struct Overlay {
    journal: File,
    /// start → (end, position of the bytes inside `journal`)
    segments: BTreeMap<u64, (u64, u64)>,
}

impl Overlay {
    pub(crate) fn new(journal: File, segments: BTreeMap<u64, (u64, u64)>) -> Self {
        Self { journal, segments }
    }

    /// Copies overlay bytes intersecting `[offset, offset + buf.len())` over `buf`.
    fn apply(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let end = offset + buf.len() as u64;
        let first = self
            .segments
            .range(..=offset)
            .next_back()
            .map_or(offset, |(&s, _)| s);
        for (&s, &(e, jpos)) in self.segments.range(first..end) {
            let lo = s.max(offset);
            let hi = e.min(end);
            if lo >= hi {
                continue;
            }
            let dst = &mut buf[(lo - offset) as usize..(hi - offset) as usize];
            read_exact_at(&self.journal, jpos + (lo - s), dst)?;
        }
        Ok(())
    }
}

/// Overlay of a frozen provider plus the provider to read the rest from.
type Frozen = (Arc<Overlay>, Arc<dyn Provider>);

/// A file opened lazily: nothing is read until a page is requested (FILE-02).
pub struct FileProvider {
    file: File,
    path: PathBuf,
    writable: bool,
    len: AtomicU64,
    page_size: usize,
    cache: Mutex<LruCache<u64, Arc<[u8]>>>,
    /// Pre-save bytes plus the provider of the next file generation to read
    /// everything else from (see [`Overlay`]).
    overlay: RwLock<Option<Frozen>>,
}

impl fmt::Debug for FileProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileProvider")
            .field("path", &self.path)
            .field("len", &self.len())
            .field("writable", &self.writable)
            .finish_non_exhaustive()
    }
}

/// How to open a file provider.
#[derive(Debug, Clone, Copy)]
pub struct FileOpenOptions {
    pub write: bool,
    pub page_size: usize,
    pub cache_budget: usize,
}

impl Default for FileOpenOptions {
    fn default() -> Self {
        Self {
            write: false,
            page_size: DEFAULT_PAGE_SIZE,
            cache_budget: DEFAULT_CACHE_BUDGET,
        }
    }
}

impl FileProvider {
    pub fn open(path: &Path, opts: FileOpenOptions) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(opts.write).open(path)?;
        let len = file.metadata()?.len();
        let page_size = opts.page_size.clamp(4096, 1 << 20);
        let pages = (opts.cache_budget / page_size).max(4);
        Ok(Self {
            file,
            path: path.to_path_buf(),
            writable: opts.write,
            len: AtomicU64::new(len),
            page_size,
            cache: Mutex::new(LruCache::new(NonZeroUsize::new(pages).expect("pages > 0"))),
            overlay: RwLock::new(None),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn file(&self) -> &File {
        &self.file
    }

    /// Re-reads the file length (growing logs, FILE-07). Returns the new length.
    pub fn refresh_len(&self) -> io::Result<u64> {
        let len = self.file.metadata()?.len();
        let old = self.len.swap(len, Ordering::SeqCst);
        if len < old {
            self.invalidate();
        } else if len > old {
            // The last page may have been cached short.
            let last = old / self.page_size as u64;
            self.cache.lock().pop(&last);
        }
        Ok(len)
    }

    /// Freezes this provider at its pre-save content: bytes overwritten by an
    /// in-place save come from `overlay`, the rest from `below` (the provider
    /// of the saved file, which may itself be frozen by a later save).
    pub(crate) fn set_overlay(&self, overlay: Arc<Overlay>, below: Arc<dyn Provider>) {
        *self.overlay.write() = Some((overlay, below));
        self.invalidate();
    }

    fn read_raw(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let guard = self.overlay.read();
        let Some((overlay, below)) = guard.as_ref() else {
            return read_exact_at(&self.file, offset, buf);
        };
        let avail = below.len().saturating_sub(offset).min(buf.len() as u64) as usize;
        below.read_at(offset, &mut buf[..avail])?;
        buf[avail..].fill(0);
        overlay.apply(offset, buf)
    }

    fn page(&self, index: u64) -> io::Result<Arc<[u8]>> {
        if let Some(page) = self.cache.lock().get(&index) {
            return Ok(page.clone());
        }
        let start = index * self.page_size as u64;
        let len = self.len();
        let size = (len.saturating_sub(start)).min(self.page_size as u64) as usize;
        let mut buf = vec![0u8; size];
        self.read_raw(start, &mut buf)?;
        let page: Arc<[u8]> = buf.into();
        self.cache.lock().put(index, page.clone());
        Ok(page)
    }
}

impl Provider for FileProvider {
    fn len(&self) -> u64 {
        self.len.load(Ordering::SeqCst)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        check_range(offset, buf.len(), self.len())?;
        let ps = self.page_size as u64;
        // Large reads bypass the cache so a hash of a 100 GB file does not
        // evict the pages the user is looking at.
        if buf.len() >= 4 * self.page_size {
            return self.read_raw(offset, buf);
        }
        let mut done = 0usize;
        while done < buf.len() {
            let pos = offset + done as u64;
            let page = self.page(pos / ps)?;
            let in_page = (pos % ps) as usize;
            let n = (page.len() - in_page).min(buf.len() - done);
            if n == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "short page"));
            }
            buf[done..done + n].copy_from_slice(&page[in_page..in_page + n]);
            done += n;
        }
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            readable: true,
            writable: self.writable,
            resizable: self.writable,
            sector_size: None,
            live: false,
        }
    }

    fn name(&self) -> String {
        display_name(&self.path)
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> io::Result<()> {
        if !self.writable {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "read-only"));
        }
        write_all_at(&self.file, offset, data)?;
        let end = offset + data.len() as u64;
        self.len.fetch_max(end, Ordering::SeqCst);
        self.invalidate();
        Ok(())
    }

    fn resize(&self, len: u64) -> io::Result<()> {
        if !self.writable {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "read-only"));
        }
        self.file.set_len(len)?;
        self.len.store(len, Ordering::SeqCst);
        self.invalidate();
        Ok(())
    }

    fn flush(&self) -> io::Result<()> {
        self.file.sync_all()
    }

    fn invalidate(&self) {
        self.cache.lock().clear();
    }
}

// ---------------------------------------------------------------------------
// Range provider (CORE-03)
// ---------------------------------------------------------------------------

/// A window onto `[start, start + len)` of another provider.
#[derive(Debug)]
pub struct RangeProvider {
    inner: Arc<dyn Provider>,
    start: u64,
    len: u64,
}

impl RangeProvider {
    pub fn new(inner: Arc<dyn Provider>, start: u64, len: u64) -> io::Result<Self> {
        check_range(start, 0, inner.len())?;
        if start.checked_add(len).is_none_or(|e| e > inner.len()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "window past end",
            ));
        }
        Ok(Self { inner, start, len })
    }
}

impl Provider for RangeProvider {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        check_range(offset, buf.len(), self.len)?;
        self.inner.read_at(self.start + offset, buf)
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            resizable: false,
            ..self.inner.capabilities()
        }
    }

    fn name(&self) -> String {
        format!(
            "{} [{:#x}..{:#x}]",
            self.inner.name(),
            self.start,
            self.start + self.len
        )
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> io::Result<()> {
        check_range(offset, data.len(), self.len)?;
        self.inner.write_at(self.start + offset, data)
    }
}

// ---------------------------------------------------------------------------
// Positional I/O helpers
// ---------------------------------------------------------------------------

pub(crate) fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

#[cfg(unix)]
pub(crate) fn read_exact_at(file: &File, offset: u64, buf: &mut [u8]) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(buf, offset)
}

#[cfg(windows)]
pub(crate) fn read_exact_at(file: &File, mut offset: u64, mut buf: &mut [u8]) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_read(buf, offset) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                buf = &mut buf[n..];
                offset += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn write_all_at(file: &File, offset: u64, data: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.write_all_at(data, offset)
}

#[cfg(windows)]
pub(crate) fn write_all_at(file: &File, mut offset: u64, mut data: &[u8]) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !data.is_empty() {
        match file.seek_write(data, offset) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => {
                data = &data[n..];
                offset += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn memory_provider_edits() {
        let p = MemoryProvider::new("m", b"hello".to_vec());
        p.insert(5, b" world").unwrap();
        p.remove(0, 1).unwrap();
        p.write_at(0, b"J").unwrap();
        let mut buf = vec![0; p.len() as usize];
        p.read_at(0, &mut buf).unwrap();
        assert_eq!(&buf, b"Jllo world");
        assert!(p.read_at(5, &mut [0; 10]).is_err());
    }

    #[test]
    fn file_provider_reads_across_pages() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        let data: Vec<u8> = (0..20_000u32).map(|i| (i * 7) as u8).collect();
        tmp.write_all(&data).unwrap();
        let p = FileProvider::open(
            tmp.path(),
            FileOpenOptions {
                page_size: 4096,
                cache_budget: 4096 * 2,
                ..FileOpenOptions::default()
            },
        )
        .unwrap();
        for &(off, len) in &[(0, 10), (4090, 20), (8000, 9000), (19_990, 10)] {
            let mut buf = vec![0; len];
            p.read_at(off, &mut buf).unwrap();
            assert_eq!(buf, data[off as usize..off as usize + len]);
        }
        assert!(p.read_at(19_995, &mut [0; 10]).is_err());
    }

    #[test]
    fn range_provider_window() {
        let base: Arc<dyn Provider> = Arc::new(MemoryProvider::new("m", (0..100).collect()));
        let r = RangeProvider::new(base, 10, 20).unwrap();
        let mut buf = [0; 5];
        r.read_at(15, &mut buf).unwrap();
        assert_eq!(buf, [25, 26, 27, 28, 29]);
        assert!(r.read_at(18, &mut buf).is_err());
    }
}
