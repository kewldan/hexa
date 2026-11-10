//! IPS: `PATCH`, records `offset:u24be size:u16be data` (size 0 → RLE
//! record `count:u16be byte`), `EOF`, optional `u24be` truncation length.

use std::sync::Arc;

use hexa_core::piece::{Piece, Source};
use hexa_core::task::Progress;
use hexa_core::{Cancel, Error, Result, Tree};

use super::PatchInfo;
use super::io::{Reader, invalid};
use crate::chunk::{Tracker, read};
use crate::tree_to_vec;

const MAGIC: &[u8] = b"PATCH";
/// `"EOF"` read as an offset.
const EOF_OFF: usize = 0x0045_4F46;
/// Largest target IPS can describe (24-bit offsets).
const MAX_TARGET: u64 = 1 << 24;
/// Minimum repeat length worth an RLE record inside a normal record.
const RLE_MIN: usize = 14;
/// Equal bytes worth bridging instead of starting a new record.
const BRIDGE: usize = 5;

enum Body {
    Data { pos: usize, len: usize },
    Rle { byte: u8, len: usize },
}

struct Record {
    offset: u64,
    body: Body,
}

fn parse(patch: &[u8]) -> Result<(Vec<Record>, Option<u64>)> {
    let mut r = Reader::new(patch);
    if r.bytes(MAGIC.len())? != MAGIC {
        return Err(invalid("not an IPS patch"));
    }
    let mut records = Vec::new();
    loop {
        if r.remaining() < 3 {
            return Err(invalid("IPS patch lacks the EOF marker"));
        }
        let offset = r.be(3)?;
        if offset == EOF_OFF as u64 {
            let truncate = match r.remaining() {
                0 => None,
                3 => Some(r.be(3)?),
                n => {
                    return Err(invalid(format!(
                        "{n} unexpected bytes after IPS EOF marker"
                    )));
                }
            };
            return Ok((records, truncate));
        }
        let size = r.be(2)? as usize;
        let body = if size == 0 {
            let len = r.be(2)? as usize;
            let byte = r.u8()?;
            if len == 0 {
                return Err(invalid("IPS RLE record of length 0"));
            }
            Body::Rle { byte, len }
        } else {
            let pos = r.pos;
            r.bytes(size)?;
            Body::Data { pos, len: size }
        };
        records.push(Record { offset, body });
    }
}

pub(super) fn inspect(patch: &[u8]) -> Result<PatchInfo> {
    let (_, truncate) = parse(patch)?;
    Ok(PatchInfo {
        target_size: truncate,
        ..PatchInfo::default()
    })
}

fn zeros(n: u64) -> Tree {
    Tree::from_piece(Piece::fill(vec![0u8], n))
}

pub(super) fn apply(
    patch: &Arc<[u8]>,
    source: &Tree,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<Tree> {
    let (records, truncate) = parse(patch)?;
    let mut prog = Tracker::new(progress, records.len() as u64);
    let mut out = source.clone();
    for (i, rec) in records.iter().enumerate() {
        if i.is_multiple_of(1024) {
            cancel.check()?;
            prog.set(i as u64);
        }
        let (piece, len) = match rec.body {
            Body::Data { pos, len } => (
                Piece {
                    source: Source::Bytes(Arc::clone(patch)),
                    offset: pos as u64,
                    len: len as u64,
                },
                len as u64,
            ),
            Body::Rle { byte, len } => (Piece::fill(vec![byte], len as u64), len as u64),
        };
        if rec.offset > out.len() {
            out = out.concat(&zeros(rec.offset - out.len()));
        }
        let end = (rec.offset + len).min(out.len());
        out = out.replace(rec.offset, end, &Tree::from_piece(piece));
    }
    if let Some(t) = truncate {
        out = if t <= out.len() {
            out.slice(0, t)
        } else {
            out.concat(&zeros(t - out.len()))
        };
    }
    prog.finish();
    Ok(out)
}

fn record(out: &mut Vec<u8>, offset: usize, data: &[u8]) {
    debug_assert!(!data.is_empty() && data.len() <= 0xFFFF && offset != EOF_OFF);
    out.extend_from_slice(&(offset as u32).to_be_bytes()[1..]);
    out.extend_from_slice(&(data.len() as u16).to_be_bytes());
    out.extend_from_slice(data);
}

fn normal(out: &mut Vec<u8>, t: &[u8], start: usize, end: usize) {
    let mut i = start;
    while i < end {
        let n = (end - i).min(0xFFFE);
        if i == EOF_OFF {
            record(out, i - 1, &t[i - 1..i + n]);
        } else {
            record(out, i, &t[i..i + n]);
        }
        i += n;
    }
}

fn rle(out: &mut Vec<u8>, t: &[u8], start: usize, end: usize) {
    let mut i = start;
    while i < end {
        if i == EOF_OFF {
            record(out, i - 1, &t[i - 1..=i]);
            i += 1;
            continue;
        }
        let n = (end - i).min(0xFFFF);
        out.extend_from_slice(&(i as u32).to_be_bytes()[1..]);
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(&(n as u16).to_be_bytes());
        out.push(t[i]);
        i += n;
    }
}

fn run_len(t: &[u8], i: usize, end: usize) -> usize {
    t[i..end].iter().take_while(|&&x| x == t[i]).count()
}

fn emit(out: &mut Vec<u8>, t: &[u8], start: usize, end: usize) {
    let mut i = start;
    while i < end {
        let r = run_len(t, i, end);
        if r >= RLE_MIN {
            rle(out, t, i, i + r);
            i += r;
            continue;
        }
        let mut k = i + r;
        while k < end {
            let r = run_len(t, k, end);
            if r >= RLE_MIN {
                break;
            }
            k += r;
        }
        normal(out, t, i, k);
        i = k;
    }
}

pub(super) fn create(
    source: &Tree,
    target: &Tree,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<Vec<u8>> {
    let (sl, tl) = (source.len(), target.len());
    if tl > MAX_TARGET || (tl < sl && tl >= MAX_TARGET) {
        return Err(Error::TooLarge {
            len: tl,
            limit: MAX_TARGET - 1,
        });
    }
    let mut prog = Tracker::new(progress, tl);
    let t = tree_to_vec(target, MAX_TARGET)?;
    let mut s = vec![0u8; sl.min(tl) as usize];
    read(source, 0, &mut s)?;
    cancel.check()?;
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < t.len() {
        if i < s.len() && s[i] == t[i] {
            i += crate::chunk::mismatch(&s[i..], &t[i..]);
            continue;
        }
        let start = i;
        while i < t.len() && (i >= s.len() || s[i] != t[i]) {
            i += 1;
        }
        match runs.last_mut() {
            Some(last) if start - last.1 <= BRIDGE => last.1 = i,
            _ => runs.push((start, i)),
        }
    }
    let mut out = MAGIC.to_vec();
    for (k, &(a, b)) in runs.iter().enumerate() {
        if k.is_multiple_of(1024) {
            cancel.check()?;
            prog.set(a as u64);
        }
        emit(&mut out, &t, a, b);
    }
    out.extend_from_slice(b"EOF");
    if tl < sl {
        out.extend_from_slice(&(tl as u32).to_be_bytes()[1..]);
    }
    prog.finish();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::ApplyOptions;
    use crate::testutil::*;

    fn apply_vec(p: &[u8], src: &[u8]) -> Result<Vec<u8>> {
        let a = crate::patch::apply_patch(
            None,
            p,
            &tree(src),
            &ApplyOptions::default(),
            &Cancel::new(),
            &noprog,
        )?;
        Ok(bytes(&a.target))
    }

    #[test]
    fn hand_made_fixture() {
        // Record at 2 writing "XY", RLE at 6 of 3×'z', record past the end.
        let mut p = b"PATCH".to_vec();
        p.extend_from_slice(&[0, 0, 2, 0, 2, b'X', b'Y']);
        p.extend_from_slice(&[0, 0, 6, 0, 0, 0, 3, b'z']);
        p.extend_from_slice(&[0, 0, 12, 0, 1, b'!']);
        p.extend_from_slice(b"EOF");
        assert_eq!(apply_vec(&p, b"0123456789").unwrap(), b"01XY45zzz9\0\0!");
        // Truncation extension.
        let mut t = p.clone();
        t.extend_from_slice(&[0, 0, 4]);
        assert_eq!(apply_vec(&t, b"0123456789").unwrap(), b"01XY");
        let info = crate::patch::inspect_patch(&t).unwrap();
        assert_eq!(info.target_size, Some(4));
    }

    #[test]
    fn invalid_patches() {
        assert!(apply_vec(b"PATCH", b"x").is_err());
        assert!(apply_vec(b"PATCH\0\0\x01\0\x05ab", b"x").is_err());
        assert!(apply_vec(b"PATCHEOF\0", b"x").is_err());
        assert!(apply_vec(b"PATCH\0\0\x01\0\0\0\0\x01EOF", b"x").is_err());
    }

    #[test]
    fn creates_rle_and_truncates() {
        let src = vec![1u8; 100];
        let mut tgt = src.clone();
        tgt[10..60].fill(9);
        tgt.truncate(80);
        let p = create(&tree(&src), &tree(&tgt), &Cancel::new(), &noprog).unwrap();
        // header + one RLE record + EOF + truncation.
        assert_eq!(
            p,
            [
                &b"PATCH"[..],
                &[0, 0, 10, 0, 0, 0, 50, 9],
                b"EOF",
                &[0, 0, 80]
            ]
            .concat()
        );
        assert_eq!(apply_vec(&p, &src).unwrap(), tgt);
    }

    #[test]
    fn avoids_eof_offset() {
        let src = vec![0u8; EOF_OFF + 100];
        let mut tgt = src.clone();
        tgt[EOF_OFF] = 1;
        tgt[EOF_OFF + 20..EOF_OFF + 60].fill(5);
        let p = create(&tree(&src), &tree(&tgt), &Cancel::new(), &noprog).unwrap();
        assert_eq!(&p[5..8], &[0x45, 0x4F, 0x45]);
        assert_eq!(apply_vec(&p, &src).unwrap(), tgt);
        let mut tgt = src.clone();
        tgt[EOF_OFF..EOF_OFF + 30].fill(7);
        let p = create(&tree(&src), &tree(&tgt), &Cancel::new(), &noprog).unwrap();
        assert_eq!(apply_vec(&p, &src).unwrap(), tgt);
    }

    #[test]
    fn too_large_target() {
        let t = Tree::from_piece(Piece::fill(vec![1u8], MAX_TARGET + 1));
        assert!(matches!(
            create(&Tree::new(), &t, &Cancel::new(), &noprog),
            Err(Error::TooLarge { .. })
        ));
    }
}
