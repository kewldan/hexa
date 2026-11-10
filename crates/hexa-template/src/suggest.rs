//! Structure from selection (STRC-20): typed interpretations of selected
//! bytes ranked by plausibility, and a builder that turns fields marked by
//! the user into template source.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::Endian;
use crate::diag::{Error, Result};
use crate::prim::{Prim, decode_uint, sign_extend};

/// One way to read the selected bytes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Suggestion {
    /// Template type (`le u32`, `be u16`, `char[4]`, `string`, ...).
    pub ty: String,
    /// The value under that interpretation.
    pub value: String,
    /// 0.0 ..= 1.0.
    pub confidence: f32,
    pub reason: String,
}

const MAGICS: &[(&[u8], &str)] = &[
    (b"\x89PNG", "PNG signature"),
    (b"\x7fELF", "ELF magic"),
    (b"MZ", "DOS/PE header"),
    (b"PK\x03\x04", "ZIP local file header"),
    (b"PK\x01\x02", "ZIP central directory header"),
    (b"PK\x05\x06", "ZIP end of central directory"),
    (b"\xca\xfe\xba\xbe", "Java class / universal Mach-O"),
    (b"\xcf\xfa\xed\xfe", "Mach-O 64-bit"),
    (b"\xfe\xed\xfa\xce", "Mach-O 32-bit (big endian)"),
    (b"\x1f\x8b", "GZIP header"),
    (b"%PDF", "PDF header"),
    (b"GIF8", "GIF header"),
    (b"\xff\xd8\xff", "JPEG start of image"),
    (b"RIFF", "RIFF container"),
    (b"\0asm", "WebAssembly module"),
    (b"SQLite format 3", "SQLite header"),
    (b"dex\n", "DEX header"),
    (b"BM", "BMP header"),
    (b"ID3", "ID3v2 tag"),
    (b"EFI PART", "GPT header"),
    (b"\x55\xaa", "boot signature"),
];

/// Ranks typed interpretations of `bytes` (a selection inside data of
/// `file_len` bytes): integers of 1/2/4/8 bytes in both byte orders as
/// plausible lengths/offsets/counts, timestamps (Unix, FILETIME), floats,
/// ASCII / UTF-16 strings and magic-looking constants.
pub fn suggest(bytes: &[u8], file_len: u64) -> Vec<Suggestion> {
    let mut out: Vec<Suggestion> = Vec::new();
    let len = bytes.len();
    if len == 0 {
        return out;
    }
    let mut add = |ty: String, value: String, confidence: f32, reason: String| {
        out.push(Suggestion {
            ty,
            value,
            confidence: confidence.clamp(0.0, 1.0),
            reason,
        });
    };

    for &(magic, what) in MAGICS {
        if bytes.starts_with(magic) {
            add(
                format!("char[{}]", magic.len()),
                crate::eval::quote(&crate::eval::decode_8bit(magic)),
                0.95,
                format!("matches the {what}"),
            );
        }
    }

    // Strings.
    let printable = |b: u8| b.is_ascii_graphic() || b == b' ' || b == b'\t';
    let nul = bytes.iter().position(|&b| b == 0);
    let text_len = nul.unwrap_or(len);
    let ascii_run = bytes[..text_len].iter().all(|&b| printable(b));
    if text_len >= 2 && ascii_run {
        let s = String::from_utf8_lossy(&bytes[..text_len]).into_owned();
        let quoted = crate::eval::quote(&s);
        if nul.is_some() {
            add(
                "string".into(),
                quoted.clone(),
                0.85,
                format!("{text_len} printable characters followed by NUL"),
            );
        }
        let whole = nul.is_none_or(|n| bytes[n..].iter().all(|&b| b == 0));
        if whole {
            let conf = if text_len == len { 0.8 } else { 0.75 };
            let what = if len == 4 && text_len == 4 {
                " (looks like a four-character code)"
            } else {
                ""
            };
            add(
                format!("char[{len}]"),
                quoted,
                conf,
                format!("printable ASCII{what}"),
            );
        }
    }
    if len >= 4 && len.is_multiple_of(2) {
        for (endian, name) in [(Endian::Little, "le"), (Endian::Big, "be")] {
            let units: Vec<u16> = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| decode_uint(c, endian) as u16)
                .collect();
            let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
            if end >= 2 && units[..end].iter().all(|&u| u < 0x80 && printable(u as u8)) {
                let s = String::from_utf16_lossy(&units[..end]);
                let ty = if end < units.len() {
                    format!("{name} wstring")
                } else {
                    format!("{name} char16[{}]", units.len())
                };
                add(ty, crate::eval::quote(&s), 0.8, "UTF-16 text".into());
            }
        }
    }

    // Integers.
    for width in [1usize, 2, 4, 8] {
        if len < width {
            continue;
        }
        let exact = len == width;
        let base = if exact { 0.25 } else { 0.1 };
        let chunk = &bytes[..width];
        let orders: &[(Endian, &str)] = if width == 1 {
            &[(Endian::Little, "")]
        } else {
            &[(Endian::Little, "le "), (Endian::Big, "be ")]
        };
        for &(endian, prefix) in orders {
            let u = decode_uint(chunk, endian) as u64;
            let ty = format!("{prefix}u{}", width * 8);
            let mut conf = base;
            let mut reasons = Vec::new();
            if u == 0 {
                reasons.push("zero".to_string());
                conf += 0.05;
            } else if u < file_len && width >= 2 {
                reasons.push(format!(
                    "fits inside the data (possible offset/length; data is {file_len} bytes)"
                ));
                conf += 0.3;
                if u <= 4096 {
                    reasons.push("small enough to be a count".into());
                    conf += 0.1;
                }
            } else if width >= 2 && u <= 4096 {
                reasons.push("small value (count/length?)".into());
                conf += 0.2;
            }
            if width >= 2 && u > 0 && u.is_power_of_two() {
                reasons.push("power of two (size/alignment?)".into());
                conf += 0.05;
            }
            if width == 4 && (946_684_800..4_102_444_800).contains(&u) {
                reasons.push(format!("plausible Unix time {}", unix_text(u as i64)));
                conf += 0.25;
            }
            if width == 8 {
                let secs = (u / 10_000_000) as i64 - 11_644_473_600;
                if (631_152_000..4_102_444_800).contains(&secs) {
                    reasons.push(format!("plausible FILETIME {}", unix_text(secs)));
                    conf += 0.35;
                } else if (946_684_800..4_102_444_800).contains(&(u as i64)) {
                    reasons.push(format!(
                        "plausible 64-bit Unix time {}",
                        unix_text(u as i64)
                    ));
                    conf += 0.25;
                }
            }
            if width >= 2 && u > 0x00FF_FFFF && chunk.iter().all(|&b| b.is_ascii_alphanumeric()) {
                reasons.push("all bytes are letters/digits (magic constant?)".into());
                conf += 0.2;
            }
            if reasons.is_empty() {
                reasons.push(if u >= file_len && width >= 4 {
                    "large value".into()
                } else {
                    "integer".into()
                });
            }
            // Penalize the byte order that makes a small value huge.
            if width >= 2 {
                let other = decode_uint(
                    chunk,
                    if endian == Endian::Little {
                        Endian::Big
                    } else {
                        Endian::Little
                    },
                ) as u64;
                if other < u && other != 0 {
                    conf -= 0.15;
                }
            }
            let value = format!("{u} (0x{u:X})");
            add(ty.clone(), value, conf, reasons.join("; "));
            let s = sign_extend(u128::from(u), (width * 8) as u32);
            if s < 0 && s > -4096 {
                add(
                    ty.replacen('u', "s", 1),
                    s.to_string(),
                    conf - 0.05,
                    "small negative signed value".into(),
                );
            }
        }
    }

    // Floats.
    for (width, prim) in [(4usize, Prim::F32), (8, Prim::F64)] {
        if len < width {
            continue;
        }
        for (endian, prefix) in [(Endian::Little, "le "), (Endian::Big, "be ")] {
            let raw = decode_uint(&bytes[..width], endian);
            let f = if prim == Prim::F32 {
                f64::from(f32::from_bits(raw as u32))
            } else {
                f64::from_bits(raw as u64)
            };
            if !f.is_finite() || f == 0.0 {
                continue;
            }
            let a = f.abs();
            if (1e-6..1e9).contains(&a) {
                let round = (f * 1000.0).fract().abs() < 1e-9;
                let conf = if round { 0.55 } else { 0.35 } + if len == width { 0.1 } else { 0.0 };
                let name = if prim == Prim::F32 { "float" } else { "double" };
                add(
                    format!("{prefix}{name}"),
                    format!("{f}"),
                    conf,
                    "plausible floating point magnitude".into(),
                );
            }
        }
    }

    out.sort_by(|a, b| {
        b.confidence
            .total_cmp(&a.confidence)
            .then_with(|| a.ty.cmp(&b.ty))
    });
    out.dedup_by(|a, b| a.ty == b.ty && a.value == b.value);
    out
}

fn unix_text(t: i64) -> String {
    let days = t.div_euclid(86_400);
    let secs = t.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

/// A field marked by the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuiltField {
    /// Offset relative to the structure start.
    pub offset: u64,
    pub size: u64,
    pub name: String,
    /// Template type, e.g. `be u32`, `char[4]`, `u8[16]`.
    pub ty: String,
    pub comment: Option<String>,
}

/// Accumulates fields marked on the hex grid and emits a template
/// (STRC-20).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StructBuilder {
    pub name: String,
    pub fields: Vec<BuiltField>,
}

fn valid_ident(s: &str) -> bool {
    let mut c = s.chars();
    c.next()
        .is_some_and(|f| f.is_ascii_alphabetic() || f == '_')
        && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
}

/// `(endian prefix, base type, array count)` of a builder type spec.
fn split_type(ty: &str) -> Result<(Option<&str>, &str, Option<u64>)> {
    let ty = ty.trim();
    let (endian, rest) = match ty.split_once(' ') {
        Some((e @ ("le" | "be"), r)) => (Some(e), r.trim()),
        _ => (None, ty),
    };
    let (base, count) = match rest.split_once('[') {
        Some((b, c)) => {
            let c = c
                .strip_suffix(']')
                .ok_or_else(|| Error::invalid(format!("bad type `{ty}`")))?;
            let n = c
                .trim()
                .parse::<u64>()
                .map_err(|_| Error::invalid(format!("bad array size in `{ty}`")))?;
            (b.trim(), Some(n))
        }
        None => (rest, None),
    };
    Ok((endian, base, count))
}

impl StructBuilder {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            fields: Vec::new(),
        }
    }

    /// Adds a field of a fixed-size type (`u32`, `be u16`, `char[8]`,
    /// `double`, ...) at `offset`.
    pub fn add(&mut self, offset: u64, ty: &str, name: &str) -> Result<&BuiltField> {
        let (_, base, count) = split_type(ty)?;
        let prim = Prim::from_name(base)
            .ok_or_else(|| Error::invalid(format!("unknown type `{base}`")))?;
        let size = prim.size().ok_or_else(|| {
            Error::invalid(format!("`{base}` has no fixed size; use `add_sized`"))
        })? * count.unwrap_or(1);
        self.insert(offset, size, ty.trim().to_string(), name)
    }

    /// Adds a field with an explicit size (strings, raw bytes): emitted as
    /// `char name[size]` for string types and `u8 name[size]` otherwise.
    pub fn add_sized(
        &mut self,
        offset: u64,
        size: u64,
        ty: &str,
        name: &str,
    ) -> Result<&BuiltField> {
        let t = match ty.trim() {
            "string" | "str" | "char" => format!("char[{size}]"),
            "wstring" | "char16" => format!("char16[{}]", size / 2),
            other if Prim::from_name(split_type(other)?.1).is_some() => {
                let (e, base, _) = split_type(other)?;
                let es = Prim::from_name(base).and_then(Prim::size).unwrap_or(1);
                if !size.is_multiple_of(es) {
                    return Err(Error::invalid(format!(
                        "{size} bytes is not a multiple of `{base}`"
                    )));
                }
                format!(
                    "{}{base}[{}]",
                    e.map(|e| format!("{e} ")).unwrap_or_default(),
                    size / es
                )
            }
            _ => format!("u8[{size}]"),
        };
        self.insert(offset, size, t, name)
    }

    fn insert(&mut self, offset: u64, size: u64, ty: String, name: &str) -> Result<&BuiltField> {
        if !valid_ident(name) {
            return Err(Error::invalid(format!(
                "`{name}` is not a valid field name"
            )));
        }
        if size == 0 {
            return Err(Error::invalid("fields must have a size"));
        }
        if self.fields.iter().any(|f| f.name == name) {
            return Err(Error::invalid(format!(
                "a field named `{name}` already exists"
            )));
        }
        let end = offset
            .checked_add(size)
            .ok_or_else(|| Error::invalid("field too large"))?;
        if let Some(f) = self
            .fields
            .iter()
            .find(|f| offset < f.offset + f.size && f.offset < end)
        {
            return Err(Error::invalid(format!("overlaps field `{}`", f.name)));
        }
        let pos = self.fields.partition_point(|f| f.offset < offset);
        self.fields.insert(
            pos,
            BuiltField {
                offset,
                size,
                name: name.to_string(),
                ty,
                comment: None,
            },
        );
        Ok(&self.fields[pos])
    }

    /// Removes a field by name.
    pub fn remove(&mut self, name: &str) -> bool {
        let n = self.fields.len();
        self.fields.retain(|f| f.name != name);
        n != self.fields.len()
    }

    /// Sets the comment of a field.
    pub fn set_comment(&mut self, name: &str, comment: Option<String>) -> bool {
        match self.fields.iter_mut().find(|f| f.name == name) {
            Some(f) => {
                f.comment = comment;
                true
            }
            None => false,
        }
    }

    /// Template source: gaps become `padding[n];`.
    pub fn to_source(&self) -> String {
        let mut s = String::new();
        let name = if valid_ident(&self.name) {
            self.name.as_str()
        } else {
            "Struct"
        };
        let _ = writeln!(s, "struct {name} {{");
        let mut pos = 0;
        for f in &self.fields {
            if f.offset > pos {
                let _ = writeln!(s, "    padding[{}];", f.offset - pos);
            }
            let (endian, base, count) = split_type(&f.ty).unwrap_or((None, "u8", None));
            let prefix = endian.map(|e| format!("{e} ")).unwrap_or_default();
            let arr = count.map(|c| format!("[{c}]")).unwrap_or_default();
            let _ = write!(s, "    {prefix}{base} {}{arr};", f.name);
            if let Some(c) = &f.comment {
                let _ = write!(s, " // {}", c.replace('\n', " "));
            }
            s.push('\n');
            pos = f.offset + f.size;
        }
        s.push_str("};\n");
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn top(b: &[u8], len: u64) -> Suggestion {
        suggest(b, len).into_iter().next().unwrap()
    }

    #[test]
    fn integers_and_byte_order() {
        let s = top(&[0x10, 0, 0, 0], 1000);
        assert_eq!(s.ty, "le u32");
        assert!(s.reason.contains("offset"), "{s:?}");
        let s = top(&[0, 0, 0, 0x10], 1000);
        assert_eq!(s.ty, "be u32");
        let s = top(&[0xFE, 0xFF], 100);
        assert!(
            suggest(&[0xFE, 0xFF], 100)
                .iter()
                .any(|x| x.ty == "le s16" && x.value == "-2")
        );
        let _ = s;
    }

    #[test]
    fn timestamps_floats_strings_magic() {
        let t = 1_700_000_000u32.to_le_bytes();
        let s = suggest(&t, 10);
        assert!(s[0].reason.contains("Unix time 2023-11-14"), "{:?}", s[0]);
        let ft = (((1_700_000_000u64) + 11_644_473_600) * 10_000_000).to_le_bytes();
        let s = suggest(&ft, 10);
        assert!(
            s[..3]
                .iter()
                .any(|x| x.reason.contains("FILETIME 2023-11-14")),
            "{s:#?}"
        );
        let f = 1.5f32.to_le_bytes();
        assert!(
            suggest(&f, 10)
                .iter()
                .any(|x| x.ty == "le float" && x.value == "1.5")
        );
        assert_eq!(top(b"IHDR", 10).ty, "char[4]");
        assert_eq!(top(b"hello\0", 10).ty, "string");
        assert!(
            suggest(&[b'h', 0, b'i', 0, 0, 0], 10)
                .iter()
                .any(|x| x.ty == "le wstring")
        );
        let m = top(b"PK\x03\x04", 100);
        assert!(m.reason.contains("ZIP"));
        assert_eq!(suggest(&[], 10).len(), 0);
    }

    #[test]
    fn builder_emits_source() {
        let mut b = StructBuilder::new("Header");
        b.add(0, "char[4]", "magic").unwrap();
        b.add(8, "be u32", "length").unwrap();
        b.add(4, "u16", "version").unwrap();
        b.add_sized(12, 6, "string", "name").unwrap();
        b.add_sized(20, 8, "le u16", "words").unwrap();
        assert!(b.add(9, "u8", "overlap").is_err());
        assert!(b.add(30, "u8", "magic").is_err());
        assert!(b.add(30, "u8", "bad name").is_err());
        assert!(b.add(30, "string", "s").is_err());
        b.set_comment("length", Some("payload size".into()));
        let src = b.to_source();
        assert_eq!(
            src,
            "struct Header {\n    char magic[4];\n    u16 version;\n    padding[2];\n    be u32 length; // payload size\n    char name[6];\n    padding[2];\n    le u16 words[4];\n};\n"
        );
        assert!(crate::parse(&src).is_ok());
        assert!(b.remove("name"));
        assert!(!b.remove("name"));
    }
}
