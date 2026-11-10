//! Disassembler (ANL-12) and branch-target extraction (ANL-13).
//!
//! Every architecture of [`Arch`] except WebAssembly is decoded by Capstone
//! 5 (vendored C sources built by `capstone-sys`). WebAssembly is decoded by
//! a small built-in decoder because the Rust bindings do not expose
//! Capstone's WASM module. AVR is not available in Capstone 5 and is
//! therefore not offered.
//!
//! Undecodable bytes never stop the listing: they are emitted as one-unit
//! `.byte` pseudo instructions with [`Instruction::is_data`] set.

mod wasm;

use capstone::arch::ArchOperand;
use capstone::{Capstone, Endian as CsEndian, ExtraMode, Mode};
use hexa_core::Tree;
use serde::{Deserialize, Serialize};

use crate::arch::Arch;
use crate::error::{ReError, Result};

/// Semantic group of an instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InsnGroup {
    /// Any jump (conditional or not).
    Jump,
    /// Subroutine call.
    Call,
    /// Return from subroutine.
    Ret,
    /// Software interrupt / trap / system call.
    Interrupt,
    /// Return from interrupt.
    InterruptReturn,
    /// Privileged instruction.
    Privilege,
    /// Branch with a PC-relative target.
    BranchRelative,
}

/// One decoded instruction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Instruction {
    /// Virtual address of the first byte.
    pub address: u64,
    /// Length in bytes.
    pub size: u32,
    /// Raw encoding.
    pub bytes: Vec<u8>,
    /// Mnemonic (lower case).
    pub mnemonic: String,
    /// Operand text.
    pub operands: String,
    /// Semantic groups (jump / call / ret ...).
    pub groups: Vec<InsnGroup>,
    /// Direct branch or call target, when statically known (ANL-13).
    pub branch_target: Option<u64>,
    /// Statically resolved memory reference (x86 RIP-relative or absolute
    /// operands, ARM64 `adr`/`adrp`, ARM PC-relative loads).
    pub memory_reference: Option<u64>,
    /// `true` when the bytes could not be decoded (`.byte` pseudo op).
    pub is_data: bool,
}

impl Instruction {
    /// `mnemonic operands` text.
    pub fn text(&self) -> String {
        if self.operands.is_empty() {
            self.mnemonic.clone()
        } else {
            format!("{} {}", self.mnemonic, self.operands)
        }
    }

    /// Whether the instruction is a call.
    pub fn is_call(&self) -> bool {
        self.groups.contains(&InsnGroup::Call)
    }

    /// Whether the instruction is a jump.
    pub fn is_jump(&self) -> bool {
        self.groups.contains(&InsnGroup::Jump)
    }

    /// Whether the instruction returns.
    pub fn is_ret(&self) -> bool {
        self.groups.contains(&InsnGroup::Ret)
    }

    /// Address of the byte following the instruction.
    pub fn end(&self) -> u64 {
        self.address.wrapping_add(u64::from(self.size))
    }
}

/// Options for [`disassemble`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(Default)]
pub struct DisasmOptions {
    /// Stop after this many instructions (0 = no limit).
    pub max_instructions: usize,
    /// Use AT&T syntax for x86 (Intel otherwise).
    pub att_syntax: bool,
}

pub(crate) fn capstone_for(arch: Arch, detail: bool) -> Result<Capstone> {
    use capstone::Arch as A;
    let none = std::iter::empty::<ExtraMode>();
    let (a, mode, extra, endian): (A, Mode, Option<ExtraMode>, Option<CsEndian>) = match arch {
        Arch::X86Real => (A::X86, Mode::Mode16, None, None),
        Arch::X86 => (A::X86, Mode::Mode32, None, None),
        Arch::X64 => (A::X86, Mode::Mode64, None, None),
        Arch::Arm => (A::ARM, Mode::Arm, None, Some(CsEndian::Little)),
        Arch::Thumb => (A::ARM, Mode::Thumb, None, Some(CsEndian::Little)),
        Arch::Arm64 => (A::ARM64, Mode::Arm, None, Some(CsEndian::Little)),
        Arch::Mips32 => (A::MIPS, Mode::Mips32, None, Some(CsEndian::Little)),
        Arch::Mips32Be => (A::MIPS, Mode::Mips32, None, Some(CsEndian::Big)),
        Arch::Mips64 => (A::MIPS, Mode::Mips64, None, Some(CsEndian::Little)),
        Arch::Mips64Be => (A::MIPS, Mode::Mips64, None, Some(CsEndian::Big)),
        Arch::Ppc32 => (A::PPC, Mode::Mode32, None, Some(CsEndian::Big)),
        Arch::Ppc64 => (A::PPC, Mode::Mode64, None, Some(CsEndian::Big)),
        Arch::Ppc64Le => (A::PPC, Mode::Mode64, None, Some(CsEndian::Little)),
        Arch::RiscV32 => (A::RISCV, Mode::RiscV32, Some(ExtraMode::RiscVC), None),
        Arch::RiscV64 => (A::RISCV, Mode::RiscV64, Some(ExtraMode::RiscVC), None),
        Arch::Sparc => (A::SPARC, Mode::Default, None, Some(CsEndian::Big)),
        Arch::SparcV9 => (A::SPARC, Mode::V9, None, Some(CsEndian::Big)),
        Arch::SysZ => (A::SYSZ, Mode::Default, None, Some(CsEndian::Big)),
        Arch::M68k => (A::M68K, Mode::M68k040, None, Some(CsEndian::Big)),
        Arch::Mos6502 => (A::MOS65XX, Mode::Mos65xx6502, None, None),
        Arch::Mos65c02 => (A::MOS65XX, Mode::Mos65xx65c02, None, None),
        Arch::W65816 => (A::MOS65XX, Mode::Mos65xx65816LongMx, None, None),
        Arch::Bpf => (A::BPF, Mode::Cbpf, None, Some(CsEndian::Little)),
        Arch::Ebpf => (A::BPF, Mode::Ebpf, None, Some(CsEndian::Little)),
        Arch::Sh4 => (A::SH, Mode::Sh4a, None, Some(CsEndian::Little)),
        Arch::TriCore => (A::TRICORE, Mode::TriCore162, None, None),
        Arch::Evm => (A::EVM, Mode::Default, None, None),
        Arch::M6809 => (A::M680X, Mode::M680x6809, None, None),
        Arch::Xcore => (A::XCORE, Mode::Default, None, Some(CsEndian::Big)),
        Arch::Tms320c64x => (A::TMS320C64X, Mode::Default, None, Some(CsEndian::Big)),
        Arch::Wasm => {
            return Err(ReError::Unsupported {
                what: "capstone",
                arch: arch.label().into(),
            });
        }
    };
    let mut cs = match extra {
        Some(x) => Capstone::new_raw(a, mode, std::iter::once(x), endian),
        None => Capstone::new_raw(a, mode, none, endian),
    }
    .map_err(|e| ReError::Disasm(e.to_string()))?;
    cs.set_detail(detail)
        .map_err(|e| ReError::Disasm(e.to_string()))?;
    Ok(cs)
}

/// Minimum instruction unit used to skip undecodable bytes.
fn skip_unit(arch: Arch) -> usize {
    match arch {
        Arch::Arm
        | Arch::Arm64
        | Arch::Mips32
        | Arch::Mips32Be
        | Arch::Mips64
        | Arch::Mips64Be
        | Arch::Ppc32
        | Arch::Ppc64
        | Arch::Ppc64Le
        | Arch::Sparc
        | Arch::SparcV9
        | Arch::Tms320c64x => 4,
        Arch::Thumb
        | Arch::RiscV32
        | Arch::RiscV64
        | Arch::SysZ
        | Arch::M68k
        | Arch::Sh4
        | Arch::TriCore
        | Arch::Xcore => 2,
        Arch::Bpf | Arch::Ebpf => 8,
        _ => 1,
    }
}

/// Disassembles `code`, whose first byte lives at virtual address
/// `address` (ANL-12).
pub fn disassemble(
    arch: Arch,
    code: &[u8],
    address: u64,
    options: &DisasmOptions,
) -> Result<Vec<Instruction>> {
    if arch == Arch::Wasm {
        return Ok(wasm::disassemble(code, address, options.max_instructions));
    }
    let mut cs = capstone_for(arch, true)?;
    if options.att_syntax && arch.is_x86() {
        cs.set_syntax(capstone::Syntax::Att)
            .map_err(|e| ReError::Disasm(e.to_string()))?;
    }
    let limit = if options.max_instructions == 0 {
        usize::MAX
    } else {
        options.max_instructions
    };
    let unit = skip_unit(arch);
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < code.len() && out.len() < limit {
        let addr = address.wrapping_add(pos as u64);
        let insns = cs
            .disasm_count(&code[pos..], addr, limit - out.len())
            .map_err(|e| ReError::Disasm(e.to_string()))?;
        let mut consumed = 0usize;
        for insn in insns.iter() {
            consumed += insn.bytes().len();
            out.push(convert(&cs, arch, insn));
        }
        pos += consumed;
        if out.len() >= limit || pos >= code.len() {
            break;
        }
        if consumed == 0 || insns.is_empty() || pos < code.len() {
            // Capstone stopped on undecodable bytes: emit a data unit.
            let n = unit.min(code.len() - pos);
            let bytes = code[pos..pos + n].to_vec();
            out.push(data_insn(address.wrapping_add(pos as u64), bytes));
            pos += n;
        }
    }
    Ok(out)
}

/// Disassembles `len` bytes of a tree starting at file offset `offset`,
/// treating that offset as virtual address `address`.
pub fn disassemble_tree(
    arch: Arch,
    tree: &Tree,
    offset: u64,
    len: usize,
    address: u64,
    options: &DisasmOptions,
) -> Result<Vec<Instruction>> {
    let code = tree.read_vec(offset, len)?;
    disassemble(arch, &code, address, options)
}

/// Decodes exactly one instruction at the start of `code`.
pub fn decode_one(arch: Arch, code: &[u8], address: u64) -> Result<Option<Instruction>> {
    let opts = DisasmOptions {
        max_instructions: 1,
        ..DisasmOptions::default()
    };
    Ok(disassemble(arch, code, address, &opts)?
        .into_iter()
        .next()
        .filter(|i| !i.is_data))
}

fn data_insn(address: u64, bytes: Vec<u8>) -> Instruction {
    let operands = bytes
        .iter()
        .map(|b| format!("0x{b:02x}"))
        .collect::<Vec<_>>()
        .join(", ");
    Instruction {
        address,
        size: bytes.len() as u32,
        bytes,
        mnemonic: ".byte".into(),
        operands,
        groups: Vec::new(),
        branch_target: None,
        memory_reference: None,
        is_data: true,
    }
}

fn convert(cs: &Capstone, arch: Arch, insn: &capstone::Insn<'_>) -> Instruction {
    let address = insn.address();
    let bytes = insn.bytes().to_vec();
    let size = bytes.len() as u32;
    let mnemonic = insn.mnemonic().unwrap_or("").to_string();
    let operands = insn.op_str().unwrap_or("").to_string();
    let mut groups = Vec::new();
    let mut ops: Vec<ArchOperand> = Vec::new();
    if let Ok(detail) = cs.insn_detail(insn) {
        for g in detail.groups() {
            let grp = match g.0 {
                1 => Some(InsnGroup::Jump),
                2 => Some(InsnGroup::Call),
                3 => Some(InsnGroup::Ret),
                4 => Some(InsnGroup::Interrupt),
                5 => Some(InsnGroup::InterruptReturn),
                6 => Some(InsnGroup::Privilege),
                7 => Some(InsnGroup::BranchRelative),
                _ => None,
            };
            if let Some(grp) = grp
                && !groups.contains(&grp)
            {
                groups.push(grp);
            }
        }
        ops = detail.arch_detail().operands();
    }
    fixup_groups(arch, &mnemonic, &operands, &mut groups);
    let is_branch = groups
        .iter()
        .any(|g| matches!(g, InsnGroup::Jump | InsnGroup::Call));
    let branch_target = if is_branch {
        let raw = branch_imm(arch, &ops).or_else(|| parse_target_text(&operands));
        if matches!(arch, Arch::RiscV32 | Arch::RiscV64)
            && !mnemonic.ends_with("jalr")
            && !mnemonic.ends_with("jr")
        {
            // Capstone reports RISC-V branch immediates as PC offsets.
            raw.map(|off| {
                let t = address.wrapping_add(off);
                if arch == Arch::RiscV32 {
                    t & 0xffff_ffff
                } else {
                    t
                }
            })
        } else {
            raw
        }
    } else {
        None
    };
    let memory_reference = memory_ref(cs, arch, address, size, &mnemonic, &ops);
    Instruction {
        address,
        size,
        bytes,
        mnemonic,
        operands,
        groups,
        branch_target,
        memory_reference,
        is_data: false,
    }
}

/// Capstone 5 does not tag every branch on every architecture; complete
/// the groups from well-known mnemonics.
fn fixup_groups(arch: Arch, mnemonic: &str, operands: &str, groups: &mut Vec<InsnGroup>) {
    let add = |g: InsnGroup, groups: &mut Vec<InsnGroup>| {
        if !groups.contains(&g) {
            groups.push(g);
        }
    };
    match arch {
        Arch::Arm64 => {
            if mnemonic == "bl" || mnemonic == "blr" {
                add(InsnGroup::Call, groups);
            } else if mnemonic == "ret" {
                add(InsnGroup::Ret, groups);
            } else if mnemonic == "b"
                || mnemonic.starts_with("b.")
                || mnemonic == "br"
                || mnemonic.starts_with("cb")
                || mnemonic.starts_with("tb")
            {
                add(InsnGroup::Jump, groups);
            }
        }
        Arch::Arm | Arch::Thumb => {
            if mnemonic.starts_with("bl") {
                add(InsnGroup::Call, groups);
            } else if (mnemonic.starts_with("bx") && operands == "lr")
                || (mnemonic.starts_with("pop") && operands.contains("pc"))
            {
                add(InsnGroup::Ret, groups);
            }
        }
        Arch::RiscV32 | Arch::RiscV64 => {
            if mnemonic == "ret" || mnemonic == "c.jr" && operands == "ra" {
                add(InsnGroup::Ret, groups);
            } else if mnemonic == "jal" || mnemonic == "c.jal" || mnemonic == "jalr" {
                if operands.starts_with("zero") {
                    add(InsnGroup::Jump, groups);
                } else {
                    add(InsnGroup::Call, groups);
                }
            } else if mnemonic == "j" || mnemonic == "c.j" || mnemonic.starts_with('b') {
                add(InsnGroup::Jump, groups);
            }
        }
        Arch::Mips32 | Arch::Mips32Be | Arch::Mips64 | Arch::Mips64Be => {
            if mnemonic == "jr" && operands == "$ra" {
                add(InsnGroup::Ret, groups);
            } else if mnemonic.starts_with("jal")
                || mnemonic.starts_with("bal")
                || mnemonic.ends_with("al") && mnemonic.starts_with('b')
            {
                add(InsnGroup::Call, groups);
            } else if mnemonic == "j"
                || mnemonic == "jr"
                || mnemonic.starts_with('b') && mnemonic != "break"
            {
                add(InsnGroup::Jump, groups);
            }
        }
        Arch::Ppc32 | Arch::Ppc64 | Arch::Ppc64Le => {
            if mnemonic == "blr" {
                add(InsnGroup::Ret, groups);
            } else if mnemonic == "bla"
                || (mnemonic.starts_with('b')
                    && mnemonic.ends_with('l')
                    && !mnemonic.ends_with("lrl"))
            {
                add(InsnGroup::Call, groups);
            } else if mnemonic.starts_with('b') {
                add(InsnGroup::Jump, groups);
            }
        }
        Arch::Sparc | Arch::SparcV9 => {
            if mnemonic == "call" {
                add(InsnGroup::Call, groups);
            } else if mnemonic == "ret" || mnemonic == "retl" {
                add(InsnGroup::Ret, groups);
            } else if mnemonic.starts_with('b') || mnemonic.starts_with("fb") || mnemonic == "jmp" {
                add(InsnGroup::Jump, groups);
            }
        }
        Arch::M68k | Arch::Sh4 => {
            if mnemonic.starts_with("jsr") || mnemonic.starts_with("bsr") {
                add(InsnGroup::Call, groups);
            } else if mnemonic.starts_with("rts") || mnemonic == "rtd" {
                add(InsnGroup::Ret, groups);
            } else if mnemonic.starts_with("jmp")
                || mnemonic.starts_with("bra")
                || (mnemonic.starts_with('b')
                    && !mnemonic.starts_with("bt")
                    && arch == Arch::M68k
                    && !mnemonic.starts_with("bchg")
                    && !mnemonic.starts_with("bclr")
                    && !mnemonic.starts_with("bset")
                    && !mnemonic.starts_with("bf"))
                || (arch == Arch::Sh4 && matches!(mnemonic, "bt" | "bf" | "bt/s" | "bf/s"))
            {
                add(InsnGroup::Jump, groups);
            }
        }
        _ => {}
    }
}

fn branch_imm(arch: Arch, ops: &[ArchOperand]) -> Option<u64> {
    let mask = |v: i64| -> u64 {
        match arch.pointer_size() {
            2 => (v as u64) & 0xffff,
            4 => (v as u64) & 0xffff_ffff,
            _ => v as u64,
        }
    };
    ops.iter().rev().find_map(|op| match op {
        ArchOperand::X86Operand(o) => match o.op_type {
            capstone::arch::x86::X86OperandType::Imm(v) => Some(mask(v)),
            _ => None,
        },
        ArchOperand::ArmOperand(o) => match o.op_type {
            capstone::arch::arm::ArmOperandType::Imm(v) => Some(u64::from(v as u32)),
            _ => None,
        },
        ArchOperand::Arm64Operand(o) => match o.op_type {
            capstone::arch::arm64::Arm64OperandType::Imm(v) => Some(v as u64),
            _ => None,
        },
        ArchOperand::MipsOperand(capstone::arch::mips::MipsOperand::Imm(v))
        | ArchOperand::PpcOperand(capstone::arch::ppc::PpcOperand::Imm(v))
        | ArchOperand::SysZOperand(capstone::arch::sysz::SysZOperand::Imm(v))
        | ArchOperand::RiscVOperand(capstone::arch::riscv::RiscVOperand::Imm(v)) => Some(mask(*v)),
        ArchOperand::Mos65xxOperand(capstone::arch::mos65xx::Mos65xxOperand::Mem(v)) => {
            Some(u64::from(*v))
        }
        ArchOperand::Mos65xxOperand(capstone::arch::mos65xx::Mos65xxOperand::Imm(v)) => {
            Some(u64::from(*v) & 0xffff)
        }
        _ => None,
    })
}

/// Parses a trailing absolute address (`0x1234`, `$1234`, `#0x10`) from
/// operand text.
fn parse_target_text(operands: &str) -> Option<u64> {
    let last = operands.rsplit([',', ' ']).next()?.trim();
    let last = last.trim_start_matches('#');
    if let Some(h) = last.strip_prefix("0x") {
        u64::from_str_radix(h, 16).ok()
    } else if let Some(h) = last.strip_prefix('$') {
        u64::from_str_radix(h, 16).ok()
    } else {
        None
    }
}

fn memory_ref(
    cs: &Capstone,
    arch: Arch,
    address: u64,
    size: u32,
    mnemonic: &str,
    ops: &[ArchOperand],
) -> Option<u64> {
    for op in ops {
        match op {
            ArchOperand::X86Operand(o) => {
                if let capstone::arch::x86::X86OperandType::Mem(m) = &o.op_type {
                    let base = cs.reg_name(m.base()).unwrap_or_default();
                    if base == "rip" {
                        return Some(
                            address
                                .wrapping_add(u64::from(size))
                                .wrapping_add(m.disp() as u64),
                        );
                    }
                    if m.base().0 == 0 && m.index().0 == 0 {
                        let v = m.disp() as u64;
                        return Some(if arch == Arch::X64 {
                            v
                        } else {
                            v & 0xffff_ffff
                        });
                    }
                }
            }
            ArchOperand::Arm64Operand(o) => {
                if (mnemonic == "adr" || mnemonic == "adrp")
                    && let capstone::arch::arm64::Arm64OperandType::Imm(v) = o.op_type
                {
                    return Some(v as u64);
                }
            }
            ArchOperand::ArmOperand(o) => {
                if let capstone::arch::arm::ArmOperandType::Mem(m) = &o.op_type
                    && cs.reg_name(m.base()).as_deref() == Some("pc")
                {
                    let pc = if arch == Arch::Thumb {
                        (address + 4) & !3
                    } else {
                        address + 8
                    };
                    let disp = i64::from(m.disp());
                    let disp = if o.subtracted { -disp } else { disp };
                    return Some(pc.wrapping_add(disp as u64) & 0xffff_ffff);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dis(arch: Arch, code: &[u8], addr: u64) -> Vec<Instruction> {
        disassemble(arch, code, addr, &DisasmOptions::default()).unwrap()
    }

    #[test]
    fn x64_basic_and_branches() {
        // push rbp; mov rbp, rsp; call +0; jmp short -2; ret
        let code = [
            0x55, 0x48, 0x89, 0xe5, 0xe8, 0x00, 0x00, 0x00, 0x00, 0xeb, 0xfe, 0xc3,
        ];
        let v = dis(Arch::X64, &code, 0x1000);
        assert_eq!(v.len(), 5);
        assert_eq!(v[0].mnemonic, "push");
        assert_eq!(v[0].operands, "rbp");
        assert_eq!(v[1].text(), "mov rbp, rsp");
        assert!(v[2].is_call());
        assert_eq!(v[2].branch_target, Some(0x1009));
        assert!(v[3].is_jump());
        assert_eq!(v[3].branch_target, Some(0x1009));
        assert!(v[4].is_ret());
        assert_eq!(v[4].branch_target, None);
    }

    #[test]
    fn x64_rip_relative() {
        // lea rax, [rip + 0x10]
        let v = dis(Arch::X64, &[0x48, 0x8d, 0x05, 0x10, 0, 0, 0], 0x2000);
        assert_eq!(v[0].memory_reference, Some(0x2017));
    }

    #[test]
    fn x86_16_and_32() {
        let v = dis(Arch::X86, &[0xb8, 0x78, 0x56, 0x34, 0x12], 0);
        assert_eq!(v[0].text(), "mov eax, 0x12345678");
        let v = dis(Arch::X86Real, &[0xb8, 0x34, 0x12], 0);
        assert_eq!(v[0].text(), "mov ax, 0x1234");
        // mov eax, [0x401000]
        let v = dis(Arch::X86, &[0xa1, 0x00, 0x10, 0x40, 0x00], 0);
        assert_eq!(v[0].memory_reference, Some(0x40_1000));
    }

    #[test]
    fn invalid_bytes_become_data() {
        // 0x0f 0x0b = ud2 is valid; 0xd6 is invalid in 64-bit mode.
        let v = dis(Arch::X64, &[0x90, 0xd6, 0x90], 0);
        assert_eq!(v.len(), 3);
        assert!(v[1].is_data);
        assert_eq!(v[1].mnemonic, ".byte");
        assert_eq!(v[2].address, 2);
    }

    #[test]
    fn arm64_branches() {
        // bl #+8 ; b #-4 ; ret ; adrp x0, #0x1000
        let words: [u32; 4] = [0x9400_0002, 0x17ff_ffff, 0xd65f_03c0, 0xb000_0000];
        let code: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let v = dis(Arch::Arm64, &code, 0x4000);
        assert_eq!(v[0].mnemonic, "bl");
        assert!(v[0].is_call());
        assert_eq!(v[0].branch_target, Some(0x4008));
        assert!(v[1].is_jump());
        assert_eq!(v[1].branch_target, Some(0x4000));
        assert!(v[2].is_ret());
        assert_eq!(v[3].mnemonic, "adrp");
        assert_eq!(v[3].memory_reference, Some(0x5000));
    }

    #[test]
    fn arm_and_thumb() {
        // bl #+8 (A32): eb000000 at 0x8000 -> target 0x8008
        let v = dis(Arch::Arm, &0xeb00_0000u32.to_le_bytes(), 0x8000);
        assert_eq!(v[0].mnemonic, "bl");
        assert_eq!(v[0].branch_target, Some(0x8008));
        // thumb: bx lr = 0x4770
        let v = dis(Arch::Thumb, &[0x70, 0x47], 0);
        assert_eq!(v[0].mnemonic, "bx");
        assert!(v[0].is_ret());
        // ldr r0, [pc, #4] (A32) e59f0004 at 0x100 -> 0x10c
        let v = dis(Arch::Arm, &0xe59f_0004u32.to_le_bytes(), 0x100);
        assert_eq!(v[0].memory_reference, Some(0x10c));
    }

    #[test]
    fn mips_ppc_riscv_sparc() {
        // MIPS BE: jal 0x400000 -> 0x0c100000
        let v = dis(Arch::Mips32Be, &0x0c10_0000u32.to_be_bytes(), 0);
        assert_eq!(v[0].mnemonic, "jal");
        assert_eq!(v[0].branch_target, Some(0x40_0000));
        // MIPS LE nop
        let v = dis(Arch::Mips32, &[0, 0, 0, 0], 0);
        assert_eq!(v[0].mnemonic, "nop");
        // PPC: bl +0x10 -> 0x48000011
        let v = dis(Arch::Ppc32, &0x4800_0011u32.to_be_bytes(), 0x100);
        assert_eq!(v[0].mnemonic, "bl");
        assert_eq!(v[0].branch_target, Some(0x110));
        // RISC-V: jal ra, +8 -> 0x008000ef
        let v = dis(Arch::RiscV64, &0x0080_00efu32.to_le_bytes(), 0x1000);
        assert!(v[0].is_call(), "{v:?}");
        assert_eq!(v[0].branch_target, Some(0x1008));
        // MIPS BE: beq $zero, $zero, +8 at 0x100 -> 0x10000002
        let v = dis(Arch::Mips32Be, &0x1000_0002u32.to_be_bytes(), 0x100);
        assert!(v[0].is_jump(), "{v:?}");
        assert_eq!(v[0].branch_target, Some(0x10c));
        // RISC-V: beq a0, a1, -4
        let v = dis(Arch::RiscV32, &0xfeb5_0ee3u32.to_le_bytes(), 0x2000);
        assert_eq!(v[0].branch_target, Some(0x1ffc), "{v:?}");
        // SPARC: call +0x10 -> 0x40000004
        let v = dis(Arch::Sparc, &0x4000_0004u32.to_be_bytes(), 0x1000);
        assert!(v[0].is_call(), "{v:?}");
        assert_eq!(v[0].branch_target, Some(0x1010), "{v:?}");
        // SPARC: nop 0x01000000
        let v = dis(Arch::Sparc, &0x0100_0000u32.to_be_bytes(), 0);
        assert_eq!(v[0].mnemonic, "nop");
    }

    #[test]
    fn other_archs_decode() {
        assert_eq!(dis(Arch::Mos6502, &[0xea], 0)[0].mnemonic, "nop");
        let v = dis(Arch::Mos6502, &[0x4c, 0x34, 0x12], 0);
        assert_eq!(v[0].mnemonic, "jmp");
        assert_eq!(v[0].branch_target, Some(0x1234));
        assert_eq!(dis(Arch::M68k, &[0x4e, 0x71], 0)[0].mnemonic, "nop");
        assert_eq!(dis(Arch::M68k, &[0x4e, 0x75], 0)[0].mnemonic, "rts");
        assert_eq!(dis(Arch::SysZ, &[0x07, 0xfe], 0)[0].mnemonic, "br");
        assert!(!dis(Arch::Ebpf, &[0x95, 0, 0, 0, 0, 0, 0, 0], 0)[0].is_data);
        assert!(!dis(Arch::Evm, &[0x60, 0x01], 0)[0].is_data);
        for arch in Arch::ALL {
            // Every architecture must at least open.
            let _ = disassemble(arch, &[0; 8], 0, &DisasmOptions::default()).unwrap();
        }
    }

    #[test]
    fn max_instructions_and_att() {
        let code = [0x90; 10];
        let opts = DisasmOptions {
            max_instructions: 3,
            att_syntax: true,
        };
        assert_eq!(disassemble(Arch::X64, &code, 0, &opts).unwrap().len(), 3);
        let v = disassemble(Arch::X64, &[0x48, 0x89, 0xe5], 0, &opts).unwrap();
        assert_eq!(v[0].text(), "movq %rsp, %rbp");
    }

    #[test]
    fn tree_input() {
        let tree = Tree::from_piece(hexa_core::piece::Piece::bytes(vec![0xcc, 0x90, 0xc3]));
        let v =
            disassemble_tree(Arch::X86, &tree, 1, 2, 0x40_1001, &DisasmOptions::default()).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].address, 0x40_1001);
        assert!(v[1].is_ret());
    }
}
