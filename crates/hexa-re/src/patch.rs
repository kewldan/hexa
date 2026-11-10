//! Instruction patching (RE-03) and patch lists with export/import and
//! porting to other file versions (RE-05).

use hexa_core::Tree;
use hexa_core::piece::Piece;
use serde::{Deserialize, Serialize};

use crate::arch::{Arch, Endian};
use crate::asm::assemble;
use crate::disasm::{DisasmOptions, Instruction, disassemble};
use crate::error::{ReError, Result};

// ---------------------------------------------------------------------------
// NOPs
// ---------------------------------------------------------------------------

/// The canonical single NOP instruction of an architecture, or `None` when
/// the architecture has no side-effect free instruction (EVM, `XCore`).
pub fn nop_bytes(arch: Arch) -> Option<&'static [u8]> {
    Some(match arch {
        Arch::X86Real | Arch::X86 | Arch::X64 => &[0x90],
        Arch::Arm => &[0x00, 0xf0, 0x20, 0xe3],
        Arch::Thumb => &[0x00, 0xbf],
        Arch::Arm64 => &[0x1f, 0x20, 0x03, 0xd5],
        Arch::Mips32 | Arch::Mips32Be | Arch::Mips64 | Arch::Mips64Be | Arch::Tms320c64x => {
            &[0, 0, 0, 0]
        }
        Arch::Ppc32 | Arch::Ppc64 => &[0x60, 0, 0, 0],
        Arch::Ppc64Le => &[0, 0, 0, 0x60],
        Arch::RiscV32 | Arch::RiscV64 => &[0x13, 0, 0, 0],
        Arch::Sparc | Arch::SparcV9 => &[0x01, 0, 0, 0],
        Arch::SysZ => &[0x07, 0x00],
        Arch::M68k => &[0x4e, 0x71],
        Arch::Mos6502 | Arch::Mos65c02 | Arch::W65816 => &[0xea],
        Arch::Bpf | Arch::Ebpf => &[0x05, 0, 0, 0, 0, 0, 0, 0],
        Arch::Sh4 => &[0x09, 0x00],
        Arch::TriCore => &[0x00, 0x00],
        Arch::M6809 => &[0x12],
        Arch::Wasm => &[0x01],
        Arch::Evm | Arch::Xcore => return None,
    })
}

/// Recommended multi-byte x86 NOPs (Intel SDM, `nop dword ptr [...]`).
const X86_NOPS: [&[u8]; 9] = [
    &[0x90],
    &[0x66, 0x90],
    &[0x0f, 0x1f, 0x00],
    &[0x0f, 0x1f, 0x40, 0x00],
    &[0x0f, 0x1f, 0x44, 0x00, 0x00],
    &[0x66, 0x0f, 0x1f, 0x44, 0x00, 0x00],
    &[0x0f, 0x1f, 0x80, 0x00, 0x00, 0x00, 0x00],
    &[0x0f, 0x1f, 0x84, 0x00, 0x00, 0x00, 0x00, 0x00],
    &[0x66, 0x0f, 0x1f, 0x84, 0x00, 0x00, 0x00, 0x00, 0x00],
];

/// Builds `len` bytes of architecture-specific NOPs (RE-03).
///
/// x86 (32/64-bit) uses the recommended multi-byte NOP forms so a gap is
/// filled with as few instructions as possible; 16-bit x86 uses `0x90`.
/// RISC-V finishes an odd half-word with `c.nop`. Other architectures need
/// `len` to be a multiple of their NOP size.
pub fn nop_fill(arch: Arch, len: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(len);
    match arch {
        Arch::X86 | Arch::X64 => {
            let mut left = len;
            while left > 0 {
                let n = left.min(X86_NOPS.len());
                out.extend_from_slice(X86_NOPS[n - 1]);
                left -= n;
            }
        }
        Arch::RiscV32 | Arch::RiscV64 => {
            if !len.is_multiple_of(2) {
                return Err(ReError::Invalid(format!(
                    "{len} bytes cannot be filled with RISC-V NOPs"
                )));
            }
            for _ in 0..len / 4 {
                out.extend_from_slice(&[0x13, 0, 0, 0]);
            }
            if len % 4 == 2 {
                out.extend_from_slice(&[0x01, 0x00]);
            }
        }
        _ => {
            let nop = nop_bytes(arch).ok_or_else(|| ReError::Unsupported {
                what: "NOP padding",
                arch: arch.label().into(),
            })?;
            if !len.is_multiple_of(nop.len()) {
                return Err(ReError::Invalid(format!(
                    "{len} bytes is not a multiple of the {}-byte NOP of {}",
                    nop.len(),
                    arch.label()
                )));
            }
            for _ in 0..len / nop.len() {
                out.extend_from_slice(nop);
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// RE-03: instruction patch planning
// ---------------------------------------------------------------------------

/// Result of planning an instruction patch (RE-03).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionPatch {
    /// Address of the patched instruction.
    pub address: u64,
    /// Bytes that will be overwritten (same length as `replacement`).
    pub original: Vec<u8>,
    /// Assembled bytes followed by NOP padding.
    pub replacement: Vec<u8>,
    /// Length of the assembled code alone.
    pub assembled_len: u32,
    /// Length of the instruction originally at `address`.
    pub original_instruction_len: u32,
    /// Number of NOP padding bytes appended.
    pub padding: u32,
    /// Every original instruction touched by the patch, first one included.
    pub overwritten: Vec<Instruction>,
    /// How many instructions *after* the first one are overwritten.
    pub extra_overwritten: u32,
    /// Human readable warning when the patch is longer than the original
    /// instruction.
    pub warning: Option<String>,
}

impl InstructionPatch {
    /// Converts the plan into a [`Patch`] at file offset `offset`.
    pub fn to_patch(&self, offset: u64, comment: impl Into<String>) -> Patch {
        Patch {
            offset,
            original: self.original.clone(),
            replacement: self.replacement.clone(),
            comment: comment.into(),
        }
    }
}

/// Assembles `source` at `address` and plans how it replaces the code in
/// `code` (the bytes starting at `address`, at least as long as the new
/// code plus one instruction is recommended).
pub fn plan_instruction_patch(
    arch: Arch,
    code: &[u8],
    address: u64,
    source: &str,
) -> Result<InstructionPatch> {
    let assembled = assemble(arch, source, address)?;
    plan_bytes_patch(arch, code, address, &assembled)
}

/// Like [`plan_instruction_patch`] but with already encoded bytes.
pub fn plan_bytes_patch(
    arch: Arch,
    code: &[u8],
    address: u64,
    assembled: &[u8],
) -> Result<InstructionPatch> {
    if assembled.is_empty() {
        return Err(ReError::Invalid("empty replacement".into()));
    }
    let insns = disassemble(arch, code, address, &DisasmOptions::default())?;
    let first = insns
        .first()
        .filter(|i| !i.is_data)
        .ok_or_else(|| ReError::Disasm(format!("no valid instruction at {address:#x}")))?;
    let first_len = first.size;
    let mut covered = 0usize;
    let mut overwritten = Vec::new();
    for insn in &insns {
        if covered >= assembled.len() {
            break;
        }
        covered += insn.size as usize;
        overwritten.push(insn.clone());
    }
    if covered < assembled.len() {
        return Err(ReError::Invalid(format!(
            "replacement is {} bytes but only {covered} bytes of code are available",
            assembled.len()
        )));
    }
    let padding = covered - assembled.len();
    let mut replacement = assembled.to_vec();
    replacement.extend(nop_fill(arch, padding)?);
    let extra = overwritten.len().saturating_sub(1) as u32;
    let warning = (assembled.len() > first_len as usize).then(|| {
        format!(
            "new code is {} bytes, longer than the original {}-byte instruction; \
             {extra} following instruction(s) will be overwritten",
            assembled.len(),
            first_len
        )
    });
    Ok(InstructionPatch {
        address,
        original: code[..covered].to_vec(),
        replacement,
        assembled_len: assembled.len() as u32,
        original_instruction_len: first_len,
        padding: padding as u32,
        overwritten,
        extra_overwritten: extra,
        warning,
    })
}

/// [`plan_instruction_patch`] reading the code from a tree at file offset
/// `offset` (the instruction's address is `address`).
pub fn plan_instruction_patch_tree(
    arch: Arch,
    tree: &Tree,
    offset: u64,
    address: u64,
    source: &str,
) -> Result<InstructionPatch> {
    let assembled = assemble(arch, source, address)?;
    let window = (assembled.len() + 64).max(64);
    let code = tree.read_vec(offset, window)?;
    plan_bytes_patch(arch, &code, address, &assembled)
}

// ---------------------------------------------------------------------------
// RE-05: patch lists
// ---------------------------------------------------------------------------

mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&crate::util::hex(v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        crate::util::parse_hex(&s).ok_or_else(|| serde::de::Error::custom("invalid hex string"))
    }
}

/// One byte-level patch (RE-05). `original` and `replacement` have the
/// same length; bytes are serialised as hex strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Patch {
    /// File offset.
    pub offset: u64,
    /// Bytes expected before patching.
    #[serde(with = "hex_bytes")]
    pub original: Vec<u8>,
    /// Bytes written by the patch.
    #[serde(with = "hex_bytes")]
    pub replacement: Vec<u8>,
    /// Free-form note.
    #[serde(default)]
    pub comment: String,
}

impl Patch {
    /// Exclusive end offset.
    pub fn end(&self) -> u64 {
        self.offset + self.original.len() as u64
    }
}

/// State of a patch against some data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PatchState {
    /// The original bytes are present.
    Original,
    /// The replacement bytes are present.
    Applied,
    /// Neither the original nor the replacement bytes are present.
    Mismatch,
    /// The patch lies (partly) outside the data.
    OutOfRange,
}

/// A serialisable list of patches (RE-05).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchList {
    /// Format version (currently 1).
    #[serde(default = "one")]
    pub version: u32,
    /// Optional name of the file the patches were made for.
    #[serde(default)]
    pub target: Option<String>,
    /// Optional size of that file.
    #[serde(default)]
    pub target_size: Option<u64>,
    /// The patches.
    pub patches: Vec<Patch>,
}

fn one() -> u32 {
    1
}

fn state_of(data: &[u8], p: &Patch) -> PatchState {
    let Ok(start) = usize::try_from(p.offset) else {
        return PatchState::OutOfRange;
    };
    let Some(cur) = data.get(start..start + p.original.len()) else {
        return PatchState::OutOfRange;
    };
    if cur == p.original.as_slice() {
        PatchState::Original
    } else if cur == p.replacement.as_slice() {
        PatchState::Applied
    } else {
        PatchState::Mismatch
    }
}

impl PatchList {
    /// Creates a list.
    pub fn new(patches: Vec<Patch>) -> Self {
        PatchList {
            version: 1,
            target: None,
            target_size: None,
            patches,
        }
    }

    /// Serialises to pretty JSON.
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Parses JSON and validates the result.
    pub fn from_json(json: &str) -> Result<Self> {
        let list: PatchList = serde_json::from_str(json)?;
        list.validate()?;
        Ok(list)
    }

    /// Checks lengths and that no two patches overlap.
    pub fn validate(&self) -> Result<()> {
        let mut spans: Vec<(u64, u64)> = Vec::new();
        for (i, p) in self.patches.iter().enumerate() {
            if p.original.len() != p.replacement.len() {
                return Err(ReError::Invalid(format!(
                    "patch {i}: original and replacement lengths differ"
                )));
            }
            if p.original.is_empty() {
                return Err(ReError::Invalid(format!("patch {i} is empty")));
            }
            spans.push((p.offset, p.end()));
        }
        spans.sort_unstable();
        if spans.windows(2).any(|w| w[1].0 < w[0].1) {
            return Err(ReError::Invalid("patches overlap".into()));
        }
        Ok(())
    }

    /// State of every patch against `data`.
    pub fn states(&self, data: &[u8]) -> Vec<PatchState> {
        self.patches.iter().map(|p| state_of(data, p)).collect()
    }

    /// State of every patch against a tree.
    pub fn states_tree(&self, tree: &Tree) -> Result<Vec<PatchState>> {
        self.patches
            .iter()
            .map(|p| {
                if p.end() > tree.len() {
                    return Ok(PatchState::OutOfRange);
                }
                let cur = tree.read_vec(p.offset, p.original.len())?;
                Ok(state_of(
                    &cur,
                    &Patch {
                        offset: 0,
                        ..p.clone()
                    },
                ))
            })
            .collect()
    }

    /// Applies every patch in place. Already-applied patches are skipped;
    /// a mismatch aborts before anything is written unless `force`.
    pub fn apply(&self, data: &mut [u8], force: bool) -> Result<()> {
        self.validate()?;
        let states = self.states(data);
        for (i, s) in states.iter().enumerate() {
            match s {
                PatchState::OutOfRange => {
                    return Err(ReError::Invalid(format!("patch {i} is out of range")));
                }
                PatchState::Mismatch if !force => {
                    return Err(ReError::Invalid(format!(
                        "patch {i}: bytes at {:#x} do not match",
                        self.patches[i].offset
                    )));
                }
                _ => {}
            }
        }
        for p in &self.patches {
            let s = p.offset as usize;
            data[s..s + p.replacement.len()].copy_from_slice(&p.replacement);
        }
        Ok(())
    }

    /// Restores the original bytes of applied patches.
    pub fn revert(&self, data: &mut [u8]) -> Result<()> {
        self.validate()?;
        for (i, p) in self.patches.iter().enumerate() {
            match state_of(data, p) {
                PatchState::Applied | PatchState::Original => {
                    let s = p.offset as usize;
                    data[s..s + p.original.len()].copy_from_slice(&p.original);
                }
                other => {
                    return Err(ReError::Invalid(format!(
                        "patch {i} cannot be reverted: {other:?}"
                    )));
                }
            }
        }
        Ok(())
    }

    /// Returns a new tree with every patch applied (same rules as
    /// [`PatchList::apply`]).
    pub fn apply_to_tree(&self, tree: &Tree, force: bool) -> Result<Tree> {
        self.validate()?;
        let states = self.states_tree(tree)?;
        for (i, s) in states.iter().enumerate() {
            if *s == PatchState::OutOfRange || (*s == PatchState::Mismatch && !force) {
                return Err(ReError::Invalid(format!(
                    "patch {i} cannot be applied: {s:?}"
                )));
            }
        }
        let mut out = tree.clone();
        for p in &self.patches {
            let piece = Tree::from_piece(Piece::bytes(p.replacement.clone()));
            out = out.replace(p.offset, p.end(), &piece);
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// RE-05: porting patches to another file version
// ---------------------------------------------------------------------------

/// Options for [`port_patches`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortOptions {
    /// Largest context (bytes on each side) tried before giving up.
    pub max_context: usize,
    /// Maximum candidate offsets reported for ambiguous patches.
    pub max_candidates: usize,
}

impl Default for PortOptions {
    fn default() -> Self {
        PortOptions {
            max_context: 4096,
            max_candidates: 16,
        }
    }
}

/// Outcome of porting one patch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PortStatus {
    /// A unique location with the original bytes was found.
    Ported,
    /// The new file already contains the replacement at a unique location.
    AlreadyApplied,
    /// Several locations match even with the largest usable context.
    Ambiguous,
    /// No location matches.
    Missing,
}

/// Port result for one patch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortResult {
    /// Index in the input list.
    pub index: usize,
    /// Offset in the old file.
    pub old_offset: u64,
    /// Outcome.
    pub status: PortStatus,
    /// Offset in the new file for `Ported` / `AlreadyApplied`.
    pub new_offset: Option<u64>,
    /// Candidate offsets for `Ambiguous`.
    pub candidates: Vec<u64>,
    /// Context bytes used before the patch in the unique signature.
    pub context_before: u32,
    /// Context bytes used after the patch in the unique signature.
    pub context_after: u32,
}

/// Report of [`port_patches`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortReport {
    /// One entry per input patch.
    pub results: Vec<PortResult>,
    /// The successfully ported (not yet applied) patches, at new offsets.
    pub ported: PatchList,
}

enum Located {
    Unique(u64, u32, u32),
    Many(Vec<u64>),
    None,
}

fn occurrences(hay: &[u8], needle: &[u8], cap: usize) -> Vec<usize> {
    memchr::memmem::find_iter(hay, needle).take(cap).collect()
}

/// Finds a unique occurrence of `sig(s, e)` in `new`, growing the context
/// window around `[off, off+len)` until it is unique.
fn locate(
    old_len: usize,
    off: usize,
    len: usize,
    new: &[u8],
    sig: &dyn Fn(usize, usize) -> Vec<u8>,
    opts: &PortOptions,
) -> Located {
    let cap = opts.max_candidates.max(2);
    let mut ctx = 0usize;
    let mut last_many: Vec<u64> = Vec::new();
    loop {
        let s = off.saturating_sub(ctx);
        let e = (off + len + ctx).min(old_len);
        let hits = occurrences(new, &sig(s, e), cap);
        match hits.len() {
            1 => {
                return Located::Unique(
                    (hits[0] + (off - s)) as u64,
                    (off - s) as u32,
                    (e - off - len) as u32,
                );
            }
            0 => {
                if ctx == 0 {
                    return Located::None;
                }
                // Context differs on one side: try each side alone.
                for (s2, e2) in [(s, off + len), (off, e)] {
                    let h = occurrences(new, &sig(s2, e2), cap);
                    if h.len() == 1 {
                        return Located::Unique(
                            (h[0] + (off - s2)) as u64,
                            (off - s2) as u32,
                            (e2 - off - len) as u32,
                        );
                    }
                }
                return if last_many.is_empty() {
                    Located::None
                } else {
                    Located::Many(last_many)
                };
            }
            _ => {
                last_many = hits.iter().map(|h| (h + (off - s)) as u64).collect();
                let exhausted = s == 0 && e == old_len;
                if exhausted || ctx >= opts.max_context {
                    return Located::Many(last_many);
                }
                ctx = if ctx == 0 {
                    4
                } else {
                    (ctx * 2).min(opts.max_context)
                };
            }
        }
    }
}

/// Ports `patches` made against `old` to the new file version `new` by
/// matching a byte signature around each patch, growing the context until
/// the signature is unique (RE-05).
pub fn port_patches(old: &[u8], new: &[u8], patches: &PatchList, opts: &PortOptions) -> PortReport {
    let mut results = Vec::new();
    let mut ported = Vec::new();
    for (index, p) in patches.patches.iter().enumerate() {
        let mut res = PortResult {
            index,
            old_offset: p.offset,
            status: PortStatus::Missing,
            new_offset: None,
            candidates: Vec::new(),
            context_before: 0,
            context_after: 0,
        };
        let off = p.offset as usize;
        let len = p.original.len();
        let in_old = off.checked_add(len).is_some_and(|e| e <= old.len());
        if in_old && len > 0 {
            let orig_sig = |s: usize, e: usize| {
                let mut v = old[s..e].to_vec();
                v[off - s..off - s + len].copy_from_slice(&p.original);
                v
            };
            let patched_sig = |s: usize, e: usize| {
                let mut v = old[s..e].to_vec();
                v[off - s..off - s + len].copy_from_slice(&p.replacement);
                v
            };
            match locate(old.len(), off, len, new, &orig_sig, opts) {
                Located::Unique(at, b, a) => {
                    res.status = PortStatus::Ported;
                    res.new_offset = Some(at);
                    res.context_before = b;
                    res.context_after = a;
                    ported.push(Patch {
                        offset: at,
                        ..p.clone()
                    });
                }
                Located::Many(c) => {
                    if let Located::Unique(at, b, a) =
                        locate(old.len(), off, len, new, &patched_sig, opts)
                    {
                        res.status = PortStatus::AlreadyApplied;
                        res.new_offset = Some(at);
                        res.context_before = b;
                        res.context_after = a;
                    } else {
                        res.status = PortStatus::Ambiguous;
                        res.candidates = c;
                    }
                }
                Located::None => {
                    if let Located::Unique(at, b, a) =
                        locate(old.len(), off, len, new, &patched_sig, opts)
                    {
                        res.status = PortStatus::AlreadyApplied;
                        res.new_offset = Some(at);
                        res.context_before = b;
                        res.context_after = a;
                    }
                }
            }
        }
        results.push(res);
    }
    PortReport {
        results,
        ported: PatchList {
            version: 1,
            target: None,
            target_size: Some(new.len() as u64),
            patches: ported,
        },
    }
}

/// [`port_patches`] over two trees.
pub fn port_patches_tree(
    old: &Tree,
    new: &Tree,
    patches: &PatchList,
    opts: &PortOptions,
) -> Result<PortReport> {
    let old = crate::tree_bytes(old)?;
    let new = crate::tree_bytes(new)?;
    Ok(port_patches(&old, &new, patches, opts))
}

/// Encodes an integer as bytes of the given width and byte order (helper
/// for checksum and pointer patches).
pub fn encode_uint(value: u64, width: usize, endian: Endian) -> Vec<u8> {
    let bytes = value.to_le_bytes();
    let mut v = bytes[..width.min(8)].to_vec();
    if endian == Endian::Big {
        v.reverse();
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x86_nop_fill_forms() {
        assert_eq!(nop_fill(Arch::X64, 0).unwrap(), Vec::<u8>::new());
        assert_eq!(nop_fill(Arch::X64, 1).unwrap(), vec![0x90]);
        assert_eq!(nop_fill(Arch::X64, 3).unwrap(), vec![0x0f, 0x1f, 0x00]);
        let v = nop_fill(Arch::X64, 11).unwrap();
        assert_eq!(v.len(), 11);
        // 9 + 2
        assert_eq!(&v[9..], &[0x66, 0x90]);
        // every NOP must decode as nop
        let ins = disassemble(Arch::X64, &v, 0, &DisasmOptions::default()).unwrap();
        assert!(ins.iter().all(|i| i.mnemonic == "nop"), "{ins:?}");
        assert_eq!(nop_fill(Arch::X86Real, 3).unwrap(), vec![0x90; 3]);
    }

    #[test]
    fn risc_nops() {
        let v = nop_fill(Arch::Arm64, 8).unwrap();
        let ins = disassemble(Arch::Arm64, &v, 0, &DisasmOptions::default()).unwrap();
        assert_eq!(ins.len(), 2);
        assert!(ins.iter().all(|i| i.mnemonic == "nop"));
        assert!(nop_fill(Arch::Arm64, 6).is_err());
        let v = nop_fill(Arch::RiscV64, 6).unwrap();
        let ins = disassemble(Arch::RiscV64, &v, 0, &DisasmOptions::default()).unwrap();
        assert_eq!(ins.len(), 2);
        assert!(ins.iter().all(|i| i.mnemonic.contains("nop")), "{ins:?}");
        for arch in Arch::ALL {
            if let Some(n) = nop_bytes(arch) {
                let ins = disassemble(arch, n, 0, &DisasmOptions::default()).unwrap();
                assert!(!ins[0].is_data, "{arch:?}");
            }
        }
        assert!(nop_fill(Arch::Evm, 1).is_err());
    }

    #[test]
    fn plan_shorter_patch_pads() {
        // mov eax, 0x12345678 ; ret  -> replace mov with "xor eax, eax"
        let code = [0xb8, 0x78, 0x56, 0x34, 0x12, 0xc3];
        let p = plan_instruction_patch(Arch::X64, &code, 0x1000, "xor eax, eax").unwrap();
        assert_eq!(p.assembled_len, 2);
        assert_eq!(p.original_instruction_len, 5);
        assert_eq!(p.padding, 3);
        assert_eq!(p.replacement, vec![0x31, 0xc0, 0x0f, 0x1f, 0x00]);
        assert_eq!(p.original, code[..5].to_vec());
        assert_eq!(p.extra_overwritten, 0);
        assert!(p.warning.is_none());
    }

    #[test]
    fn plan_longer_patch_warns() {
        // nop; nop; push rbp; ret -> 5-byte call overwrites 3 instructions
        let code = [0x90, 0x90, 0x55, 0xc3, 0xcc];
        let p = plan_instruction_patch(Arch::X64, &code, 0x1000, "call 0x2000").unwrap();
        assert_eq!(p.assembled_len, 5);
        assert_eq!(p.overwritten.len(), 5);
        assert_eq!(p.extra_overwritten, 4);
        assert_eq!(p.padding, 0);
        assert!(p.warning.as_deref().unwrap().contains("4 following"));
        let p = plan_instruction_patch(Arch::X64, &[0x90, 0x48, 0x89, 0xe5], 0, "push 1").unwrap();
        assert_eq!(p.replacement, vec![0x6a, 0x01, 0x66, 0x90]);
        assert_eq!(p.extra_overwritten, 1);
        assert!(plan_instruction_patch(Arch::X64, &[0x90], 0, "call 0x100").is_err());
    }

    #[test]
    fn plan_arm64_and_tree() {
        let code: Vec<u8> = [0xd65f_03c0u32, 0xd503_201f]
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .collect();
        let p = plan_instruction_patch(Arch::Arm64, &code, 0x4000, "mov x0, #1").unwrap();
        assert_eq!(p.replacement, 0xd280_0020u32.to_le_bytes().to_vec());
        let tree = Tree::from_piece(Piece::bytes(code.clone()));
        let p2 = plan_instruction_patch_tree(Arch::Arm64, &tree, 0, 0x4000, "mov x0, #1").unwrap();
        assert_eq!(p, p2);
        let patch = p.to_patch(0, "return 1");
        assert_eq!(patch.comment, "return 1");
    }

    fn sample_list() -> PatchList {
        PatchList::new(vec![
            Patch {
                offset: 2,
                original: vec![0x74, 0x05],
                replacement: vec![0xeb, 0x05],
                comment: "always jump".into(),
            },
            Patch {
                offset: 8,
                original: vec![0xc3],
                replacement: vec![0x90],
                comment: String::new(),
            },
        ])
    }

    #[test]
    fn json_roundtrip() {
        let list = sample_list();
        let json = list.to_json().unwrap();
        assert!(json.contains("\"original\": \"7405\""));
        assert!(json.contains("\"targetSize\""));
        let back = PatchList::from_json(&json).unwrap();
        assert_eq!(back, list);
        let minimal = r#"{"patches":[{"offset":1,"original":"AA BB","replacement":"0xcc 0xdd"}]}"#;
        let l = PatchList::from_json(minimal).unwrap();
        assert_eq!(l.version, 1);
        assert_eq!(l.patches[0].replacement, vec![0xcc, 0xdd]);
        let bad = r#"{"patches":[{"offset":1,"original":"AA","replacement":"ccdd"}]}"#;
        assert!(PatchList::from_json(bad).is_err());
        let overlap = r#"{"patches":[{"offset":1,"original":"AABB","replacement":"ccdd"},
            {"offset":2,"original":"AA","replacement":"cc"}]}"#;
        assert!(PatchList::from_json(overlap).is_err());
    }

    #[test]
    fn apply_revert_and_tree() {
        let list = sample_list();
        let orig = vec![0x55, 0x90, 0x74, 0x05, 0, 0, 0, 0, 0xc3, 0xcc];
        let mut data = orig.clone();
        assert_eq!(list.states(&data), vec![PatchState::Original; 2]);
        list.apply(&mut data, false).unwrap();
        assert_eq!(list.states(&data), vec![PatchState::Applied; 2]);
        assert_eq!(data[2], 0xeb);
        // idempotent
        list.apply(&mut data, false).unwrap();
        list.revert(&mut data).unwrap();
        assert_eq!(data, orig);
        let tree = Tree::from_piece(Piece::bytes(orig.clone()));
        let patched = list.apply_to_tree(&tree, false).unwrap();
        assert_eq!(patched.read_vec(0, 10).unwrap()[8], 0x90);
        assert_eq!(patched.len(), 10);
        assert_eq!(
            list.states_tree(&patched).unwrap(),
            vec![PatchState::Applied; 2]
        );
        let mut bad = orig.clone();
        bad[8] = 0x11;
        assert!(list.apply(&mut bad, false).is_err());
        assert_eq!(bad[2], 0x74, "nothing written on failure");
        assert!(list.apply(&mut bad.clone(), true).is_ok());
        assert_eq!(list.states(&[0u8; 4])[1], PatchState::OutOfRange);
    }

    fn noise(n: usize, seed: u64) -> Vec<u8> {
        let mut x = seed;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect()
    }

    #[test]
    fn port_to_shifted_version() {
        let mut old = noise(4000, 1);
        // The patched bytes appear many times in the file.
        for i in (0..4000).step_by(100) {
            old[i] = 0x74;
            old[i + 1] = 0x05;
        }
        let list = PatchList::new(vec![Patch {
            offset: 1500,
            original: vec![0x74, 0x05],
            replacement: vec![0xeb, 0x05],
            comment: String::new(),
        }]);
        // New version: 37 bytes inserted at the front, some edits far away.
        let mut new = noise(37, 9);
        new.extend_from_slice(&old);
        new[3000] ^= 0xff;
        let r = port_patches(&old, &new, &list, &PortOptions::default());
        assert_eq!(r.results[0].status, PortStatus::Ported);
        assert_eq!(r.results[0].new_offset, Some(1537));
        assert!(r.results[0].context_before + r.results[0].context_after > 0);
        assert_eq!(r.ported.patches[0].offset, 1537);
        // Already applied in the new version.
        let mut applied = new.clone();
        applied[1537] = 0xeb;
        let r = port_patches(&old, &applied, &list, &PortOptions::default());
        assert_eq!(r.results[0].status, PortStatus::AlreadyApplied);
        assert_eq!(r.results[0].new_offset, Some(1537));
        assert_eq!(r.ported.patches, Vec::<Patch>::new());
    }

    #[test]
    fn port_missing_and_ambiguous() {
        let old: Vec<u8> = [1u8, 2, 3, 4, 5, 6, 7, 8].repeat(2);
        let list = PatchList::new(vec![Patch {
            offset: 3,
            original: vec![4, 5],
            replacement: vec![0, 0],
            comment: String::new(),
        }]);
        // The whole file repeats: ambiguous even at full context.
        let new: Vec<u8> = [old.clone(), old.clone()].concat();
        let r = port_patches(&old, &new, &list, &PortOptions::default());
        assert_eq!(r.results[0].status, PortStatus::Ambiguous);
        assert!(r.results[0].candidates.len() >= 2);
        let r = port_patches(&old, &[9u8; 64], &list, &PortOptions::default());
        assert_eq!(r.results[0].status, PortStatus::Missing);
        // Context on the left changed, right side still unique.
        let old = noise(512, 3);
        let list = PatchList::new(vec![Patch {
            offset: 200,
            original: old[200..202].to_vec(),
            replacement: vec![0, 0],
            comment: String::new(),
        }]);
        let mut new = old.clone();
        new[198] ^= 0x55;
        let r = port_patches(&old, &new, &list, &PortOptions::default());
        assert_eq!(r.results[0].status, PortStatus::Ported);
        assert_eq!(r.results[0].new_offset, Some(200));
        let t_old = Tree::from_piece(Piece::bytes(old.clone()));
        let t_new = Tree::from_piece(Piece::bytes(new));
        let r2 = port_patches_tree(&t_old, &t_new, &list, &PortOptions::default()).unwrap();
        assert_eq!(r2.results[0].new_offset, Some(200));
    }

    #[test]
    fn uint_encoding() {
        assert_eq!(encode_uint(0x1234, 2, Endian::Big), vec![0x12, 0x34]);
        assert_eq!(
            encode_uint(0x1234, 4, Endian::Little),
            vec![0x34, 0x12, 0, 0]
        );
    }
}
