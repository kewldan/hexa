//! Microsoft VHD images, fixed and dynamic (DISK-08).

use std::io;
use std::sync::Arc;

use hexa_core::Result;
use hexa_core::provider::{Capabilities, Provider};

use super::for_blocks;
use crate::util::{be32, be64, check_range, invalid, read_vec};

const TYPE_FIXED: u32 = 2;
const TYPE_DYNAMIC: u32 = 3;
const TYPE_DIFFERENCING: u32 = 4;

/// A VHD virtual disk.
#[derive(Debug)]
pub struct VhdProvider {
    p: Arc<dyn Provider>,
    size: u64,
    dynamic: Option<Dynamic>,
}

#[derive(Debug)]
struct Dynamic {
    block_size: u64,
    bitmap_size: u64,
    bat: Vec<u32>,
}

impl VhdProvider {
    pub fn open(p: Arc<dyn Provider>) -> Result<Self> {
        if p.len() < 512 {
            return Err(invalid("too small for a VHD"));
        }
        let footer = read_vec(p.as_ref(), p.len() - 512, 512)?;
        let footer = if footer.starts_with(b"conectix") {
            footer
        } else {
            // Dynamic disks keep a copy of the footer at offset 0.
            let head = read_vec(p.as_ref(), 0, 512)?;
            if !head.starts_with(b"conectix") {
                return Err(invalid("no VHD footer"));
            }
            head
        };
        let size = be64(&footer, 48);
        let dynamic = match be32(&footer, 60) {
            TYPE_FIXED => {
                if size > p.len() {
                    return Err(invalid("fixed VHD is truncated"));
                }
                None
            }
            TYPE_DYNAMIC => {
                let h = read_vec(p.as_ref(), be64(&footer, 16), 1024)?;
                if !h.starts_with(b"cxsparse") {
                    return Err(invalid("bad VHD dynamic header"));
                }
                let entries = be32(&h, 28) as usize;
                let block_size = u64::from(be32(&h, 32));
                if block_size == 0 || block_size % 512 != 0 || entries > 1 << 24 {
                    return Err(invalid("bad VHD block geometry"));
                }
                let raw = read_vec(p.as_ref(), be64(&h, 16), entries * 4)?;
                let bat = raw
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|c| u32::from_be_bytes(*c))
                    .collect();
                Some(Dynamic {
                    block_size,
                    bitmap_size: (block_size / 512).div_ceil(8).div_ceil(512) * 512,
                    bat,
                })
            }
            TYPE_DIFFERENCING => {
                return Err(invalid(
                    "differencing VHDs need their parent and are not supported",
                ));
            }
            t => return Err(invalid(format!("unknown VHD disk type {t}"))),
        };
        Ok(Self { p, size, dynamic })
    }
}

impl Provider for VhdProvider {
    fn len(&self) -> u64 {
        self.size
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        check_range(offset, buf.len(), self.size)?;
        let Some(d) = &self.dynamic else {
            return self.p.read_at(offset, buf);
        };
        for_blocks(offset, buf, d.block_size, |block, within, dst| {
            match d.bat.get(block as usize) {
                Some(&sector) if sector != u32::MAX => {
                    let at = u64::from(sector) * 512 + d.bitmap_size + within as u64;
                    self.p.read_at(at, dst)
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::testutil::pattern;
    use crate::image::{ImageFormat, detect_format};
    use hexa_core::provider::MemoryProvider;

    fn footer(size: u64, kind: u32, data_offset: u64) -> Vec<u8> {
        let mut f = vec![0u8; 512];
        f[0..8].copy_from_slice(b"conectix");
        f[16..24].copy_from_slice(&data_offset.to_be_bytes());
        f[40..48].copy_from_slice(&size.to_be_bytes());
        f[48..56].copy_from_slice(&size.to_be_bytes());
        f[60..64].copy_from_slice(&kind.to_be_bytes());
        f
    }

    #[test]
    fn fixed_vhd() {
        let data = pattern(8192, 3);
        let mut img = data.clone();
        img.extend(footer(8192, TYPE_FIXED, u64::MAX));
        let p: Arc<dyn Provider> = Arc::new(MemoryProvider::new("f.vhd", img));
        assert_eq!(detect_format(p.as_ref()).unwrap(), ImageFormat::Vhd);
        let v = VhdProvider::open(p).unwrap();
        let mut out = vec![0u8; 8192];
        v.read_at(0, &mut out).unwrap();
        assert_eq!(out, data);
    }

    #[test]
    fn dynamic_vhd() {
        // 4 blocks of 4 KiB; blocks 0 and 2 allocated.
        let bs = 4096usize;
        let mut img = footer(4 * bs as u64, TYPE_DYNAMIC, 512);
        let mut hdr = vec![0u8; 1024];
        hdr[0..8].copy_from_slice(b"cxsparse");
        hdr[16..24].copy_from_slice(&1536u64.to_be_bytes());
        hdr[28..32].copy_from_slice(&4u32.to_be_bytes());
        hdr[32..36].copy_from_slice(&(bs as u32).to_be_bytes());
        img.extend(hdr);
        let mut bat = vec![0xFFu8; 512];
        // Block 0 at sector 4 (bitmap 512 + data), block 2 at sector 13.
        bat[0..4].copy_from_slice(&4u32.to_be_bytes());
        bat[8..12].copy_from_slice(&13u32.to_be_bytes());
        img.extend(bat);
        img.resize(4 * 512, 0);
        img.extend(vec![0xFF; 512]);
        img.extend(pattern(bs, 1));
        img.extend(vec![0xFF; 512]);
        img.extend(pattern(bs, 2));
        img.extend(footer(4 * bs as u64, TYPE_DYNAMIC, 512));
        let v = VhdProvider::open(Arc::new(MemoryProvider::new("d.vhd", img))).unwrap();
        assert_eq!(v.len(), 4 * bs as u64);
        let mut out = vec![0u8; 4 * bs];
        v.read_at(0, &mut out).unwrap();
        assert_eq!(&out[..bs], &pattern(bs, 1)[..]);
        assert!(out[bs..2 * bs].iter().all(|&b| b == 0));
        assert_eq!(&out[2 * bs..3 * bs], &pattern(bs, 2)[..]);
        let mut piece = [0u8; 10];
        v.read_at(2 * bs as u64 - 5, &mut piece).unwrap();
        assert_eq!(&piece[5..], &pattern(bs, 2)[..5]);
    }
}
