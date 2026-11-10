//! Binary-to-text encodings, encode and decode (XFRM-05).
//!
//! Decoders are lenient where it is unambiguous: ASCII whitespace is ignored
//! (Base16/32/58/64/85, Z85), hex and Base32 are case-insensitive and Base32
//! and Base64 accept input with or without padding.

use base64::Engine as _;
use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use serde::{Deserialize, Serialize};

use crate::error::{Result, TransformError};

/// Base32 alphabet (RFC 4648 §6 and §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Base32Alphabet {
    #[default]
    Standard,
    /// "Extended hex" alphabet `0-9A-V`.
    Hex,
}

/// Base64 alphabet (RFC 4648 §4 and §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Base64Alphabet {
    #[default]
    Standard,
    /// URL- and filename-safe alphabet (`-` and `_`).
    Url,
}

const fn yes() -> bool {
    true
}

fn default_uu_name() -> String {
    "data".to_owned()
}

/// A binary-to-text encoding with its options.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Codec {
    /// Hex digits, two per byte.
    Base16 {
        #[serde(default)]
        uppercase: bool,
    },
    /// RFC 4648 Base32.
    Base32 {
        #[serde(default)]
        alphabet: Base32Alphabet,
        /// Emit `=` padding when encoding (decoding accepts both).
        #[serde(default = "yes")]
        padding: bool,
    },
    /// Base58 with the Bitcoin alphabet.
    Base58,
    /// RFC 4648 Base64.
    Base64 {
        #[serde(default)]
        alphabet: Base64Alphabet,
        /// Emit `=` padding when encoding (decoding accepts both).
        #[serde(default = "yes")]
        padding: bool,
    },
    /// Adobe Ascii85 with `z` for zero groups.
    Ascii85 {
        /// Wrap the output in `<~ ~>` (decoding accepts both).
        #[serde(default)]
        delimiters: bool,
    },
    /// `ZeroMQ` Z85 (RFC 32); length must be a multiple of 4 (encode) or 5
    /// (decode).
    Z85,
    /// URL percent-encoding; everything except RFC 3986 unreserved
    /// characters is escaped.
    Url {
        /// Encode space as `+` and decode `+` as space (form encoding).
        #[serde(default)]
        space_as_plus: bool,
    },
    /// MIME quoted-printable (RFC 2045), line breaks in the input are kept.
    QuotedPrintable,
    /// Unix uuencode with a `begin`/`end` frame (decoding accepts bare
    /// lines too).
    Uuencode {
        #[serde(default = "default_uu_name")]
        file_name: String,
    },
    /// ROT13 of ASCII letters; other bytes are unchanged.
    Rot13,
}

impl Codec {
    /// Short display name.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Base16 { .. } => "Base16",
            Self::Base32 { .. } => "Base32",
            Self::Base58 => "Base58",
            Self::Base64 { .. } => "Base64",
            Self::Ascii85 { .. } => "Ascii85",
            Self::Z85 => "Z85",
            Self::Url { .. } => "URL encoding",
            Self::QuotedPrintable => "quoted-printable",
            Self::Uuencode { .. } => "uuencode",
            Self::Rot13 => "ROT13",
        }
    }
}

fn strip_ws(data: &[u8]) -> Vec<u8> {
    data.iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect()
}

fn b64_engine(alpha: Base64Alphabet, padding: bool) -> GeneralPurpose {
    let a = match alpha {
        Base64Alphabet::Standard => &alphabet::STANDARD,
        Base64Alphabet::Url => &alphabet::URL_SAFE,
    };
    GeneralPurpose::new(
        a,
        GeneralPurposeConfig::new()
            .with_encode_padding(padding)
            .with_decode_padding_mode(DecodePaddingMode::Indifferent),
    )
}

fn b32_spec(alpha: Base32Alphabet, padding: bool) -> data_encoding::Encoding {
    match (alpha, padding) {
        (Base32Alphabet::Standard, true) => data_encoding::BASE32,
        (Base32Alphabet::Standard, false) => data_encoding::BASE32_NOPAD,
        (Base32Alphabet::Hex, true) => data_encoding::BASE32HEX,
        (Base32Alphabet::Hex, false) => data_encoding::BASE32HEX_NOPAD,
    }
}

/// Encodes `data` with `codec`.
pub fn encode(codec: &Codec, data: &[u8]) -> Result<Vec<u8>> {
    Ok(match codec {
        Codec::Base16 { uppercase } => {
            let e = if *uppercase {
                data_encoding::HEXUPPER
            } else {
                data_encoding::HEXLOWER
            };
            e.encode(data).into_bytes()
        }
        Codec::Base32 { alphabet, padding } => {
            b32_spec(*alphabet, *padding).encode(data).into_bytes()
        }
        Codec::Base58 => bs58::encode(data).into_vec(),
        Codec::Base64 { alphabet, padding } => {
            b64_engine(*alphabet, *padding).encode(data).into_bytes()
        }
        Codec::Ascii85 { delimiters } => ascii85_encode(data, *delimiters),
        Codec::Z85 => z85_encode(data)?,
        Codec::Url { space_as_plus } => url_encode(data, *space_as_plus),
        Codec::QuotedPrintable => qp_encode(data),
        Codec::Uuencode { file_name } => uu_encode(data, file_name),
        Codec::Rot13 => rot13(data),
    })
}

/// Decodes `data` with `codec`.
pub fn decode(codec: &Codec, data: &[u8]) -> Result<Vec<u8>> {
    match codec {
        Codec::Base16 { .. } => data_encoding::HEXLOWER_PERMISSIVE
            .decode(&strip_ws(data))
            .map_err(|e| TransformError::encoding("Base16", e.to_string())),
        Codec::Base32 { alphabet, .. } => {
            let mut s = strip_ws(data);
            while s.last() == Some(&b'=') {
                s.pop();
            }
            s.make_ascii_uppercase();
            b32_spec(*alphabet, false)
                .decode(&s)
                .map_err(|e| TransformError::encoding("Base32", e.to_string()))
        }
        Codec::Base58 => bs58::decode(strip_ws(data))
            .into_vec()
            .map_err(|e| TransformError::encoding("Base58", e.to_string())),
        Codec::Base64 { alphabet, .. } => b64_engine(*alphabet, true)
            .decode(strip_ws(data))
            .map_err(|e| TransformError::encoding("Base64", e.to_string())),
        Codec::Ascii85 { .. } => ascii85_decode(data),
        Codec::Z85 => z85_decode(data),
        Codec::Url { space_as_plus } => url_decode(data, *space_as_plus),
        Codec::QuotedPrintable => qp_decode(data),
        Codec::Uuencode { .. } => uu_decode(data),
        Codec::Rot13 => Ok(rot13(data)),
    }
}

// ---------------------------------------------------------------- Ascii85

fn ascii85_encode(data: &[u8], delimiters: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() * 5 / 4 + 8);
    if delimiters {
        out.extend_from_slice(b"<~");
    }
    for chunk in data.chunks(4) {
        let mut group = [0u8; 4];
        group[..chunk.len()].copy_from_slice(chunk);
        let mut v = u32::from_be_bytes(group);
        if chunk.len() == 4 && v == 0 {
            out.push(b'z');
            continue;
        }
        let mut digits = [0u8; 5];
        for d in digits.iter_mut().rev() {
            *d = (v % 85) as u8 + b'!';
            v /= 85;
        }
        out.extend_from_slice(&digits[..=chunk.len()]);
    }
    if delimiters {
        out.extend_from_slice(b"~>");
    }
    out
}

fn ascii85_decode(data: &[u8]) -> Result<Vec<u8>> {
    let err = |r: String| TransformError::encoding("Ascii85", r);
    let mut s = strip_ws(data);
    if s.starts_with(b"<~") {
        s.drain(..2);
    }
    if s.ends_with(b"~>") {
        s.truncate(s.len() - 2);
    }
    let mut out = Vec::with_capacity(s.len() * 4 / 5 + 4);
    let mut group = [0u8; 5];
    let mut n = 0;
    for (i, &c) in s.iter().enumerate() {
        match c {
            b'z' if n == 0 => out.extend_from_slice(&[0; 4]),
            b'z' => return Err(err(format!("'z' inside a group at offset {i}"))),
            b'!'..=b'u' => {
                group[n] = c - b'!';
                n += 1;
                if n == 5 {
                    out.extend_from_slice(&a85_group(&group).ok_or_else(|| {
                        err(format!("group ending at offset {i} overflows 32 bits"))
                    })?);
                    n = 0;
                }
            }
            _ => {
                return Err(err(format!("invalid character 0x{c:02X} at offset {i}")));
            }
        }
    }
    if n == 1 {
        return Err(err("final group has a single character".into()));
    }
    if n > 1 {
        for g in &mut group[n..] {
            *g = 84;
        }
        let bytes = a85_group(&group).ok_or_else(|| err("final group overflows 32 bits".into()))?;
        out.extend_from_slice(&bytes[..n - 1]);
    }
    Ok(out)
}

fn a85_group(digits: &[u8; 5]) -> Option<[u8; 4]> {
    let v = digits
        .iter()
        .try_fold(0u64, |acc, &d| Some(acc * 85 + u64::from(d)))?;
    u32::try_from(v).ok().map(u32::to_be_bytes)
}

// ---------------------------------------------------------------- Z85

const Z85: &[u8; 85] =
    b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ.-:+=^!/*?&<>()[]{}@%$#";

fn z85_encode(data: &[u8]) -> Result<Vec<u8>> {
    if !data.len().is_multiple_of(4) {
        return Err(TransformError::CannotEncode {
            codec: "Z85",
            reason: format!("input length {} is not a multiple of 4", data.len()),
        });
    }
    let mut out = Vec::with_capacity(data.len() / 4 * 5);
    for chunk in data.as_chunks::<4>().0 {
        let mut v = u32::from_be_bytes(*chunk);
        let mut digits = [0u8; 5];
        for d in digits.iter_mut().rev() {
            *d = Z85[(v % 85) as usize];
            v /= 85;
        }
        out.extend_from_slice(&digits);
    }
    Ok(out)
}

fn z85_decode(data: &[u8]) -> Result<Vec<u8>> {
    let err = |r: String| TransformError::encoding("Z85", r);
    let s = strip_ws(data);
    if !s.len().is_multiple_of(5) {
        return Err(err(format!("length {} is not a multiple of 5", s.len())));
    }
    let mut rev = [255u8; 256];
    for (i, &c) in Z85.iter().enumerate() {
        rev[usize::from(c)] = i as u8;
    }
    let mut out = Vec::with_capacity(s.len() / 5 * 4);
    for (g, chunk) in s.as_chunks::<5>().0.iter().enumerate() {
        let mut digits = [0u8; 5];
        for (j, (&c, d)) in chunk.iter().zip(digits.iter_mut()).enumerate() {
            *d = rev[usize::from(c)];
            if *d == 255 {
                return Err(err(format!(
                    "invalid character 0x{c:02X} at offset {}",
                    g * 5 + j
                )));
            }
        }
        out.extend_from_slice(
            &a85_group(&digits)
                .ok_or_else(|| err(format!("group at offset {} overflows 32 bits", g * 5)))?,
        );
    }
    Ok(out)
}

// ---------------------------------------------------------------- URL

const HEX_UPPER: &[u8; 16] = b"0123456789ABCDEF";

fn url_encode(data: &[u8], space_as_plus: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() * 3);
    for &b in data {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b);
        } else if b == b' ' && space_as_plus {
            out.push(b'+');
        } else {
            out.extend_from_slice(&[
                b'%',
                HEX_UPPER[usize::from(b >> 4)],
                HEX_UPPER[usize::from(b & 15)],
            ]);
        }
    }
    out
}

fn hex_val(c: u8) -> Option<u8> {
    (c as char).to_digit(16).map(|d| d as u8)
}

fn url_decode(data: &[u8], space_as_plus: bool) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        match data[i] {
            b'%' => {
                let v = data
                    .get(i + 1..i + 3)
                    .and_then(|h| Some((hex_val(h[0])? << 4) | hex_val(h[1])?))
                    .ok_or_else(|| {
                        TransformError::encoding(
                            "URL encoding",
                            format!("invalid escape at offset {i}"),
                        )
                    })?;
                out.push(v);
                i += 3;
            }
            b'+' if space_as_plus => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- QP

const QP_MAX_LINE: usize = 76;

fn qp_encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() * 3 / 2);
    let mut line = 0usize;
    let mut i = 0;
    let at_break = |j: usize| {
        j >= data.len() || data[j] == b'\n' || (data[j] == b'\r' && data.get(j + 1) == Some(&b'\n'))
    };
    while i < data.len() {
        let b = data[i];
        if b == b'\n' {
            out.push(b'\n');
            line = 0;
            i += 1;
            continue;
        }
        if b == b'\r' && data.get(i + 1) == Some(&b'\n') {
            out.extend_from_slice(b"\r\n");
            line = 0;
            i += 2;
            continue;
        }
        let literal = ((33..=126).contains(&b) && b != b'=')
            || (matches!(b, b' ' | b'\t') && !at_break(i + 1));
        let len = if literal { 1 } else { 3 };
        // keep room for the soft-break '=' unless this is the last char of the line
        let limit = if at_break(i + 1) {
            QP_MAX_LINE
        } else {
            QP_MAX_LINE - 1
        };
        if line + len > limit {
            out.extend_from_slice(b"=\r\n");
            line = 0;
        }
        if literal {
            out.push(b);
        } else {
            out.extend_from_slice(&[
                b'=',
                HEX_UPPER[usize::from(b >> 4)],
                HEX_UPPER[usize::from(b & 15)],
            ]);
        }
        line += len;
        i += 1;
    }
    out
}

fn qp_decode(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    let is_break =
        |j: usize| data[j] == b'\n' || (data[j] == b'\r' && data.get(j + 1) == Some(&b'\n'));
    let break_len = |j: usize| if data[j] == b'\n' { 1 } else { 2 };
    while i < data.len() {
        match data[i] {
            b'=' => {
                if let Some(v) = data
                    .get(i + 1..i + 3)
                    .and_then(|h| Some((hex_val(h[0])? << 4) | hex_val(h[1])?))
                {
                    out.push(v);
                    i += 3;
                    continue;
                }
                // soft line break: '=' [spaces] CRLF | LF | end of data
                let mut j = i + 1;
                while j < data.len() && matches!(data[j], b' ' | b'\t') {
                    j += 1;
                }
                if j == data.len() {
                    i = j;
                } else if is_break(j) {
                    i = j + break_len(j);
                } else {
                    return Err(TransformError::encoding(
                        "quoted-printable",
                        format!("invalid escape at offset {i}"),
                    ));
                }
            }
            b' ' | b'\t' => {
                let mut j = i;
                while j < data.len() && matches!(data[j], b' ' | b'\t') {
                    j += 1;
                }
                // transport padding before a hard break or the end is dropped
                if j < data.len() && !is_break(j) {
                    out.extend_from_slice(&data[i..j]);
                }
                i = j;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- uuencode

fn uu_char(v: u8) -> u8 {
    if v == 0 { b'`' } else { v + 32 }
}

fn uu_encode(data: &[u8], name: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() * 4 / 3 + data.len() / 45 * 2 + 32);
    out.extend_from_slice(format!("begin 644 {name}\n").as_bytes());
    for line in data.chunks(45) {
        out.push(uu_char(line.len() as u8));
        for g in line.chunks(3) {
            let mut t = [0u8; 3];
            t[..g.len()].copy_from_slice(g);
            out.extend_from_slice(&[
                uu_char(t[0] >> 2),
                uu_char(((t[0] & 3) << 4) | (t[1] >> 4)),
                uu_char(((t[1] & 15) << 2) | (t[2] >> 6)),
                uu_char(t[2] & 63),
            ]);
        }
        out.push(b'\n');
    }
    out.extend_from_slice(b"`\nend\n");
    out
}

fn uu_decode(data: &[u8]) -> Result<Vec<u8>> {
    let lines: Vec<&[u8]> = data
        .split(|&b| b == b'\n')
        .map(|l| l.strip_suffix(b"\r").unwrap_or(l))
        .collect();
    let start = lines
        .iter()
        .position(|l| l.starts_with(b"begin "))
        .map_or(0, |p| p + 1);
    let mut out = Vec::with_capacity(data.len() * 3 / 4);
    for (ln, line) in lines.iter().enumerate().skip(start) {
        if line.trim_ascii_end() == b"end" {
            break;
        }
        if line.is_empty() {
            continue;
        }
        let n = usize::from(line[0].wrapping_sub(32) & 63);
        if n == 0 {
            continue;
        }
        let body = &line[1..];
        for (k, &c) in body.iter().enumerate() {
            if !(32..=96).contains(&c) {
                return Err(TransformError::encoding(
                    "uuencode",
                    format!(
                        "invalid character 0x{c:02X} on line {} column {}",
                        ln + 1,
                        k + 2
                    ),
                ));
            }
        }
        let v = |k: usize| body.get(k).map_or(0, |&c| c.wrapping_sub(32) & 63);
        let mut line_out = Vec::with_capacity(n + 2);
        let mut k = 0;
        while line_out.len() < n {
            let (a, b, c, d) = (v(k), v(k + 1), v(k + 2), v(k + 3));
            line_out.extend_from_slice(&[(a << 2) | (b >> 4), (b << 4) | (c >> 2), (c << 6) | d]);
            k += 4;
        }
        out.extend_from_slice(&line_out[..n]);
    }
    Ok(out)
}

// ---------------------------------------------------------------- ROT13

fn rot13(data: &[u8]) -> Vec<u8> {
    data.iter()
        .map(|&b| match b {
            b'a'..=b'z' => (b - b'a' + 13) % 26 + b'a',
            b'A'..=b'Z' => (b - b'A' + 13) % 26 + b'A',
            _ => b,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(alphabet: Base64Alphabet, padding: bool) -> Codec {
        Codec::Base64 { alphabet, padding }
    }

    fn enc(c: &Codec, d: &[u8]) -> String {
        String::from_utf8(encode(c, d).unwrap()).unwrap()
    }

    #[test]
    fn rfc4648_vectors() {
        let cases = [
            ("", "", "", "", ""),
            ("f", "66", "Zg==", "MY======", "CO======"),
            ("fo", "666f", "Zm8=", "MZXQ====", "CPNG===="),
            ("foo", "666f6f", "Zm9v", "MZXW6===", "CPNMU==="),
            ("foob", "666f6f62", "Zm9vYg==", "MZXW6YQ=", "CPNMUOG="),
            ("fooba", "666f6f6261", "Zm9vYmE=", "MZXW6YTB", "CPNMUOJ1"),
            (
                "foobar",
                "666f6f626172",
                "Zm9vYmFy",
                "MZXW6YTBOI======",
                "CPNMUOJ1E8======",
            ),
        ];
        let b32 = Codec::Base32 {
            alphabet: Base32Alphabet::Standard,
            padding: true,
        };
        let b32h = Codec::Base32 {
            alphabet: Base32Alphabet::Hex,
            padding: true,
        };
        let hex = Codec::Base16 { uppercase: false };
        for (plain, h, b, s32, h32) in cases {
            let p = plain.as_bytes();
            assert_eq!(enc(&hex, p), h);
            assert_eq!(enc(&b64(Base64Alphabet::Standard, true), p), b);
            assert_eq!(enc(&b32, p), s32);
            assert_eq!(enc(&b32h, p), h32);
            assert_eq!(decode(&hex, h.as_bytes()).unwrap(), p);
            assert_eq!(
                decode(&b64(Base64Alphabet::Standard, true), b.as_bytes()).unwrap(),
                p
            );
            assert_eq!(decode(&b32, s32.as_bytes()).unwrap(), p);
            assert_eq!(decode(&b32h, h32.as_bytes()).unwrap(), p);
            // unpadded / lowercase input is accepted too
            let unpadded = b.trim_end_matches('=');
            assert_eq!(
                decode(&b64(Base64Alphabet::Standard, true), unpadded.as_bytes()).unwrap(),
                p
            );
            let lower = s32.trim_end_matches('=').to_ascii_lowercase();
            assert_eq!(decode(&b32, lower.as_bytes()).unwrap(), p);
        }
        assert_eq!(enc(&Codec::Base16 { uppercase: true }, b"\xab\xcd"), "ABCD");
    }

    #[test]
    fn base64_variants() {
        let d = [0xfb, 0xff, 0xbf];
        assert_eq!(enc(&b64(Base64Alphabet::Standard, true), &d), "+/+/");
        assert_eq!(enc(&b64(Base64Alphabet::Url, true), &d), "-_-_");
        assert_eq!(enc(&b64(Base64Alphabet::Url, false), b"f"), "Zg");
        let wrapped = b"Zm9v\r\n YmFy\n";
        assert_eq!(
            decode(&b64(Base64Alphabet::Standard, false), wrapped).unwrap(),
            b"foobar"
        );
        assert!(decode(&b64(Base64Alphabet::Standard, true), b"Zm9v!").is_err());
        assert!(decode(&b64(Base64Alphabet::Url, true), b"+/+/").is_err());
    }

    #[test]
    fn base58_bitcoin() {
        assert_eq!(enc(&Codec::Base58, b"Hello World!"), "2NEpo7TZRRrLZSi2U");
        assert_eq!(enc(&Codec::Base58, &[0, 0, 1]), "112");
        assert_eq!(
            decode(&Codec::Base58, b"2NEpo7TZRRrLZSi2U").unwrap(),
            b"Hello World!"
        );
        assert!(decode(&Codec::Base58, b"0OIl").is_err());
    }

    #[test]
    fn ascii85_vectors() {
        let c = Codec::Ascii85 { delimiters: true };
        assert_eq!(
            enc(&c, b"Man is distinguished"),
            "<~9jqo^BlbD-BleB1DJ+*+F(f,q~>"
        );
        let c = Codec::Ascii85 { delimiters: false };
        assert_eq!(enc(&c, b"\0\0\0\0abc"), "z@:E^");
        assert_eq!(decode(&c, b"z@:E^").unwrap(), b"\0\0\0\0abc");
        assert_eq!(
            decode(&c, b"<~9jqo^BlbD-\n BleB1DJ+*+F(f,q~>").unwrap(),
            b"Man is distinguished"
        );
        assert!(decode(&c, b"9jqo^B").is_err()); // single trailing char
        assert!(decode(&c, b"9jzqo").is_err());
        assert!(decode(&c, b"uuuuu").is_err()); // overflow
        assert!(decode(&c, b"abc{").is_err());
    }

    #[test]
    fn z85_vectors() {
        let d = [0x86, 0x4F, 0xD2, 0x6F, 0xB5, 0x59, 0xF7, 0x5B];
        assert_eq!(enc(&Codec::Z85, &d), "HelloWorld");
        assert_eq!(decode(&Codec::Z85, b"HelloWorld").unwrap(), d);
        assert!(encode(&Codec::Z85, b"abc").is_err());
        assert!(decode(&Codec::Z85, b"Hell").is_err());
        assert!(decode(&Codec::Z85, b"Hell~").is_err());
    }

    #[test]
    fn url_encoding() {
        let c = Codec::Url {
            space_as_plus: false,
        };
        assert_eq!(enc(&c, b"a b&c/\xff-_.~"), "a%20b%26c%2F%FF-_.~");
        assert_eq!(decode(&c, b"a%20b%26c%2f%FF+").unwrap(), b"a b&c/\xff+");
        let f = Codec::Url {
            space_as_plus: true,
        };
        assert_eq!(enc(&f, b"a b"), "a+b");
        assert_eq!(decode(&f, b"a+b%2B").unwrap(), b"a b+");
        assert!(decode(&c, b"%2").is_err());
        assert!(decode(&c, b"%zz").is_err());
    }

    #[test]
    fn quoted_printable() {
        let c = Codec::QuotedPrintable;
        assert_eq!(
            enc(&c, b"caf\xc3\xa9 = ok \nline  \n"),
            "caf=C3=A9 =3D ok=20\nline =20\n"
        );
        assert_eq!(
            decode(&c, b"caf=C3=A9 =3D ok=20\nline =20\n").unwrap(),
            b"caf\xc3\xa9 = ok \nline  \n"
        );
        // soft breaks and transport padding
        assert_eq!(
            decode(&c, b"abc=  \r\ndef   \r\nx=\n").unwrap(),
            b"abcdef\r\nx"
        );
        assert!(decode(&c, b"a=ZZb").is_err());
        let long = vec![b'x'; 200];
        let e = encode(&c, &long).unwrap();
        assert!(e.split(|&b| b == b'\n').all(|l| l.len() <= QP_MAX_LINE + 1));
        assert_eq!(decode(&c, &e).unwrap(), long);
        let bin: Vec<u8> = (0..=255).collect();
        assert_eq!(decode(&c, &encode(&c, &bin).unwrap()).unwrap(), bin);
    }

    #[test]
    fn uuencode_vectors() {
        let c = Codec::Uuencode {
            file_name: "data".into(),
        };
        assert_eq!(enc(&c, b"Cat"), "begin 644 data\n#0V%T\n`\nend\n");
        assert_eq!(
            decode(&c, b"begin 644 cat.txt\r\n#0V%T\r\n`\r\nend\r\n").unwrap(),
            b"Cat"
        );
        assert_eq!(decode(&c, b"#0V%T\n").unwrap(), b"Cat");
        let data: Vec<u8> = (0..200u8).collect();
        assert_eq!(decode(&c, &encode(&c, &data).unwrap()).unwrap(), data);
        assert!(decode(&c, b"#0V\x7f%\n").is_err());
    }

    #[test]
    fn rot13_letters_only() {
        assert_eq!(
            enc(&Codec::Rot13, b"Hello, World! 123"),
            "Uryyb, Jbeyq! 123"
        );
        assert_eq!(decode(&Codec::Rot13, b"Uryyb").unwrap(), b"Hello");
    }

    #[test]
    fn serde_defaults() {
        let c: Codec = serde_json::from_str(r#"{"kind":"base64"}"#).unwrap();
        assert_eq!(c, b64(Base64Alphabet::Standard, true));
        let c: Codec = serde_json::from_str(r#"{"kind":"url","spaceAsPlus":true}"#).unwrap();
        assert_eq!(
            c,
            Codec::Url {
                space_as_plus: true
            }
        );
        let j = serde_json::to_string(&Codec::Base32 {
            alphabet: Base32Alphabet::Hex,
            padding: false,
        })
        .unwrap();
        assert_eq!(j, r#"{"kind":"base32","alphabet":"hex","padding":false}"#);
    }
}
