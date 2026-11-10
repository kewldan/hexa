//! Text dumps: export (IO-03) as xxd, `hexdump -C`, HTML, RTF, Markdown,
//! CSV and a C header, and import (IO-04) of xxd, `hexdump -C`,
//! `od -A x -t x1z` and plain hex with format detection and a line-by-line
//! error report. xxd, HTML and RTF output reuse [`hexa_core::format`].

use std::fmt::Write as _;

use hexa_core::format::{CopyFormat, copy_as};
use hexa_core::task::Progress;
use hexa_core::{Cancel, Error, Result, Tree};
use serde::{Deserialize, Serialize};

use crate::firmware::{LineError, Segment, merge};
use crate::util::{self, CHUNK, invalid};

/// Dump export formats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum DumpFormat {
    /// `xxd` (16 bytes per line, 2-byte groups).
    Xxd,
    /// `hexdump -C`; `squeeze` replaces repeated lines with `*`.
    HexdumpC { squeeze: bool },
    /// Coloured `<pre>` block (`hexa_core` "Copy as HTML").
    Html,
    /// Coloured RTF (`hexa_core` "Copy as RTF").
    Rtf,
    /// Markdown table with `width` bytes per row.
    Markdown { width: usize },
    /// CSV with `width` bytes per row.
    Csv { width: usize },
    /// C header with a `static const uint8_t name[]` array.
    CHeader { name: String },
}

/// HTML / RTF are built in memory; larger ranges are refused.
pub const MAX_RICH_DUMP: u64 = 16 << 20;

fn printable(b: u8) -> char {
    if (0x20..0x7F).contains(&b) {
        b as char
    } else {
        '.'
    }
}

fn c_ident(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if s.is_empty() || s.starts_with(|c: char| c.is_ascii_digit()) {
        s.insert_str(0, "data_");
    }
    s
}

fn markdown_escape(c: char) -> String {
    match c {
        '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '|' | '#' => format!("\\{c}"),
        c => c.to_string(),
    }
}

/// Streaming writer for one format.
#[derive(Debug)]
struct Dumper {
    format: DumpFormat,
    /// Previous `hexdump -C` line and whether `*` was already printed.
    prev: Option<Vec<u8>>,
    starred: bool,
}

impl Dumper {
    fn new(format: &DumpFormat) -> Result<Self> {
        match format {
            DumpFormat::Markdown { width } | DumpFormat::Csv { width }
                if !(1..=256).contains(width) =>
            {
                return Err(invalid("dump width must be 1…256"));
            }
            _ => {}
        }
        Ok(Self {
            format: format.clone(),
            prev: None,
            starred: false,
        })
    }

    fn width(&self) -> usize {
        match &self.format {
            DumpFormat::Markdown { width } | DumpFormat::Csv { width } => *width,
            DumpFormat::CHeader { .. } => 12,
            _ => 16,
        }
    }

    fn begin(&self, out: &mut String, base: u64, len: u64) {
        let w = self.width();
        match &self.format {
            DumpFormat::Markdown { .. } => {
                out.push_str("| Offset |");
                for i in 0..w {
                    let _ = write!(out, " {i:02X} |");
                }
                out.push_str(" ASCII |\n|---|");
                out.push_str(&"---|".repeat(w));
                out.push_str("---|\n");
            }
            DumpFormat::Csv { .. } => {
                out.push_str("Offset");
                for i in 0..w {
                    let _ = write!(out, ",{i:02X}");
                }
                out.push_str(",ASCII\r\n");
            }
            DumpFormat::CHeader { name } => {
                let id = c_ident(name);
                let guard = format!("{}_H_", id.to_ascii_uppercase());
                let _ = write!(
                    out,
                    "// {len} bytes from offset 0x{base:X}.\n#ifndef {guard}\n#define {guard}\n\n\
                     #include <stddef.h>\n#include <stdint.h>\n\nstatic const uint8_t {id}[{len}] = {{\n"
                );
            }
            _ => {}
        }
    }

    /// Formats whole rows (`bytes` starts at a row boundary).
    fn rows(&mut self, out: &mut String, offset: u64, bytes: &[u8]) {
        let w = self.width();
        match &self.format {
            DumpFormat::Xxd => out.push_str(&hexa_core::format::xxd(bytes, offset)),
            DumpFormat::HexdumpC { squeeze } => {
                let squeeze = *squeeze;
                for (i, line) in bytes.chunks(16).enumerate() {
                    if squeeze && line.len() == 16 && self.prev.as_deref() == Some(line) {
                        if !self.starred {
                            out.push_str("*\n");
                            self.starred = true;
                        }
                        continue;
                    }
                    self.starred = false;
                    self.prev = Some(line.to_vec());
                    let _ = write!(out, "{:08x}  ", offset + (i * 16) as u64);
                    for k in 0..16 {
                        if k == 8 {
                            out.push(' ');
                        }
                        match line.get(k) {
                            Some(b) => {
                                let _ = write!(out, "{b:02x} ");
                            }
                            None => out.push_str("   "),
                        }
                    }
                    out.push_str(" |");
                    out.extend(line.iter().map(|&b| printable(b)));
                    out.push_str("|\n");
                }
            }
            DumpFormat::Markdown { .. } => {
                for (i, line) in bytes.chunks(w).enumerate() {
                    let _ = write!(out, "| {:08X} |", offset + (i * w) as u64);
                    for k in 0..w {
                        match line.get(k) {
                            Some(b) => {
                                let _ = write!(out, " {b:02X} |");
                            }
                            None => out.push_str("  |"),
                        }
                    }
                    out.push(' ');
                    for &b in line {
                        out.push_str(&markdown_escape(printable(b)));
                    }
                    out.push_str(" |\n");
                }
            }
            DumpFormat::Csv { .. } => {
                for (i, line) in bytes.chunks(w).enumerate() {
                    let _ = write!(out, "{:08X}", offset + (i * w) as u64);
                    for k in 0..w {
                        out.push(',');
                        if let Some(b) = line.get(k) {
                            let _ = write!(out, "{b:02X}");
                        }
                    }
                    let ascii: String = line.iter().map(|&b| printable(b)).collect();
                    let _ = write!(out, ",\"{}\"\r\n", ascii.replace('"', "\"\""));
                }
            }
            DumpFormat::CHeader { .. } => {
                for line in bytes.chunks(w) {
                    out.push_str("   ");
                    for b in line {
                        let _ = write!(out, " 0x{b:02X},");
                    }
                    out.push('\n');
                }
            }
            DumpFormat::Html | DumpFormat::Rtf => unreachable!("rich formats are not streamed"),
        }
    }

    fn end(&self, out: &mut String, end_offset: u64, len: u64) {
        match &self.format {
            DumpFormat::HexdumpC { .. } if len > 0 => {
                let _ = writeln!(out, "{end_offset:08x}");
            }
            DumpFormat::CHeader { name } => {
                let id = c_ident(name);
                let guard = format!("{}_H_", id.to_ascii_uppercase());
                let _ = write!(
                    out,
                    "}};\nstatic const size_t {id}_len = {len};\n\n#endif  // {guard}\n"
                );
            }
            _ => {}
        }
    }
}

/// Formats `bytes` (whose first byte is at `base`) as a dump.
pub fn dump(bytes: &[u8], base: u64, format: &DumpFormat) -> Result<String> {
    match format {
        DumpFormat::Html => return Ok(copy_as(bytes, &CopyFormat::Html { offset: base })),
        DumpFormat::Rtf => return Ok(copy_as(bytes, &CopyFormat::Rtf { offset: base })),
        _ => {}
    }
    let mut d = Dumper::new(format)?;
    let mut out = String::new();
    let len = bytes.len() as u64;
    d.begin(&mut out, base, len);
    d.rows(&mut out, base, bytes);
    d.end(&mut out, base + len, len);
    Ok(out)
}

/// Streams a dump of `[start, end)` of a snapshot into `out` (IO-03).
pub fn dump_range(
    tree: &Tree,
    start: u64,
    end: u64,
    format: &DumpFormat,
    out: &mut dyn std::io::Write,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<()> {
    let (start, end) = util::clamp(tree, start, end);
    let len = end - start;
    if matches!(format, DumpFormat::Html | DumpFormat::Rtf) {
        if len > MAX_RICH_DUMP {
            return Err(Error::TooLarge {
                len,
                limit: MAX_RICH_DUMP,
            });
        }
        let bytes = tree.read_vec(start, len as usize)?;
        out.write_all(dump(&bytes, start, format)?.as_bytes())?;
        progress(len, len);
        return Ok(());
    }
    let mut d = Dumper::new(format)?;
    let chunk = CHUNK / 1024 / d.width() * d.width() * 1024;
    let mut s = String::new();
    d.begin(&mut s, start, len);
    util::scan(tree, start, end, chunk, cancel, progress, |at, bytes| {
        d.rows(&mut s, at, bytes);
        out.write_all(s.as_bytes())?;
        s.clear();
        Ok(true)
    })?;
    d.end(&mut s, end, len);
    out.write_all(s.as_bytes())?;
    Ok(())
}

// ------------------------------------------------------------------ import

/// Text dump flavours understood by [`import_dump`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DumpTextFormat {
    Xxd,
    HexdumpC,
    /// `od -A x -t x1z` (also without the `z` column).
    Od,
    /// Hex digits with optional `0x`, `\x`, spaces, commas, colons (`xxd -p`).
    PlainHex,
}

/// Result of [`import_dump`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DumpImport {
    pub format: DumpTextFormat,
    /// Data by offset (several segments when offsets have gaps).
    pub segments: Vec<Segment>,
    /// Lines that could not be used.
    pub errors: Vec<LineError>,
    /// Non-fatal findings, e.g. an ASCII column that does not match the
    /// hex bytes (edited dumps; the hex wins as with `xxd -r`).
    pub warnings: Vec<LineError>,
}

#[derive(Debug, PartialEq, Eq)]
enum Parsed {
    Data {
        offset: Option<u64>,
        bytes: Vec<u8>,
        ascii_ok: bool,
    },
    /// `*`: previous line repeated up to the next offset.
    Repeat,
    /// Offset-only line ending hexdump / od output.
    End(u64),
}

fn hex_offset(s: &str) -> Option<u64> {
    (!s.is_empty() && s.len() <= 16 && s.bytes().all(|c| c.is_ascii_hexdigit()))
        .then(|| u64::from_str_radix(s, 16).ok())
        .flatten()
}

fn byte_tokens<'a>(tokens: impl Iterator<Item = &'a str>, group: bool) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    for t in tokens {
        let ok = t.bytes().all(|c| c.is_ascii_hexdigit())
            && if group {
                t.len() % 2 == 0 && !t.is_empty()
            } else {
                t.len() == 2
            };
        if !ok {
            return None;
        }
        for i in (0..t.len()).step_by(2) {
            out.push(u8::from_str_radix(&t[i..i + 2], 16).ok()?);
        }
    }
    Some(out)
}

fn ascii_matches(column: &str, bytes: &[u8]) -> bool {
    let expected: String = bytes.iter().map(|&b| printable(b)).collect();
    column == expected
}

fn parse_xxd_line(l: &str) -> std::result::Result<Parsed, String> {
    let (off, rest) = l.split_once(':').ok_or("missing ':' after offset")?;
    let offset = hex_offset(off.trim()).ok_or("invalid offset")?;
    let rest = rest.strip_prefix(' ').unwrap_or(rest);
    let (hex, ascii) = rest.split_once("  ").unwrap_or((rest, ""));
    let bytes = byte_tokens(hex.split_whitespace(), true).ok_or("invalid hex group")?;
    let ascii_ok = ascii.is_empty() || {
        let tail: String = l
            .chars()
            .rev()
            .take(bytes.len())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        ascii_matches(&tail, &bytes)
    };
    Ok(Parsed::Data {
        offset: Some(offset),
        bytes,
        ascii_ok,
    })
}

fn parse_delimited_line(
    l: &str,
    open: char,
    close: char,
    min_offset_len: usize,
) -> std::result::Result<Parsed, String> {
    if l == "*" {
        return Ok(Parsed::Repeat);
    }
    let (main, ascii) = match l.find(open) {
        Some(i) => (&l[..i], Some(&l[i + 1..])),
        None => (l, None),
    };
    let mut it = main.split_whitespace();
    let off = it.next().ok_or("empty line")?;
    if off.len() < min_offset_len {
        return Err("offset column is missing".into());
    }
    let offset = hex_offset(off).ok_or("invalid offset")?;
    let bytes = byte_tokens(it, false).ok_or("invalid hex byte")?;
    if bytes.is_empty() && ascii.is_none() {
        return Ok(Parsed::End(offset));
    }
    let ascii_ok = match ascii {
        Some(a) => {
            let col = a
                .strip_suffix(close)
                .ok_or_else(|| format!("missing closing '{close}'"))?;
            ascii_matches(col, &bytes)
        }
        None => true,
    };
    Ok(Parsed::Data {
        offset: Some(offset),
        bytes,
        ascii_ok,
    })
}

fn parse_plain_line(l: &str) -> std::result::Result<Parsed, String> {
    let cleaned = l
        .replace("0x", " ")
        .replace("0X", " ")
        .replace("\\x", " ")
        .replace([',', ':', ';', '-'], " ");
    let digits: String = cleaned.split_whitespace().collect();
    if !digits.len().is_multiple_of(2) {
        return Err("odd number of hex digits".into());
    }
    let bytes = byte_tokens(std::iter::once(digits.as_str()), true)
        .filter(|b| !b.is_empty())
        .ok_or("invalid hex digit")?;
    Ok(Parsed::Data {
        offset: None,
        bytes,
        ascii_ok: true,
    })
}

fn parse_line(format: DumpTextFormat, l: &str) -> std::result::Result<Parsed, String> {
    match format {
        DumpTextFormat::Xxd => parse_xxd_line(l),
        DumpTextFormat::HexdumpC => {
            if l == "*" {
                return Ok(Parsed::Repeat);
            }
            // `hexdump -C`: offset, then two spaces.
            let off_len = l.find(' ').unwrap_or(l.len());
            if off_len < l.len() && !l[off_len..].starts_with("  ") {
                return Err("expected two spaces after the offset".into());
            }
            parse_delimited_line(l, '|', '|', 7)
        }
        DumpTextFormat::Od => parse_delimited_line(l, '>', '<', 6),
        DumpTextFormat::PlainHex => parse_plain_line(l),
    }
}

/// Guesses the dump flavour from the first lines.
pub fn detect_dump_format(text: &str) -> Option<DumpTextFormat> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .take(50)
        .collect();
    if lines.is_empty() {
        return None;
    }
    // Data lines count twice, `*` / end-offset lines once; a format needs
    // at least one data line.
    let score = |f: DumpTextFormat| {
        let (mut data, mut other) = (0, 0);
        for l in &lines {
            match parse_line(f, l) {
                Ok(Parsed::Data { .. }) => data += 1,
                Ok(_) => other += 1,
                Err(_) => {}
            }
        }
        if data == 0 { 0 } else { 2 * data + other }
    };
    let hexdump_like = lines.iter().any(|l| l.contains('|'));
    let od_like = lines.iter().any(|l| l.contains('>'));
    let mut best: Option<(usize, DumpTextFormat)> = None;
    for f in [
        DumpTextFormat::HexdumpC,
        DumpTextFormat::Xxd,
        DumpTextFormat::Od,
        DumpTextFormat::PlainHex,
    ] {
        let mut s = score(f);
        // Delimiters decide between the offset-prefixed formats.
        if (f == DumpTextFormat::HexdumpC && !hexdump_like)
            || (f == DumpTextFormat::Od && hexdump_like && !od_like)
        {
            s /= 2;
        }
        if s > 0 && best.is_none_or(|(b, _)| s > b) {
            best = Some((s, f));
        }
    }
    best.map(|(_, f)| f)
}

/// Imports a text dump (IO-04), detecting its format unless given.
pub fn import_dump(text: &str, format: Option<DumpTextFormat>) -> Result<DumpImport> {
    let format = format
        .or_else(|| detect_dump_format(text))
        .ok_or_else(|| invalid("unrecognised dump format"))?;
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let mut pieces: Vec<(u64, Vec<u8>, usize)> = Vec::new();
    let mut cursor = 0u64;
    let mut prev: Option<Vec<u8>> = None;
    let mut repeat_pending = false;
    let err = |v: &mut Vec<LineError>, line, m: String| v.push(LineError { line, message: m });
    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        let l = raw.trim_end_matches(['\r', '\n']);
        if l.trim().is_empty() {
            continue;
        }
        let l = if format == DumpTextFormat::PlainHex {
            l
        } else {
            l.trim_start()
        };
        let parsed = match parse_line(format, l) {
            Ok(p) => p,
            Err(m) => {
                err(&mut errors, line, m);
                continue;
            }
        };
        let next_offset = match &parsed {
            Parsed::Data { offset, .. } => *offset,
            Parsed::End(o) => Some(*o),
            Parsed::Repeat => None,
        };
        if repeat_pending && let (Some(target), Some(p)) = (next_offset, prev.as_ref()) {
            repeat_pending = false;
            if target < cursor || p.is_empty() || !(target - cursor).is_multiple_of(p.len() as u64)
            {
                err(
                    &mut errors,
                    line,
                    format!("offset 0x{target:X} does not continue the repeated line"),
                );
            } else {
                let n = (target - cursor) / p.len() as u64;
                if n * p.len() as u64 > 1 << 30 {
                    err(&mut errors, line, "repeated block is too large".into());
                } else {
                    let mut block = Vec::with_capacity((n as usize) * p.len());
                    for _ in 0..n {
                        block.extend_from_slice(p);
                    }
                    pieces.push((cursor, block, line));
                    cursor = target;
                }
            }
        }
        match parsed {
            Parsed::Repeat => {
                if prev.is_none() {
                    err(&mut errors, line, "'*' without a preceding line".into());
                }
                repeat_pending = true;
            }
            Parsed::End(o) => cursor = o,
            Parsed::Data {
                offset,
                bytes,
                ascii_ok,
            } => {
                if !ascii_ok {
                    err(
                        &mut warnings,
                        line,
                        "ASCII column does not match the hex bytes".into(),
                    );
                }
                let at = offset.unwrap_or(cursor);
                cursor = at + bytes.len() as u64;
                pieces.push((at, bytes.clone(), line));
                prev = Some(bytes);
            }
        }
    }
    if repeat_pending {
        errors.push(LineError {
            line: text.lines().count() + 1,
            message: "'*' is not followed by an offset".into(),
        });
    }
    let (segments, overlaps) = merge(pieces);
    for (line, address) in overlaps {
        errors.push(LineError {
            line,
            message: format!("data at 0x{address:X} overlaps earlier data"),
        });
    }
    errors.sort_by_key(|e| e.line);
    Ok(DumpImport {
        format,
        segments,
        errors,
        warnings,
    })
}

/// Writes a dump of a range into a `String` via [`dump_range`].
pub fn dump_range_to_string(
    tree: &Tree,
    start: u64,
    end: u64,
    format: &DumpFormat,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<String> {
    let mut v = Vec::new();
    dump_range(tree, start, end, format, &mut v, cancel, progress)?;
    String::from_utf8(v).map_err(|_| invalid("dump is not UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{noop, random_bytes, tree};

    /// The bytes of `printf 'Hello, World!\\n\\x00\\x01\\x02\\x7f\\x80\\xff|'` plus a
    /// backtick, a backslash and `x`.
    fn d1() -> Vec<u8> {
        let mut d = b"Hello, World!\n".to_vec();
        d.extend([0, 1, 2, 0x7F, 0x80, 0xFF]);
        d.extend(b"|`\\x");
        d
    }

    /// 40 × 'A' followed by 0…19.
    fn d2() -> Vec<u8> {
        let mut d = vec![b'A'; 40];
        d.extend(0..20u8);
        d
    }

    // Captured from xxd(1) and hexdump(1).
    const D1_XXD: &str = "00000000: 4865 6c6c 6f2c 2057 6f72 6c64 210a 0001  Hello, World!...\n\
00000010: 027f 80ff 7c60 5c78                      ....|`\\x\n";
    const D1_HEXDUMP: &str = "00000000  48 65 6c 6c 6f 2c 20 57  6f 72 6c 64 21 0a 00 01  |Hello, World!...|\n\
00000010  02 7f 80 ff 7c 60 5c 78                           |....|`\\x|\n\
00000018\n";
    const D2_HEXDUMP: &str = "00000000  41 41 41 41 41 41 41 41  41 41 41 41 41 41 41 41  |AAAAAAAAAAAAAAAA|\n\
*\n\
00000020  41 41 41 41 41 41 41 41  00 01 02 03 04 05 06 07  |AAAAAAAA........|\n\
00000030  08 09 0a 0b 0c 0d 0e 0f  10 11 12 13              |............|\n\
0000003c\n";
    // GNU od(1) layout.
    const D1_OD: &str = "000000 48 65 6c 6c 6f 2c 20 57 6f 72 6c 64 21 0a 00 01  >Hello, World!...<\n\
000010 02 7f 80 ff 7c 60 5c 78                          >....|`\\x<\n\
000018\n";

    #[test]
    fn text_dumps_match_the_tools() {
        assert_eq!(dump(&d1(), 0, &DumpFormat::Xxd).unwrap(), D1_XXD);
        let hd = DumpFormat::HexdumpC { squeeze: true };
        assert_eq!(dump(&d1(), 0, &hd).unwrap(), D1_HEXDUMP);
        assert_eq!(dump(&d2(), 0, &hd).unwrap(), D2_HEXDUMP);
        let plain = dump(&d2(), 0, &DumpFormat::HexdumpC { squeeze: false }).unwrap();
        assert_eq!(plain.lines().count(), 5);
        assert_eq!(dump(&[], 0, &hd).unwrap(), "");
    }

    #[test]
    fn table_and_source_dumps() {
        let md = dump(b"a|b", 0x10, &DumpFormat::Markdown { width: 4 }).unwrap();
        assert_eq!(
            md,
            "| Offset | 00 | 01 | 02 | 03 | ASCII |\n|---|---|---|---|---|---|\n\
| 00000010 | 61 | 7C | 62 |  | a\\|b |\n"
        );
        let csv = dump(b"x\"y", 0, &DumpFormat::Csv { width: 2 }).unwrap();
        assert_eq!(
            csv,
            "Offset,00,01,ASCII\r\n00000000,78,22,\"x\"\"\"\r\n00000002,79,,\"y\"\r\n"
        );
        let h = dump(
            &[1, 2, 255],
            0x40,
            &DumpFormat::CHeader {
                name: "boot-rom".into(),
            },
        )
        .unwrap();
        assert_eq!(
            h,
            "// 3 bytes from offset 0x40.\n#ifndef BOOT_ROM_H_\n#define BOOT_ROM_H_\n\n\
#include <stddef.h>\n#include <stdint.h>\n\n\
static const uint8_t boot_rom[3] = {\n    0x01, 0x02, 0xFF,\n};\n\
static const size_t boot_rom_len = 3;\n\n#endif  // BOOT_ROM_H_\n"
        );
        let html = dump(b"<a>", 0, &DumpFormat::Html).unwrap();
        assert!(html.starts_with("<pre") && html.contains("&lt;a&gt;"));
        let rtf = dump(b"{x}", 0, &DumpFormat::Rtf).unwrap();
        assert!(rtf.starts_with("{\\rtf1") && rtf.contains("\\{x\\}"));
        assert!(dump(b"", 0, &DumpFormat::Csv { width: 0 }).is_err());
        assert_eq!(c_ident("9lives"), "data_9lives");
    }

    #[test]
    fn streaming_matches_in_memory() {
        let mut data = random_bytes(1, 5_000_000);
        data[1_000_000..2_500_000].fill(0);
        let t = tree(&data);
        for f in [
            DumpFormat::Xxd,
            DumpFormat::HexdumpC { squeeze: true },
            DumpFormat::Markdown { width: 10 },
            DumpFormat::Csv { width: 7 },
            DumpFormat::CHeader { name: "x".into() },
        ] {
            let a = dump_range_to_string(&t, 3, 4_900_003, &f, &Cancel::new(), &noop).unwrap();
            let b = dump(&data[3..4_900_003], 3, &f).unwrap();
            assert!(a == b, "{f:?}");
        }
        let html =
            dump_range_to_string(&t, 0, 100, &DumpFormat::Html, &Cancel::new(), &noop).unwrap();
        assert_eq!(html, dump(&data[..100], 0, &DumpFormat::Html).unwrap());
        assert!(
            dump_range_to_string(&t, 0, 5_000_000, &DumpFormat::Rtf, &Cancel::new(), &noop).is_ok()
        );
    }

    fn import_one(text: &str) -> DumpImport {
        import_dump(text, None).unwrap()
    }

    fn single(imp: &DumpImport) -> &[u8] {
        assert_eq!(imp.segments.len(), 1, "{imp:?}");
        assert_eq!(imp.segments[0].address, 0);
        &imp.segments[0].data
    }

    #[test]
    fn imports_tool_output() {
        let x = import_one(D1_XXD);
        assert_eq!(x.format, DumpTextFormat::Xxd);
        assert!(x.errors.is_empty() && x.warnings.is_empty(), "{x:?}");
        assert_eq!(single(&x), d1());

        let h = import_one(D2_HEXDUMP);
        assert_eq!(h.format, DumpTextFormat::HexdumpC);
        assert!(h.errors.is_empty() && h.warnings.is_empty(), "{h:?}");
        assert_eq!(single(&h), d2());
        let h1 = import_one(D1_HEXDUMP);
        assert_eq!(single(&h1), d1());

        let o = import_one(D1_OD);
        assert_eq!(o.format, DumpTextFormat::Od);
        assert!(o.errors.is_empty() && o.warnings.is_empty(), "{o:?}");
        assert_eq!(single(&o), d1());
        let od_star =
            "000000 41 41 41 41 41 41 41 41 41 41 41 41 41 41 41 41\n*\n000030 42\n000031\n";
        let o = import_one(od_star);
        assert_eq!(o.format, DumpTextFormat::Od);
        let mut want = vec![b'A'; 48];
        want.push(b'B');
        assert_eq!(single(&o), want);

        let p = import_one("48656c6c6f2c20576f726c64210a00\n0x01, 0x02 \\x7f\n");
        assert_eq!(p.format, DumpTextFormat::PlainHex);
        assert_eq!(single(&p)[..5], *b"Hello");
        assert_eq!(single(&p).len(), 18);
    }

    #[test]
    fn roundtrip_random() {
        let data = random_bytes(4, 3000);
        for f in [DumpFormat::Xxd, DumpFormat::HexdumpC { squeeze: true }] {
            let text = dump(&data, 0x1000, &f).unwrap();
            let imp = import_one(&text);
            assert!(imp.errors.is_empty() && imp.warnings.is_empty());
            assert_eq!(
                imp.segments,
                vec![Segment {
                    address: 0x1000,
                    data: data.clone()
                }]
            );
        }
    }

    #[test]
    fn error_report() {
        let text = "00000000: 4865 6c6c  Hel.\n00000004: zz12  ..\n00000010: 4142  AB\n";
        let imp = import_dump(text, Some(DumpTextFormat::Xxd)).unwrap();
        assert_eq!(imp.errors.len(), 1);
        assert_eq!(imp.errors[0].line, 2);
        assert_eq!(imp.warnings.len(), 1);
        assert_eq!(imp.warnings[0].line, 1);
        assert_eq!(imp.segments.len(), 2);
        assert_eq!(imp.segments[1].address, 0x10);
        let bad = import_dump("*\n", Some(DumpTextFormat::HexdumpC)).unwrap();
        assert_eq!(bad.errors.len(), 2);
        assert!(import_dump("", None).is_err());
        assert!(import_dump("hello there", None).is_err());
    }
}
