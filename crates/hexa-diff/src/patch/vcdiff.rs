//! VCDIFF (RFC 3284) as produced and consumed by xdelta3: default code
//! table, `VCD_SOURCE` / `VCD_TARGET` windows, xdelta3's Adler-32 window
//! checksum (`VCD_ADLER32`, 4 bytes big endian after the section lengths).
//! Secondary compression and custom code tables are rejected.

use std::sync::Arc;
use std::sync::OnceLock;

use hexa_core::task::Progress;
use hexa_core::{Cancel, Result, Tree};

use super::PatchInfo;
use super::io::{Builder, Reader, invalid, vcd_int_len, write_vcd_int};
use super::ops::{Op, generate};
use crate::chunk::{Tracker, adler32, for_each_chunk};

pub(super) const MAGIC: [u8; 3] = [0xD6, 0xC3, 0xC4];

const VCD_DECOMPRESS: u8 = 0x01;
const VCD_CODETABLE: u8 = 0x02;
const VCD_APPHEADER: u8 = 0x04;
const VCD_SOURCE: u8 = 0x01;
const VCD_TARGET: u8 = 0x02;
const VCD_ADLER32: u8 = 0x04;

const NOOP: u8 = 0;
const ADD: u8 = 1;
const RUN: u8 = 2;
const COPY: u8 = 3;

const S_NEAR: usize = 4;
const S_SAME: usize = 3;

/// Target bytes per encoded window.
const WINDOW: u64 = 1 << 22;

#[derive(Clone, Copy, Debug, Default)]
struct Inst {
    kind: u8,
    size: u8,
    mode: u8,
}

type Table = [(Inst, Inst); 256];

/// The default code table of RFC 3284 section 5.6.
fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(|| {
        let none = Inst::default();
        let inst = |kind, size, mode| Inst { kind, size, mode };
        let mut t = [(none, none); 256];
        let mut i = 0;
        t[i] = (inst(RUN, 0, 0), none);
        i += 1;
        for size in 0..=17 {
            t[i] = (inst(ADD, size, 0), none);
            i += 1;
        }
        for mode in 0..=8 {
            t[i] = (inst(COPY, 0, mode), none);
            i += 1;
            for size in 4..=18 {
                t[i] = (inst(COPY, size, mode), none);
                i += 1;
            }
        }
        for mode in 0..=5 {
            for add in 1..=4 {
                for copy in 4..=6 {
                    t[i] = (inst(ADD, add, 0), inst(COPY, copy, mode));
                    i += 1;
                }
            }
        }
        for mode in 6..=8 {
            for add in 1..=4 {
                t[i] = (inst(ADD, add, 0), inst(COPY, 4, mode));
                i += 1;
            }
        }
        for mode in 0..=8 {
            t[i] = (inst(COPY, 4, mode), inst(ADD, 1, 0));
            i += 1;
        }
        assert_eq!(i, 256);
        t
    })
}

/// RFC 3284 address cache (`s_near` = 4, `s_same` = 3).
struct Cache {
    near: [u64; S_NEAR],
    next: usize,
    same: [u64; S_SAME * 256],
}

impl Cache {
    fn new() -> Self {
        Self {
            near: [0; S_NEAR],
            next: 0,
            same: [0; S_SAME * 256],
        }
    }

    fn update(&mut self, addr: u64) {
        self.near[self.next] = addr;
        self.next = (self.next + 1) % S_NEAR;
        self.same[(addr % (S_SAME as u64 * 256)) as usize] = addr;
    }

    fn decode(&mut self, r: &mut Reader<'_>, here: u64, mode: u8) -> Result<u64> {
        let m = mode as usize;
        let addr = match m {
            0 => r.vcd_int()?,
            1 => here
                .checked_sub(r.vcd_int()?)
                .ok_or_else(|| invalid("VCDIFF HERE address underflows"))?,
            _ if m < 2 + S_NEAR => self.near[m - 2]
                .checked_add(r.vcd_int()?)
                .ok_or_else(|| invalid("VCDIFF address overflows"))?,
            _ if m < 2 + S_NEAR + S_SAME => self.same[(m - 2 - S_NEAR) * 256 + r.u8()? as usize],
            _ => return Err(invalid("VCDIFF address mode out of range")),
        };
        if addr >= here {
            return Err(invalid(
                "VCDIFF COPY address points past the current position",
            ));
        }
        self.update(addr);
        Ok(addr)
    }

    /// Picks the cheapest mode for `addr`; returns it with the encoded bytes.
    fn encode(&mut self, addr: u64, here: u64, out: &mut Vec<u8>) -> u8 {
        let slot = (addr % (S_SAME as u64 * 256)) as usize;
        let mode = if self.same[slot] == addr {
            out.push((slot % 256) as u8);
            (2 + S_NEAR + slot / 256) as u8
        } else {
            let mut best = (0u8, addr);
            let mut consider = |mode: u8, v: u64| {
                if vcd_int_len(v) < vcd_int_len(best.1) {
                    best = (mode, v);
                }
            };
            consider(1, here - addr);
            for (i, &n) in self.near.iter().enumerate() {
                if addr >= n {
                    consider(2 + i as u8, addr - n);
                }
            }
            write_vcd_int(out, best.1);
            best.0
        };
        self.update(addr);
        mode
    }
}

fn usize_of(v: u64) -> Result<usize> {
    usize::try_from(v).map_err(|_| invalid("VCDIFF length too large"))
}

/// Parses the file header; returns the reader positioned at the first
/// window.
fn file_header(patch: &[u8]) -> Result<Reader<'_>> {
    let mut r = Reader::new(patch);
    if r.bytes(3)? != MAGIC {
        return Err(invalid("not a VCDIFF patch"));
    }
    if r.u8()? != 0 {
        return Err(invalid("unsupported VCDIFF version"));
    }
    let ind = r.u8()?;
    if ind & !(VCD_DECOMPRESS | VCD_CODETABLE | VCD_APPHEADER) != 0 {
        return Err(invalid("invalid VCDIFF header indicator"));
    }
    if ind & VCD_DECOMPRESS != 0 {
        let id = r.u8()?;
        return Err(invalid(format!(
            "VCDIFF secondary compression (id {id}) is not supported; recreate the patch with xdelta3 -S none"
        )));
    }
    if ind & VCD_CODETABLE != 0 {
        return Err(invalid("custom VCDIFF code tables are not supported"));
    }
    if ind & VCD_APPHEADER != 0 {
        let n = usize_of(r.vcd_int()?)?;
        r.bytes(n)?;
    }
    Ok(r)
}

struct Window<'p> {
    ind: u8,
    seg_len: u64,
    seg_pos: u64,
    tgt_len: u64,
    adler: Option<u32>,
    data: usize,
    data_len: usize,
    inst: &'p [u8],
    addr: &'p [u8],
}

fn window<'p>(r: &mut Reader<'p>) -> Result<Window<'p>> {
    let ind = r.u8()?;
    if ind & !(VCD_SOURCE | VCD_TARGET | VCD_ADLER32) != 0 || ind & 3 == 3 {
        return Err(invalid("invalid VCDIFF window indicator"));
    }
    let (seg_len, seg_pos) = if ind & 3 != 0 {
        (r.vcd_int()?, r.vcd_int()?)
    } else {
        (0, 0)
    };
    let enc_len = usize_of(r.vcd_int()?)?;
    let start = r.pos;
    let tgt_len = r.vcd_int()?;
    if r.u8()? != 0 {
        return Err(invalid("VCDIFF secondary compression is not supported"));
    }
    let data_len = usize_of(r.vcd_int()?)?;
    let inst_len = usize_of(r.vcd_int()?)?;
    let addr_len = usize_of(r.vcd_int()?)?;
    let adler = if ind & VCD_ADLER32 != 0 {
        Some(r.be(4)? as u32)
    } else {
        None
    };
    let data = r.pos;
    r.bytes(data_len)?;
    let inst = r.bytes(inst_len)?;
    let addr = r.bytes(addr_len)?;
    if r.pos - start != enc_len {
        return Err(invalid("VCDIFF window length mismatch"));
    }
    Ok(Window {
        ind,
        seg_len,
        seg_pos,
        tgt_len,
        adler,
        data,
        data_len,
        inst,
        addr,
    })
}

pub(super) fn inspect(patch: &[u8]) -> Result<PatchInfo> {
    let mut r = file_header(patch)?;
    let mut total = 0u64;
    while r.remaining() > 0 {
        total = total.saturating_add(window(&mut r)?.tgt_len);
    }
    Ok(PatchInfo {
        target_size: Some(total),
        ..PatchInfo::default()
    })
}

pub(super) fn apply(
    patch: &Arc<[u8]>,
    source: &Tree,
    verify: bool,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<Tree> {
    let table = table();
    let mut r = file_header(patch)?;
    let mut b = Builder::new(Arc::clone(patch));
    let mut prog = Tracker::new(progress, patch.len() as u64);
    while r.remaining() > 0 {
        cancel.check()?;
        prog.set(r.pos as u64);
        let w = window(&mut r)?;
        let win_start = b.len();
        let seg_end = w.seg_pos.checked_add(w.seg_len);
        let seg_ok = match w.ind & 3 {
            VCD_SOURCE => seg_end.is_some_and(|e| e <= source.len()),
            VCD_TARGET => seg_end.is_some_and(|e| e <= win_start),
            _ => true,
        };
        if !seg_ok {
            return Err(invalid("VCDIFF source segment is out of range"));
        }
        let mut data = Reader::new(&patch[w.data..w.data + w.data_len]);
        let mut inst = Reader::new(w.inst);
        let mut addr = Reader::new(w.addr);
        let mut cache = Cache::new();
        let mut count = 0u64;
        while inst.remaining() > 0 {
            count += 1;
            if count.is_multiple_of(4096) {
                cancel.check()?;
            }
            let (i1, i2) = table[inst.u8()? as usize];
            for i in [i1, i2] {
                if i.kind == NOOP {
                    continue;
                }
                let size = if i.size == 0 {
                    inst.vcd_int()?
                } else {
                    u64::from(i.size)
                };
                let produced = b.len() - win_start;
                if produced.checked_add(size).is_none_or(|e| e > w.tgt_len) {
                    return Err(invalid(
                        "VCDIFF window produces more than its target length",
                    ));
                }
                match i.kind {
                    ADD => {
                        let pos = w.data + data.pos;
                        data.bytes(usize_of(size)?)?;
                        b.push_patch(pos, size as usize);
                    }
                    RUN => {
                        let byte = data.u8()?;
                        b.push_fill(&[byte], size);
                    }
                    _ => {
                        let mut a = cache.decode(&mut addr, w.seg_len + produced, i.mode)?;
                        let mut rem = size;
                        if a < w.seg_len {
                            let n = rem.min(w.seg_len - a);
                            if w.ind & VCD_SOURCE != 0 {
                                b.push_source(source, w.seg_pos + a, n)?;
                            } else {
                                b.copy_self(w.seg_pos + a, n)?;
                            }
                            a += n;
                            rem -= n;
                        }
                        if rem > 0 {
                            b.copy_self(win_start + (a - w.seg_len), rem)?;
                        }
                    }
                }
            }
        }
        if data.remaining() != 0 || addr.remaining() != 0 {
            return Err(invalid("VCDIFF window has unused data or addresses"));
        }
        if b.len() - win_start != w.tgt_len {
            return Err(invalid("VCDIFF window is shorter than its target length"));
        }
        if verify && let Some(sum) = w.adler {
            let t = b.snapshot();
            if adler32(&t, win_start, win_start + w.tgt_len, cancel)? != sum {
                return Err(invalid("VCDIFF window checksum mismatch"));
            }
        }
    }
    prog.finish();
    Ok(b.finish())
}

enum WinOp {
    Copy { src: u64, len: u64 },
    Add { at: u64, len: u64 },
    Run { byte: u8, len: u64 },
}

struct Encoder<'a> {
    target: &'a Tree,
    cancel: &'a Cancel,
    checksum: bool,
    out: Vec<u8>,
    ops: Vec<WinOp>,
    win_start: u64,
    win_len: u64,
}

impl Encoder<'_> {
    fn push(&mut self, op: Op, mut pos: u64) -> Result<()> {
        let (mut off, total) = (0u64, op.len());
        while off < total {
            if self.win_len == WINDOW {
                self.flush()?;
            }
            let n = (total - off).min(WINDOW - self.win_len);
            self.ops.push(match op {
                Op::Copy { src, .. } => WinOp::Copy {
                    src: src + off,
                    len: n,
                },
                Op::Run { byte, .. } => WinOp::Run { byte, len: n },
                Op::Literal { .. } | Op::Delta { .. } => WinOp::Add { at: pos, len: n },
            });
            self.win_len += n;
            off += n;
            pos += n;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        if self.win_len == 0 {
            return Ok(());
        }
        let (mut lo, mut hi) = (u64::MAX, 0u64);
        for op in &self.ops {
            if let WinOp::Copy { src, len } = *op {
                lo = lo.min(src);
                hi = hi.max(src + len);
            }
        }
        let (seg_pos, seg_len) = if hi > 0 { (lo, hi - lo) } else { (0, 0) };
        let (mut data, mut inst, mut addr) = (Vec::new(), Vec::new(), Vec::new());
        let mut cache = Cache::new();
        let mut tpos = 0u64;
        for op in &self.ops {
            match *op {
                WinOp::Add { at, len } => {
                    for_each_chunk(self.target, at, at + len, self.cancel, |_, c| {
                        data.extend_from_slice(c);
                        Ok(())
                    })?;
                    if (1..=17).contains(&len) {
                        inst.push(1 + len as u8);
                    } else {
                        inst.push(1);
                        write_vcd_int(&mut inst, len);
                    }
                    tpos += len;
                }
                WinOp::Run { byte, len } => {
                    data.push(byte);
                    inst.push(0);
                    write_vcd_int(&mut inst, len);
                    tpos += len;
                }
                WinOp::Copy { src, len } => {
                    let mode = cache.encode(src - seg_pos, seg_len + tpos, &mut addr);
                    let base = 19 + 16 * mode;
                    if (4..=18).contains(&len) {
                        inst.push(base + (len as u8 - 3));
                    } else {
                        inst.push(base);
                        write_vcd_int(&mut inst, len);
                    }
                    tpos += len;
                }
            }
        }
        let mut body = Vec::with_capacity(data.len() + inst.len() + addr.len() + 32);
        write_vcd_int(&mut body, self.win_len);
        body.push(0);
        write_vcd_int(&mut body, data.len() as u64);
        write_vcd_int(&mut body, inst.len() as u64);
        write_vcd_int(&mut body, addr.len() as u64);
        if self.checksum {
            let sum = adler32(
                self.target,
                self.win_start,
                self.win_start + self.win_len,
                self.cancel,
            )?;
            body.extend_from_slice(&sum.to_be_bytes());
        }
        body.extend_from_slice(&data);
        body.extend_from_slice(&inst);
        body.extend_from_slice(&addr);
        let mut ind = if self.checksum { VCD_ADLER32 } else { 0 };
        if seg_len > 0 {
            ind |= VCD_SOURCE;
        }
        self.out.push(ind);
        if seg_len > 0 {
            write_vcd_int(&mut self.out, seg_len);
            write_vcd_int(&mut self.out, seg_pos);
        }
        write_vcd_int(&mut self.out, body.len() as u64);
        self.out.extend_from_slice(&body);
        self.win_start += self.win_len;
        self.win_len = 0;
        self.ops.clear();
        Ok(())
    }
}

pub(super) fn create(
    source: &Tree,
    target: &Tree,
    checksum: bool,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<Vec<u8>> {
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(&[0, 0]);
    let mut enc = Encoder {
        target,
        cancel,
        checksum,
        out,
        ops: Vec::new(),
        win_start: 0,
        win_len: 0,
    };
    let mut pos = 0u64;
    generate(source, target, false, cancel, progress, &mut |op| {
        enc.push(op, pos)?;
        pos += op.len();
        Ok(())
    })?;
    enc.flush()?;
    Ok(enc.out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::{ApplyOptions, apply_patch, inspect_patch};
    use crate::testutil::*;

    fn apply_vec(p: &[u8], src: &[u8]) -> Result<Vec<u8>> {
        let a = apply_patch(
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
    fn default_table_spot_checks() {
        let t = table();
        assert_eq!(t[0].0.kind, RUN);
        assert_eq!((t[18].0.kind, t[18].0.size), (ADD, 17));
        assert_eq!((t[19].0.kind, t[19].0.size, t[19].0.mode), (COPY, 0, 0));
        assert_eq!((t[162].0.size, t[162].0.mode), (18, 8));
        assert_eq!((t[163].0.size, t[163].1.size, t[163].1.mode), (1, 4, 0));
        assert_eq!((t[234].0.size, t[234].1.size, t[234].1.mode), (4, 6, 5));
        assert_eq!((t[235].0.size, t[235].1.mode), (1, 6));
        assert_eq!(
            (t[247].0.kind, t[247].1.kind, t[255].0.mode),
            (COPY, ADD, 8)
        );
    }

    /// Builds a window by hand: header, sections, optional checksum.
    fn window_bytes(
        ind: u8,
        seg: Option<(u64, u64)>,
        tgt: &[u8],
        data: &[u8],
        inst: &[u8],
        addr: &[u8],
    ) -> Vec<u8> {
        let mut body = Vec::new();
        write_vcd_int(&mut body, tgt.len() as u64);
        body.push(0);
        write_vcd_int(&mut body, data.len() as u64);
        write_vcd_int(&mut body, inst.len() as u64);
        write_vcd_int(&mut body, addr.len() as u64);
        if ind & VCD_ADLER32 != 0 {
            body.extend_from_slice(&crate::chunk::adler32_update(1, tgt).to_be_bytes());
        }
        body.extend_from_slice(data);
        body.extend_from_slice(inst);
        body.extend_from_slice(addr);
        let mut out = vec![ind];
        if let Some((len, pos)) = seg {
            write_vcd_int(&mut out, len);
            write_vcd_int(&mut out, pos);
        }
        write_vcd_int(&mut out, body.len() as u64);
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn hand_made_fixture() {
        let src = b"abcdefghijklmnop";
        // Window 1 (source segment "efghijklmnop" at 4): COPY 4 from 0
        // (SELF), ADD "XY", COPY 4 from 8, RUN 5×'z', COPY 6 overlapping
        // the target (HERE).
        let tgt1 = b"efghXYmnopzzzzzzzzzzz";
        let data = *b"XYz";
        let inst = [
            19 + 1, // COPY size 4 mode 0
            1 + 2,  // ADD size 2
            19 + 1, // COPY size 4 mode 0
            0,
            5, // RUN size 5
            19 + 16,
            6, // COPY size 0 (6 follows) mode 1 (HERE)
        ];
        let addr = [0, 8, 1]; // here = 12 + 15 = 27 → 27 - 1 = 26 = target 14 ('z')
        let mut p = MAGIC.to_vec();
        p.extend_from_slice(&[0, 0]);
        p.extend(window_bytes(
            VCD_SOURCE | VCD_ADLER32,
            Some((12, 4)),
            tgt1,
            &data,
            &inst,
            &addr,
        ));
        // Window 2 (target segment = first 4 output bytes): COPY 4 from 0
        // via the NEAR cache is not available yet → SELF.
        let tgt2 = b"efgh";
        p.extend(window_bytes(
            VCD_TARGET,
            Some((4, 0)),
            tgt2,
            &[],
            &[19 + 1],
            &[0],
        ));
        let mut expected = tgt1.to_vec();
        expected.extend_from_slice(tgt2);
        assert_eq!(apply_vec(&p, src).unwrap(), expected);
        assert_eq!(inspect_patch(&p).unwrap().target_size, Some(25));

        let mut bad = p.clone();
        let pos = bad.iter().position(|&b| b == b'X').unwrap();
        bad[pos] = b'Q';
        assert!(apply_vec(&bad, src).is_err(), "checksum must catch this");
    }

    #[test]
    fn rejects_unsupported_features() {
        assert!(apply_vec(&[0xD6, 0xC3, 0xC4, 0, 1, 2], b"").is_err());
        assert!(apply_vec(&[0xD6, 0xC3, 0xC4, 0, 2], b"").is_err());
        assert!(apply_vec(&[0xD6, 0xC3, 0xC4, 1, 0], b"").is_err());
        let mut p = MAGIC.to_vec();
        p.extend_from_slice(&[0, 0]);
        p.extend(window_bytes(
            VCD_SOURCE,
            Some((100, 0)),
            b"x",
            b"x",
            &[2],
            &[],
        ));
        assert!(apply_vec(&p, b"short").is_err());
        // Address cache "same" mode and app header are accepted.
        let mut p = MAGIC.to_vec();
        p.extend_from_slice(&[0, VCD_APPHEADER, 2, b'h', b'i']);
        // COPY 4 from 0 (SELF), then COPY 4 via same cache slot 0 (mode 6).
        p.extend(window_bytes(
            VCD_SOURCE,
            Some((4, 0)),
            b"abcdabcd",
            &[],
            &[20, 19 + 16 * 6 + 1],
            &[0, 0],
        ));
        assert_eq!(apply_vec(&p, b"abcd").unwrap(), b"abcdabcd");
    }
}
