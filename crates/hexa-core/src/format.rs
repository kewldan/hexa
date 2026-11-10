//! "Copy as" formatters and clipboard parsing (EDIT-10…16).

use std::fmt::Write as _;

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Output formats for "Copy as".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CopyFormat {
    HexSpaced,
    Hex,
    Hex0x,
    Escaped,
    C,
    Cpp,
    CSharp,
    Rust,
    Go,
    Java,
    Kotlin,
    Swift,
    JavaScript,
    TypeScript,
    Python,
    Pascal,
    VisualBasic,
    PowerShell,
    Asm,
    Base64,
    Base32,
    Base85,
    Url,
    Uuencode,
    Xxd {
        offset: u64,
    },
    Html {
        offset: u64,
    },
    Rtf {
        offset: u64,
    },
    /// User template (EDIT-15). In `item`: `{X}`/`{x}` hex, `{d}` decimal,
    /// `{o}` octal, `{b}` binary, `{c}` ASCII character, `{i}` index.
    #[serde(rename_all = "camelCase")]
    Template {
        prefix: String,
        item: String,
        separator: String,
        per_line: usize,
        line_prefix: String,
        suffix: String,
    },
}

/// Formats that produce a language array literal.
#[derive(Clone, Copy)]
struct ArrayStyle {
    head: &'static str,
    tail: &'static str,
    item: fn(u8) -> String,
}

fn h0x(b: u8) -> String {
    format!("0x{b:02X}")
}

fn java_byte(b: u8) -> String {
    if b > 0x7F {
        format!("(byte) 0x{b:02X}")
    } else {
        format!("0x{b:02X}")
    }
}

fn kotlin_byte(b: u8) -> String {
    if b > 0x7F {
        format!("0x{b:02X}.toByte()")
    } else {
        format!("0x{b:02X}")
    }
}

fn array(bytes: &[u8], style: ArrayStyle) -> String {
    let n = bytes.len();
    let mut s = style
        .head
        .replace("{n}", &n.to_string())
        .replace("{last}", &n.saturating_sub(1).to_string());
    for (i, line) in bytes.chunks(16).enumerate() {
        if i > 0 {
            s.push_str(",\n");
        }
        s.push_str("    ");
        let items: Vec<String> = line.iter().map(|&b| (style.item)(b)).collect();
        s.push_str(&items.join(", "));
    }
    s.push_str(style.tail);
    s
}

fn base32(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::new();
    for chunk in bytes.chunks(5) {
        let mut buf = [0u8; 5];
        buf[..chunk.len()].copy_from_slice(chunk);
        let v = buf.iter().fold(0u64, |acc, &b| (acc << 8) | u64::from(b));
        let chars = (chunk.len() * 8).div_ceil(5);
        for i in 0..8 {
            if i < chars {
                out.push(ALPHABET[((v >> (35 - i * 5)) & 31) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Ascii85 (Adobe variant without delimiters, `z` for zero groups).
fn base85(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(4) {
        let mut buf = [0u8; 4];
        buf[..chunk.len()].copy_from_slice(chunk);
        let mut v = u32::from_be_bytes(buf);
        if v == 0 && chunk.len() == 4 {
            out.push('z');
            continue;
        }
        let mut digits = [0u8; 5];
        for d in digits.iter_mut().rev() {
            *d = (v % 85) as u8 + b'!';
            v /= 85;
        }
        for &d in &digits[..=chunk.len()] {
            out.push(d as char);
        }
    }
    out
}

fn uuencode(bytes: &[u8]) -> String {
    let enc = |v: u8| if v == 0 { '`' } else { (v + 32) as char };
    let mut out = String::from("begin 644 data\n");
    for line in bytes.chunks(45) {
        out.push(enc(line.len() as u8));
        for c in line.chunks(3) {
            let mut b = [0u8; 3];
            b[..c.len()].copy_from_slice(c);
            out.push(enc(b[0] >> 2));
            out.push(enc(((b[0] & 3) << 4) | (b[1] >> 4)));
            out.push(enc(((b[1] & 15) << 2) | (b[2] >> 6)));
            out.push(enc(b[2] & 63));
        }
        out.push('\n');
    }
    out.push_str("`\nend\n");
    out
}

fn printable(b: u8) -> char {
    if (0x20..0x7F).contains(&b) {
        b as char
    } else {
        '.'
    }
}

/// xxd-style dump.
pub fn xxd(bytes: &[u8], offset: u64) -> String {
    let mut s = String::new();
    for (i, line) in bytes.chunks(16).enumerate() {
        let _ = write!(s, "{:08x}: ", offset + i as u64 * 16);
        let mut hex = String::new();
        for (j, b) in line.iter().enumerate() {
            let _ = write!(hex, "{b:02x}");
            if j % 2 == 1 {
                hex.push(' ');
            }
        }
        let _ = write!(s, "{hex:<40} ");
        s.extend(line.iter().map(|&b| printable(b)));
        s.push('\n');
    }
    s
}

/// Byte category used for coloured output (DISP-13).
pub fn category(b: u8) -> &'static str {
    match b {
        0x00 => "null",
        0xFF => "ff",
        b'\t' | b'\n' | b'\r' | b' ' => "space",
        0x21..=0x7E => "print",
        0x01..=0x1F | 0x7F => "ctrl",
        _ => "high",
    }
}

fn category_color(cat: &str) -> (u8, u8, u8) {
    match cat {
        "null" => (0x9C, 0x9C, 0xA5),
        "ff" => (0xC2, 0x41, 0x0C),
        "space" => (0x2F, 0x6B, 0xF0),
        "print" => (0x1D, 0x1D, 0x1F),
        "ctrl" => (0xB4, 0x23, 0x18),
        _ => (0x7A, 0x3E, 0xC8),
    }
}

fn html_escape(c: char) -> String {
    match c {
        '<' => "&lt;".into(),
        '>' => "&gt;".into(),
        '&' => "&amp;".into(),
        c => c.to_string(),
    }
}

fn html(bytes: &[u8], offset: u64) -> String {
    let mut s =
        String::from("<pre style=\"font-family:ui-monospace,Menlo,monospace;font-size:12px\">\n");
    for (i, line) in bytes.chunks(16).enumerate() {
        let _ = write!(
            s,
            "<span style=\"color:#6B6B73\">{:08X}</span>  ",
            offset + i as u64 * 16
        );
        for &b in line {
            let (r, g, bl) = category_color(category(b));
            let _ = write!(
                s,
                "<span style=\"color:#{r:02X}{g:02X}{bl:02X}\">{b:02X}</span> "
            );
        }
        s.push_str(&"   ".repeat(16 - line.len()));
        s.push(' ');
        for &b in line {
            s.push_str(&html_escape(printable(b)));
        }
        s.push('\n');
    }
    s.push_str("</pre>\n");
    s
}

fn rtf(bytes: &[u8], offset: u64) -> String {
    let cats = ["null", "ff", "space", "print", "ctrl", "high"];
    let mut s = String::from(
        "{\\rtf1\\ansi\\deff0{\\fonttbl{\\f0\\fmodern Menlo;}}{\\colortbl;\\red107\\green107\\blue115;",
    );
    for c in cats {
        let (r, g, b) = category_color(c);
        let _ = write!(s, "\\red{r}\\green{g}\\blue{b};");
    }
    s.push_str("}\\f0\\fs22\n");
    for (i, line) in bytes.chunks(16).enumerate() {
        let _ = write!(s, "\\cf1 {:08X}  ", offset + i as u64 * 16);
        for &b in line {
            let idx = cats.iter().position(|c| *c == category(b)).unwrap_or(3) + 2;
            let _ = write!(s, "\\cf{idx} {b:02X} ");
        }
        s.push_str("\\cf4  ");
        for &b in line {
            match printable(b) {
                c @ ('\\' | '{' | '}') => {
                    s.push('\\');
                    s.push(c);
                }
                c => s.push(c),
            }
        }
        s.push_str("\\par\n");
    }
    s.push('}');
    s
}

fn template(
    bytes: &[u8],
    prefix: &str,
    item: &str,
    separator: &str,
    per_line: usize,
    line_prefix: &str,
    suffix: &str,
) -> String {
    let mut s = prefix.to_owned();
    for (i, &b) in bytes.iter().enumerate() {
        if i > 0 {
            s.push_str(separator);
            if per_line > 0 && i % per_line == 0 {
                s.push('\n');
            }
        }
        if per_line > 0 && i % per_line == 0 {
            s.push_str(line_prefix);
        }
        s.push_str(
            &item
                .replace("{X}", &format!("{b:02X}"))
                .replace("{x}", &format!("{b:02x}"))
                .replace("{d}", &b.to_string())
                .replace("{o}", &format!("{b:03o}"))
                .replace("{b}", &format!("{b:08b}"))
                .replace("{c}", &printable(b).to_string())
                .replace("{i}", &i.to_string()),
        );
    }
    s.push_str(suffix);
    s
}

/// Formats bytes for the clipboard.
pub fn copy_as(bytes: &[u8], format: &CopyFormat) -> String {
    let hex_join = |sep: &str, f: &dyn Fn(u8) -> String| {
        bytes.iter().map(|&b| f(b)).collect::<Vec<_>>().join(sep)
    };
    let st = |head, tail, item| ArrayStyle { head, tail, item };
    match format {
        CopyFormat::HexSpaced => hex_join(" ", &|b| format!("{b:02X}")),
        CopyFormat::Hex => hex_join("", &|b| format!("{b:02X}")),
        CopyFormat::Hex0x => hex_join(", ", &h0x),
        CopyFormat::Escaped => hex_join("", &|b| format!("\\x{b:02X}")),
        CopyFormat::C => array(bytes, st("unsigned char data[{n}] = {\n", "\n};", h0x)),
        CopyFormat::Cpp => array(
            bytes,
            st(
                "constexpr std::array<std::uint8_t, {n}> data = {\n",
                "\n};",
                h0x,
            ),
        ),
        CopyFormat::CSharp => array(bytes, st("byte[] data = new byte[{n}] {\n", "\n};", h0x)),
        CopyFormat::Rust => array(bytes, st("const DATA: [u8; {n}] = [\n", "\n];", h0x)),
        CopyFormat::Go => array(bytes, st("var data = []byte{\n", ",\n}", h0x)),
        CopyFormat::Java => array(bytes, st("byte[] data = {\n", "\n};", java_byte)),
        CopyFormat::Kotlin => array(bytes, st("val data = byteArrayOf(\n", "\n)", kotlin_byte)),
        CopyFormat::Swift => array(bytes, st("let data: [UInt8] = [\n", "\n]", h0x)),
        CopyFormat::JavaScript => array(bytes, st("const data = new Uint8Array([\n", "\n]);", h0x)),
        CopyFormat::TypeScript => array(
            bytes,
            st("const data: Uint8Array = new Uint8Array([\n", "\n]);", h0x),
        ),
        CopyFormat::Python => array(bytes, st("data = bytes([\n", "\n])", h0x)),
        CopyFormat::Pascal => array(
            bytes,
            st("const Data: array[0..{last}] of Byte = (\n", "\n);", |b| {
                format!("${b:02X}")
            }),
        ),
        CopyFormat::VisualBasic => array(
            bytes,
            st("Dim data As Byte() = {\n", "\n}", |b| format!("&H{b:02X}")),
        ),
        CopyFormat::PowerShell => array(bytes, st("[byte[]] $data = @(\n", "\n)", h0x)),
        CopyFormat::Asm => bytes
            .chunks(16)
            .map(|l| {
                format!(
                    "    db {}",
                    l.iter()
                        .map(|&b| format!("0x{b:02X}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        CopyFormat::Base64 => base64::engine::general_purpose::STANDARD.encode(bytes),
        CopyFormat::Base32 => base32(bytes),
        CopyFormat::Base85 => base85(bytes),
        CopyFormat::Url => bytes
            .iter()
            .map(|&b| {
                if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                    (b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect(),
        CopyFormat::Uuencode => uuencode(bytes),
        CopyFormat::Xxd { offset } => xxd(bytes, *offset),
        CopyFormat::Html { offset } => html(bytes, *offset),
        CopyFormat::Rtf { offset } => rtf(bytes, *offset),
        CopyFormat::Template {
            prefix,
            item,
            separator,
            per_line,
            line_prefix,
            suffix,
        } => template(
            bytes,
            prefix,
            item,
            separator,
            *per_line,
            line_prefix,
            suffix,
        ),
    }
}

/// A way to interpret clipboard text as bytes (EDIT-16).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PasteCandidate {
    pub kind: &'static str,
    pub label: &'static str,
    pub bytes: Vec<u8>,
}

fn parse_hex_string(text: &str) -> Option<Vec<u8>> {
    let cleaned: String = text
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, ':' | '-' | ','))
        .collect();
    let cleaned = cleaned.strip_prefix("0x").unwrap_or(&cleaned);
    if cleaned.is_empty()
        || !cleaned.len().is_multiple_of(2)
        || !cleaned.chars().all(|c| c.is_ascii_hexdigit())
    {
        return None;
    }
    (0..cleaned.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&cleaned[i..i + 2], 16).ok())
        .collect()
}

fn parse_xxd(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut lines = 0;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let (addr, rest) = line.split_once(':')?;
        if addr.trim().is_empty() || !addr.trim().chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let hex_part = rest.split("  ").find(|s| !s.trim().is_empty())?;
        let bytes = parse_hex_string(hex_part)?;
        out.extend(bytes);
        lines += 1;
    }
    (lines > 0).then_some(out)
}

fn parse_array(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut tokens = 0;
    let mut covered = 0;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let rest = &text[i..];
        let (num, len, radix) =
            if let Some(h) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
                let n = h.chars().take_while(char::is_ascii_hexdigit).count();
                (&h[..n], n + 2, 16)
            } else if let Some(h) = rest.strip_prefix("\\x") {
                let n = h
                    .chars()
                    .take(2)
                    .take_while(char::is_ascii_hexdigit)
                    .count();
                (&h[..n], n + 2, 16)
            } else if let Some(h) = rest.strip_prefix('$').or_else(|| rest.strip_prefix("&H")) {
                let skip = rest.len() - h.len();
                let n = h.chars().take_while(char::is_ascii_hexdigit).count();
                (&h[..n], n + skip, 16)
            } else {
                i += rest.chars().next().map_or(1, char::len_utf8);
                continue;
            };
        if num.is_empty() || num.len() > 2 {
            return None;
        }
        out.push(u8::from_str_radix(num, radix).ok()?);
        tokens += 1;
        covered += len;
        i += len;
    }
    (tokens > 0 && covered * 3 >= text.trim().len()).then_some(out)
}

fn parse_base64(text: &str) -> Option<Vec<u8>> {
    let cleaned: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if cleaned.len() < 4 {
        return None;
    }
    base64::engine::general_purpose::STANDARD
        .decode(&cleaned)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(&cleaned))
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(&cleaned))
        .ok()
}

/// All plausible interpretations, most likely first. Raw text is always last.
pub fn parse_clipboard(text: &str) -> Vec<PasteCandidate> {
    let mut out = Vec::new();
    if let Some(b) = parse_xxd(text) {
        out.push(PasteCandidate {
            kind: "xxd",
            label: "Hex dump (xxd)",
            bytes: b,
        });
    }
    if let Some(b) = parse_hex_string(text) {
        out.push(PasteCandidate {
            kind: "hex",
            label: "Hex string",
            bytes: b,
        });
    } else if let Some(b) = parse_array(text) {
        out.push(PasteCandidate {
            kind: "array",
            label: "Byte array / escaped",
            bytes: b,
        });
    }
    if let Some(b) = parse_base64(text)
        && !out.iter().any(|c| c.kind == "hex")
    {
        out.push(PasteCandidate {
            kind: "base64",
            label: "Base64",
            bytes: b,
        });
    }
    out.push(PasteCandidate {
        kind: "text",
        label: "Text (UTF-8)",
        bytes: text.as_bytes().to_vec(),
    });
    out
}

/// Parses user input of a hex byte string (dialogs, fill patterns).
pub fn parse_hex_bytes(text: &str) -> Result<Vec<u8>> {
    parse_hex_string(text).ok_or_else(|| Error::invalid("expected hex bytes like 'DE AD BE EF'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_formats() {
        let b = [0xDE, 0xAD, 0x00, 0x41];
        assert_eq!(copy_as(&b, &CopyFormat::HexSpaced), "DE AD 00 41");
        assert_eq!(copy_as(&b, &CopyFormat::Escaped), "\\xDE\\xAD\\x00\\x41");
        assert_eq!(copy_as(&b, &CopyFormat::Base64), "3q0AQQ==");
        assert_eq!(copy_as(b"foobar", &CopyFormat::Base32), "MZXW6YTBOI======");
        assert_eq!(copy_as(b"Man ", &CopyFormat::Base85), "9jqo^");
        assert_eq!(copy_as(&[0, 0, 0, 0, 1], &CopyFormat::Base85), "z!<");
        assert_eq!(copy_as(b"a b/", &CopyFormat::Url), "a%20b%2F");
        assert!(
            copy_as(&b, &CopyFormat::C).starts_with("unsigned char data[4] = {\n    0xDE, 0xAD")
        );
        assert!(copy_as(&b, &CopyFormat::Java).contains("(byte) 0xDE"));
        assert!(copy_as(&b, &CopyFormat::Pascal).contains("array[0..3]"));
        assert_eq!(
            copy_as(b"Cat", &CopyFormat::Uuencode),
            "begin 644 data\n#0V%T\n`\nend\n"
        );
    }

    #[test]
    fn xxd_roundtrip() {
        let data: Vec<u8> = (0..40).collect();
        let dump = xxd(&data, 0x100);
        assert!(dump.starts_with("00000100: 0001 0203"));
        let c = parse_clipboard(&dump);
        assert_eq!(c[0].kind, "xxd");
        assert_eq!(c[0].bytes, data);
    }

    #[test]
    fn clipboard_detection() {
        assert_eq!(
            parse_clipboard("de ad be ef")[0].bytes,
            [0xDE, 0xAD, 0xBE, 0xEF]
        );
        let arr = parse_clipboard("{ 0x01, 0x02, 0xFF }");
        assert_eq!(arr[0].kind, "array");
        assert_eq!(arr[0].bytes, [1, 2, 255]);
        assert_eq!(parse_clipboard("\\x41\\x42")[0].bytes, b"AB");
        let b64 = parse_clipboard("SGVsbG8=");
        assert_eq!(b64[0].kind, "base64");
        assert_eq!(b64[0].bytes, b"Hello");
        let t = parse_clipboard("just text");
        assert_eq!(t.last().unwrap().kind, "text");
    }

    #[test]
    fn templates() {
        let f = CopyFormat::Template {
            prefix: "[".into(),
            item: "{d}".into(),
            separator: ",".into(),
            per_line: 0,
            line_prefix: String::new(),
            suffix: "]".into(),
        };
        assert_eq!(copy_as(&[1, 2, 255], &f), "[1,2,255]");
    }
}
