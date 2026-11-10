//! Encoder front end shared by BPS, VCDIFF and bsdiff: turns a source and a
//! target into copy / literal / run operations.
//!
//! The DIFF-02 alignment provides in-order copies; literal stretches are
//! then searched for runs and for blocks that moved, using a sparse hash
//! index of the source (bounded size).

use std::collections::HashMap;

use hexa_core::task::Progress;
use hexa_core::{Cancel, Result, Tree};

use crate::chunk::{CHUNK, Tracker, read};
use crate::{DiffOptions, Region, RegionKind, diff_each};

/// One encoder operation, in target order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Op {
    /// `len` target bytes equal `source[src..]`.
    Copy { src: u64, len: u64 },
    /// `len` target bytes similar to `source[src..]` (same length change).
    Delta { src: u64, len: u64 },
    /// `target[at..at+len]` stored verbatim.
    Literal { at: u64, len: u64 },
    /// `len` repetitions of `byte`.
    Run { byte: u8, len: u64 },
}

impl Op {
    pub(crate) fn len(self) -> u64 {
        match self {
            Self::Copy { len, .. }
            | Self::Delta { len, .. }
            | Self::Literal { len, .. }
            | Self::Run { len, .. } => len,
        }
    }
}

const BLOCK: usize = 32;
const MAX_INDEX: u64 = 1 << 20;
const RUN_MIN: usize = 16;
const P: u64 = 0x100_0000_01B3;

fn block_hash(b: &[u8]) -> u64 {
    b.iter().fold(0u64, |h, &x| {
        h.wrapping_mul(P).wrapping_add(u64::from(x) + 1)
    })
}

/// `P^(BLOCK-1)`.
fn p_top() -> u64 {
    (1..BLOCK).fold(1u64, |acc, _| acc.wrapping_mul(P))
}

struct Index {
    map: HashMap<u64, u64>,
}

impl Index {
    fn build(src: &Tree, cancel: &Cancel) -> Result<Self> {
        let len = src.len();
        let stride = (len / MAX_INDEX)
            .next_power_of_two()
            .clamp(BLOCK as u64, CHUNK as u64) as usize;
        let mut map = HashMap::new();
        let mut buf = vec![0u8; (CHUNK + BLOCK).min(len as usize)];
        let mut c = 0u64;
        while c < len && map.len() < 2 * MAX_INDEX as usize {
            cancel.check()?;
            let n = (len - c).min((CHUNK + BLOCK) as u64) as usize;
            read(src, c, &mut buf[..n])?;
            let mut p = 0;
            while p < CHUNK.min(n) && p + BLOCK <= n {
                map.entry(block_hash(&buf[p..p + BLOCK]))
                    .or_insert(c + p as u64);
                p += stride;
            }
            c += CHUNK as u64;
        }
        Ok(Self { map })
    }
}

/// Number of leading bytes of `data` equal to `src[at..]`.
fn match_forward(src: &Tree, at: u64, data: &[u8]) -> Result<usize> {
    let max = (src.len() - at).min(data.len() as u64) as usize;
    let mut done = 0;
    let mut size = 256;
    let mut buf = Vec::new();
    while done < max {
        let n = (max - done).min(size);
        buf.resize(n, 0);
        read(src, at + done as u64, &mut buf)?;
        let m = crate::chunk::mismatch(&buf, &data[done..done + n]);
        done += m;
        if m < n {
            break;
        }
        size = (size * 4).min(CHUNK);
    }
    Ok(done)
}

/// Number of trailing bytes of `data` equal to `src[..end]`.
fn match_backward(src: &Tree, end: u64, data: &[u8]) -> Result<usize> {
    let max = end.min(data.len() as u64) as usize;
    if max == 0 {
        return Ok(0);
    }
    let mut buf = vec![0u8; max];
    read(src, end - max as u64, &mut buf)?;
    Ok(crate::chunk::mismatch_rev(&buf, &data[data.len() - max..]))
}

struct Gen<'a, 'p> {
    source: &'a Tree,
    target: &'a Tree,
    cancel: &'a Cancel,
    index: Option<Index>,
    prog: Tracker<'p>,
}

impl Gen<'_, '_> {
    fn refine(&mut self, at: u64, len: u64, f: &mut dyn FnMut(Op) -> Result<()>) -> Result<()> {
        if self.index.is_none() && len >= BLOCK as u64 && self.source.len() >= BLOCK as u64 {
            self.index = Some(Index::build(self.source, self.cancel)?);
        }
        let ptop = p_top();
        let mut buf = vec![0u8; CHUNK.min(len as usize)];
        let mut off = 0u64;
        while off < len {
            self.cancel.check()?;
            let n = (len - off).min(CHUNK as u64) as usize;
            let t0 = at + off;
            read(self.target, t0, &mut buf[..n])?;
            let buf = &buf[..n];
            let mut lit = 0usize;
            let mut i = 0usize;
            let mut h = 0u64;
            let mut hpos = usize::MAX;
            let flush = |from: usize, to: usize, f: &mut dyn FnMut(Op) -> Result<()>| {
                if to > from {
                    f(Op::Literal {
                        at: t0 + from as u64,
                        len: (to - from) as u64,
                    })
                } else {
                    Ok(())
                }
            };
            while i < n {
                if i + RUN_MIN <= n && buf[i] == buf[i + 1] {
                    let j = i + buf[i..].iter().take_while(|&&x| x == buf[i]).count();
                    if j - i >= RUN_MIN {
                        flush(lit, i, f)?;
                        f(Op::Run {
                            byte: buf[i],
                            len: (j - i) as u64,
                        })?;
                        i = j;
                        lit = i;
                        continue;
                    }
                }
                if let Some(idx) = &self.index
                    && i + BLOCK <= n
                {
                    if hpos != usize::MAX && hpos + 1 == i {
                        h = h
                            .wrapping_sub((u64::from(buf[hpos]) + 1).wrapping_mul(ptop))
                            .wrapping_mul(P)
                            .wrapping_add(u64::from(buf[i + BLOCK - 1]) + 1);
                    } else {
                        h = block_hash(&buf[i..i + BLOCK]);
                    }
                    hpos = i;
                    if let Some(&src) = idx.map.get(&h) {
                        let fwd = match_forward(self.source, src, &buf[i..])?;
                        if fwd >= BLOCK {
                            let back = match_backward(self.source, src, &buf[lit..i])?;
                            flush(lit, i - back, f)?;
                            f(Op::Copy {
                                src: src - back as u64,
                                len: (back + fwd) as u64,
                            })?;
                            i += fwd;
                            lit = i;
                            continue;
                        }
                    }
                }
                i += 1;
            }
            flush(lit, n, f)?;
            off += n as u64;
            self.prog.add(n as u64);
        }
        Ok(())
    }
}

/// Generates the operations rebuilding `target` from `source`. With
/// `delta`, equal-length changes are reported as [`Op::Delta`] (bsdiff).
pub(crate) fn generate(
    source: &Tree,
    target: &Tree,
    delta: bool,
    cancel: &Cancel,
    progress: Progress<'_>,
    f: &mut dyn FnMut(Op) -> Result<()>,
) -> Result<()> {
    let tl = target.len();
    let half = |done: u64, total: u64| {
        let frac = if total == 0 {
            1.0
        } else {
            done as f64 / total as f64
        };
        progress((frac * tl as f64 / 2.0) as u64, tl);
    };
    let opts = DiffOptions {
        include_equal: true,
        max_regions: None,
        ..DiffOptions::default()
    };
    let mut regions: Vec<Region> = Vec::new();
    diff_each(source, target, &opts, cancel, &half, &mut |r| {
        regions.push(r);
        Ok(())
    })?;
    let mut prog = Tracker::new(progress, tl);
    prog.set(tl / 2);
    let mut g = Gen {
        source,
        target,
        cancel,
        index: None,
        prog,
    };
    // The alignment covered the first half; literal scanning the second.
    for r in regions {
        match r.kind {
            RegionKind::Equal => {
                f(Op::Copy {
                    src: r.a.start,
                    len: r.a.len,
                })?;
            }
            RegionKind::Deleted => {}
            RegionKind::Changed if delta && r.a.len == r.b.len => {
                f(Op::Delta {
                    src: r.a.start,
                    len: r.a.len,
                })?;
            }
            RegionKind::Changed | RegionKind::Inserted => g.refine(r.b.start, r.b.len, f)?,
        }
    }
    g.prog.finish();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    fn ops(src: &[u8], tgt: &[u8], delta: bool) -> Vec<Op> {
        let mut out = Vec::new();
        generate(
            &tree(src),
            &tree(tgt),
            delta,
            &Cancel::new(),
            &noprog,
            &mut |o| {
                out.push(o);
                Ok(())
            },
        )
        .unwrap();
        out
    }

    fn rebuild(src: &[u8], tgt: &[u8], ops: &[Op]) -> Vec<u8> {
        let mut out = Vec::new();
        for &o in ops {
            match o {
                Op::Copy { src: s, len } => {
                    out.extend_from_slice(&src[s as usize..(s + len) as usize]);
                }
                Op::Delta { len, .. } | Op::Literal { len, .. } => {
                    let at = out.len();
                    out.extend_from_slice(&tgt[at..at + len as usize]);
                }
                Op::Run { byte, len } => out.extend(std::iter::repeat_n(byte, len as usize)),
            }
        }
        out
    }

    #[test]
    fn finds_moved_blocks_and_runs() {
        let a = noise(1, 200_000);
        let mut b = a[100_000..150_000].to_vec();
        b.extend_from_slice(&[7u8; 1000]);
        b.extend_from_slice(&a[..100_000]);
        let o = ops(&a, &b, false);
        assert_eq!(rebuild(&a, &b, &o), b);
        let literal: u64 = o
            .iter()
            .filter(|o| matches!(o, Op::Literal { .. }))
            .map(|o| o.len())
            .sum();
        assert!(literal < 200, "{o:?}");
        assert!(o.contains(&Op::Run { byte: 7, len: 1000 }));
    }

    #[test]
    fn delta_ops_for_same_length_changes() {
        let a: Vec<u8> = noise(2, 10_000).iter().map(|x| x & 0x7F).collect();
        let mut b = a.clone();
        for x in &mut b[5000..5100] {
            *x |= 0x80;
        }
        let o = ops(&a, &b, true);
        assert!(
            o.contains(&Op::Delta {
                src: 5000,
                len: 100
            }),
            "{o:?}"
        );
        assert_eq!(rebuild(&a, &b, &o), b);
    }
}
