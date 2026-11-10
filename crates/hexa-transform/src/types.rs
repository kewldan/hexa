//! Parameter types shared by several transformations.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{Result, TransformError};

/// Upper bound for any single transformation output (decompression bombs,
/// runaway expansion). Matches the selection limit of the app.
pub const MAX_OUTPUT_LEN: usize = 512 * 1024 * 1024;

/// Element width in bits (XFRM-01, XFRM-02). Serialised as the number
/// `8`, `16`, `32` or `64`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub enum Width {
    #[default]
    W8,
    W16,
    W32,
    W64,
}

impl Width {
    /// Width in bytes.
    pub const fn bytes(self) -> usize {
        match self {
            Self::W8 => 1,
            Self::W16 => 2,
            Self::W32 => 4,
            Self::W64 => 8,
        }
    }

    /// Width in bits.
    pub const fn bits(self) -> u32 {
        self.bytes() as u32 * 8
    }

    /// Mask with all bits of the element set.
    pub const fn mask(self) -> u64 {
        match self {
            Self::W64 => u64::MAX,
            _ => (1u64 << self.bits()) - 1,
        }
    }
}

impl TryFrom<u8> for Width {
    type Error = String;

    fn try_from(v: u8) -> std::result::Result<Self, String> {
        match v {
            8 => Ok(Self::W8),
            16 => Ok(Self::W16),
            32 => Ok(Self::W32),
            64 => Ok(Self::W64),
            _ => Err(format!("width must be 8, 16, 32 or 64 bits, got {v}")),
        }
    }
}

impl From<Width> for u8 {
    fn from(w: Width) -> u8 {
        w.bits() as u8
    }
}

/// Byte order of multi-byte elements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Endian {
    #[default]
    Little,
    Big,
}

/// Reads an element of `chunk.len()` (1..=8) bytes.
pub(crate) fn read_elem(chunk: &[u8], endian: Endian) -> u64 {
    match endian {
        Endian::Little => chunk
            .iter()
            .rev()
            .fold(0u64, |acc, &b| (acc << 8) | u64::from(b)),
        Endian::Big => chunk.iter().fold(0u64, |acc, &b| (acc << 8) | u64::from(b)),
    }
}

/// Writes the low `chunk.len()` bytes of `v` into `chunk`.
pub(crate) fn write_elem(chunk: &mut [u8], v: u64, endian: Endian) {
    let n = chunk.len();
    for (i, slot) in chunk.iter_mut().enumerate() {
        let shift = match endian {
            Endian::Little => i,
            Endian::Big => n - 1 - i,
        } * 8;
        *slot = (v >> shift) as u8;
    }
}

/// Applies `f` to every whole element of `width`; a trailing partial element
/// is left unchanged.
pub(crate) fn map_elems(
    data: &[u8],
    width: Width,
    endian: Endian,
    mut f: impl FnMut(u64) -> u64,
) -> Vec<u8> {
    let mut out = data.to_vec();
    let mask = width.mask();
    for chunk in out.chunks_exact_mut(width.bytes()) {
        let v = read_elem(chunk, endian);
        write_elem(chunk, f(v) & mask, endian);
    }
    out
}

/// Arbitrary bytes, serialised as a lowercase hex string (keys, IVs, AAD,
/// XOR keys). Deserialisation accepts upper/lower case, an optional `0x`
/// prefix and whitespace between digits.
#[derive(Clone, PartialEq, Eq, Hash, Default)]
pub struct HexBytes(pub Vec<u8>);

impl HexBytes {
    /// Parses a hex string (see the type docs for the accepted syntax).
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.trim();
        let s = s
            .strip_prefix("0x")
            .or_else(|| s.strip_prefix("0X"))
            .unwrap_or(s);
        let digits: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
        if !digits.len().is_multiple_of(2) {
            return Err(TransformError::encoding("hex", "odd number of hex digits"));
        }
        let nibble = |c: u8| -> Result<u8> {
            (c as char).to_digit(16).map(|d| d as u8).ok_or_else(|| {
                TransformError::encoding("hex", format!("'{}' is not a hex digit", c as char))
            })
        };
        digits
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| Ok((nibble(p[0])? << 4) | nibble(p[1])?))
            .collect::<Result<Vec<u8>>>()
            .map(Self)
    }

    /// The bytes.
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    /// Lowercase hex representation.
    pub fn to_hex(&self) -> String {
        use fmt::Write;
        self.0
            .iter()
            .fold(String::with_capacity(self.0.len() * 2), |mut s, b| {
                let _ = write!(s, "{b:02x}");
                s
            })
    }
}

impl From<Vec<u8>> for HexBytes {
    fn from(v: Vec<u8>) -> Self {
        Self(v)
    }
}

impl From<&[u8]> for HexBytes {
    fn from(v: &[u8]) -> Self {
        Self(v.to_vec())
    }
}

impl fmt::Debug for HexBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HexBytes({})", self.to_hex())
    }
}

impl Serialize for HexBytes {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for HexBytes {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// Serde adapter for 64-bit operands: written as a JSON number when it is
/// exactly representable in a JavaScript `number` (< 2^53), otherwise as a
/// decimal string. Reads numbers (including negative ones, stored as two's
/// complement) and strings in decimal, `-decimal`, `0x` hex or `0b` binary.
pub(crate) mod operand {
    use serde::{Deserialize, Deserializer, Serializer};

    const JS_SAFE: u64 = (1 << 53) - 1;

    pub fn serialize<S: Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
        if *v <= JS_SAFE {
            s.serialize_u64(*v)
        } else {
            s.serialize_str(&v.to_string())
        }
    }

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        U(u64),
        I(i64),
        S(String),
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        match Raw::deserialize(d)? {
            Raw::U(v) => Ok(v),
            Raw::I(v) => Ok(v as u64),
            Raw::S(s) => parse(&s).map_err(serde::de::Error::custom),
        }
    }

    /// Parses an operand literal.
    pub fn parse(s: &str) -> Result<u64, String> {
        let t: String = s
            .chars()
            .filter(|c| *c != '_' && !c.is_whitespace())
            .collect();
        let (neg, body) = match t.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, t.as_str()),
        };
        let lower = body.to_ascii_lowercase();
        let parsed = if let Some(h) = lower.strip_prefix("0x") {
            u64::from_str_radix(h, 16)
        } else if let Some(b) = lower.strip_prefix("0b") {
            u64::from_str_radix(b, 2)
        } else {
            lower.parse::<u64>()
        }
        .map_err(|e| format!("invalid operand '{s}': {e}"))?;
        Ok(if neg { parsed.wrapping_neg() } else { parsed })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elem_roundtrip() {
        let mut buf = [0u8; 4];
        write_elem(&mut buf, 0x1122_3344, Endian::Little);
        assert_eq!(buf, [0x44, 0x33, 0x22, 0x11]);
        assert_eq!(read_elem(&buf, Endian::Little), 0x1122_3344);
        write_elem(&mut buf, 0x1122_3344, Endian::Big);
        assert_eq!(buf, [0x11, 0x22, 0x33, 0x44]);
        assert_eq!(read_elem(&buf, Endian::Big), 0x1122_3344);
    }

    #[test]
    fn hex_bytes_parse() {
        assert_eq!(
            HexBytes::parse("0xDE ad be EF").unwrap().0,
            [0xde, 0xad, 0xbe, 0xef]
        );
        assert_eq!(HexBytes::parse("").unwrap().0, Vec::<u8>::new());
        assert!(HexBytes::parse("abc").is_err());
        assert!(HexBytes::parse("zz").is_err());
        let json = serde_json::to_string(&HexBytes(vec![1, 0xab])).unwrap();
        assert_eq!(json, "\"01ab\"");
    }

    #[test]
    fn width_serde() {
        assert_eq!(serde_json::to_string(&Width::W32).unwrap(), "32");
        assert_eq!(serde_json::from_str::<Width>("16").unwrap(), Width::W16);
        assert!(serde_json::from_str::<Width>("12").is_err());
    }

    #[test]
    fn operand_literals() {
        assert_eq!(operand::parse("0xFF").unwrap(), 255);
        assert_eq!(operand::parse("-1").unwrap(), u64::MAX);
        assert_eq!(operand::parse("0b1010").unwrap(), 10);
        assert_eq!(operand::parse("1_000").unwrap(), 1000);
        assert!(operand::parse("x").is_err());
    }
}
