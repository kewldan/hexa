//! DIFF-09: applying diff regions and three-way merge.

use hexa_core::task::Progress;
use hexa_core::{Cancel, Error, Result, Tree};
use serde::{Deserialize, Serialize};

use crate::chunk::ranges_equal;
use crate::nway::cluster;
use crate::{DiffOptions, Region, RegionKind, Span, check_range, diff_each, tree_to_vec};

/// Edit that copies one region of B into A: replace `remove` bytes at `at`
/// with `insert` (a span of B).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegionPatch {
    pub at: u64,
    pub remove: u64,
    pub insert: Span,
}

impl RegionPatch {
    /// DIFF-09 (a): the edit turning region `r.a` of A into `r.b` of B.
    pub const fn from_region(r: &Region) -> Self {
        Self {
            at: r.a.start,
            remove: r.a.len,
            insert: r.b,
        }
    }

    /// The bytes to insert, read from `b` (small regions only).
    pub fn bytes(&self, b: &Tree, limit: u64) -> Result<Vec<u8>> {
        check_range(b, &self.insert.range())?;
        tree_to_vec(&b.slice(self.insert.start, self.insert.end()), limit)
    }
}

/// DIFF-09 (a): copies one region from B into A without copying bytes.
pub fn apply_region(a: &Tree, b: &Tree, region: &Region) -> Result<Tree> {
    check_range(a, &region.a.range())?;
    check_range(b, &region.b.range())?;
    Ok(a.replace(
        region.a.start,
        region.a.end(),
        &b.slice(region.b.start, region.b.end()),
    ))
}

/// Copies several regions from B into A. Regions must be sorted by A offset
/// and must not overlap (as produced by a diff); equal regions are skipped.
pub fn apply_regions(a: &Tree, b: &Tree, regions: &[Region]) -> Result<Tree> {
    let mut prev = 0;
    for r in regions {
        if r.a.start < prev {
            return Err(Error::Invalid("regions overlap or are not sorted".into()));
        }
        prev = r.a.end();
    }
    let mut out = a.clone();
    for r in regions.iter().rev() {
        if r.kind != RegionKind::Equal {
            out = apply_region(&out, b, r)?;
        }
    }
    Ok(out)
}

/// Input a merged segment comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MergeSource {
    Base,
    Ours,
    Theirs,
}

/// What a conflict holds in the merged output until resolved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConflictResolution {
    #[default]
    Ours,
    Theirs,
    Base,
}

/// Options of [`merge3`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MergeOptions {
    /// Diff settings for base ↔ ours and base ↔ theirs (`mode`,
    /// `merge_gap`; region limits are ignored).
    pub diff: DiffOptions,
    pub conflict: ConflictResolution,
}

/// A span of one input copied to the output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeSegment {
    pub source: MergeSource,
    pub span: Span,
    /// Index into [`MergeResult::conflicts`] when this segment is a
    /// provisional conflict resolution.
    pub conflict: Option<u32>,
}

/// Both sides changed the same base region differently.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeConflict {
    pub base: Span,
    pub ours: Span,
    pub theirs: Span,
    /// Where the provisional resolution sits in the merged output.
    pub output: Span,
}

/// Result of [`merge3`]: the output as references into the inputs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeResult {
    pub segments: Vec<MergeSegment>,
    pub conflicts: Vec<MergeConflict>,
    /// Length of the merged output.
    pub len: u64,
}

impl MergeResult {
    fn push(&mut self, source: MergeSource, span: Span, conflict: Option<u32>) {
        if span.is_empty() {
            return;
        }
        self.len += span.len;
        if let Some(last) = self.segments.last_mut()
            && conflict.is_none()
            && last.conflict.is_none()
            && last.source == source
            && last.span.end() == span.start
        {
            last.span.len += span.len;
            return;
        }
        self.segments.push(MergeSegment {
            source,
            span,
            conflict,
        });
    }

    /// The merged output as a snapshot (no bytes are copied).
    pub fn to_tree(&self, base: &Tree, ours: &Tree, theirs: &Tree) -> Tree {
        let mut out = Tree::new();
        for s in &self.segments {
            let t = match s.source {
                MergeSource::Base => base,
                MergeSource::Ours => ours,
                MergeSource::Theirs => theirs,
            };
            out = out.concat(&t.slice(s.span.start, s.span.end()));
        }
        out
    }

    /// The merged output as bytes; fails with `TooLarge` past `limit`.
    pub fn materialize(
        &self,
        base: &Tree,
        ours: &Tree,
        theirs: &Tree,
        limit: u64,
    ) -> Result<Vec<u8>> {
        tree_to_vec(&self.to_tree(base, ours, theirs), limit)
    }
}

/// DIFF-09 (b): three-way merge. Changes made by only one side are taken;
/// identical changes on both sides are taken once; differing changes of the
/// same base region become conflicts, provisionally resolved per
/// `opts.conflict`.
pub fn merge3(
    base: &Tree,
    ours: &Tree,
    theirs: &Tree,
    opts: &MergeOptions,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<MergeResult> {
    let inner = DiffOptions {
        include_equal: false,
        ..opts.diff.clone()
    };
    let mut per_input = Vec::with_capacity(2);
    for (i, t) in [ours, theirs].into_iter().enumerate() {
        let scale = |done: u64, total: u64| {
            let frac = crate::chunk::permille(done, total);
            progress(i as u64 * 1000 + frac, 2000);
        };
        let mut regions = Vec::new();
        diff_each(base, t, &inner, cancel, &scale, &mut |r| {
            regions.push(r);
            Ok(())
        })?;
        per_input.push(regions);
    }
    let mut res = MergeResult::default();
    for region in cluster(base.len(), &per_input, true) {
        cancel.check()?;
        let (o, t) = (region.inputs[0], region.inputs[1]);
        let o_changed = o.kind != RegionKind::Equal;
        let t_changed = t.kind != RegionKind::Equal;
        match (o_changed, t_changed) {
            (false, false) => res.push(MergeSource::Base, region.base, None),
            (true, false) => res.push(MergeSource::Ours, o.span, None),
            (false, true) => res.push(MergeSource::Theirs, t.span, None),
            (true, true) => {
                let same = o.span.len == t.span.len
                    && ranges_equal(ours, o.span.start, theirs, t.span.start, o.span.len, cancel)?;
                if same {
                    res.push(MergeSource::Ours, o.span, None);
                } else {
                    let (source, span) = match opts.conflict {
                        ConflictResolution::Ours => (MergeSource::Ours, o.span),
                        ConflictResolution::Theirs => (MergeSource::Theirs, t.span),
                        ConflictResolution::Base => (MergeSource::Base, region.base),
                    };
                    let idx = res.conflicts.len() as u32;
                    res.conflicts.push(MergeConflict {
                        base: region.base,
                        ours: o.span,
                        theirs: t.span,
                        output: Span::new(res.len, span.len),
                    });
                    res.push(source, span, Some(idx));
                }
            }
        }
    }
    progress(2000, 2000);
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use crate::{DiffMode, diff};
    use proptest::prelude::*;

    fn m3(
        base: &[u8],
        ours: &[u8],
        theirs: &[u8],
        c: ConflictResolution,
    ) -> (MergeResult, Vec<u8>) {
        let (b, o, t) = (tree(base), tree(ours), tree(theirs));
        let opts = MergeOptions {
            conflict: c,
            ..MergeOptions::default()
        };
        let r = merge3(&b, &o, &t, &opts, &Cancel::new(), &noprog).unwrap();
        let bytes = r.materialize(&b, &o, &t, 1 << 20).unwrap();
        assert_eq!(bytes.len() as u64, r.len);
        (r, bytes)
    }

    #[test]
    fn apply_single_region() {
        let a = tree(b"hello world");
        let b = tree(b"hello brave new world");
        let r = diff(&a, &b, &DiffOptions::default(), &Cancel::new(), &noprog).unwrap();
        assert_eq!(r.regions.len(), 1);
        let p = RegionPatch::from_region(&r.regions[0]);
        assert_eq!(p.bytes(&b, 100).unwrap(), b"brave new ");
        let out = apply_region(&a, &b, &r.regions[0]).unwrap();
        assert_eq!(bytes(&out), b"hello brave new world");
        let back = apply_region(&b, &a, &r.regions[0].swapped()).unwrap();
        assert_eq!(bytes(&back), b"hello world");
        assert!(
            apply_region(
                &a,
                &b,
                &Region {
                    a: Span::new(10, 5),
                    ..r.regions[0]
                }
            )
            .is_err()
        );
    }

    #[test]
    fn non_conflicting_and_identical_changes() {
        let base = b"AAAA-BBBB-CCCC-DDDD-EEEE";
        let ours = b"AAAA-bbbb-CCCC-DDDD-EEEE!";
        let theirs = b"AAAA-BBBB-CCCC-dd-EEEE!";
        let (r, out) = m3(base, ours, theirs, ConflictResolution::Ours);
        assert_eq!(r.conflicts, vec![]);
        assert_eq!(out, b"AAAA-bbbb-CCCC-dd-EEEE!");
    }

    #[test]
    fn conflicts_are_reported() {
        let base = b"0123456789";
        let ours = b"0123XY6789";
        let theirs = b"0123Z56789";
        let (r, out) = m3(base, ours, theirs, ConflictResolution::Ours);
        assert_eq!(r.conflicts.len(), 1);
        let c = r.conflicts[0];
        assert_eq!(c.base, Span::new(4, 2));
        assert_eq!(c.ours, Span::new(4, 2));
        assert_eq!(c.theirs, Span::new(4, 2));
        assert_eq!(out, b"0123XY6789");
        assert_eq!(c.output, Span::new(4, 2));
        assert!(r.segments.iter().any(|s| s.conflict == Some(0)));
        let (_, out) = m3(base, ours, theirs, ConflictResolution::Theirs);
        assert_eq!(out, b"0123Z56789");
        let (_, out) = m3(base, ours, theirs, ConflictResolution::Base);
        assert_eq!(out, base);
    }

    #[test]
    fn bytewise_mode_merges_too() {
        let opts = MergeOptions {
            diff: DiffOptions {
                mode: DiffMode::Bytewise,
                ..DiffOptions::default()
            },
            ..MergeOptions::default()
        };
        let (b, o, t) = (tree(b"abcdef"), tree(b"Xbcdef"), tree(b"abcdeY"));
        let r = merge3(&b, &o, &t, &opts, &Cancel::new(), &noprog).unwrap();
        assert_eq!(r.materialize(&b, &o, &t, 100).unwrap(), b"XbcdeY");
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]
        #[test]
        fn one_sided_merge_is_that_side(seed in any::<u64>(), len in 0usize..500,
            edits in proptest::collection::vec((any::<u16>(), 0usize..5,
                proptest::collection::vec(any::<u8>(), 0..6)), 0..6)) {
            let base = noise(seed, len);
            let mut ours = base.clone();
            for (pos, del, data) in edits {
                let at = pos as usize % (ours.len() + 1);
                let end = (at + del).min(ours.len());
                ours.splice(at..end, data);
            }
            let (_, out) = m3(&base, &ours, &base, ConflictResolution::Base);
            prop_assert_eq!(&out, &ours);
            let (_, out) = m3(&base, &base, &ours, ConflictResolution::Base);
            prop_assert_eq!(&out, &ours);
            let (r, out) = m3(&base, &ours, &ours, ConflictResolution::Base);
            prop_assert!(r.conflicts.is_empty());
            prop_assert_eq!(&out, &ours);
        }
    }
}
