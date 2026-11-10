//! QEMU QCOW2 v2/v3 (DISK-08): two-level cluster mapping, deflate-compressed
//! clusters, v3 zero clusters and an optional backing image.

use std::io;
use std::sync::Arc;

use hexa_core::Result;
use hexa_core::provider::{Capabilities, Provider};

use super::{BlockCache, for_blocks, inflate};
use crate::util::{be32, be64, check_range, invalid, read_upto, read_vec};

const OFFSET_MASK: u64 = 0x00FF_FFFF_FFFF_FE00;
const COMPRESSED: u64 = 1 << 62;
const ZERO_FLAG: u64 = 1;

/// Name of the backing file stored in a QCOW2 header, if any.
pub fn backing_file_name(p: &dyn Provider) -> Result<Option<String>> {
    let h = read_vec(p, 0, 32)?;
    if &h[0..4] != b"QFI\xfb" {
        return Err(invalid("not a QCOW2 image"));
    }
    let off = be64(&h, 8);
    let len = be32(&h, 16) as usize;
    if off == 0 || len == 0 || len > 1023 {
        return Ok(None);
    }
    let raw = read_vec(p, off, len)?;
    Ok(Some(String::from_utf8_lossy(&raw).into_owned()))
}

/// A QCOW2 virtual disk.
#[derive(Debug)]
pub struct Qcow2Provider {
    p: Arc<dyn Provider>,
    version: u32,
    cluster_bits: u32,
    size: u64,
    l1: Vec<u64>,
    backing: Option<Arc<dyn Provider>>,
    l2_cache: BlockCache,
    clusters: BlockCache,
}

impl Qcow2Provider {
    /// Opens an image; unallocated clusters read from `backing` (or zeros).
    pub fn open(p: Arc<dyn Provider>, backing: Option<Arc<dyn Provider>>) -> Result<Self> {
        let h = read_vec(p.as_ref(), 0, 112.min(p.len() as usize))?;
        if h.len() < 72 || &h[0..4] != b"QFI\xfb" {
            return Err(invalid("not a QCOW2 image"));
        }
        let version = be32(&h, 4);
        if !matches!(version, 2 | 3) {
            return Err(invalid(format!("unsupported QCOW2 version {version}")));
        }
        let cluster_bits = be32(&h, 20);
        if !(9..=21).contains(&cluster_bits) {
            return Err(invalid("bad QCOW2 cluster size"));
        }
        if be32(&h, 32) != 0 {
            return Err(invalid("encrypted QCOW2 images are not supported"));
        }
        if version == 3 {
            let incompatible = be64(&h, 72);
            // Bits: 0 dirty, 1 corrupt (both fine to read), 2 external data
            // file, 3 compression type, 4 extended L2.
            if incompatible & 0b10100 != 0 {
                return Err(invalid(
                    "QCOW2 feature not supported (external data / extended L2)",
                ));
            }
            let header_len = be32(&h, 100);
            if incompatible & 0b1000 != 0
                && header_len > 104
                && h.get(104).copied().unwrap_or(0) != 0
            {
                return Err(invalid("only zlib-compressed QCOW2 images are supported"));
            }
        }
        let l1_size = be32(&h, 36) as usize;
        if l1_size > 1 << 24 {
            return Err(invalid("implausible QCOW2 L1 table"));
        }
        let raw = read_vec(p.as_ref(), be64(&h, 40), l1_size * 8)?;
        let l1 = raw
            .as_chunks::<8>()
            .0
            .iter()
            .map(|c| u64::from_be_bytes(*c))
            .collect();
        Ok(Self {
            version,
            cluster_bits,
            size: be64(&h, 24),
            l1,
            backing,
            l2_cache: BlockCache::new(64),
            clusters: BlockCache::new(32),
            p,
        })
    }

    pub fn version(&self) -> u32 {
        self.version
    }

    fn cluster_size(&self) -> u64 {
        1 << self.cluster_bits
    }

    fn l2_entry(&self, cluster: u64) -> io::Result<u64> {
        let per = self.cluster_size() / 8;
        let l1 = (cluster / per) as usize;
        let Some(&l1e) = self.l1.get(l1) else {
            return Ok(0);
        };
        let l2_off = l1e & OFFSET_MASK;
        if l2_off == 0 {
            return Ok(0);
        }
        let table = self.l2_cache.get_or_load(l2_off, || {
            read_vec(self.p.as_ref(), l2_off, self.cluster_size() as usize)
        })?;
        let i = ((cluster % per) * 8) as usize;
        Ok(u64::from_be_bytes(
            table[i..i + 8].try_into().expect("8 bytes"),
        ))
    }
}

impl Provider for Qcow2Provider {
    fn len(&self) -> u64 {
        self.size
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        check_range(offset, buf.len(), self.size)?;
        let cs = self.cluster_size();
        for_blocks(offset, buf, cs, |cluster, within, dst| {
            let e = self.l2_entry(cluster)?;
            if e & COMPRESSED != 0 {
                let x = 62 - (self.cluster_bits - 8);
                let host = e & ((1 << x) - 1);
                let sectors = ((e >> x) & ((1 << (self.cluster_bits - 8)) - 1)) + 1;
                let data = self.clusters.get_or_load(cluster, || {
                    let len = (sectors * 512 - (host & 511)) as usize;
                    let comp = read_upto(self.p.as_ref(), host, len)?;
                    inflate(&comp, cs as usize, false)
                })?;
                dst.copy_from_slice(&data[within..within + dst.len()]);
                return Ok(());
            }
            let host = e & OFFSET_MASK;
            if self.version >= 3 && e & ZERO_FLAG != 0 {
                dst.fill(0);
                return Ok(());
            }
            if host != 0 {
                return self.p.read_at(host + within as u64, dst);
            }
            let pos = cluster * cs + within as u64;
            match &self.backing {
                Some(b) if pos < b.len() => {
                    let avail = (b.len() - pos).min(dst.len() as u64) as usize;
                    b.read_at(pos, &mut dst[..avail])?;
                    dst[avail..].fill(0);
                    Ok(())
                }
                _ => {
                    dst.fill(0);
                    Ok(())
                }
            }
        })
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::READ_ONLY
    }

    fn name(&self) -> String {
        self.p.name()
    }

    fn invalidate(&self) {
        self.l2_cache.clear();
        self.clusters.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::testutil::{deflate, pattern};
    use crate::image::{ImageFormat, detect_format, open_image};
    use hexa_core::provider::MemoryProvider;

    const CS: usize = 4096;

    /// 16 clusters: 0 raw, 1 compressed, 2 zero flag, 3 unallocated, 4 raw.
    fn qcow2(backing: Option<&str>) -> Vec<u8> {
        let mut d = vec![0u8; 7 * CS];
        d[0..4].copy_from_slice(b"QFI\xfb");
        d[4..8].copy_from_slice(&3u32.to_be_bytes());
        d[20..24].copy_from_slice(&12u32.to_be_bytes());
        d[24..32].copy_from_slice(&(16 * CS as u64).to_be_bytes());
        d[36..40].copy_from_slice(&1u32.to_be_bytes());
        d[40..48].copy_from_slice(&(CS as u64).to_be_bytes());
        d[96..100].copy_from_slice(&4u32.to_be_bytes());
        d[100..104].copy_from_slice(&112u32.to_be_bytes());
        if let Some(name) = backing {
            d[8..16].copy_from_slice(&512u64.to_be_bytes());
            d[16..20].copy_from_slice(&(name.len() as u32).to_be_bytes());
            d[512..512 + name.len()].copy_from_slice(name.as_bytes());
        }
        d[CS..CS + 8].copy_from_slice(&((2 * CS as u64) | (1 << 63)).to_be_bytes());
        let l2 = 2 * CS;
        let put = |d: &mut Vec<u8>, i: usize, v: u64| {
            d[l2 + i * 8..l2 + i * 8 + 8].copy_from_slice(&v.to_be_bytes());
        };
        put(&mut d, 0, (3 * CS as u64) | (1 << 63));
        let comp = deflate(&[0x77; CS]);
        let host = 5 * CS as u64 + 100;
        d[host as usize..host as usize + comp.len()].copy_from_slice(&comp);
        let sectors = (100 + comp.len()).div_ceil(512) as u64;
        put(&mut d, 1, COMPRESSED | ((sectors - 1) << 58) | host);
        put(&mut d, 2, ZERO_FLAG);
        put(&mut d, 4, (4 * CS as u64) | (1 << 63));
        d[3 * CS..4 * CS].copy_from_slice(&pattern(CS, 1));
        d[4 * CS..5 * CS].copy_from_slice(&pattern(CS, 2));
        d
    }

    #[test]
    fn reads_raw_compressed_zero_and_holes() {
        let p: Arc<dyn Provider> = Arc::new(MemoryProvider::new("q", qcow2(None)));
        assert_eq!(detect_format(p.as_ref()).unwrap(), ImageFormat::Qcow2);
        let q = Qcow2Provider::open(p, None).unwrap();
        let mut out = vec![0u8; 16 * CS];
        q.read_at(0, &mut out).unwrap();
        assert_eq!(&out[..CS], &pattern(CS, 1)[..]);
        assert!(out[CS..2 * CS].iter().all(|&b| b == 0x77));
        assert!(out[2 * CS..4 * CS].iter().all(|&b| b == 0));
        assert_eq!(&out[4 * CS..5 * CS], &pattern(CS, 2)[..]);
    }

    #[test]
    fn backing_file_fills_unallocated_clusters() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("base.img"), vec![0xBB; 16 * CS]).unwrap();
        std::fs::write(dir.path().join("top.qcow2"), qcow2(Some("base.img"))).unwrap();
        let q = open_image(&dir.path().join("top.qcow2")).unwrap();
        let mut b = [0u8; 4];
        q.read_at(3 * CS as u64, &mut b).unwrap();
        assert_eq!(b, [0xBB; 4]);
        q.read_at(2 * CS as u64, &mut b).unwrap();
        assert_eq!(b, [0; 4]);
        q.read_at(0, &mut b).unwrap();
        assert_eq!(b, pattern(4, 1)[..]);
    }
}
