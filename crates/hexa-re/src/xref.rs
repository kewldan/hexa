//! Cross references (RE-04), branch targets for navigation (ANL-13) and
//! pointer search / backward references (SRCH-12).
//!
//! Code is scanned with a linear sweep:
//!
//! * x86 / x86-64 (`iced-x86` decoder): direct calls and jumps, RIP-relative
//!   and absolute memory operands, `lea` address loads, and immediates
//!   equal to the target (`push offset`, `mov reg, offset`);
//! * ARM64: `b`, `bl`, `b.cond`, `cbz`/`cbnz`, `tbz`/`tbnz`, `adr`,
//!   `ldr` literal and `adrp` paired with a following `add` or unsigned
//!   offset load/store on the same register (within 16 instructions);
//! * ARM (A32): `b`/`bl`/`blx`, PC-relative `ldr` literals and `adr`;
//!   Thumb: `b`, `b<cond>`, `b.w`, `bl`, `blx`, `cbz`/`cbnz`, `ldr`
//!   literals (16/32-bit) and `adr`;
//! * every other Capstone architecture: decoded branch targets and
//!   memory references.
//!
//! Pointer search looks for the target stored as an 8/16/32/64-bit little
//! or big endian integer.

use std::collections::BTreeMap;

use hexa_core::Tree;
use iced_x86::{Decoder, DecoderOptions, FlowControl, Instruction as IcedInsn, Mnemonic, OpKind};
use serde::{Deserialize, Serialize};

use crate::arch::{Arch, Endian};
use crate::disasm::{DisasmOptions, decode_one, disassemble};
use crate::error::Result;
use crate::exe::ExecutableInfo;
use crate::util::sext;

/// Kind of a code reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum XrefKind {
    /// Direct call.
    Call,
    /// Direct (conditional or unconditional) jump.
    Jump,
    /// Memory operand reading or writing the address.
    Data,
    /// Address computed into a register (`lea`, `adr`, `adrp`+`add`).
    AddressLoad,
    /// Immediate operand equal to the address.
    Immediate,
}

/// A reference from code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeXref {
    /// Address of the referencing instruction (first of a pair).
    pub from: u64,
    /// File offset of `from`, when known.
    pub from_offset: Option<u64>,
    /// Reference kind.
    pub kind: XrefKind,
    /// Referenced address.
    pub target: u64,
    /// Disassembly of the referencing instruction(s).
    pub text: String,
}

#[derive(Debug, Clone, Copy)]
struct Hit {
    from: u64,
    kind: XrefKind,
    target: u64,
    /// Address of the second instruction of an ARM64 `adrp` pair.
    second: Option<u64>,
}

fn scan_x86(bitness: u32, code: &[u8], address: u64, f: &mut dyn FnMut(Hit)) {
    let mut dec = Decoder::with_ip(bitness, code, address, DecoderOptions::NONE);
    let mut ins = IcedInsn::default();
    let mask = if bitness == 64 { u64::MAX } else { 0xffff_ffff };
    while dec.can_decode() {
        dec.decode_out(&mut ins);
        if ins.is_invalid() {
            continue;
        }
        let from = ins.ip();
        let mut branch = false;
        for op in 0..ins.op_count() {
            match ins.op_kind(op) {
                OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64 => {
                    branch = true;
                    let kind = if ins.flow_control() == FlowControl::Call {
                        XrefKind::Call
                    } else {
                        XrefKind::Jump
                    };
                    f(Hit {
                        from,
                        kind,
                        target: ins.near_branch_target(),
                        second: None,
                    });
                }
                OpKind::Memory => {
                    let kind = if ins.mnemonic() == Mnemonic::Lea {
                        XrefKind::AddressLoad
                    } else {
                        XrefKind::Data
                    };
                    if ins.is_ip_rel_memory_operand() {
                        f(Hit {
                            from,
                            kind,
                            target: ins.ip_rel_memory_address(),
                            second: None,
                        });
                    } else if ins.memory_base() == iced_x86::Register::None
                        && ins.memory_index() == iced_x86::Register::None
                    {
                        f(Hit {
                            from,
                            kind,
                            target: ins.memory_displacement64() & mask,
                            second: None,
                        });
                    }
                }
                OpKind::Immediate16
                | OpKind::Immediate32
                | OpKind::Immediate32to64
                | OpKind::Immediate64
                    if !branch =>
                {
                    f(Hit {
                        from,
                        kind: XrefKind::Immediate,
                        target: ins.immediate(op) & mask,
                        second: None,
                    });
                }
                _ => {}
            }
        }
    }
}

fn scan_arm64(code: &[u8], address: u64, f: &mut dyn FnMut(Hit)) {
    // Per register: (page, adrp address, instruction index).
    let mut pages: [Option<(u64, u64, usize)>; 32] = [None; 32];
    for (i, chunk) in code.as_chunks::<4>().0.iter().enumerate() {
        let w = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        let pc = address.wrapping_add(i as u64 * 4);
        let rel = |imm: u64, bits: u32| pc.wrapping_add((sext(imm, bits) << 2) as u64);
        let rd = (w & 31) as usize;
        let rn = ((w >> 5) & 31) as usize;
        if w & 0x7c00_0000 == 0x1400_0000 {
            let kind = if w >> 31 == 1 {
                XrefKind::Call
            } else {
                XrefKind::Jump
            };
            f(Hit {
                from: pc,
                kind,
                target: rel(u64::from(w & 0x03ff_ffff), 26),
                second: None,
            });
        } else if w & 0xff00_0010 == 0x5400_0000 || w & 0x7e00_0000 == 0x3400_0000 {
            let t = rel(u64::from((w >> 5) & 0x7ffff), 19);
            f(Hit {
                from: pc,
                kind: XrefKind::Jump,
                target: t,
                second: None,
            });
        } else if w & 0x7e00_0000 == 0x3600_0000 {
            let t = rel(u64::from((w >> 5) & 0x3fff), 14);
            f(Hit {
                from: pc,
                kind: XrefKind::Jump,
                target: t,
                second: None,
            });
        } else if w & 0x9f00_0000 == 0x1000_0000 || w & 0x9f00_0000 == 0x9000_0000 {
            let imm = u64::from((w >> 5) & 0x7ffff) << 2 | u64::from((w >> 29) & 3);
            let off = sext(imm, 21);
            if w >> 31 == 0 {
                let t = pc.wrapping_add(off as u64);
                f(Hit {
                    from: pc,
                    kind: XrefKind::AddressLoad,
                    target: t,
                    second: None,
                });
            } else {
                let page = (pc & !0xfff).wrapping_add((off << 12) as u64);
                pages[rd] = Some((page, pc, i));
            }
        } else if w & 0x7f80_0000 == 0x1100_0000 {
            // add (immediate)
            if let Some((page, adrp, at)) = pages[rn]
                && i - at <= 16
            {
                let sh = if w & (1 << 22) != 0 { 12 } else { 0 };
                let t = page.wrapping_add(u64::from((w >> 10) & 0xfff) << sh);
                f(Hit {
                    from: adrp,
                    kind: XrefKind::AddressLoad,
                    target: t,
                    second: Some(pc),
                });
            }
            if rd != rn {
                pages[rd] = None;
            }
        } else if w & 0x3b00_0000 == 0x3900_0000 {
            // load/store (unsigned immediate)
            if let Some((page, adrp, at)) = pages[rn]
                && i - at <= 16
            {
                let size = w >> 30;
                let vector_q = w & (1 << 26) != 0 && (w >> 23) & 1 == 1;
                let scale = if vector_q { 4 } else { size };
                let t = page.wrapping_add(u64::from((w >> 10) & 0xfff) << scale);
                f(Hit {
                    from: adrp,
                    kind: XrefKind::Data,
                    target: t,
                    second: Some(pc),
                });
            }
        } else if w & 0x3b00_0000 == 0x1800_0000 {
            let t = rel(u64::from((w >> 5) & 0x7ffff), 19);
            f(Hit {
                from: pc,
                kind: XrefKind::Data,
                target: t,
                second: None,
            });
        }
    }
}

fn scan_arm(code: &[u8], address: u64, f: &mut dyn FnMut(Hit)) {
    for (i, chunk) in code.as_chunks::<4>().0.iter().enumerate() {
        let w = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        let pc = address.wrapping_add(i as u64 * 4);
        let cond = w >> 28;
        if w & 0x0e00_0000 == 0x0a00_0000 {
            let off = sext(u64::from(w & 0x00ff_ffff), 24) << 2;
            let (kind, extra) = if cond == 0xf {
                (XrefKind::Call, u64::from((w >> 24) & 1) << 1)
            } else if w & (1 << 24) != 0 {
                (XrefKind::Call, 0)
            } else {
                (XrefKind::Jump, 0)
            };
            let t = pc
                .wrapping_add(8)
                .wrapping_add(off as u64)
                .wrapping_add(extra)
                & 0xffff_ffff;
            f(Hit {
                from: pc,
                kind,
                target: t,
                second: None,
            });
        } else if cond != 0xf && w & 0x0f7f_0000 == 0x051f_0000 {
            let imm = u64::from(w & 0xfff);
            let base = pc.wrapping_add(8) & !3;
            let t = if w & (1 << 23) != 0 {
                base.wrapping_add(imm)
            } else {
                base.wrapping_sub(imm)
            };
            f(Hit {
                from: pc,
                kind: XrefKind::Data,
                target: t & 0xffff_ffff,
                second: None,
            });
        } else if cond != 0xf && (w & 0x0fff_0000 == 0x028f_0000 || w & 0x0fff_0000 == 0x024f_0000)
        {
            let rot = ((w >> 8) & 0xf) * 2;
            let imm = u64::from((w & 0xff).rotate_right(rot));
            let base = pc.wrapping_add(8);
            let t = if w & 0x0fff_0000 == 0x028f_0000 {
                base.wrapping_add(imm)
            } else {
                base.wrapping_sub(imm)
            };
            f(Hit {
                from: pc,
                kind: XrefKind::AddressLoad,
                target: t & 0xffff_ffff,
                second: None,
            });
        }
    }
}

fn scan_thumb(code: &[u8], address: u64, f: &mut dyn FnMut(Hit)) {
    let mut i = 0usize;
    let half = |i: usize| -> Option<u32> {
        code.get(i..i + 2)
            .map(|b| u32::from(u16::from_le_bytes([b[0], b[1]])))
    };
    while let Some(h1) = half(i) {
        let pc = address.wrapping_add(i as u64);
        let mask = |v: u64| v & 0xffff_ffff;
        if matches!(h1 >> 11, 0b11101..=0b11111) {
            let Some(h2) = half(i + 2) else {
                break;
            };
            if h1 & 0xf800 == 0xf000 && h2 & 0x8000 == 0x8000 {
                let s = (h1 >> 10) & 1;
                let j1 = (h2 >> 13) & 1;
                let j2 = (h2 >> 11) & 1;
                let imm11 = u64::from(h2 & 0x7ff);
                let op = h2 & 0xd000;
                if op == 0x8000 {
                    // B<cond>.W (T3)
                    let c = (h1 >> 6) & 0xf;
                    if c < 14 {
                        let imm = u64::from(s) << 20
                            | u64::from(j2) << 19
                            | u64::from(j1) << 18
                            | u64::from(h1 & 0x3f) << 12
                            | imm11 << 1;
                        let t = mask(pc.wrapping_add(4).wrapping_add(sext(imm, 21) as u64));
                        f(Hit {
                            from: pc,
                            kind: XrefKind::Jump,
                            target: t,
                            second: None,
                        });
                    }
                } else {
                    let i1 = !(j1 ^ s) & 1;
                    let i2 = !(j2 ^ s) & 1;
                    let imm = u64::from(s) << 24
                        | u64::from(i1) << 23
                        | u64::from(i2) << 22
                        | u64::from(h1 & 0x3ff) << 12
                        | imm11 << 1;
                    let off = sext(imm, 25) as u64;
                    let (kind, base) = match op {
                        0xd000 => (XrefKind::Call, pc.wrapping_add(4)),
                        0xc000 => (XrefKind::Call, pc.wrapping_add(4) & !3),
                        _ => (XrefKind::Jump, pc.wrapping_add(4)),
                    };
                    f(Hit {
                        from: pc,
                        kind,
                        target: mask(base.wrapping_add(off)),
                        second: None,
                    });
                }
            } else if h1 & 0xff7f == 0xf85f {
                let imm = u64::from(h2 & 0xfff);
                let base = pc.wrapping_add(4) & !3;
                let t = if h1 & 0x80 != 0 {
                    base.wrapping_add(imm)
                } else {
                    base.wrapping_sub(imm)
                };
                f(Hit {
                    from: pc,
                    kind: XrefKind::Data,
                    target: mask(t),
                    second: None,
                });
            }
            i += 4;
            continue;
        }
        if h1 & 0xf800 == 0xe000 {
            let t = pc
                .wrapping_add(4)
                .wrapping_add(sext(u64::from(h1 & 0x7ff) << 1, 12) as u64);
            f(Hit {
                from: pc,
                kind: XrefKind::Jump,
                target: mask(t),
                second: None,
            });
        } else if h1 & 0xf000 == 0xd000 && (h1 >> 8) & 0xf < 14 {
            let t = pc
                .wrapping_add(4)
                .wrapping_add(sext(u64::from(h1 & 0xff) << 1, 9) as u64);
            f(Hit {
                from: pc,
                kind: XrefKind::Jump,
                target: mask(t),
                second: None,
            });
        } else if h1 & 0xf500 == 0xb100 {
            let imm = u64::from((h1 >> 9) & 1) << 6 | u64::from((h1 >> 3) & 0x1f) << 1;
            f(Hit {
                from: pc,
                kind: XrefKind::Jump,
                target: mask(pc + 4 + imm),
                second: None,
            });
        } else if h1 & 0xf800 == 0x4800 || h1 & 0xf800 == 0xa000 {
            let t = (pc.wrapping_add(4) & !3).wrapping_add(u64::from(h1 & 0xff) * 4);
            let kind = if h1 & 0xf800 == 0x4800 {
                XrefKind::Data
            } else {
                XrefKind::AddressLoad
            };
            f(Hit {
                from: pc,
                kind,
                target: mask(t),
                second: None,
            });
        }
        i += 2;
    }
}

fn scan_generic(arch: Arch, code: &[u8], address: u64, f: &mut dyn FnMut(Hit)) -> Result<()> {
    for ins in disassemble(arch, code, address, &DisasmOptions::default())? {
        if let Some(t) = ins.branch_target {
            let kind = if ins.is_call() {
                XrefKind::Call
            } else {
                XrefKind::Jump
            };
            f(Hit {
                from: ins.address,
                kind,
                target: t,
                second: None,
            });
        }
        if let Some(t) = ins.memory_reference {
            f(Hit {
                from: ins.address,
                kind: XrefKind::Data,
                target: t,
                second: None,
            });
        }
    }
    Ok(())
}

fn scan(arch: Arch, code: &[u8], address: u64, f: &mut dyn FnMut(Hit)) -> Result<()> {
    match arch {
        Arch::X86Real | Arch::X86 | Arch::X64 => {
            scan_x86(arch.x86_bitness().unwrap_or(64), code, address, f);
        }
        Arch::Arm64 => scan_arm64(code, address, f),
        Arch::Arm => scan_arm(code, address, f),
        Arch::Thumb => scan_thumb(code, address, f),
        _ => scan_generic(arch, code, address, f)?,
    }
    Ok(())
}

fn hit_text(arch: Arch, code: &[u8], address: u64, h: &Hit) -> String {
    let at = |a: u64| -> String {
        let Some(off) = a.checked_sub(address).and_then(|o| usize::try_from(o).ok()) else {
            return String::new();
        };
        let end = (off + 16).min(code.len());
        code.get(off..end)
            .and_then(|c| decode_one(arch, c, a).ok().flatten())
            .map(|i| i.text())
            .unwrap_or_default()
    };
    match h.second {
        Some(s) => format!("{}; {}", at(h.from), at(s)),
        None => at(h.from),
    }
}

/// Finds code in `code` (loaded at `address`) that references any address
/// in `[target_start, target_end)` (RE-04). `file_offset` is the file
/// offset of `code[0]`, used to fill [`CodeXref::from_offset`].
pub fn find_code_references(
    arch: Arch,
    code: &[u8],
    address: u64,
    file_offset: Option<u64>,
    target_start: u64,
    target_end: u64,
    max_results: usize,
) -> Result<Vec<CodeXref>> {
    let mut hits = Vec::new();
    let end = target_end.max(target_start.saturating_add(1));
    scan(arch, code, address, &mut |h| {
        if h.target >= target_start && h.target < end && hits.len() < max_results {
            hits.push(h);
        }
    })?;
    Ok(hits
        .iter()
        .map(|h| CodeXref {
            from: h.from,
            from_offset: file_offset.map(|o| o + (h.from - address)),
            kind: h.kind,
            target: h.target,
            text: hit_text(arch, code, address, h),
        })
        .collect())
}

/// All direct call / jump targets in `code`, for navigation (ANL-13).
/// Returns `(from, target, kind)` sorted by `from`.
pub fn branch_targets(arch: Arch, code: &[u8], address: u64) -> Result<Vec<(u64, u64, XrefKind)>> {
    let mut v = Vec::new();
    scan(arch, code, address, &mut |h| {
        if matches!(h.kind, XrefKind::Call | XrefKind::Jump) {
            v.push((h.from, h.target, h.kind));
        }
    })?;
    v.sort_unstable_by_key(|x| x.0);
    Ok(v)
}

/// Which byte orders to try in a pointer search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EndianChoice {
    /// Little endian only.
    Little,
    /// Big endian only.
    Big,
    /// Both.
    Both,
}

/// Pointer search options (SRCH-12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PointerOptions {
    /// Widths in bytes (1, 2, 4, 8).
    pub widths: Vec<u8>,
    /// Byte orders.
    pub endian: EndianChoice,
    /// Only report offsets that are multiples of this (1 = any).
    pub alignment: u64,
    /// Stop after this many results.
    pub max_results: usize,
}

impl Default for PointerOptions {
    fn default() -> Self {
        PointerOptions {
            widths: vec![4, 8],
            endian: EndianChoice::Little,
            alignment: 1,
            max_results: 10_000,
        }
    }
}

/// A place where a value is stored as an integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PointerRef {
    /// File offset.
    pub offset: u64,
    /// Width in bytes.
    pub width: u8,
    /// Byte order.
    pub endian: Endian,
}

fn patterns(value: u64, opts: &PointerOptions) -> Vec<(Vec<u8>, u8, Endian)> {
    let mut out: Vec<(Vec<u8>, u8, Endian)> = Vec::new();
    for &w in &opts.widths {
        if !matches!(w, 1 | 2 | 4 | 8) || (w < 8 && value >> (u32::from(w) * 8) != 0) {
            continue;
        }
        let ends: &[Endian] = match opts.endian {
            EndianChoice::Little => &[Endian::Little],
            EndianChoice::Big => &[Endian::Big],
            EndianChoice::Both => &[Endian::Little, Endian::Big],
        };
        for &e in ends {
            let p = crate::patch::encode_uint(value, usize::from(w), e);
            if !out.iter().any(|(q, qw, _)| *q == p && *qw == w) {
                out.push((p, w, e));
            }
        }
    }
    out
}

/// Finds every place where `value` is stored (SRCH-12, RE-04).
pub fn find_pointers(data: &[u8], value: u64, opts: &PointerOptions) -> Vec<PointerRef> {
    let mut found: BTreeMap<(u64, u8), PointerRef> = BTreeMap::new();
    let align = opts.alignment.max(1);
    for (pat, width, endian) in patterns(value, opts) {
        for pos in memchr::memmem::find_iter(data, &pat) {
            let offset = pos as u64;
            if offset.is_multiple_of(align) {
                found.entry((offset, width)).or_insert(PointerRef {
                    offset,
                    width,
                    endian,
                });
            }
        }
    }
    found.into_values().take(opts.max_results).collect()
}

/// [`find_pointers`] streamed over a tree in 4 MiB chunks.
pub fn find_pointers_tree(
    tree: &Tree,
    value: u64,
    opts: &PointerOptions,
) -> Result<Vec<PointerRef>> {
    const CHUNK: u64 = 4 << 20;
    let pats = patterns(value, opts);
    let overlap = pats.iter().map(|p| p.0.len()).max().unwrap_or(1) as u64 - 1;
    let align = opts.alignment.max(1);
    let mut found: BTreeMap<(u64, u8), PointerRef> = BTreeMap::new();
    let len = tree.len();
    let mut start = 0u64;
    while start < len {
        let n = (CHUNK + overlap).min(len - start);
        let buf = tree.read_vec(start, n as usize)?;
        for (pat, width, endian) in &pats {
            for pos in memchr::memmem::find_iter(&buf, pat) {
                let offset = start + pos as u64;
                if offset.is_multiple_of(align) {
                    found.entry((offset, *width)).or_insert(PointerRef {
                        offset,
                        width: *width,
                        endian: *endian,
                    });
                }
            }
        }
        if found.len() >= opts.max_results {
            break;
        }
        start += CHUNK;
    }
    Ok(found.into_values().take(opts.max_results).collect())
}

/// A stored pointer to the target inside an executable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PointerXref {
    /// Where the pointer is stored.
    pub location: PointerRef,
    /// Virtual address of the storage, when mapped.
    pub virtual_address: Option<u64>,
    /// Section holding it.
    pub section: Option<String>,
    /// `true` when the stored value is the RVA rather than the VA.
    pub is_rva: bool,
}

/// All references to an address in an executable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XrefReport {
    /// Target virtual address.
    pub target: u64,
    /// References from code.
    pub code: Vec<CodeXref>,
    /// Stored pointers.
    pub pointers: Vec<PointerXref>,
}

/// Finds code references and stored pointers to `target_va` in an
/// executable (RE-04). Code sections are scanned with the image's
/// architecture; pointers are searched with the image's pointer size and
/// byte order (plus 32-bit RVAs for PE).
pub fn find_xrefs(
    data: &[u8],
    info: &ExecutableInfo,
    target_va: u64,
    max_results: usize,
) -> Result<XrefReport> {
    let mut report = XrefReport {
        target: target_va,
        code: Vec::new(),
        pointers: Vec::new(),
    };
    if let Some(arch) = info.arch {
        for (off, size, va) in info.code_ranges() {
            let (Ok(s), Ok(e)) = (usize::try_from(off), usize::try_from(off + size)) else {
                continue;
            };
            let Some(code) = data.get(s..e.min(data.len())) else {
                continue;
            };
            let left = max_results.saturating_sub(report.code.len());
            report.code.extend(find_code_references(
                arch,
                code,
                va,
                Some(off),
                target_va,
                target_va + 1,
                left,
            )?);
        }
    }
    let width = if info.bits == 64 { 8 } else { 4 };
    let endian = match info.endian {
        Endian::Little => EndianChoice::Little,
        Endian::Big => EndianChoice::Big,
    };
    let opts = PointerOptions {
        widths: vec![width],
        endian,
        alignment: 1,
        max_results,
    };
    let mut push = |p: PointerRef, is_rva: bool| {
        report.pointers.push(PointerXref {
            location: p,
            virtual_address: info.offset_to_va(p.offset),
            section: info.section_at_offset(p.offset).map(|s| s.name.clone()),
            is_rva,
        });
    };
    for p in find_pointers(data, target_va, &opts) {
        push(p, false);
    }
    if info.format == crate::exe::ExecutableFormat::Pe {
        let rva = target_va.wrapping_sub(info.image_base());
        if rva != target_va && u32::try_from(rva).is_ok() {
            let o = PointerOptions {
                widths: vec![4],
                ..opts
            };
            for p in find_pointers(data, rva, &o) {
                push(p, true);
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::assemble;

    #[test]
    fn x64_references() {
        let src = "call 0x1100; jmp 0x1100; lea rax, [rel 0x1100]; mov rcx, [0x1100]; mov eax, 0x1100; je 0x1100; nop";
        let code = assemble(Arch::X64, src, 0x1000).unwrap();
        let refs = find_code_references(Arch::X64, &code, 0x1000, Some(0x400), 0x1100, 0x1101, 100)
            .unwrap();
        let kinds: Vec<XrefKind> = refs.iter().map(|r| r.kind).collect();
        assert_eq!(
            kinds,
            vec![
                XrefKind::Call,
                XrefKind::Jump,
                XrefKind::AddressLoad,
                XrefKind::Data,
                XrefKind::Immediate,
                XrefKind::Jump
            ],
            "{refs:?}"
        );
        assert_eq!(refs[0].from, 0x1000);
        assert_eq!(refs[0].from_offset, Some(0x400));
        assert!(refs[0].text.starts_with("call"));
        assert!(refs.iter().all(|r| r.target == 0x1100));
        let bt = branch_targets(Arch::X64, &code, 0x1000).unwrap();
        assert_eq!(bt.len(), 3);
    }

    #[test]
    fn x86_absolute() {
        let code = assemble(
            Arch::X86,
            "push 0x402000; call dword ptr [0x403000]",
            0x40_1000,
        )
        .unwrap();
        let r = find_code_references(Arch::X86, &code, 0x40_1000, None, 0x40_3000, 0x40_3001, 10)
            .unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].kind, XrefKind::Data);
        let r = find_code_references(Arch::X86, &code, 0x40_1000, None, 0x40_2000, 0x40_2001, 10)
            .unwrap();
        assert_eq!(r[0].kind, XrefKind::Immediate);
    }

    #[test]
    fn arm64_pairs() {
        let src = "adrp x0, 0x5000; add x0, x0, #0x123; bl 0x4100; adrp x1, 0x5000; ldr x2, [x1, #0x128]; b.eq 0x4000; adr x3, 0x4010; ldr x4, 0x4100";
        let code = assemble(Arch::Arm64, src, 0x4000).unwrap();
        let r = find_code_references(Arch::Arm64, &code, 0x4000, None, 0x5123, 0x5124, 10).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].kind, XrefKind::AddressLoad);
        assert_eq!(r[0].from, 0x4000);
        assert_eq!(r[0].text, "adrp x0, #0x5000; add x0, x0, #0x123");
        let r = find_code_references(Arch::Arm64, &code, 0x4000, None, 0x5128, 0x5129, 10).unwrap();
        assert_eq!(r[0].kind, XrefKind::Data);
        assert_eq!(r[0].from, 0x400c);
        let r = find_code_references(Arch::Arm64, &code, 0x4000, None, 0x4100, 0x4101, 10).unwrap();
        assert_eq!(
            r.iter().map(|x| x.kind).collect::<Vec<_>>(),
            vec![XrefKind::Call, XrefKind::Data]
        );
        let bt = branch_targets(Arch::Arm64, &code, 0x4000).unwrap();
        assert_eq!(
            bt,
            vec![
                (0x4008, 0x4100, XrefKind::Call),
                (0x4014, 0x4000, XrefKind::Jump)
            ]
        );
        let r = find_code_references(Arch::Arm64, &code, 0x4000, None, 0x4010, 0x4011, 10).unwrap();
        assert_eq!(r[0].kind, XrefKind::AddressLoad);
    }

    #[test]
    fn arm_and_thumb() {
        // A32: bl +8 at 0x8000 -> 0x8010 (eb000002); ldr r0, [pc, #4] -> 0x800c
        let code: Vec<u8> = [0xeb00_0002u32, 0xe59f_0004]
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .collect();
        let bt = branch_targets(Arch::Arm, &code, 0x8000).unwrap();
        assert_eq!(bt, vec![(0x8000, 0x8010, XrefKind::Call)]);
        let r = find_code_references(Arch::Arm, &code, 0x8000, None, 0x8010, 0x8011, 10).unwrap();
        assert_eq!(r.len(), 2, "{r:?}");
        // Thumb: bl (f000 f802) at 0x100 -> 0x108; b.n -2 (e7fe) at 0x104
        let code = [0x00, 0xf0, 0x02, 0xf8, 0xfe, 0xe7, 0x01, 0x48];
        let bt = branch_targets(Arch::Thumb, &code, 0x100).unwrap();
        assert_eq!(
            bt,
            vec![
                (0x100, 0x108, XrefKind::Call),
                (0x104, 0x104, XrefKind::Jump)
            ]
        );
        // capstone agrees on the bl target
        let ins = decode_one(Arch::Thumb, &code, 0x100).unwrap().unwrap();
        assert_eq!(ins.branch_target, Some(0x108));
        // ldr r0, [pc, #4] at 0x106 -> align(0x10a)=0x108 + 4 = 0x10c
        let r = find_code_references(Arch::Thumb, &code, 0x100, None, 0x10c, 0x10d, 10).unwrap();
        assert_eq!(r[0].kind, XrefKind::Data);
    }

    #[test]
    fn generic_arch() {
        // MIPS BE jal 0x400000
        let code = 0x0c10_0000u32.to_be_bytes();
        let r =
            find_code_references(Arch::Mips32Be, &code, 0, None, 0x40_0000, 0x40_0001, 10).unwrap();
        assert_eq!(r[0].kind, XrefKind::Call);
    }

    #[test]
    fn pointers() {
        let mut data = vec![0xffu8; 64];
        data[3..7].copy_from_slice(&0x0040_1000u32.to_le_bytes());
        data[16..24].copy_from_slice(&0x0040_1000u64.to_le_bytes());
        data[40..44].copy_from_slice(&0x0040_1000u32.to_be_bytes());
        let opts = PointerOptions {
            widths: vec![4, 8],
            endian: EndianChoice::Both,
            alignment: 1,
            max_results: 100,
        };
        let r = find_pointers(&data, 0x40_1000, &opts);
        let offs: Vec<(u64, u8)> = r.iter().map(|p| (p.offset, p.width)).collect();
        assert_eq!(offs, vec![(3, 4), (16, 4), (16, 8), (40, 4)]);
        assert_eq!(r[3].endian, Endian::Big);
        let aligned = find_pointers(
            &data,
            0x40_1000,
            &PointerOptions {
                alignment: 4,
                ..opts.clone()
            },
        );
        assert!(aligned.iter().all(|p| p.offset % 4 == 0));
        // 16-bit and 8-bit
        let r = find_pointers(
            &[0x34, 0x12, 0x12, 0x34],
            0x1234,
            &PointerOptions {
                widths: vec![1, 2],
                ..opts.clone()
            },
        );
        assert_eq!(r.len(), 2);
        // Value too wide for the width is skipped.
        let wide = PointerOptions {
            widths: vec![4],
            ..opts.clone()
        };
        assert_eq!(
            find_pointers(&data, 0x1_0000_0000, &wide),
            Vec::<PointerRef>::new()
        );
        // Tree variant across chunk boundaries.
        let mut big = vec![0u8; (4 << 20) + 16];
        let at = (4 << 20) - 2;
        big[at..at + 4].copy_from_slice(&0xdead_beefu32.to_le_bytes());
        let tree = Tree::from_piece(hexa_core::piece::Piece::bytes(big));
        let four = PointerOptions {
            widths: vec![4],
            ..PointerOptions::default()
        };
        let r = find_pointers_tree(&tree, 0xdead_beef, &four).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].offset, at as u64);
    }
}
