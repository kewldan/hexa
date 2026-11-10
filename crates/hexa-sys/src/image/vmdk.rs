//! `VMware` VMDK (DISK-08): hosted sparse extents (monolithic sparse and
//! stream-optimized with deflate-compressed grains) and text descriptors
//! with FLAT / SPARSE / ZERO extents.

use std::io;
use std::path::Path;
use std::sync::Arc;

use hexa_core::Result;
use hexa_core::provider::{Capabilities, FileOpenOptions, FileProvider, Provider};

use super::{BlockCache, for_blocks, inflate};
use crate::concat::{ConcatProvider, Segment};
use crate::util::{check_range, invalid, le32, le64, read_upto, read_vec};

const GD_AT_END: u64 = u64::MAX;
const FLAG_COMPRESSED: u32 = 1 << 16;

/// A hosted sparse VMDK extent.
#[derive(Debug)]
pub struct VmdkSparseProvider {
    p: Arc<dyn Provider>,
    capacity: u64,
    grain_size: u64,
    gtes_per_gt: u64,
    compressed: bool,
    /// GTE value 1 means "zero grain" (header flag bit 2).
    zero_grain_gte: bool,
    gd: Vec<u32>,
    tables: BlockCache,
    grains: BlockCache,
}

impl VmdkSparseProvider {
    pub fn open(p: Arc<dyn Provider>) -> Result<Self> {
        let mut h = read_vec(p.as_ref(), 0, 512)?;
        if &h[0..4] != b"KDMV" {
            return Err(invalid("not a sparse VMDK extent"));
        }
        if le64(&h, 56) == GD_AT_END {
            // Stream-optimized: the real header is the footer near the end.
            let footer = read_vec(p.as_ref(), p.len().saturating_sub(1024), 512)?;
            if &footer[0..4] != b"KDMV" {
                return Err(invalid("stream-optimized VMDK without footer"));
            }
            h = footer;
        }
        let flags = le32(&h, 8);
        let capacity = le64(&h, 12) * 512;
        let grain_size = le64(&h, 20) * 512;
        let gtes = u64::from(le32(&h, 44));
        let gd_offset = le64(&h, 56) * 512;
        if grain_size == 0 || gtes == 0 || grain_size > 1 << 28 {
            return Err(invalid("bad VMDK grain geometry"));
        }
        let gd_entries = (capacity / grain_size).div_ceil(gtes);
        if gd_entries > 1 << 24 {
            return Err(invalid("implausible VMDK grain directory"));
        }
        let raw = read_vec(p.as_ref(), gd_offset, (gd_entries * 4) as usize)?;
        let gd = raw
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes(*c))
            .collect();
        Ok(Self {
            p,
            capacity,
            grain_size,
            gtes_per_gt: gtes,
            compressed: flags & FLAG_COMPRESSED != 0 && u16::from_le_bytes([h[77], h[78]]) == 1,
            zero_grain_gte: flags & 4 != 0,
            gd,
            tables: BlockCache::new(64),
            grains: BlockCache::new(32),
        })
    }

    /// Sector offset of a grain (0 = unallocated).
    fn grain_entry(&self, grain: u64) -> io::Result<u32> {
        let gt = grain / self.gtes_per_gt;
        let Some(&gt_sector) = self.gd.get(gt as usize) else {
            return Ok(0);
        };
        if gt_sector == 0 {
            return Ok(0);
        }
        let table = self.tables.get_or_load(gt, || {
            read_vec(
                self.p.as_ref(),
                u64::from(gt_sector) * 512,
                (self.gtes_per_gt * 4) as usize,
            )
        })?;
        Ok(le32(&table, ((grain % self.gtes_per_gt) * 4) as usize))
    }
}

impl Provider for VmdkSparseProvider {
    fn len(&self) -> u64 {
        self.capacity
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        check_range(offset, buf.len(), self.capacity)?;
        for_blocks(offset, buf, self.grain_size, |grain, within, dst| {
            let sector = self.grain_entry(grain)?;
            if sector == 0 || (sector == 1 && self.zero_grain_gte) {
                dst.fill(0);
                return Ok(());
            }
            let at = u64::from(sector) * 512;
            if !self.compressed {
                return self.p.read_at(at + within as u64, dst);
            }
            let data = self.grains.get_or_load(grain, || {
                // Grain marker: u64 LBA, u32 compressed size, data.
                let marker = read_vec(self.p.as_ref(), at, 12)?;
                let size = le32(&marker, 8) as usize;
                let comp = read_upto(self.p.as_ref(), at + 12, size)?;
                inflate(&comp, self.grain_size as usize, true)
            })?;
            dst.copy_from_slice(&data[within..within + dst.len()]);
            Ok(())
        })
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::READ_ONLY
    }

    fn name(&self) -> String {
        self.p.name()
    }

    fn invalidate(&self) {
        self.tables.clear();
        self.grains.clear();
    }
}

/// One extent line of a VMDK descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DescriptorExtent {
    pub access: String,
    /// Size in 512-byte sectors.
    pub sectors: u64,
    /// `FLAT`, `SPARSE`, `ZERO`, `VMFS` …
    pub kind: String,
    pub file: Option<String>,
    /// Start sector inside `file` (FLAT/VMFS).
    pub offset: u64,
}

/// Parses the extent lines of a VMDK text descriptor.
pub fn parse_descriptor(text: &str) -> Vec<DescriptorExtent> {
    let mut out = Vec::new();
    for line in text.lines().map(str::trim) {
        let mut parts = line.splitn(4, ' ');
        let (Some(access), Some(sectors), Some(kind)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if !matches!(access, "RW" | "RDONLY" | "NOACCESS") {
            continue;
        }
        let Ok(sectors) = sectors.parse::<u64>() else {
            continue;
        };
        let rest = parts.next().unwrap_or("").trim();
        let (file, tail) = match rest.strip_prefix('"').and_then(|r| r.split_once('"')) {
            Some((f, t)) => (Some(f.to_string()), t.trim()),
            None => (None, rest),
        };
        out.push(DescriptorExtent {
            access: access.to_string(),
            sectors,
            kind: kind.to_string(),
            file,
            offset: tail
                .split_whitespace()
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
        });
    }
    out
}

/// Opens a VMDK text descriptor and the extent files it names.
pub fn open_vmdk_descriptor(path: &Path) -> Result<ConcatProvider> {
    let raw = std::fs::read(path)?;
    if raw.len() > 1 << 20 {
        return Err(invalid("VMDK descriptor is too large"));
    }
    let text = String::from_utf8_lossy(&raw);
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut segments = Vec::new();
    for e in parse_descriptor(&text) {
        let len = e.sectors * 512;
        let open = |name: &Option<String>| -> Result<Arc<dyn Provider>> {
            let name = name
                .as_ref()
                .ok_or_else(|| invalid("extent without a file name"))?;
            Ok(Arc::new(FileProvider::open(
                &dir.join(name),
                FileOpenOptions::default(),
            )?))
        };
        match e.kind.as_str() {
            "FLAT" | "VMFS" => segments.push(Segment::range(open(&e.file)?, e.offset * 512, len)),
            "SPARSE" => {
                let sparse = Arc::new(VmdkSparseProvider::open(open(&e.file)?)?);
                segments.push(Segment::range(sparse, 0, len));
            }
            "ZERO" => segments.push(Segment::zeros(len)),
            other => return Err(invalid(format!("unsupported VMDK extent type {other}"))),
        }
    }
    if segments.is_empty() {
        return Err(invalid("VMDK descriptor lists no extents"));
    }
    Ok(ConcatProvider::new(
        crate::util::display_name(path),
        segments,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::open_image;
    use crate::image::testutil::{pattern, zlib};
    use hexa_core::provider::MemoryProvider;

    fn header(capacity_sectors: u64, gd_sector: u64, flags: u32, compress: u16) -> Vec<u8> {
        let mut h = vec![0u8; 512];
        h[0..4].copy_from_slice(b"KDMV");
        h[4..8].copy_from_slice(&3u32.to_le_bytes());
        h[8..12].copy_from_slice(&flags.to_le_bytes());
        h[12..20].copy_from_slice(&capacity_sectors.to_le_bytes());
        h[20..28].copy_from_slice(&8u64.to_le_bytes());
        h[44..48].copy_from_slice(&512u32.to_le_bytes());
        h[56..64].copy_from_slice(&gd_sector.to_le_bytes());
        h[77..79].copy_from_slice(&compress.to_le_bytes());
        h
    }

    /// 8 grains of 4 KiB; grains 0 and 3 allocated at sectors 8 and 16.
    fn sparse_image() -> Vec<u8> {
        let mut d = header(64, 1, 3, 0);
        d.resize(24 * 512, 0);
        d[512..516].copy_from_slice(&2u32.to_le_bytes());
        d[1024..1028].copy_from_slice(&8u32.to_le_bytes());
        d[1024 + 12..1024 + 16].copy_from_slice(&16u32.to_le_bytes());
        d[8 * 512..16 * 512].copy_from_slice(&pattern(4096, 1));
        d[16 * 512..24 * 512].copy_from_slice(&pattern(4096, 2));
        d
    }

    #[test]
    fn monolithic_sparse() {
        let v =
            VmdkSparseProvider::open(Arc::new(MemoryProvider::new("s", sparse_image()))).unwrap();
        assert_eq!(v.len(), 32 * 1024);
        let mut out = vec![0u8; 32 * 1024];
        v.read_at(0, &mut out).unwrap();
        assert_eq!(&out[..4096], &pattern(4096, 1)[..]);
        assert!(out[4096..3 * 4096].iter().all(|&b| b == 0));
        assert_eq!(&out[3 * 4096..4 * 4096], &pattern(4096, 2)[..]);
    }

    #[test]
    fn stream_optimized_with_footer() {
        let mut d = header(64, GD_AT_END, FLAG_COMPRESSED | (1 << 17) | 3, 1);
        d.resize(512, 0);
        let put_grain = |d: &mut Vec<u8>, lba: u64, data: &[u8]| -> u32 {
            let sector = (d.len() / 512) as u32;
            let c = zlib(data);
            d.extend(lba.to_le_bytes());
            d.extend((c.len() as u32).to_le_bytes());
            d.extend(c);
            d.resize(d.len().div_ceil(512) * 512, 0);
            sector
        };
        let g1 = put_grain(&mut d, 8, &pattern(4096, 5));
        let g6 = put_grain(&mut d, 48, &[0x42; 4096]);
        let gt = (d.len() / 512) as u32;
        let mut table = vec![0u8; 2048];
        table[4..8].copy_from_slice(&g1.to_le_bytes());
        table[24..28].copy_from_slice(&g6.to_le_bytes());
        d.extend(table);
        let gd = (d.len() / 512) as u64;
        let mut dir = vec![0u8; 512];
        dir[0..4].copy_from_slice(&gt.to_le_bytes());
        d.extend(dir);
        d.extend(vec![0u8; 512]);
        d.extend(header(64, gd, FLAG_COMPRESSED | (1 << 17) | 3, 1));
        d.extend(vec![0u8; 512]);
        let v = VmdkSparseProvider::open(Arc::new(MemoryProvider::new("so", d))).unwrap();
        let mut out = vec![0u8; 32 * 1024];
        v.read_at(0, &mut out).unwrap();
        assert!(out[..4096].iter().all(|&b| b == 0));
        assert_eq!(&out[4096..8192], &pattern(4096, 5)[..]);
        assert!(out[6 * 4096..7 * 4096].iter().all(|&b| b == 0x42));
    }

    #[test]
    fn descriptor_with_flat_sparse_and_zero_extents() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a-flat.vmdk"), pattern(2048, 7)).unwrap();
        std::fs::write(dir.path().join("b-s001.vmdk"), sparse_image()).unwrap();
        let desc = "# Disk DescriptorFile\nversion=1\ncreateType=\"custom\"\n\n# Extent description\n\
            RW 2 FLAT \"a-flat.vmdk\" 1\nRW 4 ZERO\nRW 64 SPARSE \"b-s001.vmdk\"\n\nddb.adapterType = \"lsilogic\"\n";
        let path = dir.path().join("disk.vmdk");
        std::fs::write(&path, desc).unwrap();
        let ext = parse_descriptor(desc);
        assert_eq!(ext.len(), 3);
        assert_eq!(ext[0].offset, 1);
        let p = open_image(&path).unwrap();
        assert_eq!(p.len(), (2 + 4 + 64) * 512);
        let mut out = vec![0u8; p.len() as usize];
        p.read_at(0, &mut out).unwrap();
        assert_eq!(&out[..1024], &pattern(2048, 7)[512..1536]);
        assert!(out[1024..3072].iter().all(|&b| b == 0));
        assert_eq!(&out[3072..3072 + 4096], &pattern(4096, 1)[..]);
    }
}
