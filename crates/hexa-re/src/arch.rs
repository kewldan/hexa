//! Processor architectures understood by the disassembler (ANL-12), the
//! assembler (ANL-14) and the patcher (RE-03).

use serde::{Deserialize, Serialize};

/// A processor architecture together with its execution mode and byte order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Arch {
    /// x86 real / 16-bit protected mode.
    #[serde(rename = "x86_16")]
    X86Real,
    /// x86 32-bit (IA-32).
    X86,
    /// x86-64 / AMD64.
    #[serde(rename = "x86_64")]
    X64,
    /// 32-bit ARM, A32 instruction set.
    Arm,
    /// 32-bit ARM, Thumb / Thumb-2 instruction set.
    Thumb,
    /// `AArch64` (ARM64), little endian.
    Arm64,
    /// MIPS32 little endian.
    Mips32,
    /// MIPS32 big endian.
    Mips32Be,
    /// MIPS64 little endian.
    Mips64,
    /// MIPS64 big endian.
    Mips64Be,
    /// PowerPC 32-bit big endian.
    Ppc32,
    /// PowerPC 64-bit big endian.
    Ppc64,
    /// PowerPC 64-bit little endian.
    Ppc64Le,
    /// RISC-V RV32GC.
    RiscV32,
    /// RISC-V RV64GC.
    RiscV64,
    /// SPARC (V8) big endian.
    Sparc,
    /// SPARC V9 big endian.
    SparcV9,
    /// IBM z/Architecture (`SystemZ`).
    SysZ,
    /// Motorola 68040.
    M68k,
    /// MOS 6502.
    Mos6502,
    /// WDC 65C02.
    Mos65c02,
    /// WDC 65816.
    W65816,
    /// Classic BPF.
    Bpf,
    /// Extended BPF.
    Ebpf,
    /// Hitachi `SuperH` SH-4A.
    Sh4,
    /// Infineon `TriCore` 1.6.2.
    TriCore,
    /// Ethereum virtual machine bytecode.
    Evm,
    /// Motorola 6809.
    M6809,
    /// XMOS `XCore`.
    Xcore,
    /// TI `TMS320C64x` DSP.
    Tms320c64x,
    /// WebAssembly bytecode (built-in decoder; Capstone bindings do not
    /// expose it).
    Wasm,
}

/// Byte order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Endian {
    /// Least significant byte first.
    Little,
    /// Most significant byte first.
    Big,
}

/// Static description of an architecture for UI pickers.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchInfo {
    /// The architecture.
    pub arch: Arch,
    /// Human readable label.
    pub label: &'static str,
    /// Whether [`crate::asm::assemble`] supports it.
    pub can_assemble: bool,
    /// Whether [`crate::patch::nop_fill`] knows a NOP for it.
    pub has_nop: bool,
}

impl Arch {
    /// Every supported architecture.
    pub const ALL: [Arch; 31] = [
        Arch::X86Real,
        Arch::X86,
        Arch::X64,
        Arch::Arm,
        Arch::Thumb,
        Arch::Arm64,
        Arch::Mips32,
        Arch::Mips32Be,
        Arch::Mips64,
        Arch::Mips64Be,
        Arch::Ppc32,
        Arch::Ppc64,
        Arch::Ppc64Le,
        Arch::RiscV32,
        Arch::RiscV64,
        Arch::Sparc,
        Arch::SparcV9,
        Arch::SysZ,
        Arch::M68k,
        Arch::Mos6502,
        Arch::Mos65c02,
        Arch::W65816,
        Arch::Bpf,
        Arch::Ebpf,
        Arch::Sh4,
        Arch::TriCore,
        Arch::Evm,
        Arch::M6809,
        Arch::Xcore,
        Arch::Tms320c64x,
        Arch::Wasm,
    ];

    /// Human readable label.
    pub fn label(self) -> &'static str {
        match self {
            Arch::X86Real => "x86 (16-bit)",
            Arch::X86 => "x86 (32-bit)",
            Arch::X64 => "x86-64",
            Arch::Arm => "ARM (A32)",
            Arch::Thumb => "ARM (Thumb)",
            Arch::Arm64 => "ARM64 (AArch64)",
            Arch::Mips32 => "MIPS32 LE",
            Arch::Mips32Be => "MIPS32 BE",
            Arch::Mips64 => "MIPS64 LE",
            Arch::Mips64Be => "MIPS64 BE",
            Arch::Ppc32 => "PowerPC 32 BE",
            Arch::Ppc64 => "PowerPC 64 BE",
            Arch::Ppc64Le => "PowerPC 64 LE",
            Arch::RiscV32 => "RISC-V 32",
            Arch::RiscV64 => "RISC-V 64",
            Arch::Sparc => "SPARC",
            Arch::SparcV9 => "SPARC V9",
            Arch::SysZ => "SystemZ",
            Arch::M68k => "Motorola 68k",
            Arch::Mos6502 => "MOS 6502",
            Arch::Mos65c02 => "WDC 65C02",
            Arch::W65816 => "WDC 65816",
            Arch::Bpf => "BPF (classic)",
            Arch::Ebpf => "eBPF",
            Arch::Sh4 => "SuperH SH-4A",
            Arch::TriCore => "TriCore",
            Arch::Evm => "EVM",
            Arch::M6809 => "Motorola 6809",
            Arch::Xcore => "XCore",
            Arch::Tms320c64x => "TMS320C64x",
            Arch::Wasm => "WebAssembly",
        }
    }

    /// Byte order of instruction words.
    pub fn endian(self) -> Endian {
        match self {
            Arch::Mips32Be
            | Arch::Mips64Be
            | Arch::Ppc32
            | Arch::Ppc64
            | Arch::Sparc
            | Arch::SparcV9
            | Arch::SysZ
            | Arch::M68k
            | Arch::Evm
            | Arch::M6809
            | Arch::Xcore
            | Arch::Tms320c64x => Endian::Big,
            _ => Endian::Little,
        }
    }

    /// Pointer width in bytes.
    pub fn pointer_size(self) -> u8 {
        match self {
            Arch::X86Real | Arch::Mos6502 | Arch::Mos65c02 | Arch::W65816 | Arch::M6809 => 2,
            Arch::X64
            | Arch::Arm64
            | Arch::Mips64
            | Arch::Mips64Be
            | Arch::Ppc64
            | Arch::Ppc64Le
            | Arch::RiscV64
            | Arch::SparcV9
            | Arch::SysZ
            | Arch::Ebpf => 8,
            _ => 4,
        }
    }

    /// Whether this is one of the x86 modes.
    pub fn is_x86(self) -> bool {
        matches!(self, Arch::X86Real | Arch::X86 | Arch::X64)
    }

    /// Bitness of an x86 mode (16, 32, 64); `None` for other architectures.
    pub fn x86_bitness(self) -> Option<u32> {
        match self {
            Arch::X86Real => Some(16),
            Arch::X86 => Some(32),
            Arch::X64 => Some(64),
            _ => None,
        }
    }

    /// Whether [`crate::asm::assemble`] supports the architecture.
    pub fn can_assemble(self) -> bool {
        self.is_x86() || self == Arch::Arm64
    }

    /// Descriptions of every architecture, for UI pickers.
    pub fn catalog() -> Vec<ArchInfo> {
        Arch::ALL
            .iter()
            .map(|&arch| ArchInfo {
                arch,
                label: arch.label(),
                can_assemble: arch.can_assemble(),
                has_nop: crate::patch::nop_bytes(arch).is_some(),
            })
            .collect()
    }
}

impl std::fmt::Display for Arch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_names() {
        assert_eq!(serde_json::to_string(&Arch::X64).unwrap(), "\"x86_64\"");
        assert_eq!(serde_json::to_string(&Arch::X86Real).unwrap(), "\"x86_16\"");
        assert_eq!(
            serde_json::to_string(&Arch::Mips32Be).unwrap(),
            "\"mips32Be\""
        );
        let a: Arch = serde_json::from_str("\"arm64\"").unwrap();
        assert_eq!(a, Arch::Arm64);
    }

    #[test]
    fn catalog_complete() {
        let c = Arch::catalog();
        assert_eq!(c.len(), Arch::ALL.len());
        assert!(c.iter().any(|i| i.arch == Arch::X64 && i.can_assemble));
        assert_eq!(Arch::Ppc32.endian(), Endian::Big);
        assert_eq!(Arch::X64.pointer_size(), 8);
    }
}
