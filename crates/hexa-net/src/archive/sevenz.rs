//! 7z entries (FILE-15).
//!
//! Repacking decodes every entry and writes a new non-solid LZMA2 archive:
//! the 7z crate cannot copy compressed blocks raw, and solid blocks would have
//! to be recompressed anyway once one of their files changes.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::time::SystemTime;

use sevenz_rust2::{ArchiveReader, ArchiveWriter, EntryDecision, Password};

use super::{ArchiveEntry, read_limited};
use crate::error::{Error, Result};
use crate::time::unix_seconds;

fn open(path: &Path) -> Result<ArchiveReader<BufReader<File>>> {
    Ok(ArchiveReader::new(
        BufReader::new(File::open(path)?),
        Password::empty(),
    )?)
}

pub(super) fn list(path: &Path) -> Result<Vec<ArchiveEntry>> {
    let reader = open(path)?;
    Ok(reader
        .archive()
        .files
        .iter()
        .enumerate()
        .map(|(index, f)| ArchiveEntry {
            index,
            name: f.name.clone(),
            size: f.size,
            compressed_size: (f.compressed_size > 0).then_some(f.compressed_size),
            is_dir: f.is_directory,
            method: "7z".into(),
            data_offset: None,
            modified: f
                .has_last_modified_date
                .then(|| unix_seconds(SystemTime::from(f.last_modified_date))),
            encrypted: false,
        })
        .collect())
}

pub(super) fn read(path: &Path, name: &str, limit: u64) -> Result<Vec<u8>> {
    let mut reader = open(path)?;
    let mut found = None;
    reader.for_each_entries_with_decision(|entry, data| {
        if entry.name != name {
            return Ok(EntryDecision::Continue);
        }
        found =
            Some(read_limited(data, limit, entry.size).map_err(|e| {
                sevenz_rust2::Error::Io(std::io::Error::from(e), "read entry".into())
            })?);
        Ok(EntryDecision::Stop)
    })?;
    found.ok_or_else(|| Error::EntryNotFound(name.to_owned()))
}

pub(super) fn repack(src: &Path, name: &str, data: &[u8], out: &mut File) -> Result<()> {
    let mut reader = open(src)?;
    let mut writer = ArchiveWriter::new(out)?;
    let mut replaced = false;
    reader.for_each_entries(|entry, content| {
        let copy = entry.clone();
        if entry.is_directory {
            writer.push_archive_entry::<&[u8]>(copy, None)?;
        } else if entry.name == name {
            writer.push_archive_entry(copy, Some(data))?;
            replaced = true;
        } else if entry.has_stream {
            writer.push_archive_entry(copy, Some(content as &mut dyn Read))?;
        } else {
            writer.push_archive_entry::<&[u8]>(copy, None)?;
        }
        Ok(true)
    })?;
    if !replaced {
        return Err(Error::EntryNotFound(name.to_owned()));
    }
    writer.finish()?;
    Ok(())
}
