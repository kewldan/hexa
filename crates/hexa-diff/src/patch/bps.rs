//! BPS: `BPS1`, source size, target size, metadata, actions
//! `((length - 1) << 2) | action` with `SourceRead`, `TargetRead`,
//! `SourceCopy` and `TargetCopy`, then CRC-32 of source, target and patch.

use std::sync::Arc;

use hexa_core::task::Progress;
use hexa_core::{Cancel, Result, Tree};

use super::PatchInfo;
use super::io::{Builder, Reader, invalid, write_num};
use super::ops::{Op, generate};
use crate::chunk::{Tracker, crc32, for_each_chunk};

const MAGIC: &[u8] = b"BPS1";
const SOURCE_READ: u64 = 0;
const TARGET_READ: u64 = 1;
const SOURCE_COPY: u64 = 2;
const TARGET_COPY: u64 = 3;
/// Runs at least this long become `TargetRead` + `TargetCopy`.
const RUN_MIN: u64 = 4;

struct Header {
    source_size: u64,
    target_size: u64,
    metadata: (usize, usize),
    body: usize,
    source_crc: u32,
    target_crc: u32,
    patch_crc: u32,
}

fn header(patch: &[u8]) -> Result<Header> {
    if patch.len() < MAGIC.len() + 3 + 12 || !patch.starts_with(MAGIC) {
        return Err(invalid("not a BPS patch"));
    }
    let end = patch.len() - 12;
    let mut r = Reader::new(&patch[..end]);
    r.bytes(MAGIC.len())?;
    let source_size = r.num()?;
    let target_size = r.num()?;
    let meta_len = usize::try_from(r.num()?).map_err(|_| invalid("BPS metadata too long"))?;
    let meta_pos = r.pos;
    r.bytes(meta_len)?;
    let mut f = Reader::new(&patch[end..]);
    Ok(Header {
        source_size,
        target_size,
        metadata: (meta_pos, meta_len),
        body: r.pos,
        source_crc: f.u32_le()?,
        target_crc: f.u32_le()?,
        patch_crc: f.u32_le()?,
    })
}

pub(super) fn inspect(patch: &[u8]) -> Result<PatchInfo> {
    let h = header(patch)?;
    let (mp, ml) = h.metadata;
    Ok(PatchInfo {
        source_size: Some(h.source_size),
        target_size: Some(h.target_size),
        source_crc32: Some(h.source_crc),
        target_crc32: Some(h.target_crc),
        patch_checksum_ok: Some(crc32fast::hash(&patch[..patch.len() - 4]) == h.patch_crc),
        metadata: (ml > 0).then(|| String::from_utf8_lossy(&patch[mp..mp + ml]).into_owned()),
        ..PatchInfo::default()
    })
}

fn signed(r: &mut Reader<'_>, base: u64) -> Result<u64> {
    let v = r.num()?;
    let mag = v >> 1;
    let out = if v & 1 == 1 {
        base.checked_sub(mag)
    } else {
        base.checked_add(mag)
    };
    out.ok_or_else(|| invalid("BPS relative offset out of range"))
}

pub(super) fn apply(
    patch: &Arc<[u8]>,
    source: &Tree,
    verify: bool,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<Tree> {
    let h = header(patch)?;
    if verify {
        if crc32fast::hash(&patch[..patch.len() - 4]) != h.patch_crc {
            return Err(invalid("BPS patch checksum mismatch (corrupted patch)"));
        }
        if source.len() != h.source_size || crc32(source, cancel)? != h.source_crc {
            return Err(invalid(
                "input does not match the BPS patch source (size or CRC-32 differ)",
            ));
        }
    }
    let end = patch.len() - 12;
    let mut r = Reader::new(&patch[..end]);
    r.pos = h.body;
    let mut b = Builder::new(Arc::clone(patch));
    let mut prog = Tracker::new(progress, h.target_size);
    let (mut src_rel, mut tgt_rel) = (0u64, 0u64);
    let mut actions = 0u64;
    while r.remaining() > 0 {
        actions += 1;
        if actions.is_multiple_of(1024) {
            cancel.check()?;
            prog.set(b.len());
        }
        let data = r.num()?;
        let len = (data >> 2) + 1;
        let out = b.len();
        if out.checked_add(len).is_none_or(|e| e > h.target_size) {
            return Err(invalid("BPS action writes past the target size"));
        }
        match data & 3 {
            SOURCE_READ => b.push_source(source, out, len)?,
            TARGET_READ => {
                let pos = r.pos;
                r.bytes(len as usize)?;
                b.push_patch(pos, len as usize);
            }
            SOURCE_COPY => {
                src_rel = signed(&mut r, src_rel)?;
                b.push_source(source, src_rel, len)?;
                src_rel += len;
            }
            TARGET_COPY => {
                tgt_rel = signed(&mut r, tgt_rel)?;
                if tgt_rel >= out {
                    return Err(invalid("BPS TargetCopy reads unwritten data"));
                }
                b.copy_self(tgt_rel, len)?;
                tgt_rel += len;
            }
            _ => unreachable!(),
        }
    }
    if b.len() != h.target_size {
        return Err(invalid("BPS patch produced the wrong target size"));
    }
    let target = b.finish();
    if verify && crc32(&target, cancel)? != h.target_crc {
        return Err(invalid("BPS output checksum mismatch"));
    }
    prog.finish();
    Ok(target)
}

fn action(out: &mut Vec<u8>, kind: u64, len: u64) {
    write_num(out, ((len - 1) << 2) | kind);
}

fn relative(out: &mut Vec<u8>, from: u64, to: u64) {
    let v = if to >= from {
        (to - from) << 1
    } else {
        ((from - to) << 1) | 1
    };
    write_num(out, v);
}

pub(super) fn create(
    source: &Tree,
    target: &Tree,
    metadata: &str,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<Vec<u8>> {
    let mut out = MAGIC.to_vec();
    write_num(&mut out, source.len());
    write_num(&mut out, target.len());
    write_num(&mut out, metadata.len() as u64);
    out.extend_from_slice(metadata.as_bytes());
    let (mut pos, mut src_rel, mut tgt_rel) = (0u64, 0u64, 0u64);
    generate(source, target, false, cancel, progress, &mut |op| {
        match op {
            Op::Copy { src, len } if src == pos => action(&mut out, SOURCE_READ, len),
            Op::Copy { src, len } => {
                action(&mut out, SOURCE_COPY, len);
                relative(&mut out, src_rel, src);
                src_rel = src + len;
            }
            Op::Run { byte, len } if len >= RUN_MIN => {
                action(&mut out, TARGET_READ, 1);
                out.push(byte);
                action(&mut out, TARGET_COPY, len - 1);
                relative(&mut out, tgt_rel, pos);
                tgt_rel = pos + len - 1;
            }
            Op::Run { byte, len } => {
                action(&mut out, TARGET_READ, len);
                out.extend(std::iter::repeat_n(byte, len as usize));
            }
            Op::Literal { len, .. } | Op::Delta { len, .. } => {
                action(&mut out, TARGET_READ, len);
                for_each_chunk(target, pos, pos + len, cancel, |_, c| {
                    out.extend_from_slice(c);
                    Ok(())
                })?;
            }
        }
        pos += op.len();
        Ok(())
    })?;
    out.extend_from_slice(&crc32(source, cancel)?.to_le_bytes());
    out.extend_from_slice(&crc32(target, cancel)?.to_le_bytes());
    let crc = crc32fast::hash(&out);
    out.extend_from_slice(&crc.to_le_bytes());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::{ApplyOptions, apply_patch, inspect_patch};
    use crate::testutil::*;

    #[test]
    fn hand_made_fixture() {
        let src = b"Hello, world";
        let tgt = b"Hello, Hello!!!!!";
        let mut p = b"BPS1".to_vec();
        p.extend_from_slice(&[0x8C, 0x91, 0x83, b'x', b'm', b'l']);
        // SourceRead 7 ("Hello, ")
        p.push(0x80 | (6 << 2));
        // SourceCopy 5 from offset 0 (relative +0)
        p.push(0x80 | (4 << 2) | 2);
        p.push(0x80);
        // TargetRead 1 "!"
        p.extend_from_slice(&[0x80 | 1, b'!']);
        // TargetCopy 4 from target offset 12 (relative +12)
        p.push(0x80 | (3 << 2) | 3);
        p.push(0x80 | 0x18);
        p.extend_from_slice(&crc32fast::hash(src).to_le_bytes());
        p.extend_from_slice(&crc32fast::hash(tgt).to_le_bytes());
        let c = crc32fast::hash(&p);
        p.extend_from_slice(&c.to_le_bytes());
        let o = ApplyOptions::default();
        let a = apply_patch(None, &p, &tree(src), &o, &Cancel::new(), &noprog).unwrap();
        assert_eq!(bytes(&a.target), tgt);
        let info = inspect_patch(&p).unwrap();
        assert_eq!(info.metadata.as_deref(), Some("xml"));
        assert_eq!(info.target_size, Some(17));
        assert!(
            apply_patch(
                None,
                &p,
                &tree(b"Hello, World"),
                &o,
                &Cancel::new(),
                &noprog
            )
            .is_err()
        );
    }
}
