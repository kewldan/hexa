//! Comparison statistics (DIFF-11).

use serde::{Deserialize, Serialize};

use crate::{Region, RegionKind};

/// Number and total size of the regions of one kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KindStats {
    pub count: u64,
    /// Bytes covered in input A.
    pub bytes_a: u64,
    /// Bytes covered in input B.
    pub bytes_b: u64,
}

impl KindStats {
    fn add(&mut self, r: &Region) {
        self.count += 1;
        self.bytes_a += r.a.len;
        self.bytes_b += r.b.len;
    }
}

/// DIFF-11: match percentage plus count and size of regions per kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffStats {
    pub len_a: u64,
    pub len_b: u64,
    /// Bytes matched between A and B (before `merge_gap` folding).
    pub equal_bytes: u64,
    /// `equal_bytes / max(len_a, len_b) * 100` (100 for two empty inputs).
    pub match_percent: f64,
    pub equal: KindStats,
    pub changed: KindStats,
    pub inserted: KindStats,
    pub deleted: KindStats,
    /// Number of non-equal regions.
    pub differences: u64,
}

impl DiffStats {
    pub(crate) fn new(len_a: u64, len_b: u64) -> Self {
        Self {
            len_a,
            len_b,
            ..Self::default()
        }
    }

    pub(crate) fn add(&mut self, r: &Region) {
        match r.kind {
            RegionKind::Equal => self.equal.add(r),
            RegionKind::Changed => self.changed.add(r),
            RegionKind::Inserted => self.inserted.add(r),
            RegionKind::Deleted => self.deleted.add(r),
        }
        if r.kind != RegionKind::Equal {
            self.differences += 1;
        }
    }

    pub(crate) fn finish(&mut self) {
        let max = self.len_a.max(self.len_b);
        self.match_percent = if max == 0 {
            100.0
        } else {
            self.equal_bytes as f64 * 100.0 / max as f64
        };
    }

    /// Statistics of a region list. When `regions` lacks equal regions the
    /// equal byte count is derived from the uncovered part of A.
    pub fn from_regions(regions: &[Region], len_a: u64, len_b: u64) -> Self {
        let mut s = Self::new(len_a, len_b);
        for r in regions {
            s.add(r);
        }
        s.equal_bytes = if s.equal.count > 0 {
            s.equal.bytes_a
        } else {
            len_a.saturating_sub(s.changed.bytes_a + s.deleted.bytes_a)
        };
        s.finish();
        s
    }

    /// Statistics of one kind.
    pub fn kind(&self, kind: RegionKind) -> &KindStats {
        match kind {
            RegionKind::Equal => &self.equal,
            RegionKind::Changed => &self.changed,
            RegionKind::Inserted => &self.inserted,
            RegionKind::Deleted => &self.deleted,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Span;

    #[test]
    fn from_regions_counts() {
        let regions = [
            Region {
                a: Span::new(2, 2),
                b: Span::new(2, 3),
                kind: RegionKind::Changed,
            },
            Region {
                a: Span::new(8, 0),
                b: Span::new(9, 4),
                kind: RegionKind::Inserted,
            },
        ];
        let s = DiffStats::from_regions(&regions, 10, 15);
        assert_eq!(s.changed.count, 1);
        assert_eq!(s.inserted.bytes_b, 4);
        assert_eq!(s.differences, 2);
        assert_eq!(s.equal_bytes, 8);
        assert!((s.match_percent - 8.0 * 100.0 / 15.0).abs() < 1e-9);
        assert!((DiffStats::from_regions(&[], 0, 0).match_percent - 100.0).abs() < 1e-9);
    }
}
