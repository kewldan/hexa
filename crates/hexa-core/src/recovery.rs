//! Crash recovery of unsaved edits (FILE-10).
//!
//! The current piece list is serialised compactly: bytes that still come from
//! the opened file are stored as references, only typed or pasted bytes are
//! stored inline. Restoring requires the file to be unchanged on disk.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use crate::document::{Document, EditMode, FileStamp, OpenOptions, file_provider};
use crate::error::{Error, Result};
use crate::history::Change;
use crate::piece::{Piece, Source};
use crate::provider::{FileOpenOptions, FileProvider, Provider};
use crate::tree::Tree;

const MAGIC: &[u8; 8] = b"HXRECOV1";

struct Writer(Vec<u8>);

impl Writer {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn bytes(&mut self, b: &[u8]) {
        self.u64(b.len() as u64);
        self.0.extend_from_slice(b);
    }
    fn str(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        if self.0.len() < n {
            return Err(Error::invalid("truncated recovery data"));
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("8 bytes"),
        ))
    }
    fn bytes(&mut self) -> Result<Vec<u8>> {
        let n = self.u64()? as usize;
        Ok(self.take(n)?.to_vec())
    }
    fn str(&mut self) -> Result<String> {
        String::from_utf8(self.bytes()?).map_err(|_| Error::invalid("bad UTF-8 in recovery data"))
    }
}

fn stamp_parts(s: Option<FileStamp>) -> (u64, u64, u64) {
    s.map_or((u64::MAX, 0, 0), |s| {
        let d = s
            .modified
            .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
            .unwrap_or_default();
        (s.len, d.as_secs(), u64::from(d.subsec_nanos()))
    })
}

impl Document {
    /// Serialises unsaved state; `None` when there is nothing to recover.
    pub fn export_recovery(&self) -> Result<Option<Vec<u8>>> {
        if !self.is_modified() {
            return Ok(None);
        }
        let tree = self.snapshot();
        let mut w = Writer(MAGIC.to_vec());
        w.str(
            &self
                .path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
        w.str(&self.name);
        let (len, secs, nanos) = stamp_parts(self.stamp);
        w.u64(len);
        w.u64(secs);
        w.u64(nanos);
        w.u8(match self.mode() {
            EditMode::ReadOnly => 0,
            EditMode::Overwrite => 1,
            EditMode::Insert => 2,
        });
        w.u8(u8::from(self.fixed_size));
        let base_is_file = self.path.is_some() && file_provider(&self.base).is_some();
        let pieces = tree.pieces(0, tree.len());
        w.u64(pieces.len() as u64);
        for (_, p) in pieces {
            match &p.source {
                Source::Provider(prov) if base_is_file && p.source.is_provider(&self.base) => {
                    let _ = prov;
                    w.u8(0);
                    w.u64(p.offset);
                    w.u64(p.len);
                }
                Source::Provider(prov) if file_provider(prov).is_some() => {
                    let fp = file_provider(prov).expect("checked");
                    w.u8(4);
                    w.str(&fp.path().to_string_lossy());
                    w.u64(p.offset);
                    w.u64(p.len);
                }
                Source::Fill(pattern) => {
                    w.u8(2);
                    w.bytes(pattern);
                    w.u64(p.offset);
                    w.u64(p.len);
                }
                Source::Random(seed) => {
                    w.u8(3);
                    w.u64(*seed);
                    w.u64(p.offset);
                    w.u64(p.len);
                }
                _ => {
                    let mut data = vec![0u8; p.len as usize];
                    p.read(0, &mut data)?;
                    w.u8(1);
                    w.bytes(&data);
                }
            }
        }
        Ok(Some(w.0))
    }

    /// Path and name stored in recovery data, for listing before restoring.
    pub fn peek_recovery(data: &[u8]) -> Result<(Option<PathBuf>, String)> {
        let mut r = Reader(data);
        if r.take(8)? != MAGIC {
            return Err(Error::invalid("not a Hexa recovery file"));
        }
        let path = r.str()?;
        let name = r.str()?;
        Ok(((!path.is_empty()).then(|| PathBuf::from(path)), name))
    }

    /// Rebuilds a document from [`Document::export_recovery`] output. The
    /// restored state is one undoable step on top of the file.
    pub fn import_recovery(data: &[u8]) -> Result<Self> {
        let mut r = Reader(data);
        if r.take(8)? != MAGIC {
            return Err(Error::invalid("not a Hexa recovery file"));
        }
        let path = r.str()?;
        let name = r.str()?;
        let (len, secs, nanos) = (r.u64()?, r.u64()?, r.u64()?);
        let mode = match r.u8()? {
            0 => EditMode::ReadOnly,
            2 => EditMode::Insert,
            _ => EditMode::Overwrite,
        };
        let fixed = r.u8()? != 0;
        let mut doc = if path.is_empty() {
            let mut d = Document::new_empty();
            d.name = name;
            d
        } else {
            let p = Path::new(&path);
            let now = FileStamp::read(p)?;
            let modified = Some(UNIX_EPOCH + Duration::new(secs, nanos as u32));
            if now.len != len || (now.modified.is_some() && now.modified != modified) {
                return Err(Error::invalid(format!(
                    "{path} changed on disk after the crash; recovery would corrupt it"
                )));
            }
            Document::open(
                p,
                &OpenOptions {
                    mode: EditMode::Overwrite,
                    ..OpenOptions::default()
                },
            )?
        };
        let count = r.u64()?;
        let mut pieces = Vec::new();
        for _ in 0..count {
            let piece = match r.u8()? {
                0 => {
                    let (off, len) = (r.u64()?, r.u64()?);
                    if off + len > doc.base.len() {
                        return Err(Error::invalid(
                            "recovery references bytes past the file end",
                        ));
                    }
                    Piece::provider(doc.base.clone(), off, len)
                }
                1 => Piece::bytes(r.bytes()?),
                2 => {
                    let pattern = r.bytes()?;
                    let (off, len) = (r.u64()?, r.u64()?);
                    let mut p = Piece::fill(pattern, len);
                    p.offset = off;
                    p
                }
                3 => {
                    let seed = r.u64()?;
                    let (off, len) = (r.u64()?, r.u64()?);
                    let mut p = Piece::random(seed, len);
                    p.offset = off;
                    p
                }
                4 => {
                    let fpath = r.str()?;
                    let (off, len) = (r.u64()?, r.u64()?);
                    let fp: Arc<dyn Provider> = Arc::new(FileProvider::open(
                        Path::new(&fpath),
                        FileOpenOptions::default(),
                    )?);
                    if off + len > fp.len() {
                        return Err(Error::invalid(format!("{fpath} is shorter than recorded")));
                    }
                    Piece::provider(fp, off, len)
                }
                k => return Err(Error::invalid(format!("unknown piece kind {k}"))),
            };
            pieces.push(piece);
        }
        let tree = Tree::from_pieces(pieces);
        let old_len = doc.len();
        let new_len = tree.len();
        doc.history.commit(
            tree,
            "Recovered session",
            Change {
                start: 0,
                removed: old_len,
                inserted: new_len,
            },
        );
        if !doc.write_denied() {
            doc.set_mode(mode)?;
        }
        doc.set_fixed_size(fixed);
        Ok(doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_file_document() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.bin");
        std::fs::write(&p, b"0123456789").unwrap();
        let mut doc = Document::open(&p, &OpenOptions::default()).unwrap();
        doc.set_mode(EditMode::Insert).unwrap();
        doc.insert(3, b"abc").unwrap();
        doc.fill(0, 2, &crate::document::Fill::Byte { value: 0xEE })
            .unwrap();
        let data = doc.export_recovery().unwrap().unwrap();
        assert_eq!(
            Document::peek_recovery(&data).unwrap().0.as_deref(),
            Some(p.as_path())
        );
        let restored = Document::import_recovery(&data).unwrap();
        assert_eq!(
            restored.read_vec(0, 100).unwrap(),
            doc.read_vec(0, 100).unwrap()
        );
        assert_eq!(restored.mode(), EditMode::Insert);
        assert!(restored.is_modified());

        std::fs::write(&p, b"changed!!!").unwrap();
        assert!(Document::import_recovery(&data).is_err());
    }

    #[test]
    fn roundtrip_new_document() {
        let mut doc = Document::new_empty();
        doc.insert(0, b"hello").unwrap();
        let data = doc.export_recovery().unwrap().unwrap();
        let restored = Document::import_recovery(&data).unwrap();
        assert_eq!(restored.read_vec(0, 10).unwrap(), b"hello");
        assert!(Document::new_empty().export_recovery().unwrap().is_none());
    }
}
