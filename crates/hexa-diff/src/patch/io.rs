//! Byte-level helpers for patch formats: a bounds-checked reader, varints
//! and a builder assembling the target from pieces.

use std::sync::Arc;

use hexa_core::piece::{Piece, Source};
use hexa_core::{Error, Result, Tree};

pub(crate) fn invalid(msg: impl Into<String>) -> Error {
    Error::Invalid(msg.into())
}

/// Bounds-checked reader over the patch bytes.
pub(crate) struct Reader<'p> {
    pub(crate) data: &'p [u8],
    pub(crate) pos: usize,
}

impl<'p> Reader<'p> {
    pub(crate) fn new(data: &'p [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub(crate) fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub(crate) fn bytes(&mut self, n: usize) -> Result<&'p [u8]> {
        if n > self.remaining() {
            return Err(invalid("patch is truncated"));
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    pub(crate) fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }

    pub(crate) fn be(&mut self, n: usize) -> Result<u64> {
        Ok(self
            .bytes(n)?
            .iter()
            .fold(0u64, |acc, &b| (acc << 8) | u64::from(b)))
    }

    pub(crate) fn u32_le(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(
            self.bytes(4)?.try_into().expect("4 bytes"),
        ))
    }

    /// byuu's varint (UPS, BPS).
    pub(crate) fn num(&mut self) -> Result<u64> {
        let mut data = 0u64;
        let mut shift = 1u64;
        loop {
            let x = self.u8()?;
            data = (u64::from(x & 0x7F))
                .checked_mul(shift)
                .and_then(|v| v.checked_add(data))
                .ok_or_else(|| invalid("number overflows"))?;
            if x & 0x80 != 0 {
                return Ok(data);
            }
            shift = shift
                .checked_mul(128)
                .ok_or_else(|| invalid("number overflows"))?;
            data = data
                .checked_add(shift)
                .ok_or_else(|| invalid("number overflows"))?;
        }
    }

    /// RFC 3284 big-endian base-128 integer.
    pub(crate) fn vcd_int(&mut self) -> Result<u64> {
        let mut v = 0u64;
        for _ in 0..10 {
            let x = self.u8()?;
            if v > u64::MAX >> 7 {
                return Err(invalid("integer overflows"));
            }
            v = (v << 7) | u64::from(x & 0x7F);
            if x & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(invalid("integer too long"))
    }
}

/// byuu's varint (UPS, BPS).
pub(crate) fn write_num(out: &mut Vec<u8>, mut n: u64) {
    loop {
        let x = (n & 0x7F) as u8;
        n >>= 7;
        if n == 0 {
            out.push(0x80 | x);
            break;
        }
        out.push(x);
        n -= 1;
    }
}

/// RFC 3284 big-endian base-128 integer.
pub(crate) fn write_vcd_int(out: &mut Vec<u8>, n: u64) {
    let mut tmp = [0u8; 10];
    let mut i = tmp.len();
    let mut v = n;
    loop {
        i -= 1;
        tmp[i] = (v & 0x7F) as u8 | if i == tmp.len() - 1 { 0 } else { 0x80 };
        v >>= 7;
        if v == 0 {
            break;
        }
    }
    out.extend_from_slice(&tmp[i..]);
}

pub(crate) fn vcd_int_len(n: u64) -> usize {
    let mut v = Vec::new();
    write_vcd_int(&mut v, n);
    v.len()
}

/// Pieces below this size are copied instead of referenced.
const SMALL: u64 = 64;
/// Size at which buffered bytes become a piece.
const FLUSH: usize = 1 << 20;

/// Assembles a target snapshot from source slices, patch literals, fills
/// and computed bytes, without materialising referenced data.
pub(crate) struct Builder {
    tree: Tree,
    pending: Vec<u8>,
    len: u64,
    patch: Arc<[u8]>,
}

impl Builder {
    pub(crate) fn new(patch: Arc<[u8]>) -> Self {
        Self {
            tree: Tree::new(),
            pending: Vec::new(),
            len: 0,
            patch,
        }
    }

    pub(crate) fn len(&self) -> u64 {
        self.len
    }

    fn flush(&mut self) {
        if !self.pending.is_empty() {
            let data = std::mem::take(&mut self.pending);
            self.tree = self.tree.concat(&Tree::from_piece(Piece::bytes(data)));
        }
    }

    pub(crate) fn push_bytes(&mut self, data: &[u8]) {
        self.pending.extend_from_slice(data);
        self.len += data.len() as u64;
        if self.pending.len() >= FLUSH {
            self.flush();
        }
    }

    pub(crate) fn push_byte(&mut self, b: u8) {
        self.pending.push(b);
        self.len += 1;
        if self.pending.len() >= FLUSH {
            self.flush();
        }
    }

    pub(crate) fn push_tree(&mut self, t: &Tree) -> Result<()> {
        if t.len() < SMALL {
            let mut buf = [0u8; SMALL as usize];
            let n = t.len() as usize;
            t.read(0, &mut buf[..n])?;
            self.push_bytes(&buf[..n]);
        } else {
            self.flush();
            self.len += t.len();
            self.tree = self.tree.concat(t);
        }
        Ok(())
    }

    /// Appends `src[off..off+len]`.
    pub(crate) fn push_source(&mut self, src: &Tree, off: u64, len: u64) -> Result<()> {
        let end = off
            .checked_add(len)
            .filter(|&e| e <= src.len())
            .ok_or_else(|| {
                invalid(format!(
                    "patch reads source bytes {off}+{len} past its end ({})",
                    src.len()
                ))
            })?;
        self.push_tree(&src.slice(off, end))
    }

    /// Appends `patch[off..off+len]` (validated by the caller's reader).
    pub(crate) fn push_patch(&mut self, off: usize, len: usize) {
        if (len as u64) < SMALL {
            let p = Arc::clone(&self.patch);
            self.push_bytes(&p[off..off + len]);
        } else {
            self.flush();
            self.len += len as u64;
            let piece = Piece {
                source: Source::Bytes(Arc::clone(&self.patch)),
                offset: off as u64,
                len: len as u64,
            };
            self.tree = self.tree.concat(&Tree::from_piece(piece));
        }
    }

    /// Appends `n` copies of `pattern` (cyclically).
    pub(crate) fn push_fill(&mut self, pattern: &[u8], n: u64) {
        if n == 0 {
            return;
        }
        if n < SMALL {
            for i in 0..n as usize {
                self.push_byte(pattern[i % pattern.len()]);
            }
        } else {
            self.flush();
            self.len += n;
            self.tree = self
                .tree
                .concat(&Tree::from_piece(Piece::fill(pattern.to_vec(), n)));
        }
    }

    /// Reads already produced output.
    pub(crate) fn read(&mut self, at: u64, buf: &mut [u8]) -> Result<()> {
        self.flush();
        self.tree.read(at, buf)?;
        Ok(())
    }

    /// Copies `len` bytes starting at output offset `from`; the ranges may
    /// overlap (the copy then repeats the last `len - from` bytes).
    pub(crate) fn copy_self(&mut self, from: u64, len: u64) -> Result<()> {
        if len == 0 {
            return Ok(());
        }
        if from >= self.len {
            return Err(invalid("copy from beyond the produced target"));
        }
        let mut src = from;
        let mut rem = len;
        while rem > 0 {
            let avail = self.len - src;
            if avail < rem && avail <= 4096 {
                let mut pat = vec![0u8; avail as usize];
                self.read(src, &mut pat)?;
                self.push_fill(&pat, rem);
                return Ok(());
            }
            let n = rem.min(avail);
            self.flush();
            let slice = self.tree.slice(src, src + n);
            self.push_tree(&slice)?;
            src += n;
            rem -= n;
        }
        Ok(())
    }

    /// The output produced so far.
    pub(crate) fn snapshot(&mut self) -> Tree {
        self.flush();
        self.tree.clone()
    }

    pub(crate) fn finish(mut self) -> Tree {
        self.flush();
        self.tree
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use proptest::prelude::*;

    #[test]
    fn byuu_numbers() {
        for n in [
            0u64,
            1,
            127,
            128,
            129,
            16_511,
            16_512,
            1 << 40,
            u64::MAX >> 8,
        ] {
            let mut v = Vec::new();
            write_num(&mut v, n);
            let mut r = Reader::new(&v);
            assert_eq!(r.num().unwrap(), n);
            assert_eq!(r.remaining(), 0);
        }
        let mut v = Vec::new();
        write_num(&mut v, 128);
        assert_eq!(v, [0x00, 0x80]);
        assert!(Reader::new(&[0u8; 20]).num().is_err());
    }

    #[test]
    fn vcd_numbers() {
        let mut v = Vec::new();
        write_vcd_int(&mut v, 123_456_789);
        assert_eq!(v, [0xBA, 0xEF, 0x9A, 0x15]);
        assert_eq!(Reader::new(&v).vcd_int().unwrap(), 123_456_789);
        assert_eq!(vcd_int_len(127), 1);
        assert_eq!(vcd_int_len(128), 2);
        assert!(Reader::new(&[0xFF; 11]).vcd_int().is_err());
    }

    proptest! {
        #[test]
        fn builder_matches_vec(ops in proptest::collection::vec((0u8..4, any::<u16>(), 1u16..300), 0..40)) {
            let patch: Arc<[u8]> = noise(3, 4096).into();
            let src = tree(&noise(4, 4096));
            let src_bytes = bytes(&src);
            let mut b = Builder::new(Arc::clone(&patch));
            let mut model = Vec::new();
            for (kind, at, len) in ops {
                let len = u64::from(len);
                match kind {
                    0 => {
                        let off = u64::from(at) % (4096 - len);
                        b.push_source(&src, off, len).unwrap();
                        model.extend_from_slice(&src_bytes[off as usize..(off + len) as usize]);
                    }
                    1 => {
                        let off = at as usize % (4096 - len as usize);
                        b.push_patch(off, len as usize);
                        model.extend_from_slice(&patch[off..off + len as usize]);
                    }
                    2 => {
                        b.push_fill(&[at as u8, 7], len);
                        for i in 0..len {
                            model.push(if i % 2 == 0 { at as u8 } else { 7 });
                        }
                    }
                    _ => {
                        if model.is_empty() { continue; }
                        let from = u64::from(at) % model.len() as u64;
                        b.copy_self(from, len).unwrap();
                        for i in 0..len {
                            model.push(model[(from + i) as usize]);
                        }
                    }
                }
                prop_assert_eq!(b.len(), model.len() as u64);
            }
            prop_assert_eq!(bytes(&b.finish()), model);
        }
    }
}
