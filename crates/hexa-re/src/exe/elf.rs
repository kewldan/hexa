//! ELF parsing (RE-01): 32/64-bit, little and big endian.

use std::collections::HashMap;

use goblin::elf::Elf;
use goblin::elf::header::{
    EM_68K, EM_386, EM_AARCH64, EM_ARM, EM_BPF, EM_MIPS, EM_PPC, EM_PPC64, EM_RISCV, EM_S390,
    EM_SH, EM_SPARC, EM_SPARCV9, EM_X86_64, ET_CORE, ET_DYN, ET_EXEC, ET_REL, machine_to_str,
};
use goblin::elf::program_header::{PT_LOAD, pt_to_str};
use goblin::elf::section_header::{
    SHF_ALLOC, SHF_EXECINSTR, SHF_WRITE, SHT_NOBITS, SHT_REL, SHT_RELA,
};
use goblin::elf::sym::{STB_GLOBAL, STB_WEAK, type_to_str};

use super::{
    AddressSpace, ExecutableFormat, ExecutableInfo, Export, Import, MAX_SYMBOLS, Mapping, Region,
    Section, Segment, Symbol, demangled, perms,
};
use crate::arch::{Arch, Endian};
use crate::error::Result;

const EM_TRICORE: u16 = 44;
const EM_XCORE: u16 = 203;
const EM_TI_C6000: u16 = 140;

fn arch_of(machine: u16, is64: bool, le: bool, entry: u64) -> Option<Arch> {
    Some(match machine {
        EM_386 => Arch::X86,
        EM_X86_64 => Arch::X64,
        EM_ARM => {
            if entry & 1 == 1 {
                Arch::Thumb
            } else {
                Arch::Arm
            }
        }
        EM_AARCH64 => Arch::Arm64,
        EM_MIPS => match (is64, le) {
            (false, true) => Arch::Mips32,
            (false, false) => Arch::Mips32Be,
            (true, true) => Arch::Mips64,
            (true, false) => Arch::Mips64Be,
        },
        EM_PPC if !le => Arch::Ppc32,
        EM_PPC64 => {
            if le {
                Arch::Ppc64Le
            } else {
                Arch::Ppc64
            }
        }
        EM_RISCV => {
            if is64 {
                Arch::RiscV64
            } else {
                Arch::RiscV32
            }
        }
        EM_SPARC => Arch::Sparc,
        EM_SPARCV9 => Arch::SparcV9,
        EM_S390 => Arch::SysZ,
        EM_68K => Arch::M68k,
        EM_BPF => Arch::Ebpf,
        EM_SH => Arch::Sh4,
        EM_TRICORE => Arch::TriCore,
        EM_XCORE => Arch::Xcore,
        EM_TI_C6000 => Arch::Tms320c64x,
        _ => return None,
    })
}

pub(super) fn parse(data: &[u8]) -> Result<ExecutableInfo> {
    let elf = Elf::parse(data)?;
    let h = &elf.header;
    let mut info = ExecutableInfo::empty(ExecutableFormat::Elf);
    info.bits = if elf.is_64 { 64 } else { 32 };
    info.endian = if elf.little_endian {
        Endian::Little
    } else {
        Endian::Big
    };
    info.machine = format!("{} ({})", machine_to_str(h.e_machine), h.e_machine);
    info.arch = arch_of(h.e_machine, elf.is_64, elf.little_endian, h.e_entry);
    info.kind = match h.e_type {
        ET_REL => "relocatable object",
        ET_EXEC => "executable",
        ET_DYN if elf.interpreter.is_some() => "position-independent executable",
        ET_DYN => "shared object",
        ET_CORE => "core dump",
        _ => "unknown",
    }
    .into();
    info.interpreter = elf.interpreter.map(str::to_string);
    info.libraries = elf
        .libraries
        .iter()
        .map(std::string::ToString::to_string)
        .collect();

    info.headers.push(Region {
        name: "ELF header".into(),
        offset: 0,
        size: u64::from(h.e_ehsize),
    });
    if h.e_phnum > 0 {
        info.headers.push(Region {
            name: "program header table".into(),
            offset: h.e_phoff,
            size: u64::from(h.e_phnum) * u64::from(h.e_phentsize),
        });
    }
    if h.e_shnum > 0 {
        info.headers.push(Region {
            name: "section header table".into(),
            offset: h.e_shoff,
            size: u64::from(h.e_shnum) * u64::from(h.e_shentsize),
        });
    }

    // Segments and mappings.
    let mut space = AddressSpace::default();
    let mut min_load = u64::MAX;
    for ph in &elf.program_headers {
        let p = ph.p_flags;
        info.segments.push(Segment {
            name: pt_to_str(ph.p_type).to_string(),
            virtual_address: ph.p_vaddr,
            virtual_size: ph.p_memsz,
            offset: ph.p_offset,
            file_size: ph.p_filesz,
            permissions: perms(p & 4 != 0, p & 2 != 0, p & 1 != 0),
        });
        if ph.p_type == PT_LOAD {
            min_load = min_load.min(ph.p_vaddr);
            space.mappings.push(Mapping {
                name: format!("LOAD {}", perms(p & 4 != 0, p & 2 != 0, p & 1 != 0)),
                virtual_address: ph.p_vaddr,
                virtual_size: ph.p_memsz,
                offset: ph.p_offset,
                file_size: ph
                    .p_filesz
                    .min((data.len() as u64).saturating_sub(ph.p_offset)),
            });
        }
    }

    // Sections.
    for (i, sh) in elf.section_headers.iter().enumerate() {
        let name = elf.shdr_strtab.get_at(sh.sh_name).unwrap_or("").to_string();
        let f = sh.sh_flags;
        let nobits = sh.sh_type == SHT_NOBITS;
        let exec = f & u64::from(SHF_EXECINSTR) != 0;
        info.sections.push(Section {
            name: name.clone(),
            virtual_address: sh.sh_addr,
            virtual_size: sh.sh_size,
            offset: sh.sh_offset,
            file_size: if nobits { 0 } else { sh.sh_size },
            flags: f,
            permissions: perms(
                f & u64::from(SHF_ALLOC) != 0,
                f & u64::from(SHF_WRITE) != 0,
                exec,
            ),
            executable: exec,
            header_offset: Some(h.e_shoff + i as u64 * u64::from(h.e_shentsize)),
        });
        if sh.sh_type == SHT_REL || sh.sh_type == SHT_RELA {
            info.relocations.regions.push(Region {
                name,
                offset: sh.sh_offset,
                size: sh.sh_size,
            });
        }
    }
    if space.mappings.is_empty() {
        // Relocatable objects: map allocated sections at their addresses.
        for s in &info.sections {
            if s.flags & u64::from(SHF_ALLOC) != 0 && s.file_size > 0 {
                min_load = min_load.min(s.virtual_address);
                space.mappings.push(Mapping {
                    name: s.name.clone(),
                    virtual_address: s.virtual_address,
                    virtual_size: s.virtual_size,
                    offset: s.offset,
                    file_size: s.file_size,
                });
            }
        }
    }
    space.image_base = if min_load == u64::MAX { 0 } else { min_load };
    info.address_space = space;

    if h.e_entry != 0 {
        let entry = if h.e_machine == EM_ARM {
            h.e_entry & !1
        } else {
            h.e_entry
        };
        info.entry_point = Some(entry);
        info.entry_offset = info.va_to_offset(entry);
    }

    // Relocations.
    let mut slots: HashMap<usize, u64> = HashMap::new();
    for relocs in [&elf.dynrelas, &elf.dynrels, &elf.pltrelocs] {
        for r in relocs {
            info.relocations
                .add(goblin::elf::reloc::r_to_str(r.r_type, h.e_machine), 1);
            if r.r_sym != 0 {
                slots.entry(r.r_sym).or_insert(r.r_offset);
            }
        }
    }
    for (_, relocs) in &elf.shdr_relocs {
        for r in relocs {
            info.relocations
                .add(goblin::elf::reloc::r_to_str(r.r_type, h.e_machine), 1);
        }
    }
    info.relocations.finish();

    // Dynamic symbols: imports and exports.
    for (idx, sym) in elf.dynsyms.iter().enumerate() {
        let Some(name) = elf.dynstrtab.get_at(sym.st_name) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let bind = sym.st_bind();
        if sym.st_shndx == 0 {
            let address = slots.get(&idx).copied();
            info.imports.push(Import {
                library: None,
                name: name.to_string(),
                demangled: demangled(name),
                ordinal: None,
                address,
                offset: address.and_then(|a| info.address_space.va_to_offset(a)),
            });
        } else if bind == STB_GLOBAL || bind == STB_WEAK {
            info.exports.push(Export {
                name: name.to_string(),
                demangled: demangled(name),
                address: sym.st_value,
                offset: info.address_space.va_to_offset(sym.st_value),
                forwarder: None,
            });
        }
    }

    // Symbol tables (.symtab first, then .dynsym).
    let mut seen = std::collections::HashSet::new();
    for (table, strtab) in [(&elf.syms, &elf.strtab), (&elf.dynsyms, &elf.dynstrtab)] {
        for sym in table {
            let Some(name) = strtab.get_at(sym.st_name) else {
                continue;
            };
            if name.is_empty() || !seen.insert((name.to_string(), sym.st_value)) {
                continue;
            }
            if info.symbols.len() >= MAX_SYMBOLS {
                info.symbols_truncated = true;
                break;
            }
            let defined = sym.st_shndx != 0;
            info.symbols.push(Symbol {
                name: name.to_string(),
                demangled: demangled(name),
                address: sym.st_value,
                size: sym.st_size,
                kind: type_to_str(sym.st_type()).to_lowercase(),
                global: matches!(sym.st_bind(), STB_GLOBAL | STB_WEAK),
                defined,
                offset: if defined {
                    info.address_space.va_to_offset(sym.st_value)
                } else {
                    None
                },
            });
        }
    }
    Ok(info)
}
