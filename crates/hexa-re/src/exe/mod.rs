//! Executable parsing (RE-01) and address conversion (RE-02).
//!
//! [`parse`] recognises PE (PE32 / PE32+), ELF (32/64, little / big endian)
//! and Mach-O (32/64 plus fat / universal containers) and returns an
//! [`ExecutableInfo`] in which every structure carries its file offset and
//! size so the UI can highlight it. Parsing is built on `goblin`; the PE
//! resource tree, the Rich header and PE base relocations are decoded here.
//!
//! [`AddressSpace`] converts between virtual addresses, RVAs and file
//! offsets ("goto virtual address").

mod elf;
mod macho;
mod pe;

use hexa_core::Tree;
use serde::{Deserialize, Serialize};

use crate::arch::{Arch, Endian};
use crate::error::{ReError, Result};

pub use pe::{rich_product_name, rich_visual_studio};

/// Maximum number of symbols collected per image.
pub const MAX_SYMBOLS: usize = 200_000;

/// Executable container format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExecutableFormat {
    /// Portable Executable (Windows, UEFI).
    Pe,
    /// Executable and Linkable Format.
    Elf,
    /// Mach-O single-architecture image.
    MachO,
    /// Mach-O fat / universal container.
    MachOFat,
}

/// A named byte range of the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Region {
    /// Description (e.g. "COFF header", "`LC_SEGMENT_64`").
    pub name: String,
    /// File offset.
    pub offset: u64,
    /// Size in bytes.
    pub size: u64,
}

/// A section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Section {
    /// Section name (Mach-O: `segment,section`).
    pub name: String,
    /// Virtual address (absolute, image base included).
    pub virtual_address: u64,
    /// Size in memory.
    pub virtual_size: u64,
    /// File offset of the section data.
    pub offset: u64,
    /// Size of the section data in the file (0 for BSS-like sections).
    pub file_size: u64,
    /// Raw format-specific flags / characteristics.
    pub flags: u64,
    /// `rwx` permission string.
    pub permissions: String,
    /// Contains executable code.
    pub executable: bool,
    /// Byte range of the section header entry.
    pub header_offset: Option<u64>,
}

/// A segment / program header / load command segment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    /// Name or program header type.
    pub name: String,
    /// Virtual address.
    pub virtual_address: u64,
    /// Size in memory.
    pub virtual_size: u64,
    /// File offset.
    pub offset: u64,
    /// Size in the file.
    pub file_size: u64,
    /// `rwx` permission string.
    pub permissions: String,
}

/// An imported symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Import {
    /// Library the symbol comes from, when the format records it.
    pub library: Option<String>,
    /// Symbol name (`#ordinal` for PE ordinal imports).
    pub name: String,
    /// Demangled name when the symbol is mangled.
    pub demangled: Option<String>,
    /// Import by ordinal.
    pub ordinal: Option<u32>,
    /// Address of the slot (IAT entry, GOT entry, bind address).
    pub address: Option<u64>,
    /// File offset of that slot.
    pub offset: Option<u64>,
}

/// An exported symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Export {
    /// Symbol name.
    pub name: String,
    /// Demangled name when the symbol is mangled.
    pub demangled: Option<String>,
    /// Virtual address (0 for forwarders / re-exports).
    pub address: u64,
    /// File offset of the exported code / data.
    pub offset: Option<u64>,
    /// Forwarder (`dll.name`) or re-export library.
    pub forwarder: Option<String>,
}

/// A symbol table entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Symbol {
    /// Raw name.
    pub name: String,
    /// Demangled name when the symbol is mangled (RE-08).
    pub demangled: Option<String>,
    /// Value / address.
    pub address: u64,
    /// Size when known.
    pub size: u64,
    /// Kind (`func`, `object`, `section` ...).
    pub kind: String,
    /// Global (external) binding.
    pub global: bool,
    /// Defined in this image.
    pub defined: bool,
    /// File offset of the symbol's data when mapped.
    pub offset: Option<u64>,
}

/// Number of relocations of one type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelocationCount {
    /// Relocation type name.
    pub kind: String,
    /// Count.
    pub count: u64,
}

/// Summary of relocation information.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelocationSummary {
    /// Total number of relocation entries.
    pub total: u64,
    /// Counts per type, most frequent first.
    pub by_type: Vec<RelocationCount>,
    /// File regions holding relocation tables.
    pub regions: Vec<Region>,
}

impl RelocationSummary {
    pub(crate) fn add(&mut self, kind: &str, n: u64) {
        self.total += n;
        if let Some(c) = self.by_type.iter_mut().find(|c| c.kind == kind) {
            c.count += n;
        } else {
            self.by_type.push(RelocationCount {
                kind: kind.to_string(),
                count: n,
            });
        }
    }

    pub(crate) fn finish(&mut self) {
        self.by_type
            .sort_by(|a, b| b.count.cmp(&a.count).then(a.kind.cmp(&b.kind)));
    }
}

/// Data leaf of the PE resource tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceData {
    /// RVA of the data.
    pub rva: u32,
    /// File offset of the data.
    pub offset: Option<u64>,
    /// Size in bytes.
    pub size: u32,
    /// Code page.
    pub code_page: u32,
}

/// A node of the PE resource tree: level 1 = type, level 2 = name / id,
/// level 3 = language.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceNode {
    /// Display name (`ICON`, `#101`, `lang 1033`, string names).
    pub name: String,
    /// Numeric id when the entry is not named.
    pub id: Option<u32>,
    /// File offset of the directory entry (or of the root directory).
    pub offset: u64,
    /// Children (directories).
    pub children: Vec<ResourceNode>,
    /// Leaf data.
    pub data: Option<ResourceData>,
}

/// One decoded entry of the MSVC Rich header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RichEntry {
    /// Product id (tool type).
    pub product_id: u16,
    /// Tool build number.
    pub build: u16,
    /// Number of objects built with this tool.
    pub count: u32,
    /// Product name (e.g. `Utc1900_CPP`).
    pub product: String,
    /// Visual Studio release, when derivable.
    pub visual_studio: Option<String>,
}

/// The MSVC "Rich" header between the DOS stub and the PE header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RichHeader {
    /// File offset of the `DanS` marker.
    pub offset: u64,
    /// Size up to and including the XOR key.
    pub size: u64,
    /// XOR key (also a checksum over the DOS header and entries).
    pub key: u32,
    /// Whether the key matches the recomputed checksum.
    pub checksum_valid: bool,
    /// Entries.
    pub entries: Vec<RichEntry>,
}

/// One slice of a fat / universal Mach-O.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FatSlice {
    /// Architecture name.
    pub arch_name: String,
    /// CPU type.
    pub cpu_type: u32,
    /// CPU subtype.
    pub cpu_subtype: u32,
    /// File offset of the slice.
    pub offset: u64,
    /// Size of the slice.
    pub size: u64,
    /// Parsed slice (offsets are absolute in the container file).
    pub info: Option<Box<ExecutableInfo>>,
    /// Parse error, if any.
    pub error: Option<String>,
}

/// A stored checksum field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChecksumField {
    /// File offset of the field.
    pub offset: u64,
    /// Size in bytes.
    pub size: u64,
    /// Stored value.
    pub value: u32,
}

/// One file-backed range of the virtual address space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mapping {
    /// Name of the section / segment.
    pub name: String,
    /// Start virtual address.
    pub virtual_address: u64,
    /// Size in memory.
    pub virtual_size: u64,
    /// File offset.
    pub offset: u64,
    /// File-backed size.
    pub file_size: u64,
}

/// Kind of address given to [`AddressSpace::resolve`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AddressKind {
    /// Absolute virtual address.
    VirtualAddress,
    /// Relative virtual address (VA minus image base).
    Rva,
    /// File offset.
    FileOffset,
}

/// All representations of a location (RE-02).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedAddress {
    /// File offset, if file-backed.
    pub offset: Option<u64>,
    /// Virtual address, if mapped.
    pub virtual_address: Option<u64>,
    /// RVA, if mapped.
    pub rva: Option<u64>,
    /// Name of the containing mapping.
    pub mapping: Option<String>,
}

/// Virtual address space of an image (RE-02).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddressSpace {
    /// Image base used for RVAs.
    pub image_base: u64,
    /// File-backed mappings, in file order.
    pub mappings: Vec<Mapping>,
}

impl AddressSpace {
    /// Converts a virtual address to a file offset. Addresses in the
    /// zero-filled tail of a mapping (BSS) return `None`.
    pub fn va_to_offset(&self, va: u64) -> Option<u64> {
        // Later mappings win (PE sections are listed after the headers
        // mapping that may overlap them).
        self.mappings.iter().rev().find_map(|m| {
            let rel = va.checked_sub(m.virtual_address)?;
            (rel < m.file_size).then(|| m.offset + rel)
        })
    }

    /// Converts a file offset to a virtual address.
    pub fn offset_to_va(&self, offset: u64) -> Option<u64> {
        self.mappings.iter().rev().find_map(|m| {
            let rel = offset.checked_sub(m.offset)?;
            (rel < m.file_size).then(|| m.virtual_address + rel)
        })
    }

    /// Converts an RVA to a file offset.
    pub fn rva_to_offset(&self, rva: u64) -> Option<u64> {
        self.va_to_offset(self.image_base.wrapping_add(rva))
    }

    /// Converts a file offset to an RVA.
    pub fn offset_to_rva(&self, offset: u64) -> Option<u64> {
        self.offset_to_va(offset)
            .map(|va| va.wrapping_sub(self.image_base))
    }

    /// Whether a virtual address falls in any mapping (file-backed or not).
    pub fn is_mapped(&self, va: u64) -> bool {
        self.mappings.iter().any(|m| {
            va.checked_sub(m.virtual_address)
                .is_some_and(|rel| rel < m.virtual_size.max(m.file_size))
        })
    }

    /// Resolves an address of any kind into all representations.
    pub fn resolve(&self, kind: AddressKind, value: u64) -> ResolvedAddress {
        let (offset, va) = match kind {
            AddressKind::FileOffset => (Some(value), self.offset_to_va(value)),
            AddressKind::VirtualAddress => (self.va_to_offset(value), Some(value)),
            AddressKind::Rva => {
                let va = self.image_base.wrapping_add(value);
                (self.va_to_offset(va), Some(va))
            }
        };
        let va = va.filter(|v| self.is_mapped(*v));
        let mapping = va.and_then(|v| {
            self.mappings
                .iter()
                .rev()
                .find(|m| {
                    v.checked_sub(m.virtual_address)
                        .is_some_and(|rel| rel < m.virtual_size.max(m.file_size))
                })
                .map(|m| m.name.clone())
        });
        ResolvedAddress {
            offset,
            virtual_address: va,
            rva: va.map(|v| v.wrapping_sub(self.image_base)),
            mapping,
        }
    }
}

/// Parsed executable (RE-01).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutableInfo {
    /// Container format.
    pub format: ExecutableFormat,
    /// 32 or 64 (0 for fat containers).
    pub bits: u8,
    /// Byte order.
    pub endian: Endian,
    /// Machine name as stored in the header.
    pub machine: String,
    /// Disassembler architecture, when supported.
    pub arch: Option<Arch>,
    /// File kind (`executable`, `dll`, `shared object`, `object`, ...).
    pub kind: String,
    /// Entry point virtual address.
    pub entry_point: Option<u64>,
    /// Entry point file offset.
    pub entry_offset: Option<u64>,
    /// Header structures (DOS header, COFF header, program header table...).
    pub headers: Vec<Region>,
    /// Sections.
    pub sections: Vec<Section>,
    /// Segments.
    pub segments: Vec<Segment>,
    /// Imports.
    pub imports: Vec<Import>,
    /// Exports.
    pub exports: Vec<Export>,
    /// Symbols (at most [`MAX_SYMBOLS`]).
    pub symbols: Vec<Symbol>,
    /// Set when symbols were truncated.
    pub symbols_truncated: bool,
    /// Needed libraries / dylibs / DLLs.
    pub libraries: Vec<String>,
    /// Program interpreter (ELF) or dynamic linker.
    pub interpreter: Option<String>,
    /// Relocation summary.
    pub relocations: RelocationSummary,
    /// PE resource tree root.
    pub resources: Option<ResourceNode>,
    /// PE Rich header.
    pub rich_header: Option<RichHeader>,
    /// .NET CLR header (PE) region.
    pub clr_header: Option<Region>,
    /// Stored image checksum (PE only).
    pub checksum: Option<ChecksumField>,
    /// Fat / universal slices.
    pub slices: Vec<FatSlice>,
    /// Address space (RE-02).
    pub address_space: AddressSpace,
    /// Non-fatal parse problems.
    pub warnings: Vec<String>,
}

impl ExecutableInfo {
    pub(crate) fn empty(format: ExecutableFormat) -> Self {
        ExecutableInfo {
            format,
            bits: 0,
            endian: Endian::Little,
            machine: String::new(),
            arch: None,
            kind: String::new(),
            entry_point: None,
            entry_offset: None,
            headers: Vec::new(),
            sections: Vec::new(),
            segments: Vec::new(),
            imports: Vec::new(),
            exports: Vec::new(),
            symbols: Vec::new(),
            symbols_truncated: false,
            libraries: Vec::new(),
            interpreter: None,
            relocations: RelocationSummary::default(),
            resources: None,
            rich_header: None,
            clr_header: None,
            checksum: None,
            slices: Vec::new(),
            address_space: AddressSpace::default(),
            warnings: Vec::new(),
        }
    }

    /// Image base.
    pub fn image_base(&self) -> u64 {
        self.address_space.image_base
    }

    /// "Goto virtual address": VA → file offset (RE-02).
    pub fn va_to_offset(&self, va: u64) -> Option<u64> {
        self.address_space.va_to_offset(va)
    }

    /// File offset → VA (RE-02).
    pub fn offset_to_va(&self, offset: u64) -> Option<u64> {
        self.address_space.offset_to_va(offset)
    }

    /// RVA → file offset (RE-02).
    pub fn rva_to_offset(&self, rva: u64) -> Option<u64> {
        self.address_space.rva_to_offset(rva)
    }

    /// File offset → RVA (RE-02).
    pub fn offset_to_rva(&self, offset: u64) -> Option<u64> {
        self.address_space.offset_to_rva(offset)
    }

    /// Resolves an address of any kind (RE-02).
    pub fn resolve(&self, kind: AddressKind, value: u64) -> ResolvedAddress {
        self.address_space.resolve(kind, value)
    }

    /// Like [`ExecutableInfo::va_to_offset`] but with an error for the UI.
    pub fn goto_va(&self, va: u64) -> Result<u64> {
        self.va_to_offset(va).ok_or(ReError::Unmapped(va))
    }

    /// Section containing a virtual address.
    pub fn section_at_va(&self, va: u64) -> Option<&Section> {
        self.sections.iter().find(|s| {
            va.checked_sub(s.virtual_address)
                .is_some_and(|r| r < s.virtual_size.max(s.file_size))
        })
    }

    /// Section containing a file offset.
    pub fn section_at_offset(&self, offset: u64) -> Option<&Section> {
        self.sections.iter().find(|s| {
            s.file_size > 0
                && offset
                    .checked_sub(s.offset)
                    .is_some_and(|r| r < s.file_size)
        })
    }

    /// File-backed executable ranges (executable sections, or executable
    /// segments when there are no section headers): `(offset, size, va)`.
    pub fn code_ranges(&self) -> Vec<(u64, u64, u64)> {
        let mut v: Vec<(u64, u64, u64)> = self
            .sections
            .iter()
            .filter(|s| s.executable && s.file_size > 0)
            .map(|s| (s.offset, s.file_size, s.virtual_address))
            .collect();
        if v.is_empty() {
            v = self
                .segments
                .iter()
                .filter(|s| s.permissions.contains('x') && s.file_size > 0)
                .map(|s| (s.offset, s.file_size, s.virtual_address))
                .collect();
        }
        v
    }

    fn shift(&mut self, delta: u64) {
        let sh = |o: &mut u64| *o += delta;
        for r in &mut self.headers {
            sh(&mut r.offset);
        }
        for s in &mut self.sections {
            if s.file_size > 0 {
                sh(&mut s.offset);
            }
            if let Some(h) = s.header_offset.as_mut() {
                sh(h);
            }
        }
        for s in &mut self.segments {
            sh(&mut s.offset);
        }
        for i in &mut self.imports {
            if let Some(o) = i.offset.as_mut() {
                sh(o);
            }
        }
        for e in &mut self.exports {
            if let Some(o) = e.offset.as_mut() {
                sh(o);
            }
        }
        for s in &mut self.symbols {
            if let Some(o) = s.offset.as_mut() {
                sh(o);
            }
        }
        for r in &mut self.relocations.regions {
            sh(&mut r.offset);
        }
        for m in &mut self.address_space.mappings {
            sh(&mut m.offset);
        }
        if let Some(o) = self.entry_offset.as_mut() {
            sh(o);
        }
    }
}

pub(crate) fn perms(r: bool, w: bool, x: bool) -> String {
    format!(
        "{}{}{}",
        if r { 'r' } else { '-' },
        if w { 'w' } else { '-' },
        if x { 'x' } else { '-' }
    )
}

pub(crate) fn demangled(name: &str) -> Option<String> {
    crate::demangle::demangle(name).map(|d| d.text)
}

/// File offset of the PE optional-header `CheckSum` field.
pub fn pe_checksum_offset(data: &[u8]) -> Option<usize> {
    pe::layout(data)
        .map(|l| l.checksum_offset)
        .filter(|o| o + 4 <= data.len())
}

/// Detects the container format from the first bytes.
pub fn detect_format(data: &[u8]) -> Option<ExecutableFormat> {
    let magic = data.get(..4)?;
    match magic {
        [b'M', b'Z', ..] => Some(ExecutableFormat::Pe),
        [0x7f, b'E', b'L', b'F'] => Some(ExecutableFormat::Elf),
        [0xfe, 0xed, 0xfa, 0xce | 0xcf] | [0xce | 0xcf, 0xfa, 0xed, 0xfe] => {
            Some(ExecutableFormat::MachO)
        }
        [0xca, 0xfe, 0xba, 0xbe] => {
            // Java class files share the magic; fat headers have a small
            // architecture count where class files have a version >= 45.
            let n = u32::from_be_bytes(data.get(4..8)?.try_into().ok()?);
            (n > 0 && n < 32).then_some(ExecutableFormat::MachOFat)
        }
        _ => None,
    }
}

/// Parses an executable image (RE-01).
pub fn parse(data: &[u8]) -> Result<ExecutableInfo> {
    match detect_format(data).ok_or(ReError::UnknownFormat)? {
        ExecutableFormat::Pe => pe::parse(data),
        ExecutableFormat::Elf => elf::parse(data),
        ExecutableFormat::MachO => macho::parse(data),
        ExecutableFormat::MachOFat => parse_fat(data),
    }
}

/// Parses an executable from a tree snapshot (RE-01).
pub fn parse_tree(tree: &Tree) -> Result<ExecutableInfo> {
    parse(&crate::tree_bytes(tree)?)
}

fn parse_fat(data: &[u8]) -> Result<ExecutableInfo> {
    let multi = goblin::mach::MultiArch::new(data)?;
    let mut info = ExecutableInfo::empty(ExecutableFormat::MachOFat);
    info.endian = Endian::Big;
    info.kind = "universal binary".into();
    info.machine = "universal".into();
    let arches = multi.arches()?;
    info.headers.push(Region {
        name: "fat header".into(),
        offset: 0,
        size: 8 + 20 * arches.len() as u64,
    });
    for fa in arches {
        let off = u64::from(fa.offset);
        let size = u64::from(fa.size);
        let mut slice = FatSlice {
            arch_name: macho::cpu_name(fa.cputype, fa.cpusubtype),
            cpu_type: fa.cputype,
            cpu_subtype: fa.cpusubtype,
            offset: off,
            size,
            info: None,
            error: None,
        };
        let bytes = usize::try_from(off)
            .ok()
            .zip(usize::try_from(off + size).ok())
            .and_then(|(s, e)| data.get(s..e));
        match bytes {
            None => slice.error = Some("slice extends past end of file".into()),
            Some(b) => match macho::parse(b) {
                Ok(mut sub) => {
                    sub.shift(off);
                    slice.info = Some(Box::new(sub));
                }
                Err(e) => slice.error = Some(e.to_string()),
            },
        }
        info.slices.push(slice);
    }
    Ok(info)
}
