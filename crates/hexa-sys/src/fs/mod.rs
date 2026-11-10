//! File systems on a provider (DISK-05), sector ↔ file maps (DISK-06) and
//! deleted-entry recovery (DISK-07).
//!
//! Browsable: FAT12/16/32, exFAT, ext2/3/4, ISO 9660 (+Joliet, Rock Ridge
//! names) and NTFS. Detected with key superblock fields only: HFS+/HFSX,
//! APFS, Btrfs, XFS and UDF.
//!
//! All offsets are byte offsets relative to the start of the provider the
//! file system was opened on (usually a partition).

use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::sync::Arc;

use hexa_core::provider::Provider;
use hexa_core::{Cancel, Result};
use serde::{Deserialize, Serialize};

use crate::util::{check_cancel, invalid};

pub mod exfat;
pub mod ext;
pub mod fat;
pub mod iso9660;
pub mod ntfs;
pub mod probe;

pub use exfat::ExFatFs;
pub use ext::ExtFs;
pub use fat::FatFs;
pub use iso9660::IsoFs;
pub use ntfs::NtfsFs;

/// File system type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FsKind {
    Fat12,
    Fat16,
    Fat32,
    ExFat,
    Ext2,
    Ext3,
    Ext4,
    Iso9660,
    Ntfs,
    HfsPlus,
    Hfsx,
    Apfs,
    Btrfs,
    Xfs,
    Udf,
}

impl FsKind {
    /// Whether directory browsing and maps are implemented for this type.
    pub fn browsable(self) -> bool {
        !matches!(
            self,
            Self::HfsPlus | Self::Hfsx | Self::Apfs | Self::Btrfs | Self::Xfs | Self::Udf
        )
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Fat12 => "FAT12",
            Self::Fat16 => "FAT16",
            Self::Fat32 => "FAT32",
            Self::ExFat => "exFAT",
            Self::Ext2 => "ext2",
            Self::Ext3 => "ext3",
            Self::Ext4 => "ext4",
            Self::Iso9660 => "ISO 9660",
            Self::Ntfs => "NTFS",
            Self::HfsPlus => "HFS+",
            Self::Hfsx => "HFSX",
            Self::Apfs => "APFS",
            Self::Btrfs => "Btrfs",
            Self::Xfs => "XFS",
            Self::Udf => "UDF",
        }
    }
}

/// One superblock / boot-sector field, optionally with where it lives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InfoField {
    pub name: String,
    pub value: String,
    /// Byte offset of the field (navigation target).
    pub offset: Option<u64>,
}

impl InfoField {
    #[allow(clippy::needless_pass_by_value)] // Called with plain values.
    pub(crate) fn new(name: &str, value: impl ToString, offset: Option<u64>) -> Self {
        Self {
            name: name.to_string(),
            value: value.to_string(),
            offset,
        }
    }
}

/// Summary of a detected file system.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsInfo {
    pub kind: FsKind,
    pub label: Option<String>,
    pub uuid: Option<String>,
    /// Allocation unit (cluster / block) in bytes.
    pub block_size: u32,
    pub total_size: u64,
    pub free_size: Option<u64>,
    pub fields: Vec<InfoField>,
}

/// A file or directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    /// FS-specific number: FAT/exFAT first cluster, ext inode, ISO extent
    /// LBA, NTFS MFT record.
    pub id: u64,
    /// FS-specific extra locator (opaque to callers).
    #[serde(default)]
    pub aux: u64,
    pub name: String,
    /// Full path from the root, `/`-separated.
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub deleted: bool,
    /// Directory entry / inode / MFT record offset (navigation target).
    pub metadata_offset: Option<u64>,
    /// Unix seconds.
    pub modified: Option<i64>,
    pub created: Option<i64>,
    /// Raw FS attribute bits.
    pub attributes: u32,
}

impl FileEntry {
    pub(crate) fn root(id: u64) -> Self {
        Self {
            id,
            aux: 0,
            name: String::new(),
            path: "/".into(),
            is_dir: true,
            size: 0,
            deleted: false,
            metadata_offset: None,
            modified: None,
            created: None,
            attributes: 0,
        }
    }

    pub(crate) fn child_path(parent: &str, name: &str) -> String {
        if parent.ends_with('/') {
            format!("{parent}{name}")
        } else {
            format!("{parent}/{name}")
        }
    }
}

/// A run of file content: `len` bytes at file offset `logical` stored at
/// volume offset `physical` (`None` = sparse hole).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Extent {
    pub logical: u64,
    pub physical: Option<u64>,
    pub len: u64,
}

/// What a region of the volume holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RegionKind {
    Metadata,
    File,
    Directory,
}

/// A labelled byte range of the volume (DISK-06).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Region {
    pub start: u64,
    pub len: u64,
    pub kind: RegionKind,
    /// `FAT #1`, `Inode table (group 3)`, or the file path.
    pub label: String,
    /// File id for file/directory regions.
    pub file_id: Option<u64>,
}

impl Region {
    pub(crate) fn meta(start: u64, len: u64, label: impl Into<String>) -> Self {
        Self {
            start,
            len,
            kind: RegionKind::Metadata,
            label: label.into(),
            file_id: None,
        }
    }

    pub fn end(&self) -> u64 {
        self.start.saturating_add(self.len)
    }
}

/// A browsable file system (DISK-05).
pub trait FileSystem: Send + Sync + fmt::Debug {
    fn info(&self) -> FsInfo;

    /// The provider the file system lives on.
    fn provider(&self) -> &Arc<dyn Provider>;

    fn root(&self) -> FileEntry;

    fn list_dir(&self, dir: &FileEntry) -> Result<Vec<FileEntry>>;

    /// File → volume extents (DISK-06).
    fn extents(&self, file: &FileEntry) -> Result<Vec<Extent>>;

    /// Fixed metadata areas: boot sectors, FATs, inode tables, the MFT …
    fn metadata_regions(&self) -> Result<Vec<Region>>;

    /// Finer description of a metadata offset (`inode 12`, `MFT record 5`).
    fn describe_offset(&self, _offset: u64) -> Option<String> {
        None
    }

    /// Deleted entries that can still be located (DISK-07).
    fn deleted(&self, _cancel: Option<&Cancel>) -> Result<Vec<FileEntry>> {
        Ok(Vec::new())
    }

    /// Reads file content at `offset`; returns the number of bytes read
    /// (short at end of file). Holes read as zeros.
    fn read_file(&self, file: &FileEntry, offset: u64, buf: &mut [u8]) -> Result<usize> {
        read_via_extents(
            self.provider().as_ref(),
            &self.extents(file)?,
            file.size,
            offset,
            buf,
        )
    }

    /// Finds an entry by `/`-separated path (case-insensitive fallback).
    fn lookup(&self, path: &str) -> Result<Option<FileEntry>> {
        let mut cur = self.root();
        for part in path.split('/').filter(|p| !p.is_empty()) {
            let children = self.list_dir(&cur)?;
            let found = children
                .iter()
                .find(|c| c.name == part)
                .or_else(|| children.iter().find(|c| c.name.eq_ignore_ascii_case(part)));
            match found {
                Some(c) => cur = c.clone(),
                None => return Ok(None),
            }
        }
        Ok(Some(cur))
    }
}

/// Copies file bytes described by `extents` into `buf`.
pub(crate) fn read_via_extents(
    p: &dyn Provider,
    extents: &[Extent],
    size: u64,
    offset: u64,
    buf: &mut [u8],
) -> Result<usize> {
    if offset >= size {
        return Ok(0);
    }
    let n = (size - offset).min(buf.len() as u64) as usize;
    let out = &mut buf[..n];
    out.fill(0);
    let end = offset + n as u64;
    for e in extents {
        let (lo, hi) = (e.logical.max(offset), (e.logical + e.len).min(end));
        if lo >= hi {
            continue;
        }
        if let Some(phys) = e.physical {
            let dst = &mut out[(lo - offset) as usize..(hi - offset) as usize];
            let src = phys + (lo - e.logical);
            let avail = p.len().saturating_sub(src).min(dst.len() as u64) as usize;
            p.read_at(src, &mut dst[..avail])?;
        }
    }
    Ok(n)
}

/// Merges `(logical, physical, len)` runs that continue each other.
pub(crate) fn push_extent(out: &mut Vec<Extent>, logical: u64, physical: Option<u64>, len: u64) {
    if len == 0 {
        return;
    }
    if let Some(last) = out.last_mut()
        && last.logical + last.len == logical
        && match (last.physical, physical) {
            (Some(a), Some(b)) => a + last.len == b,
            (None, None) => true,
            _ => false,
        }
    {
        last.len += len;
        return;
    }
    out.push(Extent {
        logical,
        physical,
        len,
    });
}

/// Truncates extents to `size` bytes of file content.
pub(crate) fn truncate_extents(extents: &mut Vec<Extent>, size: u64) {
    extents.retain(|e| e.logical < size);
    for e in extents.iter_mut() {
        e.len = e.len.min(size - e.logical);
    }
}

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

/// Detects the file system on `p` and returns its summary, including types
/// that are only recognised (HFS+, APFS, Btrfs, XFS, UDF).
pub fn probe_filesystem(p: &Arc<dyn Provider>) -> Result<Option<FsInfo>> {
    if let Some(fs) = try_open(p)? {
        return Ok(Some(fs.info()));
    }
    probe::probe_other(p.as_ref())
}

/// Opens a browsable file system on `p`.
pub fn open_filesystem(p: &Arc<dyn Provider>) -> Result<Box<dyn FileSystem>> {
    match try_open(p)? {
        Some(fs) => Ok(fs),
        None => match probe::probe_other(p.as_ref())? {
            Some(info) => Err(invalid(format!(
                "{} is detected but cannot be browsed",
                info.kind.display_name()
            ))),
            None => Err(invalid("no supported file system found")),
        },
    }
}

fn try_open(p: &Arc<dyn Provider>) -> Result<Option<Box<dyn FileSystem>>> {
    if p.len() < 512 {
        return Ok(None);
    }
    let mut boot = [0u8; 512];
    p.read_at(0, &mut boot)?;
    if &boot[3..11] == b"NTFS    " {
        return Ok(Some(Box::new(NtfsFs::open(p.clone())?)));
    }
    if &boot[3..11] == b"EXFAT   " {
        return Ok(Some(Box::new(ExFatFs::open(p.clone())?)));
    }
    if FatFs::detect(&boot) {
        return Ok(Some(Box::new(FatFs::open(p.clone())?)));
    }
    if ExtFs::detect(p.as_ref())? {
        return Ok(Some(Box::new(ExtFs::open(p.clone())?)));
    }
    if IsoFs::detect(p.as_ref())? {
        return Ok(Some(Box::new(IsoFs::open(p.clone())?)));
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
// Maps (DISK-06)
// ---------------------------------------------------------------------------

/// Sector → owner map of a whole volume (DISK-06).
#[derive(Debug, Clone, Default)]
pub struct FsMap {
    files: Vec<Region>,
    meta: Vec<Region>,
    max_file_len: u64,
    max_meta_len: u64,
}

/// Upper bound on entries walked when building a map.
pub const MAP_ENTRY_LIMIT: usize = 2_000_000;

impl FsMap {
    /// Walks every directory and records file extents and metadata areas.
    pub fn build(fs: &dyn FileSystem, cancel: Option<&Cancel>) -> Result<Self> {
        let mut files = Vec::new();
        let mut seen: HashSet<(u64, bool)> = HashSet::new();
        let root = fs.root();
        for x in fs.extents(&root).unwrap_or_default() {
            if let Some(phys) = x.physical {
                files.push(Region {
                    start: phys,
                    len: x.len,
                    kind: RegionKind::Directory,
                    label: root.path.clone(),
                    file_id: Some(root.id),
                });
            }
        }
        let mut queue = VecDeque::from([root]);
        let mut walked = 0usize;
        while let Some(dir) = queue.pop_front() {
            check_cancel(cancel)?;
            let Ok(children) = fs.list_dir(&dir) else {
                continue;
            };
            for e in children {
                walked += 1;
                if walked > MAP_ENTRY_LIMIT {
                    return Err(invalid("too many entries to map"));
                }
                if e.name == "." || e.name == ".." || !seen.insert((e.id, e.is_dir)) {
                    continue;
                }
                let kind = if e.is_dir {
                    RegionKind::Directory
                } else {
                    RegionKind::File
                };
                if let Ok(exts) = fs.extents(&e) {
                    for x in exts {
                        if let Some(phys) = x.physical {
                            files.push(Region {
                                start: phys,
                                len: x.len,
                                kind,
                                label: e.path.clone(),
                                file_id: Some(e.id),
                            });
                        }
                    }
                }
                if e.is_dir {
                    queue.push_back(e);
                }
            }
        }
        Ok(Self::from_regions(files, fs.metadata_regions()?))
    }

    /// A map from explicit regions.
    pub fn from_regions(mut files: Vec<Region>, mut meta: Vec<Region>) -> Self {
        files.sort_by_key(|r| r.start);
        meta.sort_by_key(|r| r.start);
        Self {
            max_file_len: files.iter().map(|r| r.len).max().unwrap_or(0),
            max_meta_len: meta.iter().map(|r| r.len).max().unwrap_or(0),
            files,
            meta,
        }
    }

    fn find(list: &[Region], max_len: u64, offset: u64) -> Option<&Region> {
        let idx = list.partition_point(|r| r.start <= offset);
        let mut best: Option<&Region> = None;
        for r in list[..idx].iter().rev() {
            if r.start.saturating_add(max_len) <= offset {
                break;
            }
            if offset < r.end() && best.is_none_or(|b| r.len < b.len) {
                best = Some(r);
            }
        }
        best
    }

    /// Who owns the byte at `offset`: a file/directory first, then metadata.
    pub fn owner_at(&self, offset: u64) -> Option<&Region> {
        Self::find(&self.files, self.max_file_len, offset)
            .or_else(|| Self::find(&self.meta, self.max_meta_len, offset))
    }

    pub fn file_regions(&self) -> &[Region] {
        &self.files
    }

    pub fn metadata_regions(&self) -> &[Region] {
        &self.meta
    }
}

// ---------------------------------------------------------------------------
// Time helpers
// ---------------------------------------------------------------------------

/// Days since 1970-01-01 of a proleptic Gregorian date.
pub(crate) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Unix time of a date/time, `None` when out of range.
pub(crate) fn unix_time(y: i64, mo: i64, d: i64, h: i64, mi: i64, s: i64) -> Option<i64> {
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + s)
}

/// FAT/exFAT DOS date + time.
pub(crate) fn dos_time(date: u16, time: u16) -> Option<i64> {
    if date == 0 {
        return None;
    }
    unix_time(
        1980 + i64::from(date >> 9),
        i64::from((date >> 5) & 0xF),
        i64::from(date & 0x1F),
        i64::from(time >> 11),
        i64::from((time >> 5) & 0x3F),
        i64::from(time & 0x1F) * 2,
    )
}

/// Windows FILETIME (100 ns since 1601) → Unix seconds.
pub(crate) fn filetime(ft: u64) -> Option<i64> {
    if ft == 0 {
        return None;
    }
    Some((ft / 10_000_000) as i64 - 11_644_473_600)
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;
    use hexa_core::provider::MemoryProvider;

    pub(crate) fn mem(data: Vec<u8>) -> Arc<dyn Provider> {
        Arc::new(MemoryProvider::new("img", data))
    }

    pub(crate) fn put(buf: &mut [u8], at: usize, bytes: &[u8]) {
        buf[at..at + bytes.len()].copy_from_slice(bytes);
    }

    pub(crate) fn put16(buf: &mut [u8], at: usize, v: u16) {
        put(buf, at, &v.to_le_bytes());
    }

    pub(crate) fn put32(buf: &mut [u8], at: usize, v: u32) {
        put(buf, at, &v.to_le_bytes());
    }

    pub(crate) fn put64(buf: &mut [u8], at: usize, v: u64) {
        put(buf, at, &v.to_le_bytes());
    }

    pub(crate) fn read_all(fs: &dyn FileSystem, e: &FileEntry) -> Vec<u8> {
        let mut buf = vec![0u8; e.size as usize];
        let n = fs.read_file(e, 0, &mut buf).unwrap();
        buf.truncate(n);
        buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_conversions() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(unix_time(2000, 3, 1, 0, 0, 0), Some(951_868_800));
        // 2020-05-17 12:30:10
        let date = ((2020 - 1980) << 9) | (5 << 5) | 0x11;
        let time = (12 << 11) | (30 << 5) | 5;
        assert_eq!(dos_time(date, time), unix_time(2020, 5, 17, 12, 30, 10));
        assert_eq!(filetime(116_444_736_000_000_000), Some(0));
    }

    #[test]
    fn map_prefers_files_and_smallest_region() {
        let file = |s, l, n: &str| Region {
            start: s,
            len: l,
            kind: RegionKind::File,
            label: n.into(),
            file_id: Some(1),
        };
        let m = FsMap::from_regions(
            vec![file(1000, 100, "/a"), file(5000, 10, "/b")],
            vec![
                Region::meta(0, 10_000, "MFT"),
                Region::meta(4096, 1024, "record 4"),
            ],
        );
        assert_eq!(m.owner_at(1050).unwrap().label, "/a");
        assert_eq!(m.owner_at(4100).unwrap().label, "record 4");
        assert_eq!(m.owner_at(5005).unwrap().label, "/b");
        assert_eq!(m.owner_at(9000).unwrap().label, "MFT");
        assert!(m.owner_at(20_000).is_none());
    }
}
