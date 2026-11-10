//! DIFF-03: comparison of several inputs against a common base.

use hexa_core::task::Progress;
use hexa_core::{Cancel, Result, Tree};
use serde::{Deserialize, Serialize};

use crate::stats::DiffStats;
use crate::{DiffOptions, Region, RegionKind, Span, diff_each};

/// What one input holds for a region of the base.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NwaySpan {
    pub span: Span,
    pub kind: RegionKind,
}

/// One region of the base and the matching span of every other input.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NwayRegion {
    pub base: Span,
    /// One entry per compared input, in the order they were given.
    pub inputs: Vec<NwaySpan>,
    /// Number of inputs that differ from the base here.
    pub differing: u32,
}

/// Result of [`diff_nway`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NwayResult {
    pub regions: Vec<NwayRegion>,
    /// Pairwise statistics base ↔ input, per input.
    pub stats: Vec<DiffStats>,
    /// `true` when `max_regions` cut the region list.
    pub truncated: bool,
}

/// DIFF-03: diffs every input against `base` (DIFF-02 or DIFF-01 according
/// to `opts.mode`) and splits the base into regions where the set of
/// differing inputs is constant. Overlapping differences of several inputs
/// are united into one region so every input's span is well defined.
pub fn diff_nway(
    base: &Tree,
    inputs: &[Tree],
    opts: &DiffOptions,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<NwayResult> {
    let n = inputs.len() as u64;
    let inner = DiffOptions {
        include_equal: false,
        ..opts.clone()
    };
    let mut per_input = Vec::with_capacity(inputs.len());
    let mut stats = Vec::with_capacity(inputs.len());
    for (i, input) in inputs.iter().enumerate() {
        let scale = |done: u64, total: u64| {
            let frac = crate::chunk::permille(done, total);
            progress(i as u64 * 1000 + frac, n * 1000);
        };
        let mut regions = Vec::new();
        let s = diff_each(base, input, &inner, cancel, &scale, &mut |r| {
            regions.push(r);
            Ok(())
        })?;
        per_input.push(regions);
        stats.push(s);
    }
    let mut regions = cluster(base.len(), &per_input, opts.include_equal);
    let limit = opts.max_regions.unwrap_or(usize::MAX);
    let truncated = regions.len() > limit;
    regions.truncate(limit);
    progress(n * 1000, n * 1000);
    Ok(NwayResult {
        regions,
        stats,
        truncated,
    })
}

/// Maps base offsets to one input's offsets through its sorted non-equal
/// regions; queries must be non-decreasing.
struct Mapper<'r> {
    regions: &'r [Region],
    idx: usize,
    delta: i128,
}

impl Mapper<'_> {
    fn advance(&mut self, done: impl Fn(&Region) -> bool) -> usize {
        let start = self.idx;
        while let Some(r) = self.regions.get(self.idx)
            && done(r)
        {
            self.delta += i128::from(r.b.len) - i128::from(r.a.len);
            self.idx += 1;
        }
        self.idx - start
    }

    /// Position in the input just before anything inserted at `p`.
    fn left(&mut self, p: u64) -> u64 {
        self.advance(|r| r.a.end() < p || (r.a.end() == p && r.a.len > 0));
        (i128::from(p) + self.delta) as u64
    }

    /// Position after everything inserted at `p`, plus how many regions were
    /// passed on the way.
    fn right(&mut self, p: u64) -> (u64, usize) {
        let k = self.advance(|r| r.a.end() <= p);
        ((i128::from(p) + self.delta) as u64, k)
    }
}

/// Unites the non-equal regions of all inputs (base coordinates in `a`).
pub(crate) fn cluster(
    base_len: u64,
    per_input: &[Vec<Region>],
    include_equal: bool,
) -> Vec<NwayRegion> {
    let mut all: Vec<(u64, u64)> = per_input
        .iter()
        .flatten()
        .filter(|r| r.kind != RegionKind::Equal)
        .map(|r| (r.a.start, r.a.end()))
        .collect();
    all.sort_unstable();
    let mut clusters: Vec<(u64, u64)> = Vec::new();
    for (s, e) in all {
        match clusters.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => clusters.push((s, e)),
        }
    }
    let mut maps: Vec<Mapper<'_>> = per_input
        .iter()
        .map(|r| Mapper {
            regions: r,
            idx: 0,
            delta: 0,
        })
        .collect();
    let mut last_right = vec![0u64; per_input.len()];
    let mut out = Vec::new();
    let mut prev_end = 0u64;
    let equal_run = |out: &mut Vec<NwayRegion>, from: u64, to: u64, last_right: &[u64]| {
        if include_equal && to > from {
            out.push(NwayRegion {
                base: Span::new(from, to - from),
                inputs: last_right
                    .iter()
                    .map(|&s| NwaySpan {
                        span: Span::new(s, to - from),
                        kind: RegionKind::Equal,
                    })
                    .collect(),
                differing: 0,
            });
        }
    };
    for (s, e) in clusters {
        equal_run(&mut out, prev_end, s, &last_right);
        let mut inputs = Vec::with_capacity(maps.len());
        let mut differing = 0;
        for (m, lr) in maps.iter_mut().zip(&mut last_right) {
            let l = m.left(s);
            let (r, passed) = m.right(e);
            *lr = r;
            let kind = if passed == 0 {
                RegionKind::Equal
            } else {
                differing += 1;
                RegionKind::of_lengths(e - s, r - l)
            };
            inputs.push(NwaySpan {
                span: Span::new(l, r - l),
                kind,
            });
        }
        out.push(NwayRegion {
            base: Span::new(s, e - s),
            inputs,
            differing,
        });
        prev_end = e;
    }
    equal_run(&mut out, prev_end, base_len, &last_right);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use proptest::prelude::*;

    fn run(base: &[u8], inputs: &[&[u8]], eq: bool) -> NwayResult {
        let opts = DiffOptions {
            include_equal: eq,
            ..DiffOptions::default()
        };
        let trees: Vec<Tree> = inputs.iter().map(|x| tree(x)).collect();
        diff_nway(&tree(base), &trees, &opts, &Cancel::new(), &noprog).unwrap()
    }

    #[test]
    fn three_inputs() {
        let base = b"0123456789abcdefghij";
        let x = b"0123X56789abcdefghij"; // change at 4
        let y = b"0123456789abcdefghijTAIL"; // append
        let z = b"012345689abcdefgh++ij"; // delete 7, insert at 18
        let r = run(base, &[x, y, z], false);
        assert_eq!(r.stats.len(), 3);
        let bases: Vec<Span> = r.regions.iter().map(|r| r.base).collect();
        assert_eq!(
            bases,
            vec![
                Span::new(4, 1),
                Span::new(7, 1),
                Span::new(18, 0),
                Span::new(20, 0)
            ]
        );
        assert_eq!(r.regions[0].inputs[0].kind, RegionKind::Changed);
        assert_eq!(r.regions[0].inputs[1].kind, RegionKind::Equal);
        assert_eq!(r.regions[1].inputs[2].kind, RegionKind::Deleted);
        assert_eq!(r.regions[1].inputs[2].span, Span::new(7, 0));
        assert_eq!(r.regions[2].inputs[2].span, Span::new(17, 2));
        assert_eq!(r.regions[3].inputs[1].span, Span::new(20, 4));
        assert_eq!(r.regions[3].inputs[2].span, Span::new(21, 0));
        assert_eq!(r.regions[3].differing, 1);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]
        #[test]
        fn spans_tile_every_input(seed in any::<u64>(), len in 0usize..400,
            edits in proptest::collection::vec((0usize..3, any::<u16>(), 0usize..5,
                proptest::collection::vec(any::<u8>(), 0..6)), 0..12)) {
            let base = noise(seed, len);
            let mut ins = [base.clone(), base.clone(), base.clone()];
            for (which, pos, del, data) in edits {
                let v = &mut ins[which];
                let at = pos as usize % (v.len() + 1);
                let end = (at + del).min(v.len());
                v.splice(at..end, data);
            }
            let refs: Vec<&[u8]> = ins.iter().map(Vec::as_slice).collect();
            let r = run(&base, &refs, true);
            let mut pos = [0u64; 3];
            let mut bpos = 0;
            for reg in &r.regions {
                prop_assert_eq!(reg.base.start, bpos);
                bpos = reg.base.end();
                for (i, s) in reg.inputs.iter().enumerate() {
                    prop_assert_eq!(s.span.start, pos[i]);
                    pos[i] = s.span.end();
                    if s.kind == RegionKind::Equal {
                        let bs = &base[reg.base.start as usize..reg.base.end() as usize];
                        let is = &ins[i][s.span.start as usize..s.span.end() as usize];
                        prop_assert_eq!(bs, is);
                    }
                }
            }
            prop_assert_eq!(bpos, base.len() as u64);
            for i in 0..3 {
                prop_assert_eq!(pos[i], ins[i].len() as u64);
            }
        }
    }
}
