//! DIFF-01: streaming byte-by-byte comparison.

use std::ops::Range;

use hexa_core::{Cancel, Result, Tree};

use crate::chunk::{CHUNK, Tracker, mismatch, read};
use crate::sink::Sink;

/// Compares `a[ra]` with `b[rb]` offset by offset and feeds the sink. The
/// longer range's tail becomes an insertion or deletion.
pub(crate) fn compare(
    a: &Tree,
    ra: Range<u64>,
    b: &Tree,
    rb: Range<u64>,
    sink: &mut Sink<'_>,
    cancel: &Cancel,
    prog: &mut Tracker<'_>,
) -> Result<()> {
    let (la, lb) = (ra.end - ra.start, rb.end - rb.start);
    let n = la.min(lb);
    let size = CHUNK.min(n as usize);
    let mut ba = vec![0u8; size];
    let mut bb = vec![0u8; size];
    let mut off = 0u64;
    while off < n {
        cancel.check()?;
        let len = (n - off).min(CHUNK as u64) as usize;
        let (ca, cb) = (&mut ba[..len], &mut bb[..len]);
        read(a, ra.start + off, ca)?;
        read(b, rb.start + off, cb)?;
        if ca == cb {
            sink.equal(len as u64)?;
        } else {
            let mut i = 0;
            while i < len {
                let m = mismatch(&ca[i..], &cb[i..]);
                sink.equal(m as u64)?;
                i += m;
                if i == len {
                    break;
                }
                let start = i;
                while i < len && ca[i] != cb[i] {
                    i += 1;
                }
                let d = (i - start) as u64;
                sink.diff(d, d)?;
            }
        }
        off += len as u64;
        prog.add(len as u64);
    }
    if la != lb {
        sink.split()?;
        sink.diff(la - n, lb - n)?;
        prog.add(la.max(lb) - n);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::testutil::*;
    use crate::{DiffMode, DiffOptions, Region, RegionKind, Span, diff};
    use hexa_core::Cancel;
    use proptest::prelude::*;

    fn bytewise(a: &[u8], b: &[u8], gap: u64, eq: bool) -> crate::DiffResult {
        let opts = DiffOptions {
            mode: DiffMode::Bytewise,
            merge_gap: gap,
            include_equal: eq,
            max_regions: None,
        };
        diff(&tree(a), &tree(b), &opts, &Cancel::new(), &noprog).unwrap()
    }

    #[test]
    fn simple_fixture() {
        let r = bytewise(b"abcdefgh", b"abXdeYYhij", 0, false);
        assert_eq!(
            r.regions,
            vec![
                Region {
                    a: Span::new(2, 1),
                    b: Span::new(2, 1),
                    kind: RegionKind::Changed
                },
                Region {
                    a: Span::new(5, 2),
                    b: Span::new(5, 2),
                    kind: RegionKind::Changed
                },
                Region {
                    a: Span::new(8, 0),
                    b: Span::new(8, 2),
                    kind: RegionKind::Inserted
                },
            ]
        );
        assert_eq!(r.stats.equal_bytes, 5);
        assert_eq!(r.stats.changed.count, 2);
        assert_eq!(r.stats.inserted.bytes_b, 2);
        let r = bytewise(b"abcdefgh", b"abXdeYYhij", 3, false);
        assert_eq!(r.regions.len(), 1);
        assert_eq!(r.regions[0].a, Span::new(2, 6));
        assert_eq!(r.regions[0].b, Span::new(2, 8));
        let r = bytewise(b"abcdefgh", b"abXd", 0, false);
        assert_eq!(r.regions[1].kind, RegionKind::Deleted);
        assert_eq!(r.regions[1].a, Span::new(4, 4));
    }

    #[test]
    fn crosses_chunk_boundaries() {
        let mut a = noise(5, (1 << 20) + 300);
        let mut b = a.clone();
        for x in &mut b[(1 << 20) - 10..(1 << 20) + 10] {
            *x ^= 0xFF;
        }
        a.extend_from_slice(b"tail");
        let r = bytewise(&a, &b, 0, false);
        assert_eq!(r.regions.len(), 2);
        assert_eq!(r.regions[0].a, Span::new((1 << 20) - 10, 20));
        assert_eq!(r.regions[1].kind, RegionKind::Deleted);
    }

    proptest! {
        #[test]
        fn matches_naive(a in proptest::collection::vec(0u8..4, 0..400),
                         b in proptest::collection::vec(0u8..4, 0..400)) {
            let r = bytewise(&a, &b, 0, true);
            // Regions tile both inputs.
            let (mut pa, mut pb) = (0, 0);
            for reg in &r.regions {
                prop_assert_eq!(reg.a.start, pa);
                prop_assert_eq!(reg.b.start, pb);
                pa = reg.a.end();
                pb = reg.b.end();
                let sa = &a[reg.a.start as usize..reg.a.end() as usize];
                let sb = &b[reg.b.start as usize..reg.b.end() as usize];
                match reg.kind {
                    RegionKind::Equal => prop_assert_eq!(sa, sb),
                    RegionKind::Changed => {
                        prop_assert_eq!(sa.len(), sb.len());
                        prop_assert!(sa.iter().zip(sb).all(|(x, y)| x != y));
                    }
                    RegionKind::Inserted => prop_assert!(sa.is_empty()),
                    RegionKind::Deleted => prop_assert!(sb.is_empty()),
                }
            }
            prop_assert_eq!(pa, a.len() as u64);
            prop_assert_eq!(pb, b.len() as u64);
            let naive = a.iter().zip(&b).filter(|(x, y)| x == y).count() as u64;
            prop_assert_eq!(r.stats.equal_bytes, naive);
        }
    }
}
