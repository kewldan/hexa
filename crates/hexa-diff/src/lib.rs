//! Binary diff, merge and patch formats for Hexa.
//!
//! * DIFF-01 — streaming byte-by-byte comparison ([`DiffMode::Bytewise`]).
//! * DIFF-02 — shift-aware diff with rolling-hash anchors and Myers between
//!   them ([`DiffMode::ShiftAware`]).
//! * DIFF-03 — N-way comparison against a base ([`diff_nway`]).
//! * DIFF-04 — comparison of two ranges ([`diff_ranges`]).
//! * DIFF-07 — region list export as text, CSV and JSON ([`export`]).
//! * DIFF-09 — applying regions and three-way merge ([`merge`]).
//! * DIFF-11 — statistics ([`DiffStats`]).
//! * DIFF-12 / IO-05 — IPS, UPS, BPS, VCDIFF and bsdiff patches ([`patch`]).
//!
//! All inputs are [`Tree`] snapshots read in bounded chunks, so the size of
//! the compared files is limited by disk, not memory.

mod chunk;
mod compare;
pub mod export;
pub mod merge;
mod myers;
mod nway;
pub mod patch;
mod shift;
mod sink;
mod stats;

use std::ops::Range;

use hexa_core::task::Progress;
use hexa_core::{Cancel, Error, Result, Tree};
use serde::{Deserialize, Serialize};

pub use export::{ExportFormat, export, export_string};
pub use merge::{
    ConflictResolution, MergeConflict, MergeOptions, MergeResult, MergeSegment, MergeSource,
    RegionPatch, apply_region, apply_regions, merge3,
};
pub use nway::{NwayRegion, NwayResult, NwaySpan, diff_nway};
pub use patch::{
    Applied, ApplyOptions, CreateOptions, PatchFormat, PatchInfo, apply_patch, create_patch,
    detect_format, inspect_patch,
};
pub use stats::{DiffStats, KindStats};

/// A byte range `[start, start + len)` of one input.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Span {
    pub start: u64,
    pub len: u64,
}

impl Span {
    pub const fn new(start: u64, len: u64) -> Self {
        Self { start, len }
    }

    /// Exclusive end offset.
    pub const fn end(self) -> u64 {
        self.start + self.len
    }

    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    pub const fn range(self) -> Range<u64> {
        self.start..self.end()
    }
}

/// How a region of A relates to the matching region of B (DIFF-01, DIFF-02).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RegionKind {
    /// Identical bytes in A and B.
    Equal,
    /// Bytes of A replaced by (possibly a different number of) bytes of B.
    Changed,
    /// Bytes present only in B (`a.len == 0`).
    Inserted,
    /// Bytes present only in A (`b.len == 0`).
    Deleted,
}

impl RegionKind {
    /// Kind of a non-equal region with the given lengths.
    pub const fn of_lengths(a_len: u64, b_len: u64) -> Self {
        match (a_len, b_len) {
            (0, _) => Self::Inserted,
            (_, 0) => Self::Deleted,
            _ => Self::Changed,
        }
    }

    /// The kind seen from the other side (A and B swapped).
    pub const fn swapped(self) -> Self {
        match self {
            Self::Inserted => Self::Deleted,
            Self::Deleted => Self::Inserted,
            k => k,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Equal => "equal",
            Self::Changed => "changed",
            Self::Inserted => "inserted",
            Self::Deleted => "deleted",
        }
    }
}

/// One aligned region: `a` in the first input matches `b` in the second.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Region {
    pub a: Span,
    pub b: Span,
    pub kind: RegionKind,
}

impl Region {
    /// The same region with the roles of A and B exchanged.
    pub const fn swapped(&self) -> Self {
        Self {
            a: self.b,
            b: self.a,
            kind: self.kind.swapped(),
        }
    }
}

/// Comparison algorithm.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiffMode {
    /// DIFF-01: offset `i` of A is compared with offset `i` of B; the longer
    /// input's tail is reported as inserted or deleted.
    Bytewise,
    /// DIFF-02: insertions and deletions are detected so shifted data still
    /// matches.
    #[default]
    ShiftAware,
}

/// Options shared by every comparison.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DiffOptions {
    pub mode: DiffMode,
    /// Also report [`RegionKind::Equal`] regions (off by default).
    pub include_equal: bool,
    /// Differences separated by fewer than this many equal bytes are merged
    /// into one region. `0` merges only directly adjacent differences.
    pub merge_gap: u64,
    /// Upper bound for the collected region list; statistics still cover
    /// the whole input when it is reached (`truncated` is set).
    pub max_regions: Option<usize>,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            mode: DiffMode::ShiftAware,
            include_equal: false,
            merge_gap: 0,
            max_regions: Some(1_000_000),
        }
    }
}

/// Regions plus statistics of a comparison (DIFF-07, DIFF-11).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffResult {
    pub regions: Vec<Region>,
    pub stats: DiffStats,
    /// `true` when `max_regions` stopped the collection early.
    pub truncated: bool,
}

/// Compares two snapshots (DIFF-01 or DIFF-02, depending on `opts.mode`).
pub fn diff(
    a: &Tree,
    b: &Tree,
    opts: &DiffOptions,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<DiffResult> {
    let mut regions = Vec::new();
    let mut truncated = false;
    let limit = opts.max_regions.unwrap_or(usize::MAX);
    let stats = diff_each(a, b, opts, cancel, progress, &mut |r| {
        if regions.len() < limit {
            regions.push(r);
        } else {
            truncated = true;
        }
        Ok(())
    })?;
    Ok(DiffResult {
        regions,
        stats,
        truncated,
    })
}

/// Streams the regions of a comparison to `f` in order instead of collecting
/// them; `opts.max_regions` is ignored.
pub fn diff_each(
    a: &Tree,
    b: &Tree,
    opts: &DiffOptions,
    cancel: &Cancel,
    progress: Progress<'_>,
    f: &mut dyn FnMut(Region) -> Result<()>,
) -> Result<DiffStats> {
    let (la, lb) = (a.len(), b.len());
    let mut sink = sink::Sink::new(opts.merge_gap, opts.include_equal, la, lb, f);
    let mut prog = chunk::Tracker::new(progress, 0);
    match opts.mode {
        DiffMode::Bytewise => {
            prog.set_total(la.max(lb));
            compare::compare(a, 0..la, b, 0..lb, &mut sink, cancel, &mut prog)?;
        }
        DiffMode::ShiftAware => {
            prog.set_total(2 * (la + lb));
            shift::ShiftDiff::new(a, b, cancel, &mut prog).run(&mut sink)?;
        }
    }
    prog.finish();
    sink.finish()
}

/// DIFF-04: compares `a[a_range]` with `b[b_range]`. Regions are reported
/// in the absolute offsets of `a` and `b`.
pub fn diff_ranges(
    a: &Tree,
    a_range: Range<u64>,
    b: &Tree,
    b_range: Range<u64>,
    opts: &DiffOptions,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<DiffResult> {
    check_range(a, &a_range)?;
    check_range(b, &b_range)?;
    let sa = a.slice(a_range.start, a_range.end);
    let sb = b.slice(b_range.start, b_range.end);
    let mut res = diff(&sa, &sb, opts, cancel, progress)?;
    for r in &mut res.regions {
        r.a.start += a_range.start;
        r.b.start += b_range.start;
    }
    Ok(res)
}

pub(crate) fn check_range(t: &Tree, r: &Range<u64>) -> Result<()> {
    if r.start > r.end || r.end > t.len() {
        return Err(Error::OutOfRange {
            start: r.start,
            end: r.end,
            len: t.len(),
        });
    }
    Ok(())
}

/// Reads a whole snapshot into memory, failing with [`Error::TooLarge`] when
/// it exceeds `limit` bytes. Meant for small results (DIFF-09).
pub fn tree_to_vec(tree: &Tree, limit: u64) -> Result<Vec<u8>> {
    let len = tree.len();
    if len > limit {
        return Err(Error::TooLarge { len, limit });
    }
    let mut buf = vec![0; usize::try_from(len).map_err(|_| Error::TooLarge { len, limit })?];
    tree.read(0, &mut buf)?;
    Ok(buf)
}

#[cfg(test)]
pub(crate) mod testutil {
    use hexa_core::Tree;
    use hexa_core::piece::Piece;

    pub fn tree(data: &[u8]) -> Tree {
        if data.is_empty() {
            Tree::new()
        } else {
            Tree::from_piece(Piece::bytes(data.to_vec()))
        }
    }

    pub fn bytes(t: &Tree) -> Vec<u8> {
        t.read_vec(0, usize::MAX).unwrap()
    }

    /// Deterministic pseudo-random bytes.
    pub fn noise(seed: u64, len: usize) -> Vec<u8> {
        let mut out = vec![0; len];
        Piece::random(seed, len as u64).read(0, &mut out).unwrap();
        out
    }

    pub fn noprog(_: u64, _: u64) {}
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;

    #[test]
    fn ranges_are_absolute() {
        let a = tree(b"xxxxHELLOyyyy");
        let b = tree(b"zzHELLOzz");
        let opts = DiffOptions {
            mode: DiffMode::Bytewise,
            ..DiffOptions::default()
        };
        let r = diff_ranges(&a, 4..9, &b, 2..7, &opts, &Cancel::new(), &noprog).unwrap();
        assert!(r.regions.is_empty());
        assert!((r.stats.match_percent - 100.0).abs() < 1e-9);

        let b2 = tree(b"zzHEXLOzz");
        let r = diff_ranges(&a, 4..9, &b2, 2..7, &opts, &Cancel::new(), &noprog).unwrap();
        assert_eq!(
            r.regions,
            vec![Region {
                a: Span::new(6, 1),
                b: Span::new(4, 1),
                kind: RegionKind::Changed
            }]
        );
        assert!(diff_ranges(&a, 4..99, &b, 0..1, &opts, &Cancel::new(), &noprog).is_err());
    }

    #[test]
    fn max_regions_truncates_but_counts() {
        let a = tree(&[0u8; 100]);
        let mut v = vec![0u8; 100];
        for i in (0..100).step_by(2) {
            v[i] = 1;
        }
        let b = tree(&v);
        let opts = DiffOptions {
            mode: DiffMode::Bytewise,
            max_regions: Some(5),
            ..DiffOptions::default()
        };
        let r = diff(&a, &b, &opts, &Cancel::new(), &noprog).unwrap();
        assert_eq!(r.regions.len(), 5);
        assert!(r.truncated);
        assert_eq!(r.stats.changed.count, 50);
        assert_eq!(r.stats.equal_bytes, 50);
    }

    #[test]
    fn cancel_is_honoured() {
        let a = tree(&noise(1, 1 << 20));
        let b = tree(&noise(2, 1 << 20));
        let c = Cancel::new();
        c.cancel();
        for mode in [DiffMode::Bytewise, DiffMode::ShiftAware] {
            let opts = DiffOptions {
                mode,
                ..DiffOptions::default()
            };
            assert!(matches!(
                diff(&a, &b, &opts, &c, &noprog),
                Err(Error::Cancelled)
            ));
        }
    }

    #[test]
    fn serde_is_camel_case() {
        let r = Region {
            a: Span::new(1, 2),
            b: Span::new(3, 0),
            kind: RegionKind::Deleted,
        };
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(
            s,
            r#"{"a":{"start":1,"len":2},"b":{"start":3,"len":0},"kind":"deleted"}"#
        );
        let opts: DiffOptions = serde_json::from_str(r#"{"mergeGap":4}"#).unwrap();
        assert_eq!(opts.merge_gap, 4);
        assert_eq!(opts.mode, DiffMode::ShiftAware);
    }
}
