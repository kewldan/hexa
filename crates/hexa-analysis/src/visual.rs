//! Byte-level visualisations (ANL-10): digraph (consecutive byte pairs) and a
//! Hilbert-curve layout with per-byte colour classes in the style of binvis.

use hexa_core::task::Progress;
use hexa_core::{Cancel, Result, Tree};
use serde::{Deserialize, Serialize};

use crate::util::{self, CHUNK, invalid};

/// Counts of consecutive byte pairs, downsampled to `size × size` cells.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Digraph {
    /// Cells per axis (power of two, 1…256).
    pub size: u32,
    /// Byte values per cell along each axis (`256 / size`).
    pub cell: u32,
    /// Row-major counts: row = first byte / cell, column = second byte / cell.
    pub counts: Vec<u64>,
    /// Largest cell count (for log scaling).
    pub max: u64,
    /// Number of pairs counted.
    pub pairs: u64,
}

fn check_size(size: u32) -> Result<u32> {
    if size == 0 || size > 256 || !size.is_power_of_two() {
        return Err(invalid("digraph size must be a power of two in 1…256"));
    }
    Ok(256 / size)
}

#[derive(Debug)]
struct DigraphAcc {
    size: u32,
    shift: u32,
    counts: Vec<u64>,
    prev: Option<u8>,
    pairs: u64,
}

impl DigraphAcc {
    fn new(size: u32) -> Result<Self> {
        let cell = check_size(size)?;
        Ok(Self {
            size,
            shift: cell.trailing_zeros(),
            counts: vec![0; (size * size) as usize],
            prev: None,
            pairs: 0,
        })
    }

    fn feed(&mut self, data: &[u8]) {
        let mut prev = self.prev;
        for &b in data {
            if let Some(p) = prev {
                let r = u32::from(p) >> self.shift;
                let c = u32::from(b) >> self.shift;
                self.counts[(r * self.size + c) as usize] += 1;
                self.pairs += 1;
            }
            prev = Some(b);
        }
        self.prev = prev;
    }

    fn finish(self) -> Digraph {
        Digraph {
            size: self.size,
            cell: 256 / self.size,
            max: self.counts.iter().copied().max().unwrap_or(0),
            counts: self.counts,
            pairs: self.pairs,
        }
    }
}

/// Digraph of a buffer.
pub fn digraph_bytes(data: &[u8], size: u32) -> Result<Digraph> {
    let mut acc = DigraphAcc::new(size)?;
    acc.feed(data);
    Ok(acc.finish())
}

/// Digraph of `[start, end)` of a snapshot.
pub fn digraph(
    tree: &Tree,
    start: u64,
    end: u64,
    size: u32,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<Digraph> {
    let mut acc = DigraphAcc::new(size)?;
    util::scan(tree, start, end, CHUNK, cancel, progress, |_, c| {
        acc.feed(c);
        Ok(true)
    })?;
    Ok(acc.finish())
}

fn rot(n: u32, x: &mut u32, y: &mut u32, rx: u32, ry: u32) {
    if ry == 0 {
        if rx == 1 {
            *x = n - 1 - *x;
            *y = n - 1 - *y;
        }
        std::mem::swap(x, y);
    }
}

/// Hilbert curve of order `order` (side `2^order`): curve index → `(x, y)`.
pub fn hilbert_d2xy(order: u32, d: u64) -> (u32, u32) {
    let n = 1u32 << order;
    let (mut x, mut y) = (0u32, 0u32);
    let mut t = d;
    let mut s = 1u32;
    while s < n {
        let rx = (1 & (t / 2)) as u32;
        let ry = (1 & (t ^ u64::from(rx))) as u32;
        rot(s, &mut x, &mut y, rx, ry);
        x += s * rx;
        y += s * ry;
        t /= 4;
        s *= 2;
    }
    (x, y)
}

/// Inverse of [`hilbert_d2xy`].
pub fn hilbert_xy2d(order: u32, x: u32, y: u32) -> u64 {
    let n = 1u32 << order;
    let (mut x, mut y) = (x, y);
    let mut d = 0u64;
    let mut s = n / 2;
    while s > 0 {
        let rx = u32::from(x & s > 0);
        let ry = u32::from(y & s > 0);
        d += u64::from(s) * u64::from(s) * u64::from((3 * rx) ^ ry);
        rot(n, &mut x, &mut y, rx, ry);
        s /= 2;
    }
    d
}

/// Colour class of a byte (binvis "class" scheme).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[repr(u8)]
pub enum ByteClass {
    /// `0x00`.
    Zero = 0,
    /// Control characters `0x01…0x1F`, `0x7F` (except whitespace).
    Low = 1,
    /// Printable ASCII and `\t`, `\n`, `\r`.
    Ascii = 2,
    /// `0x80…0xFE`.
    High = 3,
    /// `0xFF`.
    Ff = 4,
}

/// Cell value of [`HilbertMap::cells`] past the end of the data.
pub const EMPTY_CELL: u8 = 255;

/// Class of one byte.
pub fn byte_class(b: u8) -> ByteClass {
    match b {
        0x00 => ByteClass::Zero,
        0xFF => ByteClass::Ff,
        b'\t' | b'\n' | b'\r' | 0x20..=0x7E => ByteClass::Ascii,
        0x01..=0x1F | 0x7F => ByteClass::Low,
        _ => ByteClass::High,
    }
}

/// Default RGB colour of a class (black, green, blue, red, white).
pub fn class_color(class: ByteClass) -> [u8; 3] {
    match class {
        ByteClass::Zero => [0, 0, 0],
        ByteClass::Low => [77, 175, 74],
        ByteClass::Ascii => [55, 126, 184],
        ByteClass::High => [228, 26, 28],
        ByteClass::Ff => [255, 255, 255],
    }
}

/// Hilbert layout of a range: cell `d` of the curve covers
/// `[start + d * bytes_per_cell, …)` and holds the dominant [`ByteClass`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HilbertMap {
    pub order: u32,
    /// `2^order`.
    pub side: u32,
    pub start: u64,
    pub bytes_per_cell: u64,
    /// Row-major (`y * side + x`) class codes (`ByteClass as u8`) or
    /// [`EMPTY_CELL`].
    pub cells: Vec<u8>,
}

impl HilbertMap {
    /// Pixels as RGBA (empty cells transparent).
    pub fn to_rgba(&self) -> Vec<u8> {
        let classes = [
            ByteClass::Zero,
            ByteClass::Low,
            ByteClass::Ascii,
            ByteClass::High,
            ByteClass::Ff,
        ];
        self.cells
            .iter()
            .flat_map(|&c| match classes.get(usize::from(c)) {
                Some(&cls) => {
                    let [r, g, b] = class_color(cls);
                    [r, g, b, 255]
                }
                None => [0, 0, 0, 0],
            })
            .collect()
    }

    /// File offset shown at pixel `(x, y)`, if any.
    pub fn offset_at(&self, x: u32, y: u32) -> Option<u64> {
        if x >= self.side || y >= self.side {
            return None;
        }
        let i = y as usize * self.side as usize + x as usize;
        (self.cells[i] != EMPTY_CELL)
            .then(|| self.start + hilbert_xy2d(self.order, x, y) * self.bytes_per_cell)
    }
}

/// Builds the Hilbert layout of `[start, end)` with side `2^order`
/// (`order` 1…12).
pub fn hilbert_map(
    tree: &Tree,
    start: u64,
    end: u64,
    order: u32,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<HilbertMap> {
    if !(1..=12).contains(&order) {
        return Err(invalid("Hilbert order must be 1…12"));
    }
    let (start, end) = util::clamp(tree, start, end);
    let side = 1u32 << order;
    let cells_total = u64::from(side) * u64::from(side);
    let bpc = (end - start).div_ceil(cells_total).max(1);
    let mut cells = vec![EMPTY_CELL; cells_total as usize];
    let mut counts = [0u64; 5];
    let mut d = 0u64;
    let mut in_cell = 0u64;
    let mut flush = |d: u64, counts: &mut [u64; 5]| {
        let best = (0..5)
            .max_by_key(|&i| (counts[i], std::cmp::Reverse(i)))
            .unwrap_or(0);
        let (x, y) = hilbert_d2xy(order, d);
        cells[(y * side + x) as usize] = best as u8;
        *counts = [0; 5];
    };
    util::scan(tree, start, end, CHUNK, cancel, progress, |_, chunk| {
        for &b in chunk {
            counts[byte_class(b) as usize] += 1;
            in_cell += 1;
            if in_cell == bpc {
                flush(d, &mut counts);
                d += 1;
                in_cell = 0;
            }
        }
        Ok(true)
    })?;
    if in_cell > 0 {
        flush(d, &mut counts);
    }
    Ok(HilbertMap {
        order,
        side,
        start,
        bytes_per_cell: bpc,
        cells,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{noop, tree};

    #[test]
    fn digraph_counts_pairs() {
        let d = digraph_bytes(b"abab", 256).unwrap();
        assert_eq!(d.pairs, 3);
        assert_eq!(d.counts[usize::from(b'a') * 256 + usize::from(b'b')], 2);
        assert_eq!(d.counts[usize::from(b'b') * 256 + usize::from(b'a')], 1);
        assert_eq!(d.max, 2);
        let small = digraph_bytes(&[0x00, 0xFF, 0x10], 4).unwrap();
        assert_eq!(small.cell, 64);
        assert_eq!(
            small.counts,
            vec![0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0]
        );
        assert!(digraph_bytes(b"", 3).is_err());
        // Pairs that straddle chunk boundaries are counted when streaming.
        let data: Vec<u8> = (0..10_000_000u32).map(|i| (i % 251) as u8).collect();
        let a = digraph(
            &tree(&data),
            0,
            data.len() as u64,
            16,
            &Cancel::new(),
            &noop,
        )
        .unwrap();
        assert_eq!(a, digraph_bytes(&data, 16).unwrap());
    }

    #[test]
    fn hilbert_roundtrip_and_adjacency() {
        for order in 1..=6 {
            let n = 1u64 << (2 * order);
            let mut prev = hilbert_d2xy(order, 0);
            assert_eq!(prev, (0, 0));
            for d in 0..n {
                let (x, y) = hilbert_d2xy(order, d);
                assert_eq!(hilbert_xy2d(order, x, y), d);
                if d > 0 {
                    assert_eq!(x.abs_diff(prev.0) + y.abs_diff(prev.1), 1);
                }
                prev = (x, y);
            }
        }
        // Order 1: (0,0) (0,1) (1,1) (1,0).
        let pts: Vec<_> = (0..4).map(|d| hilbert_d2xy(1, d)).collect();
        assert_eq!(pts, [(0, 0), (0, 1), (1, 1), (1, 0)]);
    }

    #[test]
    fn classes_and_map() {
        assert_eq!(byte_class(0), ByteClass::Zero);
        assert_eq!(byte_class(b'\n'), ByteClass::Ascii);
        assert_eq!(byte_class(0x07), ByteClass::Low);
        assert_eq!(byte_class(0x80), ByteClass::High);
        assert_eq!(byte_class(0xFF), ByteClass::Ff);
        let mut data = vec![0u8; 8];
        data.extend(b"textTEXT");
        data.extend([0xFF; 6]);
        let m = hilbert_map(&tree(&data), 0, 22, 1, &Cancel::new(), &noop).unwrap();
        assert_eq!(m.bytes_per_cell, 6);
        // d=0 → (0,0): zeros; d=1 → (0,1): 2 zeros + 4 text; d=2 → (1,1)
        // text; d=3 → (1,0): 4 × 0xFF.
        assert_eq!(m.cells, vec![0, 4, 2, 2]);
        assert_eq!(m.offset_at(1, 0), Some(18));
        assert_eq!(m.to_rgba().len(), 16);
        let sparse = hilbert_map(&tree(b"ab"), 0, 2, 2, &Cancel::new(), &noop).unwrap();
        assert_eq!(
            sparse
                .cells
                .iter()
                .filter(|c| **c == EMPTY_CELL)
                .map(|_| 1)
                .sum::<usize>(),
            14
        );
        assert_eq!(sparse.offset_at(3, 3), None);
        assert!(hilbert_map(&tree(b"ab"), 0, 2, 0, &Cancel::new(), &noop).is_err());
    }
}
