//! Compiler, packer, protector and runtime detection (RE-07).
//!
//! Detection combines a data-driven signature table ([`SIGNATURES`]:
//! strings searched in one Aho-Corasick pass, section names and imported
//! libraries) with structural decoders: the MSVC Rich header (product ids
//! and Visual Studio release), Go `buildinfo`, `PyInstaller`'s cookie,
//! the .NET CLR header and metadata version, Java class / JAR files, and
//! version strings such as `GCC: (GNU) 13.2.0` or `clang version 17`.
//! Every finding carries the file offsets of its evidence.

use std::collections::BTreeMap;

use aho_corasick::AhoCorasick;
use hexa_core::Tree;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::exe::{ExecutableFormat, ExecutableInfo, parse};

/// Category of a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Category {
    /// Compiler / toolchain.
    Compiler,
    /// Linker.
    Linker,
    /// Executable packer / compressor.
    Packer,
    /// Protector / virtualiser.
    Protector,
    /// Language runtime or framework.
    Runtime,
    /// Installer or self-extracting archive.
    Installer,
    /// Bundled script / application packager.
    Bundler,
}

/// Confidence of a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Confidence {
    /// Weak hint.
    Low,
    /// Typical but not exclusive marker.
    Medium,
    /// Structural or unique marker.
    High,
}

/// Where a finding was seen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Evidence {
    /// File offset.
    pub offset: u64,
    /// Length of the evidence bytes.
    pub length: u64,
    /// What was matched.
    pub description: String,
}

/// One detected tool / runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    /// Name (e.g. `UPX`, `Go`, `MSVC`).
    pub name: String,
    /// Category.
    pub category: Category,
    /// Version, when derivable.
    pub version: Option<String>,
    /// Highest confidence among the evidence.
    pub confidence: Confidence,
    /// Evidence (at most [`MAX_EVIDENCE`] entries).
    pub evidence: Vec<Evidence>,
}

/// Maximum evidence entries kept per finding.
pub const MAX_EVIDENCE: usize = 8;

/// What a signature matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SigKind {
    /// Byte string anywhere in the file.
    Bytes,
    /// Exact section name.
    Section,
    /// Imported library (case-insensitive prefix).
    Library,
}

/// A signature table entry.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Signature {
    /// Finding name.
    pub name: &'static str,
    /// Category.
    pub category: Category,
    /// Match kind.
    pub kind: SigKind,
    /// Pattern.
    #[serde(serialize_with = "ser_pattern")]
    pub pattern: &'static [u8],
    /// Confidence.
    pub confidence: Confidence,
    /// Fixed version reported by this signature.
    pub version: Option<&'static str>,
}

fn ser_pattern<S: serde::Serializer>(
    p: &&'static [u8],
    s: S,
) -> std::result::Result<S::Ok, S::Error> {
    s.serialize_str(&String::from_utf8_lossy(p))
}

const fn sig(
    name: &'static str,
    category: Category,
    kind: SigKind,
    pattern: &'static [u8],
    confidence: Confidence,
) -> Signature {
    Signature {
        name,
        category,
        kind,
        pattern,
        confidence,
        version: None,
    }
}

const fn lib(name: &'static str, pattern: &'static [u8], version: &'static str) -> Signature {
    Signature {
        name,
        category: Category::Compiler,
        kind: SigKind::Library,
        pattern,
        confidence: Confidence::Medium,
        version: Some(version),
    }
}

use Category as C;
use Confidence as F;
use SigKind as K;

/// The signature table (RE-07).
pub const SIGNATURES: &[Signature] = &[
    // Packers
    sig("UPX", C::Packer, K::Section, b"UPX0", F::High),
    sig("UPX", C::Packer, K::Section, b"UPX1", F::High),
    sig("UPX", C::Packer, K::Bytes, b"UPX!", F::Medium),
    sig(
        "UPX",
        C::Packer,
        K::Bytes,
        b"$Info: This file is packed with the UPX executable packer",
        F::High,
    ),
    sig("UPX", C::Packer, K::Bytes, b"$Id: UPX ", F::High),
    sig("MPRESS", C::Packer, K::Section, b".MPRESS1", F::High),
    sig("MPRESS", C::Packer, K::Section, b".MPRESS2", F::High),
    sig("ASPack", C::Packer, K::Section, b".aspack", F::High),
    sig("ASPack", C::Packer, K::Section, b".adata", F::Low),
    sig("PECompact", C::Packer, K::Section, b"PEC2", F::Medium),
    sig("PECompact", C::Packer, K::Bytes, b"PECompact2", F::High),
    sig("Petite", C::Packer, K::Section, b".petite", F::High),
    sig("FSG", C::Packer, K::Bytes, b"FSG!", F::Medium),
    sig("NsPack", C::Packer, K::Section, b".nsp0", F::High),
    // Protectors
    sig(
        "Themida / WinLicense",
        C::Protector,
        K::Section,
        b".themida",
        F::High,
    ),
    sig(
        "Themida / WinLicense",
        C::Protector,
        K::Section,
        b".winlice",
        F::High,
    ),
    sig("VMProtect", C::Protector, K::Section, b".vmp0", F::High),
    sig("VMProtect", C::Protector, K::Section, b".vmp1", F::High),
    sig("VMProtect", C::Protector, K::Section, b".vmp2", F::High),
    sig(
        "Enigma Protector",
        C::Protector,
        K::Section,
        b".enigma1",
        F::High,
    ),
    sig(
        "Enigma Protector",
        C::Protector,
        K::Section,
        b".enigma2",
        F::High,
    ),
    sig("Obsidium", C::Protector, K::Bytes, b"Obsidium", F::Medium),
    sig("Armadillo", C::Protector, K::Bytes, b"Armadillo", F::Low),
    // Bundlers
    sig(
        "PyInstaller",
        C::Bundler,
        K::Bytes,
        b"MEI\x0c\x0b\x0a\x0b\x0e",
        F::High,
    ),
    sig(
        "PyInstaller",
        C::Bundler,
        K::Bytes,
        b"pyi-runtime-tmpdir",
        F::High,
    ),
    sig("PyInstaller", C::Bundler, K::Bytes, b"_MEIPASS", F::Medium),
    sig(
        "Nuitka",
        C::Bundler,
        K::Bytes,
        b"NUITKA_ONEFILE_PARENT",
        F::High,
    ),
    sig(
        "Nuitka",
        C::Bundler,
        K::Bytes,
        b"__nuitka_binary_dir",
        F::High,
    ),
    sig("py2exe", C::Bundler, K::Bytes, b"PY2EXE_VERBOSE", F::High),
    sig("cx_Freeze", C::Bundler, K::Bytes, b"cx_Freeze", F::Medium),
    sig("Electron", C::Bundler, K::Bytes, b"electron.asar", F::High),
    sig(
        "Electron",
        C::Bundler,
        K::Library,
        b"Electron Framework",
        F::High,
    ),
    sig(
        "Node.js single executable",
        C::Bundler,
        K::Bytes,
        b"NODE_SEA_FUSE_fce680ab2cc467b6e072b8b5df1996b2",
        F::High,
    ),
    sig(
        "Node.js single executable",
        C::Bundler,
        K::Bytes,
        b"NODE_SEA_BLOB",
        F::High,
    ),
    sig(
        ".NET single-file bundle",
        C::Bundler,
        K::Bytes,
        b"DOTNET_BUNDLE_EXTRACT_BASE_DIR",
        F::High,
    ),
    sig("Launch4j", C::Bundler, K::Bytes, b"launch4j", F::High),
    sig("AutoIt", C::Bundler, K::Bytes, b"AU3!EA06", F::High),
    sig(
        "AutoIt",
        C::Bundler,
        K::Bytes,
        b">>>AUTOIT SCRIPT<<<",
        F::High,
    ),
    sig(
        "AutoHotkey",
        C::Bundler,
        K::Bytes,
        b">AUTOHOTKEY SCRIPT<",
        F::High,
    ),
    // Installers
    sig(
        "Inno Setup",
        C::Installer,
        K::Bytes,
        b"Inno Setup Setup Data",
        F::High,
    ),
    sig("NSIS", C::Installer, K::Bytes, b"Nullsoft.NSIS", F::High),
    sig("NSIS", C::Installer, K::Bytes, b"NullsoftInst", F::High),
    sig("WiX Burn", C::Installer, K::Section, b".wixburn", F::High),
    sig(
        "InstallShield",
        C::Installer,
        K::Bytes,
        b"InstallShield",
        F::Medium,
    ),
    sig(
        "7-Zip SFX",
        C::Installer,
        K::Bytes,
        b"7z\xbc\xaf\x27\x1c",
        F::Medium,
    ),
    sig(
        "RAR SFX",
        C::Installer,
        K::Bytes,
        b"Rar!\x1a\x07",
        F::Medium,
    ),
    // Runtimes / languages
    sig("Go", C::Runtime, K::Bytes, b"\xff Go buildinf:", F::High),
    sig("Go", C::Runtime, K::Bytes, b"Go build ID: \"", F::High),
    sig("Go", C::Runtime, K::Bytes, b"runtime.gopanic", F::Medium),
    sig("Go", C::Runtime, K::Section, b".gopclntab", F::High),
    sig("Go", C::Runtime, K::Section, b"__TEXT,__gopclntab", F::High),
    sig("Go", C::Runtime, K::Section, b".go.buildinfo", F::High),
    sig("Rust", C::Runtime, K::Bytes, b"/rustc/", F::High),
    sig("Rust", C::Runtime, K::Bytes, b"\\rustc\\", F::High),
    sig("Rust", C::Runtime, K::Bytes, b"rustc version ", F::High),
    sig(
        "Rust",
        C::Runtime,
        K::Bytes,
        b"called `Option::unwrap()` on a `None` value",
        F::High,
    ),
    sig("Rust", C::Runtime, K::Bytes, b"RUST_BACKTRACE", F::Medium),
    sig("Rust", C::Runtime, K::Bytes, b".cargo/registry", F::Medium),
    sig(".NET", C::Runtime, K::Library, b"mscoree.dll", F::High),
    sig(
        "Delphi",
        C::Compiler,
        K::Bytes,
        b"SOFTWARE\\Borland\\Delphi",
        F::High,
    ),
    sig(
        "Delphi",
        C::Compiler,
        K::Bytes,
        b"SOFTWARE\\Embarcadero",
        F::Medium,
    ),
    sig("Delphi", C::Compiler, K::Library, b"borlndmm", F::High),
    sig("Free Pascal", C::Compiler, K::Bytes, b"FPC 2.", F::Medium),
    sig("Free Pascal", C::Compiler, K::Bytes, b"FPC 3.", F::Medium),
    sig(
        "Visual Basic 6",
        C::Runtime,
        K::Library,
        b"msvbvm60",
        F::High,
    ),
    sig(
        "Swift",
        C::Runtime,
        K::Section,
        b"__TEXT,__swift5_typeref",
        F::High,
    ),
    sig(
        "Swift",
        C::Runtime,
        K::Section,
        b"__TEXT,__swift5_proto",
        F::High,
    ),
    sig("Swift", C::Runtime, K::Section, b"swift5_typeref", F::High),
    sig(
        "Swift",
        C::Runtime,
        K::Library,
        b"/usr/lib/swift/libswiftCore",
        F::High,
    ),
    sig(
        "Swift",
        C::Runtime,
        K::Library,
        b"@rpath/libswiftCore",
        F::High,
    ),
    sig("Swift", C::Runtime, K::Library, b"libswiftCore", F::High),
    sig(
        "Objective-C",
        C::Runtime,
        K::Section,
        b"__DATA,__objc_classlist",
        F::Medium,
    ),
    sig(
        "Objective-C",
        C::Runtime,
        K::Section,
        b"__DATA_CONST,__objc_classlist",
        F::Medium,
    ),
    sig(
        "Objective-C",
        C::Runtime,
        K::Library,
        b"/usr/lib/libobjc",
        F::Medium,
    ),
    sig("Qt", C::Runtime, K::Library, b"qt5core", F::High),
    sig("Qt", C::Runtime, K::Library, b"qt6core", F::High),
    sig("Qt", C::Runtime, K::Library, b"libqt5core", F::High),
    sig("Qt", C::Runtime, K::Library, b"libqt6core", F::High),
    sig("Qt", C::Runtime, K::Bytes, b"qt_version_tag", F::Medium),
    // Compilers / linkers
    sig("GCC", C::Compiler, K::Bytes, b"GCC: (", F::High),
    sig(
        "MinGW-w64",
        C::Compiler,
        K::Bytes,
        b"Mingw-w64 runtime failure",
        F::High,
    ),
    sig("MinGW-w64", C::Compiler, K::Library, b"libgcc_s_", F::High),
    sig(
        "MinGW-w64",
        C::Compiler,
        K::Library,
        b"libstdc++-6.dll",
        F::Medium,
    ),
    sig("Clang", C::Compiler, K::Bytes, b"clang version ", F::High),
    sig("LLD", C::Linker, K::Bytes, b"Linker: LLD ", F::High),
    sig(
        "Open Watcom",
        C::Compiler,
        K::Bytes,
        b"WATCOM C/C++",
        F::High,
    ),
    sig(
        "Intel C++",
        C::Compiler,
        K::Bytes,
        b"Intel(R) C++ Compiler",
        F::Medium,
    ),
    sig("PureBasic", C::Compiler, K::Bytes, b"PureBasic", F::Medium),
    sig("Zig", C::Compiler, K::Bytes, b"zig_panic", F::Low),
    lib("MSVC", b"vcruntime140", "Visual Studio 2015+ runtime"),
    lib("MSVC", b"msvcp140", "Visual Studio 2015+ runtime"),
    lib("MSVC", b"msvcr120", "Visual Studio 2013 runtime"),
    lib("MSVC", b"msvcr110", "Visual Studio 2012 runtime"),
    lib("MSVC", b"msvcr100", "Visual Studio 2010 runtime"),
    lib("MSVC", b"msvcr90", "Visual Studio 2008 runtime"),
    lib("MSVC", b"msvcr80", "Visual Studio 2005 runtime"),
    lib("MSVC", b"msvcr71", "Visual Studio .NET 2003 runtime"),
];

struct Collector {
    findings: BTreeMap<String, Finding>,
}

impl Collector {
    fn add(
        &mut self,
        name: &str,
        category: Category,
        confidence: Confidence,
        version: Option<String>,
        ev: Evidence,
    ) {
        let f = self
            .findings
            .entry(name.to_string())
            .or_insert_with(|| Finding {
                name: name.to_string(),
                category,
                version: None,
                confidence,
                evidence: Vec::new(),
            });
        f.confidence = f.confidence.max(confidence);
        if f.version.is_none() {
            f.version = version;
        }
        if f.evidence.len() < MAX_EVIDENCE && !f.evidence.contains(&ev) {
            f.evidence.push(ev);
        }
    }
}

/// Printable text starting at `off` (stops at NUL / control characters).
fn text_at(data: &[u8], off: usize, max: usize) -> String {
    let tail = data.get(off..).unwrap_or(&[]);
    let end = tail
        .iter()
        .take(max)
        .position(|&c| !(0x20..0x7f).contains(&c))
        .unwrap_or(tail.len().min(max));
    String::from_utf8_lossy(&tail[..end]).trim().to_string()
}

fn version_after(data: &[u8], off: usize, prefix_len: usize) -> Option<String> {
    let t = text_at(data, off + prefix_len, 80);
    let v: String = t
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_end_matches([',', ')'])
        .to_string();
    (!v.is_empty() && v.chars().next().is_some_and(|c| c.is_ascii_digit())).then_some(v)
}

fn uvarint(data: &[u8], mut pos: usize) -> Option<(u64, usize)> {
    let mut v = 0u64;
    let mut shift = 0;
    loop {
        let b = *data.get(pos)?;
        pos += 1;
        v |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Some((v, pos));
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
}

/// Decodes the Go version from a `\xff Go buildinf:` header.
fn go_version(data: &[u8], off: usize, info: Option<&ExecutableInfo>) -> Option<String> {
    let ptr_size = usize::from(*data.get(off + 14)?);
    let flags = *data.get(off + 15)?;
    if flags & 2 != 0 {
        let (len, p) = uvarint(data, off + 32)?;
        let s = data.get(p..p + usize::try_from(len).ok()?)?;
        return Some(String::from_utf8_lossy(s).into_owned());
    }
    // Go < 1.18: pointer to a string header {data, len}.
    let info = info?;
    let big = flags & 1 != 0;
    let rd = |o: usize, n: usize| -> Option<u64> {
        let b = data.get(o..o + n)?;
        let mut v = 0u64;
        for i in 0..n {
            let byte = if big { b[i] } else { b[n - 1 - i] };
            v = v << 8 | u64::from(byte);
        }
        Some(v)
    };
    if ptr_size != 4 && ptr_size != 8 {
        return None;
    }
    let hdr = info.va_to_offset(rd(off + 16, ptr_size)?)? as usize;
    let str_ptr = rd(hdr, ptr_size)?;
    let len = usize::try_from(rd(hdr + ptr_size, ptr_size)?).ok()?;
    let s = info.va_to_offset(str_ptr)? as usize;
    if len >= 64 {
        return None;
    }
    Some(String::from_utf8_lossy(data.get(s..s + len)?).into_owned())
}

fn pyinstaller_version(data: &[u8], off: usize) -> Option<String> {
    let v = u32::from_be_bytes(data.get(off + 20..off + 24)?.try_into().ok()?);
    let (maj, min) = if v >= 100 {
        (v / 100, v % 100)
    } else {
        (v / 10, v % 10)
    };
    (maj == 2 || maj == 3).then(|| format!("Python {maj}.{min}"))
}

/// Detects compilers, packers, protectors and runtimes (RE-07).
pub fn detect(data: &[u8]) -> Vec<Finding> {
    let mut c = Collector {
        findings: BTreeMap::new(),
    };
    let info = parse(data).ok();

    // One pass over the bytes for every string signature.
    let byte_sigs: Vec<&Signature> = SIGNATURES
        .iter()
        .filter(|s| s.kind == SigKind::Bytes)
        .collect();
    if let Ok(ac) = AhoCorasick::new(byte_sigs.iter().map(|s| s.pattern)) {
        let mut seen = vec![0usize; byte_sigs.len()];
        for m in ac.find_overlapping_iter(data) {
            let idx = m.pattern().as_usize();
            seen[idx] += 1;
            if seen[idx] > MAX_EVIDENCE {
                continue;
            }
            let s = byte_sigs[idx];
            let off = m.start();
            let version = match s.pattern {
                b"$Id: UPX " => version_after(data, off, 9),
                b"GCC: (" => {
                    let t = text_at(data, off + 5, 120);
                    t.rsplit(' ').next().map(str::to_string)
                }
                b"clang version " | b"rustc version " => version_after(data, off, 14),
                b"Linker: LLD " => version_after(data, off, 12),
                b"FPC 2." | b"FPC 3." => version_after(data, off, 4),
                b"/rustc/" | b"\\rustc\\" => {
                    let h = text_at(data, off + 7, 40);
                    (h.len() == 40 && h.bytes().all(|b| b.is_ascii_hexdigit()))
                        .then(|| format!("rustc commit {}", &h[..10]))
                }
                b"\xff Go buildinf:" => go_version(data, off, info.as_ref()),
                b"MEI\x0c\x0b\x0a\x0b\x0e" => pyinstaller_version(data, off),
                _ => None,
            };
            if s.pattern == b"/rustc/".as_slice() && version.is_none() {
                continue;
            }
            let shown = String::from_utf8_lossy(s.pattern)
                .chars()
                .map(|ch| if ch.is_control() { '.' } else { ch })
                .collect::<String>();
            c.add(
                s.name,
                s.category,
                s.confidence,
                version,
                Evidence {
                    offset: off as u64,
                    length: s.pattern.len() as u64,
                    description: format!("string \"{shown}\""),
                },
            );
        }
    }

    let mut images: Vec<&ExecutableInfo> = Vec::new();
    if let Some(i) = info.as_ref() {
        images.push(i);
        images.extend(i.slices.iter().filter_map(|s| s.info.as_deref()));
    }
    for img in &images {
        structural(&mut c, data, img);
    }

    // Java.
    if data.starts_with(&[0xca, 0xfe, 0xba, 0xbe]) && info.is_none() && data.len() >= 8 {
        let major = u16::from_be_bytes([data[6], data[7]]);
        if major >= 45 {
            let version = if major >= 49 {
                format!("Java {}", major - 44)
            } else {
                format!("Java 1.{}", major.saturating_sub(44))
            };
            c.add(
                "Java class",
                Category::Runtime,
                Confidence::High,
                Some(version),
                Evidence {
                    offset: 0,
                    length: 8,
                    description: format!("class file, major version {major}"),
                },
            );
        }
    }
    if data.starts_with(b"PK\x03\x04")
        && let Some(p) = memchr::memmem::find(data, b"META-INF/MANIFEST.MF")
    {
        c.add(
            "Java archive (JAR)",
            Category::Runtime,
            Confidence::High,
            None,
            Evidence {
                offset: p as u64,
                length: 20,
                description: "ZIP entry META-INF/MANIFEST.MF".into(),
            },
        );
    }
    if data.starts_with(b"\0asm") && data.len() >= 8 {
        c.add(
            "WebAssembly",
            Category::Runtime,
            Confidence::High,
            Some(format!(
                "binary format {}",
                u32::from_le_bytes([data[4], data[5], data[6], data[7]])
            )),
            Evidence {
                offset: 0,
                length: 8,
                description: "wasm magic".into(),
            },
        );
    }

    let mut out: Vec<Finding> = c.findings.into_values().collect();
    out.sort_by(|a, b| {
        b.confidence
            .cmp(&a.confidence)
            .then(a.category.cmp(&b.category))
            .then(a.name.cmp(&b.name))
    });
    out
}

fn structural(c: &mut Collector, data: &[u8], info: &ExecutableInfo) {
    for s in SIGNATURES.iter().filter(|s| s.kind == SigKind::Section) {
        for sec in &info.sections {
            if sec.name.as_bytes() == s.pattern {
                c.add(
                    s.name,
                    s.category,
                    s.confidence,
                    s.version.map(str::to_string),
                    Evidence {
                        offset: sec.header_offset.unwrap_or(sec.offset),
                        length: if info.format == ExecutableFormat::Pe {
                            40
                        } else {
                            sec.file_size.min(64)
                        },
                        description: format!("section {}", sec.name),
                    },
                );
            }
        }
    }
    for s in SIGNATURES.iter().filter(|s| s.kind == SigKind::Library) {
        let pat = String::from_utf8_lossy(s.pattern).to_ascii_lowercase();
        for libname in &info.libraries {
            let l = libname.to_ascii_lowercase();
            let file = l.rsplit(['/', '\\']).next().unwrap_or(&l);
            if l.starts_with(&pat) || file.starts_with(&pat) || l.contains(&pat) {
                let offset = memchr::memmem::find(data, libname.as_bytes()).unwrap_or(0) as u64;
                c.add(
                    s.name,
                    s.category,
                    s.confidence,
                    s.version.map(str::to_string),
                    Evidence {
                        offset,
                        length: libname.len() as u64,
                        description: format!("imports {libname}"),
                    },
                );
            }
        }
    }
    if let Some(rich) = &info.rich_header {
        let newest = rich
            .entries
            .iter()
            .filter(|e| e.product.starts_with("Utc") || e.product.starts_with("Linker"))
            .filter_map(|e| e.visual_studio.clone())
            .max();
        let products: Vec<String> = rich
            .entries
            .iter()
            .take(6)
            .map(|e| format!("{} build {} x{}", e.product, e.build, e.count))
            .collect();
        c.add(
            "MSVC",
            Category::Compiler,
            Confidence::High,
            newest,
            Evidence {
                offset: rich.offset,
                length: rich.size,
                description: format!("Rich header: {}", products.join(", ")),
            },
        );
    }
    if let Some(clr) = &info.clr_header {
        let version = memchr::memmem::find(data, b"BSJB").and_then(|p| {
            let len = crate::util::le32(data, p + 12)? as usize;
            (len < 256).then(|| text_at(data, p + 16, len))
        });
        c.add(
            ".NET",
            Category::Runtime,
            Confidence::High,
            version.filter(|v| !v.is_empty()),
            Evidence {
                offset: clr.offset,
                length: clr.size,
                description: "CLR runtime header".into(),
            },
        );
    }
}

/// [`detect`] over a tree.
pub fn detect_tree(tree: &Tree) -> Result<Vec<Finding>> {
    Ok(detect(&crate::tree_bytes(tree)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find<'a>(v: &'a [Finding], name: &str) -> Option<&'a Finding> {
        v.iter().find(|f| f.name == name)
    }

    #[test]
    fn string_signatures() {
        let mut data = vec![0u8; 64];
        data.extend(b"\0GCC: (GNU) 13.2.0\0");
        data.extend(b"clang version 17.0.6 (https://github.com/llvm)\0");
        data.extend(b"$Id: UPX 4.22 Copyright (C) 1996-2024\0");
        data.extend(b"rustc version 1.76.0 (07dca489a 2024-02-04)\0");
        data.extend(b"/rustc/07dca489ac2d933c78d3c5158e3f43beefeb02ce/library/core\0");
        let f = detect(&data);
        let gcc = find(&f, "GCC").unwrap();
        assert_eq!(gcc.version.as_deref(), Some("13.2.0"));
        assert_eq!(gcc.evidence[0].offset, 65);
        assert_eq!(
            find(&f, "Clang").unwrap().version.as_deref(),
            Some("17.0.6")
        );
        assert_eq!(find(&f, "UPX").unwrap().version.as_deref(), Some("4.22"));
        let rust = find(&f, "Rust").unwrap();
        assert_eq!(rust.version.as_deref(), Some("1.76.0"));
        assert_eq!(rust.evidence.len(), 2);
    }

    #[test]
    fn go_buildinfo_inline() {
        let mut data = vec![0u8; 16];
        let at = data.len();
        data.extend(b"\xff Go buildinf:");
        data.push(8); // pointer size
        data.push(2); // inline strings
        data.resize(at + 32, 0);
        data.push(8);
        data.extend(b"go1.22.1");
        let f = detect(&data);
        let go = find(&f, "Go").unwrap();
        assert_eq!(go.version.as_deref(), Some("go1.22.1"));
        assert_eq!(go.evidence[0].offset, at as u64);
        assert_eq!(go.confidence, Confidence::High);
    }

    #[test]
    fn pyinstaller_cookie() {
        let mut data = vec![0u8; 100];
        let at = data.len();
        data.extend(b"MEI\x0c\x0b\x0a\x0b\x0e");
        data.extend(1000u32.to_be_bytes());
        data.extend(0u32.to_be_bytes());
        data.extend(0u32.to_be_bytes());
        data.extend(311u32.to_be_bytes());
        data.extend(b"python311.dll\0");
        let f = detect(&data);
        let p = find(&f, "PyInstaller").unwrap();
        assert_eq!(p.version.as_deref(), Some("Python 3.11"));
        assert_eq!(p.evidence[0].offset, at as u64);
    }

    #[test]
    fn java_and_wasm() {
        let class = [0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 61];
        let f = detect(&class);
        assert_eq!(
            find(&f, "Java class").unwrap().version.as_deref(),
            Some("Java 17")
        );
        let mut jar = b"PK\x03\x04".to_vec();
        jar.extend([0u8; 26]);
        jar.extend(b"META-INF/MANIFEST.MF");
        assert!(find(&detect(&jar), "Java archive (JAR)").is_some());
        let wasm = b"\0asm\x01\0\0\0";
        assert!(find(&detect(wasm), "WebAssembly").is_some());
        assert_eq!(detect(b"plain text"), Vec::<Finding>::new());
    }

    #[test]
    fn signature_table_serialises() {
        let json = serde_json::to_string(&SIGNATURES[0]).unwrap();
        assert!(json.contains("\"pattern\":\"UPX0\""));
        assert!(json.contains("\"category\":\"packer\""));
    }
}
