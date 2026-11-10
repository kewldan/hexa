//! Mach-O parsing (RE-01): 32/64-bit images; fat containers are split in
//! `super::parse_fat`.

use goblin::mach::MachO;
use goblin::mach::exports::ExportInfo;
use goblin::mach::load_command::{CommandVariant, cmd_to_str};

use super::{
    AddressSpace, ExecutableFormat, ExecutableInfo, Export, Import, MAX_SYMBOLS, Mapping, Region,
    Section, Segment, Symbol, demangled, perms,
};
use crate::arch::{Arch, Endian};
use crate::error::Result;

const CPU_ARCH_ABI64: u32 = 0x0100_0000;
const CPU_ARCH_ABI64_32: u32 = 0x0200_0000;

pub(super) fn cpu_name(cputype: u32, cpusubtype: u32) -> String {
    goblin::mach::constants::cputype::get_arch_name_from_types(cputype, cpusubtype & 0x00ff_ffff)
        .map_or_else(
            || match cputype {
                7 => "i386".into(),
                0x0100_0007 => "x86_64".into(),
                12 => "arm".into(),
                0x0100_000c => "arm64".into(),
                0x0200_000c => "arm64_32".into(),
                18 => "ppc".into(),
                0x0100_0012 => "ppc64".into(),
                _ => format!("cpu {cputype:#x}"),
            },
            str::to_string,
        )
}

fn arch_of(cputype: u32) -> Option<Arch> {
    Some(match cputype {
        7 => Arch::X86,
        0x0100_0007 => Arch::X64,
        12 => Arch::Arm,
        0x0100_000c | 0x0200_000c => Arch::Arm64,
        18 => Arch::Ppc32,
        0x0100_0012 => Arch::Ppc64,
        6 => Arch::M68k,
        14 => Arch::Sparc,
        _ => return None,
    })
}

fn file_type(t: u32) -> &'static str {
    match t {
        1 => "object",
        2 => "executable",
        3 => "fixed VM shared library",
        4 => "core",
        5 => "preloaded executable",
        6 => "dylib",
        7 => "dynamic linker",
        8 => "bundle",
        9 => "dylib stub",
        10 => "dSYM companion",
        11 => "kext bundle",
        12 => "fileset",
        _ => "unknown",
    }
}

fn vm_perms(p: u32) -> String {
    perms(p & 1 != 0, p & 2 != 0, p & 4 != 0)
}

fn fixed_name(b: &[u8; 16]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(16);
    String::from_utf8_lossy(&b[..end]).into_owned()
}

pub(super) fn parse(data: &[u8]) -> Result<ExecutableInfo> {
    let m = MachO::parse(data, 0)?;
    let h = &m.header;
    let mut info = ExecutableInfo::empty(ExecutableFormat::MachO);
    info.bits = if m.is_64 { 64 } else { 32 };
    info.endian = if m.little_endian {
        Endian::Little
    } else {
        Endian::Big
    };
    info.machine = cpu_name(h.cputype, h.cpusubtype);
    info.arch = arch_of(h.cputype);
    if h.cputype & (CPU_ARCH_ABI64 | CPU_ARCH_ABI64_32) == 0 && h.cputype == 12 {
        info.arch = Some(Arch::Arm);
    }
    info.kind = file_type(h.filetype).into();
    let header_size = if m.is_64 { 32 } else { 28 };
    info.headers.push(Region {
        name: "Mach-O header".into(),
        offset: 0,
        size: header_size,
    });
    info.headers.push(Region {
        name: "load commands".into(),
        offset: header_size,
        size: u64::from(h.sizeofcmds),
    });
    for lc in &m.load_commands {
        info.headers.push(Region {
            name: cmd_to_str(lc.command.cmd()).to_string(),
            offset: lc.offset as u64,
            size: lc.command.cmdsize() as u64,
        });
        match &lc.command {
            CommandVariant::DyldInfo(d) | CommandVariant::DyldInfoOnly(d) => {
                for (name, off, size) in [
                    ("rebase info", d.rebase_off, d.rebase_size),
                    ("bind info", d.bind_off, d.bind_size),
                    ("weak bind info", d.weak_bind_off, d.weak_bind_size),
                    ("lazy bind info", d.lazy_bind_off, d.lazy_bind_size),
                ] {
                    if size > 0 {
                        info.relocations.regions.push(Region {
                            name: name.into(),
                            offset: u64::from(off),
                            size: u64::from(size),
                        });
                    }
                }
            }
            CommandVariant::DyldChainedFixups(d) => {
                info.relocations.regions.push(Region {
                    name: "chained fixups".into(),
                    offset: u64::from(d.dataoff),
                    size: u64::from(d.datasize),
                });
            }
            _ => {}
        }
    }

    let mut space = AddressSpace::default();
    for seg in &m.segments {
        let name = fixed_name(&seg.segname);
        if seg.fileoff == 0 && seg.filesize > 0 {
            space.image_base = seg.vmaddr;
        }
        info.segments.push(Segment {
            name: name.clone(),
            virtual_address: seg.vmaddr,
            virtual_size: seg.vmsize,
            offset: seg.fileoff,
            file_size: seg.filesize,
            permissions: vm_perms(seg.initprot),
        });
        if seg.filesize > 0 {
            space.mappings.push(Mapping {
                name: name.clone(),
                virtual_address: seg.vmaddr,
                virtual_size: seg.vmsize,
                offset: seg.fileoff,
                file_size: seg
                    .filesize
                    .min((data.len() as u64).saturating_sub(seg.fileoff)),
            });
        }
        if let Ok(sections) = seg.sections() {
            for (s, _) in sections {
                let sect = s.name().unwrap_or("").to_string();
                let zerofill = matches!(s.flags & 0xff, 0x01 | 0x0c | 0x12);
                let exec = s.flags & 0x8000_0400 != 0;
                info.sections.push(Section {
                    name: format!("{name},{sect}"),
                    virtual_address: s.addr,
                    virtual_size: s.size,
                    offset: u64::from(s.offset),
                    file_size: if zerofill || s.offset == 0 { 0 } else { s.size },
                    flags: u64::from(s.flags),
                    permissions: vm_perms(seg.initprot),
                    executable: exec,
                    header_offset: None,
                });
                if s.nreloc > 0 {
                    info.relocations
                        .add("section relocation", u64::from(s.nreloc));
                    info.relocations.regions.push(Region {
                        name: format!("{name},{sect} relocations"),
                        offset: u64::from(s.reloff),
                        size: u64::from(s.nreloc) * 8,
                    });
                }
            }
        }
    }
    if space.image_base == 0
        && let Some(min) = space.mappings.iter().map(|m| m.virtual_address).min()
    {
        space.image_base = min;
    }
    info.address_space = space;
    info.relocations.finish();

    if m.entry != 0 {
        info.entry_point = Some(m.entry);
        info.entry_offset = info.va_to_offset(m.entry);
    }
    info.libraries = m
        .libs
        .iter()
        .filter(|l| **l != "self")
        .map(std::string::ToString::to_string)
        .collect();

    if let Ok(imports) = m.imports() {
        for imp in imports {
            let address = Some(imp.address);
            info.imports.push(Import {
                library: Some(imp.dylib.to_string()),
                name: imp.name.to_string(),
                demangled: demangled(imp.name),
                ordinal: None,
                address,
                offset: info.va_to_offset(imp.address),
            });
        }
    } else {
        info.warnings
            .push("could not decode bind information".into());
    }
    if let Ok(exports) = m.exports() {
        for e in exports {
            let (address, forwarder) = match &e.info {
                ExportInfo::Regular { address, .. } => (info.image_base() + address, None),
                ExportInfo::Reexport { lib, .. } => (0, Some((*lib).to_string())),
                ExportInfo::Stub { .. } => (0, None),
            };
            info.exports.push(Export {
                demangled: demangled(&e.name),
                name: e.name.clone(),
                address,
                offset: if address == 0 {
                    None
                } else {
                    info.va_to_offset(address)
                },
                forwarder,
            });
        }
    }

    for sym in m.symbols() {
        let Ok((name, nl)) = sym else {
            break;
        };
        if nl.n_type & 0xe0 != 0 || name.is_empty() {
            continue;
        }
        if info.symbols.len() >= MAX_SYMBOLS {
            info.symbols_truncated = true;
            break;
        }
        let ty = nl.n_type & 0x0e;
        let defined = ty != 0;
        info.symbols.push(Symbol {
            name: name.to_string(),
            demangled: demangled(name),
            address: nl.n_value,
            size: 0,
            kind: match ty {
                0x0 => "undefined",
                0x2 => "absolute",
                0xe => "section",
                0xc => "prebound",
                0xa => "indirect",
                _ => "other",
            }
            .into(),
            global: nl.n_type & 0x01 != 0,
            defined,
            offset: if ty == 0xe {
                info.address_space.va_to_offset(nl.n_value)
            } else {
                None
            },
        });
    }
    Ok(info)
}
