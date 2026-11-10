//! Built-in primitive types (STRC-01) and their binary encodings.

use crate::ast::Endian;

/// Primitive type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Prim {
    /// Unsigned integer of the given byte width (1..=16).
    U(u8),
    /// Signed integer of the given byte width (1..=16).
    S(u8),
    F16,
    F32,
    F64,
    /// 8-bit character (strings when used in arrays).
    Char,
    /// UTF-16 code unit.
    Char16,
    Bool,
    /// Skipped bytes.
    Padding,
    /// NUL-terminated 8-bit string.
    CStr,
    /// NUL-terminated UTF-16 string.
    WStr,
}

impl Prim {
    /// Resolves built-in type names, including 010 Editor, C `<stdint.h>`
    /// and Windows spellings.
    pub fn from_name(name: &str) -> Option<Prim> {
        use Prim::{Bool, CStr, Char, Char16, F16, F32, F64, Padding, S, U, WStr};
        Some(match name {
            "u8" | "uchar" | "ubyte" | "UCHAR" | "BYTE" | "UBYTE" | "uint8" | "uint8_t"
            | "UINT8" | "u1" => U(1),
            "u16" | "ushort" | "uint16" | "uint16_t" | "USHORT" | "UINT16" | "WORD" | "u2" => U(2),
            "u24" => U(3),
            "u32" | "uint" | "uint32" | "uint32_t" | "ulong" | "UINT" | "UINT32" | "ULONG"
            | "DWORD" | "u4" => U(4),
            "u48" => U(6),
            "u64" | "uint64" | "uint64_t" | "uquad" | "UQUAD" | "UINT64" | "QWORD" | "__uint64"
            | "size_t" | "uintptr_t" | "u8_t" | "ULONGLONG" | "DWORD64" => U(8),
            "u96" => U(12),
            "u128" | "uint128_t" | "__uint128_t" => U(16),
            "s8" | "i8" | "byte" | "int8" | "int8_t" | "CHAR" | "INT8" | "s1" => S(1),
            "s16" | "i16" | "short" | "int16" | "int16_t" | "SHORT" | "INT16" | "s2" => S(2),
            "s24" | "i24" => S(3),
            "s32" | "i32" | "int" | "int32" | "int32_t" | "long" | "INT" | "INT32" | "LONG"
            | "s4" => S(4),
            "s48" | "i48" => S(6),
            "s64" | "i64" | "int64" | "int64_t" | "quad" | "QUAD" | "INT64" | "__int64"
            | "ssize_t" | "intptr_t" | "ptrdiff_t" | "LONGLONG" | "s8_t" => S(8),
            "s96" | "i96" => S(12),
            "s128" | "i128" | "int128_t" | "__int128_t" => S(16),
            "f16" | "hfloat" | "HFLOAT" | "half" => F16,
            "float" | "f32" | "FLOAT" | "f4" => F32,
            "double" | "f64" | "DOUBLE" | "f8" => F64,
            "char" => Char,
            "char16" | "wchar_t" | "wchar" | "WCHAR" | "char16_t" => Char16,
            "bool" | "_Bool" | "BOOL8" => Bool,
            "padding" => Padding,
            "string" | "str" | "std::string::NullString" | "NullString" | "strz" => CStr,
            "wstring" | "string16" | "std::string::NullString16" | "NullString16" => WStr,
            _ => return None,
        })
    }

    /// Canonical name.
    pub fn name(self) -> String {
        match self {
            Prim::U(n) => format!("u{}", u32::from(n) * 8),
            Prim::S(n) => format!("s{}", u32::from(n) * 8),
            Prim::F16 => "f16".into(),
            Prim::F32 => "float".into(),
            Prim::F64 => "double".into(),
            Prim::Char => "char".into(),
            Prim::Char16 => "char16".into(),
            Prim::Bool => "bool".into(),
            Prim::Padding => "padding".into(),
            Prim::CStr => "string".into(),
            Prim::WStr => "wstring".into(),
        }
    }

    /// Fixed size in bytes (`None` for NUL-terminated strings).
    pub fn size(self) -> Option<u64> {
        Some(match self {
            Prim::U(n) | Prim::S(n) => u64::from(n),
            Prim::F16 | Prim::Char16 => 2,
            Prim::F32 => 4,
            Prim::F64 => 8,
            Prim::Char | Prim::Bool | Prim::Padding => 1,
            Prim::CStr | Prim::WStr => return None,
        })
    }

    pub fn is_int(self) -> bool {
        matches!(self, Prim::U(_) | Prim::S(_))
    }

    pub fn is_signed(self) -> bool {
        matches!(self, Prim::S(_))
    }

    pub fn is_float(self) -> bool {
        matches!(self, Prim::F16 | Prim::F32 | Prim::F64)
    }

    /// Natural C alignment.
    pub fn align(self) -> u64 {
        self.size().unwrap_or(1).clamp(1, 16)
    }
}

/// Decodes an unsigned integer from `bytes`.
pub fn decode_uint(bytes: &[u8], endian: Endian) -> u128 {
    let mut v: u128 = 0;
    match endian {
        Endian::Little => {
            for &b in bytes.iter().rev() {
                v = (v << 8) | u128::from(b);
            }
        }
        Endian::Big => {
            for &b in bytes {
                v = (v << 8) | u128::from(b);
            }
        }
    }
    v
}

/// Sign-extends a `bits`-wide value.
pub fn sign_extend(v: u128, bits: u32) -> i128 {
    if bits == 0 || bits >= 128 {
        return v as i128;
    }
    let shift = 128 - bits;
    ((v << shift) as i128) >> shift
}

/// Encodes the low `n` bytes of `v`.
pub fn encode_uint(v: u128, n: usize, endian: Endian) -> Vec<u8> {
    let le: Vec<u8> = (0..n).map(|i| (v >> (8 * i)) as u8).collect();
    match endian {
        Endian::Little => le,
        Endian::Big => le.into_iter().rev().collect(),
    }
}

/// IEEE 754 half precision to `f64`.
pub fn f16_to_f64(h: u16) -> f64 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = i32::from((h >> 10) & 0x1F);
    let frac = f64::from(h & 0x3FF);
    if exp == 0 {
        sign * frac * 2f64.powi(-24)
    } else if exp == 31 {
        if frac == 0.0 {
            sign * f64::INFINITY
        } else {
            f64::NAN
        }
    } else {
        sign * (1.0 + frac / 1024.0) * 2f64.powi(exp - 15)
    }
}

/// `f64` to IEEE 754 half precision (round to nearest).
pub fn f64_to_f16(v: f64) -> u16 {
    if v.is_nan() {
        return 0x7E00;
    }
    let sign: u16 = if v.is_sign_negative() { 0x8000 } else { 0 };
    let a = v.abs();
    if a == 0.0 {
        return sign;
    }
    if a >= 65520.0 {
        return sign | 0x7C00;
    }
    if a < 2f64.powi(-14) {
        let frac = (a / 2f64.powi(-24)).round() as u16;
        return sign | frac;
    }
    let mut exp = a.log2().floor() as i32;
    let mut frac = ((a / 2f64.powi(exp) - 1.0) * 1024.0).round() as i32;
    if frac == 1024 {
        frac = 0;
        exp += 1;
    }
    sign | (((exp + 15) as u16) << 10) | frac as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_sizes() {
        assert_eq!(Prim::from_name("DWORD"), Some(Prim::U(4)));
        assert_eq!(Prim::from_name("i64"), Some(Prim::S(8)));
        assert_eq!(Prim::from_name("u24").and_then(Prim::size), Some(3));
        assert_eq!(Prim::from_name("string").and_then(Prim::size), None);
        assert_eq!(Prim::U(4).name(), "u32");
        assert_eq!(Prim::from_name("nope"), None);
    }

    #[test]
    fn int_codec() {
        assert_eq!(decode_uint(&[1, 2], Endian::Little), 0x0201);
        assert_eq!(decode_uint(&[1, 2], Endian::Big), 0x0102);
        assert_eq!(sign_extend(0xFF, 8), -1);
        assert_eq!(sign_extend(0x7F, 8), 127);
        assert_eq!(encode_uint(0x0102, 2, Endian::Big), vec![1, 2]);
    }

    #[test]
    fn half_floats() {
        assert!((f16_to_f64(0x3C00) - 1.0).abs() < 1e-9);
        assert!((f16_to_f64(0xC000) + 2.0).abs() < 1e-9);
        assert_eq!(f64_to_f16(1.0), 0x3C00);
        assert_eq!(f64_to_f16(-2.0), 0xC000);
        assert_eq!(f64_to_f16(0.5), 0x3800);
    }
}
