//! UPS: `UPS1`, source size, target size, records `skip, xor bytes…, 0`,
//! then CRC-32 of source, target and the patch itself (little endian).

use std::sync::Arc;

use hexa_core::task::Progress;
use hexa_core::{Cancel, Result, Tree};

use super::PatchInfo;
use super::io::{Builder, Reader, invalid, write_num};
use crate::chunk::{CHUNK, Cursor, Tracker, crc32, mismatch};

const MAGIC: &[u8] = b"UPS1";

struct Header {
    source_size: u64,
    target_size: u64,
    body: usize,
    source_crc: u32,
    target_crc: u32,
    patch_crc: u32,
}

fn header(patch: &[u8]) -> Result<Header> {
    if patch.len() < MAGIC.len() + 2 + 12 || !patch.starts_with(MAGIC) {
        return Err(invalid("not a UPS patch"));
    }
    let end = patch.len() - 12;
    let mut r = Reader::new(&patch[..end]);
    r.bytes(MAGIC.len())?;
    let source_size = r.num()?;
    let target_size = r.num()?;
    let mut f = Reader::new(&patch[end..]);
    Ok(Header {
        source_size,
        target_size,
        body: r.pos,
        source_crc: f.u32_le()?,
        target_crc: f.u32_le()?,
        patch_crc: f.u32_le()?,
    })
}

pub(super) fn inspect(patch: &[u8]) -> Result<PatchInfo> {
    let h = header(patch)?;
    Ok(PatchInfo {
        source_size: Some(h.source_size),
        target_size: Some(h.target_size),
        source_crc32: Some(h.source_crc),
        target_crc32: Some(h.target_crc),
        patch_checksum_ok: Some(crc32fast::hash(&patch[..patch.len() - 4]) == h.patch_crc),
        ..PatchInfo::default()
    })
}

/// Appends unchanged bytes `[pos, pos+n)` clipped to `out_size`.
fn unchanged(b: &mut Builder, source: &Tree, pos: u64, n: u64, out_size: u64) -> Result<()> {
    let end = pos.saturating_add(n).min(out_size);
    if pos >= end {
        return Ok(());
    }
    let src_end = end.min(source.len());
    if pos < src_end {
        b.push_source(source, pos, src_end - pos)?;
    }
    let zero_from = pos.max(src_end);
    if end > zero_from {
        b.push_fill(&[0], end - zero_from);
    }
    Ok(())
}

/// Returns the target and whether the patch was applied in reverse.
pub(super) fn apply(
    patch: &Arc<[u8]>,
    source: &Tree,
    verify: bool,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<(Tree, bool)> {
    let h = header(patch)?;
    if verify && crc32fast::hash(&patch[..patch.len() - 4]) != h.patch_crc {
        return Err(invalid("UPS patch checksum mismatch (corrupted patch)"));
    }
    let len = source.len();
    let reversed = if verify {
        let crc = crc32(source, cancel)?;
        if len == h.source_size && crc == h.source_crc {
            false
        } else if len == h.target_size && crc == h.target_crc {
            true
        } else {
            return Err(invalid(
                "input does not match the UPS patch (size or CRC-32 differ)",
            ));
        }
    } else {
        len != h.source_size && len == h.target_size
    };
    let (out_size, expected) = if reversed {
        (h.source_size, h.source_crc)
    } else {
        (h.target_size, h.target_crc)
    };
    let end = patch.len() - 12;
    let mut r = Reader::new(&patch[..end]);
    r.pos = h.body;
    let mut b = Builder::new(Arc::clone(patch));
    let mut cur = Cursor::new(source);
    let mut prog = Tracker::new(progress, end as u64);
    let mut pos = 0u64;
    let mut records = 0u64;
    while r.remaining() > 0 {
        records += 1;
        if records.is_multiple_of(1024) {
            cancel.check()?;
            prog.set(r.pos as u64);
        }
        let skip = r.num()?;
        unchanged(&mut b, source, pos, skip, out_size)?;
        pos = pos
            .checked_add(skip)
            .ok_or_else(|| invalid("UPS offset overflows"))?;
        loop {
            let x = r.u8()?;
            if x == 0 {
                unchanged(&mut b, source, pos, 1, out_size)?;
                pos = pos.saturating_add(1);
                break;
            }
            if pos < out_size {
                b.push_byte(cur.get(pos)?.unwrap_or(0) ^ x);
            }
            pos = pos.saturating_add(1);
        }
    }
    unchanged(&mut b, source, pos, out_size.saturating_sub(pos), out_size)?;
    let target = b.finish();
    debug_assert_eq!(target.len(), out_size);
    if verify && crc32(&target, cancel)? != expected {
        return Err(invalid("UPS output checksum mismatch"));
    }
    prog.finish();
    Ok((target, reversed))
}

pub(super) fn create(
    source: &Tree,
    target: &Tree,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<Vec<u8>> {
    let (sl, tl) = (source.len(), target.len());
    let max = sl.max(tl);
    let mut out = MAGIC.to_vec();
    write_num(&mut out, sl);
    write_num(&mut out, tl);
    let mut hs = crc32fast::Hasher::new();
    let mut ht = crc32fast::Hasher::new();
    let (mut cs, mut ct) = (Cursor::new(source), Cursor::new(target));
    let mut sb = vec![0u8; CHUNK.min(max as usize)];
    let mut tb = sb.clone();
    let mut prog = Tracker::new(progress, max);
    let mut last = 0u64;
    let mut in_run = false;
    let mut pos = 0u64;
    while pos < max {
        cancel.check()?;
        let n = (max - pos).min(CHUNK as u64) as usize;
        let (sb, tb) = (&mut sb[..n], &mut tb[..n]);
        cs.fill(pos, sb)?;
        ct.fill(pos, tb)?;
        hs.update(&sb[..sl.saturating_sub(pos).min(n as u64) as usize]);
        ht.update(&tb[..tl.saturating_sub(pos).min(n as u64) as usize]);
        let mut i = 0;
        while i < n {
            if !in_run {
                i += mismatch(&sb[i..], &tb[i..]);
                if i == n {
                    break;
                }
                write_num(&mut out, pos + i as u64 - last);
                in_run = true;
            }
            while i < n && sb[i] != tb[i] {
                out.push(sb[i] ^ tb[i]);
                i += 1;
            }
            if i < n {
                out.push(0);
                last = pos + i as u64 + 1;
                in_run = false;
                i += 1;
            }
        }
        pos += n as u64;
        prog.add(n as u64);
    }
    if in_run {
        out.push(0);
    }
    out.extend_from_slice(&hs.finalize().to_le_bytes());
    out.extend_from_slice(&ht.finalize().to_le_bytes());
    let crc = crc32fast::hash(&out);
    out.extend_from_slice(&crc.to_le_bytes());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::{ApplyOptions, apply_patch, inspect_patch};
    use crate::testutil::*;

    fn finish(mut body: Vec<u8>, src: &[u8], tgt: &[u8]) -> Vec<u8> {
        body.extend_from_slice(&crc32fast::hash(src).to_le_bytes());
        body.extend_from_slice(&crc32fast::hash(tgt).to_le_bytes());
        let c = crc32fast::hash(&body);
        body.extend_from_slice(&c.to_le_bytes());
        body
    }

    #[test]
    fn hand_made_fixture_both_directions() {
        let src = b"ABCDEFGH";
        let tgt = b"ABxDEFGHij";
        // sizes 8 / 10; skip 2, xor 'C'^'x', end; skip 4 (D..G… then H), xor 'i','j'.
        let mut p = b"UPS1".to_vec();
        p.extend_from_slice(&[0x88, 0x8A]);
        p.extend_from_slice(&[0x82, b'C' ^ b'x', 0]);
        p.extend_from_slice(&[0x84, b'i', b'j', 0]);
        let p = finish(p, src, tgt);
        let c = Cancel::new();
        let o = ApplyOptions::default();
        let a = apply_patch(None, &p, &tree(src), &o, &c, &noprog).unwrap();
        assert_eq!(bytes(&a.target), tgt);
        assert!(!a.reversed);
        let back = apply_patch(None, &p, &tree(tgt), &o, &c, &noprog).unwrap();
        assert_eq!(bytes(&back.target), src);
        assert!(back.reversed);
        assert!(apply_patch(None, &p, &tree(b"ABCDEFGX"), &o, &c, &noprog).is_err());
        let info = inspect_patch(&p).unwrap();
        assert_eq!((info.source_size, info.target_size), (Some(8), Some(10)));
        assert_eq!(info.patch_checksum_ok, Some(true));
        // Our encoder produces the same bytes.
        let made = create(&tree(src), &tree(tgt), &c, &noprog).unwrap();
        assert_eq!(made, p);
        let mut bad = p.clone();
        bad[6] ^= 1;
        assert!(apply_patch(None, &bad, &tree(src), &o, &c, &noprog).is_err());
        let lax = ApplyOptions { verify: false };
        assert!(apply_patch(None, &bad, &tree(src), &lax, &c, &noprog).is_ok());
    }
}
