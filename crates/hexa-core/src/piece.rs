//! Pieces: `{source, offset, length}` slices of the document (CORE-04, CORE-07).

use std::fmt;
use std::io;
use std::sync::Arc;

use crate::provider::Provider;

/// Where the bytes of a piece come from.
#[derive(Clone)]
pub enum Source {
    /// A slice of a provider (the opened file, an inserted file …).
    Provider(Arc<dyn Provider>),
    /// Bytes typed or pasted by the user.
    Bytes(Arc<[u8]>),
    /// A repeating pattern, never materialised (CORE-07).
    Fill(Arc<[u8]>),
    /// Pseudo-random bytes from a seed, never materialised (CORE-07).
    Random(u64),
}

impl fmt::Debug for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider(p) => write!(f, "Provider({})", p.name()),
            Self::Bytes(b) => write!(f, "Bytes({})", b.len()),
            Self::Fill(p) => write!(f, "Fill({p:02x?})"),
            Self::Random(s) => write!(f, "Random({s:#x})"),
        }
    }
}

impl Source {
    pub fn same_as(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Provider(a), Self::Provider(b)) => {
                std::ptr::addr_eq(Arc::as_ptr(a), Arc::as_ptr(b))
            }
            (Self::Bytes(a), Self::Bytes(b)) | (Self::Fill(a), Self::Fill(b)) => Arc::ptr_eq(a, b),
            (Self::Random(a), Self::Random(b)) => a == b,
            _ => false,
        }
    }

    pub fn is_provider(&self, p: &Arc<dyn Provider>) -> bool {
        matches!(self, Self::Provider(q) if std::ptr::addr_eq(Arc::as_ptr(q), Arc::as_ptr(p)))
    }
}

/// A run of bytes taken from `source` starting at `offset`.
#[derive(Clone, Debug)]
pub struct Piece {
    pub source: Source,
    pub offset: u64,
    pub len: u64,
}

impl Piece {
    pub fn bytes(data: impl Into<Arc<[u8]>>) -> Self {
        let data: Arc<[u8]> = data.into();
        let len = data.len() as u64;
        Self {
            source: Source::Bytes(data),
            offset: 0,
            len,
        }
    }

    pub fn provider(p: Arc<dyn Provider>, offset: u64, len: u64) -> Self {
        Self {
            source: Source::Provider(p),
            offset,
            len,
        }
    }

    pub fn fill(pattern: impl Into<Arc<[u8]>>, len: u64) -> Self {
        Self {
            source: Source::Fill(pattern.into()),
            offset: 0,
            len,
        }
    }

    pub fn random(seed: u64, len: u64) -> Self {
        Self {
            source: Source::Random(seed),
            offset: 0,
            len,
        }
    }

    /// Splits at `at` (relative, `0 < at < len`).
    pub fn split(&self, at: u64) -> (Self, Self) {
        debug_assert!(at > 0 && at < self.len);
        let left = Self {
            source: self.source.clone(),
            offset: self.offset,
            len: at,
        };
        let right = Self {
            source: self.source.clone(),
            offset: self.offset + at,
            len: self.len - at,
        };
        (left, right)
    }

    /// Sub-piece `[from, from + len)` relative to this piece.
    pub fn slice(&self, from: u64, len: u64) -> Self {
        debug_assert!(from + len <= self.len);
        Self {
            source: self.source.clone(),
            offset: self.offset + from,
            len,
        }
    }

    /// Whether `next` continues this piece in its source, so both can merge.
    pub fn continues_with(&self, next: &Self) -> bool {
        self.source.same_as(&next.source) && self.offset + self.len == next.offset
    }

    /// Reads `buf.len()` bytes starting `from` bytes into the piece.
    pub fn read(&self, from: u64, buf: &mut [u8]) -> io::Result<()> {
        debug_assert!(from + buf.len() as u64 <= self.len);
        let at = self.offset + from;
        match &self.source {
            Source::Provider(p) => p.read_at(at, buf),
            Source::Bytes(b) => {
                let s = at as usize;
                buf.copy_from_slice(&b[s..s + buf.len()]);
                Ok(())
            }
            Source::Fill(pattern) => {
                let n = pattern.len() as u64;
                for (i, out) in buf.iter_mut().enumerate() {
                    *out = pattern[((at + i as u64) % n) as usize];
                }
                Ok(())
            }
            Source::Random(seed) => {
                for (i, out) in buf.iter_mut().enumerate() {
                    *out = random_byte(*seed, at + i as u64);
                }
                Ok(())
            }
        }
    }
}

/// Deterministic, position-addressable pseudo-random byte stream.
pub fn random_byte(seed: u64, pos: u64) -> u8 {
    let word = splitmix64(seed ^ (pos / 8).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    (word >> ((pos % 8) * 8)) as u8
}

fn splitmix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_and_random_are_position_addressable() {
        let p = Piece::fill(vec![1u8, 2, 3], 10);
        let mut buf = [0; 4];
        p.read(4, &mut buf).unwrap();
        assert_eq!(buf, [2, 3, 1, 2]);

        let r = Piece::random(42, 100);
        let mut whole = [0; 100];
        r.read(0, &mut whole).unwrap();
        let mut part = [0; 10];
        r.slice(37, 20).read(3, &mut part).unwrap();
        assert_eq!(part, whole[40..50]);
    }
}
