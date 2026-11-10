//! Turns a stream of equal / different runs into merged regions.

use hexa_core::Result;

use crate::stats::DiffStats;
use crate::{Region, RegionKind, Span};

/// Collects runs in order; positions in A and B are implicit.
pub(crate) struct Sink<'f> {
    merge_gap: u64,
    include_equal: bool,
    a_pos: u64,
    b_pos: u64,
    /// Pending difference.
    cur: Option<Region>,
    /// Pending equal run (directly after `cur` when both are set).
    eq: Option<Region>,
    stats: DiffStats,
    out: &'f mut dyn FnMut(Region) -> Result<()>,
}

impl<'f> Sink<'f> {
    pub(crate) fn new(
        merge_gap: u64,
        include_equal: bool,
        len_a: u64,
        len_b: u64,
        out: &'f mut dyn FnMut(Region) -> Result<()>,
    ) -> Self {
        Self {
            merge_gap,
            include_equal,
            a_pos: 0,
            b_pos: 0,
            cur: None,
            eq: None,
            stats: DiffStats::new(len_a, len_b),
            out,
        }
    }

    fn emit(&mut self, mut r: Region) -> Result<()> {
        if r.kind != RegionKind::Equal {
            r.kind = RegionKind::of_lengths(r.a.len, r.b.len);
        }
        self.stats.add(&r);
        if r.kind == RegionKind::Equal && !self.include_equal {
            return Ok(());
        }
        (self.out)(r)
    }

    /// `n` bytes equal in both inputs.
    pub(crate) fn equal(&mut self, n: u64) -> Result<()> {
        if n == 0 {
            return Ok(());
        }
        self.stats.equal_bytes += n;
        let e = self.eq.get_or_insert(Region {
            a: Span::new(self.a_pos, 0),
            b: Span::new(self.b_pos, 0),
            kind: RegionKind::Equal,
        });
        e.a.len += n;
        e.b.len += n;
        let gap = e.a.len;
        self.a_pos += n;
        self.b_pos += n;
        if gap >= self.merge_gap
            && let Some(c) = self.cur.take()
        {
            self.emit(c)?;
        }
        Ok(())
    }

    /// `a_len` bytes of A replaced by `b_len` bytes of B.
    pub(crate) fn diff(&mut self, a_len: u64, b_len: u64) -> Result<()> {
        if a_len == 0 && b_len == 0 {
            return Ok(());
        }
        if let Some(e) = self.eq.take() {
            match &mut self.cur {
                Some(c) if e.a.len < self.merge_gap => {
                    c.a.len += e.a.len;
                    c.b.len += e.b.len;
                }
                _ => {
                    if let Some(c) = self.cur.take() {
                        self.emit(c)?;
                    }
                    self.emit(e)?;
                }
            }
        }
        match &mut self.cur {
            Some(c) => {
                c.a.len += a_len;
                c.b.len += b_len;
            }
            None => {
                self.cur = Some(Region {
                    a: Span::new(self.a_pos, a_len),
                    b: Span::new(self.b_pos, b_len),
                    kind: RegionKind::Changed,
                });
            }
        }
        self.a_pos += a_len;
        self.b_pos += b_len;
        Ok(())
    }

    /// Ends the pending difference so the next one starts a new region.
    pub(crate) fn split(&mut self) -> Result<()> {
        if self.eq.is_none()
            && let Some(c) = self.cur.take()
        {
            self.emit(c)?;
        }
        Ok(())
    }

    pub(crate) fn finish(mut self) -> Result<DiffStats> {
        if let Some(c) = self.cur.take() {
            self.emit(c)?;
        }
        if let Some(e) = self.eq.take() {
            self.emit(e)?;
        }
        debug_assert_eq!(self.a_pos, self.stats.len_a);
        debug_assert_eq!(self.b_pos, self.stats.len_b);
        self.stats.finish();
        Ok(self.stats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(gap: u64, ops: &[(u64, u64, bool)]) -> Vec<Region> {
        let (la, lb) = ops.iter().fold((0, 0), |(a, b), &(x, y, _)| (a + x, b + y));
        let mut out = Vec::new();
        let mut f = |r| {
            out.push(r);
            Ok(())
        };
        let mut s = Sink::new(gap, true, la, lb, &mut f);
        for &(x, y, eq) in ops {
            if eq {
                s.equal(x).unwrap();
            } else {
                s.diff(x, y).unwrap();
            }
        }
        s.finish().unwrap();
        out
    }

    #[test]
    fn merges_adjacent_and_gaps() {
        let r = run(
            0,
            &[(2, 0, false), (0, 3, false), (4, 4, true), (1, 1, false)],
        );
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].kind, RegionKind::Changed);
        assert_eq!((r[0].a.len, r[0].b.len), (2, 3));
        assert_eq!(r[1].kind, RegionKind::Equal);
        assert_eq!(r[2].a, Span::new(6, 1));

        let r = run(
            5,
            &[(2, 0, false), (4, 4, true), (1, 1, false), (9, 9, true)],
        );
        assert_eq!(r.len(), 2);
        assert_eq!((r[0].a, r[0].b), (Span::new(0, 7), Span::new(0, 5)));
        assert_eq!(r[1].kind, RegionKind::Equal);
        assert_eq!(r[1].a, Span::new(7, 9));
    }
}
