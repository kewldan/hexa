//! A provider made of consecutive segments of other providers (DISK-08).
//!
//! Used for split raw images (`disk.001`, `disk.002` …), VMDK descriptors with
//! several flat extents and any other "glue these ranges together" layout.

use std::io;
use std::sync::Arc;

use hexa_core::provider::{Capabilities, Provider};

use crate::util::check_range;

/// One piece of a [`ConcatProvider`].
#[derive(Debug, Clone)]
pub struct Segment {
    /// Where the bytes come from; `None` reads as zeros.
    pub source: Option<Arc<dyn Provider>>,
    /// Start of the piece inside `source`.
    pub offset: u64,
    /// Length of the piece.
    pub len: u64,
}

impl Segment {
    /// The whole of `source`.
    pub fn whole(source: Arc<dyn Provider>) -> Self {
        let len = source.len();
        Self {
            source: Some(source),
            offset: 0,
            len,
        }
    }

    /// `[offset, offset + len)` of `source`.
    pub fn range(source: Arc<dyn Provider>, offset: u64, len: u64) -> Self {
        Self {
            source: Some(source),
            offset,
            len,
        }
    }

    /// `len` zero bytes.
    pub fn zeros(len: u64) -> Self {
        Self {
            source: None,
            offset: 0,
            len,
        }
    }
}

/// Segments laid end to end.
#[derive(Debug)]
pub struct ConcatProvider {
    name: String,
    segments: Vec<Segment>,
    /// Logical start of each segment.
    starts: Vec<u64>,
    len: u64,
}

impl ConcatProvider {
    pub fn new(name: impl Into<String>, segments: Vec<Segment>) -> io::Result<Self> {
        let mut starts = Vec::with_capacity(segments.len());
        let mut len = 0u64;
        for s in &segments {
            if let Some(src) = &s.source
                && s.offset.checked_add(s.len).is_none_or(|e| e > src.len())
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "segment past end of its source",
                ));
            }
            starts.push(len);
            len = len
                .checked_add(s.len)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "image too large"))?;
        }
        Ok(Self {
            name: name.into(),
            segments,
            starts,
            len,
        })
    }

    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// Index of the segment holding logical offset `pos` (`pos < len`).
    fn find(&self, pos: u64) -> usize {
        self.starts.partition_point(|&s| s <= pos) - 1
    }

    fn for_each_piece(
        &self,
        offset: u64,
        len: usize,
        mut f: impl FnMut(&Segment, u64, usize, usize) -> io::Result<()>,
    ) -> io::Result<()> {
        check_range(offset, len, self.len)?;
        let mut done = 0usize;
        while done < len {
            let pos = offset + done as u64;
            let i = self.find(pos);
            let seg = &self.segments[i];
            let within = pos - self.starts[i];
            let n = ((seg.len - within) as usize).min(len - done);
            if n == 0 {
                // Zero-length segment at this position; move on.
                continue;
            }
            f(seg, seg.offset + within, done, n)?;
            done += n;
        }
        Ok(())
    }
}

impl Provider for ConcatProvider {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let len = buf.len();
        self.for_each_piece(offset, len, |seg, at, done, n| {
            let dst = &mut buf[done..done + n];
            if let Some(src) = &seg.source {
                src.read_at(at, dst)
            } else {
                dst.fill(0);
                Ok(())
            }
        })
    }

    fn capabilities(&self) -> Capabilities {
        let writable = !self.segments.is_empty()
            && self
                .segments
                .iter()
                .all(|s| s.source.as_ref().is_some_and(|p| p.capabilities().writable));
        Capabilities {
            writable,
            ..Capabilities::READ_ONLY
        }
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> io::Result<()> {
        self.for_each_piece(offset, data.len(), |seg, at, done, n| match &seg.source {
            Some(src) => src.write_at(at, &data[done..done + n]),
            None => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "cannot write into a zero segment",
            )),
        })
    }

    fn flush(&self) -> io::Result<()> {
        for s in &self.segments {
            if let Some(src) = &s.source {
                src.flush()?;
            }
        }
        Ok(())
    }

    fn invalidate(&self) {
        for s in &self.segments {
            if let Some(src) = &s.source {
                src.invalidate();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hexa_core::provider::MemoryProvider;

    #[test]
    fn reads_across_segments_and_zero_fill() {
        let a: Arc<dyn Provider> = Arc::new(MemoryProvider::new("a", vec![1; 10]));
        let b: Arc<dyn Provider> = Arc::new(MemoryProvider::new("b", (0..10).collect()));
        let c = ConcatProvider::new(
            "c",
            vec![
                Segment::whole(a),
                Segment::zeros(0),
                Segment::zeros(4),
                Segment::range(b, 5, 5),
            ],
        )
        .unwrap();
        assert_eq!(c.len(), 19);
        let mut buf = [0xAA; 8];
        c.read_at(8, &mut buf).unwrap();
        assert_eq!(buf, [1, 1, 0, 0, 0, 0, 5, 6]);
        assert!(!c.capabilities().writable);
        assert!(c.read_at(15, &mut buf).is_err());
    }
}
