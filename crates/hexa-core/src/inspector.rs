//! Data Inspector: bytes under the cursor in many types (INSP-01…03, 06, 07, 15).

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Endian {
    Little,
    Big,
}

/// Every type the inspector shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InspectType {
    Binary,
    Octal,
    Hex,
    I8,
    U8,
    I16,
    U16,
    I24,
    U24,
    I32,
    U32,
    I48,
    U48,
    I64,
    U64,
    I128,
    U128,
    F16,
    Bf16,
    F32,
    F64,
    Ascii,
    Utf8,
    Utf16,
    Utf32,
}

impl InspectType {
    pub const ALL: [Self; 25] = [
        Self::Binary,
        Self::Octal,
        Self::Hex,
        Self::I8,
        Self::U8,
        Self::I16,
        Self::U16,
        Self::I24,
        Self::U24,
        Self::I32,
        Self::U32,
        Self::I48,
        Self::U48,
        Self::I64,
        Self::U64,
        Self::I128,
        Self::U128,
        Self::F16,
        Self::Bf16,
        Self::F32,
        Self::F64,
        Self::Ascii,
        Self::Utf8,
        Self::Utf16,
        Self::Utf32,
    ];

    /// Fixed width in bytes; `None` for variable-width characters.
    pub fn width(self) -> Option<usize> {
        Some(match self {
            Self::Binary | Self::Octal | Self::Hex | Self::I8 | Self::U8 | Self::Ascii => 1,
            Self::I16 | Self::U16 | Self::F16 | Self::Bf16 => 2,
            Self::I24 | Self::U24 => 3,
            Self::I32 | Self::U32 | Self::F32 | Self::Utf32 => 4,
            Self::I48 | Self::U48 => 6,
            Self::I64 | Self::U64 | Self::F64 => 8,
            Self::I128 | Self::U128 => 16,
            Self::Utf8 | Self::Utf16 => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Binary => "binary",
            Self::Octal => "octal",
            Self::Hex => "hex",
            Self::I8 => "int8",
            Self::U8 => "uint8",
            Self::I16 => "int16",
            Self::U16 => "uint16",
            Self::I24 => "int24",
            Self::U24 => "uint24",
            Self::I32 => "int32",
            Self::U32 => "uint32",
            Self::I48 => "int48",
            Self::U48 => "uint48",
            Self::I64 => "int64",
            Self::U64 => "uint64",
            Self::I128 => "int128",
            Self::U128 => "uint128",
            Self::F16 => "float16",
            Self::Bf16 => "bfloat16",
            Self::F32 => "float32",
            Self::F64 => "float64",
            Self::Ascii => "ASCII",
            Self::Utf8 => "UTF-8",
            Self::Utf16 => "UTF-16",
            Self::Utf32 => "UTF-32",
        }
    }

    fn is_signed(self) -> bool {
        matches!(
            self,
            Self::I8 | Self::I16 | Self::I24 | Self::I32 | Self::I48 | Self::I64 | Self::I128
        )
    }
}

/// One inspector row.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    pub ty: InspectType,
    pub label: &'static str,
    /// `None` when there are not enough bytes or the value is invalid.
    pub value: Option<String>,
    /// Number of bytes the value spans.
    pub size: usize,
}

fn ordered<const N: usize>(bytes: &[u8], endian: Endian) -> Option<[u8; N]> {
    let mut a: [u8; N] = bytes.get(..N)?.try_into().ok()?;
    if endian == Endian::Big {
        a.reverse();
    }
    Some(a) // little-endian order
}

fn uint(bytes: &[u8], width: usize, endian: Endian) -> Option<u128> {
    let a: [u8; 16] = {
        let src = bytes.get(..width)?;
        let mut a = [0u8; 16];
        a[..width].copy_from_slice(src);
        if endian == Endian::Big {
            a[..width].reverse();
        }
        a
    };
    Some(u128::from_le_bytes(a))
}

fn sint(bytes: &[u8], width: usize, endian: Endian) -> Option<i128> {
    let v = uint(bytes, width, endian)?;
    let shift = 128 - width * 8;
    Some(((v << shift) as i128) >> shift)
}

fn float_str(v: f64) -> String {
    if v.is_nan() {
        "NaN".into()
    } else if v.is_infinite() {
        if v > 0.0 {
            "+∞".into()
        } else {
            "−∞".into()
        }
    } else {
        let s = format!("{v}");
        if s.len() > 24 { format!("{v:e}") } else { s }
    }
}

fn utf8_char(bytes: &[u8]) -> Option<(char, usize)> {
    let first = *bytes.first()?;
    let n = match first {
        0x00..=0x7F => 1,
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => return None,
    };
    let s = std::str::from_utf8(bytes.get(..n)?).ok()?;
    s.chars().next().map(|c| (c, n))
}

fn utf16_char(bytes: &[u8], endian: Endian) -> Option<(char, usize)> {
    let unit = |i: usize| -> Option<u16> {
        let a: [u8; 2] = ordered(bytes.get(i..)?, endian)?;
        Some(u16::from_le_bytes(a))
    };
    let u0 = unit(0)?;
    if (0xD800..0xDC00).contains(&u0) {
        let u1 = unit(2)?;
        let c = char::decode_utf16([u0, u1]).next()?.ok()?;
        Some((c, 4))
    } else {
        char::decode_utf16([u0]).next()?.ok().map(|c| (c, 2))
    }
}

fn show_char(c: char) -> String {
    match c {
        '\0' => "NUL".into(),
        '\t' => "\\t".into(),
        '\n' => "\\n".into(),
        '\r' => "\\r".into(),
        c if c.is_control() => format!("U+{:04X}", u32::from(c)),
        c => format!("'{c}'  U+{:04X}", u32::from(c)),
    }
}

/// Decodes one value. `bytes` starts at the cursor.
pub fn decode(ty: InspectType, bytes: &[u8], endian: Endian) -> (Option<String>, usize) {
    let width = ty.width();
    let value = match ty {
        InspectType::Binary => bytes.first().map(|b| format!("{b:08b}")),
        InspectType::Octal => bytes.first().map(|b| format!("{b:03o}")),
        InspectType::Hex => bytes.first().map(|b| format!("{b:02X}")),
        InspectType::F16 => {
            ordered::<2>(bytes, endian).map(|a| float_str(f64::from(half::f16::from_le_bytes(a))))
        }
        InspectType::Bf16 => {
            ordered::<2>(bytes, endian).map(|a| float_str(f64::from(half::bf16::from_le_bytes(a))))
        }
        InspectType::F32 => {
            ordered::<4>(bytes, endian).map(|a| float_str(f64::from(f32::from_le_bytes(a))))
        }
        InspectType::F64 => ordered::<8>(bytes, endian).map(|a| float_str(f64::from_le_bytes(a))),
        InspectType::Ascii => bytes.first().map(|&b| {
            if b < 0x80 {
                show_char(char::from(b))
            } else {
                "—".into()
            }
        }),
        InspectType::Utf8 => {
            return utf8_char(bytes).map_or((None, 1), |(c, n)| (Some(show_char(c)), n));
        }
        InspectType::Utf16 => {
            return utf16_char(bytes, endian).map_or((None, 2), |(c, n)| (Some(show_char(c)), n));
        }
        InspectType::Utf32 => ordered::<4>(bytes, endian)
            .and_then(|a| char::from_u32(u32::from_le_bytes(a)))
            .map(show_char),
        t if t.is_signed() => sint(bytes, width.unwrap_or(1), endian).map(|v| v.to_string()),
        t => uint(bytes, t.width().unwrap_or(1), endian).map(|v| v.to_string()),
    };
    (value, width.unwrap_or(1))
}

/// All rows for the bytes at the cursor.
pub fn inspect(bytes: &[u8], endian: Endian) -> Vec<Row> {
    InspectType::ALL
        .iter()
        .map(|&ty| {
            let (value, size) = decode(ty, bytes, endian);
            Row {
                ty,
                label: ty.label(),
                value,
                size,
            }
        })
        .collect()
}

fn parse_int(text: &str) -> Result<i128> {
    let t = text.trim().replace(['_', ' '], "");
    let (neg, t) = t
        .strip_prefix('-')
        .map_or((false, t.as_str()), |r| (true, r));
    let (radix, digits) = if let Some(r) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        (16, r)
    } else if let Some(r) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        (2, r)
    } else if let Some(r) = t.strip_prefix("0o").or_else(|| t.strip_prefix("0O")) {
        (8, r)
    } else {
        (10, t)
    };
    let v = u128::from_str_radix(digits, radix)
        .map_err(|_| Error::invalid(format!("'{text}' is not a number")))?;
    if neg {
        if v > i128::MAX as u128 + 1 {
            return Err(Error::invalid("number out of range"));
        }
        Ok((v as i128).wrapping_neg())
    } else {
        i128::try_from(v).map_err(|_| Error::invalid("number out of range"))
    }
}

fn to_bytes(v: u128, width: usize, endian: Endian) -> Vec<u8> {
    let mut b = v.to_le_bytes()[..width].to_vec();
    if endian == Endian::Big {
        b.reverse();
    }
    b
}

/// Encodes a user-typed value with range checks (INSP-15).
pub fn encode(ty: InspectType, text: &str, endian: Endian) -> Result<Vec<u8>> {
    let order = |mut b: Vec<u8>| {
        if endian == Endian::Big {
            b.reverse();
        }
        b
    };
    let text = text.trim();
    match ty {
        InspectType::Binary | InspectType::Octal | InspectType::Hex => {
            let radix = match ty {
                InspectType::Binary => 2,
                InspectType::Octal => 8,
                _ => 16,
            };
            let v = u8::from_str_radix(text, radix)
                .map_err(|_| Error::invalid(format!("'{text}' is not a valid byte")))?;
            Ok(vec![v])
        }
        InspectType::F16 => {
            let v: f32 = text.parse().map_err(|_| Error::invalid("not a number"))?;
            Ok(order(half::f16::from_f32(v).to_le_bytes().to_vec()))
        }
        InspectType::Bf16 => {
            let v: f32 = text.parse().map_err(|_| Error::invalid("not a number"))?;
            Ok(order(half::bf16::from_f32(v).to_le_bytes().to_vec()))
        }
        InspectType::F32 => {
            let v: f32 = text.parse().map_err(|_| Error::invalid("not a number"))?;
            Ok(order(v.to_le_bytes().to_vec()))
        }
        InspectType::F64 => {
            let v: f64 = text.parse().map_err(|_| Error::invalid("not a number"))?;
            Ok(order(v.to_le_bytes().to_vec()))
        }
        InspectType::Ascii | InspectType::Utf8 | InspectType::Utf16 | InspectType::Utf32 => {
            let mut chars = text.chars();
            let c = chars.next().ok_or_else(|| Error::invalid("empty"))?;
            if chars.next().is_some() {
                return Err(Error::invalid("type a single character"));
            }
            match ty {
                InspectType::Ascii if c.is_ascii() => Ok(vec![c as u8]),
                InspectType::Ascii => Err(Error::invalid("not an ASCII character")),
                InspectType::Utf8 => Ok(c.to_string().into_bytes()),
                InspectType::Utf16 => {
                    let mut buf = [0u16; 2];
                    Ok(c.encode_utf16(&mut buf)
                        .iter()
                        .flat_map(|u| order(u.to_le_bytes().to_vec()))
                        .collect())
                }
                _ => Ok(order(u32::from(c).to_le_bytes().to_vec())),
            }
        }
        t => {
            let width = t.width().expect("integer width");
            let v = parse_int(text)?;
            let bits = width as u32 * 8;
            let ok = if t.is_signed() {
                let min = -(1i128 << (bits - 1));
                let max = if bits == 128 {
                    i128::MAX
                } else {
                    (1i128 << (bits - 1)) - 1
                };
                v >= min && v <= max
            } else {
                v >= 0 && (bits == 128 || v < (1i128 << bits))
            };
            if !ok {
                return Err(Error::invalid(format!(
                    "{text} does not fit into {}",
                    t.label()
                )));
            }
            Ok(to_bytes(v as u128, width, endian))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn val(ty: InspectType, b: &[u8], e: Endian) -> String {
        decode(ty, b, e).0.unwrap()
    }

    #[test]
    fn decodes_integers_and_floats() {
        let b = [0xFE, 0xFF, 0xFF, 0xFF, 0, 0, 0x80, 0x3F];
        assert_eq!(val(InspectType::I8, &b, Endian::Little), "-2");
        assert_eq!(val(InspectType::U16, &b, Endian::Little), "65534");
        assert_eq!(val(InspectType::I24, &b, Endian::Little), "-2");
        assert_eq!(val(InspectType::I32, &b, Endian::Little), "-2");
        assert_eq!(val(InspectType::U16, &b, Endian::Big), "65279");
        assert_eq!(val(InspectType::F32, &b[4..], Endian::Little), "1");
        assert_eq!(val(InspectType::F16, &[0x00, 0x3C], Endian::Little), "1");
        assert_eq!(val(InspectType::Bf16, &[0x80, 0x3F], Endian::Little), "1");
        assert!(
            decode(InspectType::U64, &b[..4], Endian::Little)
                .0
                .is_none()
        );
    }

    #[test]
    fn decodes_chars() {
        assert_eq!(
            val(InspectType::Utf8, "Ж".as_bytes(), Endian::Little),
            "'Ж'  U+0416"
        );
        assert_eq!(
            decode(InspectType::Utf8, "Ж".as_bytes(), Endian::Little).1,
            2
        );
        assert_eq!(
            val(
                InspectType::Utf16,
                &[0x3D, 0xD8, 0x00, 0xDE],
                Endian::Little
            ),
            "'😀'  U+1F600"
        );
        assert_eq!(val(InspectType::Ascii, b"\n", Endian::Little), "\\n");
    }

    #[test]
    fn encodes_with_range_checks() {
        assert_eq!(
            encode(InspectType::U16, "0x1234", Endian::Big).unwrap(),
            [0x12, 0x34]
        );
        assert_eq!(
            encode(InspectType::I8, "-128", Endian::Little).unwrap(),
            [0x80]
        );
        assert!(encode(InspectType::I8, "128", Endian::Little).is_err());
        assert!(encode(InspectType::U8, "-1", Endian::Little).is_err());
        assert_eq!(
            encode(InspectType::U24, "16777215", Endian::Little).unwrap(),
            [0xFF; 3]
        );
        assert_eq!(
            encode(InspectType::I128, "-1", Endian::Little).unwrap(),
            vec![0xFF; 16]
        );
        assert_eq!(
            encode(InspectType::F32, "1", Endian::Little).unwrap(),
            [0, 0, 0x80, 0x3F]
        );
        assert_eq!(
            encode(InspectType::Binary, "1010", Endian::Little).unwrap(),
            [10]
        );
        assert_eq!(
            encode(InspectType::Utf16, "😀", Endian::Little).unwrap(),
            [0x3D, 0xD8, 0x00, 0xDE]
        );
    }
}
