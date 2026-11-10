//! Cloning and raw imaging (DISK-08 raw/dd image, DISK-09 clone): block copy
//! from one provider to another provider or a file, with an MD5 of the
//! copied data, progress, cancellation and optional verification.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::Path;

use hexa_core::provider::Provider;
use hexa_core::task::Progress;
use hexa_core::{Cancel, Result};
use md5::{Digest, Md5};
use serde::Serialize;

use crate::util::{check_cancel, invalid};
use crate::wipe::align_block;

/// What to copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloneOptions {
    pub block_size: usize,
    pub source_offset: u64,
    pub dest_offset: u64,
    /// Bytes to copy; `None` = rest of the source.
    pub length: Option<u64>,
    /// Re-read the destination and compare.
    pub verify: bool,
}

impl Default for CloneOptions {
    fn default() -> Self {
        Self {
            block_size: 1 << 20,
            source_offset: 0,
            dest_offset: 0,
            length: None,
            verify: false,
        }
    }
}

/// Outcome of a clone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloneReport {
    pub bytes_copied: u64,
    /// MD5 of the source bytes (lowercase hex).
    pub md5: String,
    pub verified: Option<bool>,
    pub first_mismatch: Option<u64>,
}

pub(crate) fn hex(b: &[u8]) -> String {
    crate::util::hex_upper(b).to_lowercase()
}

fn range(src: &dyn Provider, opts: &CloneOptions) -> Result<u64> {
    let len = opts
        .length
        .unwrap_or_else(|| src.len().saturating_sub(opts.source_offset));
    if opts
        .source_offset
        .checked_add(len)
        .is_none_or(|e| e > src.len())
    {
        return Err(invalid("clone range is past the end of the source"));
    }
    Ok(len)
}

/// Copies `src` into `dst` (DISK-09).
pub fn clone_provider(
    src: &dyn Provider,
    dst: &dyn Provider,
    opts: &CloneOptions,
    progress: Progress<'_>,
    cancel: Option<&Cancel>,
) -> Result<CloneReport> {
    if !dst.capabilities().writable {
        return Err(hexa_core::Error::ReadOnly);
    }
    let len = range(src, opts)?;
    let bs = align_block(opts.block_size, dst.capabilities().sector_size);
    let total = if opts.verify { len * 2 } else { len };
    let mut buf = vec![0u8; bs];
    let mut md5 = Md5::new();
    let mut done = 0u64;
    while done < len {
        check_cancel(cancel)?;
        let n = ((len - done) as usize).min(bs);
        src.read_at(opts.source_offset + done, &mut buf[..n])?;
        md5.update(&buf[..n]);
        dst.write_at(opts.dest_offset + done, &buf[..n])?;
        done += n as u64;
        progress(done, total);
    }
    dst.flush()?;
    let mut report = CloneReport {
        bytes_copied: len,
        md5: hex(&md5.finalize()),
        verified: None,
        first_mismatch: None,
    };
    if opts.verify {
        dst.invalidate();
        let mut other = vec![0u8; bs];
        let mut pos = 0u64;
        while pos < len {
            check_cancel(cancel)?;
            let n = ((len - pos) as usize).min(bs);
            src.read_at(opts.source_offset + pos, &mut buf[..n])?;
            dst.read_at(opts.dest_offset + pos, &mut other[..n])?;
            if let Some(i) = buf[..n].iter().zip(&other[..n]).position(|(a, b)| a != b) {
                report.first_mismatch = Some(pos + i as u64);
                break;
            }
            pos += n as u64;
            progress(len + pos, total);
        }
        report.verified = Some(report.first_mismatch.is_none());
    }
    Ok(report)
}

/// Writes `src` to a new raw (dd) image file (DISK-08); `dest_offset` is
/// ignored and the file is created or truncated.
pub fn clone_to_file(
    src: &dyn Provider,
    dest: &Path,
    opts: &CloneOptions,
    progress: Progress<'_>,
    cancel: Option<&Cancel>,
) -> Result<CloneReport> {
    let len = range(src, opts)?;
    let bs = opts.block_size.max(4096);
    let total = if opts.verify { len * 2 } else { len };
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(dest)?;
    let mut buf = vec![0u8; bs];
    let mut md5 = Md5::new();
    let mut done = 0u64;
    while done < len {
        check_cancel(cancel)?;
        let n = ((len - done) as usize).min(bs);
        src.read_at(opts.source_offset + done, &mut buf[..n])?;
        md5.update(&buf[..n]);
        file.write_all(&buf[..n])?;
        done += n as u64;
        progress(done, total);
    }
    file.sync_all()?;
    drop(file);
    let mut report = CloneReport {
        bytes_copied: len,
        md5: hex(&md5.finalize()),
        verified: None,
        first_mismatch: None,
    };
    if opts.verify {
        let mut f = std::fs::File::open(dest)?;
        let mut other = vec![0u8; bs];
        let mut pos = 0u64;
        while pos < len {
            check_cancel(cancel)?;
            let n = ((len - pos) as usize).min(bs);
            src.read_at(opts.source_offset + pos, &mut buf[..n])?;
            f.read_exact(&mut other[..n])?;
            if let Some(i) = buf[..n].iter().zip(&other[..n]).position(|(a, b)| a != b) {
                report.first_mismatch = Some(pos + i as u64);
                break;
            }
            pos += n as u64;
            progress(len + pos, total);
        }
        report.verified = Some(report.first_mismatch.is_none());
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hexa_core::provider::MemoryProvider;
    use hexa_core::task::no_progress;

    #[test]
    fn clone_between_providers_and_to_file() {
        let data: Vec<u8> = (0..100_000u32).map(|i| (i * 31) as u8).collect();
        let src = MemoryProvider::new("s", data.clone());
        let dst = MemoryProvider::new("d", vec![0; 100_000]);
        let opts = CloneOptions {
            block_size: 4096,
            source_offset: 10,
            dest_offset: 20,
            length: Some(50_000),
            verify: true,
        };
        let r = clone_provider(&src, &dst, &opts, &no_progress, None).unwrap();
        assert_eq!(r.verified, Some(true));
        let mut buf = vec![0u8; 50_000];
        dst.read_at(20, &mut buf).unwrap();
        assert_eq!(buf, data[10..50_010]);
        assert_eq!(r.md5, hex(&Md5::digest(&data[10..50_010])));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("disk.dd");
        let r = clone_to_file(
            &src,
            &path,
            &CloneOptions {
                verify: true,
                ..CloneOptions::default()
            },
            &no_progress,
            None,
        )
        .unwrap();
        assert_eq!(r.verified, Some(true));
        assert_eq!(std::fs::read(&path).unwrap(), data);

        let cancel = Cancel::new();
        cancel.cancel();
        assert!(matches!(
            clone_provider(&src, &dst, &opts, &no_progress, Some(&cancel)),
            Err(hexa_core::Error::Cancelled)
        ));
    }
}
