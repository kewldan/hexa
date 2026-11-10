//! Dumping memory to files (PROC-08).
//!
//! Unreadable pages inside the dumped range are written as zeros and listed
//! in the [`DumpReport`], so file offsets always equal `address - start`.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::ops::Range;
use std::path::Path;

use hexa_core::Cancel;
use hexa_core::task::Progress;
use serde::Serialize;

use crate::error::{Error, Result};
use crate::region::{MemoryRegion, find_module, modules};
use crate::source::MemorySource;

const CHUNK: u64 = 1 << 20;

/// What a dump wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DumpReport {
    /// First dumped address (file offset 0).
    pub start: u64,
    pub bytes_written: u64,
    /// Address ranges that could not be read and were zero-filled.
    pub unreadable: Vec<Range<u64>>,
}

/// Dumps `[start, start + len)` to `path` (PROC-08).
pub fn dump_range(
    source: &dyn MemorySource,
    start: u64,
    len: u64,
    path: &Path,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<DumpReport> {
    let end = start
        .checked_add(len)
        .ok_or_else(|| Error::invalid("range overflows the address space"))?;
    let mut out = BufWriter::new(File::create(path)?);
    let mut unreadable: Vec<Range<u64>> = Vec::new();
    let mut buf = Vec::new();
    let mut pos = start;
    progress(0, len);
    while pos < end {
        cancel.check()?;
        let next = (pos + CHUNK).min(end);
        buf.resize((next - pos) as usize, 0);
        let valid = source.read_partial(pos, &mut buf);
        let mut cursor = pos;
        for v in valid.iter().chain(std::iter::once(&(next..next))) {
            if v.start > cursor {
                match unreadable.last_mut() {
                    Some(last) if last.end == cursor => last.end = v.start,
                    _ => unreadable.push(cursor..v.start),
                }
            }
            cursor = cursor.max(v.end);
        }
        out.write_all(&buf)?;
        pos = next;
        progress(pos - start, len);
    }
    out.flush()?;
    Ok(DumpReport {
        start,
        bytes_written: len,
        unreadable,
    })
}

/// Dumps one region (PROC-08).
pub fn dump_region(
    source: &dyn MemorySource,
    region: &MemoryRegion,
    path: &Path,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<DumpReport> {
    dump_range(source, region.start, region.size, path, cancel, progress)
}

/// Dumps a module (by file name or path) from its base to the end of its last
/// region; gaps between its sections are zero-filled (PROC-08).
pub fn dump_module(
    source: &dyn MemorySource,
    module: &str,
    path: &Path,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<DumpReport> {
    let mods = modules(&source.regions()?);
    let m = find_module(&mods, module)
        .ok_or_else(|| Error::invalid(format!("module {module} is not loaded")))?;
    dump_range(source, m.base, m.size, path, cancel, progress)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::region::{Protection, RegionKind};
    use crate::source::MemorySnapshot;
    use hexa_core::task::no_progress;

    #[test]
    fn dumps_module_with_gap() {
        let mut s = MemorySnapshot::new("s", 8);
        let img = |start| MemoryRegion {
            kind: RegionKind::Image,
            path: Some("C:\\app\\lib.dll".into()),
            ..MemoryRegion::new(start, 0, Protection::R)
        };
        s.add_region(img(0x1000), vec![0xAA; 0x1000]).unwrap();
        s.add_region(img(0x3000), vec![0xBB; 0x800]).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lib.bin");
        let r = dump_module(&s, "LIB.DLL", &path, &Cancel::new(), &no_progress).unwrap();
        assert_eq!(r.start, 0x1000);
        assert_eq!(r.bytes_written, 0x2800);
        assert_eq!(r.unreadable, vec![0x2000..0x3000]);
        let data = std::fs::read(&path).unwrap();
        assert_eq!(data.len(), 0x2800);
        assert_eq!(data[0], 0xAA);
        assert_eq!(data[0x1000], 0);
        assert_eq!(data[0x2000], 0xBB);

        let region = s.regions().unwrap()[1].clone();
        let r = dump_region(&s, &region, &path, &Cancel::new(), &no_progress).unwrap();
        assert_eq!(r.unreadable.len(), 0);
        assert_eq!(std::fs::read(&path).unwrap(), vec![0xBB; 0x800]);
        assert!(dump_module(&s, "missing", &path, &Cancel::new(), &no_progress).is_err());
    }
}
