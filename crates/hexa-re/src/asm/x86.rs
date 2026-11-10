//! Intel-syntax x86 / x86-64 assembler on top of the `iced-x86` encoder
//! (ANL-14).
//!
//! The parser turns one statement into operands (register, memory,
//! immediate). Every `iced_x86::Code` whose mnemonic and operand kinds
//! match is then encoded and the shortest encoding wins, which mirrors what
//! conventional assemblers do (`jmp` picks `rel8` when the target is in
//! range, `add eax, 1` picks the sign-extended `imm8` form, and so on).

use std::collections::HashMap;
use std::sync::OnceLock;

use iced_x86::{
    Code, Encoder, EncodingKind, Instruction, MemorySize, Mnemonic, OpCodeOperandKind as K, OpKind,
    Register, RepPrefixKind,
};

use super::parse_int;
use crate::error::{ReError, Result};

#[derive(Debug, Clone, Copy, PartialEq)]
struct Mem {
    size: Option<usize>,
    seg: Register,
    base: Register,
    index: Register,
    scale: u32,
    disp: i128,
    /// `[rip + disp]`: `disp` is relative to the next instruction.
    rip_relative: bool,
    /// `[rel addr]`: `disp` is an absolute RIP-relative target.
    rip_absolute: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Operand {
    Reg(Register),
    Mem(Mem),
    Imm(i128),
}

fn registers() -> &'static HashMap<String, Register> {
    static MAP: OnceLock<HashMap<String, Register>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut m = HashMap::new();
        for r in Register::values() {
            if r == Register::None {
                continue;
            }
            let name = format!("{r:?}").to_ascii_lowercase();
            if name.starts_with("dontuse") {
                continue;
            }
            m.insert(name, r);
        }
        for i in 8..16 {
            if let Some(&r) = m.get(&format!("r{i}l")) {
                m.insert(format!("r{i}b"), r);
            }
        }
        m.insert("st".into(), Register::ST0);
        for i in 0..8 {
            if let Some(&r) = m.get(&format!("st{i}")) {
                m.insert(format!("st({i})"), r);
            }
        }
        m
    })
}

fn mnemonics() -> &'static HashMap<String, Vec<Code>> {
    static MAP: OnceLock<HashMap<String, Vec<Code>>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut m: HashMap<String, Vec<Code>> = HashMap::new();
        for code in Code::values() {
            let info = code.op_code();
            if !info.is_instruction()
                || code.mnemonic() == Mnemonic::INVALID
                || info.decoder_option() != 0
                || matches!(info.encoding(), EncodingKind::MVEX)
            {
                continue;
            }
            let name = format!("{:?}", code.mnemonic()).to_ascii_lowercase();
            m.entry(name).or_default().push(code);
        }
        m
    })
}

/// Maps assembler aliases (`jz`, `cmovnae`, `sal`, ...) to iced names.
fn canonical_mnemonic(m: &str) -> String {
    const CC: [(&str, &str); 16] = [
        ("z", "e"),
        ("nz", "ne"),
        ("c", "b"),
        ("nae", "b"),
        ("nc", "ae"),
        ("nb", "ae"),
        ("na", "be"),
        ("nbe", "a"),
        ("nge", "l"),
        ("nl", "ge"),
        ("ng", "le"),
        ("nle", "g"),
        ("pe", "p"),
        ("po", "np"),
        ("ecxz", "ecxz"),
        ("rcxz", "rcxz"),
    ];
    match m {
        "sal" => return "shl".into(),
        "retn" => return "ret".into(),
        "fwait" => return "wait".into(),
        "xlat" => return "xlatb".into(),
        "movabs" => return "mov".into(),
        "int1" | "icebp" => return "int1".into(),
        _ => {}
    }
    for prefix in ["cmov", "set", "j"] {
        if let Some(cc) = m.strip_prefix(prefix) {
            for (alias, real) in CC {
                if cc == alias {
                    return format!("{prefix}{real}");
                }
            }
        }
    }
    m.to_string()
}

fn err(msg: impl Into<String>) -> ReError {
    ReError::Asm(msg.into())
}

fn split_operands(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for c in s.chars() {
        match c {
            '[' | '(' => {
                depth += 1;
                cur.push(c);
            }
            ']' | ')' => {
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

fn size_keyword(w: &str) -> Option<usize> {
    Some(match w {
        "byte" => 1,
        "word" => 2,
        "dword" => 4,
        "fword" => 6,
        "qword" => 8,
        "tword" | "tbyte" => 10,
        "oword" | "xmmword" => 16,
        "ymmword" => 32,
        "zmmword" => 64,
        _ => return None,
    })
}

fn parse_operand(text: &str) -> Result<Operand> {
    let t = text.trim().to_ascii_lowercase();
    if let Some(&r) = registers().get(t.replace(' ', "").as_str()) {
        return Ok(Operand::Reg(r));
    }
    let mut rest = t.as_str();
    let mut size = None;
    if let Some((w, tail)) = rest.split_once(char::is_whitespace)
        && let Some(sz) = size_keyword(w)
    {
        size = Some(sz);
        rest = tail.trim_start();
        if let Some(tail) = rest.strip_prefix("ptr") {
            rest = tail.trim_start();
        }
    }
    let mut seg = Register::None;
    if let Some(open) = rest.find('[') {
        let prefix = rest[..open].trim().trim_end_matches(':').trim();
        if !prefix.is_empty() {
            match registers().get(prefix) {
                Some(&r) if r.is_segment_register() => seg = r,
                _ => return Err(err(format!("bad memory operand prefix `{prefix}`"))),
            }
        }
        let close = rest
            .rfind(']')
            .ok_or_else(|| err(format!("missing `]` in `{text}`")))?;
        let mut mem = parse_address(&rest[open + 1..close])?;
        if seg == Register::None {
            seg = mem.seg;
        }
        mem.seg = seg;
        mem.size = size;
        return Ok(Operand::Mem(mem));
    }
    if size.is_some() {
        return Err(err(format!(
            "size keyword on a non-memory operand `{text}`"
        )));
    }
    parse_int(rest)
        .map(Operand::Imm)
        .ok_or_else(|| err(format!("unrecognised operand `{text}`")))
}

fn parse_address(inner: &str) -> Result<Mem> {
    let mut mem = Mem {
        size: None,
        seg: Register::None,
        base: Register::None,
        index: Register::None,
        scale: 0,
        disp: 0,
        rip_relative: false,
        rip_absolute: false,
    };
    let mut s = inner.trim().to_string();
    if let Some(r) = s.strip_prefix("rel ") {
        mem.rip_absolute = true;
        s = r.trim().to_string();
    }
    if let Some((p, r)) = s.split_once(':')
        && let Some(&sr) = registers().get(p.trim())
        && sr.is_segment_register()
    {
        mem.seg = sr;
        s = r.to_string();
    }
    // Tokenise into signed terms.
    let mut terms: Vec<(bool, String)> = Vec::new();
    let mut neg = false;
    let mut cur = String::new();
    for c in s.chars() {
        if c == '+' || c == '-' {
            if !cur.trim().is_empty() {
                terms.push((neg, cur.trim().to_string()));
            }
            cur.clear();
            neg = c == '-';
        } else {
            cur.push(c);
        }
    }
    if !cur.trim().is_empty() {
        terms.push((neg, cur.trim().to_string()));
    }
    for (neg, term) in terms {
        let term = term.replace(' ', "");
        if let Some((a, b)) = term.split_once('*') {
            let (reg, scale) = match (registers().get(a), registers().get(b)) {
                (Some(&r), None) => (r, b),
                (None, Some(&r)) => (r, a),
                _ => return Err(err(format!("bad scaled index `{term}`"))),
            };
            let scale = parse_int(scale)
                .filter(|v| matches!(v, 1 | 2 | 4 | 8))
                .ok_or_else(|| err(format!("bad scale in `{term}`")))?;
            if neg || mem.index != Register::None {
                return Err(err(format!("bad index `{term}`")));
            }
            mem.index = reg;
            mem.scale = scale as u32;
        } else if let Some(&r) = registers().get(term.as_str()) {
            if neg {
                return Err(err("registers cannot be subtracted"));
            }
            if r == Register::RIP || r == Register::EIP {
                mem.base = r;
                mem.rip_relative = true;
            } else if mem.base == Register::None {
                mem.base = r;
            } else if mem.index == Register::None {
                mem.index = r;
                mem.scale = 1;
            } else {
                return Err(err("too many registers in memory operand"));
            }
        } else {
            let v = parse_int(&term).ok_or_else(|| err(format!("bad displacement `{term}`")))?;
            mem.disp += if neg { -v } else { v };
        }
    }
    if mem.rip_absolute {
        if mem.base != Register::None || mem.index != Register::None {
            return Err(err("`rel` takes an absolute address only"));
        }
        mem.base = Register::RIP;
    }
    if mem.index != Register::None && mem.scale == 0 {
        mem.scale = 1;
    }
    Ok(mem)
}

enum Slot {
    Reg(fn(Register) -> bool),
    Fixed(Register),
    RegOrMem(fn(Register) -> bool),
    Mem,
    MemOffs,
    Imm(OpKind, i128, i128),
    Imm8Second,
    ImmConst1,
    Branch(u32),
}

fn slot(k: K) -> Option<Slot> {
    use Slot::{Branch, Fixed, Imm, Mem, MemOffs, Reg, RegOrMem};
    Some(match k {
        K::mem
        | K::mem_mpx
        | K::mem_mib
        | K::mem_vsib32x
        | K::mem_vsib64x
        | K::mem_vsib32y
        | K::mem_vsib64y
        | K::mem_vsib32z
        | K::mem_vsib64z
        | K::sibmem => Mem,
        K::mem_offs => MemOffs,
        K::r8_or_mem => RegOrMem(Register::is_gpr8),
        K::r16_or_mem => RegOrMem(Register::is_gpr16),
        K::r32_or_mem | K::r32_or_mem_mpx => RegOrMem(Register::is_gpr32),
        K::r64_or_mem | K::r64_or_mem_mpx => RegOrMem(Register::is_gpr64),
        K::mm_or_mem => RegOrMem(Register::is_mm),
        K::xmm_or_mem => RegOrMem(Register::is_xmm),
        K::ymm_or_mem => RegOrMem(Register::is_ymm),
        K::zmm_or_mem => RegOrMem(Register::is_zmm),
        K::bnd_or_mem_mpx => RegOrMem(Register::is_bnd),
        K::k_or_mem => RegOrMem(Register::is_k),
        K::r8_reg | K::r8_opcode => Reg(Register::is_gpr8),
        K::r16_reg | K::r16_reg_mem | K::r16_rm | K::r16_opcode => Reg(Register::is_gpr16),
        K::r32_reg | K::r32_reg_mem | K::r32_rm | K::r32_opcode | K::r32_vvvv => {
            Reg(Register::is_gpr32)
        }
        K::r64_reg | K::r64_reg_mem | K::r64_rm | K::r64_opcode | K::r64_vvvv => {
            Reg(Register::is_gpr64)
        }
        K::seg_reg => Reg(Register::is_segment_register),
        K::k_reg | K::kp1_reg | K::k_rm | K::k_vvvv => Reg(Register::is_k),
        K::mm_reg | K::mm_rm => Reg(Register::is_mm),
        K::xmm_reg | K::xmm_rm | K::xmm_vvvv | K::xmmp3_vvvv | K::xmm_is4 | K::xmm_is5 => {
            Reg(Register::is_xmm)
        }
        K::ymm_reg | K::ymm_rm | K::ymm_vvvv | K::ymm_is4 | K::ymm_is5 => Reg(Register::is_ymm),
        K::zmm_reg | K::zmm_rm | K::zmm_vvvv | K::zmmp3_vvvv => Reg(Register::is_zmm),
        K::cr_reg => Reg(Register::is_cr),
        K::dr_reg => Reg(Register::is_dr),
        K::tr_reg => Reg(Register::is_tr),
        K::bnd_reg => Reg(Register::is_bnd),
        K::tmm_reg | K::tmm_rm | K::tmm_vvvv => Reg(Register::is_tmm),
        K::sti_opcode => Reg(Register::is_st),
        K::es => Fixed(Register::ES),
        K::cs => Fixed(Register::CS),
        K::ss => Fixed(Register::SS),
        K::ds => Fixed(Register::DS),
        K::fs => Fixed(Register::FS),
        K::gs => Fixed(Register::GS),
        K::al => Fixed(Register::AL),
        K::cl => Fixed(Register::CL),
        K::ax => Fixed(Register::AX),
        K::dx => Fixed(Register::DX),
        K::eax => Fixed(Register::EAX),
        K::rax => Fixed(Register::RAX),
        K::st0 => Fixed(Register::ST0),
        K::imm4_m2z => Imm(OpKind::Immediate8, 0, 15),
        K::imm8 => Slot::Imm8Second,
        K::imm8_const_1 => Slot::ImmConst1,
        K::imm8sex16 => Imm(OpKind::Immediate8to16, -128, 127),
        K::imm8sex32 => Imm(OpKind::Immediate8to32, -128, 127),
        K::imm8sex64 => Imm(OpKind::Immediate8to64, -128, 127),
        K::imm16 => Imm(OpKind::Immediate16, -0x8000, 0xffff),
        K::imm32 => Imm(
            OpKind::Immediate32,
            i128::from(i32::MIN),
            i128::from(u32::MAX),
        ),
        K::imm32sex64 => Imm(
            OpKind::Immediate32to64,
            i128::from(i32::MIN),
            i128::from(i32::MAX),
        ),
        K::imm64 => Imm(
            OpKind::Immediate64,
            i128::from(i64::MIN),
            i128::from(u64::MAX),
        ),
        K::br16_1 | K::br16_2 | K::xbegin_2 => Branch(16),
        K::br32_1 | K::br32_4 | K::xbegin_4 => Branch(32),
        K::br64_1 | K::br64_4 => Branch(64),
        _ => return None,
    })
}

/// Accepts unsigned spellings of negative sign-extended immediates, e.g.
/// `0xfffffff0` for an `imm8sex32` slot.
fn normalise_sext(kind: OpKind, v: i128) -> i128 {
    let width = match kind {
        OpKind::Immediate8to16 => 16,
        OpKind::Immediate8to32 => 32,
        OpKind::Immediate8to64 | OpKind::Immediate32to64 => 64,
        _ => return v,
    };
    let top = 1i128 << width;
    if v >= top / 2 && v < top { v - top } else { v }
}

fn build(
    code: Code,
    bitness: u32,
    ops: &[Operand],
    rip_target: Option<u64>,
) -> Option<Instruction> {
    let info = code.op_code();
    if info.op_count() as usize != ops.len() {
        return None;
    }
    let kinds = info.op_kinds();
    // Branch instructions have a single target operand.
    if let [single] = kinds
        && let Some(Slot::Branch(bits)) = slot(*single)
    {
        let Operand::Imm(target) = ops[0] else {
            return None;
        };
        if bits != bitness || target < 0 {
            return None;
        }
        return Instruction::with_branch(code, u64::try_from(target).ok()?).ok();
    }
    let mut ins = Instruction::default();
    ins.set_code(code);
    for (i, (&k, op)) in kinds.iter().zip(ops).enumerate() {
        let i = i as u32;
        let s = slot(k)?;
        match (s, op) {
            (Slot::Reg(pred) | Slot::RegOrMem(pred), Operand::Reg(r)) if pred(*r) => {
                ins.set_op_kind(i, OpKind::Register);
                ins.set_op_register(i, *r);
            }
            (Slot::Fixed(want), Operand::Reg(r)) if want == *r => {
                ins.set_op_kind(i, OpKind::Register);
                ins.set_op_register(i, *r);
            }
            (Slot::RegOrMem(_) | Slot::Mem | Slot::MemOffs, Operand::Mem(m)) => {
                let offs = matches!(k, K::mem_offs);
                if offs && (m.base != Register::None || m.index != Register::None) {
                    return None;
                }
                ins.set_op_kind(i, OpKind::Memory);
                ins.set_memory_base(m.base);
                ins.set_memory_index(m.index);
                ins.set_memory_index_scale(if m.index == Register::None {
                    1
                } else {
                    m.scale
                });
                ins.set_segment_prefix(m.seg);
                let disp = if m.base == Register::RIP {
                    i128::from(rip_target.unwrap_or(0))
                } else {
                    m.disp
                };
                let absolute = m.base == Register::None && m.index == Register::None;
                let displ_size = if absolute || offs {
                    bitness / 8
                } else {
                    u32::from(disp != 0)
                };
                ins.set_memory_displ_size(displ_size);
                ins.set_memory_displacement64(disp as u64);
            }
            (Slot::Imm(kind, lo, hi), Operand::Imm(v)) => {
                let v = normalise_sext(kind, *v);
                if v < lo || v > hi {
                    return None;
                }
                ins.set_op_kind(i, kind);
                ins.try_set_immediate_u64(i, v as u64).ok()?;
            }
            (Slot::Imm8Second, Operand::Imm(v)) => {
                if *v < -128 || *v > 255 {
                    return None;
                }
                let prev_imm = i > 0 && matches!(kinds[i as usize - 1], K::imm8 | K::imm16);
                let kind = if prev_imm {
                    OpKind::Immediate8_2nd
                } else {
                    OpKind::Immediate8
                };
                ins.set_op_kind(i, kind);
                ins.try_set_immediate_u64(i, *v as u64).ok()?;
            }
            (Slot::ImmConst1, Operand::Imm(1)) => {
                ins.set_op_kind(i, OpKind::Immediate8);
                ins.try_set_immediate_u64(i, 1).ok()?;
            }
            _ => return None,
        }
    }
    Some(ins)
}

fn encode(ins: &Instruction, bitness: u32, ip: u64) -> Option<Vec<u8>> {
    let mut enc = Encoder::new(bitness);
    enc.encode(ins, ip).ok()?;
    Some(enc.take_buffer())
}

fn string_instruction(
    mnemonic: &str,
    bitness: u32,
    rep: RepPrefixKind,
) -> Option<Result<Instruction>> {
    let a = bitness;
    let seg = Register::None;
    let r = match mnemonic {
        "movsb" => Instruction::with_movsb(a, seg, rep),
        "movsw" => Instruction::with_movsw(a, seg, rep),
        "movsd" => Instruction::with_movsd(a, seg, rep),
        "movsq" => Instruction::with_movsq(a, seg, rep),
        "cmpsb" => Instruction::with_cmpsb(a, seg, rep),
        "cmpsw" => Instruction::with_cmpsw(a, seg, rep),
        "cmpsd" => Instruction::with_cmpsd(a, seg, rep),
        "cmpsq" => Instruction::with_cmpsq(a, seg, rep),
        "lodsb" => Instruction::with_lodsb(a, seg, rep),
        "lodsw" => Instruction::with_lodsw(a, seg, rep),
        "lodsd" => Instruction::with_lodsd(a, seg, rep),
        "lodsq" => Instruction::with_lodsq(a, seg, rep),
        "stosb" => Instruction::with_stosb(a, rep),
        "stosw" => Instruction::with_stosw(a, rep),
        "stosd" => Instruction::with_stosd(a, rep),
        "stosq" => Instruction::with_stosq(a, rep),
        "scasb" => Instruction::with_scasb(a, rep),
        "scasw" => Instruction::with_scasw(a, rep),
        "scasd" => Instruction::with_scasd(a, rep),
        "scasq" => Instruction::with_scasq(a, rep),
        _ => return None,
    };
    Some(r.map_err(|e| err(e.to_string())))
}

fn apply_prefixes(ins: &mut Instruction, lock: bool, rep: RepPrefixKind) {
    ins.set_has_lock_prefix(lock);
    if rep == RepPrefixKind::Repe {
        ins.set_has_repe_prefix(true);
    } else if rep == RepPrefixKind::Repne {
        ins.set_has_repne_prefix(true);
    }
}

/// Assembles one statement for the given bitness (16, 32 or 64) at `ip`.
pub fn assemble_one(bitness: u32, statement: &str, ip: u64) -> Result<Vec<u8>> {
    let text = statement.trim().to_ascii_lowercase();
    let mut words = text.splitn(2, char::is_whitespace);
    let mut head = words.next().unwrap_or("").to_string();
    let mut rest = words.next().unwrap_or("").trim().to_string();
    let mut lock = false;
    let mut rep = RepPrefixKind::None;
    loop {
        match head.as_str() {
            "lock" => lock = true,
            "rep" | "repe" | "repz" => rep = RepPrefixKind::Repe,
            "repne" | "repnz" => rep = RepPrefixKind::Repne,
            _ => break,
        }
        let mut w = rest.splitn(2, char::is_whitespace);
        head = w.next().unwrap_or("").to_string();
        rest = w.next().unwrap_or("").trim().to_string();
    }
    if head.is_empty() {
        return Err(err("missing mnemonic"));
    }
    if rest.is_empty()
        && let Some(ins) = string_instruction(&head, bitness, rep)
    {
        let mut ins = ins?;
        ins.set_has_lock_prefix(lock);
        return encode(&ins, bitness, ip).ok_or_else(|| err(format!("cannot encode `{head}`")));
    }
    let ops = split_operands(&rest)
        .iter()
        .map(|o| parse_operand(o))
        .collect::<Result<Vec<_>>>()?;
    let name = canonical_mnemonic(&head);
    let codes = mnemonics()
        .get(&name)
        .ok_or_else(|| err(format!("unknown mnemonic `{head}`")))?;
    let needs_rip_fixup = ops
        .iter()
        .any(|o| matches!(o, Operand::Mem(m) if m.rip_relative && !m.rip_absolute));
    let rip_abs = ops.iter().find_map(|o| match o {
        Operand::Mem(m) if m.rip_absolute => Some(m.disp as u64),
        _ => None,
    });
    let unsized_mem = ops
        .iter()
        .any(|o| matches!(o, Operand::Mem(m) if m.size.is_none()));

    // (bytes, memory size, code)
    let mut found: Vec<(Vec<u8>, usize, Code)> = Vec::new();
    for &code in codes {
        let info = code.op_code();
        let in_mode = match bitness {
            16 => info.mode16(),
            32 => info.mode32(),
            _ => info.mode64(),
        };
        if !in_mode {
            continue;
        }
        let Some(mut ins) = build(code, bitness, &ops, rip_abs.or(Some(ip))) else {
            continue;
        };
        let mem_size = ins.memory_size().size();
        if let Some(Operand::Mem(m)) = ops.iter().find(|o| matches!(o, Operand::Mem(_)))
            && let Some(want) = m.size
            && mem_size != 0
            && mem_size != want
            && ins.memory_size() != MemorySize::Unknown
        {
            continue;
        }
        apply_prefixes(&mut ins, lock, rep);
        let Some(mut bytes) = encode(&ins, bitness, ip) else {
            continue;
        };
        if needs_rip_fixup {
            let rel = ops
                .iter()
                .find_map(|o| match o {
                    Operand::Mem(m) if m.rip_relative => Some(m.disp),
                    _ => None,
                })
                .unwrap_or(0);
            let target = i128::from(ip) + bytes.len() as i128 + rel;
            let Some(ins2) = build(code, bitness, &ops, Some(target as u64)) else {
                continue;
            };
            let mut ins2 = ins2;
            apply_prefixes(&mut ins2, lock, rep);
            let Some(b2) = encode(&ins2, bitness, ip) else {
                continue;
            };
            bytes = b2;
        }
        found.push((bytes, mem_size, code));
    }
    if found.is_empty() {
        return Err(err(format!(
            "no encoding of `{}` matches its operands",
            statement.trim()
        )));
    }
    if unsized_mem {
        let mut sizes: Vec<usize> = found.iter().map(|f| f.1).filter(|&s| s != 0).collect();
        sizes.sort_unstable();
        sizes.dedup();
        if sizes.len() > 1 {
            let native = (bitness / 8) as usize;
            let stacky = matches!(name.as_str(), "push" | "pop" | "call" | "jmp");
            if stacky && sizes.contains(&native) {
                found.retain(|f| f.1 == native || f.1 == 0);
            } else {
                return Err(err(format!(
                    "operand size of `{}` is ambiguous; add byte/word/dword/qword ptr",
                    statement.trim()
                )));
            }
        }
    }
    let best = found
        .into_iter()
        .min_by_key(|f| f.0.len())
        .map(|f| f.0)
        .unwrap_or_default();
    Ok(best)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a64(s: &str) -> Vec<u8> {
        assemble_one(64, s, 0x1000).unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    fn a32(s: &str) -> Vec<u8> {
        assemble_one(32, s, 0x40_1000).unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    #[test]
    fn simple_encodings() {
        assert_eq!(a64("nop"), [0x90]);
        assert_eq!(a64("ret"), [0xc3]);
        assert_eq!(a64("int3"), [0xcc]);
        assert_eq!(a64("int 0x80"), [0xcd, 0x80]);
        assert_eq!(a64("push rbp"), [0x55]);
        assert_eq!(a64("mov rbp, rsp"), [0x48, 0x89, 0xe5]);
        assert_eq!(a64("xor eax, eax"), [0x31, 0xc0]);
        assert_eq!(a64("mov eax, 1"), [0xb8, 1, 0, 0, 0]);
        assert_eq!(a64("add rsp, 8"), [0x48, 0x83, 0xc4, 0x08]);
        assert_eq!(a64("sub rsp, 0x100"), [0x48, 0x81, 0xec, 0, 1, 0, 0]);
        assert_eq!(a64("and eax, 0xfffffff0"), [0x83, 0xe0, 0xf0]);
        assert_eq!(a64("and eax, -16"), [0x83, 0xe0, 0xf0]);
        assert_eq!(a64("shl eax, 1"), [0xd1, 0xe0]);
        assert_eq!(a64("sal eax, 3"), [0xc1, 0xe0, 0x03]);
        assert_eq!(
            a64("movabs rax, 0x1122334455667788"),
            [0x48, 0xb8, 0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11]
        );
        assert_eq!(a64("mov r8b, 1"), [0x41, 0xb0, 0x01]);
        assert_eq!(a64("syscall"), [0x0f, 0x05]);
    }

    #[test]
    fn branches_pick_shortest() {
        assert_eq!(a64("jmp 0x1002"), [0xeb, 0x00]);
        assert_eq!(a64("jmp 0x1000"), [0xeb, 0xfe]);
        assert_eq!(a64("jmp 0x2000"), [0xe9, 0xfb, 0x0f, 0, 0]);
        assert_eq!(a64("call 0x1005"), [0xe8, 0, 0, 0, 0]);
        assert_eq!(a64("jz 0x1010"), [0x74, 0x0e]);
        assert_eq!(a64("jne 0x5000"), [0x0f, 0x85, 0xfa, 0x3f, 0, 0]);
        assert_eq!(a32("call 0x401005"), [0xe8, 0, 0, 0, 0]);
        assert_eq!(a64("jmp rax"), [0xff, 0xe0]);
        assert_eq!(a64("call qword ptr [rax]"), [0xff, 0x10]);
        assert_eq!(a64("call [rax]"), [0xff, 0x10]);
    }

    #[test]
    fn memory_operands() {
        assert_eq!(a64("mov eax, dword ptr [rbx]"), [0x8b, 0x03]);
        assert_eq!(a64("mov eax, [rbx+8]"), [0x8b, 0x43, 0x08]);
        assert_eq!(a64("mov eax, [rbx+0x1000]"), [0x8b, 0x83, 0, 0x10, 0, 0]);
        assert_eq!(
            a64("mov rax, [rsp+rcx*8-8]"),
            [0x48, 0x8b, 0x44, 0xcc, 0xf8]
        );
        assert_eq!(
            a64("lea rax, [rip+0x10]"),
            [0x48, 0x8d, 0x05, 0x10, 0, 0, 0]
        );
        assert_eq!(
            a64("lea rax, [rel 0x1017]"),
            [0x48, 0x8d, 0x05, 0x10, 0, 0, 0]
        );
        assert_eq!(a64("mov dword ptr [rax], 5"), [0xc7, 0x00, 5, 0, 0, 0]);
        assert_eq!(a64("mov byte [rax], 5"), [0xc6, 0x00, 5]);
        assert_eq!(
            a64("mov rax, fs:[0x28]"),
            [0x64, 0x48, 0x8b, 0x04, 0x25, 0x28, 0, 0, 0]
        );
        assert_eq!(a32("mov eax, [0x402000]"), [0xa1, 0x00, 0x20, 0x40, 0x00]);
        assert_eq!(a32("mov ecx, [ebp-4]"), [0x8b, 0x4d, 0xfc]);
        assert!(assemble_one(64, "mov [rax], 5", 0).is_err());
        assert!(assemble_one(64, "inc [rax]", 0).is_err());
        assert_eq!(a64("movzx eax, byte ptr [rcx]"), [0x0f, 0xb6, 0x01]);
    }

    #[test]
    fn prefixes_and_strings() {
        assert_eq!(a64("lock inc dword ptr [rax]"), [0xf0, 0xff, 0x00]);
        assert_eq!(a64("rep movsb"), [0xf3, 0xa4]);
        assert_eq!(a64("stosq"), [0x48, 0xab]);
        assert_eq!(a64("repne scasb"), [0xf2, 0xae]);
        assert_eq!(a64("movsd xmm0, xmm1"), [0xf2, 0x0f, 0x10, 0xc1]);
    }

    #[test]
    fn sse_and_avx() {
        assert_eq!(a64("pxor xmm0, xmm0"), [0x66, 0x0f, 0xef, 0xc0]);
        assert_eq!(a64("vpxor ymm1, ymm2, ymm3"), [0xc5, 0xed, 0xef, 0xcb]);
    }

    #[test]
    fn sixteen_bit() {
        assert_eq!(
            assemble_one(16, "mov ax, 0x1234", 0).unwrap(),
            [0xb8, 0x34, 0x12]
        );
        assert_eq!(
            assemble_one(16, "mov ax, [bx+si+2]", 0).unwrap(),
            [0x8b, 0x40, 0x02]
        );
        assert_eq!(assemble_one(16, "jmp 0x10", 0).unwrap(), [0xeb, 0x0e]);
        assert_eq!(assemble_one(16, "int 0x21", 0).unwrap(), [0xcd, 0x21]);
    }

    #[test]
    fn errors() {
        assert!(assemble_one(64, "frobnicate eax", 0).is_err());
        assert!(assemble_one(64, "mov eax, rbx", 0).is_err());
        assert!(assemble_one(64, "mov eax, [rbx", 0).is_err());
        assert!(assemble_one(32, "push r8", 0).is_err());
        assert!(assemble_one(64, "add al, 0x1ff", 0).is_err());
    }

    #[test]
    fn roundtrip_through_disassembler() {
        use crate::arch::Arch;
        use crate::disasm::decode_one;
        for (src, text) in [
            (
                "mov ecx, dword ptr [rip + 0x20]",
                "mov ecx, dword ptr [rip + 0x20]",
            ),
            ("imul eax, ebx, 0x10", "imul eax, ebx, 0x10"),
            ("test al, al", "test al, al"),
            ("cmovnz rax, rbx", "cmovne rax, rbx"),
            ("setz al", "sete al"),
        ] {
            let b = a64(src);
            let i = decode_one(Arch::X64, &b, 0x1000).unwrap().unwrap();
            assert_eq!(i.text(), text, "{src}");
            assert_eq!(i.size as usize, b.len());
        }
    }
}
