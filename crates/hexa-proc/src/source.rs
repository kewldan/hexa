//! Memory sources: anything with an address space that can be read (PROC-04).
//!
//! [`MemorySource`] is the abstraction every higher-level feature (provider,
//! freezer, scanner, pointer scan, dump) works with, so they run equally on a
//! live local process ([`ProcessMemory`]), a GDB remote target
//! ([`crate::gdb::GdbSource`]) and synthetic or captured memory
//! ([`MemorySnapshot`]) — the latter lets the scanner logic be tested without
//! privileges.

use std::fmt;
use std::ops::Range;

use hexa_core::Cancel;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::os;
use crate::region::{MemoryRegion, Module, RegionFilter, modules};

/// Granularity of partial reads. Smaller than or equal to every OS page size.
pub const PAGE_SIZE: u64 = 4096;

/// A readable (and maybe writable) address space.
pub trait MemorySource: Send + Sync + fmt::Debug {
    /// Display name ("PID 42 (game)", "gdb 127.0.0.1:1234").
    fn name(&self) -> String;

    /// Reads exactly `buf.len()` bytes at `address`; fails with
    /// [`Error::Unmapped`] if any byte is not readable.
    fn read(&self, address: u64, buf: &mut [u8]) -> Result<()>;

    /// Writes `data` at `address`.
    fn write(&self, address: u64, data: &[u8]) -> Result<()> {
        let _ = (address, data);
        Err(Error::unsupported("this memory source is read-only"))
    }

    fn writable(&self) -> bool {
        false
    }

    /// Mapped regions sorted by address.
    fn regions(&self) -> Result<Vec<MemoryRegion>>;

    /// Exclusive upper bound of the user address space.
    fn max_address(&self) -> u64;

    /// Pointer width of the target in bytes (4 or 8).
    fn pointer_size(&self) -> u8 {
        8
    }

    /// Reads what can be read: unreadable pages are zero-filled. Returns the
    /// absolute address ranges that hold real data, sorted and coalesced.
    fn read_partial(&self, address: u64, buf: &mut [u8]) -> Vec<Range<u64>> {
        read_partial_paged(self, address, buf)
    }
}

/// Default [`MemorySource::read_partial`]: one read, then page by page.
pub fn read_partial_paged<S: MemorySource + ?Sized>(
    source: &S,
    address: u64,
    buf: &mut [u8],
) -> Vec<Range<u64>> {
    let end = address.saturating_add(buf.len() as u64);
    if buf.is_empty() {
        return Vec::new();
    }
    if source.read(address, buf).is_ok() {
        #[allow(clippy::single_range_in_vec_init)] // one readable range
        return vec![address..end];
    }
    let mut ok: Vec<Range<u64>> = Vec::new();
    let mut pos = address;
    while pos < end {
        let next = ((pos / PAGE_SIZE) + 1).saturating_mul(PAGE_SIZE).min(end);
        let dst = &mut buf[(pos - address) as usize..(next - address) as usize];
        if source.read(pos, dst).is_ok() {
            match ok.last_mut() {
                Some(last) if last.end == pos => last.end = next,
                _ => ok.push(pos..next),
            }
        } else {
            dst.fill(0);
        }
        pos = next;
    }
    ok
}

/// Access requested when opening a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Access {
    Read,
    ReadWrite,
}

/// A live local process (PROC-04): Linux `process_vm_readv` +
/// `/proc/<pid>/mem`, macOS `mach_vm_read_overwrite` / `mach_vm_write`
/// (temporarily adding write protection when needed), Windows
/// `ReadProcessMemory` / `WriteProcessMemory`.
///
/// Reads are never cached (PROC-05).
#[derive(Debug)]
pub struct ProcessMemory {
    pid: u32,
    name: String,
    access: Access,
    pointer_size: u8,
    handle: os::Handle,
}

impl ProcessMemory {
    /// Opens `pid`. Permission failures carry a [`crate::Diagnostic`].
    pub fn open(pid: u32, access: Access) -> Result<Self> {
        let handle = os::Handle::open(pid, access == Access::ReadWrite)?;
        let exe = os::executable_path(pid);
        let name = exe
            .as_deref()
            .map(|p| p.rsplit(['/', '\\']).next().unwrap_or(p).to_string())
            .unwrap_or_default();
        let pointer_size =
            os::process_arch(pid).map_or(std::mem::size_of::<usize>() as u8, |(_, bits)| bits / 8);
        Ok(Self {
            pid,
            name,
            access,
            pointer_size,
            handle,
        })
    }

    /// Opens the calling process (always allowed).
    pub fn open_self(access: Access) -> Result<Self> {
        Self::open(std::process::id(), access)
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn access(&self) -> Access {
        self.access
    }

    /// Loaded modules (images grouped by path).
    pub fn modules(&self) -> Result<Vec<Module>> {
        Ok(modules(&self.regions()?))
    }
}

impl MemorySource for ProcessMemory {
    fn name(&self) -> String {
        if self.name.is_empty() {
            format!("PID {}", self.pid)
        } else {
            format!("PID {} ({})", self.pid, self.name)
        }
    }

    fn read(&self, address: u64, buf: &mut [u8]) -> Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        self.handle.read(address, buf)
    }

    fn write(&self, address: u64, data: &[u8]) -> Result<()> {
        if self.access != Access::ReadWrite {
            return Err(Error::unsupported("process was opened read-only"));
        }
        if data.is_empty() {
            return Ok(());
        }
        self.handle.write(address, data)
    }

    fn writable(&self) -> bool {
        self.access == Access::ReadWrite
    }

    fn regions(&self) -> Result<Vec<MemoryRegion>> {
        self.handle.regions()
    }

    fn max_address(&self) -> u64 {
        self.handle.max_address(self.pointer_size)
    }

    fn pointer_size(&self) -> u8 {
        self.pointer_size
    }
}

/// Memory held in RAM: synthetic memory for tests, or a captured copy of a
/// live source (offline scanning, comparing snapshots).
#[derive(Debug)]
pub struct MemorySnapshot {
    name: String,
    pointer_size: u8,
    max_address: u64,
    /// Sorted by start; each buffer is exactly `region.size` long.
    regions: Vec<(MemoryRegion, RwLock<Vec<u8>>)>,
}

impl MemorySnapshot {
    pub fn new(name: impl Into<String>, pointer_size: u8) -> Self {
        Self {
            name: name.into(),
            pointer_size,
            max_address: 1 << 47,
            regions: Vec::new(),
        }
    }

    /// Adds a region; its size is taken from `bytes`. Overlaps are rejected.
    pub fn add_region(&mut self, mut region: MemoryRegion, bytes: Vec<u8>) -> Result<()> {
        region.size = bytes.len() as u64;
        let at = self
            .regions
            .partition_point(|(r, _)| r.start < region.start);
        let overlaps_prev = at > 0 && self.regions[at - 1].0.end() > region.start;
        let overlaps_next = self
            .regions
            .get(at)
            .is_some_and(|(r, _)| r.start < region.end());
        if overlaps_prev || overlaps_next {
            return Err(Error::invalid("snapshot regions overlap"));
        }
        self.max_address = self.max_address.max(region.end());
        self.regions.insert(at, (region, RwLock::new(bytes)));
        Ok(())
    }

    pub fn set_max_address(&mut self, max: u64) {
        self.max_address = max;
    }

    /// Copies the regions of `source` selected by `filter`. Unreadable pages
    /// become zeros.
    pub fn capture(
        source: &dyn MemorySource,
        filter: &RegionFilter,
        cancel: &Cancel,
    ) -> Result<Self> {
        let mut snap = Self::new(
            format!("{} (snapshot)", source.name()),
            source.pointer_size(),
        );
        snap.max_address = source.max_address();
        for region in source.regions()? {
            if !filter.matches(&region) {
                continue;
            }
            cancel.check()?;
            let mut bytes = vec![0u8; region.size as usize];
            source.read_partial(region.start, &mut bytes);
            snap.add_region(region, bytes)?;
        }
        Ok(snap)
    }

    fn find(&self, address: u64) -> Option<usize> {
        let i = self.regions.partition_point(|(r, _)| r.start <= address);
        (i > 0 && self.regions[i - 1].0.contains(address)).then(|| i - 1)
    }
}

impl MemorySource for MemorySnapshot {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn read(&self, address: u64, buf: &mut [u8]) -> Result<()> {
        let mut done = 0usize;
        while done < buf.len() {
            let pos = address + done as u64;
            let i = self.find(pos).ok_or(Error::Unmapped { address: pos })?;
            let (region, data) = &self.regions[i];
            let data = data.read();
            let off = (pos - region.start) as usize;
            let n = (data.len() - off).min(buf.len() - done);
            buf[done..done + n].copy_from_slice(&data[off..off + n]);
            done += n;
        }
        Ok(())
    }

    fn write(&self, address: u64, src: &[u8]) -> Result<()> {
        let mut done = 0usize;
        while done < src.len() {
            let pos = address + done as u64;
            let i = self.find(pos).ok_or(Error::Unmapped { address: pos })?;
            let (region, data) = &self.regions[i];
            let mut data = data.write();
            let off = (pos - region.start) as usize;
            let n = (data.len() - off).min(src.len() - done);
            data[off..off + n].copy_from_slice(&src[done..done + n]);
            done += n;
        }
        Ok(())
    }

    fn writable(&self) -> bool {
        true
    }

    fn regions(&self) -> Result<Vec<MemoryRegion>> {
        Ok(self.regions.iter().map(|(r, _)| r.clone()).collect())
    }

    fn max_address(&self) -> u64 {
        self.max_address
    }

    fn pointer_size(&self) -> u8 {
        self.pointer_size
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::region::Protection;

    #[test]
    fn snapshot_read_write_partial() {
        let mut s = MemorySnapshot::new("t", 8);
        s.add_region(
            MemoryRegion::new(0x1000, 0, Protection::RW),
            vec![1; 0x1000],
        )
        .unwrap();
        s.add_region(
            MemoryRegion::new(0x2000, 0, Protection::RW),
            vec![2; 0x1000],
        )
        .unwrap();
        s.add_region(
            MemoryRegion::new(0x4000, 0, Protection::RW),
            vec![4; 0x1000],
        )
        .unwrap();
        assert!(
            s.add_region(MemoryRegion::new(0x4800, 0, Protection::RW), vec![0; 16])
                .is_err()
        );
        let mut buf = [0u8; 4];
        s.read(0x1ffe, &mut buf).unwrap();
        assert_eq!(buf, [1, 1, 2, 2]);
        assert!(matches!(
            s.read(0x2ffe, &mut buf),
            Err(Error::Unmapped { address: 0x3000 })
        ));
        s.write(0x2000, &[9, 9]).unwrap();
        let mut big = vec![0xffu8; 0x3000];
        let ok = s.read_partial(0x2000, &mut big);
        assert_eq!(ok, vec![0x2000..0x3000, 0x4000..0x5000]);
        assert_eq!(&big[..3], &[9, 9, 2]);
        assert!(big[0x1000..0x2000].iter().all(|&b| b == 0));
    }

    #[test]
    fn self_process_read_write() {
        let mem = ProcessMemory::open_self(Access::ReadWrite).unwrap();
        let data: Vec<u8> = (0..=255u8).cycle().take(10_000).collect();
        let addr = data.as_ptr() as u64;
        let mut buf = vec![0u8; data.len()];
        mem.read(addr, &mut buf).unwrap();
        assert_eq!(buf, data);

        let target = std::sync::atomic::AtomicU64::new(7);
        let taddr = (&raw const target) as u64;
        mem.write(taddr, &0x1122_3344_5566_7788u64.to_ne_bytes())
            .unwrap();
        assert_eq!(
            target.load(std::sync::atomic::Ordering::SeqCst),
            0x1122_3344_5566_7788
        );
        assert!(mem.read(0, &mut [0u8; 8]).unwrap_err().is_unmapped());
        assert_eq!(mem.pointer_size() as usize, std::mem::size_of::<usize>());
        assert!(mem.name().contains(&std::process::id().to_string()));
    }

    #[test]
    fn self_regions_cover_stack_heap_and_image() {
        let mem = ProcessMemory::open_self(Access::Read).unwrap();
        let regions = mem.regions().unwrap();
        assert_ne!(regions.len(), 0);
        assert!(regions.windows(2).all(|w| w[0].end() <= w[1].start));
        let local = 5u32;
        let heap = Box::new(5u32);
        for addr in [(&raw const local) as u64, (&raw const *heap) as u64] {
            let r = regions.iter().find(|r| r.contains(addr)).expect("mapped");
            assert!(r.protection.read && r.protection.write, "{r:?}");
        }
        let code = self_process_read_write as fn() as usize as u64;
        let r = regions
            .iter()
            .find(|r| r.contains(code))
            .expect("code mapped");
        assert!(r.protection.execute, "{r:?}");
        let mods = mem.modules().unwrap();
        assert!(mods.iter().any(|m| m.contains(code)), "{mods:?}");
        assert!(mem.max_address() >= regions.last().unwrap().start);
    }

    #[test]
    fn child_process_access_or_diagnostic() {
        let child = spawn_sleeper();
        let Some(mut child) = child else {
            return;
        };
        let pid = child.id();
        match ProcessMemory::open(pid, Access::Read) {
            Ok(mem) => {
                let regions = mem.regions().unwrap();
                assert_ne!(regions.len(), 0);
            }
            Err(Error::PermissionDenied { diagnostic, .. }) => {
                assert_ne!(diagnostic.summary.len(), 0);
                assert!(!diagnostic.steps.is_empty() || diagnostic.protected);
            }
            Err(e) => panic!("unexpected error {e}"),
        }
        let _ = child.kill();
        let _ = child.wait();
    }

    fn spawn_sleeper() -> Option<std::process::Child> {
        #[cfg(unix)]
        let cmd = std::process::Command::new("sleep").arg("30").spawn();
        #[cfg(windows)]
        let cmd = std::process::Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn();
        #[cfg(not(any(unix, windows)))]
        let cmd: std::io::Result<std::process::Child> = Err(std::io::Error::other("unsupported"));
        cmd.ok()
    }
}
