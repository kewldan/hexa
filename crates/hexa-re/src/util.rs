//! Small byte-level helpers.

use crate::arch::Endian;

pub(crate) fn u16_at(b: &[u8], off: usize, e: Endian) -> Option<u16> {
    let s: [u8; 2] = b.get(off..off.checked_add(2)?)?.try_into().ok()?;
    Some(match e {
        Endian::Little => u16::from_le_bytes(s),
        Endian::Big => u16::from_be_bytes(s),
    })
}

pub(crate) fn u32_at(b: &[u8], off: usize, e: Endian) -> Option<u32> {
    let s: [u8; 4] = b.get(off..off.checked_add(4)?)?.try_into().ok()?;
    Some(match e {
        Endian::Little => u32::from_le_bytes(s),
        Endian::Big => u32::from_be_bytes(s),
    })
}

pub(crate) fn u64_at(b: &[u8], off: usize, e: Endian) -> Option<u64> {
    let s: [u8; 8] = b.get(off..off.checked_add(8)?)?.try_into().ok()?;
    Some(match e {
        Endian::Little => u64::from_le_bytes(s),
        Endian::Big => u64::from_be_bytes(s),
    })
}

pub(crate) fn le16(b: &[u8], off: usize) -> Option<u16> {
    u16_at(b, off, Endian::Little)
}

pub(crate) fn le32(b: &[u8], off: usize) -> Option<u32> {
    u32_at(b, off, Endian::Little)
}

/// Sign-extends the low `bits` bits of `v`.
pub(crate) fn sext(v: u64, bits: u32) -> i64 {
    let shift = 64 - bits;
    ((v << shift) as i64) >> shift
}

/// Lower-case hex without separators.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Parses hex, ignoring whitespace, `0x` prefixes and `:`/`-` separators.
pub(crate) fn parse_hex(s: &str) -> Option<Vec<u8>> {
    let cleaned: String = s
        .replace("0x", "")
        .replace("0X", "")
        .chars()
        .filter(|c| !c.is_whitespace() && *c != ':' && *c != '-' && *c != ',')
        .collect();
    if !cleaned.len().is_multiple_of(2) {
        return None;
    }
    (0..cleaned.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&cleaned[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip() {
        assert_eq!(hex(&[0, 0xab, 0x10]), "00ab10");
        assert_eq!(parse_hex("00 AB:10").unwrap(), vec![0, 0xab, 0x10]);
        assert_eq!(parse_hex("0x90 0x90").unwrap(), vec![0x90, 0x90]);
        assert!(parse_hex("abc").is_none());
    }

    #[test]
    fn sign_extension() {
        assert_eq!(sext(0x3ff_ffff, 26), -1);
        assert_eq!(sext(0x1, 26), 1);
        assert_eq!(u32_at(&[1, 2, 3, 4], 0, Endian::Big), Some(0x0102_0304));
    }
}
