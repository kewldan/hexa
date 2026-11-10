//! Hand-built executable samples for integration tests.
#![allow(dead_code)]

use hexa_re::Arch;
use hexa_re::asm::assemble;

pub fn put(buf: &mut Vec<u8>, off: usize, bytes: &[u8]) {
    if buf.len() < off + bytes.len() {
        buf.resize(off + bytes.len(), 0);
    }
    buf[off..off + bytes.len()].copy_from_slice(bytes);
}

pub fn p16(buf: &mut Vec<u8>, off: usize, v: u16) {
    put(buf, off, &v.to_le_bytes());
}

pub fn p32(buf: &mut Vec<u8>, off: usize, v: u32) {
    put(buf, off, &v.to_le_bytes());
}

pub fn p64(buf: &mut Vec<u8>, off: usize, v: u64) {
    put(buf, off, &v.to_le_bytes());
}

fn utf16(s: &str) -> Vec<u8> {
    let mut v = (s.len() as u16).to_le_bytes().to_vec();
    for u in s.encode_utf16() {
        v.extend(u.to_le_bytes());
    }
    v
}

/// Rich header block for `prefix` (bytes before the block).
pub fn rich_block(prefix: &[u8], entries: &[(u16, u16, u32)]) -> Vec<u8> {
    let mut csum = prefix.len() as u32;
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
    for v in [0x536e_6144 ^ key, key, key, key] {
        out.extend(v.to_le_bytes());
    }
    for &(pid, build, count) in entries {
        let comp = u32::from(pid) << 16 | u32::from(build);
        out.extend((comp ^ key).to_le_bytes());
        out.extend((count ^ key).to_le_bytes());
    }
    out.extend(0x6863_6952u32.to_le_bytes());
    out.extend(key.to_le_bytes());
    out
}

pub const PE64_BASE: u64 = 0x1_8000_0000;

type SectionSpec = (&'static [u8], u32, u32, u32, u32, u32);

/// A PE32+ x86-64 DLL with imports, exports, resources, base relocations,
/// a Rich header and code referencing a string and the IAT.
pub fn pe64() -> Vec<u8> {
    let mut f = vec![0u8; 0xc00];
    put(&mut f, 0, b"MZ");
    let e_lfanew = 0xb0usize;
    let rich = rich_block(&f[..0x80], &[(0x0105, 30_133, 7), (0x0102, 30_133, 1)]);
    put(&mut f, 0x80, &rich);
    p32(&mut f, 0x3c, e_lfanew as u32);
    put(&mut f, e_lfanew, b"PE\0\0");
    let coff = e_lfanew + 4;
    p16(&mut f, coff, 0x8664);
    p16(&mut f, coff + 2, 4);
    p16(&mut f, coff + 16, 240);
    p16(&mut f, coff + 18, 0x2022);
    let opt = coff + 20;
    p16(&mut f, opt, 0x20b);
    f[opt + 2] = 14;
    p32(&mut f, opt + 4, 0x200);
    p32(&mut f, opt + 16, 0x1000); // entry
    p32(&mut f, opt + 20, 0x1000);
    p64(&mut f, opt + 24, PE64_BASE);
    p32(&mut f, opt + 32, 0x1000);
    p32(&mut f, opt + 36, 0x200);
    p16(&mut f, opt + 40, 6);
    p16(&mut f, opt + 48, 6);
    p32(&mut f, opt + 56, 0x5000);
    p32(&mut f, opt + 60, 0x400);
    p16(&mut f, opt + 68, 3);
    p16(&mut f, opt + 70, 0x8160);
    p64(&mut f, opt + 72, 0x10_0000);
    p64(&mut f, opt + 80, 0x1000);
    p64(&mut f, opt + 88, 0x10_0000);
    p64(&mut f, opt + 96, 0x1000);
    p32(&mut f, opt + 108, 16);
    let dirs = opt + 112;
    let dir = |f: &mut Vec<u8>, i: usize, rva: u32, size: u32| {
        p32(f, dirs + i * 8, rva);
        p32(f, dirs + i * 8 + 4, size);
    };
    dir(&mut f, 0, 0x2140, 0x60);
    dir(&mut f, 1, 0x2000, 0x28);
    dir(&mut f, 2, 0x3000, 0x120);
    dir(&mut f, 5, 0x4000, 0x10);
    dir(&mut f, 12, 0x2080, 0x20);
    let st = opt + 240;
    // (name, virtual size, rva, raw size, raw pointer, characteristics)
    let sections: [SectionSpec; 4] = [
        (b".text", 0x100, 0x1000, 0x200, 0x400, 0x6000_0020),
        (b".rdata", 0x200, 0x2000, 0x200, 0x600, 0x4000_0040),
        (b".rsrc", 0x200, 0x3000, 0x200, 0x800, 0x4000_0040),
        (b".reloc", 0x10, 0x4000, 0x200, 0xa00, 0x4200_0040),
    ];
    for (i, (name, vsize, rva, raw, ptr, ch)) in sections.iter().enumerate() {
        let s = st + i * 40;
        put(&mut f, s, name);
        p32(&mut f, s + 8, *vsize);
        p32(&mut f, s + 12, *rva);
        p32(&mut f, s + 16, *raw);
        p32(&mut f, s + 20, *ptr);
        p32(&mut f, s + 36, *ch);
    }
    // .text
    let code = assemble(
        Arch::X64,
        "sub rsp, 0x28; lea rcx, [rel 0x1800021a0]; call qword ptr [rel 0x180002080]; int3",
        PE64_BASE + 0x1000,
    )
    .unwrap();
    put(&mut f, 0x400, &code);
    put(&mut f, 0x420, &[0xc3]); // exported function at RVA 0x1020
    // .rdata: imports
    let r = |rva: usize| rva - 0x2000 + 0x600;
    p32(&mut f, r(0x2000), 0x2040);
    p32(&mut f, r(0x200c), 0x2100);
    p32(&mut f, r(0x2010), 0x2080);
    for base in [0x2040, 0x2080] {
        p64(&mut f, r(base), 0x2110);
        p64(&mut f, r(base + 8), 0x2120);
        p64(&mut f, r(base + 16), 0x8000_0000_0000_0005);
    }
    put(&mut f, r(0x2100), b"KERNEL32.dll\0");
    put(&mut f, r(0x2112), b"ExitProcess\0");
    put(&mut f, r(0x2122), b"GetStdHandle\0");
    // exports
    let e = r(0x2140);
    p32(&mut f, e + 12, 0x2180);
    p32(&mut f, e + 16, 1);
    p32(&mut f, e + 20, 1);
    p32(&mut f, e + 24, 1);
    p32(&mut f, e + 28, 0x2170);
    p32(&mut f, e + 32, 0x2174);
    p32(&mut f, e + 36, 0x2178);
    p32(&mut f, r(0x2170), 0x1020);
    p32(&mut f, r(0x2174), 0x2190);
    p16(&mut f, r(0x2178), 0);
    put(&mut f, r(0x2180), b"test.dll\0");
    put(&mut f, r(0x2190), b"_ZN3foo3barEv\0");
    put(&mut f, r(0x21a0), b"Hello\0");
    p64(&mut f, r(0x21b0), PE64_BASE + 0x21a0);
    // .rsrc
    let b = 0x800usize;
    // root: 1 named + 1 id
    p16(&mut f, b + 12, 1);
    p16(&mut f, b + 14, 1);
    p32(&mut f, b + 16, 0x8000_0080);
    p32(&mut f, b + 20, 0x8000_0020);
    p32(&mut f, b + 24, 3);
    p32(&mut f, b + 28, 0x8000_0038);
    // MYDATA -> #101
    p16(&mut f, b + 0x20 + 14, 1);
    p32(&mut f, b + 0x20 + 16, 101);
    p32(&mut f, b + 0x20 + 20, 0x8000_0050);
    // ICON -> #1
    p16(&mut f, b + 0x38 + 14, 1);
    p32(&mut f, b + 0x38 + 16, 1);
    p32(&mut f, b + 0x38 + 20, 0x8000_0068);
    // languages
    p16(&mut f, b + 0x50 + 14, 1);
    p32(&mut f, b + 0x50 + 16, 0);
    p32(&mut f, b + 0x50 + 20, 0x90);
    p16(&mut f, b + 0x68 + 14, 1);
    p32(&mut f, b + 0x68 + 16, 1033);
    p32(&mut f, b + 0x68 + 20, 0xa0);
    put(&mut f, b + 0x80, &utf16("MYDATA"));
    p32(&mut f, b + 0x90, 0x3100);
    p32(&mut f, b + 0x94, 5);
    p32(&mut f, b + 0xa0, 0x3110);
    p32(&mut f, b + 0xa4, 16);
    p32(&mut f, b + 0xa8, 1252);
    put(&mut f, b + 0x100, b"hello");
    // .reloc
    p32(&mut f, 0xa00, 0x2000);
    p32(&mut f, 0xa04, 16);
    p16(&mut f, 0xa08, 0xa000 | 0x1b0);
    p16(&mut f, 0xa0a, 0xa000 | 0x080);
    p16(&mut f, 0xa0c, 0xa000 | 0x088);
    p16(&mut f, 0xa0e, 0);
    f
}

/// A PE32 i386 image with UPX section names, a CLR header and .NET
/// metadata.
pub fn pe32_upx_dotnet() -> Vec<u8> {
    let mut f = vec![0u8; 0x400];
    put(&mut f, 0, b"MZ");
    p32(&mut f, 0x3c, 0x80);
    put(&mut f, 0x80, b"PE\0\0");
    let coff = 0x84;
    p16(&mut f, coff, 0x14c);
    p16(&mut f, coff + 2, 2);
    p16(&mut f, coff + 16, 224);
    p16(&mut f, coff + 18, 0x0102);
    let opt = coff + 20;
    p16(&mut f, opt, 0x10b);
    p32(&mut f, opt + 16, 0x2100);
    p32(&mut f, opt + 28, 0x40_0000);
    p32(&mut f, opt + 32, 0x1000);
    p32(&mut f, opt + 36, 0x200);
    p32(&mut f, opt + 56, 0x3000);
    p32(&mut f, opt + 60, 0x200);
    p16(&mut f, opt + 68, 2);
    p32(&mut f, opt + 92, 16);
    let dirs = opt + 96;
    p32(&mut f, dirs + 14 * 8, 0x2000);
    p32(&mut f, dirs + 14 * 8 + 4, 0x48);
    let st = opt + 224;
    put(&mut f, st, b"UPX0");
    p32(&mut f, st + 8, 0x1000);
    p32(&mut f, st + 12, 0x1000);
    p32(&mut f, st + 36, 0xe000_0080);
    put(&mut f, st + 40, b"UPX1");
    p32(&mut f, st + 48, 0x1000);
    p32(&mut f, st + 52, 0x2000);
    p32(&mut f, st + 56, 0x200);
    p32(&mut f, st + 60, 0x200);
    p32(&mut f, st + 76, 0xe000_0040);
    p32(&mut f, 0x200, 0x48);
    put(&mut f, 0x250, b"BSJB");
    p16(&mut f, 0x254, 1);
    p16(&mut f, 0x256, 1);
    p32(&mut f, 0x25c, 12);
    put(&mut f, 0x260, b"v4.0.30319\0\0");
    put(&mut f, 0x300, b"$Id: UPX 4.22 Copyright (C) 1996-2024\0");
    f
}

pub struct ESec {
    pub name: &'static str,
    pub sh_type: u32,
    pub flags: u64,
    pub addr: u64,
    pub offset: u64,
    pub data: Vec<u8>,
    pub size: u64,
    pub link: u32,
    pub info: u32,
    pub entsize: u64,
}

impl ESec {
    pub fn new(
        name: &'static str,
        sh_type: u32,
        flags: u64,
        addr: u64,
        offset: u64,
        data: Vec<u8>,
    ) -> Self {
        let size = data.len() as u64;
        ESec {
            name,
            sh_type,
            flags,
            addr,
            offset,
            data,
            size,
            link: 0,
            info: 0,
            entsize: 0,
        }
    }
}

pub struct Phdr {
    pub p_type: u32,
    pub flags: u32,
    pub offset: u64,
    pub vaddr: u64,
    pub filesz: u64,
    pub memsz: u64,
}

pub struct ElfWriter {
    pub is64: bool,
    pub le: bool,
    buf: Vec<u8>,
}

impl ElfWriter {
    fn w(&mut self, off: usize, v: u64, n: usize) {
        let bytes = if self.le {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        };
        let slice = if self.le {
            &bytes[..n]
        } else {
            &bytes[8 - n..]
        };
        let s = slice.to_vec();
        put(&mut self.buf, off, &s);
    }

    fn addr(&mut self, off: usize, v: u64) {
        let n = if self.is64 { 8 } else { 4 };
        self.w(off, v, n);
    }
}

/// Generic ELF writer: sections are placed at their given offsets; the
/// section name table and section headers are appended.
pub fn build_elf(
    is64: bool,
    le: bool,
    machine: u16,
    e_type: u16,
    entry: u64,
    phdrs: &[Phdr],
    sections: &[ESec],
) -> Vec<u8> {
    let mut e = ElfWriter {
        is64,
        le,
        buf: Vec::new(),
    };
    let ehsize = if is64 { 64 } else { 52 };
    let phentsize = if is64 { 56 } else { 32 };
    let shentsize = if is64 { 64 } else { 40 };
    for s in sections {
        put(&mut e.buf, s.offset as usize, &s.data);
    }
    // .shstrtab
    let mut shstr = vec![0u8];
    let mut names = Vec::new();
    for s in sections {
        names.push(shstr.len() as u32);
        shstr.extend(s.name.as_bytes());
        shstr.push(0);
    }
    let shstr_name = shstr.len() as u32;
    shstr.extend(b".shstrtab\0");
    let shstr_off = (e.buf.len().max(ehsize + phentsize * phdrs.len()) + 7) & !7;
    put(&mut e.buf, shstr_off, &shstr);
    let shoff = (shstr_off + shstr.len() + 7) & !7;
    let shnum = sections.len() + 2;
    e.buf.resize(shoff + shnum * shentsize, 0);
    // ELF header
    put(
        &mut e.buf,
        0,
        &[
            0x7f,
            b'E',
            b'L',
            b'F',
            if is64 { 2 } else { 1 },
            if le { 1 } else { 2 },
            1,
        ],
    );
    e.w(16, u64::from(e_type), 2);
    e.w(18, u64::from(machine), 2);
    e.w(20, 1, 4);
    e.addr(24, entry);
    let (phoff_at, shoff_at, rest) = if is64 { (32, 40, 48) } else { (28, 32, 36) };
    e.addr(phoff_at, if phdrs.is_empty() { 0 } else { ehsize as u64 });
    e.addr(shoff_at, shoff as u64);
    e.w(rest + 4, ehsize as u64, 2);
    e.w(rest + 6, phentsize as u64, 2);
    e.w(rest + 8, phdrs.len() as u64, 2);
    e.w(rest + 10, shentsize as u64, 2);
    e.w(rest + 12, shnum as u64, 2);
    e.w(rest + 14, (shnum - 1) as u64, 2);
    for (i, p) in phdrs.iter().enumerate() {
        let o = ehsize + i * phentsize;
        e.w(o, u64::from(p.p_type), 4);
        if is64 {
            e.w(o + 4, u64::from(p.flags), 4);
            e.w(o + 8, p.offset, 8);
            e.w(o + 16, p.vaddr, 8);
            e.w(o + 24, p.vaddr, 8);
            e.w(o + 32, p.filesz, 8);
            e.w(o + 40, p.memsz, 8);
            e.w(o + 48, 0x1000, 8);
        } else {
            e.w(o + 4, p.offset, 4);
            e.w(o + 8, p.vaddr, 4);
            e.w(o + 12, p.vaddr, 4);
            e.w(o + 16, p.filesz, 4);
            e.w(o + 20, p.memsz, 4);
            e.w(o + 24, u64::from(p.flags), 4);
            e.w(o + 28, 0x1000, 4);
        }
    }
    let sh =
        |e: &mut ElfWriter, idx: usize, name: u32, s: (u32, u64, u64, u64, u64, u32, u32, u64)| {
            let o = shoff + idx * shentsize;
            let (ty, flags, addr, off, size, link, info, entsize) = s;
            e.w(o, u64::from(name), 4);
            e.w(o + 4, u64::from(ty), 4);
            if is64 {
                e.w(o + 8, flags, 8);
                e.w(o + 16, addr, 8);
                e.w(o + 24, off, 8);
                e.w(o + 32, size, 8);
                e.w(o + 40, u64::from(link), 4);
                e.w(o + 44, u64::from(info), 4);
                e.w(o + 48, 8, 8);
                e.w(o + 56, entsize, 8);
            } else {
                e.w(o + 8, flags, 4);
                e.w(o + 12, addr, 4);
                e.w(o + 16, off, 4);
                e.w(o + 20, size, 4);
                e.w(o + 24, u64::from(link), 4);
                e.w(o + 28, u64::from(info), 4);
                e.w(o + 32, 4, 4);
                e.w(o + 36, entsize, 4);
            }
        };
    for (i, s) in sections.iter().enumerate() {
        sh(
            &mut e,
            i + 1,
            names[i],
            (
                s.sh_type, s.flags, s.addr, s.offset, s.size, s.link, s.info, s.entsize,
            ),
        );
    }
    sh(
        &mut e,
        shnum - 1,
        shstr_name,
        (3, 0, 0, shstr_off as u64, shstr.len() as u64, 0, 0, 0),
    );
    e.buf
}

pub fn elf_sym64(name: u32, info: u8, shndx: u16, value: u64, size: u64) -> Vec<u8> {
    let mut v = name.to_le_bytes().to_vec();
    v.push(info);
    v.push(0);
    v.extend(shndx.to_le_bytes());
    v.extend(value.to_le_bytes());
    v.extend(size.to_le_bytes());
    v
}

/// x86-64 executable with .text/.data/.bss/.comment/.symtab/.rela.text.
pub fn elf64() -> Vec<u8> {
    let code = assemble(
        Arch::X64,
        "push rbp; mov rbp, rsp; call 0x400110; mov eax, dword ptr [rel 0x600200]; pop rbp; ret",
        0x40_0100,
    )
    .unwrap();
    let mut text = code;
    text.resize(0x10, 0x90);
    text.extend([0xc3; 0x10]);
    let strtab = b"\0main\0counter\0_ZN3foo3barEv\0ext_fn\0".to_vec();
    let mut symtab = vec![0u8; 24];
    symtab.extend(elf_sym64(1, 0x12, 1, 0x40_0100, 0x10));
    symtab.extend(elf_sym64(6, 0x11, 2, 0x60_0200, 4));
    symtab.extend(elf_sym64(14, 0x12, 1, 0x40_0110, 0x10));
    symtab.extend(elf_sym64(28, 0x10, 0, 0, 0));
    let mut rela = Vec::new();
    for (off, sym, ty) in [(0x40_0105u64, 4u64, 4u64), (0x40_010cu64, 2, 2)] {
        rela.extend(off.to_le_bytes());
        rela.extend((sym << 32 | ty).to_le_bytes());
        rela.extend((-4i64).to_le_bytes());
    }
    let mut secs = vec![
        ESec::new(".text", 1, 6, 0x40_0100, 0x100, text),
        ESec::new(
            ".data",
            1,
            3,
            0x60_0200,
            0x200,
            vec![1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        ),
        ESec {
            size: 0x20,
            ..ESec::new(".bss", 8, 3, 0x60_0210, 0x210, Vec::new())
        },
        ESec::new(
            ".comment",
            1,
            0x30,
            0,
            0x210,
            b"GCC: (GNU) 13.2.0\0".to_vec(),
        ),
        ESec {
            link: 6,
            info: 1,
            entsize: 24,
            ..ESec::new(".symtab", 2, 0, 0, 0x228, symtab)
        },
        ESec::new(".strtab", 3, 0, 0, 0x2a0, strtab),
        ESec {
            link: 5,
            info: 1,
            entsize: 24,
            ..ESec::new(".rela.text", 4, 0x40, 0, 0x2d0, rela)
        },
    ];
    secs[3].flags = 0x30;
    let phdrs = [
        Phdr {
            p_type: 1,
            flags: 5,
            offset: 0,
            vaddr: 0x40_0000,
            filesz: 0x200,
            memsz: 0x200,
        },
        Phdr {
            p_type: 1,
            flags: 6,
            offset: 0x200,
            vaddr: 0x60_0200,
            filesz: 0x10,
            memsz: 0x30,
        },
    ];
    build_elf(true, true, 62, 2, 0x40_0100, &phdrs, &secs)
}

/// MIPS32 big-endian executable.
pub fn elf32_mips_be() -> Vec<u8> {
    let mut text = Vec::new();
    for w in [0x0c10_0044u32, 0x0000_0000, 0x03e0_0008, 0x0000_0000] {
        text.extend(w.to_be_bytes());
    }
    let secs = [ESec::new(".text", 1, 6, 0x40_0100, 0x100, text)];
    let phdrs = [Phdr {
        p_type: 1,
        flags: 5,
        offset: 0,
        vaddr: 0x40_0000,
        filesz: 0x110,
        memsz: 0x110,
    }];
    build_elf(false, false, 8, 2, 0x40_0100, &phdrs, &secs)
}

fn seg64(
    name: &str,
    vmaddr: u64,
    vmsize: u64,
    fileoff: u64,
    filesize: u64,
    prot: u32,
    sects: &[(&str, u64, u64, u32, u32)],
) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend(0x19u32.to_le_bytes());
    v.extend((72 + 80 * sects.len() as u32).to_le_bytes());
    let mut n = [0u8; 16];
    n[..name.len()].copy_from_slice(name.as_bytes());
    v.extend(n);
    for x in [vmaddr, vmsize, fileoff, filesize] {
        v.extend(x.to_le_bytes());
    }
    v.extend(prot.to_le_bytes());
    v.extend(prot.to_le_bytes());
    v.extend((sects.len() as u32).to_le_bytes());
    v.extend(0u32.to_le_bytes());
    for (sname, addr, size, offset, flags) in sects {
        let mut s = [0u8; 16];
        s[..sname.len()].copy_from_slice(sname.as_bytes());
        v.extend(s);
        v.extend(n);
        v.extend(addr.to_le_bytes());
        v.extend(size.to_le_bytes());
        v.extend(offset.to_le_bytes());
        v.extend(2u32.to_le_bytes());
        v.extend([0u8; 8]);
        v.extend(flags.to_le_bytes());
        v.extend([0u8; 12]);
    }
    v
}

fn dylib_cmd(path: &str) -> Vec<u8> {
    let mut name = path.as_bytes().to_vec();
    name.push(0);
    while !(24 + name.len()).is_multiple_of(8) {
        name.push(0);
    }
    let mut v = Vec::new();
    v.extend(0xcu32.to_le_bytes());
    v.extend(((24 + name.len()) as u32).to_le_bytes());
    v.extend(24u32.to_le_bytes());
    v.extend(2u32.to_le_bytes());
    v.extend(0x0001_0000u32.to_le_bytes());
    v.extend(0x0001_0000u32.to_le_bytes());
    v.extend(name);
    v
}

/// 64-bit Mach-O executable (`cputype` arm64 or `x86_64`).
pub fn macho64(cputype: u32) -> Vec<u8> {
    let text_addr = 0x1_0000_0400u64;
    let code: Vec<u8> = if cputype == 0x0100_000c {
        assemble(Arch::Arm64, "bl 0x100000408; ret; nop; ret", text_addr).unwrap()
    } else {
        assemble(Arch::X64, "call 0x100000408; ret; nop; nop; ret", text_addr).unwrap()
    };
    let mut cmds = Vec::new();
    cmds.push(seg64("__PAGEZERO", 0, 0x1_0000_0000, 0, 0, 0, &[]));
    cmds.push(seg64(
        "__TEXT",
        0x1_0000_0000,
        0x4000,
        0,
        0x800,
        5,
        &[("__text", text_addr, 0x10, 0x400, 0x8000_0400)],
    ));
    cmds.push(seg64(
        "__LINKEDIT",
        0x1_0000_4000,
        0x4000,
        0x800,
        0x100,
        1,
        &[],
    ));
    let mut symtab = Vec::new();
    symtab.extend(2u32.to_le_bytes());
    symtab.extend(24u32.to_le_bytes());
    symtab.extend(0x800u32.to_le_bytes());
    symtab.extend(2u32.to_le_bytes());
    symtab.extend(0x820u32.to_le_bytes());
    symtab.extend(0x40u32.to_le_bytes());
    cmds.push(symtab);
    let mut main = Vec::new();
    main.extend(0x8000_0028u32.to_le_bytes());
    main.extend(24u32.to_le_bytes());
    main.extend(0x400u64.to_le_bytes());
    main.extend(0u64.to_le_bytes());
    cmds.push(main);
    cmds.push(dylib_cmd("/usr/lib/libSystem.B.dylib"));
    cmds.push(dylib_cmd("@rpath/libswiftCore.dylib"));
    let sizeofcmds: usize = cmds.iter().map(Vec::len).sum();
    let mut f = Vec::new();
    for v in [
        0xfeed_facfu32,
        cputype,
        0,
        2,
        cmds.len() as u32,
        sizeofcmds as u32,
        0x0020_0085,
        0,
    ] {
        f.extend(v.to_le_bytes());
    }
    for c in cmds {
        f.extend(c);
    }
    put(&mut f, 0x400, &code);
    // symbols
    let strtab = b"\0_main\0_$s4main4noopyyF\0";
    f.resize(0x900, 0);
    let nl = |off: usize, strx: u32, value: u64, f: &mut Vec<u8>| {
        p32(f, off, strx);
        f[off + 4] = 0x0f;
        f[off + 5] = 1;
        p64(f, off + 8, value);
    };
    nl(0x800, 1, text_addr, &mut f);
    nl(0x810, 7, text_addr + 8, &mut f);
    put(&mut f, 0x820, strtab);
    f.resize(0x900, 0);
    f
}

/// Universal binary with arm64 and `x86_64` slices at 0x1000 and 0x2000.
pub fn fat() -> Vec<u8> {
    let a = macho64(0x0100_000c);
    let b = macho64(0x0100_0007);
    let mut f = Vec::new();
    f.extend(0xcafe_babeu32.to_be_bytes());
    f.extend(2u32.to_be_bytes());
    for (cpu, sub, off, len) in [
        (0x0100_000cu32, 0u32, 0x1000u32, a.len()),
        (0x0100_0007, 3, 0x2000, b.len()),
    ] {
        for v in [cpu, sub, off, len as u32, 12] {
            f.extend(v.to_be_bytes());
        }
    }
    put(&mut f, 0x1000, &a);
    put(&mut f, 0x2000, &b);
    f
}
