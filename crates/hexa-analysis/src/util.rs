//! Shared helpers: chunked streaming over tree snapshots.

use hexa_core::task::Progress;
use hexa_core::{Cancel, Error, Result, Tree};

/// Streaming chunk size; every tree scan reads at most this many bytes at once.
pub(crate) const CHUNK: usize = 4 << 20;

pub(crate) fn invalid(msg: impl Into<String>) -> Error {
    Error::Invalid(msg.into())
}

/// Clamps `[start, end)` to the tree.
pub(crate) fn clamp(tree: &Tree, start: u64, end: u64) -> (u64, u64) {
    let end = end.min(tree.len());
    (start.min(end), end)
}

/// Calls `f(offset, bytes)` for consecutive chunks of `[start, end)` (clamped
/// to the tree). `f` returns `false` to stop early.
pub(crate) fn scan(
    tree: &Tree,
    start: u64,
    end: u64,
    chunk: usize,
    cancel: &Cancel,
    progress: Progress<'_>,
    mut f: impl FnMut(u64, &[u8]) -> Result<bool>,
) -> Result<()> {
    let (start, end) = clamp(tree, start, end);
    let total = end - start;
    let mut buf = vec![0u8; total.min(chunk.max(1) as u64) as usize];
    let mut at = start;
    while at < end {
        cancel.check()?;
        let n = (end - at).min(buf.len() as u64) as usize;
        tree.read(at, &mut buf[..n])?;
        let go = f(at, &buf[..n])?;
        at += n as u64;
        progress(at - start, total);
        if !go {
            break;
        }
    }
    Ok(())
}

/// Reads `len` bytes at `offset`, or `None` when the range leaves the tree.
pub(crate) fn read_exact(tree: &Tree, offset: u64, len: usize) -> Option<Vec<u8>> {
    let end = offset.checked_add(len as u64)?;
    if end > tree.len() {
        return None;
    }
    let mut buf = vec![0u8; len];
    tree.read(offset, &mut buf).ok()?;
    Some(buf)
}

/// Reads `[start, end)` into memory, failing with `TooLarge` above `limit`.
pub(crate) fn read_all(tree: &Tree, start: u64, end: u64, limit: u64) -> Result<Vec<u8>> {
    let (start, end) = clamp(tree, start, end);
    let len = end - start;
    if len > limit {
        return Err(Error::TooLarge { len, limit });
    }
    Ok(tree.read_vec(start, len as usize)?)
}

/// Sequential byte reader over a tree range with an internal chunk buffer.
#[derive(Debug)]
pub(crate) struct Cursor<'a> {
    tree: &'a Tree,
    next: u64,
    end: u64,
    buf: Vec<u8>,
    idx: usize,
}

impl<'a> Cursor<'a> {
    pub(crate) fn new(tree: &'a Tree, start: u64, end: u64, chunk: usize) -> Self {
        let (start, end) = clamp(tree, start, end);
        Self {
            tree,
            next: start,
            end,
            buf: Vec::with_capacity(chunk.max(1)),
            idx: 0,
        }
    }

    /// Returns the next byte or `None` at the end of the range.
    pub(crate) fn next_byte(&mut self) -> Result<Option<u8>> {
        if self.idx == self.buf.len() {
            if self.next >= self.end {
                return Ok(None);
            }
            let cap = self.buf.capacity() as u64;
            let n = (self.end - self.next).min(cap) as usize;
            self.buf.resize(n, 0);
            self.tree.read(self.next, &mut self.buf)?;
            self.next += n as u64;
            self.idx = 0;
        }
        let b = self.buf[self.idx];
        self.idx += 1;
        Ok(Some(b))
    }
}

/// Lower-case hex string.
pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Shannon entropy in bits per byte of a histogram.
pub(crate) fn entropy_of(counts: &[u64; 256], total: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    let n = total as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / n;
            -p * p.log2()
        })
        .sum()
}

/// Pearson chi-square statistic against a uniform byte distribution.
pub(crate) fn chi_square_of(counts: &[u64; 256], total: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    let expected = total as f64 / 256.0;
    counts
        .iter()
        .map(|&c| {
            let d = c as f64 - expected;
            d * d / expected
        })
        .sum()
}
