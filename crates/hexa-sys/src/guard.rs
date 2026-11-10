//! Safety net for writes to disks (DISK-12): a [`WriteGuard`] stages writes
//! and shows a preview (offset, old bytes, new bytes) before anything is
//! committed, and every commit produces an [`UndoJournal`] of the
//! overwritten bytes that can roll the change back (and be saved to disk).

use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;

use hexa_core::provider::Provider;
use hexa_core::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::util::{invalid, read_vec};

/// One staged write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingWrite {
    pub offset: u64,
    /// Bytes on the target when the write was staged.
    pub old: Vec<u8>,
    pub new: Vec<u8>,
}

/// Overwritten bytes recorded by a commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalEntry {
    pub offset: u64,
    pub old: Vec<u8>,
}

/// Undo journal of a committed [`WriteGuard`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoJournal {
    pub entries: Vec<JournalEntry>,
}

const JOURNAL_MAGIC: &[u8; 8] = b"HEXAUNDO";

impl UndoJournal {
    /// Restores the old bytes, newest write first.
    pub fn rollback(&self, target: &dyn Provider) -> Result<()> {
        for e in self.entries.iter().rev() {
            target.write_at(e.offset, &e.old)?;
        }
        target.flush()?;
        Ok(())
    }

    pub fn total_bytes(&self) -> u64 {
        self.entries.iter().map(|e| e.old.len() as u64).sum()
    }

    /// Saves the journal: magic, `u64` count, then `u64 offset`,
    /// `u64 length`, bytes per entry (little-endian).
    pub fn save(&self, path: &Path) -> Result<()> {
        let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
        f.write_all(JOURNAL_MAGIC)?;
        f.write_all(&(self.entries.len() as u64).to_le_bytes())?;
        for e in &self.entries {
            f.write_all(&e.offset.to_le_bytes())?;
            f.write_all(&(e.old.len() as u64).to_le_bytes())?;
            f.write_all(&e.old)?;
        }
        f.into_inner()
            .map_err(std::io::IntoInnerError::into_error)?
            .sync_all()?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self> {
        let mut f = std::io::BufReader::new(std::fs::File::open(path)?);
        let mut magic = [0u8; 8];
        f.read_exact(&mut magic)?;
        if &magic != JOURNAL_MAGIC {
            return Err(invalid("not an undo journal"));
        }
        let mut u = [0u8; 8];
        f.read_exact(&mut u)?;
        let count = u64::from_le_bytes(u);
        let mut entries = Vec::new();
        for _ in 0..count {
            f.read_exact(&mut u)?;
            let offset = u64::from_le_bytes(u);
            f.read_exact(&mut u)?;
            let len = u64::from_le_bytes(u);
            if len > 1 << 32 {
                return Err(invalid("corrupt undo journal"));
            }
            let mut old = vec![0u8; len as usize];
            f.read_exact(&mut old)?;
            entries.push(JournalEntry { offset, old });
        }
        Ok(Self { entries })
    }
}

/// Stages writes to a provider for review before committing (DISK-12).
#[derive(Debug)]
pub struct WriteGuard {
    target: Arc<dyn Provider>,
    pending: Vec<PendingWrite>,
}

impl WriteGuard {
    pub fn new(target: Arc<dyn Provider>) -> Self {
        Self {
            target,
            pending: Vec::new(),
        }
    }

    /// Stages `data` at `offset`, capturing the current bytes.
    pub fn stage(&mut self, offset: u64, data: &[u8]) -> Result<()> {
        if !self.target.capabilities().writable {
            return Err(Error::ReadOnly);
        }
        let end = offset.checked_add(data.len() as u64);
        if end.is_none_or(|e| e > self.target.len()) {
            return Err(Error::OutOfRange {
                start: offset,
                end: end.unwrap_or(u64::MAX),
                len: self.target.len(),
            });
        }
        let old = read_vec(self.target.as_ref(), offset, data.len())?;
        self.pending.push(PendingWrite {
            offset,
            old,
            new: data.to_vec(),
        });
        Ok(())
    }

    /// The staged writes in order (the preview).
    pub fn pending(&self) -> &[PendingWrite] {
        &self.pending
    }

    pub fn total_bytes(&self) -> u64 {
        self.pending.iter().map(|w| w.new.len() as u64).sum()
    }

    /// Drops every staged write.
    pub fn discard(&mut self) {
        self.pending.clear();
    }

    /// Applies the staged writes. Fails without writing anything if the
    /// target changed since staging. Returns the undo journal.
    pub fn commit(mut self) -> Result<UndoJournal> {
        self.target.invalidate();
        for w in &self.pending {
            let now = read_vec(self.target.as_ref(), w.offset, w.old.len())?;
            if now != w.old {
                return Err(invalid(format!(
                    "bytes at {:#x} changed since the write was staged",
                    w.offset
                )));
            }
        }
        let mut journal = UndoJournal::default();
        for w in std::mem::take(&mut self.pending) {
            let old = read_vec(self.target.as_ref(), w.offset, w.new.len())?;
            journal.entries.push(JournalEntry {
                offset: w.offset,
                old,
            });
            if let Err(e) = self.target.write_at(w.offset, &w.new) {
                // Leave the target as it was before the commit.
                let _ = journal.rollback(self.target.as_ref());
                return Err(e.into());
            }
        }
        self.target.flush()?;
        Ok(journal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hexa_core::provider::MemoryProvider;

    #[test]
    fn preview_commit_and_rollback() {
        let p: Arc<dyn Provider> = Arc::new(MemoryProvider::new("d", (0..64).collect()));
        let mut g = WriteGuard::new(p.clone());
        g.stage(4, b"AAAA").unwrap();
        g.stage(6, b"BB").unwrap();
        assert_eq!(g.pending()[0].old, vec![4, 5, 6, 7]);
        assert_eq!(g.pending()[1].old, vec![6, 7]);
        assert!(g.stage(62, b"xyz").is_err());
        let journal = g.commit().unwrap();
        let mut buf = [0u8; 6];
        p.read_at(3, &mut buf).unwrap();
        assert_eq!(&buf, &[3, b'A', b'A', b'B', b'B', 8]);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("undo.bin");
        journal.save(&path).unwrap();
        let loaded = UndoJournal::load(&path).unwrap();
        assert_eq!(loaded, journal);
        loaded.rollback(p.as_ref()).unwrap();
        let mut all = vec![0u8; 64];
        p.read_at(0, &mut all).unwrap();
        assert_eq!(all, (0..64).collect::<Vec<u8>>());
    }

    #[test]
    fn commit_refuses_when_target_changed() {
        let p: Arc<dyn Provider> = Arc::new(MemoryProvider::new("d", vec![0; 16]));
        let mut g = WriteGuard::new(p.clone());
        g.stage(0, b"new").unwrap();
        p.write_at(1, b"!").unwrap();
        assert!(g.commit().is_err());
        let mut b = [0u8; 3];
        p.read_at(0, &mut b).unwrap();
        assert_eq!(&b, &[0, b'!', 0]);
    }
}
