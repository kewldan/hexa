//! PE32 / PE32+ parsing (RE-01).

use goblin::pe::PE;
use goblin::pe::options::{ParseMode, ParseOptions};

use super::{
    AddressSpace, ChecksumField, ExecutableFormat, ExecutableInfo, Export, Import, Mapping, Region,
    RelocationSummary, ResourceData, ResourceNode, RichEntry, RichHeader, Section, demangled,
    perms,
};
use crate::arch::{Arch, Endian};
use crate::error::{ReError, Result};
use crate::util::{le16, le32, u64_at};

/// Data directory indices.
pub(crate) const DIR_RESOURCE: usize = 2;
pub(crate) const DIR_BASERELOC: usize = 5;
pub(crate) const DIR_CLR: usize = 14;

const DIR_NAMES: [&str; 16] = [
    "export directory",
    "import directory",
    "resource directory",
    "exception directory",
    "certificate table",
    "base relocation table",
    "debug directory",
    "architecture",
    "global pointer",
    "TLS directory",
    "load config directory",
    "bound import",
    "import address table",
    "delay import descriptor",
    "CLR runtime header",
    "reserved",
];

/// Layout of the PE headers needed by several modules.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PeLayout {
    pub e_lfanew: usize,
    pub opt: usize,
    pub is64: bool,
    pub checksum_offset: usize,
    pub dirs: usize,
    pub ndirs: usize,
}

pub(crate) fn layout(data: &[u8]) -> Option<PeLayout> {
    if data.get(..2)? != b"MZ" {
        return None;
    }
    let e_lfanew = le32(data, 0x3c)? as usize;
    if data.get(e_lfanew..e_lfanew + 4)? != b"PE\0\0" {
        return None;
    }
    let opt = e_lfanew + 24;
    let is64 = match le16(data, opt)? {
        0x10b => false,
        0x20b => true,
        _ => return None,
    };
    let (ndirs_at, dirs) = if is64 {
        (opt + 108, opt + 112)
    } else {
        (opt + 92, opt + 96)
    };
    let ndirs = (le32(data, ndirs_at)? as usize).min(16);
    Some(PeLayout {
        e_lfanew,
        opt,
        is64,
        checksum_offset: opt + 64,
        dirs,
        ndirs,
    })
}

pub(crate) fn data_dir(data: &[u8], l: &PeLayout, idx: usize) -> Option<(u32, u32)> {
    if idx >= l.ndirs {
        return None;
    }
    let at = l.dirs + idx * 8;
    let rva = le32(data, at)?;
    let size = le32(data, at + 4)?;
    (rva != 0 && size != 0).then_some((rva, size))
}

fn machine(m: u16) -> (Option<Arch>, &'static str) {
    match m {
        0x14c => (Some(Arch::X86), "i386"),
        0x8664 => (Some(Arch::X64), "x86-64"),
        0x1c0 => (Some(Arch::Arm), "ARM"),
        0x1c2 => (Some(Arch::Thumb), "ARM Thumb"),
        0x1c4 => (Some(Arch::Thumb), "ARMv7 (Thumb-2)"),
        0xaa64 => (Some(Arch::Arm64), "ARM64"),
        0xa641 => (Some(Arch::Arm64), "ARM64EC"),
        0xa64e => (Some(Arch::Arm64), "ARM64X"),
        0x166 => (Some(Arch::Mips32), "MIPS R4000"),
        0x169 => (Some(Arch::Mips32), "MIPS WCE v2"),
        0x5032 => (Some(Arch::RiscV32), "RISC-V 32"),
        0x5064 => (Some(Arch::RiscV64), "RISC-V 64"),
        0x1f0 => (None, "PowerPC LE"),
        0x200 => (None, "IA-64"),
        0xebc => (None, "EFI byte code"),
        0x9041 => (None, "M32R"),
        0x6232 => (None, "LoongArch 32"),
        0x6264 => (None, "LoongArch 64"),
        _ => (None, "unknown"),
    }
}

fn subsystem(s: u16) -> &'static str {
    match s {
        1 => "native",
        2 => "Windows GUI",
        3 => "Windows console",
        5 => "OS/2 console",
        7 => "POSIX console",
        9 => "Windows CE GUI",
        10 => "EFI application",
        11 => "EFI boot service driver",
        12 => "EFI runtime driver",
        13 => "EFI ROM",
        14 => "Xbox",
        16 => "Windows boot application",
        _ => "unknown subsystem",
    }
}

const RESOURCE_TYPES: [(u32, &str); 21] = [
    (1, "CURSOR"),
    (2, "BITMAP"),
    (3, "ICON"),
    (4, "MENU"),
    (5, "DIALOG"),
    (6, "STRING"),
    (7, "FONTDIR"),
    (8, "FONT"),
    (9, "ACCELERATOR"),
    (10, "RCDATA"),
    (11, "MESSAGETABLE"),
    (12, "GROUP_CURSOR"),
    (14, "GROUP_ICON"),
    (16, "VERSION"),
    (17, "DLGINCLUDE"),
    (19, "PLUGPLAY"),
    (20, "VXD"),
    (21, "ANICURSOR"),
    (22, "ANIICON"),
    (23, "HTML"),
    (24, "MANIFEST"),
];

fn parse_pe(data: &[u8]) -> Result<PE<'_>> {
    let mut opts = ParseOptions::default();
    opts.parse_attribute_certificates = false;
    opts.parse_resources = false;
    if let Ok(pe) = PE::parse_with_opts(data, &opts) {
        Ok(pe)
    } else {
        let mut opts = opts.with_parse_mode(ParseMode::Permissive);
        opts.parse_tls_data = false;
        PE::parse_with_opts(data, &opts).or_else(|_| {
            let mut o = opts.with_parse_imports(false);
            o.resolve_rva = true;
            PE::parse_with_opts(data, &o).map_err(ReError::from)
        })
    }
}

pub(super) fn parse(data: &[u8]) -> Result<ExecutableInfo> {
    let l = layout(data).ok_or_else(|| ReError::Malformed("no PE signature".into()))?;
    let pe = parse_pe(data)?;
    let mut info = ExecutableInfo::empty(ExecutableFormat::Pe);
    info.bits = if l.is64 { 64 } else { 32 };
    info.endian = Endian::Little;
    let coff = l.e_lfanew + 4;
    let mach = le16(data, coff).unwrap_or(0);
    let nsect = usize::from(le16(data, coff + 2).unwrap_or(0));
    let opt_size = usize::from(le16(data, coff + 16).unwrap_or(0));
    let chars = le16(data, coff + 18).unwrap_or(0);
    let (arch, mname) = machine(mach);
    info.arch = arch;
    info.machine = format!("{mname} ({mach:#06x})");
    let sub = le16(data, l.opt + 68).unwrap_or(0);
    info.kind = format!(
        "{}, {}",
        if chars & 0x2000 != 0 {
            "dll"
        } else {
            "executable"
        },
        subsystem(sub)
    );
    let image_base = if l.is64 {
        u64_at(data, l.opt + 24, Endian::Little).unwrap_or(0)
    } else {
        u64::from(le32(data, l.opt + 28).unwrap_or(0))
    };
    let size_of_headers = u64::from(le32(data, l.opt + 60).unwrap_or(0));

    // Header regions.
    info.headers.push(Region {
        name: "DOS header".into(),
        offset: 0,
        size: 64,
    });
    if l.e_lfanew > 64 {
        info.headers.push(Region {
            name: "DOS stub".into(),
            offset: 64,
            size: l.e_lfanew as u64 - 64,
        });
    }
    info.headers.push(Region {
        name: "PE signature".into(),
        offset: l.e_lfanew as u64,
        size: 4,
    });
    info.headers.push(Region {
        name: "COFF file header".into(),
        offset: coff as u64,
        size: 20,
    });
    info.headers.push(Region {
        name: if l.is64 {
            "optional header (PE32+)"
        } else {
            "optional header (PE32)"
        }
        .into(),
        offset: l.opt as u64,
        size: opt_size as u64,
    });
    let sect_table = l.opt + opt_size;
    info.headers.push(Region {
        name: "section table".into(),
        offset: sect_table as u64,
        size: (nsect * 40) as u64,
    });
    info.checksum = le32(data, l.checksum_offset).map(|value| ChecksumField {
        offset: l.checksum_offset as u64,
        size: 4,
        value,
    });

    // Sections and address space.
    let mut space = AddressSpace {
        image_base,
        mappings: vec![Mapping {
            name: "headers".into(),
            virtual_address: image_base,
            virtual_size: size_of_headers,
            offset: 0,
            file_size: size_of_headers.min(data.len() as u64),
        }],
    };
    for (i, s) in pe.sections.iter().enumerate() {
        let name = s
            .real_name
            .clone()
            .or_else(|| s.name().ok().map(str::to_string))
            .unwrap_or_default();
        let raw = u64::from(s.size_of_raw_data);
        let vsize = if s.virtual_size == 0 {
            raw
        } else {
            u64::from(s.virtual_size)
        };
        let ptr = u64::from(s.pointer_to_raw_data);
        let file_size = if ptr == 0 {
            0
        } else {
            raw.min(vsize).min((data.len() as u64).saturating_sub(ptr))
        };
        let c = s.characteristics;
        let exec = c & 0x2000_0000 != 0 || c & 0x20 != 0;
        info.sections.push(Section {
            name: name.clone(),
            virtual_address: image_base + u64::from(s.virtual_address),
            virtual_size: vsize,
            offset: ptr,
            file_size,
            flags: u64::from(c),
            permissions: perms(c & 0x4000_0000 != 0, c & 0x8000_0000 != 0, exec),
            executable: exec,
            header_offset: Some((sect_table + i * 40) as u64),
        });
        if file_size > 0 || vsize > 0 {
            space.mappings.push(Mapping {
                name: info.sections[i].name.clone(),
                virtual_address: image_base + u64::from(s.virtual_address),
                virtual_size: vsize,
                offset: ptr,
                file_size,
            });
        }
    }
    info.address_space = space;

    // Data directory regions.
    for (idx, dir_name) in DIR_NAMES.iter().enumerate().take(l.ndirs) {
        if let Some((rva, size)) = data_dir(data, &l, idx) {
            let off = if idx == 4 {
                // The certificate table holds a file offset, not an RVA.
                Some(u64::from(rva))
            } else {
                info.rva_to_offset(u64::from(rva))
            };
            if let Some(off) = off {
                info.headers.push(Region {
                    name: (*dir_name).into(),
                    offset: off,
                    size: u64::from(size),
                });
                if idx == DIR_CLR {
                    info.clr_header = Some(Region {
                        name: "CLR runtime header".into(),
                        offset: off,
                        size: u64::from(size),
                    });
                }
            } else {
                info.warnings
                    .push(format!("{dir_name} at RVA {rva:#x} is not file-backed"));
            }
        }
    }

    let entry_rva = le32(data, l.opt + 16).unwrap_or(0);
    if entry_rva != 0 {
        let va = image_base + u64::from(entry_rva);
        info.entry_point = Some(va);
        info.entry_offset = info.va_to_offset(va);
    }

    // Imports.
    for imp in &pe.imports {
        let address = image_base + imp.offset as u64;
        let (name, ordinal) = if let Some(rest) = imp.name.strip_prefix("ORDINAL ") {
            (format!("#{rest}"), rest.parse().ok())
        } else {
            (imp.name.to_string(), None)
        };
        info.imports.push(Import {
            library: Some(imp.dll.to_string()),
            demangled: demangled(&name),
            name,
            ordinal,
            address: Some(address),
            offset: info.va_to_offset(address),
        });
    }
    info.libraries = pe
        .libraries
        .iter()
        .map(std::string::ToString::to_string)
        .collect();

    // Exports.
    for e in &pe.exports {
        let forwarder = e.reexport.as_ref().map(|r| match r {
            goblin::pe::export::Reexport::DLLName { export, lib } => format!("{lib}.{export}"),
            goblin::pe::export::Reexport::DLLOrdinal { ordinal, lib } => {
                format!("{lib}.#{ordinal}")
            }
        });
        let name = e.name.unwrap_or("").to_string();
        let address = if forwarder.is_some() {
            0
        } else {
            image_base + e.rva as u64
        };
        info.exports.push(Export {
            demangled: demangled(&name),
            name,
            address,
            offset: if forwarder.is_some() {
                None
            } else {
                info.va_to_offset(address)
            },
            forwarder,
        });
    }

    info.relocations = base_relocations(data, &l, &info.address_space);
    info.resources = resources(data, &l, &info.address_space);
    info.rich_header = rich_header(data, l.e_lfanew);
    Ok(info)
}

fn reloc_type(t: u16, mach_arm: bool) -> &'static str {
    match t {
        0 => "ABSOLUTE",
        1 => "HIGH",
        2 => "LOW",
        3 => "HIGHLOW",
        4 => "HIGHADJ",
        5 if mach_arm => "ARM_MOV32",
        5 => "MIPS_JMPADDR",
        7 => "THUMB_MOV32",
        9 => "MIPS_JMPADDR16",
        10 => "DIR64",
        _ => "OTHER",
    }
}

fn base_relocations(data: &[u8], l: &PeLayout, space: &AddressSpace) -> RelocationSummary {
    let mut sum = RelocationSummary::default();
    let Some((rva, size)) = data_dir(data, l, DIR_BASERELOC) else {
        return sum;
    };
    let Some(start) = space.rva_to_offset(u64::from(rva)) else {
        return sum;
    };
    let mach = le16(data, l.e_lfanew + 4).unwrap_or(0);
    let arm = matches!(mach, 0x1c0 | 0x1c2 | 0x1c4);
    sum.regions.push(Region {
        name: "base relocations".into(),
        offset: start,
        size: u64::from(size),
    });
    let start = start as usize;
    let end = (start + size as usize).min(data.len());
    let mut pos = start;
    while pos + 8 <= end {
        let block = le32(data, pos + 4).unwrap_or(0) as usize;
        if block < 8 || pos + block > end {
            break;
        }
        for e in (pos + 8..pos + block).step_by(2) {
            if let Some(v) = le16(data, e) {
                let t = v >> 12;
                if t != 0 {
                    sum.add(reloc_type(t, arm), 1);
                }
            }
        }
        pos += block;
    }
    sum.finish();
    sum
}

fn utf16_name(data: &[u8], off: usize) -> Option<String> {
    let len = usize::from(le16(data, off)?);
    let bytes = data.get(off + 2..off + 2 + len * 2)?;
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    Some(String::from_utf16_lossy(&units))
}

struct ResCtx<'a> {
    data: &'a [u8],
    base: usize,
    space: &'a AddressSpace,
    budget: usize,
}

fn res_dir(ctx: &mut ResCtx<'_>, dir_off: usize, level: usize, out: &mut Vec<ResourceNode>) {
    if level > 3 {
        return;
    }
    let data = ctx.data;
    let Some(named) = le16(data, dir_off + 12) else {
        return;
    };
    let Some(ids) = le16(data, dir_off + 14) else {
        return;
    };
    let count = usize::from(named) + usize::from(ids);
    for i in 0..count {
        if ctx.budget == 0 {
            return;
        }
        ctx.budget -= 1;
        let e = dir_off + 16 + i * 8;
        let (Some(name_field), Some(target)) = (le32(data, e), le32(data, e + 4)) else {
            return;
        };
        let (name, id) = if name_field & 0x8000_0000 != 0 {
            let s = utf16_name(data, ctx.base + (name_field & 0x7fff_ffff) as usize)
                .unwrap_or_else(|| "?".into());
            (s, None)
        } else {
            let id = name_field & 0xffff;
            let n = match level {
                0 => RESOURCE_TYPES
                    .iter()
                    .find(|(t, _)| *t == id)
                    .map_or_else(|| format!("#{id}"), |(_, n)| (*n).to_string()),
                1 => format!("#{id}"),
                _ => format!("lang {id}"),
            };
            (n, Some(id))
        };
        let mut node = ResourceNode {
            name,
            id,
            offset: e as u64,
            children: Vec::new(),
            data: None,
        };
        let sub = ctx.base + (target & 0x7fff_ffff) as usize;
        if target & 0x8000_0000 != 0 {
            if sub > dir_off {
                res_dir(ctx, sub, level + 1, &mut node.children);
            }
        } else if let (Some(rva), Some(size), Some(cp)) =
            (le32(data, sub), le32(data, sub + 4), le32(data, sub + 8))
        {
            node.data = Some(ResourceData {
                rva,
                offset: ctx.space.rva_to_offset(u64::from(rva)),
                size,
                code_page: cp,
            });
        }
        out.push(node);
    }
}

fn resources(data: &[u8], l: &PeLayout, space: &AddressSpace) -> Option<ResourceNode> {
    let (rva, _) = data_dir(data, l, DIR_RESOURCE)?;
    let base = space.rva_to_offset(u64::from(rva))? as usize;
    let mut ctx = ResCtx {
        data,
        base,
        space,
        budget: 65_536,
    };
    let mut root = ResourceNode {
        name: "resources".into(),
        id: None,
        offset: base as u64,
        children: Vec::new(),
        data: None,
    };
    res_dir(&mut ctx, base, 0, &mut root.children);
    Some(root)
}

const DANS: u32 = 0x536e_6144;
const RICH: u32 = 0x6863_6952;

fn rich_header(data: &[u8], e_lfanew: usize) -> Option<RichHeader> {
    let end = e_lfanew.min(data.len());
    let rich = (0x80..end.saturating_sub(7))
        .step_by(4)
        .find(|&o| le32(data, o) == Some(RICH))?;
    let key = le32(data, rich + 4)?;
    let mut dans = None;
    let mut o = rich;
    while o >= 0x80 {
        o -= 4;
        if le32(data, o)? ^ key == DANS {
            dans = Some(o);
            break;
        }
    }
    let dans = dans?;
    let mut entries = Vec::new();
    let mut csum = dans as u32;
    for (i, &b) in data[..dans].iter().enumerate() {
        if (0x3c..0x40).contains(&i) {
            continue;
        }
        csum = csum.wrapping_add(u32::from(b).rotate_left(i as u32 & 31));
    }
    let mut p = dans + 16;
    while p + 8 <= rich {
        let comp = le32(data, p)? ^ key;
        let count = le32(data, p + 4)? ^ key;
        csum = csum.wrapping_add(comp.rotate_left(count & 31));
        let product_id = (comp >> 16) as u16;
        let build = comp as u16;
        entries.push(RichEntry {
            product_id,
            build,
            count,
            product: rich_product_name(product_id).to_string(),
            visual_studio: rich_visual_studio(product_id, build),
        });
        p += 8;
    }
    Some(RichHeader {
        offset: dans as u64,
        size: (rich + 8 - dans) as u64,
        key,
        checksum_valid: csum == key,
        entries,
    })
}

/// Builds a Rich header block (used by tests and by tooling that wants to
/// rewrite a header): `DanS`, padding, entries, `Rich`, key.
#[cfg(test)]
pub(crate) fn encode_rich(prefix: &[u8], entries: &[(u16, u16, u32)]) -> Vec<u8> {
    let dans = prefix.len();
    let mut csum = dans as u32;
    for (i, &b) in prefix.iter().enumerate() {
        if (0x3c..0x40).contains(&i) {
            continue;
        }
        csum = csum.wrapping_add(u32::from(b).rotate_left(i as u32 & 31));
    }
    for &(pid, build, count) in entries {
        let comp = u32::from(pid) << 16 | u32::from(build);
        csum = csum.wrapping_add(comp.rotate_left(count & 31));
    }
    let key = csum;
    let mut out = Vec::new();
    for v in [DANS ^ key, key, key, key] {
        out.extend(v.to_le_bytes());
    }
    for &(pid, build, count) in entries {
        let comp = u32::from(pid) << 16 | u32::from(build);
        out.extend((comp ^ key).to_le_bytes());
        out.extend((count ^ key).to_le_bytes());
    }
    out.extend(RICH.to_le_bytes());
    out.extend(key.to_le_bytes());
    out
}

const RICH_PRODUCTS: [&str; 271] = [
    "Unknown",
    "Import0",
    "Linker510",
    "Cvtomf510",
    "Linker600",
    "Cvtomf600",
    "Cvtres500",
    "Utc11_Basic",
    "Utc11_C",
    "Utc12_Basic",
    "Utc12_C",
    "Utc12_CPP",
    "AliasObj60",
    "VisualBasic60",
    "Masm613",
    "Masm710",
    "Linker511",
    "Cvtomf511",
    "Masm614",
    "Linker512",
    "Cvtomf512",
    "Utc12_C_Std",
    "Utc12_CPP_Std",
    "Utc12_C_Book",
    "Utc12_CPP_Book",
    "Implib700",
    "Cvtomf700",
    "Utc13_Basic",
    "Utc13_C",
    "Utc13_CPP",
    "Linker610",
    "Cvtomf610",
    "Linker601",
    "Cvtomf601",
    "Utc12_1_Basic",
    "Utc12_1_C",
    "Utc12_1_CPP",
    "Linker620",
    "Cvtomf620",
    "AliasObj70",
    "Linker621",
    "Cvtomf621",
    "Masm615",
    "Utc13_LTCG_C",
    "Utc13_LTCG_CPP",
    "Masm620",
    "ILAsm100",
    "Utc12_2_Basic",
    "Utc12_2_C",
    "Utc12_2_CPP",
    "Utc12_2_C_Std",
    "Utc12_2_CPP_Std",
    "Utc12_2_C_Book",
    "Utc12_2_CPP_Book",
    "Implib622",
    "Cvtomf622",
    "Cvtres501",
    "Utc13_C_Std",
    "Utc13_CPP_Std",
    "Cvtpgd1300",
    "Linker622",
    "Linker700",
    "Export622",
    "Export700",
    "Masm700",
    "Utc13_POGO_I_C",
    "Utc13_POGO_I_CPP",
    "Utc13_POGO_O_C",
    "Utc13_POGO_O_CPP",
    "Cvtres700",
    "Cvtres710p",
    "Linker710p",
    "Cvtomf710p",
    "Export710p",
    "Implib710p",
    "Masm710p",
    "Utc1310p_C",
    "Utc1310p_CPP",
    "Utc1310p_C_Std",
    "Utc1310p_CPP_Std",
    "Utc1310p_LTCG_C",
    "Utc1310p_LTCG_CPP",
    "Utc1310p_POGO_I_C",
    "Utc1310p_POGO_I_CPP",
    "Utc1310p_POGO_O_C",
    "Utc1310p_POGO_O_CPP",
    "Linker624",
    "Cvtomf624",
    "Export624",
    "Implib624",
    "Linker710",
    "Cvtomf710",
    "Export710",
    "Implib710",
    "Cvtres710",
    "Utc1310_C",
    "Utc1310_CPP",
    "Utc1310_C_Std",
    "Utc1310_CPP_Std",
    "Utc1310_LTCG_C",
    "Utc1310_LTCG_CPP",
    "Utc1310_POGO_I_C",
    "Utc1310_POGO_I_CPP",
    "Utc1310_POGO_O_C",
    "Utc1310_POGO_O_CPP",
    "AliasObj710",
    "AliasObj710p",
    "Cvtpgd1310",
    "Cvtpgd1310p",
    "Utc1400_C",
    "Utc1400_CPP",
    "Utc1400_C_Std",
    "Utc1400_CPP_Std",
    "Utc1400_LTCG_C",
    "Utc1400_LTCG_CPP",
    "Utc1400_POGO_I_C",
    "Utc1400_POGO_I_CPP",
    "Utc1400_POGO_O_C",
    "Utc1400_POGO_O_CPP",
    "Cvtpgd1400",
    "Linker800",
    "Cvtomf800",
    "Export800",
    "Implib800",
    "Cvtres800",
    "Masm800",
    "AliasObj800",
    "PhoenixPrerelease",
    "Utc1400_CVTCIL_C",
    "Utc1400_CVTCIL_CPP",
    "Utc1400_LTCG_MSIL",
    "Utc1500_C",
    "Utc1500_CPP",
    "Utc1500_C_Std",
    "Utc1500_CPP_Std",
    "Utc1500_CVTCIL_C",
    "Utc1500_CVTCIL_CPP",
    "Utc1500_LTCG_C",
    "Utc1500_LTCG_CPP",
    "Utc1500_LTCG_MSIL",
    "Utc1500_POGO_I_C",
    "Utc1500_POGO_I_CPP",
    "Utc1500_POGO_O_C",
    "Utc1500_POGO_O_CPP",
    "Cvtpgd1500",
    "Linker900",
    "Export900",
    "Implib900",
    "Cvtres900",
    "Masm900",
    "AliasObj900",
    "Resource",
    "AliasObj1000",
    "Cvtpgd1600",
    "Cvtres1000",
    "Export1000",
    "Implib1000",
    "Linker1000",
    "Masm1000",
    "Phx1600_C",
    "Phx1600_CPP",
    "Phx1600_CVTCIL_C",
    "Phx1600_CVTCIL_CPP",
    "Phx1600_LTCG_C",
    "Phx1600_LTCG_CPP",
    "Phx1600_LTCG_MSIL",
    "Phx1600_POGO_I_C",
    "Phx1600_POGO_I_CPP",
    "Phx1600_POGO_O_C",
    "Phx1600_POGO_O_CPP",
    "Utc1600_C",
    "Utc1600_CPP",
    "Utc1600_CVTCIL_C",
    "Utc1600_CVTCIL_CPP",
    "Utc1600_LTCG_C",
    "Utc1600_LTCG_CPP",
    "Utc1600_LTCG_MSIL",
    "Utc1600_POGO_I_C",
    "Utc1600_POGO_I_CPP",
    "Utc1600_POGO_O_C",
    "Utc1600_POGO_O_CPP",
    "AliasObj1010",
    "Cvtpgd1610",
    "Cvtres1010",
    "Export1010",
    "Implib1010",
    "Linker1010",
    "Masm1010",
    "Utc1610_C",
    "Utc1610_CPP",
    "Utc1610_CVTCIL_C",
    "Utc1610_CVTCIL_CPP",
    "Utc1610_LTCG_C",
    "Utc1610_LTCG_CPP",
    "Utc1610_LTCG_MSIL",
    "Utc1610_POGO_I_C",
    "Utc1610_POGO_I_CPP",
    "Utc1610_POGO_O_C",
    "Utc1610_POGO_O_CPP",
    "AliasObj1100",
    "Cvtpgd1700",
    "Cvtres1100",
    "Export1100",
    "Implib1100",
    "Linker1100",
    "Masm1100",
    "Utc1700_C",
    "Utc1700_CPP",
    "Utc1700_CVTCIL_C",
    "Utc1700_CVTCIL_CPP",
    "Utc1700_LTCG_C",
    "Utc1700_LTCG_CPP",
    "Utc1700_LTCG_MSIL",
    "Utc1700_POGO_I_C",
    "Utc1700_POGO_I_CPP",
    "Utc1700_POGO_O_C",
    "Utc1700_POGO_O_CPP",
    "AliasObj1200",
    "Cvtpgd1800",
    "Cvtres1200",
    "Export1200",
    "Implib1200",
    "Linker1200",
    "Masm1200",
    "Utc1800_C",
    "Utc1800_CPP",
    "Utc1800_CVTCIL_C",
    "Utc1800_CVTCIL_CPP",
    "Utc1800_LTCG_C",
    "Utc1800_LTCG_CPP",
    "Utc1800_LTCG_MSIL",
    "Utc1800_POGO_I_C",
    "Utc1800_POGO_I_CPP",
    "Utc1800_POGO_O_C",
    "Utc1800_POGO_O_CPP",
    "AliasObj1210",
    "Cvtpgd1810",
    "Cvtres1210",
    "Export1210",
    "Implib1210",
    "Linker1210",
    "Masm1210",
    "Utc1810_C",
    "Utc1810_CPP",
    "Utc1810_CVTCIL_C",
    "Utc1810_CVTCIL_CPP",
    "Utc1810_LTCG_C",
    "Utc1810_LTCG_CPP",
    "Utc1810_LTCG_MSIL",
    "Utc1810_POGO_I_C",
    "Utc1810_POGO_I_CPP",
    "Utc1810_POGO_O_C",
    "Utc1810_POGO_O_CPP",
    "AliasObj1400",
    "Cvtpgd1900",
    "Cvtres1400",
    "Export1400",
    "Implib1400",
    "Linker1400",
    "Masm1400",
    "Utc1900_C",
    "Utc1900_CPP",
    "Utc1900_CVTCIL_C",
    "Utc1900_CVTCIL_CPP",
    "Utc1900_LTCG_C",
    "Utc1900_LTCG_CPP",
    "Utc1900_LTCG_MSIL",
    "Utc1900_POGO_I_C",
    "Utc1900_POGO_I_CPP",
    "Utc1900_POGO_O_C",
    "Utc1900_POGO_O_CPP",
];

/// Name of a Rich header product id (RE-07), e.g. `Utc1900_CPP`.
pub fn rich_product_name(id: u16) -> &'static str {
    RICH_PRODUCTS
        .get(usize::from(id))
        .copied()
        .unwrap_or("Unknown")
}

/// Visual Studio release that produced a Rich header entry.
///
/// Compiler (`Utc`, `Phx`, `Cvtpgd`) ids carry the compiler version and
/// tool ids (`Linker`, `Masm`, ...) the toolset version. The 14.x toolset
/// is shared by VS 2015-2022, so the build number picks the release
/// (heuristic boundaries: < 25000 VS2015, < 27000 VS2017, < 30500 VS2019).
pub fn rich_visual_studio(id: u16, build: u16) -> Option<String> {
    let name = rich_product_name(id);
    let prefix: String = name.chars().take_while(char::is_ascii_alphabetic).collect();
    let digits: String = name[prefix.len()..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    if digits.is_empty() {
        return None;
    }
    let compiler = matches!(prefix.as_str(), "Utc" | "Phx" | "Cvtpgd");
    let modern = || {
        Some(
            match build {
                0..25_000 => "Visual Studio 2015",
                25_000..27_000 => "Visual Studio 2017",
                27_000..30_500 => "Visual Studio 2019",
                _ => "Visual Studio 2022",
            }
            .to_string(),
        )
    };
    let release = if compiler {
        match digits.as_str() {
            "11" => "Visual C++ 5.0",
            "12" => "Visual C++ 6.0",
            "13" | "1300" => "Visual Studio .NET 2002",
            "1310" => "Visual Studio .NET 2003",
            "1400" => "Visual Studio 2005",
            "1500" => "Visual Studio 2008",
            "1600" => "Visual Studio 2010",
            "1610" => "Visual Studio 2010 SP1",
            "1700" => "Visual Studio 2012",
            "1800" | "1810" => "Visual Studio 2013",
            "1900" => return modern(),
            _ => return None,
        }
    } else {
        match digits.as_str() {
            "500" | "501" | "510" | "511" | "512" => "Visual C++ 5.0",
            "60" | "600" | "601" | "610" | "612" | "613" | "614" | "615" | "620" | "621"
            | "622" | "624" => "Visual C++ 6.0",
            "70" | "100" | "700" => "Visual Studio .NET 2002",
            "710" => "Visual Studio .NET 2003",
            "800" => "Visual Studio 2005",
            "900" => "Visual Studio 2008",
            "1000" => "Visual Studio 2010",
            "1010" => "Visual Studio 2010 SP1",
            "1100" => "Visual Studio 2012",
            "1200" | "1210" => "Visual Studio 2013",
            "1400" => return modern(),
            _ => return None,
        }
    };
    Some(release.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rich_names() {
        assert_eq!(rich_product_name(0x0104), "Utc1900_C");
        assert_eq!(rich_product_name(0x0105), "Utc1900_CPP");
        assert_eq!(rich_product_name(0x0102), "Linker1400");
        assert_eq!(rich_product_name(0x00de), "Linker1200");
        assert_eq!(rich_product_name(0x0091), "Linker900");
        assert_eq!(rich_product_name(0x9999), "Unknown");
        assert_eq!(
            rich_visual_studio(0x0105, 30_133).as_deref(),
            Some("Visual Studio 2019")
        );
        assert_eq!(
            rich_visual_studio(0x0102, 33_145).as_deref(),
            Some("Visual Studio 2022")
        );
        assert_eq!(
            rich_visual_studio(0x0084, 30_729).as_deref(),
            Some("Visual Studio 2008")
        );
        assert_eq!(
            rich_visual_studio(0x006d, 50_727).as_deref(),
            Some("Visual Studio 2005")
        );
        assert_eq!(
            rich_visual_studio(0x0078, 50_727).as_deref(),
            Some("Visual Studio 2005")
        );
        assert_eq!(rich_visual_studio(0x0001, 0), None);
    }

    #[test]
    fn rich_roundtrip() {
        let mut prefix = vec![0u8; 0x80];
        prefix[0] = b'M';
        prefix[1] = b'Z';
        let block = encode_rich(&prefix, &[(0x0105, 30_133, 12), (0x0102, 30_133, 1)]);
        let mut data = prefix.clone();
        data.extend(&block);
        let e_lfanew = data.len();
        data[0x3c..0x40].copy_from_slice(&(e_lfanew as u32).to_le_bytes());
        data.extend(b"PE\0\0");
        let h = rich_header(&data, e_lfanew).unwrap();
        assert_eq!(h.offset, 0x80);
        assert!(h.checksum_valid);
        assert_eq!(h.entries.len(), 2);
        assert_eq!(h.entries[0].product, "Utc1900_CPP");
        assert_eq!(h.entries[0].count, 12);
        assert_eq!(h.entries[1].build, 30_133);
    }
}
