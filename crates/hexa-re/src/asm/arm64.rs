//! ARM64 (`AArch64`) assembler for a common instruction subset (ANL-14).
//!
//! Supported (64-bit `x` and 32-bit `w` forms where the architecture has
//! them):
//!
//! * control flow: `b`, `bl`, `b.<cond>`, `cbz`, `cbnz`, `tbz`, `tbnz`,
//!   `br`, `blr`, `ret`; branch operands are **absolute** target addresses,
//!   matching the disassembler output (`bl #0x1008`);
//! * `nop`, `svc`, `brk`, `hlt`;
//! * `adr`, `adrp` (absolute target);
//! * `mov` (register, `sp`, or 16-bit-chunk immediates via `movz`/`movn`),
//!   `movz`, `movn`, `movk` (with `lsl #16/32/48`);
//! * `add`, `adds`, `sub`, `subs`, `cmp`, `cmn` with a 12-bit immediate
//!   (optionally `lsl #12`) or a shifted register;
//! * `and`, `orr`, `eor`, `ands`, `bic`, `orn`, `mvn`, `tst` with a shifted
//!   register (logical bitmask immediates are not supported);
//! * `mul`, `madd`, `msub`, `udiv`, `sdiv`, `lsl`/`lsr`/`asr` (register or
//!   immediate), `csel`, `cset`;
//! * `ldr`, `str`, `ldrb`, `strb`, `ldrh`, `strh`, `ldrsw` with unsigned
//!   offset, pre-index (`[xn, #i]!`), post-index (`[xn], #i`) and
//!   unscaled (`ldur`/`stur`) addressing, `ldr` literal (absolute address);
//! * `ldp`, `stp` (offset, pre-index, post-index).
//!
//! SIMD/FP registers, register-offset addressing and system registers are
//! not supported.

use super::parse_int;
use crate::error::{ReError, Result};

fn err(msg: impl Into<String>) -> ReError {
    ReError::Asm(msg.into())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegKind {
    Gp,
    Zr,
    Sp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Reg {
    num: u32,
    is64: bool,
    kind: RegKind,
}

fn parse_reg(s: &str) -> Option<Reg> {
    let s = s.trim();
    let (num, is64, kind) = match s {
        "sp" => (31, true, RegKind::Sp),
        "wsp" => (31, false, RegKind::Sp),
        "xzr" => (31, true, RegKind::Zr),
        "wzr" => (31, false, RegKind::Zr),
        "lr" => (30, true, RegKind::Gp),
        "fp" => (29, true, RegKind::Gp),
        "ip0" => (16, true, RegKind::Gp),
        "ip1" => (17, true, RegKind::Gp),
        _ => {
            let is64 = match s.as_bytes().first()? {
                b'x' => true,
                b'w' => false,
                _ => return None,
            };
            let n: u32 = s[1..].parse().ok()?;
            if n > 30 {
                return None;
            }
            (n, is64, RegKind::Gp)
        }
    };
    Some(Reg { num, is64, kind })
}

fn cond(s: &str) -> Option<u32> {
    Some(match s {
        "eq" => 0,
        "ne" => 1,
        "cs" | "hs" => 2,
        "cc" | "lo" => 3,
        "mi" => 4,
        "pl" => 5,
        "vs" => 6,
        "vc" => 7,
        "hi" => 8,
        "ls" => 9,
        "ge" => 10,
        "lt" => 11,
        "gt" => 12,
        "le" => 13,
        "al" => 14,
        "nv" => 15,
        _ => return None,
    })
}

fn imm(s: &str) -> Result<i128> {
    let t = s.trim().trim_start_matches('#');
    parse_int(t).ok_or_else(|| err(format!("bad immediate `{s}`")))
}

#[derive(Debug, Clone, PartialEq)]
enum Op {
    Reg(Reg),
    Imm(i128),
    Shift(u32, u32),
    Mem { base: Reg, offset: i128, pre: bool },
    Cond(u32),
}

fn split_ops(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut cur = String::new();
    for c in s.chars() {
        match c {
            '[' => {
                depth += 1;
                cur.push(c);
            }
            ']' => {
                depth -= 1;
                cur.push(c);
            }
            ',' if depth == 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

fn parse_op(s: &str) -> Result<Op> {
    let s = s.trim();
    if let Some(r) = parse_reg(s) {
        return Ok(Op::Reg(r));
    }
    if let Some(c) = cond(s) {
        return Ok(Op::Cond(c));
    }
    for (name, code) in [("lsl", 0), ("lsr", 1), ("asr", 2), ("ror", 3)] {
        if let Some(rest) = s.strip_prefix(name)
            && rest.starts_with(char::is_whitespace)
        {
            let amount = imm(rest)?;
            return Ok(Op::Shift(
                code,
                u32::try_from(amount).map_err(|_| err("bad shift"))?,
            ));
        }
    }
    if s.starts_with('[') {
        let pre = s.ends_with('!');
        let body = s.trim_end_matches('!').trim();
        let inner = body
            .strip_prefix('[')
            .and_then(|b| b.strip_suffix(']'))
            .ok_or_else(|| err(format!("bad memory operand `{s}`")))?;
        let mut parts = inner.splitn(2, ',');
        let base = parse_reg(parts.next().unwrap_or(""))
            .filter(|r| r.is64 && r.kind != RegKind::Zr)
            .ok_or_else(|| err(format!("bad base register in `{s}`")))?;
        let offset = match parts.next() {
            Some(o) => imm(o)?,
            None => 0,
        };
        return Ok(Op::Mem { base, offset, pre });
    }
    Ok(Op::Imm(imm(s)?))
}

fn sf(r: Reg) -> u32 {
    u32::from(r.is64) << 31
}

fn gp_or_zr(op: &Op) -> Result<Reg> {
    match op {
        Op::Reg(r) if r.kind != RegKind::Sp => Ok(*r),
        _ => Err(err("expected a general-purpose register")),
    }
}

fn gp_or_sp(op: &Op) -> Result<Reg> {
    match op {
        Op::Reg(r) if r.kind != RegKind::Zr => Ok(*r),
        _ => Err(err("expected a general-purpose register or sp")),
    }
}

fn same_width(regs: &[Reg]) -> Result<()> {
    if regs.windows(2).all(|w| w[0].is64 == w[1].is64) {
        Ok(())
    } else {
        Err(err("mixed 32-bit and 64-bit registers"))
    }
}

fn rel(target: i128, pc: u64, bits: u32, scale: u32) -> Result<u32> {
    let delta = target - i128::from(pc);
    if delta % (1 << scale) != 0 {
        return Err(err("branch target is not aligned"));
    }
    let v = delta >> scale;
    let lim = 1i128 << (bits - 1);
    if v < -lim || v >= lim {
        return Err(err("branch target out of range"));
    }
    Ok((v as u32) & ((1u32 << bits) - 1))
}

fn want(ops: &[Op], n: usize, m: &str) -> Result<()> {
    if ops.len() == n {
        Ok(())
    } else {
        Err(err(format!("`{m}` expects {n} operands")))
    }
}

fn target(op: &Op) -> Result<i128> {
    match op {
        Op::Imm(v) => Ok(*v),
        _ => Err(err("expected an address")),
    }
}

fn shift_of(ops: &[Op], idx: usize, allow_ror: bool, is64: bool) -> Result<(u32, u32)> {
    match ops.get(idx) {
        None => Ok((0, 0)),
        Some(Op::Shift(kind, amount)) => {
            if *kind == 3 && !allow_ror {
                return Err(err("ror is not allowed here"));
            }
            let max = if is64 { 63 } else { 31 };
            if *amount > max {
                return Err(err("shift amount out of range"));
            }
            Ok((*kind, *amount))
        }
        Some(_) => Err(err("expected a shift")),
    }
}

fn add_sub(m: &str, ops: &[Op], sub: bool, set: bool) -> Result<u32> {
    if ops.len() < 3 {
        return Err(err(format!("`{m}` expects at least 3 operands")));
    }
    let op = u32::from(sub) << 30 | u32::from(set) << 29;
    match &ops[2] {
        Op::Imm(v) => {
            let rd = if set {
                gp_or_zr(&ops[0])?
            } else {
                gp_or_sp(&ops[0])?
            };
            let rn = gp_or_sp(&ops[1])?;
            same_width(&[rd, rn])?;
            let (mut val, mut sh) = (*v, 0);
            if let Some(Op::Shift(0, 12)) = ops.get(3) {
                sh = 1;
            } else if ops.len() > 3 {
                return Err(err("only `lsl #12` may follow the immediate"));
            }
            let (op, val2) = if val < 0 {
                (op ^ (1 << 30), -val)
            } else {
                (op, val)
            };
            val = val2;
            if sh == 0 && val > 0xfff && val.trailing_zeros() >= 12 && val <= 0xff_f000 {
                sh = 1;
                val >>= 12;
            }
            if val > 0xfff {
                return Err(err("immediate does not fit in 12 bits"));
            }
            Ok(sf(rd) | op | 0x1100_0000 | sh << 22 | (val as u32) << 10 | rn.num << 5 | rd.num)
        }
        Op::Reg(_) => {
            let rd = gp_or_zr(&ops[0])?;
            let rn = gp_or_zr(&ops[1])?;
            let rm = gp_or_zr(&ops[2])?;
            same_width(&[rd, rn, rm])?;
            let (kind, amount) = shift_of(ops, 3, false, rd.is64)?;
            Ok(sf(rd)
                | op
                | 0x0B00_0000
                | kind << 22
                | rm.num << 16
                | amount << 10
                | rn.num << 5
                | rd.num)
        }
        _ => Err(err(format!("bad operands for `{m}`"))),
    }
}

fn logical(ops: &[Op], opc: u32, negate: bool) -> Result<u32> {
    if ops.len() < 3 {
        return Err(err("logical instruction expects 3 operands"));
    }
    let rd = gp_or_zr(&ops[0])?;
    let rn = gp_or_zr(&ops[1])?;
    let rm = match &ops[2] {
        Op::Reg(r) if r.kind != RegKind::Sp => *r,
        Op::Imm(_) => return Err(err("logical bitmask immediates are not supported")),
        _ => return Err(err("expected a register")),
    };
    same_width(&[rd, rn, rm])?;
    let (kind, amount) = shift_of(ops, 3, true, rd.is64)?;
    Ok(sf(rd)
        | opc << 29
        | 0x0A00_0000
        | kind << 22
        | u32::from(negate) << 21
        | rm.num << 16
        | amount << 10
        | rn.num << 5
        | rd.num)
}

fn zr(is64: bool) -> Op {
    Op::Reg(Reg {
        num: 31,
        is64,
        kind: RegKind::Zr,
    })
}

fn mov_wide(rd: Reg, opc: u32, value: i128, shift: u32) -> Result<u32> {
    if !(0..=0xffff).contains(&value) {
        return Err(err("immediate does not fit in 16 bits"));
    }
    if !shift.is_multiple_of(16) || shift > if rd.is64 { 48 } else { 16 } {
        return Err(err("bad shift for wide move"));
    }
    Ok(sf(rd) | opc << 29 | 0x1280_0000 | (shift / 16) << 21 | (value as u32) << 5 | rd.num)
}

fn mov_imm(rd: Reg, v: i128) -> Result<u32> {
    let width = if rd.is64 { 64 } else { 32 };
    let mask: u128 = if rd.is64 {
        u128::from(u64::MAX)
    } else {
        u128::from(u32::MAX)
    };
    if v < -(1i128 << (width - 1)) || v > mask as i128 {
        return Err(err("immediate out of range"));
    }
    let u = (v as u128) & mask;
    for shift in (0..width).step_by(16) {
        if u & !(0xffffu128 << shift) == 0 {
            return mov_wide(rd, 0b10, ((u >> shift) & 0xffff) as i128, shift);
        }
        let n = !u & mask;
        if n & !(0xffffu128 << shift) == 0 {
            return mov_wide(rd, 0b00, ((n >> shift) & 0xffff) as i128, shift);
        }
    }
    Err(err(
        "immediate needs more than one instruction (use movz/movk)",
    ))
}

fn bitfield(rd: Reg, rn: Reg, signed: bool, immr: u32, imms: u32) -> u32 {
    let base = if signed { 0x1300_0000 } else { 0x5300_0000 };
    sf(rd) | base | u32::from(rd.is64) << 22 | immr << 16 | imms << 10 | rn.num << 5 | rd.num
}

#[derive(Clone, Copy)]
struct MemForm {
    /// log2 of the access size.
    size: u32,
    load: bool,
    signed_word: bool,
}

fn load_store(m: &str, ops: &[Op], form: MemForm, pc: u64, unscaled: bool) -> Result<u32> {
    if ops.len() < 2 {
        return Err(err(format!("`{m}` expects 2 operands")));
    }
    let rt = gp_or_zr(&ops[0])?;
    let opc = if form.signed_word {
        0b10
    } else {
        u32::from(form.load)
    };
    let base_bits = form.size << 30 | 0x3800_0000 | opc << 22;
    match (&ops[1], ops.get(2)) {
        (Op::Imm(addr), None) if form.load && (form.size >= 2) && !unscaled => {
            // ldr literal
            let imm19 = rel(*addr, pc, 19, 2)?;
            let opc = if form.signed_word {
                0b10
            } else {
                u32::from(form.size == 3)
            };
            Ok(opc << 30 | 0x1800_0000 | imm19 << 5 | rt.num)
        }
        (Op::Mem { base, offset, pre }, None) => {
            let fits_scaled =
                *offset >= 0 && offset % (1 << form.size) == 0 && (offset >> form.size) <= 0xfff;
            if *pre {
                let imm9 = simm9(*offset)?;
                Ok(base_bits | imm9 << 12 | 0b11 << 10 | base.num << 5 | rt.num)
            } else if !unscaled && fits_scaled {
                let imm12 = (offset >> form.size) as u32;
                Ok(base_bits | 1 << 24 | imm12 << 10 | base.num << 5 | rt.num)
            } else {
                let imm9 = simm9(*offset)?;
                Ok(base_bits | imm9 << 12 | base.num << 5 | rt.num)
            }
        }
        (
            Op::Mem {
                base,
                offset: 0,
                pre: false,
            },
            Some(Op::Imm(post)),
        ) => {
            let imm9 = simm9(*post)?;
            Ok(base_bits | imm9 << 12 | 0b01 << 10 | base.num << 5 | rt.num)
        }
        _ => Err(err(format!("bad addressing mode for `{m}`"))),
    }
}

fn simm9(v: i128) -> Result<u32> {
    if !(-256..=255).contains(&v) {
        return Err(err("offset does not fit in 9 bits"));
    }
    Ok((v as u32) & 0x1ff)
}

fn pair(m: &str, ops: &[Op], load: bool) -> Result<u32> {
    if ops.len() < 3 {
        return Err(err(format!("`{m}` expects 3 operands")));
    }
    let t1 = gp_or_zr(&ops[0])?;
    let t2 = gp_or_zr(&ops[1])?;
    same_width(&[t1, t2])?;
    let scale = if t1.is64 { 3 } else { 2 };
    let opc = if t1.is64 { 0b10 } else { 0b00 };
    let (base, offset, mode) = match (&ops[2], ops.get(3)) {
        (
            Op::Mem {
                base,
                offset,
                pre: true,
            },
            None,
        ) => (*base, *offset, 0b011),
        (
            Op::Mem {
                base,
                offset,
                pre: false,
            },
            None,
        ) => (*base, *offset, 0b010),
        (
            Op::Mem {
                base,
                offset: 0,
                pre: false,
            },
            Some(Op::Imm(post)),
        ) => (*base, *post, 0b001),
        _ => return Err(err(format!("bad addressing mode for `{m}`"))),
    };
    if offset % (1 << scale) != 0 {
        return Err(err("pair offset is not aligned"));
    }
    let imm7 = offset >> scale;
    if !(-64..=63).contains(&imm7) {
        return Err(err("pair offset out of range"));
    }
    Ok(opc << 30
        | 0x2800_0000
        | mode << 23
        | u32::from(load) << 22
        | ((imm7 as u32) & 0x7f) << 15
        | t2.num << 10
        | base.num << 5
        | t1.num)
}

/// Assembles one statement at `pc` and returns the instruction word.
pub fn assemble_one(statement: &str, pc: u64) -> Result<u32> {
    let text = statement.trim().to_ascii_lowercase();
    let (m, rest) = match text.split_once(char::is_whitespace) {
        Some((m, r)) => (m.to_string(), r.trim().to_string()),
        None => (text.clone(), String::new()),
    };
    let ops: Vec<Op> = split_ops(&rest)
        .iter()
        .map(|s| parse_op(s))
        .collect::<Result<_>>()?;
    if !pc.is_multiple_of(4) {
        return Err(err("ARM64 instructions must be 4-byte aligned"));
    }
    let m = m.as_str();
    if let Some(c) = m.strip_prefix("b.") {
        want(&ops, 1, m)?;
        let c = cond(c).ok_or_else(|| err(format!("unknown condition `{c}`")))?;
        return Ok(0x5400_0000 | rel(target(&ops[0])?, pc, 19, 2)? << 5 | c);
    }
    let word = match m {
        "nop" => 0xd503_201f,
        "ret" => {
            let rn = match ops.first() {
                Some(op) => gp_or_zr(op)?.num,
                None => 30,
            };
            0xd65f_0000 | rn << 5
        }
        "br" | "blr" => {
            want(&ops, 1, m)?;
            let rn = gp_or_zr(&ops[0])?;
            if !rn.is64 {
                return Err(err("expected an x register"));
            }
            (if m == "br" { 0xd61f_0000 } else { 0xd63f_0000 }) | rn.num << 5
        }
        "b" | "bl" => {
            want(&ops, 1, m)?;
            let imm26 = rel(target(&ops[0])?, pc, 26, 2)?;
            (if m == "b" { 0x1400_0000 } else { 0x9400_0000 }) | imm26
        }
        "cbz" | "cbnz" => {
            want(&ops, 2, m)?;
            let rt = gp_or_zr(&ops[0])?;
            sf(rt)
                | if m == "cbz" { 0x3400_0000 } else { 0x3500_0000 }
                | rel(target(&ops[1])?, pc, 19, 2)? << 5
                | rt.num
        }
        "tbz" | "tbnz" => {
            want(&ops, 3, m)?;
            let rt = gp_or_zr(&ops[0])?;
            let bit = match ops[1] {
                Op::Imm(b) if (0..if rt.is64 { 64 } else { 32 }).contains(&b) => b as u32,
                _ => return Err(err("bad bit number")),
            };
            (bit >> 5) << 31
                | if m == "tbz" { 0x3600_0000 } else { 0x3700_0000 }
                | (bit & 31) << 19
                | rel(target(&ops[2])?, pc, 14, 2)? << 5
                | rt.num
        }
        "svc" | "brk" | "hlt" => {
            want(&ops, 1, m)?;
            let v = match ops[0] {
                Op::Imm(v) if (0..=0xffff).contains(&v) => v as u32,
                _ => return Err(err("expected a 16-bit immediate")),
            };
            let base = match m {
                "svc" => 0xd400_0001,
                "brk" => 0xd420_0000,
                _ => 0xd440_0000,
            };
            base | v << 5
        }
        "adr" | "adrp" => {
            want(&ops, 2, m)?;
            let rd = gp_or_zr(&ops[0])?;
            if !rd.is64 {
                return Err(err("expected an x register"));
            }
            let t = target(&ops[1])?;
            let delta = if m == "adr" {
                t - i128::from(pc)
            } else {
                (t >> 12) - (i128::from(pc) >> 12)
            };
            if !(-(1 << 20)..(1 << 20)).contains(&delta) {
                return Err(err("target out of range"));
            }
            let d = (delta as u32) & 0x1f_ffff;
            (if m == "adr" { 0x1000_0000 } else { 0x9000_0000 })
                | (d & 3) << 29
                | (d >> 2) << 5
                | rd.num
        }
        "mov" => {
            want(&ops, 2, m)?;
            match (&ops[0], &ops[1]) {
                (Op::Reg(rd), Op::Reg(rn)) => {
                    same_width(&[*rd, *rn])?;
                    if rd.kind == RegKind::Sp || rn.kind == RegKind::Sp {
                        sf(*rd) | 0x1100_0000 | rn.num << 5 | rd.num
                    } else {
                        sf(*rd) | 0x2A00_03E0 | rn.num << 16 | rd.num
                    }
                }
                (Op::Reg(rd), Op::Imm(v)) => mov_imm(gp_or_zr(&ops[0]).map(|_| *rd)?, *v)?,
                _ => return Err(err("bad operands for `mov`")),
            }
        }
        "movz" | "movn" | "movk" => {
            if ops.len() < 2 {
                return Err(err(format!("`{m}` expects 2 operands")));
            }
            let rd = gp_or_zr(&ops[0])?;
            let v = target(&ops[1])?;
            let shift = match ops.get(2) {
                Some(Op::Shift(0, s)) => *s,
                None => 0,
                _ => return Err(err("expected `lsl #n`")),
            };
            let opc = match m {
                "movn" => 0b00,
                "movz" => 0b10,
                _ => 0b11,
            };
            mov_wide(rd, opc, v, shift)?
        }
        "add" => add_sub(m, &ops, false, false)?,
        "adds" => add_sub(m, &ops, false, true)?,
        "sub" => add_sub(m, &ops, true, false)?,
        "subs" => add_sub(m, &ops, true, true)?,
        "cmp" | "cmn" => {
            if ops.len() < 2 {
                return Err(err(format!("`{m}` expects 2 operands")));
            }
            let rn = match &ops[0] {
                Op::Reg(r) => *r,
                _ => return Err(err("expected a register")),
            };
            let mut full = vec![zr(rn.is64)];
            full.extend(ops.iter().cloned());
            add_sub(m, &full, m == "cmp", true)?
        }
        "and" => logical(&ops, 0b00, false)?,
        "bic" => logical(&ops, 0b00, true)?,
        "orr" => logical(&ops, 0b01, false)?,
        "orn" => logical(&ops, 0b01, true)?,
        "eor" => logical(&ops, 0b10, false)?,
        "eon" => logical(&ops, 0b10, true)?,
        "ands" => logical(&ops, 0b11, false)?,
        "bics" => logical(&ops, 0b11, true)?,
        "tst" | "mvn" => {
            if ops.is_empty() {
                return Err(err(format!("`{m}` expects operands")));
            }
            let r = gp_or_zr(&ops[0])?;
            let mut full = if m == "tst" {
                vec![zr(r.is64)]
            } else {
                vec![ops[0].clone(), zr(r.is64)]
            };
            full.extend(ops.iter().skip(usize::from(m == "mvn")).cloned());
            if m == "tst" {
                logical(&full, 0b11, false)?
            } else {
                logical(&full, 0b01, true)?
            }
        }
        "mul" | "madd" | "msub" => {
            let (rd, rn, rm, ra) = if m == "mul" {
                want(&ops, 3, m)?;
                let rd = gp_or_zr(&ops[0])?;
                (rd, gp_or_zr(&ops[1])?, gp_or_zr(&ops[2])?, 31)
            } else {
                want(&ops, 4, m)?;
                (
                    gp_or_zr(&ops[0])?,
                    gp_or_zr(&ops[1])?,
                    gp_or_zr(&ops[2])?,
                    gp_or_zr(&ops[3])?.num,
                )
            };
            same_width(&[rd, rn, rm])?;
            sf(rd)
                | 0x1B00_0000
                | u32::from(m == "msub") << 15
                | rm.num << 16
                | ra << 10
                | rn.num << 5
                | rd.num
        }
        "udiv" | "sdiv" | "lslv" | "lsrv" | "asrv" => {
            want(&ops, 3, m)?;
            let (rd, rn, rm) = (gp_or_zr(&ops[0])?, gp_or_zr(&ops[1])?, gp_or_zr(&ops[2])?);
            same_width(&[rd, rn, rm])?;
            let opcode = match m {
                "udiv" => 0x02,
                "sdiv" => 0x03,
                "lslv" => 0x08,
                "lsrv" => 0x09,
                _ => 0x0a,
            };
            sf(rd) | 0x1AC0_0000 | rm.num << 16 | opcode << 10 | rn.num << 5 | rd.num
        }
        "lsl" | "lsr" | "asr" => {
            want(&ops, 3, m)?;
            let rd = gp_or_zr(&ops[0])?;
            let rn = gp_or_zr(&ops[1])?;
            same_width(&[rd, rn])?;
            match &ops[2] {
                Op::Reg(rm) => {
                    let rm = gp_or_zr(&Op::Reg(*rm))?;
                    same_width(&[rd, rm])?;
                    let opcode = match m {
                        "lsl" => 0x08,
                        "lsr" => 0x09,
                        _ => 0x0a,
                    };
                    sf(rd) | 0x1AC0_0000 | rm.num << 16 | opcode << 10 | rn.num << 5 | rd.num
                }
                Op::Imm(sh) => {
                    let w = if rd.is64 { 64u32 } else { 32 };
                    let sh = u32::try_from(*sh)
                        .ok()
                        .filter(|s| *s < w)
                        .ok_or_else(|| err("shift amount out of range"))?;
                    match m {
                        "lsl" => bitfield(rd, rn, false, (w - sh) % w, w - 1 - sh),
                        "lsr" => bitfield(rd, rn, false, sh, w - 1),
                        _ => bitfield(rd, rn, true, sh, w - 1),
                    }
                }
                _ => return Err(err("bad shift operand")),
            }
        }
        "csel" => {
            want(&ops, 4, m)?;
            let (rd, rn, rm) = (gp_or_zr(&ops[0])?, gp_or_zr(&ops[1])?, gp_or_zr(&ops[2])?);
            same_width(&[rd, rn, rm])?;
            let Op::Cond(c) = ops[3] else {
                return Err(err("expected a condition"));
            };
            sf(rd) | 0x1A80_0000 | rm.num << 16 | c << 12 | rn.num << 5 | rd.num
        }
        "cset" => {
            want(&ops, 2, m)?;
            let rd = gp_or_zr(&ops[0])?;
            let Op::Cond(c) = ops[1] else {
                return Err(err("expected a condition"));
            };
            if c >= 14 {
                return Err(err("cset cannot use al/nv"));
            }
            // csinc rd, zr, zr, invert(c)
            sf(rd) | 0x1A80_0400 | 31 << 16 | (c ^ 1) << 12 | 31 << 5 | rd.num
        }
        "ldr" | "str" | "ldur" | "stur" => {
            let rt = gp_or_zr(ops.first().ok_or_else(|| err("missing register"))?)?;
            let form = MemForm {
                size: if rt.is64 { 3 } else { 2 },
                load: m.starts_with("ld"),
                signed_word: false,
            };
            load_store(m, &ops, form, pc, m.ends_with("ur"))?
        }
        "ldrb" | "strb" | "ldrh" | "strh" | "ldurb" | "sturb" | "ldurh" | "sturh" => {
            let rt = gp_or_zr(ops.first().ok_or_else(|| err("missing register"))?)?;
            if rt.is64 {
                return Err(err("byte/halfword accesses use w registers"));
            }
            let form = MemForm {
                size: u32::from(m.ends_with('h')),
                load: m.starts_with("ld"),
                signed_word: false,
            };
            load_store(m, &ops, form, pc, m.contains("ur"))?
        }
        "ldrsw" => {
            let rt = gp_or_zr(ops.first().ok_or_else(|| err("missing register"))?)?;
            if !rt.is64 {
                return Err(err("ldrsw needs an x register"));
            }
            let form = MemForm {
                size: 2,
                load: true,
                signed_word: true,
            };
            load_store(m, &ops, form, pc, false)?
        }
        "ldp" | "stp" => pair(m, &ops, m == "ldp")?,
        _ => return Err(err(format!("unsupported ARM64 mnemonic `{m}`"))),
    };
    Ok(word)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::Arch;
    use crate::disasm::decode_one;

    fn enc(s: &str) -> u32 {
        assemble_one(s, 0x1000).unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    #[test]
    fn known_words() {
        assert_eq!(enc("nop"), 0xd503_201f);
        assert_eq!(enc("ret"), 0xd65f_03c0);
        assert_eq!(enc("bl 0x1008"), 0x9400_0002);
        assert_eq!(enc("b 0xffc"), 0x17ff_ffff);
        assert_eq!(enc("mov x0, x1"), 0xaa01_03e0);
        assert_eq!(enc("mov x29, sp"), 0x9100_03fd);
        assert_eq!(enc("mov w0, #0"), 0x5280_0000);
        assert_eq!(enc("mov x0, #-1"), 0x9280_0000);
        assert_eq!(enc("add x0, x1, #16"), 0x9100_4020);
        assert_eq!(enc("sub sp, sp, #0x20"), 0xd100_83ff);
        assert_eq!(enc("stp x29, x30, [sp, #-16]!"), 0xa9bf_7bfd);
        assert_eq!(enc("ldp x29, x30, [sp], #16"), 0xa8c1_7bfd);
        assert_eq!(enc("ldr x0, [x1, #8]"), 0xf940_0420);
        assert_eq!(enc("str w2, [x3]"), 0xb900_0062);
        assert_eq!(enc("svc #0x80"), 0xd400_1001);
        assert_eq!(enc("cmp x0, #1"), 0xf100_041f);
    }

    #[test]
    fn roundtrip_with_capstone() {
        let cases = [
            ("b.ne 0x1010", "b.ne #0x1010"),
            ("cbz x3, 0x1020", "cbz x3, #0x1020"),
            ("cbnz w3, 0xff0", "cbnz w3, #0xff0"),
            ("tbnz x5, #33, 0x1004", "tbnz x5, #0x21, #0x1004"),
            ("adrp x0, 0x5000", "adrp x0, #0x5000"),
            ("adr x1, 0x1010", "adr x1, #0x1010"),
            ("blr x8", "blr x8"),
            ("br x16", "br x16"),
            ("ret x1", "ret x1"),
            ("movz x0, #0x1234, lsl #16", "mov x0, #0x12340000"),
            ("movk x0, #0xbeef", "movk x0, #0xbeef"),
            ("add x0, x1, x2, lsl #3", "add x0, x1, x2, lsl #3"),
            ("subs w0, w1, w2", "subs w0, w1, w2"),
            ("and x0, x1, x2", "and x0, x1, x2"),
            ("eor w3, w4, w5, ror #2", "eor w3, w4, w5, ror #2"),
            ("tst x0, x1", "tst x0, x1"),
            ("mvn x0, x1", "mvn x0, x1"),
            ("mul x0, x1, x2", "mul x0, x1, x2"),
            ("udiv w0, w1, w2", "udiv w0, w1, w2"),
            ("lsl x0, x1, #4", "lsl x0, x1, #4"),
            ("lsr w0, w1, #4", "lsr w0, w1, #4"),
            ("asr x0, x1, x2", "asr x0, x1, x2"),
            ("csel x0, x1, x2, eq", "csel x0, x1, x2, eq"),
            ("cset w0, ne", "cset w0, ne"),
            ("ldrb w0, [x1, #3]", "ldrb w0, [x1, #3]"),
            ("strh w0, [x1, #2]", "strh w0, [x1, #2]"),
            ("ldur x0, [x1, #-8]", "ldur x0, [x1, #-8]"),
            ("ldr x0, [x1, #-8]", "ldur x0, [x1, #-8]"),
            ("str x0, [sp, #-16]!", "str x0, [sp, #-0x10]!"),
            ("ldr x0, 0x1100", "ldr x0, #0x1100"),
            ("ldrsw x0, [x1, #4]", "ldrsw x0, [x1, #4]"),
            ("stp w1, w2, [x0, #8]", "stp w1, w2, [x0, #8]"),
            ("brk #1", "brk #1"),
            ("add w0, w1, #1, lsl #12", "add w0, w1, #1, lsl #12"),
            ("cmn x2, #5", "cmn x2, #5"),
        ];
        for (src, expected) in cases {
            let w = enc(src);
            let insn = decode_one(Arch::Arm64, &w.to_le_bytes(), 0x1000)
                .unwrap()
                .unwrap_or_else(|| panic!("{src}: undecodable {w:#x}"));
            assert_eq!(insn.text(), expected, "{src}");
        }
    }

    #[test]
    fn errors() {
        assert!(assemble_one("b 0x1002", 0x1000).is_err());
        assert!(assemble_one("b 0x10000000", 0).is_err());
        assert!(assemble_one("add x0, w1, #1", 0).is_err());
        assert!(assemble_one("add x0, x1, #0x1001", 0).is_err());
        assert!(assemble_one("mov x0, #0x12345678", 0).is_err());
        assert!(assemble_one("and x0, x1, #1", 0).is_err());
        assert!(assemble_one("fmov d0, d1", 0).is_err());
        assert!(assemble_one("nop", 2).is_err());
    }
}
