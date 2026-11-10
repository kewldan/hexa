//! DIFF-02: shift-aware diff.
//!
//! 1. Content-defined anchors: a gear rolling hash over a 64-byte window is
//!    computed for every position of both inputs; positions whose hash has
//!    its top `bits` bits clear become anchor candidates. The same content
//!    therefore produces the same anchors in both inputs regardless of
//!    shifts. Only hashes unique in both inputs are kept, and a longest
//!    increasing subsequence turns them into a monotonic alignment.
//! 2. Every anchor is extended to a maximal match in both directions.
//! 3. Gaps between matches are trimmed and diffed with Myers' algorithm when
//!    small, re-anchored at a finer granularity when large, and compared
//!    byte-by-byte as a last resort.
//!
//! The anchor tables are capped (the anchor density is lowered when they
//! fill up), so memory stays bounded for multi-gigabyte inputs.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use hexa_core::{Cancel, Result, Tree};

use crate::chunk::{CHUNK, Tracker, common_prefix, common_suffix, for_each_chunk, read};
use crate::compare::compare;
use crate::myers::{self, Edit};
use crate::sink::Sink;

/// Gaps up to this many bytes (A + B) go through Myers.
const MYERS_MAX_BYTES: u64 = 128 << 10;
/// Edit distance bound for Myers.
const MYERS_MAX_D: usize = 1024;
/// Gear hash window (bytes that influence a hash value).
const WINDOW: u64 = 64;
/// Desired number of anchors per input.
const TARGET_ANCHORS: u64 = 1 << 18;
/// Hard bound on anchor table entries.
const MAX_TABLE: usize = 1 << 19;
/// Minimum verified match to accept an anchor.
const MIN_MATCH: u64 = 32;
/// Recursion depth for re-anchoring large gaps.
const MAX_DEPTH: u32 = 2;

const fn splitmix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

const GEAR: [u64; 256] = {
    let mut t = [0u64; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = splitmix(i as u64 ^ 0x6865_7861);
        i += 1;
    }
    t
};

/// Hash value → position, or `DUP` when the hash occurs more than once.
const DUP: u64 = u64::MAX;

struct Table {
    map: HashMap<u64, u64>,
    bits: u32,
}

impl Table {
    fn accepts(bits: u32, h: u64) -> bool {
        h >> (64 - bits) == 0
    }

    fn insert(&mut self, h: u64, pos: u64) {
        match self.map.entry(h) {
            Entry::Occupied(mut e) => {
                e.insert(DUP);
            }
            Entry::Vacant(e) => {
                e.insert(pos);
            }
        }
        while self.map.len() > MAX_TABLE && self.bits < 63 {
            self.bits += 1;
            let bits = self.bits;
            self.map.retain(|&h, _| Self::accepts(bits, h));
        }
    }
}

pub(crate) struct ShiftDiff<'a, 'p> {
    a: &'a Tree,
    b: &'a Tree,
    cancel: &'a Cancel,
    prog: &'a mut Tracker<'p>,
}

impl<'a, 'p> ShiftDiff<'a, 'p> {
    pub(crate) fn new(
        a: &'a Tree,
        b: &'a Tree,
        cancel: &'a Cancel,
        prog: &'a mut Tracker<'p>,
    ) -> Self {
        Self { a, b, cancel, prog }
    }

    pub(crate) fn run(mut self, sink: &mut Sink<'_>) -> Result<()> {
        let (la, lb) = (self.a.len(), self.b.len());
        self.gap(sink, 0, la, 0, lb, 0)
    }

    /// Diffs `a[a0..a1]` against `b[b0..b1]`.
    fn gap(
        &mut self,
        sink: &mut Sink<'_>,
        a0: u64,
        a1: u64,
        b0: u64,
        b1: u64,
        depth: u32,
    ) -> Result<()> {
        self.cancel.check()?;
        let max = (a1 - a0).min(b1 - b0);
        let pre = common_prefix(self.a, a0, self.b, b0, max, self.cancel)?;
        let suf = common_suffix(self.a, a1, self.b, b1, max - pre, self.cancel)?;
        sink.equal(pre)?;
        self.prog.add(2 * pre);
        let (a0, b0, a1, b1) = (a0 + pre, b0 + pre, a1 - suf, b1 - suf);
        let (la, lb) = (a1 - a0, b1 - b0);
        if la == 0 || lb == 0 {
            sink.diff(la, lb)?;
            self.prog.add(la + lb);
        } else if la + lb <= MYERS_MAX_BYTES {
            self.myers(sink, a0, a1, b0, b1)?;
        } else if depth >= MAX_DEPTH || !self.anchored(sink, a0, a1, b0, b1, depth + 1)? {
            compare(self.a, a0..a1, self.b, b0..b1, sink, self.cancel, self.prog)?;
            self.prog.add(la.min(lb));
        }
        sink.equal(suf)?;
        self.prog.add(2 * suf);
        Ok(())
    }

    fn myers(&mut self, sink: &mut Sink<'_>, a0: u64, a1: u64, b0: u64, b1: u64) -> Result<()> {
        let mut ba = vec![0; (a1 - a0) as usize];
        let mut bb = vec![0; (b1 - b0) as usize];
        read(self.a, a0, &mut ba)?;
        read(self.b, b0, &mut bb)?;
        if let Some(edits) = myers::diff(&ba, &bb, MYERS_MAX_D) {
            for e in edits {
                match e {
                    Edit::Equal(n) => sink.equal(n as u64)?,
                    Edit::Delete(n) => sink.diff(n as u64, 0)?,
                    Edit::Insert(n) => sink.diff(0, n as u64)?,
                }
            }
            self.prog.add((a1 - a0) + (b1 - b0));
        } else {
            compare(self.a, a0..a1, self.b, b0..b1, sink, self.cancel, self.prog)?;
            self.prog.add((a1 - a0).min(b1 - b0));
        }
        Ok(())
    }

    fn scan(&mut self, t: &Tree, start: u64, end: u64, bits: u32) -> Result<Table> {
        let mut table = Table {
            map: HashMap::new(),
            bits,
        };
        let mut h = 0u64;
        for_each_chunk(t, start, end, self.cancel, |pos, chunk| {
            for (i, &x) in chunk.iter().enumerate() {
                h = (h << 1).wrapping_add(GEAR[x as usize]);
                let p = pos + i as u64;
                if p + 1 >= start + WINDOW && Table::accepts(table.bits, h) {
                    table.insert(h, p + 1 - WINDOW);
                }
            }
            self.prog.add(chunk.len() as u64);
            Ok(())
        })?;
        Ok(table)
    }

    /// Anchors `a[a0..a1]` against `b[b0..b1]` and diffs the gaps. Returns
    /// `false` (without touching the sink) when no anchor was found.
    fn anchored(
        &mut self,
        sink: &mut Sink<'_>,
        a0: u64,
        a1: u64,
        b0: u64,
        b1: u64,
        depth: u32,
    ) -> Result<bool> {
        let longest = (a1 - a0).max(b1 - b0);
        let spacing = (longest / TARGET_ANCHORS).max(WINDOW).next_power_of_two();
        let bits = spacing.trailing_zeros();
        let ta = self.scan(self.a, a0, a1, bits)?;
        let tb = self.scan(self.b, b0, b1, bits)?;
        let mut pairs: Vec<(u64, u64)> = tb
            .map
            .iter()
            .filter(|&(_, &pb)| pb != DUP)
            .filter_map(|(h, &pb)| match ta.map.get(h) {
                Some(&pa) if pa != DUP => Some((pa, pb)),
                _ => None,
            })
            .collect();
        drop((ta, tb));
        if pairs.is_empty() {
            return Ok(false);
        }
        pairs.sort_unstable_by_key(|&(_, pb)| pb);
        let chain = lis(&pairs);
        let (mut ca, mut cb) = (a0, b0);
        let mut any = false;
        for (pa, pb) in chain {
            self.cancel.check()?;
            if pa < ca || pb < cb {
                continue;
            }
            let fwd = common_prefix(self.a, pa, self.b, pb, (a1 - pa).min(b1 - pb), self.cancel)?;
            if fwd < MIN_MATCH {
                continue;
            }
            let back = common_suffix(self.a, pa, self.b, pb, (pa - ca).min(pb - cb), self.cancel)?;
            self.gap(sink, ca, pa - back, cb, pb - back, depth)?;
            sink.equal(back + fwd)?;
            self.prog.add(2 * (back + fwd));
            ca = pa + fwd;
            cb = pb + fwd;
            any = true;
        }
        if !any {
            // Nothing was emitted yet, so the caller can still fall back.
            return Ok(false);
        }
        self.gap(sink, ca, a1, cb, b1, depth)?;
        Ok(true)
    }
}

/// Longest strictly increasing subsequence of `pairs[i].0` (pairs are
/// sorted by `.1`, which is unique).
fn lis(pairs: &[(u64, u64)]) -> Vec<(u64, u64)> {
    let mut tails: Vec<usize> = Vec::new();
    let mut prev = vec![usize::MAX; pairs.len()];
    for (i, &(a, _)) in pairs.iter().enumerate() {
        let k = tails.partition_point(|&t| pairs[t].0 < a);
        if k > 0 {
            prev[i] = tails[k - 1];
        }
        if k == tails.len() {
            tails.push(i);
        } else {
            tails[k] = i;
        }
    }
    let mut out = Vec::with_capacity(tails.len());
    let mut cur = tails.last().copied().unwrap_or(usize::MAX);
    while cur != usize::MAX {
        out.push(pairs[cur]);
        cur = prev[cur];
    }
    out.reverse();
    out
}

const _: () = assert!(CHUNK as u64 >= WINDOW);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merge::apply_regions;
    use crate::testutil::*;
    use crate::{DiffOptions, DiffResult, Region, RegionKind, Span, diff};
    use hexa_core::piece::Piece;
    use proptest::prelude::*;

    fn shift(a: &Tree, b: &Tree, eq: bool) -> DiffResult {
        let opts = DiffOptions {
            include_equal: eq,
            max_regions: None,
            ..DiffOptions::default()
        };
        diff(a, b, &opts, &Cancel::new(), &noprog).unwrap()
    }

    fn check_tiling(a: &Tree, b: &Tree, r: &DiffResult) {
        let (mut pa, mut pb) = (0, 0);
        for reg in &r.regions {
            assert_eq!((reg.a.start, reg.b.start), (pa, pb));
            if reg.kind == RegionKind::Equal {
                assert!(
                    crate::chunk::ranges_equal(a, pa, b, pb, reg.a.len, &Cancel::new()).unwrap()
                );
                assert_eq!(reg.a.len, reg.b.len);
            } else {
                assert_eq!(reg.kind, RegionKind::of_lengths(reg.a.len, reg.b.len));
            }
            pa = reg.a.end();
            pb = reg.b.end();
        }
        assert_eq!((pa, pb), (a.len(), b.len()));
    }

    #[test]
    fn lis_works() {
        let p = [(5, 0), (1, 1), (2, 2), (9, 3), (3, 4)];
        assert_eq!(lis(&p), vec![(1, 1), (2, 2), (3, 4)]);
        assert_eq!(lis(&[]), vec![]);
    }

    #[test]
    fn small_insertion_is_found() {
        let a = b"the quick brown fox jumps over the lazy dog".to_vec();
        let b = b"the quick brown cat fox jumps over the lazy dog!".to_vec();
        let r = shift(&tree(&a), &tree(&b), false);
        assert_eq!(
            r.regions,
            vec![
                Region {
                    a: Span::new(16, 0),
                    b: Span::new(16, 4),
                    kind: RegionKind::Inserted
                },
                Region {
                    a: Span::new(43, 0),
                    b: Span::new(47, 1),
                    kind: RegionKind::Inserted
                },
            ]
        );
    }

    #[test]
    fn large_shifts_use_anchors() {
        // 3 MiB of noise with an insertion, a deletion and a replacement.
        let a: Vec<u8> = noise(11, 3 << 20).iter().map(|x| x & 0x7F).collect();
        let mut b = a.clone();
        b.splice(2_500_000..2_500_000, noise(12, 777));
        b.drain(1_000_000..1_000_300);
        for x in &mut b[200_000..200_010] {
            *x |= 0x80;
        }
        let (ta, tb) = (tree(&a), tree(&b));
        let r = shift(&ta, &tb, true);
        check_tiling(&ta, &tb, &r);
        let diffs: Vec<_> = r
            .regions
            .iter()
            .filter(|r| r.kind != RegionKind::Equal)
            .collect();
        assert_eq!(diffs.len(), 3, "{diffs:?}");
        assert_eq!(diffs[0].a, Span::new(200_000, 10));
        assert_eq!(diffs[1].kind, RegionKind::Deleted);
        assert_eq!(diffs[1].a.len, 300);
        assert_eq!(diffs[2].kind, RegionKind::Inserted);
        assert_eq!(diffs[2].b.len, 777);
        assert!(r.stats.match_percent > 99.9);
    }

    #[test]
    fn huge_virtual_inputs_stream() {
        // 64 MiB generated on the fly: never materialised by the diff.
        let len = 64u64 << 20;
        let a = Tree::from_piece(Piece::random(7, len));
        let b = a.insert(40 << 20, &tree(b"inserted!"));
        let b = b.remove(10 << 20, (10 << 20) + 5);
        let r = shift(&a, &b, false);
        assert_eq!(r.regions.len(), 2, "{:?}", r.regions);
        assert_eq!(r.regions[0].kind, RegionKind::Deleted);
        assert_eq!(r.regions[1].kind, RegionKind::Inserted);
        assert_eq!(r.regions[1].b.len, 9);
    }

    #[test]
    fn unrelated_and_degenerate_inputs() {
        let a = tree(&noise(1, 300_000));
        let b = tree(&noise(2, 200_000));
        let r = shift(&a, &b, true);
        check_tiling(&a, &b, &r);
        let z = tree(&vec![0u8; 500_000]);
        let z2 = tree(&vec![0u8; 400_000]);
        let r = shift(&z, &z2, true);
        check_tiling(&z, &z2, &r);
        assert_eq!(r.stats.equal_bytes, 400_000);
        let r = shift(&Tree::new(), &z2, true);
        check_tiling(&Tree::new(), &z2, &r);
    }

    fn mutate(base: &[u8], edits: &[(u16, u8, Vec<u8>)]) -> Vec<u8> {
        let mut out = base.to_vec();
        for (pos, del, ins) in edits {
            let at = *pos as usize % (out.len() + 1);
            let end = (at + *del as usize % 40).min(out.len());
            out.splice(at..end, ins.iter().copied());
        }
        out
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]
        #[test]
        fn regions_reconstruct_b(seed in any::<u64>(), len in 0usize..5000,
                                 edits in proptest::collection::vec(
                                     (any::<u16>(), any::<u8>(),
                                      proptest::collection::vec(any::<u8>(), 0..50)), 0..8),
                                 gap in 0u64..8) {
            let a = noise(seed, len);
            let b = mutate(&a, &edits);
            let (ta, tb) = (tree(&a), tree(&b));
            let r = shift(&ta, &tb, true);
            check_tiling(&ta, &tb, &r);
            let opts = DiffOptions { merge_gap: gap, max_regions: None, ..DiffOptions::default() };
            let r = diff(&ta, &tb, &opts, &Cancel::new(), &noprog).unwrap();
            let rebuilt = apply_regions(&ta, &tb, &r.regions).unwrap();
            prop_assert_eq!(bytes(&rebuilt), b);
        }
    }
}
