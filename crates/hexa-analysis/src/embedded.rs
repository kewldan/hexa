//! Embedded file detection (ANL-16, used by SRCH-10 carving): finds file
//! signatures in a snapshot and estimates each file's length from its
//! structure — PNG chunks up to `IEND`, JPEG segments up to `EOI`, GIF
//! blocks up to the trailer, ZIP up to the end-of-central-directory record,
//! PDF up to the last `%%EOF`, gzip by inflating the deflate stream,
//! ELF / PE / Mach-O from header, segment and section tables, BMP and RIFF
//! size fields, 7z start header, RAR blocks, SQLite page size × count and
//! top-level MP4 boxes.

use aho_corasick::AhoCorasick;
use hexa_core::hash::{CRC_PRESETS, Crc};
use hexa_core::task::Progress;
use hexa_core::{Cancel, Result, Tree};
use serde::{Deserialize, Serialize};

use crate::util::{self, CHUNK, read_exact};

/// A file found inside the scanned range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Embedded {
    pub offset: u64,
    /// Estimated length; `None` when the end could not be determined
    /// (truncated or corrupted data, Java class files).
    pub len_guess: Option<u64>,
    /// Short type id: `png`, `jpeg`, `gif`, `zip`, `pdf`, `gzip`, `elf`,
    /// `pe`, `macho`, `class`, `bmp`, `wav`, `avi`, `webp`, `riff`, `7z`,
    /// `rar`, `sqlite`, `mp4`, `mov`, `heic`, `avif`, `3gp`.
    pub kind: String,
    /// Human-readable details (dimensions, architecture, brand …).
    pub description: String,
}

/// Options of [`find_embedded`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EmbeddedOptions {
    /// Kinds to report (empty = all).
    pub kinds: Vec<String>,
    pub max_results: usize,
    /// How far end-marker searches (JPEG, ZIP, PDF, gzip) may look.
    pub max_scan: u64,
}

impl Default for EmbeddedOptions {
    fn default() -> Self {
        Self {
            kinds: Vec::new(),
            max_results: 10_000,
            max_scan: 512 << 20,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sig {
    Png,
    Jpeg,
    Gif,
    Zip,
    Pdf,
    Gzip,
    Elf,
    Pe,
    MachO,
    Bmp,
    Riff,
    SevenZip,
    Rar,
    Sqlite,
    /// `ftyp` four bytes into the box.
    Mp4,
}

const SIGS: &[(&[u8], Sig)] = &[
    (b"\x89PNG\r\n\x1a\n", Sig::Png),
    (b"\xFF\xD8\xFF", Sig::Jpeg),
    (b"GIF87a", Sig::Gif),
    (b"GIF89a", Sig::Gif),
    (b"PK\x03\x04", Sig::Zip),
    (b"%PDF-", Sig::Pdf),
    (b"\x1F\x8B\x08", Sig::Gzip),
    (b"\x7FELF", Sig::Elf),
    (b"MZ", Sig::Pe),
    (b"\xFE\xED\xFA\xCE", Sig::MachO),
    (b"\xFE\xED\xFA\xCF", Sig::MachO),
    (b"\xCE\xFA\xED\xFE", Sig::MachO),
    (b"\xCF\xFA\xED\xFE", Sig::MachO),
    (b"\xCA\xFE\xBA\xBE", Sig::MachO),
    (b"BM", Sig::Bmp),
    (b"RIFF", Sig::Riff),
    (b"7z\xBC\xAF\x27\x1C", Sig::SevenZip),
    (b"Rar!\x1A\x07", Sig::Rar),
    (b"SQLite format 3\0", Sig::Sqlite),
    (b"ftyp", Sig::Mp4),
];

/// Every kind [`find_embedded`] can report.
pub const KINDS: &[&str] = &[
    "png", "jpeg", "gif", "zip", "pdf", "gzip", "elf", "pe", "macho", "class", "bmp", "wav", "avi",
    "webp", "riff", "7z", "rar", "sqlite", "mp4", "mov", "heic", "avif", "3gp",
];

/// Random-access reader with bounds checks.
struct R<'a> {
    tree: &'a Tree,
    max_scan: u64,
}

impl R<'_> {
    fn bytes(&self, at: u64, n: usize) -> Option<Vec<u8>> {
        read_exact(self.tree, at, n)
    }
    fn u8(&self, at: u64) -> Option<u8> {
        self.bytes(at, 1).map(|b| b[0])
    }
    fn u16(&self, at: u64, be: bool) -> Option<u16> {
        let b = self.bytes(at, 2)?;
        Some(if be {
            u16::from_be_bytes([b[0], b[1]])
        } else {
            u16::from_le_bytes([b[0], b[1]])
        })
    }
    fn u32(&self, at: u64, be: bool) -> Option<u32> {
        let b: [u8; 4] = self.bytes(at, 4)?.try_into().ok()?;
        Some(if be {
            u32::from_be_bytes(b)
        } else {
            u32::from_le_bytes(b)
        })
    }
    fn u64(&self, at: u64, be: bool) -> Option<u64> {
        let b: [u8; 8] = self.bytes(at, 8)?.try_into().ok()?;
        Some(if be {
            u64::from_be_bytes(b)
        } else {
            u64::from_le_bytes(b)
        })
    }
    fn len(&self) -> u64 {
        self.tree.len()
    }

    /// Offsets of `pattern` in `[from, from + max_scan)`; `f` returns
    /// `false` to stop.
    fn find_all(&self, from: u64, patterns: &[&[u8]], mut f: impl FnMut(usize, u64) -> bool) {
        let Ok(ac) = AhoCorasick::new(patterns) else {
            return;
        };
        let longest = patterns.iter().map(|p| p.len()).max().unwrap_or(1) as u64;
        let end = from.saturating_add(self.max_scan).min(self.len());
        let step = 1u64 << 20;
        let mut at = from;
        while at < end {
            let n = (end - at).min(step + longest - 1);
            let Ok(buf) = self.tree.read_vec(at, n as usize) else {
                return;
            };
            for m in ac.find_iter(&buf) {
                if (m.start() as u64) < step && !f(m.pattern().as_usize(), at + m.start() as u64) {
                    return;
                }
            }
            at += step;
        }
    }
}

fn hit(kind: &str, len: Option<u64>, description: String) -> (String, Option<u64>, String) {
    (kind.to_string(), len, description)
}

type Found = (String, Option<u64>, String);

fn png(r: &R<'_>, off: u64) -> Option<Found> {
    let ihdr = r.bytes(off + 8, 16)?;
    if &ihdr[4..8] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(ihdr[8..12].try_into().ok()?);
    let h = u32::from_be_bytes(ihdr[12..16].try_into().ok()?);
    let desc = format!("PNG image {w}×{h}");
    let mut pos = off + 8;
    for _ in 0..1_000_000 {
        let Some(head) = r.bytes(pos, 8) else {
            return Some(hit("png", None, desc));
        };
        let len = u64::from(u32::from_be_bytes(head[..4].try_into().ok()?));
        if !head[4..8].iter().all(u8::is_ascii_alphabetic) {
            return Some(hit("png", None, desc));
        }
        pos += 12 + len;
        if &head[4..8] == b"IEND" {
            return Some(hit("png", (pos <= r.len()).then_some(pos - off), desc));
        }
    }
    Some(hit("png", None, desc))
}

fn jpeg(r: &R<'_>, off: u64) -> Option<Found> {
    let m = r.u8(off + 3)?;
    if !matches!(m, 0xC0..=0xCF | 0xDB | 0xDD | 0xE0..=0xEF | 0xFE) {
        return None;
    }
    let mut desc = "JPEG image".to_string();
    let mut pos = off + 2;
    let limit = off.saturating_add(r.max_scan).min(r.len());
    while pos < limit {
        if r.u8(pos)? != 0xFF {
            return Some(hit("jpeg", None, desc));
        }
        let marker = r.u8(pos + 1)?;
        match marker {
            0xD9 => return Some(hit("jpeg", Some(pos + 2 - off), desc)),
            0xFF => pos += 1,
            0x01 | 0xD0..=0xD7 => pos += 2,
            _ => {
                let len = u64::from(r.u16(pos + 2, true)?);
                if len < 2 {
                    return Some(hit("jpeg", None, desc));
                }
                if matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF)
                    && let Some(f) = r.bytes(pos + 5, 4)
                {
                    let h = u16::from_be_bytes([f[0], f[1]]);
                    let w = u16::from_be_bytes([f[2], f[3]]);
                    desc = format!("JPEG image {w}×{h}");
                }
                pos += 2 + len;
                if marker == 0xDA {
                    // Entropy-coded data: find the next marker that is not
                    // a stuffed byte or a restart marker.
                    let mut next = None;
                    let mut prev_ff_at = None;
                    r.find_all(pos, &[b"\xFF"], |_, at| {
                        if prev_ff_at == Some(at) {
                            prev_ff_at = Some(at + 1);
                            return true;
                        }
                        match r.u8(at + 1) {
                            Some(0x00 | 0xD0..=0xD7) => true,
                            Some(0xFF) => {
                                prev_ff_at = Some(at + 1);
                                true
                            }
                            Some(_) => {
                                next = Some(at);
                                false
                            }
                            None => false,
                        }
                    });
                    match next {
                        Some(n) => pos = n,
                        None => return Some(hit("jpeg", None, desc)),
                    }
                }
            }
        }
    }
    Some(hit("jpeg", None, desc))
}

fn skip_sub_blocks(r: &R<'_>, mut pos: u64) -> Option<u64> {
    loop {
        let n = u64::from(r.u8(pos)?);
        pos += 1 + n;
        if n == 0 {
            return Some(pos);
        }
    }
}

fn gif(r: &R<'_>, off: u64) -> Option<Found> {
    let w = r.u16(off + 6, false)?;
    let h = r.u16(off + 8, false)?;
    let packed = r.u8(off + 10)?;
    let desc = format!("GIF image {w}×{h}");
    let gct = if packed & 0x80 != 0 {
        3u64 << ((packed & 7) + 1)
    } else {
        0
    };
    let mut pos = off + 13 + gct;
    let mut walk = || -> Option<u64> {
        for _ in 0..10_000_000 {
            match r.u8(pos)? {
                0x3B => return Some(pos + 1),
                0x21 => pos = skip_sub_blocks(r, pos + 2)?,
                0x2C => {
                    let p = r.u8(pos + 9)?;
                    let lct = if p & 0x80 != 0 {
                        3u64 << ((p & 7) + 1)
                    } else {
                        0
                    };
                    pos = skip_sub_blocks(r, pos + 10 + lct + 1)?;
                }
                _ => return None,
            }
        }
        None
    };
    Some(hit("gif", walk().map(|e| e - off), desc))
}

fn zip(r: &R<'_>, off: u64) -> Option<Found> {
    let version = r.u16(off + 4, false)?;
    let method = r.u16(off + 8, false)?;
    if version > 100 || !matches!(method, 0..=20 | 93 | 95 | 96 | 97 | 98 | 99) {
        return None;
    }
    let name_len = usize::from(r.u16(off + 26, false)?);
    let first = r
        .bytes(off + 30, name_len.min(64))
        .map(|n| String::from_utf8_lossy(&n).into_owned())
        .unwrap_or_default();
    let mut end = None;
    r.find_all(off, &[b"PK\x05\x06"], |_, at| {
        let Some(clen) = r.u16(at + 20, false) else {
            return false;
        };
        let cd_size = r.u32(at + 12, false).map(u64::from);
        let cd_off = r.u32(at + 16, false).map(u64::from);
        let consistent = matches!((cd_off, cd_size), (Some(o), Some(s)) if off + o + s == at)
            || cd_off == Some(u64::from(u32::MAX));
        if consistent || end.is_none() {
            end = Some(at + 22 + u64::from(clen));
        }
        !consistent
    });
    let desc = if first.is_empty() {
        "ZIP archive".to_string()
    } else {
        format!("ZIP archive (first entry {first:?})")
    };
    Some(hit(
        "zip",
        end.filter(|&e| e <= r.len()).map(|e| e - off),
        desc,
    ))
}

fn pdf(r: &R<'_>, off: u64) -> Option<Found> {
    let v = r.bytes(off + 5, 3)?;
    if !(v[0].is_ascii_digit() && v[1] == b'.' && v[2].is_ascii_digit()) {
        return None;
    }
    let desc = format!("PDF document {}", String::from_utf8_lossy(&v));
    let mut last = None;
    r.find_all(off + 5, &[b"%%EOF", b"%PDF-"], |pat, at| {
        if pat == 1 {
            return false;
        }
        last = Some(at + 5);
        true
    });
    let end = last.map(|mut e| {
        match r.bytes(e, 2).as_deref() {
            Some([b'\r', b'\n']) => e += 2,
            Some([b'\r' | b'\n', ..]) => e += 1,
            _ => {}
        }
        e - off
    });
    Some(hit("pdf", end, desc))
}

fn inflate_len(r: &R<'_>, start: u64) -> Option<u64> {
    use miniz_oxide::inflate::stream::{InflateState, inflate};
    use miniz_oxide::{DataFormat, MZError, MZFlush, MZStatus};
    let mut state = InflateState::new_boxed(DataFormat::Raw);
    let mut out = vec![0u8; 64 << 10];
    let mut pos = start;
    let mut consumed = 0u64;
    while pos - start < r.max_scan {
        let chunk = r.tree.read_vec(pos, 256 << 10).ok()?;
        if chunk.is_empty() {
            return None;
        }
        let mut input = &chunk[..];
        loop {
            let res = inflate(&mut state, input, &mut out, MZFlush::None);
            input = &input[res.bytes_consumed..];
            consumed += res.bytes_consumed as u64;
            match res.status {
                Ok(MZStatus::StreamEnd) => return Some(consumed),
                Ok(_) | Err(MZError::Buf) => {}
                Err(_) => return None,
            }
            if (input.is_empty() && res.bytes_written < out.len())
                || (res.bytes_consumed == 0 && res.bytes_written == 0)
            {
                break;
            }
        }
        pos += chunk.len() as u64;
    }
    None
}

fn gzip(r: &R<'_>, off: u64) -> Option<Found> {
    let flags = r.u8(off + 3)?;
    if flags & 0xE0 != 0 {
        return None;
    }
    let os = r.u8(off + 9)?;
    if os > 13 && os != 255 {
        return None;
    }
    let mut pos = off + 10;
    if flags & 4 != 0 {
        pos += 2 + u64::from(r.u16(pos, false)?);
    }
    let mut name = String::new();
    for (bit, keep) in [(8u8, true), (16, false)] {
        if flags & bit != 0 {
            let mut s = Vec::new();
            loop {
                let c = r.u8(pos)?;
                pos += 1;
                if c == 0 {
                    break;
                }
                if s.len() < 256 {
                    s.push(c);
                }
            }
            if keep {
                name = String::from_utf8_lossy(&s).into_owned();
            }
        }
    }
    if flags & 2 != 0 {
        pos += 2;
    }
    let desc = if name.is_empty() {
        "gzip stream".to_string()
    } else {
        format!("gzip stream ({name})")
    };
    let len = inflate_len(r, pos)
        .map(|n| pos + n + 8 - off)
        .filter(|&l| off + l <= r.len());
    Some(hit("gzip", len, desc))
}

fn elf(r: &R<'_>, off: u64) -> Option<Found> {
    let id = r.bytes(off + 4, 3)?;
    let (class, data, version) = (id[0], id[1], id[2]);
    if !(1..=2).contains(&class) || !(1..=2).contains(&data) || version != 1 {
        return None;
    }
    let be = data == 2;
    let is64 = class == 2;
    let machine = r.u16(off + 0x12, be)?;
    let (phoff, shoff, phent, phnum, shent, shnum) = if is64 {
        (
            r.u64(off + 0x20, be)?,
            r.u64(off + 0x28, be)?,
            r.u16(off + 0x36, be)?,
            r.u16(off + 0x38, be)?,
            r.u16(off + 0x3A, be)?,
            r.u16(off + 0x3C, be)?,
        )
    } else {
        (
            u64::from(r.u32(off + 0x1C, be)?),
            u64::from(r.u32(off + 0x20, be)?),
            r.u16(off + 0x2A, be)?,
            r.u16(off + 0x2C, be)?,
            r.u16(off + 0x2E, be)?,
            r.u16(off + 0x30, be)?,
        )
    };
    let mut end = if is64 { 64 } else { 52 };
    end = end.max(phoff.saturating_add(u64::from(phent) * u64::from(phnum)));
    end = end.max(shoff.saturating_add(u64::from(shent) * u64::from(shnum)));
    for i in 0..u64::from(phnum) {
        let p = off + phoff + i * u64::from(phent);
        let (o, s) = if is64 {
            (r.u64(p + 8, be), r.u64(p + 32, be))
        } else {
            (
                r.u32(p + 4, be).map(u64::from),
                r.u32(p + 16, be).map(u64::from),
            )
        };
        if let (Some(o), Some(s)) = (o, s) {
            end = end.max(o.saturating_add(s));
        }
    }
    for i in 0..u64::from(shnum) {
        let p = off + shoff + i * u64::from(shent);
        let ty = r.u32(p + 4, be);
        let (o, s) = if is64 {
            (r.u64(p + 24, be), r.u64(p + 32, be))
        } else {
            (
                r.u32(p + 16, be).map(u64::from),
                r.u32(p + 20, be).map(u64::from),
            )
        };
        if let (Some(t), Some(o), Some(s)) = (ty, o, s)
            && t != 8
        {
            end = end.max(o.saturating_add(s));
        }
    }
    let arch = match machine {
        3 => "x86",
        8 => "MIPS",
        0x14 => "PowerPC",
        0x28 => "ARM",
        0x3E => "x86-64",
        0xB7 => "AArch64",
        0xF3 => "RISC-V",
        _ => "unknown machine",
    };
    let desc = format!(
        "ELF {}-bit {} {arch}",
        if is64 { 64 } else { 32 },
        if be { "BE" } else { "LE" }
    );
    Some(hit("elf", (off + end <= r.len()).then_some(end), desc))
}

fn pe(r: &R<'_>, off: u64) -> Option<Found> {
    let lfanew = u64::from(r.u32(off + 0x3C, false)?);
    if !(0x40..=0x10000).contains(&lfanew) || r.bytes(off + lfanew, 4)? != b"PE\0\0" {
        return None;
    }
    let coff = off + lfanew + 4;
    let machine = r.u16(coff, false)?;
    let nsec = u64::from(r.u16(coff + 2, false)?);
    let opt_size = u64::from(r.u16(coff + 16, false)?);
    let opt = coff + 20;
    let magic = r.u16(opt, false)?;
    let dd = match magic {
        0x10B => opt + 96,
        0x20B => opt + 112,
        _ => return None,
    };
    let mut end = u64::from(r.u32(opt + 60, false)?);
    let sec = opt + opt_size;
    end = end.max(sec + nsec * 40 - off);
    for i in 0..nsec.min(256) {
        let s = sec + i * 40;
        let raw_size = u64::from(r.u32(s + 16, false)?);
        let raw_ptr = u64::from(r.u32(s + 20, false)?);
        if raw_size > 0 {
            end = end.max(raw_ptr + raw_size);
        }
    }
    // Authenticode certificates (security directory) use file offsets.
    if opt_size >= dd - opt + 40
        && let (Some(cert_off), Some(cert_len)) = (r.u32(dd + 32, false), r.u32(dd + 36, false))
        && cert_len > 0
    {
        end = end.max(u64::from(cert_off) + u64::from(cert_len));
    }
    let arch = match machine {
        0x14C => "x86",
        0x8664 => "x86-64",
        0x1C0 | 0x1C4 => "ARM",
        0xAA64 => "ARM64",
        _ => "unknown machine",
    };
    let desc = format!(
        "PE {} {arch}",
        if magic == 0x20B { "PE32+" } else { "PE32" }
    );
    Some(hit("pe", (off + end <= r.len()).then_some(end), desc))
}

fn macho(r: &R<'_>, off: u64) -> Option<Found> {
    let magic = r.u32(off, true)?;
    if magic == 0xCAFE_BABE {
        let n = r.u32(off + 4, true)?;
        if n == 0 || n > 30 {
            // Java class file: minor/major version instead of an arch count.
            let major = r.u16(off + 6, true)?;
            return (45..=80)
                .contains(&major)
                .then(|| hit("class", None, format!("Java class (version {major})")));
        }
        let mut end = 8 + u64::from(n) * 20;
        for i in 0..u64::from(n) {
            let a = off + 8 + i * 20;
            end = end.max(u64::from(r.u32(a + 8, true)?) + u64::from(r.u32(a + 12, true)?));
        }
        return Some(hit(
            "macho",
            (off + end <= r.len()).then_some(end),
            format!("Mach-O universal binary ({n} architectures)"),
        ));
    }
    let (be, is64) = match magic {
        0xFEED_FACE => (true, false),
        0xFEED_FACF => (true, true),
        0xCEFA_EDFE => (false, false),
        0xCFFA_EDFE => (false, true),
        _ => return None,
    };
    let cpu = r.u32(off + 4, be)?;
    let ncmds = u64::from(r.u32(off + 16, be)?);
    let sizeofcmds = u64::from(r.u32(off + 20, be)?);
    if ncmds > 10_000 || sizeofcmds > 16 << 20 {
        return None;
    }
    let header = if is64 { 32 } else { 28 };
    let mut end = header + sizeofcmds;
    let mut pos = off + header;
    for _ in 0..ncmds {
        let cmd = r.u32(pos, be)?;
        let size = u64::from(r.u32(pos + 4, be)?);
        if size < 8 {
            break;
        }
        match cmd {
            0x1 => {
                end = end.max(u64::from(r.u32(pos + 32, be)?) + u64::from(r.u32(pos + 36, be)?));
            }
            0x19 => end = end.max(r.u64(pos + 40, be)?.saturating_add(r.u64(pos + 48, be)?)),
            0x1D => {
                end = end.max(u64::from(r.u32(pos + 8, be)?) + u64::from(r.u32(pos + 12, be)?));
            }
            _ => {}
        }
        pos += size;
    }
    let arch = match cpu {
        7 => "x86",
        0x0100_0007 => "x86-64",
        12 => "ARM",
        0x0100_000C => "ARM64",
        18 => "PowerPC",
        0x0100_0012 => "PowerPC64",
        _ => "unknown CPU",
    };
    let desc = format!("Mach-O {}-bit {arch}", if is64 { 64 } else { 32 });
    Some(hit("macho", (off + end <= r.len()).then_some(end), desc))
}

fn bmp(r: &R<'_>, off: u64) -> Option<Found> {
    let size = u64::from(r.u32(off + 2, false)?);
    let reserved = r.u32(off + 6, false)?;
    let data = u64::from(r.u32(off + 10, false)?);
    let dib = r.u32(off + 14, false)?;
    if reserved != 0
        || size < 26
        || !(26..size).contains(&data)
        || ![12, 40, 52, 56, 64, 108, 124].contains(&dib)
    {
        return None;
    }
    let (w, h) = if dib == 12 {
        (
            i64::from(r.u16(off + 18, false)?),
            i64::from(r.u16(off + 20, false)?),
        )
    } else {
        (
            i64::from(r.u32(off + 18, false)? as i32),
            i64::from(r.u32(off + 22, false)? as i32),
        )
    };
    let desc = format!("BMP image {w}×{}", h.abs());
    Some(hit("bmp", (off + size <= r.len()).then_some(size), desc))
}

fn riff(r: &R<'_>, off: u64) -> Option<Found> {
    let size = u64::from(r.u32(off + 4, false)?);
    let form = r.bytes(off + 8, 4)?;
    if size < 4 || !form.iter().all(|c| (0x20..0x7F).contains(c)) {
        return None;
    }
    let kind = match &form[..] {
        b"WAVE" => "wav",
        b"AVI " => "avi",
        b"WEBP" => "webp",
        _ => "riff",
    };
    let len = 8 + size + (size & 1);
    let len = if off + len > r.len() && off + 8 + size == r.len() {
        8 + size
    } else {
        len
    };
    let desc = format!("RIFF {}", String::from_utf8_lossy(&form).trim_end());
    Some(hit(kind, (off + len <= r.len()).then_some(len), desc))
}

fn seven_zip(r: &R<'_>, off: u64) -> Option<Found> {
    let major = r.u8(off + 6)?;
    let head = r.bytes(off + 8, 24)?;
    if major != 0 {
        return None;
    }
    let crc_params = CRC_PRESETS
        .iter()
        .find(|p| p.name == "CRC-32/ISO-HDLC")?
        .params;
    let mut crc = Crc::new(crc_params).ok()?;
    crc.update(&head[4..]);
    if crc.finish() != u64::from(u32::from_le_bytes(head[..4].try_into().ok()?)) {
        return None;
    }
    let next_off = u64::from_le_bytes(head[4..12].try_into().ok()?);
    let next_size = u64::from_le_bytes(head[12..20].try_into().ok()?);
    let len = 32u64.checked_add(next_off)?.checked_add(next_size)?;
    let desc = format!("7-Zip archive (version 0.{})", r.u8(off + 7)?);
    Some(hit("7z", (off + len <= r.len()).then_some(len), desc))
}

fn vint(r: &R<'_>, mut pos: u64) -> Option<(u64, u64)> {
    let mut v = 0u64;
    for i in 0..10 {
        let b = r.u8(pos)?;
        pos += 1;
        v |= u64::from(b & 0x7F) << (7 * i);
        if b & 0x80 == 0 {
            return Some((v, pos));
        }
    }
    None
}

fn rar(r: &R<'_>, off: u64) -> Option<Found> {
    match r.u8(off + 6)? {
        0x01 if r.u8(off + 7)? == 0 => {
            let mut pos = off + 8;
            for _ in 0..1_000_000 {
                let (hsize, after) = vint(r, pos + 4)?;
                let (htype, p) = vint(r, after)?;
                let (flags, mut p) = vint(r, p)?;
                if flags & 1 != 0 {
                    p = vint(r, p)?.1;
                }
                let data = if flags & 2 != 0 { vint(r, p)?.0 } else { 0 };
                if hsize == 0 || hsize > 2 << 20 {
                    return Some(hit("rar", None, "RAR 5 archive".into()));
                }
                let next = after.checked_add(hsize)?.checked_add(data)?;
                if htype == 5 {
                    return Some(hit(
                        "rar",
                        (next <= r.len()).then_some(next - off),
                        "RAR 5 archive".into(),
                    ));
                }
                pos = next;
            }
            Some(hit("rar", None, "RAR 5 archive".into()))
        }
        0x00 => {
            let mut pos = off;
            for _ in 0..1_000_000 {
                let ty = r.u8(pos + 2)?;
                let flags = r.u16(pos + 3, false)?;
                let size = u64::from(r.u16(pos + 5, false)?);
                if size < 7 || !(0x72..=0x7B).contains(&ty) {
                    return Some(hit("rar", None, "RAR 4 archive".into()));
                }
                let mut add = 0u64;
                if flags & 0x8000 != 0 || ty == 0x74 {
                    add = u64::from(r.u32(pos + 7, false)?);
                    if ty == 0x74 && flags & 0x100 != 0 {
                        add |= u64::from(r.u32(pos + 32, false)?) << 32;
                    }
                }
                if ty == 0x7B {
                    let end = pos + size;
                    return Some(hit(
                        "rar",
                        (end <= r.len()).then_some(end - off),
                        "RAR 4 archive".into(),
                    ));
                }
                pos += size + add;
                if pos >= r.len() {
                    // Archives without an end block end with the last file.
                    return Some(hit(
                        "rar",
                        (pos == r.len()).then_some(pos - off),
                        "RAR 4 archive".into(),
                    ));
                }
            }
            Some(hit("rar", None, "RAR 4 archive".into()))
        }
        _ => None,
    }
}

fn sqlite(r: &R<'_>, off: u64) -> Option<Found> {
    let ps = r.u16(off + 16, true)?;
    let page = if ps == 1 { 65_536 } else { u64::from(ps) };
    if !page.is_power_of_two() || !(512..=65_536).contains(&page) {
        return None;
    }
    let count = u64::from(r.u32(off + 28, true)?);
    let change = r.u32(off + 24, true)?;
    let valid_for = r.u32(off + 92, true)?;
    let len = (count > 0 && change == valid_for).then_some(page * count);
    let desc = format!("SQLite 3 database (page size {page})");
    Some(hit("sqlite", len.filter(|&l| off + l <= r.len()), desc))
}

fn mp4(r: &R<'_>, ftyp_at: u64) -> Option<Found> {
    let off = ftyp_at.checked_sub(4)?;
    let size = r.u32(off, true)?;
    let brand = r.bytes(ftyp_at + 4, 4)?;
    if !(8..=4096).contains(&size)
        || !brand
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || *c == b' ')
    {
        return None;
    }
    let kind = match &brand[..] {
        b"qt  " => "mov",
        b"heic" | b"heix" | b"mif1" | b"msf1" | b"hevc" => "heic",
        b"avif" | b"avis" => "avif",
        b if b.starts_with(b"3g") => "3gp",
        _ => "mp4",
    };
    let mut pos = off;
    let mut boxes = 0;
    let end = loop {
        let Some(h) = r.bytes(pos, 8) else {
            break pos;
        };
        let size = u64::from(u32::from_be_bytes(h[..4].try_into().ok()?));
        if !h[4..8]
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || *c == b' ' || *c == 0xA9)
        {
            break pos;
        }
        let size = match size {
            0 => r.len() - pos,
            1 => r.u64(pos + 8, true)?,
            s => s,
        };
        if size < 8 || pos + size > r.len() {
            break if boxes == 0 { return None } else { pos };
        }
        pos += size;
        boxes += 1;
        if boxes > 100_000 {
            break pos;
        }
    };
    let desc = format!(
        "ISO media ({}, brand {:?})",
        kind,
        String::from_utf8_lossy(&brand)
    );
    Some(hit(kind, Some(end - off), desc))
}

fn validate(r: &R<'_>, sig: Sig, at: u64) -> Option<(u64, Found)> {
    let off = if sig == Sig::Mp4 {
        at.checked_sub(4)?
    } else {
        at
    };
    let found = match sig {
        Sig::Png => png(r, at),
        Sig::Jpeg => jpeg(r, at),
        Sig::Gif => gif(r, at),
        Sig::Zip => zip(r, at),
        Sig::Pdf => pdf(r, at),
        Sig::Gzip => gzip(r, at),
        Sig::Elf => elf(r, at),
        Sig::Pe => pe(r, at),
        Sig::MachO => macho(r, at),
        Sig::Bmp => bmp(r, at),
        Sig::Riff => riff(r, at),
        Sig::SevenZip => seven_zip(r, at),
        Sig::Rar => rar(r, at),
        Sig::Sqlite => sqlite(r, at),
        Sig::Mp4 => mp4(r, at),
    }?;
    Some((off, found))
}

/// Scans `[start, end)` for embedded files.
pub fn find_embedded(
    tree: &Tree,
    start: u64,
    end: u64,
    opts: &EmbeddedOptions,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<Vec<Embedded>> {
    let patterns: Vec<&[u8]> = SIGS.iter().map(|(p, _)| *p).collect();
    let ac = AhoCorasick::new(&patterns).map_err(|e| util::invalid(e.to_string()))?;
    let longest = patterns.iter().map(|p| p.len()).max().unwrap_or(1);
    let r = R {
        tree,
        max_scan: opts.max_scan,
    };
    let (start, end) = util::clamp(tree, start, end);
    let wanted = |k: &str| opts.kinds.is_empty() || opts.kinds.iter().any(|w| w == k);
    let mut out: Vec<Embedded> = Vec::new();
    let mut zip_until = 0u64;
    let mut at = start;
    while at < end && out.len() < opts.max_results {
        cancel.check()?;
        let body = (end - at).min(CHUNK as u64);
        // Read a little past the chunk so signatures spanning it are seen.
        let n = (end - at).min(body + longest as u64 - 1) as usize;
        let buf = tree.read_vec(at, n)?;
        for m in ac.find_overlapping_iter(&buf) {
            if m.start() as u64 >= body {
                continue;
            }
            let pos = at + m.start() as u64;
            let sig = SIGS[m.pattern().as_usize()].1;
            if sig == Sig::Zip && pos < zip_until {
                continue;
            }
            if let Some((off, (kind, len, description))) = validate(&r, sig, pos)
                && off >= start
                && wanted(&kind)
            {
                if kind == "zip" {
                    zip_until = len.map_or(zip_until, |l| off + l);
                }
                out.push(Embedded {
                    offset: off,
                    len_guess: len,
                    kind,
                    description,
                });
                if out.len() >= opts.max_results {
                    break;
                }
            }
        }
        at += body;
        progress(at - start, end - start);
    }
    out.sort_by_key(|e| e.offset);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{noop, random_bytes, tree};
    use std::io::Cursor;

    fn scan(data: &[u8]) -> Vec<Embedded> {
        find_embedded(
            &tree(data),
            0,
            data.len() as u64,
            &EmbeddedOptions::default(),
            &Cancel::new(),
            &noop,
        )
        .unwrap()
    }

    fn png_bytes() -> Vec<u8> {
        let img = image::RgbaImage::from_fn(5, 4, |x, y| image::Rgba([x as u8, y as u8, 9, 255]));
        let mut c = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut c, image::ImageFormat::Png)
            .unwrap();
        c.into_inner()
    }

    fn encode(f: image::ImageFormat) -> Vec<u8> {
        let img =
            image::RgbImage::from_fn(16, 8, |x, y| image::Rgb([x as u8 * 9, y as u8 * 20, 50]));
        let mut c = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut c, f)
            .unwrap();
        c.into_inner()
    }

    /// Junk without accidental signatures.
    fn filler(n: usize) -> Vec<u8> {
        vec![0x11; n]
    }

    fn carve(parts: &[Vec<u8>]) -> (Vec<u8>, Vec<(u64, u64)>) {
        let mut data = filler(100);
        let mut spans = Vec::new();
        for p in parts {
            spans.push((data.len() as u64, p.len() as u64));
            data.extend(p);
            data.extend(filler(37));
        }
        (data, spans)
    }

    fn check(parts: &[Vec<u8>], kinds: &[&str]) {
        let (data, spans) = carve(parts);
        let found = scan(&data);
        let got: Vec<(u64, Option<u64>, &str)> = found
            .iter()
            .map(|e| (e.offset, e.len_guess, e.kind.as_str()))
            .collect();
        let want: Vec<(u64, Option<u64>, &str)> = spans
            .iter()
            .zip(kinds)
            .map(|(&(o, l), k)| (o, Some(l), *k))
            .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn images() {
        check(
            &[
                png_bytes(),
                encode(image::ImageFormat::Jpeg),
                encode(image::ImageFormat::Gif),
                encode(image::ImageFormat::Bmp),
                encode(image::ImageFormat::WebP),
            ],
            &["png", "jpeg", "gif", "bmp", "webp"],
        );
        let found = scan(&png_bytes());
        assert_eq!(found[0].description, "PNG image 5×4");
    }

    fn zip_bytes() -> Vec<u8> {
        // Single stored entry "a.txt" containing "hi".
        let mut z = Vec::new();
        z.extend(b"PK\x03\x04");
        z.extend([20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        z.extend(0xD8A2_1F2Bu32.to_le_bytes());
        z.extend(2u32.to_le_bytes());
        z.extend(2u32.to_le_bytes());
        z.extend(5u16.to_le_bytes());
        z.extend(0u16.to_le_bytes());
        z.extend(b"a.txt");
        z.extend(b"hi");
        let cd = z.len() as u32;
        z.extend(b"PK\x01\x02");
        z.extend([20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        z.extend(0xD8A2_1F2Bu32.to_le_bytes());
        z.extend(2u32.to_le_bytes());
        z.extend(2u32.to_le_bytes());
        z.extend(5u16.to_le_bytes());
        z.extend([0u8; 12]);
        z.extend(0u32.to_le_bytes());
        z.extend(b"a.txt");
        let cd_size = z.len() as u32 - cd;
        z.extend(b"PK\x05\x06");
        z.extend([0, 0, 0, 0, 1, 0, 1, 0]);
        z.extend(cd_size.to_le_bytes());
        z.extend(cd.to_le_bytes());
        z.extend(3u16.to_le_bytes());
        z.extend(b"abc");
        z
    }

    fn gzip_bytes() -> Vec<u8> {
        let payload = b"hello hello hello gzip world".repeat(50);
        let mut g = vec![0x1F, 0x8B, 0x08, 0x08, 0, 0, 0, 0, 0, 3];
        g.extend(b"x.txt\0");
        g.extend(miniz_oxide::deflate::compress_to_vec(&payload, 6));
        g.extend([1, 2, 3, 4]);
        g.extend((payload.len() as u32).to_le_bytes());
        g
    }

    fn pdf_bytes() -> Vec<u8> {
        b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\ntrailer\n<<>>\n%%EOF\nupdate\n%%EOF\n".to_vec()
    }

    fn elf_bytes() -> Vec<u8> {
        let mut e = vec![0u8; 0x200];
        e[..4].copy_from_slice(b"\x7FELF");
        e[4] = 2;
        e[5] = 1;
        e[6] = 1;
        e[0x12..0x14].copy_from_slice(&0x3Eu16.to_le_bytes());
        e[0x20..0x28].copy_from_slice(&64u64.to_le_bytes()); // phoff
        e[0x28..0x30].copy_from_slice(&0x1C0u64.to_le_bytes()); // shoff
        e[0x36..0x38].copy_from_slice(&56u16.to_le_bytes());
        e[0x38..0x3A].copy_from_slice(&1u16.to_le_bytes());
        e[0x3A..0x3C].copy_from_slice(&64u16.to_le_bytes());
        e[0x3C..0x3E].copy_from_slice(&1u16.to_le_bytes());
        // PT_LOAD covering 0x180 bytes.
        e[64 + 8..64 + 16].copy_from_slice(&0u64.to_le_bytes());
        e[64 + 32..64 + 40].copy_from_slice(&0x180u64.to_le_bytes());
        // Section header ends at 0x1C0 + 64 = 0x200.
        e
    }

    fn pe_bytes() -> Vec<u8> {
        let mut p = vec![0u8; 0x400];
        p[..2].copy_from_slice(b"MZ");
        p[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        p[0x80..0x84].copy_from_slice(b"PE\0\0");
        p[0x84..0x86].copy_from_slice(&0x8664u16.to_le_bytes());
        p[0x86..0x88].copy_from_slice(&1u16.to_le_bytes());
        p[0x94..0x96].copy_from_slice(&240u16.to_le_bytes());
        let opt = 0x98;
        p[opt..opt + 2].copy_from_slice(&0x20Bu16.to_le_bytes());
        p[opt + 60..opt + 64].copy_from_slice(&0x200u32.to_le_bytes());
        let sec = opt + 240;
        p[sec..sec + 5].copy_from_slice(b".text");
        p[sec + 16..sec + 20].copy_from_slice(&0x200u32.to_le_bytes());
        p[sec + 20..sec + 24].copy_from_slice(&0x200u32.to_le_bytes());
        p
    }

    fn macho_bytes() -> Vec<u8> {
        let mut m = vec![0u8; 0x300];
        m[..4].copy_from_slice(&0xFEED_FACFu32.to_le_bytes());
        m[4..8].copy_from_slice(&0x0100_000Cu32.to_le_bytes());
        m[16..20].copy_from_slice(&1u32.to_le_bytes());
        m[20..24].copy_from_slice(&72u32.to_le_bytes());
        m[32..36].copy_from_slice(&0x19u32.to_le_bytes());
        m[36..40].copy_from_slice(&72u32.to_le_bytes());
        m[72..80].copy_from_slice(&0u64.to_le_bytes());
        m[80..88].copy_from_slice(&0x300u64.to_le_bytes());
        m
    }

    fn sevenz_bytes() -> Vec<u8> {
        let mut s = b"7z\xBC\xAF\x27\x1C\x00\x04".to_vec();
        let mut head = Vec::new();
        head.extend(10u64.to_le_bytes());
        head.extend(6u64.to_le_bytes());
        head.extend([0u8; 4]);
        let mut c = Crc::new(
            CRC_PRESETS
                .iter()
                .find(|p| p.name == "CRC-32/ISO-HDLC")
                .unwrap()
                .params,
        )
        .unwrap();
        c.update(&head);
        s.extend((c.finish() as u32).to_le_bytes());
        s.extend(head);
        s.extend([0x22; 16]);
        s
    }

    fn rar5_bytes() -> Vec<u8> {
        let mut r = b"Rar!\x1A\x07\x01\x00".to_vec();
        // Main header: crc, size 3, type 1, flags 0, archive flags 0.
        r.extend([0, 0, 0, 0, 3, 1, 0, 0]);
        // File header with data: size 4, type 2, flags 2, data size 5, extra.
        r.extend([0, 0, 0, 0, 4, 2, 2, 5, 0]);
        r.extend(b"DATA!");
        // End of archive: size 3, type 5, flags 0, end flags 0.
        r.extend([0, 0, 0, 0, 3, 5, 0, 0]);
        r
    }

    fn rar4_bytes() -> Vec<u8> {
        let mut r = b"Rar!\x1A\x07\x00".to_vec();
        r.extend([0, 0, 0x73, 0, 0, 13, 0, 0, 0, 0, 0, 0, 0]);
        let mut file = vec![0, 0, 0x74, 0, 0x80, 32, 0];
        file.extend(4u32.to_le_bytes());
        file.resize(32, 0);
        r.extend(file);
        r.extend(b"body");
        r.extend([0, 0, 0x7B, 0, 0x40, 7, 0]);
        r
    }

    fn sqlite_bytes() -> Vec<u8> {
        let mut s = vec![0u8; 1024];
        s[..16].copy_from_slice(b"SQLite format 3\0");
        s[16..18].copy_from_slice(&512u16.to_be_bytes());
        s[24..28].copy_from_slice(&7u32.to_be_bytes());
        s[28..32].copy_from_slice(&2u32.to_be_bytes());
        s[92..96].copy_from_slice(&7u32.to_be_bytes());
        s
    }

    fn mp4_bytes() -> Vec<u8> {
        let mut m = Vec::new();
        m.extend(20u32.to_be_bytes());
        m.extend(b"ftypisom");
        m.extend(0u32.to_be_bytes());
        m.extend(b"isom");
        m.extend(16u32.to_be_bytes());
        m.extend(b"mdat");
        m.extend([1u8; 8]);
        m.extend(8u32.to_be_bytes());
        m.extend(b"free");
        m
    }

    fn wav_bytes() -> Vec<u8> {
        let mut w = b"RIFF".to_vec();
        w.extend(9u32.to_le_bytes());
        w.extend(b"WAVEdata\x01");
        w.push(0); // pad byte of the odd-sized chunk
        w
    }

    #[test]
    fn archives_documents_executables() {
        check(
            &[
                zip_bytes(),
                gzip_bytes(),
                pdf_bytes(),
                elf_bytes(),
                pe_bytes(),
                macho_bytes(),
                sevenz_bytes(),
                rar5_bytes(),
                rar4_bytes(),
                sqlite_bytes(),
                mp4_bytes(),
                wav_bytes(),
            ],
            &[
                "zip", "gzip", "pdf", "elf", "pe", "macho", "7z", "rar", "rar", "sqlite", "mp4",
                "wav",
            ],
        );
    }

    #[test]
    fn descriptions_and_filters() {
        let (data, _) = carve(&[elf_bytes(), gzip_bytes(), pe_bytes()]);
        let found = scan(&data);
        assert_eq!(found[0].description, "ELF 64-bit LE x86-64");
        assert_eq!(found[1].description, "gzip stream (x.txt)");
        assert_eq!(found[2].description, "PE PE32+ x86-64");
        let only = find_embedded(
            &tree(&data),
            0,
            data.len() as u64,
            &EmbeddedOptions {
                kinds: vec!["pe".into()],
                ..EmbeddedOptions::default()
            },
            &Cancel::new(),
            &noop,
        )
        .unwrap();
        assert_eq!(only.len(), 1);
        let class = b"\xCA\xFE\xBA\xBE\x00\x00\x00\x34rest";
        let found = scan(class);
        assert_eq!(
            (found[0].kind.as_str(), found[0].len_guess),
            ("class", None)
        );
    }

    #[test]
    fn truncated_and_noise() {
        let mut png = png_bytes();
        png.truncate(png.len() - 6);
        let found = scan(&png);
        assert_eq!((found[0].kind.as_str(), found[0].len_guess), ("png", None));
        // Random data yields few, if any, validated hits.
        let noise = random_bytes(5, 3 << 20);
        assert!(scan(&noise).len() < 5);
        // A signature straddling the chunk boundary is found.
        let mut big = vec![0x11u8; CHUNK - 3];
        big.extend(png_bytes());
        let found = scan(&big);
        assert_eq!(found[0].offset, (CHUNK - 3) as u64);
        assert!(found[0].len_guess.is_some());
    }
}
