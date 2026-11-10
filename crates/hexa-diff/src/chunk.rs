//! Chunked reading helpers shared by the diff and patch code.

use hexa_core::task::Progress;
use hexa_core::{Cancel, Error, Result, Tree};

/// Largest buffer used for streaming comparisons.
pub(crate) const CHUNK: usize = 1 << 20;

/// Progress reporting with a running counter.
pub(crate) struct Tracker<'a> {
    f: Progress<'a>,
    done: u64,
    total: u64,
}

impl<'a> Tracker<'a> {
    pub(crate) fn new(f: Progress<'a>, total: u64) -> Self {
        Self { f, done: 0, total }
    }

    pub(crate) fn set_total(&mut self, total: u64) {
        self.total = total;
    }

    pub(crate) fn add(&mut self, n: u64) {
        self.done = self.done.saturating_add(n);
        (self.f)(self.done.min(self.total), self.total);
    }

    pub(crate) fn set(&mut self, done: u64) {
        self.done = done;
        (self.f)(self.done.min(self.total), self.total);
    }

    pub(crate) fn finish(&mut self) {
        self.done = self.total;
        (self.f)(self.total, self.total);
    }
}

/// `done / total` in thousandths (1000 when `total` is zero).
pub(crate) fn permille(done: u64, total: u64) -> u64 {
    done.saturating_mul(1000).checked_div(total).unwrap_or(1000)
}

/// Reads `buf.len()` bytes at `offset`.
pub(crate) fn read(t: &Tree, offset: u64, buf: &mut [u8]) -> Result<()> {
    t.read(offset, buf).map_err(Error::from)
}

/// Index of the first differing byte of `a` and `b` (or the shorter length).
pub(crate) fn mismatch(a: &[u8], b: &[u8]) -> usize {
    let n = a.len().min(b.len());
    let (a, b) = (&a[..n], &b[..n]);
    let mut i = 0;
    while i + 64 <= n && a[i..i + 64] == b[i..i + 64] {
        i += 64;
    }
    while i + 8 <= n {
        let x = u64::from_le_bytes(a[i..i + 8].try_into().expect("8 bytes"));
        let y = u64::from_le_bytes(b[i..i + 8].try_into().expect("8 bytes"));
        if x != y {
            return i + ((x ^ y).trailing_zeros() / 8) as usize;
        }
        i += 8;
    }
    while i < n && a[i] == b[i] {
        i += 1;
    }
    i
}

/// Number of equal bytes at the end of `a` and `b`.
pub(crate) fn mismatch_rev(a: &[u8], b: &[u8]) -> usize {
    let n = a.len().min(b.len());
    let (a, b) = (&a[a.len() - n..], &b[b.len() - n..]);
    let mut k = 0;
    while k + 64 <= n && a[n - k - 64..n - k] == b[n - k - 64..n - k] {
        k += 64;
    }
    while k < n && a[n - k - 1] == b[n - k - 1] {
        k += 1;
    }
    k
}

/// Length of the common prefix of `a[a0..]` and `b[b0..]`, at most `max`.
/// Reads with growing buffers so short matches stay cheap.
pub(crate) fn common_prefix(
    a: &Tree,
    a0: u64,
    b: &Tree,
    b0: u64,
    max: u64,
    cancel: &Cancel,
) -> Result<u64> {
    let mut done = 0u64;
    let mut size = 256usize;
    let mut ba = Vec::new();
    let mut bb = Vec::new();
    while done < max {
        cancel.check()?;
        let n = (max - done).min(size as u64) as usize;
        ba.resize(n, 0);
        bb.resize(n, 0);
        read(a, a0 + done, &mut ba)?;
        read(b, b0 + done, &mut bb)?;
        let m = mismatch(&ba, &bb);
        done += m as u64;
        if m < n {
            break;
        }
        size = (size * 4).min(CHUNK);
    }
    Ok(done)
}

/// Length of the common suffix of `a[..a_end]` and `b[..b_end]`, at most
/// `max`.
pub(crate) fn common_suffix(
    a: &Tree,
    a_end: u64,
    b: &Tree,
    b_end: u64,
    max: u64,
    cancel: &Cancel,
) -> Result<u64> {
    let mut done = 0u64;
    let mut size = 256usize;
    let mut ba = Vec::new();
    let mut bb = Vec::new();
    while done < max {
        cancel.check()?;
        let n = (max - done).min(size as u64) as usize;
        ba.resize(n, 0);
        bb.resize(n, 0);
        read(a, a_end - done - n as u64, &mut ba)?;
        read(b, b_end - done - n as u64, &mut bb)?;
        let m = mismatch_rev(&ba, &bb);
        done += m as u64;
        if m < n {
            break;
        }
        size = (size * 4).min(CHUNK);
    }
    Ok(done)
}

/// Whether `a[a0..a0+len]` equals `b[b0..b0+len]`.
pub(crate) fn ranges_equal(
    a: &Tree,
    a0: u64,
    b: &Tree,
    b0: u64,
    len: u64,
    cancel: &Cancel,
) -> Result<bool> {
    Ok(common_prefix(a, a0, b, b0, len, cancel)? == len)
}

/// Calls `f` with consecutive chunks of `t[start..end]`.
pub(crate) fn for_each_chunk(
    t: &Tree,
    start: u64,
    end: u64,
    cancel: &Cancel,
    mut f: impl FnMut(u64, &[u8]) -> Result<()>,
) -> Result<()> {
    let mut buf = vec![0u8; CHUNK.min(end.saturating_sub(start) as usize)];
    let mut pos = start;
    while pos < end {
        cancel.check()?;
        let n = (end - pos).min(CHUNK as u64) as usize;
        read(t, pos, &mut buf[..n])?;
        f(pos, &buf[..n])?;
        pos += n as u64;
    }
    Ok(())
}

/// CRC-32 (ISO-HDLC) of a whole snapshot.
pub(crate) fn crc32(t: &Tree, cancel: &Cancel) -> Result<u32> {
    let mut h = crc32fast::Hasher::new();
    for_each_chunk(t, 0, t.len(), cancel, |_, c| {
        h.update(c);
        Ok(())
    })?;
    Ok(h.finalize())
}

/// Adler-32 state update (RFC 1950).
pub(crate) fn adler32_update(mut state: u32, data: &[u8]) -> u32 {
    const MOD: u32 = 65_521;
    let mut a = state & 0xFFFF;
    let mut b = state >> 16;
    for block in data.chunks(5552) {
        for &x in block {
            a += u32::from(x);
            b += a;
        }
        a %= MOD;
        b %= MOD;
    }
    state = (b << 16) | a;
    state
}

/// Adler-32 of `t[start..end]`.
pub(crate) fn adler32(t: &Tree, start: u64, end: u64, cancel: &Cancel) -> Result<u32> {
    let mut s = 1u32;
    for_each_chunk(t, start, end, cancel, |_, c| {
        s = adler32_update(s, c);
        Ok(())
    })?;
    Ok(s)
}

/// Buffered random access to a snapshot, efficient for mostly sequential
/// reads.
pub(crate) struct Cursor<'t> {
    tree: &'t Tree,
    buf: Vec<u8>,
    at: u64,
}

impl<'t> Cursor<'t> {
    pub(crate) fn new(tree: &'t Tree) -> Self {
        Self {
            tree,
            buf: Vec::new(),
            at: 0,
        }
    }

    /// Byte at `pos`, or `None` past the end.
    pub(crate) fn get(&mut self, pos: u64) -> Result<Option<u8>> {
        if pos >= self.tree.len() {
            return Ok(None);
        }
        if pos < self.at || pos >= self.at + self.buf.len() as u64 {
            let n = (self.tree.len() - pos).min(1 << 16) as usize;
            self.buf.resize(n, 0);
            read(self.tree, pos, &mut self.buf)?;
            self.at = pos;
        }
        Ok(Some(self.buf[(pos - self.at) as usize]))
    }

    /// Fills `out` with `t[pos..]`, using zero past the end.
    pub(crate) fn fill(&mut self, pos: u64, out: &mut [u8]) -> Result<()> {
        let len = self.tree.len();
        let avail = len.saturating_sub(pos).min(out.len() as u64) as usize;
        if avail > 0 {
            read(self.tree, pos, &mut out[..avail])?;
        }
        out[avail..].fill(0);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use proptest::prelude::*;

    #[test]
    fn adler_known_value() {
        assert_eq!(adler32_update(1, b"Wikipedia"), 0x11E6_0398);
        let t = tree(b"Wikipedia");
        assert_eq!(adler32(&t, 0, 9, &Cancel::new()).unwrap(), 0x11E6_0398);
    }

    #[test]
    fn crc_known_value() {
        let t = tree(b"123456789");
        assert_eq!(crc32(&t, &Cancel::new()).unwrap(), 0xCBF4_3926);
    }

    proptest! {
        #[test]
        fn mismatch_matches_naive(a in proptest::collection::vec(0u8..3, 0..300),
                                  b in proptest::collection::vec(0u8..3, 0..300)) {
            let naive = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
            prop_assert_eq!(mismatch(&a, &b), naive);
            let naive_rev = a.iter().rev().zip(b.iter().rev()).take_while(|(x, y)| x == y).count();
            prop_assert_eq!(mismatch_rev(&a, &b), naive_rev);
            let (ta, tb) = (tree(&a), tree(&b));
            let c = Cancel::new();
            let n = a.len().min(b.len()) as u64;
            prop_assert_eq!(common_prefix(&ta, 0, &tb, 0, n, &c).unwrap(), naive as u64);
            prop_assert_eq!(
                common_suffix(&ta, a.len() as u64, &tb, b.len() as u64, n, &c).unwrap(),
                naive_rev as u64
            );
        }
    }
}
