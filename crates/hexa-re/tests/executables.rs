//! RE-01 / RE-02 / RE-04 / RE-06 / RE-07 against hand-built executables.

mod common;

use hexa_core::Tree;
use hexa_core::piece::Piece;
use hexa_re::Arch;
use hexa_re::arch::Endian;
use hexa_re::checksum::{fix_pe_checksum, image_checksum};
use hexa_re::detect::{Confidence, detect};
use hexa_re::exe::{AddressKind, ExecutableFormat, parse, parse_tree};
use hexa_re::xref::{XrefKind, find_xrefs};

use common::*;

#[test]
fn pe64_structure() {
    let data = pe64();
    let info = parse(&data).unwrap();
    assert_eq!(info.format, ExecutableFormat::Pe);
    assert_eq!(info.bits, 64);
    assert_eq!(info.arch, Some(Arch::X64));
    assert!(info.kind.starts_with("dll"), "{}", info.kind);
    assert!(info.kind.contains("console"));
    assert_eq!(info.image_base(), PE64_BASE);
    assert_eq!(info.entry_point, Some(PE64_BASE + 0x1000));
    assert_eq!(info.entry_offset, Some(0x400));
    let names: Vec<&str> = info.sections.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec![".text", ".rdata", ".rsrc", ".reloc"]);
    let text = &info.sections[0];
    assert!(text.executable);
    assert_eq!(text.permissions, "r-x");
    assert_eq!(text.header_offset, Some(0xb0 + 24 + 240));
    assert!(
        info.headers
            .iter()
            .any(|h| h.name == "COFF file header" && h.offset == 0xb4)
    );
    assert!(
        info.headers
            .iter()
            .any(|h| h.name == "import directory" && h.offset == 0x600)
    );

    // Imports.
    let imp: Vec<(&str, Option<u32>)> = info
        .imports
        .iter()
        .map(|i| (i.name.as_str(), i.ordinal))
        .collect();
    assert_eq!(
        imp,
        vec![
            ("ExitProcess", None),
            ("GetStdHandle", None),
            ("#5", Some(5))
        ]
    );
    assert_eq!(info.imports[0].library.as_deref(), Some("KERNEL32.dll"));
    assert_eq!(info.imports[0].address, Some(PE64_BASE + 0x2080));
    assert_eq!(info.imports[0].offset, Some(0x680));
    assert_eq!(info.libraries, vec!["KERNEL32.dll"]);

    // Exports with demangling.
    assert_eq!(info.exports.len(), 1);
    assert_eq!(info.exports[0].name, "_ZN3foo3barEv");
    assert_eq!(info.exports[0].demangled.as_deref(), Some("foo::bar()"));
    assert_eq!(info.exports[0].address, PE64_BASE + 0x1020);
    assert_eq!(info.exports[0].offset, Some(0x420));

    // Relocations.
    assert_eq!(info.relocations.total, 3);
    assert_eq!(info.relocations.by_type[0].kind, "DIR64");
    assert_eq!(info.relocations.regions[0].offset, 0xa00);

    // Resources.
    let root = info.resources.as_ref().unwrap();
    assert_eq!(root.offset, 0x800);
    assert_eq!(root.children.len(), 2);
    assert_eq!(root.children[0].name, "MYDATA");
    assert_eq!(root.children[0].children[0].name, "#101");
    let leaf = &root.children[0].children[0].children[0];
    assert_eq!(leaf.name, "lang 0");
    let d = leaf.data.as_ref().unwrap();
    assert_eq!((d.rva, d.size, d.offset), (0x3100, 5, Some(0x900)));
    assert_eq!(&data[0x900..0x905], b"hello");
    assert_eq!(root.children[1].name, "ICON");
    assert_eq!(root.children[1].id, Some(3));
    let icon = &root.children[1].children[0].children[0];
    assert_eq!(icon.name, "lang 1033");
    assert_eq!(icon.data.as_ref().unwrap().code_page, 1252);

    // Rich header.
    let rich = info.rich_header.as_ref().unwrap();
    assert!(rich.checksum_valid);
    assert_eq!(rich.offset, 0x80);
    assert_eq!(rich.entries[0].product, "Utc1900_CPP");
    assert_eq!(
        rich.entries[0].visual_studio.as_deref(),
        Some("Visual Studio 2019")
    );

    // Serialises with camelCase.
    let json = serde_json::to_string(&info).unwrap();
    assert!(json.contains("\"entryPoint\""));
    assert!(json.contains("\"richHeader\""));
}

#[test]
fn pe_address_conversion() {
    let info = parse(&pe64()).unwrap();
    assert_eq!(info.va_to_offset(PE64_BASE + 0x2010), Some(0x610));
    assert_eq!(info.rva_to_offset(0x1004), Some(0x404));
    assert_eq!(info.offset_to_va(0x810), Some(PE64_BASE + 0x3010));
    assert_eq!(info.offset_to_rva(0x810), Some(0x3010));
    // Headers map at the image base.
    assert_eq!(info.va_to_offset(PE64_BASE + 0x3c), Some(0x3c));
    // Past the raw data of .reloc (virtual size 0x10).
    assert_eq!(info.va_to_offset(PE64_BASE + 0x4100), None);
    assert!(info.goto_va(0x1234).is_err());
    let r = info.resolve(AddressKind::Rva, 0x2008);
    assert_eq!(r.offset, Some(0x608));
    assert_eq!(r.virtual_address, Some(PE64_BASE + 0x2008));
    assert_eq!(r.mapping.as_deref(), Some(".rdata"));
    let r = info.resolve(AddressKind::FileOffset, 0x404);
    assert_eq!(r.rva, Some(0x1004));
    assert_eq!(
        info.section_at_va(PE64_BASE + 0x1001).unwrap().name,
        ".text"
    );
    assert_eq!(info.section_at_offset(0xa02).unwrap().name, ".reloc");
}

#[test]
fn pe_xrefs() {
    let data = pe64();
    let info = parse(&data).unwrap();
    let r = find_xrefs(&data, &info, PE64_BASE + 0x21a0, 100).unwrap();
    assert_eq!(r.code.len(), 1);
    assert_eq!(r.code[0].kind, XrefKind::AddressLoad);
    assert_eq!(r.code[0].from, PE64_BASE + 0x1004);
    assert_eq!(r.code[0].from_offset, Some(0x404));
    assert!(r.code[0].text.starts_with("lea rcx"), "{}", r.code[0].text);
    assert_eq!(r.pointers.len(), 1);
    assert_eq!(r.pointers[0].location.offset, 0x7b0);
    assert_eq!(r.pointers[0].virtual_address, Some(PE64_BASE + 0x21b0));
    assert_eq!(r.pointers[0].section.as_deref(), Some(".rdata"));
    // The IAT slot is referenced by `call [rip+...]`; its RVA is stored in
    // the import descriptor.
    let r = find_xrefs(&data, &info, PE64_BASE + 0x2080, 100).unwrap();
    assert_eq!(r.code[0].kind, XrefKind::Data);
    assert!(
        r.pointers
            .iter()
            .any(|p| p.is_rva && p.location.offset == 0x610)
    );
}

#[test]
fn pe_checksum_fix() {
    let mut data = pe64();
    let c = image_checksum(&data);
    assert_eq!(c.field_offset, Some(0xb4 + 20 + 64));
    assert_eq!(c.stored, Some(0));
    assert!(!c.is_valid());
    let patch = c.fix().unwrap();
    let v = fix_pe_checksum(&mut data).unwrap();
    assert_eq!(patch.replacement, v.to_le_bytes().to_vec());
    assert!(image_checksum(&data).is_valid());
    let info = parse(&data).unwrap();
    assert_eq!(info.checksum.unwrap().value, v);
}

#[test]
fn pe32_upx_dotnet() {
    let data = common::pe32_upx_dotnet();
    let info = parse(&data).unwrap();
    assert_eq!(info.bits, 32);
    assert_eq!(info.arch, Some(Arch::X86));
    assert_eq!(info.image_base(), 0x40_0000);
    // UPX0 has no raw data.
    assert_eq!(info.va_to_offset(0x40_1000), None);
    assert_eq!(info.va_to_offset(0x40_2004), Some(0x204));
    assert_eq!(info.clr_header.as_ref().unwrap().offset, 0x200);
    let f = detect(&data);
    let upx = f.iter().find(|f| f.name == "UPX").unwrap();
    assert_eq!(upx.confidence, Confidence::High);
    assert_eq!(upx.version.as_deref(), Some("4.22"));
    assert!(upx.evidence.iter().any(|e| e.description == "section UPX0"));
    assert!(upx.evidence.iter().any(|e| e.offset == 0x300));
    let net = f.iter().find(|f| f.name == ".NET").unwrap();
    assert_eq!(net.version.as_deref(), Some("v4.0.30319"));
    assert_eq!(net.evidence[0].offset, 0x200);
}

#[test]
fn pe_detects_msvc() {
    let f = detect(&pe64());
    let msvc = f.iter().find(|f| f.name == "MSVC").unwrap();
    assert_eq!(msvc.version.as_deref(), Some("Visual Studio 2019"));
    assert_eq!(msvc.evidence[0].offset, 0x80);
    assert!(msvc.evidence[0].description.contains("Utc1900_CPP"));
}

#[test]
fn elf64_structure() {
    let data = elf64();
    let info = parse(&data).unwrap();
    assert_eq!(info.format, ExecutableFormat::Elf);
    assert_eq!((info.bits, info.endian), (64, Endian::Little));
    assert_eq!(info.arch, Some(Arch::X64));
    assert_eq!(info.kind, "executable");
    assert_eq!(info.entry_point, Some(0x40_0100));
    assert_eq!(info.entry_offset, Some(0x100));
    assert_eq!(info.image_base(), 0x40_0000);
    assert_eq!(info.segments.len(), 2);
    assert_eq!(info.segments[0].permissions, "r-x");
    let bss = info.sections.iter().find(|s| s.name == ".bss").unwrap();
    assert_eq!(bss.file_size, 0);
    assert_eq!(bss.virtual_size, 0x20);
    assert!(
        info.sections
            .iter()
            .find(|s| s.name == ".text")
            .unwrap()
            .executable
    );
    // VA conversion: .data is file-backed, .bss is not.
    assert_eq!(info.va_to_offset(0x60_0204), Some(0x204));
    assert_eq!(info.va_to_offset(0x60_0218), None);
    assert_eq!(info.offset_to_va(0x105), Some(0x40_0105));
    // Symbols with demangling.
    let bar = info
        .symbols
        .iter()
        .find(|s| s.name == "_ZN3foo3barEv")
        .unwrap();
    assert_eq!(bar.demangled.as_deref(), Some("foo::bar()"));
    assert_eq!(bar.kind, "func");
    assert_eq!(bar.offset, Some(0x110));
    let ext = info.symbols.iter().find(|s| s.name == "ext_fn").unwrap();
    assert!(!ext.defined);
    assert_eq!(info.relocations.total, 2);
    assert!(
        info.relocations
            .by_type
            .iter()
            .any(|r| r.kind.contains("PLT32"))
    );
    assert_eq!(info.relocations.regions[0].offset, 0x2d0);
    assert!(
        info.headers
            .iter()
            .any(|h| h.name == "program header table" && h.offset == 64)
    );
    // Detection from .comment.
    let f = detect(&data);
    let gcc = f.iter().find(|f| f.name == "GCC").unwrap();
    assert_eq!(gcc.version.as_deref(), Some("13.2.0"));
    assert_eq!(gcc.evidence[0].offset, 0x210);
    // Cross references to .data from code.
    let x = find_xrefs(&data, &info, 0x60_0200, 10).unwrap();
    assert_eq!(x.code.len(), 1);
    assert_eq!(x.code[0].kind, XrefKind::Data);
    assert_eq!(x.code[0].from_offset, Some(0x109));
    // Tree input.
    let tree = Tree::from_piece(Piece::bytes(data));
    assert_eq!(parse_tree(&tree).unwrap(), info);
}

#[test]
fn elf32_mips_big_endian() {
    let data = elf32_mips_be();
    let info = parse(&data).unwrap();
    assert_eq!((info.bits, info.endian), (32, Endian::Big));
    assert_eq!(info.arch, Some(Arch::Mips32Be));
    assert_eq!(info.va_to_offset(0x40_0104), Some(0x104));
    let x = find_xrefs(&data, &info, 0x40_0110, 10).unwrap();
    assert_eq!(x.code.len(), 1);
    assert_eq!(x.code[0].kind, XrefKind::Call);
    assert_eq!(x.code[0].from, 0x40_0100);
}

#[test]
fn macho_arm64() {
    let data = macho64(0x0100_000c);
    let info = parse(&data).unwrap();
    assert_eq!(info.format, ExecutableFormat::MachO);
    assert_eq!(info.arch, Some(Arch::Arm64));
    assert_eq!(info.machine, "arm64");
    assert_eq!(info.kind, "executable");
    assert_eq!(info.image_base(), 0x1_0000_0000);
    assert_eq!(info.entry_point, Some(0x1_0000_0400));
    assert_eq!(info.entry_offset, Some(0x400));
    assert_eq!(info.segments.len(), 3);
    assert_eq!(info.segments[1].permissions, "r-x");
    let text = info
        .sections
        .iter()
        .find(|s| s.name == "__TEXT,__text")
        .unwrap();
    assert!(text.executable);
    assert_eq!(text.offset, 0x400);
    assert_eq!(
        info.libraries,
        vec!["/usr/lib/libSystem.B.dylib", "@rpath/libswiftCore.dylib"]
    );
    let swift = info
        .symbols
        .iter()
        .find(|s| s.name == "_$s4main4noopyyF")
        .unwrap();
    assert_eq!(swift.demangled.as_deref(), Some("main.noop() -> ()"));
    assert_eq!(swift.offset, Some(0x408));
    assert!(info.headers.iter().any(|h| h.name == "LC_MAIN"));
    assert_eq!(info.va_to_offset(0x1_0000_0404), Some(0x404));
    assert_eq!(info.va_to_offset(0x10), None);
    let f = detect(&data);
    assert!(
        f.iter()
            .any(|f| f.name == "Swift" && f.confidence == Confidence::High)
    );
    let x = find_xrefs(&data, &info, 0x1_0000_0408, 10).unwrap();
    assert_eq!(x.code[0].kind, XrefKind::Call);
    assert_eq!(x.code[0].text, "bl #0x100000408");
}

#[test]
fn macho_fat() {
    let data = fat();
    let info = parse(&data).unwrap();
    assert_eq!(info.format, ExecutableFormat::MachOFat);
    assert_eq!(info.slices.len(), 2);
    assert_eq!(info.slices[0].arch_name, "arm64");
    assert_eq!(info.slices[1].arch_name, "x86_64");
    let a = info.slices[0].info.as_ref().unwrap();
    assert_eq!(a.arch, Some(Arch::Arm64));
    // Offsets are absolute in the container.
    assert_eq!(a.entry_offset, Some(0x1400));
    assert_eq!(a.va_to_offset(0x1_0000_0404), Some(0x1404));
    let b = info.slices[1].info.as_ref().unwrap();
    assert_eq!(b.arch, Some(Arch::X64));
    assert_eq!(b.entry_offset, Some(0x2400));
    assert_eq!(b.sections[0].offset, 0x2400);
    assert_eq!(&data[0x2400..0x2401], &[0xe8]);
}

#[test]
fn unknown_and_malformed() {
    assert!(parse(b"hello world").is_err());
    let mut pe = pe64();
    pe.truncate(0x100);
    assert!(parse(&pe).is_err());
    assert!(parse(&[0x7f, b'E', b'L', b'F', 2, 1, 1]).is_err());
}
