//! Disk images (DISK-08): format detection and read-only providers for
//! raw/split raw, VHD (fixed, dynamic), VHDX, VMDK (monolithic sparse,
//! stream-optimized, flat/split descriptors), QCOW2 (v2/v3, compressed
//! clusters, backing files), DMG (UDIF raw/zero/zlib/bzip2/lzfse/ADC) and
//! E01/EWF, plus an E01 writer. Raw image creation lives in
//! [`crate::clone::clone_to_file`].

use std::io;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hexa_core::Result;
use hexa_core::provider::{FileOpenOptions, FileProvider, Provider};
use lru::LruCache;
use parking_lot::Mutex;
use serde::Serialize;

use crate::concat::{ConcatProvider, Segment};
use crate::util::{invalid, read_upto};

pub mod dmg;
pub mod ewf;
pub mod qcow2;
pub mod vhd;
pub mod vhdx;
pub mod vmdk;

pub use dmg::DmgProvider;
pub use ewf::{E01Options, E01Report, EwfProvider, create_e01};
pub use qcow2::Qcow2Provider;
pub use vhd::VhdProvider;
pub use vhdx::VhdxProvider;
pub use vmdk::{VmdkSparseProvider, open_vmdk_descriptor};

/// Container format of an image file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ImageFormat {
    Raw,
    Vhd,
    Vhdx,
    VmdkSparse,
    VmdkDescriptor,
    Qcow2,
    Dmg,
    Ewf,
}

/// Detects the container format from content.
pub fn detect_format(p: &dyn Provider) -> Result<ImageFormat> {
    let head = read_upto(p, 0, 512)?;
    let tail = read_upto(p, p.len().saturating_sub(512), 512)?;
    Ok(if head.starts_with(b"EVF\x09\x0d\x0a\xff\x00") {
        ImageFormat::Ewf
    } else if head.starts_with(b"QFI\xfb") {
        ImageFormat::Qcow2
    } else if head.starts_with(b"vhdxfile") {
        ImageFormat::Vhdx
    } else if head.starts_with(b"KDMV") {
        ImageFormat::VmdkSparse
    } else if head.starts_with(b"# Disk DescriptorFile") {
        ImageFormat::VmdkDescriptor
    } else if tail.starts_with(b"koly") {
        ImageFormat::Dmg
    } else if tail.starts_with(b"conectix") || head.starts_with(b"conectix") {
        ImageFormat::Vhd
    } else {
        ImageFormat::Raw
    })
}

fn open_file(path: &Path) -> Result<Arc<dyn Provider>> {
    Ok(Arc::new(FileProvider::open(
        path,
        FileOpenOptions::default(),
    )?))
}

/// Split raw images: `disk.001`, `disk.002` … → one provider.
pub fn open_split_raw(first: &Path) -> Result<ConcatProvider> {
    let ext = first
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    let width = ext.len();
    let start: u32 = ext
        .parse()
        .map_err(|_| invalid("split image must end in a number"))?;
    let mut segments = Vec::new();
    for n in start.. {
        let path = first.with_extension(format!("{n:0width$}"));
        if !path.exists() {
            break;
        }
        segments.push(Segment::whole(open_file(&path)?));
    }
    let name = crate::util::display_name(first);
    Ok(ConcatProvider::new(name, segments)?)
}

/// Opens any supported image file as a provider of the virtual disk
/// (DISK-08). Unknown content is treated as a raw image; `name.001` opens
/// the whole split set.
pub fn open_image(path: &Path) -> Result<Arc<dyn Provider>> {
    let file = open_file(path)?;
    Ok(match detect_format(file.as_ref())? {
        ImageFormat::Raw => {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or_default();
            if ext.len() == 3 && ext.chars().all(|c| c.is_ascii_digit()) {
                Arc::new(open_split_raw(path)?)
            } else {
                file
            }
        }
        ImageFormat::Vhd => Arc::new(VhdProvider::open(file)?),
        ImageFormat::Vhdx => Arc::new(VhdxProvider::open(file)?),
        ImageFormat::VmdkSparse => Arc::new(VmdkSparseProvider::open(file)?),
        ImageFormat::VmdkDescriptor => Arc::new(open_vmdk_descriptor(path)?),
        ImageFormat::Qcow2 => {
            let backing = qcow2::backing_file_name(file.as_ref())?
                .map(|name| {
                    let p = PathBuf::from(&name);
                    let p = if p.is_absolute() {
                        p
                    } else {
                        path.parent().unwrap_or(Path::new(".")).join(p)
                    };
                    open_image(&p)
                })
                .transpose()?;
            Arc::new(Qcow2Provider::open(file, backing)?)
        }
        ImageFormat::Dmg => Arc::new(DmgProvider::open(file)?),
        ImageFormat::Ewf => Arc::new(EwfProvider::open_path(path)?),
    })
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// A small LRU of decoded blocks (decompressed chunks, grain tables …).
pub(crate) struct BlockCache {
    lru: Mutex<LruCache<u64, Arc<[u8]>>>,
}

impl std::fmt::Debug for BlockCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlockCache")
            .field("len", &self.lru.lock().len())
            .finish()
    }
}

impl BlockCache {
    pub(crate) fn new(entries: usize) -> Self {
        Self {
            lru: Mutex::new(LruCache::new(
                NonZeroUsize::new(entries.max(1)).expect("non-zero"),
            )),
        }
    }

    pub(crate) fn get_or_load(
        &self,
        key: u64,
        load: impl FnOnce() -> io::Result<Vec<u8>>,
    ) -> io::Result<Arc<[u8]>> {
        if let Some(v) = self.lru.lock().get(&key) {
            return Ok(v.clone());
        }
        let v: Arc<[u8]> = load()?.into();
        self.lru.lock().put(key, v.clone());
        Ok(v)
    }

    pub(crate) fn clear(&self) {
        self.lru.lock().clear();
    }
}

/// Splits `[offset, offset + buf.len())` at `block` boundaries and calls
/// `f(block index, offset within block, destination)` for each piece.
pub(crate) fn for_blocks(
    offset: u64,
    buf: &mut [u8],
    block: u64,
    mut f: impl FnMut(u64, usize, &mut [u8]) -> io::Result<()>,
) -> io::Result<()> {
    let mut done = 0usize;
    while done < buf.len() {
        let pos = offset + done as u64;
        let within = (pos % block) as usize;
        let n = (block as usize - within).min(buf.len() - done);
        f(pos / block, within, &mut buf[done..done + n])?;
        done += n;
    }
    Ok(())
}

/// Inflates a zlib stream (`zlib = true`) or raw deflate into exactly
/// `size` bytes (short output is zero-padded, extra input ignored).
pub(crate) fn inflate(data: &[u8], size: usize, zlib: bool) -> io::Result<Vec<u8>> {
    let mut out = vec![0u8; size];
    let mut d = flate2::Decompress::new(zlib);
    d.decompress(data, &mut out, flate2::FlushDecompress::Finish)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(out)
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::io::Write;

    pub(crate) fn zlib(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    pub(crate) fn deflate(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    pub(crate) fn pattern(len: usize, seed: u8) -> Vec<u8> {
        (0..len)
            .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_raw_and_detection() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("disk.001"), [1u8; 1000]).unwrap();
        std::fs::write(dir.path().join("disk.002"), [2u8; 500]).unwrap();
        let p = open_image(&dir.path().join("disk.001")).unwrap();
        assert_eq!(p.len(), 1500);
        let mut b = [0u8; 4];
        p.read_at(998, &mut b).unwrap();
        assert_eq!(b, [1, 1, 2, 2]);
        let raw = dir.path().join("plain.img");
        std::fs::write(&raw, [0u8; 2048]).unwrap();
        let p = open_image(&raw).unwrap();
        assert_eq!(detect_format(p.as_ref()).unwrap(), ImageFormat::Raw);
        assert_eq!(p.len(), 2048);
    }
}
