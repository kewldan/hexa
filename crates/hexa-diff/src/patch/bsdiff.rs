//! bsdiff 4 (`BSDIFF40`): 32-byte header, then bzip2-compressed control,
//! diff and extra blocks. Control triples `(x, y, z)`: add `x` diff bytes
//! to old data, copy `y` extra bytes, seek the old position by `z`.

use std::io::{Read, Write};
use std::sync::Arc;

use bzip2::Compression;
use bzip2::read::BzDecoder;
use bzip2::write::BzEncoder;
use hexa_core::task::Progress;
use hexa_core::{Cancel, Error, Result, Tree};

use super::PatchInfo;
use super::io::{Builder, invalid};
use super::ops::{Op, generate};
use crate::chunk::{Cursor, read};

pub(super) const MAGIC: &[u8] = b"BSDIFF40";
const HEADER: usize = 32;
const BUF: usize = 1 << 16;
/// Zero diff bytes at least this long become references to the old data.
const ZERO_RUN: usize = 256;

fn offt_read(b: &[u8]) -> i64 {
    let v = u64::from_le_bytes(b.try_into().expect("8 bytes"));
    let mag = (v & !(1 << 63)) as i64;
    if v >> 63 == 1 { -mag } else { mag }
}

fn offt_write(x: i64) -> [u8; 8] {
    let mut b = x.unsigned_abs().to_le_bytes();
    if x < 0 {
        b[7] |= 0x80;
    }
    b
}

struct Header {
    ctrl: usize,
    diff: usize,
    new_size: u64,
}

fn header(patch: &[u8]) -> Result<Header> {
    if patch.len() < HEADER || !patch.starts_with(MAGIC) {
        return Err(invalid("not a bsdiff patch"));
    }
    let ctrl = offt_read(&patch[8..16]);
    let diff = offt_read(&patch[16..24]);
    let new_size = offt_read(&patch[24..32]);
    let fits = |v: i64| usize::try_from(v).ok();
    match (fits(ctrl), fits(diff), u64::try_from(new_size)) {
        (Some(c), Some(d), Ok(n))
            if c.checked_add(d).is_some_and(|s| s <= patch.len() - HEADER) =>
        {
            Ok(Header {
                ctrl: c,
                diff: d,
                new_size: n,
            })
        }
        _ => Err(invalid("corrupt bsdiff header")),
    }
}

pub(super) fn inspect(patch: &[u8]) -> Result<PatchInfo> {
    let h = header(patch)?;
    Ok(PatchInfo {
        target_size: Some(h.new_size),
        ..PatchInfo::default()
    })
}

fn bz_err(_: std::io::Error) -> Error {
    invalid("corrupt bzip2 block in bsdiff patch")
}

fn usize_of(v: i64) -> Result<u64> {
    u64::try_from(v).map_err(|_| invalid("negative length in bsdiff control block"))
}

/// Appends `diff[i] + old[oldpos + i]` (old bytes outside the file count as
/// zero), referencing the old data for long runs of zero diff bytes.
fn add_diff(
    b: &mut Builder,
    source: &Tree,
    oldpos: i64,
    diff: &[u8],
    old: &mut Vec<u8>,
) -> Result<()> {
    let n = diff.len();
    let sl = source.len() as i64;
    old.clear();
    old.resize(n, 0);
    let lo = oldpos.max(0);
    let hi = (oldpos + n as i64).min(sl);
    if lo < hi {
        let at = (lo - oldpos) as usize;
        read(source, lo as u64, &mut old[at..at + (hi - lo) as usize])?;
    }
    let in_range = |i: usize| {
        let p = oldpos + i as i64;
        p >= 0 && p < sl
    };
    let mut i = 0;
    let mut computed = Vec::with_capacity(n);
    while i < n {
        if diff[i] == 0 && in_range(i) {
            let run = diff[i..]
                .iter()
                .enumerate()
                .take_while(|&(k, &d)| d == 0 && in_range(i + k))
                .count();
            if run >= ZERO_RUN {
                b.push_bytes(&computed);
                computed.clear();
                b.push_source(source, (oldpos + i as i64) as u64, run as u64)?;
                i += run;
                continue;
            }
        }
        computed.push(diff[i].wrapping_add(old[i]));
        i += 1;
    }
    b.push_bytes(&computed);
    Ok(())
}

pub(super) fn apply(
    patch: &Arc<[u8]>,
    source: &Tree,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<Tree> {
    let h = header(patch)?;
    let c0 = HEADER;
    let d0 = c0 + h.ctrl;
    let e0 = d0 + h.diff;
    let mut ctrl = BzDecoder::new(&patch[c0..d0]);
    let mut diff = BzDecoder::new(&patch[d0..e0]);
    let mut extra = BzDecoder::new(&patch[e0..]);
    let mut b = Builder::new(Arc::clone(patch));
    let mut buf = vec![0u8; BUF];
    let mut old = Vec::with_capacity(BUF);
    let mut oldpos = 0i64;
    while b.len() < h.new_size {
        cancel.check()?;
        progress(b.len(), h.new_size);
        let mut t = [0u8; 24];
        ctrl.read_exact(&mut t).map_err(bz_err)?;
        let x = usize_of(offt_read(&t[0..8]))?;
        let y = usize_of(offt_read(&t[8..16]))?;
        let z = offt_read(&t[16..24]);
        if b.len().checked_add(x).is_none_or(|e| e > h.new_size) {
            return Err(invalid("bsdiff control block writes past the new size"));
        }
        let mut left = x;
        while left > 0 {
            cancel.check()?;
            let n = left.min(BUF as u64) as usize;
            diff.read_exact(&mut buf[..n]).map_err(bz_err)?;
            add_diff(&mut b, source, oldpos, &buf[..n], &mut old)?;
            oldpos = oldpos.saturating_add(n as i64);
            left -= n as u64;
        }
        if b.len().checked_add(y).is_none_or(|e| e > h.new_size) {
            return Err(invalid("bsdiff control block writes past the new size"));
        }
        let mut left = y;
        while left > 0 {
            let n = left.min(BUF as u64) as usize;
            extra.read_exact(&mut buf[..n]).map_err(bz_err)?;
            b.push_bytes(&buf[..n]);
            left -= n as u64;
        }
        oldpos = oldpos.saturating_add(z);
    }
    progress(h.new_size, h.new_size);
    Ok(b.finish())
}

struct Encoder {
    ctrl: BzEncoder<Vec<u8>>,
    diff: BzEncoder<Vec<u8>>,
    extra: BzEncoder<Vec<u8>>,
    cur_old: i64,
    x: u64,
    y: u64,
}

impl Encoder {
    fn close(&mut self, next_old: i64) -> Result<()> {
        let z = next_old - (self.cur_old + self.x as i64);
        if self.x > 0 || self.y > 0 || z != 0 {
            let mut t = [0u8; 24];
            t[0..8].copy_from_slice(&offt_write(self.x as i64));
            t[8..16].copy_from_slice(&offt_write(self.y as i64));
            t[16..24].copy_from_slice(&offt_write(z));
            self.ctrl.write_all(&t)?;
        }
        self.cur_old = next_old;
        self.x = 0;
        self.y = 0;
        Ok(())
    }
}

pub(super) fn create(
    source: &Tree,
    target: &Tree,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<Vec<u8>> {
    let mut enc = Encoder {
        ctrl: BzEncoder::new(Vec::new(), Compression::best()),
        diff: BzEncoder::new(Vec::new(), Compression::best()),
        extra: BzEncoder::new(Vec::new(), Compression::best()),
        cur_old: 0,
        x: 0,
        y: 0,
    };
    let mut cs = Cursor::new(source);
    let mut ct = Cursor::new(target);
    let (mut sb, mut tb) = (vec![0u8; BUF], vec![0u8; BUF]);
    let mut pos = 0u64;
    generate(source, target, true, cancel, progress, &mut |op| {
        match op {
            Op::Copy { src, len } | Op::Delta { src, len } => {
                if !(enc.y == 0 && src as i64 == enc.cur_old + enc.x as i64) {
                    enc.close(src as i64)?;
                }
                enc.x += len;
                let mut off = 0;
                while off < len {
                    let n = (len - off).min(BUF as u64) as usize;
                    cs.fill(src + off, &mut sb[..n])?;
                    ct.fill(pos + off, &mut tb[..n])?;
                    for (t, s) in tb[..n].iter_mut().zip(&sb[..n]) {
                        *t = t.wrapping_sub(*s);
                    }
                    enc.diff.write_all(&tb[..n])?;
                    off += n as u64;
                }
            }
            Op::Literal { len, .. } | Op::Run { len, .. } => {
                enc.y += len;
                let mut off = 0;
                while off < len {
                    let n = (len - off).min(BUF as u64) as usize;
                    match op {
                        Op::Run { byte, .. } => tb[..n].fill(byte),
                        _ => ct.fill(pos + off, &mut tb[..n])?,
                    }
                    enc.extra.write_all(&tb[..n])?;
                    off += n as u64;
                }
            }
        }
        pos += op.len();
        Ok(())
    })?;
    let end = enc.cur_old + enc.x as i64;
    enc.close(end)?;
    let ctrl = enc.ctrl.finish()?;
    let diff = enc.diff.finish()?;
    let extra = enc.extra.finish()?;
    let mut out = Vec::with_capacity(HEADER + ctrl.len() + diff.len() + extra.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&offt_write(ctrl.len() as i64));
    out.extend_from_slice(&offt_write(diff.len() as i64));
    out.extend_from_slice(&offt_write(target.len() as i64));
    out.extend_from_slice(&ctrl);
    out.extend_from_slice(&diff);
    out.extend_from_slice(&extra);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::{ApplyOptions, apply_patch, inspect_patch};
    use crate::testutil::*;

    fn bz(data: &[u8]) -> Vec<u8> {
        let mut e = BzEncoder::new(Vec::new(), Compression::best());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    fn triple(x: i64, y: i64, z: i64) -> Vec<u8> {
        [offt_write(x), offt_write(y), offt_write(z)].concat()
    }

    #[test]
    fn offt_encoding() {
        for v in [0i64, 1, -1, 300, -300, i64::MAX] {
            assert_eq!(offt_read(&offt_write(v)), v);
        }
        assert_eq!(offt_write(-2), [2, 0, 0, 0, 0, 0, 0, 0x80]);
    }

    #[test]
    fn hand_made_fixture() {
        let old = b"abcdefgh";
        // new = (old[0..4] + [0,0,1,0]) ++ "XYZ" ++ (old[2..5] + 0) ++ seek
        let ctrl = [triple(4, 3, -2), triple(3, 0, 0)].concat();
        let diff = [0u8, 0, 1, 0, 0, 0, 0];
        let extra = b"XYZ";
        let (c, d, e) = (bz(&ctrl), bz(&diff), bz(extra));
        let mut p = MAGIC.to_vec();
        p.extend_from_slice(&offt_write(c.len() as i64));
        p.extend_from_slice(&offt_write(d.len() as i64));
        p.extend_from_slice(&offt_write(10));
        p.extend([c, d, e].concat());
        let o = ApplyOptions::default();
        let a = apply_patch(None, &p, &tree(old), &o, &Cancel::new(), &noprog).unwrap();
        assert_eq!(bytes(&a.target), b"abddXYZcde");
        assert_eq!(inspect_patch(&p).unwrap().target_size, Some(10));
        // Corrupt header lengths.
        let mut bad = p.clone();
        bad[8] = 0xFF;
        assert!(apply_patch(None, &bad, &tree(old), &o, &Cancel::new(), &noprog).is_err());
    }

    #[test]
    fn old_positions_outside_the_file_read_as_zero() {
        let ctrl = triple(4, 0, 0);
        let (c, d, e) = (bz(&ctrl), bz(&[1, 2, 3, 4]), bz(&[]));
        let mut p = MAGIC.to_vec();
        p.extend_from_slice(&offt_write(c.len() as i64));
        p.extend_from_slice(&offt_write(d.len() as i64));
        p.extend_from_slice(&offt_write(4));
        p.extend([c, d, e].concat());
        let a = apply_patch(
            None,
            &p,
            &tree(b"\x10\x10"),
            &ApplyOptions::default(),
            &Cancel::new(),
            &noprog,
        )
        .unwrap();
        assert_eq!(bytes(&a.target), [0x11, 0x12, 3, 4]);
    }
}
