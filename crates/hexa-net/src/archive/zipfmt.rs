//! zip entries (FILE-15).

use std::fs::File;
use std::io::{BufReader, Write};
use std::path::Path;

use zip::{CompressionMethod, ZipArchive, ZipWriter};

use super::{ArchiveEntry, read_limited};
use crate::error::{Error, Result};
use crate::time::{Civil, unix_from_civil};

fn open(path: &Path) -> Result<ZipArchive<BufReader<File>>> {
    Ok(ZipArchive::new(BufReader::new(File::open(path)?))?)
}

pub(super) fn list(path: &Path) -> Result<Vec<ArchiveEntry>> {
    let mut za = open(path)?;
    let mut out = Vec::with_capacity(za.len());
    for index in 0..za.len() {
        let f = za.by_index_raw(index)?;
        let stored = f.compression() == CompressionMethod::Stored;
        let encrypted = f.encrypted();
        let is_dir = f.is_dir();
        out.push(ArchiveEntry {
            index,
            name: f.name()?.into_owned(),
            size: f.size(),
            compressed_size: Some(f.compressed_size()),
            is_dir,
            method: format!("{:?}", f.compression()),
            data_offset: if stored && !encrypted && !is_dir {
                f.data_start()
            } else {
                None
            },
            modified: f.last_modified().map(|d| {
                unix_from_civil(Civil {
                    year: i64::from(d.year()),
                    month: u32::from(d.month()).max(1),
                    day: u32::from(d.day()).max(1),
                    hour: u32::from(d.hour()),
                    minute: u32::from(d.minute()),
                    second: u32::from(d.second()),
                })
            }),
            encrypted,
        });
    }
    Ok(out)
}

pub(super) fn read(path: &Path, index: usize, limit: u64) -> Result<Vec<u8>> {
    let mut za = open(path)?;
    let f = za.by_index(index)?;
    let size = f.size();
    read_limited(f, limit, size)
}

pub(super) fn repack(src: &Path, index: usize, data: &[u8], out: &mut File) -> Result<()> {
    let mut za = open(src)?;
    let mut w = ZipWriter::new(out);
    w.set_raw_comment(za.comment().into())?;
    for i in 0..za.len() {
        let f = za.by_index_raw(i)?;
        if i != index {
            w.raw_copy_file(f)?;
            continue;
        }
        if f.encrypted() {
            return Err(Error::Unsupported(
                "replacing an encrypted zip entry".into(),
            ));
        }
        let method = match f.compression() {
            CompressionMethod::Stored => CompressionMethod::Stored,
            _ => CompressionMethod::Deflated,
        };
        let options = f
            .options()
            .compression_method(method)
            .large_file(data.len() as u64 >= u64::from(u32::MAX));
        let name = f.name()?.into_owned();
        drop(f);
        w.start_file(name, options)?;
        w.write_all(data)?;
    }
    w.finish()?;
    Ok(())
}
