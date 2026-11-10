//! A [`Provider`] over a whole address space (PROC-03, PROC-05).
//!
//! The provider spans `[0, max user address)`. Unmapped gaps are listed by
//! [`AddressSpaceProvider::spans`] / [`AddressSpaceProvider::gaps`] so the UI
//! can collapse them into data folds.
//!
//! **Unmapped bytes are an error.** [`Provider::read_at`] fails when any byte
//! of the request is not readable; [`crate::is_unmapped`] identifies that
//! error and the UI renders `??`. To render a chunk that straddles a region
//! boundary without losing the mapped half, use
//! [`AddressSpaceProvider::read_partial`], which zero-fills unreadable pages
//! and returns which ranges are real.
//!
//! The provider is flagged `live` and never caches: every read goes to the
//! target (PROC-05). [`changed_bytes`] compares two reads for highlighting.

use std::fmt;
use std::io;
use std::ops::Range;
use std::sync::Arc;

use hexa_core::provider::{Capabilities, Provider};
use parking_lot::RwLock;

use crate::error::{Error, Result};
use crate::region::{AddressSpan, MemoryRegion, address_spans, gaps};
use crate::source::{Access, MemorySource, ProcessMemory};

/// Live view of a [`MemorySource`] as a hex-editor provider.
pub struct AddressSpaceProvider {
    source: Arc<dyn MemorySource>,
    len: u64,
    regions: RwLock<Vec<MemoryRegion>>,
}

impl fmt::Debug for AddressSpaceProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AddressSpaceProvider")
            .field("source", &self.source.name())
            .field("len", &self.len)
            .field("regions", &self.regions.read().len())
            .finish()
    }
}

impl AddressSpaceProvider {
    /// Wraps any memory source (local process, GDB remote, snapshot).
    pub fn new(source: Arc<dyn MemorySource>) -> Result<Self> {
        let regions = source.regions()?;
        let len = source
            .max_address()
            .max(regions.last().map_or(0, MemoryRegion::end));
        Ok(Self {
            source,
            len,
            regions: RwLock::new(regions),
        })
    }

    /// Opens a local process.
    pub fn open_process(pid: u32, access: Access) -> Result<Self> {
        Self::new(Arc::new(ProcessMemory::open(pid, access)?))
    }

    pub fn source(&self) -> &Arc<dyn MemorySource> {
        &self.source
    }

    /// Cached region list (refreshed by [`Self::refresh_regions`]).
    pub fn regions(&self) -> Vec<MemoryRegion> {
        self.regions.read().clone()
    }

    /// Re-reads the memory map (new allocations, unloaded modules).
    pub fn refresh_regions(&self) -> Result<Vec<MemoryRegion>> {
        let regions = self.source.regions()?;
        self.regions.write().clone_from(&regions);
        Ok(regions)
    }

    /// Mapped spans and unmapped gaps covering the whole provider.
    pub fn spans(&self) -> Vec<AddressSpan> {
        address_spans(&self.regions.read(), self.len)
    }

    /// Unmapped gaps the UI folds away.
    pub fn gaps(&self) -> Vec<Range<u64>> {
        gaps(&self.regions.read(), self.len)
    }

    /// The region containing `address`, if mapped.
    pub fn region_at(&self, address: u64) -> Option<MemoryRegion> {
        let regions = self.regions.read();
        let i = regions.partition_point(|r| r.start <= address);
        (i > 0 && regions[i - 1].contains(address)).then(|| regions[i - 1].clone())
    }

    /// Reads with unreadable pages zero-filled; returns the readable ranges as
    /// absolute addresses (= provider offsets).
    pub fn read_partial(&self, offset: u64, buf: &mut [u8]) -> Vec<Range<u64>> {
        let avail = self.len.saturating_sub(offset).min(buf.len() as u64) as usize;
        buf[avail..].fill(0);
        self.source.read_partial(offset, &mut buf[..avail])
    }
}

fn check(offset: u64, len: usize, total: u64) -> io::Result<()> {
    match offset.checked_add(len as u64) {
        Some(end) if end <= total => Ok(()),
        _ => Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("read {offset:#x}+{len} past end {total:#x}"),
        )),
    }
}

impl Provider for AddressSpaceProvider {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        check(offset, buf.len(), self.len)?;
        self.source.read(offset, buf).map_err(io::Error::from)
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            readable: true,
            writable: self.source.writable(),
            resizable: false,
            sector_size: None,
            live: true,
        }
    }

    fn name(&self) -> String {
        self.source.name()
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> io::Result<()> {
        check(offset, data.len(), self.len)?;
        if !self.source.writable() {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "read-only"));
        }
        self.source.write(offset, data).map_err(io::Error::from)
    }

    fn invalidate(&self) {
        // Nothing is cached; refresh the memory map instead so folds follow
        // the target.
        let _ = self.refresh_regions();
    }
}

/// Ranges of bytes that differ between two reads of the same span, for
/// highlighting live changes (PROC-05). Bytes beyond the shorter slice count
/// as changed.
pub fn changed_bytes(old: &[u8], new: &[u8]) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    let common = old.len().min(new.len());
    for i in (0..common).filter(|&i| old[i] != new[i]) {
        match out.last_mut() {
            Some(r) if r.end == i => r.end = i + 1,
            _ => out.push(i..i + 1),
        }
    }
    let longest = old.len().max(new.len());
    if longest > common {
        match out.last_mut() {
            Some(r) if r.end == common => r.end = longest,
            _ => out.push(common..longest),
        }
    }
    out
}

impl From<Error> for hexa_core::Error {
    fn from(e: Error) -> Self {
        match e {
            Error::Cancelled => Self::Cancelled,
            other => Self::Io(other.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::is_unmapped;
    use crate::region::Protection;
    use crate::source::MemorySnapshot;

    #[test]
    fn changed_ranges() {
        assert_eq!(changed_bytes(b"abcdef", b"abXYeZ"), vec![2..4, 5..6]);
        assert_eq!(changed_bytes(b"ab", b"abcd"), vec![2..4]);
        assert_eq!(changed_bytes(b"aX", b"aYcd"), vec![1..4]);
        assert_eq!(changed_bytes(b"same", b"same").len(), 0);
    }

    #[test]
    fn snapshot_provider_folds() {
        let mut s = MemorySnapshot::new("snap", 8);
        s.add_region(
            MemoryRegion::new(0x1000, 0, Protection::RW),
            vec![7; 0x1000],
        )
        .unwrap();
        s.set_max_address(0x10000);
        let p = AddressSpaceProvider::new(Arc::new(s)).unwrap();
        assert_eq!(p.len(), 0x10000);
        assert!(p.capabilities().live);
        assert_eq!(p.gaps(), vec![0..0x1000, 0x2000..0x10000]);
        let mut b = [0u8; 4];
        p.read_at(0x1000, &mut b).unwrap();
        assert_eq!(b, [7; 4]);
        let err = p.read_at(0x2000, &mut b).unwrap_err();
        assert!(is_unmapped(&err));
        p.write_at(0x1000, &[1, 2]).unwrap();
        let mut big = [0xffu8; 0x2000];
        assert_eq!(p.read_partial(0x1000, &mut big), vec![0x1000..0x2000]);
        assert_eq!(&big[..3], &[1, 2, 7]);
        assert_eq!(big[0x1000], 0);
        assert!(p.region_at(0x1800).is_some());
        assert!(p.region_at(0x2800).is_none());
    }

    #[test]
    fn self_process_provider() {
        let p = AddressSpaceProvider::open_process(std::process::id(), Access::ReadWrite).unwrap();
        assert!(p.capabilities().live && p.capabilities().writable);
        let value = std::sync::atomic::AtomicU32::new(0x0102_0304);
        let addr = (&raw const value) as u64;
        let mut before = [0u8; 4];
        p.read_at(addr, &mut before).unwrap();
        assert_eq!(u32::from_ne_bytes(before), 0x0102_0304);
        value.store(0x0102_0399, std::sync::atomic::Ordering::SeqCst);
        let mut after = [0u8; 4];
        p.read_at(addr, &mut after).unwrap();
        assert_ne!(changed_bytes(&before, &after).len(), 0);
        // Page zero is never mapped.
        assert!(is_unmapped(&p.read_at(0, &mut after).unwrap_err()));
        assert!(p.gaps().first().is_some_and(|g| g.start == 0));
        assert!(p.spans().iter().any(|s| s.mapped));
    }
}
