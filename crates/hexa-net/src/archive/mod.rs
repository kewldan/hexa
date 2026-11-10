//! Files inside archives (FILE-15).
//!
//! [`Archive`] lists the entries of zip, tar, tar.gz / tgz, single-member gz
//! and 7z archives, reads an entry, opens it as a [`Provider`] and writes a
//! modified entry back by repacking the whole archive into a new file
//! atomically (temporary file in the destination directory, then rename):
//!
//! * zip: every other entry is copied raw (compressed bytes untouched); the
//!   replaced entry keeps its name, attributes and timestamp, and is stored
//!   when it was stored, deflated otherwise.
//! * tar / tar.gz: headers, GNU long-name and PAX records are copied as they
//!   are; the replaced entry gets a new size (also in its PAX `size` record)
//!   and checksum. tar.gz and gz are recompressed, keeping the gzip file name
//!   and modification time.
//! * 7z: rewritten entry by entry (non-solid LZMA2), since compressed 7z
//!   blocks cannot be copied raw by the 7z crate.
//!
//! Entries stored uncompressed (zip `Stored`, plain tar) open as a window on
//! the archive file without copying; everything else is decompressed into
//! memory up to [`Archive::set_max_entry_size`]. Entry providers are
//! read-only: saving goes through [`Archive::repack`].

mod gzfmt;
mod sevenz;
mod tarfmt;
mod zipfmt;

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hexa_core::provider::{
    Capabilities, FileOpenOptions, FileProvider, MemoryProvider, Provider, RangeProvider,
};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Default cap on entries decompressed into memory.
pub const DEFAULT_MAX_ENTRY_SIZE: u64 = 1 << 30;

/// Container format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ArchiveKind {
    Zip,
    Tar,
    /// gzip-compressed tar (`.tar.gz`, `.tgz`).
    TarGz,
    /// Single-member gzip file; its one entry is the decompressed data.
    Gzip,
    SevenZip,
}

impl ArchiveKind {
    /// Detects the format from magic bytes (and the extension for old tars
    /// without the `ustar` magic). `Ok(None)` when it is not an archive.
    pub fn detect(path: &Path) -> Result<Option<Self>> {
        let mut head = Vec::with_capacity(512);
        File::open(path)?.take(512).read_to_end(&mut head)?;
        let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        let named = |suffix: &str| file_name.as_deref().is_some_and(|n| has_suffix(n, suffix));
        let ustar = |b: &[u8]| b.len() >= 262 && &b[257..262] == b"ustar";
        if head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06") {
            return Ok(Some(Self::Zip));
        }
        if head.starts_with(b"7z\xBC\xAF\x27\x1C") {
            return Ok(Some(Self::SevenZip));
        }
        if head.starts_with(&[0x1f, 0x8b]) {
            let mut inner = Vec::with_capacity(512);
            let decoder = flate2::read::MultiGzDecoder::new(File::open(path)?);
            // A corrupt stream is still reported as gzip; reading it fails later.
            let _ = decoder.take(512).read_to_end(&mut inner);
            let tar_name = named("tar.gz") || named("tgz");
            return Ok(Some(if ustar(&inner) || tar_name {
                Self::TarGz
            } else {
                Self::Gzip
            }));
        }
        if ustar(&head) || (named("tar") && head.len() >= 512) {
            return Ok(Some(Self::Tar));
        }
        Ok(None)
    }
}

/// One entry of an archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveEntry {
    /// Position in the archive's own order.
    pub index: usize,
    /// Path inside the archive (`/` separated).
    pub name: String,
    /// Uncompressed size (for gz: the trailer's size modulo 2^32).
    pub size: u64,
    pub compressed_size: Option<u64>,
    pub is_dir: bool,
    /// Compression method (`Stored`, `Deflated`, `gzip`, `7z` …).
    pub method: String,
    /// Offset of the raw bytes in the archive file when the entry is stored
    /// uncompressed and contiguously.
    pub data_offset: Option<u64>,
    /// Modification time, Unix seconds.
    pub modified: Option<i64>,
    pub encrypted: bool,
}

/// An archive on disk and its entry list (FILE-15).
#[derive(Debug, Clone)]
pub struct Archive {
    path: PathBuf,
    kind: ArchiveKind,
    entries: Vec<ArchiveEntry>,
    max_entry_size: u64,
}

impl Archive {
    /// Detects the format and lists the entries.
    pub fn open(path: &Path) -> Result<Self> {
        let kind = ArchiveKind::detect(path)?
            .ok_or_else(|| Error::Unsupported(format!("{} is not an archive", path.display())))?;
        Self::open_as(path, kind)
    }

    /// Lists the entries, assuming the given format.
    pub fn open_as(path: &Path, kind: ArchiveKind) -> Result<Self> {
        let entries = match kind {
            ArchiveKind::Zip => zipfmt::list(path)?,
            ArchiveKind::Tar => tarfmt::list(path, false)?,
            ArchiveKind::TarGz => tarfmt::list(path, true)?,
            ArchiveKind::Gzip => vec![gzfmt::entry(path)?],
            ArchiveKind::SevenZip => sevenz::list(path)?,
        };
        Ok(Self {
            path: path.to_path_buf(),
            kind,
            entries,
            max_entry_size: DEFAULT_MAX_ENTRY_SIZE,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn kind(&self) -> ArchiveKind {
        self.kind
    }

    pub fn entries(&self) -> &[ArchiveEntry] {
        &self.entries
    }

    /// The entry called `name`; with duplicates the last one wins, as when
    /// extracting.
    pub fn entry(&self, name: &str) -> Option<&ArchiveEntry> {
        self.entries.iter().rev().find(|e| e.name == name)
    }

    /// Largest entry [`Self::read`] and [`Self::open_entry`] decompress into
    /// memory.
    pub fn set_max_entry_size(&mut self, limit: u64) {
        self.max_entry_size = limit;
    }

    fn file_entry(&self, name: &str) -> Result<&ArchiveEntry> {
        let entry = self
            .entry(name)
            .ok_or_else(|| Error::EntryNotFound(name.to_owned()))?;
        if entry.is_dir {
            return Err(Error::Unsupported(format!("`{name}` is a directory")));
        }
        if entry.encrypted {
            return Err(Error::Unsupported(format!("`{name}` is encrypted")));
        }
        Ok(entry)
    }

    /// Decompressed content of an entry.
    pub fn read(&self, name: &str) -> Result<Vec<u8>> {
        let entry = self.file_entry(name)?;
        let limit = self.max_entry_size;
        if entry.size > limit && self.kind != ArchiveKind::Gzip {
            return Err(Error::TooLarge {
                size: entry.size,
                limit,
            });
        }
        match self.kind {
            ArchiveKind::Zip => zipfmt::read(&self.path, entry.index, limit),
            ArchiveKind::Tar => tarfmt::read(&self.path, false, entry.index, limit),
            ArchiveKind::TarGz => tarfmt::read(&self.path, true, entry.index, limit),
            ArchiveKind::Gzip => gzfmt::read(&self.path, limit),
            ArchiveKind::SevenZip => sevenz::read(&self.path, &entry.name, limit),
        }
    }

    /// Opens an entry as a read-only provider: a window on the archive file
    /// for stored entries, decompressed bytes otherwise.
    pub fn open_entry(&self, name: &str) -> Result<Arc<dyn Provider>> {
        let entry = self.file_entry(name)?;
        let archive_name = self.path.file_name().map_or_else(
            || self.path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let display = format!("{archive_name}!/{}", entry.name);
        let inner: Arc<dyn Provider> = if let Some(offset) = entry.data_offset {
            let file: Arc<dyn Provider> =
                Arc::new(FileProvider::open(&self.path, FileOpenOptions::default())?);
            Arc::new(RangeProvider::new(file, offset, entry.size)?)
        } else {
            Arc::new(MemoryProvider::new(display.clone(), self.read(name)?))
        };
        Ok(Arc::new(EntryProvider {
            name: display,
            inner,
        }))
    }

    /// Writes a copy of the archive to `dst` with entry `name` replaced by
    /// `data`. `dst` may be the archive itself; it is replaced atomically.
    pub fn repack(&self, name: &str, data: &[u8], dst: &Path) -> Result<()> {
        let entry = self.file_entry(name)?;
        let src = self.path.as_path();
        match self.kind {
            ArchiveKind::Zip => {
                write_atomic(src, dst, |out| zipfmt::repack(src, entry.index, data, out))
            }
            ArchiveKind::Tar => write_atomic(src, dst, |out| {
                tarfmt::repack(src, false, entry.index, data, out)
            }),
            ArchiveKind::TarGz => write_atomic(src, dst, |out| {
                tarfmt::repack(src, true, entry.index, data, out)
            }),
            ArchiveKind::Gzip => write_atomic(src, dst, |out| gzfmt::repack(src, data, out)),
            ArchiveKind::SevenZip => {
                if self.entries.iter().filter(|e| e.name == entry.name).count() > 1 {
                    return Err(Error::Unsupported(format!(
                        "several 7z entries are called `{name}`"
                    )));
                }
                write_atomic(src, dst, |out| sevenz::repack(src, &entry.name, data, out))
            }
        }
    }
}

/// Whether `name` ends with `.<ext>`, ignoring ASCII case.
pub(crate) fn has_suffix(name: &str, ext: &str) -> bool {
    name.len() > ext.len()
        && name.as_bytes()[name.len() - ext.len() - 1] == b'.'
        && name[name.len() - ext.len()..].eq_ignore_ascii_case(ext)
}

/// Writes through a temporary file next to `dst`, then renames it over `dst`.
fn write_atomic(src: &Path, dst: &Path, write: impl FnOnce(&mut File) -> Result<()>) -> Result<()> {
    let dir = dst
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::Builder::new()
        .prefix(".hexa-repack-")
        .tempfile_in(dir)?;
    write(tmp.as_file_mut())?;
    tmp.as_file().sync_all()?;
    if let Ok(meta) = fs::metadata(src) {
        // Keep the original mode; failure only loses permissions.
        let _ = fs::set_permissions(tmp.path(), meta.permissions());
    }
    tmp.persist(dst).map_err(|e| Error::Io(e.error))?;
    Ok(())
}

/// Reads at most `limit` bytes, failing when there are more.
fn read_limited(reader: impl Read, limit: u64, hint: u64) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(hint.min(limit).min(64 << 20) as usize);
    reader.take(limit.saturating_add(1)).read_to_end(&mut out)?;
    if out.len() as u64 > limit {
        return Err(Error::TooLarge {
            size: out.len() as u64,
            limit,
        });
    }
    Ok(out)
}

/// Read-only provider of one archive entry.
#[derive(Debug)]
struct EntryProvider {
    name: String,
    inner: Arc<dyn Provider>,
}

impl Provider for EntryProvider {
    fn len(&self) -> u64 {
        self.inner.len()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.inner.read_at(offset, buf)
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::READ_ONLY
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn invalidate(&self) {
        self.inner.invalidate();
    }
}

#[cfg(test)]
mod tests;
