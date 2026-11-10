//! Single-member gzip files (FILE-15).

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;

use flate2::Compression;
use flate2::GzBuilder;
use flate2::read::GzDecoder;

use super::{ArchiveEntry, read_limited};
use crate::error::Result;

/// File name and mtime stored in the gzip header.
pub(super) struct GzMeta {
    pub filename: Option<Vec<u8>>,
    pub mtime: u32,
}

/// Reads the gzip header of `path`.
pub(super) fn meta(path: &Path) -> Result<GzMeta> {
    let mut d = GzDecoder::new(BufReader::new(File::open(path)?));
    // The header is parsed by the first read.
    let _ = d.read(&mut [0u8; 1])?;
    Ok(d.header().map_or(
        GzMeta {
            filename: None,
            mtime: 0,
        },
        |h| GzMeta {
            filename: h.filename().map(<[u8]>::to_vec),
            mtime: h.mtime(),
        },
    ))
}

/// Builds a gzip writer carrying `meta` over `out`.
pub(super) fn encoder<W: Write>(meta: &GzMeta, out: W) -> flate2::write::GzEncoder<W> {
    let mut b = GzBuilder::new().mtime(meta.mtime);
    if let Some(name) = &meta.filename {
        b = b.filename(name.clone());
    }
    b.write(out, Compression::default())
}

pub(super) fn entry(path: &Path) -> Result<ArchiveEntry> {
    let meta = meta(path)?;
    let mut f = File::open(path)?;
    let compressed = f.metadata()?.len();
    let mut isize = [0u8; 4];
    f.seek(SeekFrom::End(-4))?;
    f.read_exact(&mut isize)?;
    let name = meta.filename.as_deref().map_or_else(
        || {
            let file = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if super::has_suffix(&file, "gz") {
                file[..file.len() - 3].to_owned()
            } else {
                "data".to_owned()
            }
        },
        |n| {
            let n = String::from_utf8_lossy(n);
            n.rsplit(['/', '\\']).next().unwrap_or_default().to_owned()
        },
    );
    Ok(ArchiveEntry {
        index: 0,
        name,
        size: u64::from(u32::from_le_bytes(isize)),
        compressed_size: Some(compressed),
        is_dir: false,
        method: "gzip".into(),
        data_offset: None,
        modified: (meta.mtime != 0).then_some(i64::from(meta.mtime)),
        encrypted: false,
    })
}

pub(super) fn read(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let d = GzDecoder::new(BufReader::new(File::open(path)?));
    read_limited(d, limit, 0)
}

pub(super) fn repack(src: &Path, data: &[u8], out: &mut File) -> Result<()> {
    let meta = meta(src)?;
    let mut enc = encoder(&meta, out);
    enc.write_all(data)?;
    enc.finish()?;
    Ok(())
}
