//! tar and tar.gz entries (FILE-15).
//!
//! Entries are walked in raw mode so GNU long-name (`L`, `K`) and PAX (`x`,
//! `g`) records can be copied verbatim when repacking; the logical name of an
//! entry is resolved from them the same way the `tar` crate does.

use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::Path;

use flate2::read::MultiGzDecoder;
use tar::{Entries, Entry, Header};

use super::{ArchiveEntry, gzfmt, read_limited};
use crate::error::{Error, Result};

/// Largest extension record (long name / PAX) accepted.
const MAX_EXT: u64 = 1 << 20;

/// An extension record preceding an entry.
struct Ext {
    header: Header,
    data: Vec<u8>,
}

impl Ext {
    fn is_pax_local(&self) -> bool {
        self.header.entry_type().is_pax_local_extensions()
    }
}

/// Parses PAX records (`<len> <key>=<value>\n`).
fn parse_pax(data: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut out = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        let Some(sp) = rest.iter().position(|&b| b == b' ') else {
            break;
        };
        let Some(len) = std::str::from_utf8(&rest[..sp])
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
        else {
            break;
        };
        if len <= sp + 1 || len > rest.len() {
            break;
        }
        let record = &rest[sp + 1..len];
        let record = record.strip_suffix(b"\n").unwrap_or(record);
        if let Some(eq) = record.iter().position(|&b| b == b'=') {
            out.push((record[..eq].to_vec(), record[eq + 1..].to_vec()));
        }
        rest = &rest[len..];
    }
    out
}

fn encode_pax(records: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (k, v) in records {
        let body = k.len() + v.len() + 3; // ' ', '=', '\n'
        let mut len = body + 1;
        loop {
            let total = len.to_string().len() + body;
            if total == len {
                break;
            }
            len = total;
        }
        out.extend_from_slice(len.to_string().as_bytes());
        out.push(b' ');
        out.extend_from_slice(k);
        out.push(b'=');
        out.extend_from_slice(v);
        out.push(b'\n');
    }
    out
}

fn pax_value(pending: &[Ext], key: &[u8]) -> Option<Vec<u8>> {
    pending
        .iter()
        .filter(|e| e.is_pax_local())
        .flat_map(|e| parse_pax(&e.data))
        .filter(|(k, _)| k.as_slice() == key)
        .map(|(_, v)| v)
        .next_back()
}

fn logical_name(header: &Header, pending: &[Ext]) -> String {
    if let Some(long) = pending
        .iter()
        .rev()
        .find(|e| e.header.entry_type().is_gnu_longname())
    {
        let end = long
            .data
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(long.data.len());
        return String::from_utf8_lossy(&long.data[..end]).into_owned();
    }
    if let Some(path) = pax_value(pending, b"path") {
        return String::from_utf8_lossy(&path).into_owned();
    }
    String::from_utf8_lossy(&header.path_bytes()).into_owned()
}

/// Walks logical entries; `visit(index, name, entry, extensions)` returns
/// whether to continue.
fn walk<R: Read>(
    entries: Entries<'_, R>,
    mut visit: impl FnMut(usize, &str, Entry<'_, R>, &[Ext]) -> Result<bool>,
) -> Result<()> {
    let mut pending: Vec<Ext> = Vec::new();
    let mut index = 0;
    for entry in entries.raw(true) {
        let entry = entry?;
        let t = entry.header().entry_type();
        if t.is_gnu_longname()
            || t.is_gnu_longlink()
            || t.is_pax_local_extensions()
            || t.is_pax_global_extensions()
        {
            let header = entry.header().clone();
            if entry.size() > MAX_EXT {
                return Err(Error::Archive("oversized tar extension record".into()));
            }
            let mut data = Vec::new();
            let mut entry = entry;
            entry.read_to_end(&mut data)?;
            pending.push(Ext { header, data });
            continue;
        }
        let name = logical_name(entry.header(), &pending);
        if !visit(index, &name, entry, &pending)? {
            return Ok(());
        }
        pending.clear();
        index += 1;
    }
    Ok(())
}

fn entry_size(header: &Header, pending: &[Ext]) -> u64 {
    pax_value(pending, b"size")
        .and_then(|v| String::from_utf8(v).ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| header.entry_size().unwrap_or(0))
}

fn list_from<R: Read>(entries: Entries<'_, R>, plain: bool) -> Result<Vec<ArchiveEntry>> {
    let mut out = Vec::new();
    walk(entries, |index, name, entry, pending| {
        let h = entry.header();
        let t = h.entry_type();
        let size = entry_size(h, pending);
        let contiguous = matches!(t, tar::EntryType::Regular | tar::EntryType::Continuous);
        out.push(ArchiveEntry {
            index,
            name: name.to_owned(),
            size,
            compressed_size: None,
            is_dir: t.is_dir(),
            method: if plain { "stored" } else { "gzip" }.into(),
            data_offset: (plain && contiguous).then(|| entry.raw_file_position()),
            modified: h.mtime().ok().map(|m| m as i64),
            encrypted: false,
        });
        Ok(true)
    })?;
    Ok(out)
}

pub(super) fn list(path: &Path, gz: bool) -> Result<Vec<ArchiveEntry>> {
    if gz {
        let mut a = tar::Archive::new(MultiGzDecoder::new(BufReader::new(File::open(path)?)));
        list_from(a.entries()?, false)
    } else {
        let mut a = tar::Archive::new(File::open(path)?);
        list_from(a.entries_with_seek()?, true)
    }
}

fn read_from<R: Read>(entries: Entries<'_, R>, index: usize, limit: u64) -> Result<Vec<u8>> {
    let mut found = None;
    walk(entries, |i, _, entry, pending| {
        if i != index {
            return Ok(true);
        }
        let size = entry_size(entry.header(), pending);
        found = Some(read_limited(entry, limit, size)?);
        Ok(false)
    })?;
    found.ok_or_else(|| Error::EntryNotFound(format!("#{index}")))
}

pub(super) fn read(path: &Path, gz: bool, index: usize, limit: u64) -> Result<Vec<u8>> {
    if gz {
        let mut a = tar::Archive::new(MultiGzDecoder::new(BufReader::new(File::open(path)?)));
        read_from(a.entries()?, index, limit)
    } else {
        let mut a = tar::Archive::new(File::open(path)?);
        read_from(a.entries_with_seek()?, index, limit)
    }
}

/// Copies every record, replacing entry `index` by `data`.
fn repack_into<R: Read, W: Write>(
    entries: Entries<'_, R>,
    out: W,
    index: usize,
    data: &[u8],
) -> Result<W> {
    let mut b = tar::Builder::new(out);
    let mut replaced = false;
    walk(entries, |i, _, mut entry, pending| {
        let target = i == index;
        for ext in pending {
            if target && ext.is_pax_local() {
                let mut records = parse_pax(&ext.data);
                for (k, v) in &mut records {
                    if k.as_slice() == b"size" {
                        *v = data.len().to_string().into_bytes();
                    }
                }
                let body = encode_pax(&records);
                let mut h = ext.header.clone();
                h.set_size(body.len() as u64);
                h.set_cksum();
                b.append(&h, body.as_slice())?;
            } else {
                b.append(&ext.header, ext.data.as_slice())?;
            }
        }
        let mut h = entry.header().clone();
        if target {
            h.set_size(data.len() as u64);
            h.set_cksum();
            b.append(&h, data)?;
            replaced = true;
        } else {
            b.append(&h, &mut entry)?;
        }
        Ok(true)
    })?;
    if !replaced {
        return Err(Error::EntryNotFound(format!("#{index}")));
    }
    Ok(b.into_inner()?)
}

pub(super) fn repack(
    src: &Path,
    gz: bool,
    index: usize,
    data: &[u8],
    out: &mut File,
) -> Result<()> {
    if gz {
        let meta = gzfmt::meta(src)?;
        let mut a = tar::Archive::new(MultiGzDecoder::new(BufReader::new(File::open(src)?)));
        let enc = repack_into(a.entries()?, gzfmt::encoder(&meta, out), index, data)?;
        enc.finish()?;
    } else {
        let mut a = tar::Archive::new(BufReader::new(File::open(src)?));
        repack_into(a.entries()?, out, index, data)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pax_round_trip() {
        let records = vec![
            (b"path".to_vec(), b"a/very/long/name".to_vec()),
            (b"size".to_vec(), b"123456789012".to_vec()),
            (b"k".to_vec(), vec![b'x'; 95]),
        ];
        let enc = encode_pax(&records);
        assert!(enc.starts_with(b"25 path=a/very/long/name\n"));
        // 1 + 95 + 3 = 99 bytes of body, so the length takes 3 digits.
        assert!(enc.ends_with(b"x\n"));
        assert!(enc.windows(6).any(|w| w == b"102 k="));
        assert_eq!(parse_pax(&enc), records);
    }
}
