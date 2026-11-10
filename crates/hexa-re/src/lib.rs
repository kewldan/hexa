//! Reverse engineering for Hexa: executables, disassembly, assembly,
//! demangling and patches.
//!
//! | Spec | Module |
//! |------|--------|
//! | RE-01 | [`exe`] (PE, ELF, Mach-O, fat/universal) |
//! | RE-02 | [`exe::AddressSpace`], [`exe::ExecutableInfo::va_to_offset`] |
//! | ANL-12 | [`disasm`] (Capstone + built-in WebAssembly decoder) |
//! | ANL-13 | [`disasm::Instruction::branch_target`], [`xref::branch_targets`] |
//! | ANL-14 | [`asm`] (pure Rust: x86 family via `iced-x86`, ARM64 subset) |
//! | RE-03 | [`patch::plan_instruction_patch`], [`patch::nop_fill`] |
//! | RE-04, SRCH-12 | [`xref`] |
//! | RE-05 | [`patch::PatchList`], [`patch::port_patches`] |
//! | RE-06 | [`checksum`] |
//! | RE-07 | [`detect`] |
//! | RE-08 | [`demangle`] |
//! | RE-09 | [`tools`] |
//!
//! Every public type that reaches the UI is `serde`-serialisable with
//! `camelCase` field names.

pub mod arch;
pub mod asm;
pub mod checksum;
pub mod demangle;
pub mod detect;
pub mod disasm;
pub mod error;
pub mod exe;
pub mod patch;
pub mod tools;
pub mod xref;

mod util;

pub use arch::Arch;
pub use error::{ReError, Result};

/// Reads the whole content of a tree into memory.
///
/// Executable parsing, signature porting and cross-reference scans need
/// random access over the full file, so they operate on byte slices; this
/// helper turns a [`hexa_core::Tree`] snapshot into one.
pub fn tree_bytes(tree: &hexa_core::Tree) -> Result<Vec<u8>> {
    let len = usize::try_from(tree.len())
        .map_err(|_| ReError::Invalid("file does not fit in memory".into()))?;
    Ok(tree.read_vec(0, len)?)
}
