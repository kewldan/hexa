//! Assembler (ANL-14), pure Rust.
//!
//! Keystone was evaluated first: its vendored `CMake` project no longer
//! configures with `CMake` 4 (`cmake_minimum_required` < 3.5) and it is
//! GPL-2.0 licensed, incompatible with this MIT workspace. The assembler is
//! therefore implemented here:
//!
//! * **x86 / x86-64 / 16-bit** ([`Arch::X86Real`], [`Arch::X86`],
//!   [`Arch::X64`]): an Intel-syntax parser feeding the `iced-x86` encoder.
//!   Any instruction `iced-x86` can encode (legacy, VEX, EVEX) is accepted as
//!   long as its operands are registers, memory references
//!   (`[base + index*scale + disp]`, `seg:[...]`, `[rip + disp]`, size
//!   keywords `byte/word/dword/qword/tword/xmmword/ymmword/zmmword [ptr]`),
//!   immediates or branch targets. Prefixes `lock`, `rep`, `repe/repz`,
//!   `repne/repnz` and string instructions (`movsb` ... `scasq`) are
//!   supported. Far pointers, `{k}` masks / `{sae}` decorators and
//!   label arithmetic are not.
//! * **ARM64** ([`Arch::Arm64`]): a hand-written encoder for the common
//!   subset listed in [`arm64`].
//!
//! Several instructions can be separated by `;` or newlines; each one is
//! encoded at the address following the previous one.

pub mod arm64;
pub mod x86;

use crate::arch::Arch;
use crate::error::{ReError, Result};

/// Assembles `source` for `arch` at virtual address `address` and returns
/// the machine code.
pub fn assemble(arch: Arch, source: &str, address: u64) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for line in split_statements(source) {
        let at = address.wrapping_add(out.len() as u64);
        let bytes = match arch {
            Arch::X86Real | Arch::X86 | Arch::X64 => {
                x86::assemble_one(arch.x86_bitness().unwrap_or(64), line, at)?
            }
            Arch::Arm64 => arm64::assemble_one(line, at)?.to_le_bytes().to_vec(),
            _ => {
                return Err(ReError::Unsupported {
                    what: "assembly",
                    arch: arch.label().into(),
                });
            }
        };
        out.extend(bytes);
    }
    if out.is_empty() {
        return Err(ReError::Asm("empty input".into()));
    }
    Ok(out)
}

/// Splits on `;` / newlines and strips `#`/`//` comments, ignoring empty
/// statements.
fn split_statements(source: &str) -> impl Iterator<Item = &str> {
    source
        .lines()
        .map(|l| {
            let l = l.split("//").next().unwrap_or("");
            l.trim()
        })
        .flat_map(|l| l.split(';'))
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Parses an integer literal: decimal, `0x` hex, `0b` binary, `0o` octal,
/// `h`-suffixed hex, optional sign.
pub(crate) fn parse_int(s: &str) -> Option<i128> {
    let s = s.trim().replace('_', "");
    let (neg, body) = if let Some(r) = s.strip_prefix('-') {
        (true, r.trim())
    } else if let Some(r) = s.strip_prefix('+') {
        (false, r.trim())
    } else {
        (false, s.as_str())
    };
    let lower = body.to_ascii_lowercase();
    let v = if let Some(h) = lower.strip_prefix("0x") {
        i128::from_str_radix(h, 16).ok()?
    } else if let Some(b) = lower.strip_prefix("0b") {
        i128::from_str_radix(b, 2).ok()?
    } else if let Some(o) = lower.strip_prefix("0o") {
        i128::from_str_radix(o, 8).ok()?
    } else if let Some(h) = lower.strip_suffix('h') {
        if !h.starts_with(|c: char| c.is_ascii_digit()) {
            return None;
        }
        i128::from_str_radix(h, 16).ok()?
    } else {
        lower.parse::<i128>().ok()?
    };
    Some(if neg { -v } else { v })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals() {
        assert_eq!(parse_int("0x10"), Some(16));
        assert_eq!(parse_int("-8"), Some(-8));
        assert_eq!(parse_int("0ffh"), Some(255));
        assert_eq!(parse_int("0b101"), Some(5));
        assert_eq!(parse_int("eax"), None);
        assert_eq!(parse_int("ah"), None);
    }

    #[test]
    fn multiple_statements() {
        let b = assemble(Arch::X64, "nop; ret\n int3 // trap", 0).unwrap();
        assert_eq!(b, vec![0x90, 0xc3, 0xcc]);
        assert!(assemble(Arch::Mips32, "nop", 0).is_err());
        assert!(assemble(Arch::X64, "  ", 0).is_err());
    }
}
