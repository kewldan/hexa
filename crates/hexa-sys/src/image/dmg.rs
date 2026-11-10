//! Apple UDIF disk images (`.dmg`, DISK-08): `koly` trailer, XML plist with
//! `blkx` tables, raw / zero / zlib / bzip2 / LZFSE / ADC chunks.

use std::io::{self, Read};
use std::sync::Arc;

use hexa_core::Result;
use hexa_core::provider::{Capabilities, Provider};

use super::BlockCache;
use crate::util::{be32, be64, check_range, invalid, read_vec};

const CHUNK_ZERO: u32 = 0;
const CHUNK_RAW: u32 = 1;
const CHUNK_IGNORE: u32 = 2;
const CHUNK_ADC: u32 = 0x8000_0004;
const CHUNK_ZLIB: u32 = 0x8000_0005;
const CHUNK_BZIP2: u32 = 0x8000_0006;
const CHUNK_LZFSE: u32 = 0x8000_0007;
const CHUNK_LZMA: u32 = 0x8000_0008;
const CHUNK_COMMENT: u32 = 0x7FFF_FFFE;
const CHUNK_END: u32 = 0xFFFF_FFFF;

#[derive(Debug, Clone, Copy)]
struct Chunk {
    kind: u32,
    sector: u64,
    count: u64,
    offset: u64,
    len: u64,
}

/// A UDIF disk image.
#[derive(Debug)]
pub struct DmgProvider {
    p: Arc<dyn Provider>,
    size: u64,
    chunks: Vec<Chunk>,
    cache: BlockCache,
}

impl DmgProvider {
    pub fn open(p: Arc<dyn Provider>) -> Result<Self> {
        if p.len() < 512 {
            return Err(invalid("too small for a DMG"));
        }
        let koly = read_vec(p.as_ref(), p.len() - 512, 512)?;
        if &koly[0..4] != b"koly" {
            return Err(invalid("no UDIF koly trailer"));
        }
        let data_fork = be64(&koly, 24);
        let xml_off = be64(&koly, 216);
        let xml_len = be64(&koly, 224);
        if xml_len == 0 || xml_len > 64 << 20 {
            return Err(invalid("DMG has no usable XML plist"));
        }
        let xml = read_vec(p.as_ref(), xml_off, xml_len as usize)?;
        let plist = plist::Value::from_reader_xml(&xml[..])
            .map_err(|e| invalid(format!("bad DMG plist: {e}")))?;
        let blkx = plist
            .as_dictionary()
            .and_then(|d| d.get("resource-fork"))
            .and_then(plist::Value::as_dictionary)
            .and_then(|d| d.get("blkx"))
            .and_then(plist::Value::as_array)
            .ok_or_else(|| invalid("DMG plist has no blkx table"))?;
        let mut chunks = Vec::new();
        for entry in blkx {
            let Some(mish) = entry
                .as_dictionary()
                .and_then(|d| d.get("Data"))
                .and_then(plist::Value::as_data)
            else {
                continue;
            };
            if mish.len() < 204 || &mish[0..4] != b"mish" {
                continue;
            }
            let first = be64(mish, 8);
            let data_off = be64(mish, 24);
            let count = be32(mish, 200) as usize;
            for i in 0..count {
                let c = 204 + i * 40;
                if c + 40 > mish.len() {
                    break;
                }
                let kind = be32(mish, c);
                if kind == CHUNK_END || kind == CHUNK_COMMENT {
                    continue;
                }
                chunks.push(Chunk {
                    kind,
                    sector: first + be64(mish, c + 8),
                    count: be64(mish, c + 16),
                    offset: data_fork + data_off + be64(mish, c + 24),
                    len: be64(mish, c + 32),
                });
            }
        }
        chunks.sort_by_key(|c| c.sector);
        let sectors = match be64(&koly, 492) {
            0 => chunks.iter().map(|c| c.sector + c.count).max().unwrap_or(0),
            n => n,
        };
        Ok(Self {
            p,
            size: sectors * 512,
            chunks,
            cache: BlockCache::new(16),
        })
    }

    fn decode(&self, c: &Chunk) -> io::Result<Vec<u8>> {
        let size = (c.count * 512) as usize;
        if c.len > 256 << 20 || size > 256 << 20 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "DMG chunk too large",
            ));
        }
        let comp = read_vec(self.p.as_ref(), c.offset, c.len as usize)?;
        let mut out = match c.kind {
            CHUNK_ZLIB => super::inflate(&comp, size, true)?,
            CHUNK_BZIP2 => {
                let mut v = Vec::with_capacity(size);
                bzip2::read::BzDecoder::new(&comp[..]).read_to_end(&mut v)?;
                v
            }
            CHUNK_LZFSE => {
                let mut v = Vec::with_capacity(size);
                lzfse_rust::decode_bytes(&comp, &mut v)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
                v
            }
            CHUNK_ADC => adc_decode(&comp, size)?,
            CHUNK_LZMA => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "LZMA DMG chunks are not supported",
                ));
            }
            k => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("unknown DMG chunk type {k:#x}"),
                ));
            }
        };
        out.resize(size, 0);
        Ok(out)
    }
}

/// Apple Data Compression (ADC) decoder.
pub(crate) fn adc_decode(input: &[u8], size: usize) -> io::Result<Vec<u8>> {
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "corrupt ADC data");
    let mut out: Vec<u8> = Vec::with_capacity(size);
    let mut i = 0;
    while i < input.len() && out.len() < size {
        let b = input[i];
        let (len, dist, adv) = if b & 0x80 != 0 {
            let len = usize::from(b & 0x7F) + 1;
            let lit = input.get(i + 1..i + 1 + len).ok_or_else(bad)?;
            out.extend_from_slice(lit);
            i += 1 + len;
            continue;
        } else if b & 0x40 != 0 {
            let hi = *input.get(i + 1).ok_or_else(bad)?;
            let lo = *input.get(i + 2).ok_or_else(bad)?;
            (
                usize::from(b & 0x3F) + 4,
                usize::from(u16::from_be_bytes([hi, lo])),
                3,
            )
        } else {
            let lo = *input.get(i + 1).ok_or_else(bad)?;
            (
                usize::from((b & 0x3C) >> 2) + 3,
                (usize::from(b & 0x03) << 8) | usize::from(lo),
                2,
            )
        };
        if dist >= out.len() {
            return Err(bad());
        }
        let start = out.len() - dist - 1;
        for k in 0..len {
            out.push(out[start + k]);
        }
        i += adv;
    }
    out.truncate(size);
    Ok(out)
}

impl Provider for DmgProvider {
    fn len(&self) -> u64 {
        self.size
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        check_range(offset, buf.len(), self.size)?;
        let mut done = 0usize;
        while done < buf.len() {
            let pos = offset + done as u64;
            let sector = pos / 512;
            let idx = self.chunks.partition_point(|c| c.sector <= sector);
            let chunk = idx
                .checked_sub(1)
                .map(|i| (i, self.chunks[i]))
                .filter(|(_, c)| sector < c.sector + c.count);
            let Some((ci, c)) = chunk else {
                // Gap: zeros up to the next chunk.
                let next = self.chunks.get(idx).map_or(self.size, |c| c.sector * 512);
                let n = ((next - pos) as usize).min(buf.len() - done).max(1);
                buf[done..done + n].fill(0);
                done += n;
                continue;
            };
            let start = c.sector * 512;
            let within = pos - start;
            let n = ((c.count * 512 - within) as usize).min(buf.len() - done);
            let dst = &mut buf[done..done + n];
            match c.kind {
                CHUNK_ZERO | CHUNK_IGNORE => dst.fill(0),
                CHUNK_RAW => {
                    let avail = c.len.saturating_sub(within).min(n as u64) as usize;
                    self.p.read_at(c.offset + within, &mut dst[..avail])?;
                    dst[avail..].fill(0);
                }
                _ => {
                    let data = self.cache.get_or_load(ci as u64, || self.decode(&c))?;
                    dst.copy_from_slice(&data[within as usize..within as usize + n]);
                }
            }
            done += n;
        }
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::READ_ONLY
    }

    fn name(&self) -> String {
        self.p.name()
    }

    fn invalidate(&self) {
        self.cache.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::testutil::{pattern, zlib};
    use crate::image::{ImageFormat, detect_format};
    use hexa_core::provider::MemoryProvider;
    use std::io::Write;

    fn mish(first: u64, chunks: &[(u32, u64, u64, u64, u64)]) -> Vec<u8> {
        let mut m = vec![0u8; 204];
        m[0..4].copy_from_slice(b"mish");
        m[4..8].copy_from_slice(&1u32.to_be_bytes());
        m[8..16].copy_from_slice(&first.to_be_bytes());
        m[200..204].copy_from_slice(&((chunks.len() + 1) as u32).to_be_bytes());
        for &(kind, sector, count, off, len) in
            chunks.iter().chain([(CHUNK_END, 0, 0, 0, 0)].iter())
        {
            m.extend(kind.to_be_bytes());
            m.extend(0u32.to_be_bytes());
            m.extend(sector.to_be_bytes());
            m.extend(count.to_be_bytes());
            m.extend(off.to_be_bytes());
            m.extend(len.to_be_bytes());
        }
        m
    }

    #[test]
    fn reads_udif_chunks() {
        let raw = pattern(1024, 1);
        let z = pattern(2048, 2);
        let bz_src = pattern(1024, 3);
        let lz_src = pattern(1024, 4);
        let mut data = raw.clone();
        let zc = zlib(&z);
        let mut bz = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
        bz.write_all(&bz_src).unwrap();
        let bzc = bz.finish().unwrap();
        let mut lzc = Vec::new();
        lzfse_rust::encode_bytes(&lz_src, &mut lzc).unwrap();
        // ADC: literal "abc", then copy 6 bytes from distance 3 → "abcabcabc".
        let adc = vec![0x82, b'a', b'b', b'c', 0x0C, 0x02];
        let mut chunks = vec![(CHUNK_RAW, 0, 2, 0, 1024), (CHUNK_ZERO, 2, 2, 0, 0)];
        let mut add = |data: &mut Vec<u8>, kind, sector, count, bytes: &[u8]| {
            chunks.push((kind, sector, count, data.len() as u64, bytes.len() as u64));
            data.extend_from_slice(bytes);
        };
        add(&mut data, CHUNK_ZLIB, 4, 4, &zc);
        add(&mut data, CHUNK_BZIP2, 8, 2, &bzc);
        add(&mut data, CHUNK_LZFSE, 10, 2, &lzc);
        add(&mut data, CHUNK_ADC, 12, 1, &adc);
        let mut dict = plist::Dictionary::new();
        let mut entry = plist::Dictionary::new();
        entry.insert("Name".into(), "whole disk".into());
        entry.insert("Data".into(), plist::Value::Data(mish(0, &chunks)));
        let mut rf = plist::Dictionary::new();
        rf.insert("blkx".into(), plist::Value::Array(vec![entry.into()]));
        dict.insert("resource-fork".into(), rf.into());
        let mut xml = Vec::new();
        plist::Value::Dictionary(dict)
            .to_writer_xml(&mut xml)
            .unwrap();
        let xml_off = data.len() as u64;
        data.extend(&xml);
        let mut koly = vec![0u8; 512];
        koly[0..4].copy_from_slice(b"koly");
        koly[4..8].copy_from_slice(&4u32.to_be_bytes());
        koly[8..12].copy_from_slice(&512u32.to_be_bytes());
        koly[32..40].copy_from_slice(&xml_off.to_be_bytes());
        koly[216..224].copy_from_slice(&xml_off.to_be_bytes());
        koly[224..232].copy_from_slice(&(xml.len() as u64).to_be_bytes());
        koly[492..500].copy_from_slice(&14u64.to_be_bytes());
        data.extend(koly);

        let p: Arc<dyn Provider> = Arc::new(MemoryProvider::new("i.dmg", data));
        assert_eq!(detect_format(p.as_ref()).unwrap(), ImageFormat::Dmg);
        let d = DmgProvider::open(p).unwrap();
        assert_eq!(d.len(), 14 * 512);
        let mut out = vec![0u8; 14 * 512];
        d.read_at(0, &mut out).unwrap();
        assert_eq!(&out[..1024], &raw[..]);
        assert!(out[1024..2048].iter().all(|&b| b == 0));
        assert_eq!(&out[2048..4096], &z[..]);
        assert_eq!(&out[4096..5120], &bz_src[..]);
        assert_eq!(&out[5120..6144], &lz_src[..]);
        assert_eq!(&out[6144..6153], b"abcabcabc");
        assert!(out[6153..].iter().all(|&b| b == 0));
    }
}
