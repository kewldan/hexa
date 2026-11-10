//! Microsoft VHDX images (DISK-08): headers and region tables with CRC-32C,
//! metadata (block size, virtual size, sector size) and the BAT.

use std::io;
use std::sync::Arc;

use hexa_core::Result;
use hexa_core::provider::{Capabilities, Provider};

use super::for_blocks;
use crate::util::{check_range, crc32c, guid_bytes, invalid, le16, le32, le64, read_vec};

const BAT_GUID: &str = "2DC27766-F623-4200-9D64-115E9BFD4A08";
const METADATA_GUID: &str = "8B7CA206-4790-4B9A-B8FE-575F050F886E";
const FILE_PARAMETERS: &str = "CAA16737-FA36-4D43-B3B6-33F0AA44E76B";
const VIRTUAL_DISK_SIZE: &str = "2FA54224-CD1B-4876-B211-5DBED83BF4B8";
const LOGICAL_SECTOR_SIZE: &str = "8141BF1D-A96F-4709-BA47-F233A8FAAB5F";
const KIB64: u64 = 64 * 1024;

const PAYLOAD_FULLY_PRESENT: u64 = 6;
const PAYLOAD_PARTIALLY_PRESENT: u64 = 7;

/// A VHDX virtual disk.
#[derive(Debug)]
pub struct VhdxProvider {
    p: Arc<dyn Provider>,
    size: u64,
    block_size: u64,
    logical_sector: u32,
    chunk_ratio: u64,
    bat: Vec<u64>,
    /// The log was not empty; data may be stale (log replay unsupported).
    pub log_pending: bool,
}

fn checked(buf: &mut [u8], sig: &[u8; 4]) -> bool {
    if &buf[0..4] != sig {
        return false;
    }
    let stored = le32(buf, 4);
    buf[4..8].fill(0);
    let ok = crc32c(buf) == stored;
    buf[4..8].copy_from_slice(&stored.to_le_bytes());
    ok
}

impl VhdxProvider {
    pub fn open(p: Arc<dyn Provider>) -> Result<Self> {
        let id = read_vec(p.as_ref(), 0, 8)?;
        if &id != b"vhdxfile" {
            return Err(invalid("not a VHDX file"));
        }
        // Current header = valid one with the higher sequence number.
        let mut header: Option<Vec<u8>> = None;
        for off in [KIB64, 2 * KIB64] {
            let mut h = read_vec(p.as_ref(), off, 4096)?;
            if checked(&mut h, b"head") && header.as_ref().is_none_or(|c| le64(&h, 8) > le64(c, 8))
            {
                header = Some(h);
            }
        }
        let header = header.ok_or_else(|| invalid("no valid VHDX header"))?;
        let log_pending = header[48..64].iter().any(|&b| b != 0);

        let mut regions = None;
        for off in [3 * KIB64, 4 * KIB64] {
            let mut r = read_vec(p.as_ref(), off, KIB64 as usize)?;
            if checked(&mut r, b"regi") {
                regions = Some(r);
                break;
            }
        }
        let regions = regions.ok_or_else(|| invalid("no valid VHDX region table"))?;
        let find = |guid: &str| -> Option<(u64, u64)> {
            let g = guid_bytes(guid)?;
            (0..le32(&regions, 8).min(2047) as usize).find_map(|i| {
                let e = &regions[16 + i * 32..48 + i * 32];
                (e[0..16] == g).then(|| (le64(e, 16), u64::from(le32(e, 24))))
            })
        };
        let (bat_off, bat_len) = find(BAT_GUID).ok_or_else(|| invalid("VHDX has no BAT region"))?;
        let (meta_off, meta_len) =
            find(METADATA_GUID).ok_or_else(|| invalid("VHDX has no metadata region"))?;

        let meta = read_vec(p.as_ref(), meta_off, meta_len.min(1 << 20) as usize)?;
        if &meta[0..8] != b"metadata" {
            return Err(invalid("bad VHDX metadata table"));
        }
        let item = |guid: &str| -> Option<&[u8]> {
            let g = guid_bytes(guid)?;
            (0..usize::from(le16(&meta, 10)).min(2047)).find_map(|i| {
                let e = meta.get(32 + i * 32..64 + i * 32)?;
                if e[0..16] != g {
                    return None;
                }
                let off = le32(e, 16) as usize;
                meta.get(off..off + le32(e, 20) as usize)
            })
        };
        let params = item(FILE_PARAMETERS).ok_or_else(|| invalid("VHDX lacks file parameters"))?;
        let block_size = u64::from(le32(params, 0));
        if le32(params, 4) & 2 != 0 {
            return Err(invalid(
                "differencing VHDX images need their parent and are not supported",
            ));
        }
        let size = le64(
            item(VIRTUAL_DISK_SIZE).ok_or_else(|| invalid("VHDX lacks a size"))?,
            0,
        );
        let logical_sector = item(LOGICAL_SECTOR_SIZE).map_or(512, |s| le32(s, 0));
        if !block_size.is_power_of_two()
            || block_size < (1 << 20)
            || !matches!(logical_sector, 512 | 4096)
        {
            return Err(invalid("bad VHDX geometry"));
        }
        let chunk_ratio = ((1u64 << 23) * u64::from(logical_sector)) / block_size;
        let blocks = size.div_ceil(block_size);
        let entries = blocks + blocks.saturating_sub(1) / chunk_ratio + 1;
        let need = (entries * 8).min(bat_len);
        let raw = read_vec(p.as_ref(), bat_off, need as usize)?;
        let bat = raw
            .as_chunks::<8>()
            .0
            .iter()
            .map(|c| u64::from_le_bytes(*c))
            .collect();
        Ok(Self {
            p,
            size,
            block_size,
            logical_sector,
            chunk_ratio,
            bat,
            log_pending,
        })
    }

    pub fn logical_sector_size(&self) -> u32 {
        self.logical_sector
    }
}

impl Provider for VhdxProvider {
    fn len(&self) -> u64 {
        self.size
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        check_range(offset, buf.len(), self.size)?;
        for_blocks(offset, buf, self.block_size, |block, within, dst| {
            let idx = block + block / self.chunk_ratio;
            let entry = self.bat.get(idx as usize).copied().unwrap_or(0);
            let state = entry & 7;
            if state == PAYLOAD_FULLY_PRESENT || state == PAYLOAD_PARTIALLY_PRESENT {
                let file_off = (entry >> 20) << 20;
                self.p.read_at(file_off + within as u64, dst)
            } else {
                dst.fill(0);
                Ok(())
            }
        })
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            sector_size: Some(self.logical_sector),
            ..Capabilities::READ_ONLY
        }
    }

    fn name(&self) -> String {
        self.p.name()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::testutil::pattern;
    use hexa_core::provider::MemoryProvider;

    const MIB: usize = 1 << 20;

    fn seal(buf: &mut [u8]) {
        buf[4..8].fill(0);
        let c = crc32c(buf);
        buf[4..8].copy_from_slice(&c.to_le_bytes());
    }

    fn vhdx_image() -> Vec<u8> {
        let mut d = vec![0u8; 5 * MIB];
        d[0..8].copy_from_slice(b"vhdxfile");
        for (off, seq) in [(64 * 1024usize, 1u64), (128 * 1024, 2)] {
            let mut h = vec![0u8; 4096];
            h[0..4].copy_from_slice(b"head");
            h[8..16].copy_from_slice(&seq.to_le_bytes());
            h[66..68].copy_from_slice(&1u16.to_le_bytes());
            seal(&mut h);
            d[off..off + 4096].copy_from_slice(&h);
        }
        let mut r = vec![0u8; 64 * 1024];
        r[0..4].copy_from_slice(b"regi");
        r[8..12].copy_from_slice(&2u32.to_le_bytes());
        for (i, (g, off, len)) in [(BAT_GUID, 2 * MIB, MIB), (METADATA_GUID, MIB, MIB)]
            .iter()
            .enumerate()
        {
            let e = 16 + i * 32;
            r[e..e + 16].copy_from_slice(&guid_bytes(g).unwrap());
            r[e + 16..e + 24].copy_from_slice(&(*off as u64).to_le_bytes());
            r[e + 24..e + 28].copy_from_slice(&(*len as u32).to_le_bytes());
            r[e + 28..e + 32].copy_from_slice(&1u32.to_le_bytes());
        }
        seal(&mut r);
        d[3 * 64 * 1024..4 * 64 * 1024].copy_from_slice(&r);

        let m = MIB;
        d[m..m + 8].copy_from_slice(b"metadata");
        d[m + 10..m + 12].copy_from_slice(&3u16.to_le_bytes());
        let items: [(&str, Vec<u8>); 3] = [
            (
                FILE_PARAMETERS,
                [(MIB as u32).to_le_bytes(), 0u32.to_le_bytes()].concat(),
            ),
            (VIRTUAL_DISK_SIZE, (3 * MIB as u64).to_le_bytes().to_vec()),
            (LOGICAL_SECTOR_SIZE, 512u32.to_le_bytes().to_vec()),
        ];
        for (i, (g, v)) in items.iter().enumerate() {
            let e = m + 32 + i * 32;
            let off = 64 * 1024 + i * 64;
            d[e..e + 16].copy_from_slice(&guid_bytes(g).unwrap());
            d[e + 16..e + 20].copy_from_slice(&(off as u32).to_le_bytes());
            d[e + 20..e + 24].copy_from_slice(&(v.len() as u32).to_le_bytes());
            d[m + off..m + off + v.len()].copy_from_slice(v);
        }
        let bat = 2 * MIB;
        let entry = |mb: u64| (mb << 20) | PAYLOAD_FULLY_PRESENT;
        d[bat..bat + 8].copy_from_slice(&entry(3).to_le_bytes());
        d[bat + 16..bat + 24].copy_from_slice(&entry(4).to_le_bytes());
        d[3 * MIB..4 * MIB].copy_from_slice(&pattern(MIB, 9));
        d[4 * MIB..5 * MIB].copy_from_slice(&pattern(MIB, 4));
        d
    }

    #[test]
    fn reads_vhdx_blocks() {
        let mut img = vhdx_image();
        let v = VhdxProvider::open(Arc::new(MemoryProvider::new("x.vhdx", img.clone()))).unwrap();
        assert_eq!(v.len(), 3 * MIB as u64);
        assert!(!v.log_pending);
        let mut out = vec![0u8; 3 * MIB];
        v.read_at(0, &mut out).unwrap();
        assert_eq!(&out[..MIB], &pattern(MIB, 9)[..]);
        assert!(out[MIB..2 * MIB].iter().all(|&b| b == 0));
        assert_eq!(&out[2 * MIB..], &pattern(MIB, 4)[..]);

        // A broken region table checksum is rejected.
        img[3 * 64 * 1024 + 20] ^= 1;
        assert!(VhdxProvider::open(Arc::new(MemoryProvider::new("x", img))).is_err());
    }
}
