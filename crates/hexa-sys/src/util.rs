//! Small shared helpers: bounds-checked integer decoding, checksums, GUIDs,
//! UTF-16 decoding and a fast PRNG for wipes and shredding.

use std::io;

use hexa_core::provider::Provider;
use hexa_core::{Error, Result};

/// A `hexa_core::Error::Invalid` with `msg`.
pub(crate) fn invalid(msg: impl Into<String>) -> Error {
    Error::Invalid(msg.into())
}

/// Fails with `UnexpectedEof` unless `[offset, offset + len)` fits in `total`.
pub(crate) fn check_range(offset: u64, len: usize, total: u64) -> io::Result<()> {
    match offset.checked_add(len as u64) {
        Some(end) if end <= total => Ok(()),
        _ => Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("read {offset}+{len} past end {total}"),
        )),
    }
}

/// Reads exactly `len` bytes at `offset`.
pub(crate) fn read_vec(p: &dyn Provider, offset: u64, len: usize) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; len];
    p.read_at(offset, &mut buf)?;
    Ok(buf)
}

/// Reads up to `len` bytes at `offset`, clamped to the provider length.
pub(crate) fn read_upto(p: &dyn Provider, offset: u64, len: usize) -> io::Result<Vec<u8>> {
    let avail = p.len().saturating_sub(offset).min(len as u64) as usize;
    read_vec(p, offset, avail)
}

macro_rules! int_reader {
    ($name:ident, $ty:ty, $conv:ident) => {
        /// Decodes an integer at `off`; out-of-bounds reads yield 0 so parsers
        /// of untrusted images never panic.
        #[inline]
        pub(crate) fn $name(b: &[u8], off: usize) -> $ty {
            const N: usize = std::mem::size_of::<$ty>();
            match off.checked_add(N).and_then(|end| b.get(off..end)) {
                Some(s) => <$ty>::$conv(s.try_into().expect("slice length")),
                None => 0,
            }
        }
    };
}

int_reader!(le16, u16, from_le_bytes);
int_reader!(le32, u32, from_le_bytes);
int_reader!(le64, u64, from_le_bytes);
int_reader!(be16, u16, from_be_bytes);
int_reader!(be32, u32, from_be_bytes);
int_reader!(be64, u64, from_be_bytes);

/// Byte at `off` or 0.
#[inline]
pub(crate) fn byte(b: &[u8], off: usize) -> u8 {
    b.get(off).copied().unwrap_or(0)
}

/// Sub-slice `[off, off + len)` clamped to `b`.
pub(crate) fn slice(b: &[u8], off: usize, len: usize) -> &[u8] {
    let start = off.min(b.len());
    let end = off.saturating_add(len).min(b.len());
    &b[start..end]
}

/// ASCII/Latin-1 text with trailing NULs and spaces removed.
pub(crate) fn ascii(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    b[..end]
        .iter()
        .map(|&c| c as char)
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// UTF-16LE text, cut at the first NUL.
pub(crate) fn utf16le(b: &[u8]) -> String {
    let units: Vec<u16> = b
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// UTF-16BE text, cut at the first NUL.
pub(crate) fn utf16be(b: &[u8]) -> String {
    let units: Vec<u16> = b
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_be_bytes(*c))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

// ---------------------------------------------------------------------------
// Checksums
// ---------------------------------------------------------------------------

const fn crc_table(poly: u32) -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { poly ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

static CRC32_TABLE: [u32; 256] = crc_table(0xEDB8_8320);
static CRC32C_TABLE: [u32; 256] = crc_table(0x82F6_3B78);

fn crc_with(table: &[u32; 256], data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c = table[((c ^ u32::from(b)) & 0xFF) as usize] ^ (c >> 8);
    }
    !c
}

/// CRC-32 (IEEE 802.3), used by GPT.
pub(crate) fn crc32(data: &[u8]) -> u32 {
    crc_with(&CRC32_TABLE, data)
}

/// CRC-32C (Castagnoli), used by VHDX.
pub(crate) fn crc32c(data: &[u8]) -> u32 {
    crc_with(&CRC32C_TABLE, data)
}

/// Adler-32, used by EWF section and table checksums.
pub(crate) fn adler32(data: &[u8]) -> u32 {
    adler32_update(1, data)
}

/// Continues an Adler-32 checksum.
pub(crate) fn adler32_update(state: u32, data: &[u8]) -> u32 {
    const MOD: u32 = 65_521;
    let mut a = state & 0xFFFF;
    let mut b = state >> 16;
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += u32::from(x);
            b += a;
        }
        a %= MOD;
        b %= MOD;
    }
    (b << 16) | a
}

// ---------------------------------------------------------------------------
// GUIDs
// ---------------------------------------------------------------------------

/// Formats an on-disk (mixed-endian, Microsoft layout) GUID.
pub(crate) fn guid_string(b: &[u8]) -> String {
    if b.len() < 16 {
        return String::new();
    }
    format!(
        "{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        le32(b, 0),
        le16(b, 4),
        le16(b, 6),
        b[8],
        b[9],
        b[10],
        b[11],
        b[12],
        b[13],
        b[14],
        b[15]
    )
}

/// Formats a big-endian (RFC 4122 byte order) UUID.
pub(crate) fn uuid_string(b: &[u8]) -> String {
    if b.len() < 16 {
        return String::new();
    }
    let h = hex_upper(&b[..16]);
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// Uppercase hex of `b` without separators.
pub(crate) fn hex_upper(b: &[u8]) -> String {
    use std::fmt::Write as _;
    b.iter()
        .fold(String::with_capacity(b.len() * 2), |mut s, x| {
            let _ = write!(s, "{x:02X}");
            s
        })
}

/// Parses `XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX` into on-disk mixed-endian bytes.
pub(crate) fn guid_bytes(s: &str) -> Option<[u8; 16]> {
    let hex: String = s.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 {
        return None;
    }
    let mut raw = [0u8; 16];
    for (i, out) in raw.iter_mut().enumerate() {
        *out = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    raw[0..4].reverse();
    raw[4..6].reverse();
    raw[6..8].reverse();
    Some(raw)
}

// ---------------------------------------------------------------------------
// PRNG
// ---------------------------------------------------------------------------

/// xoshiro256** — fast, non-cryptographic generator for wipe patterns. Seeds
/// come from the OS so passes are unpredictable, while a stored seed lets the
/// verify pass regenerate the exact same stream.
#[derive(Debug, Clone)]
pub(crate) struct Rng {
    s: [u64; 4],
}

impl Rng {
    pub(crate) fn from_seed(seed: u64) -> Self {
        // SplitMix64 expands the seed into the state.
        let mut x = seed;
        let mut next = || {
            x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = x;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        };
        Self {
            s: [next(), next(), next(), next()],
        }
    }

    /// A seed from the operating system's CSPRNG.
    pub(crate) fn os_seed() -> u64 {
        let mut b = [0u8; 8];
        if getrandom::fill(&mut b).is_err() {
            // Fall back to the clock; still fine for overwrite patterns.
            let t = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos() as u64);
            return t ^ u64::from(std::process::id()).rotate_left(32);
        }
        u64::from_le_bytes(b)
    }

    pub(crate) fn next_u64(&mut self) -> u64 {
        let result = self.s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = self.s[1] << 17;
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);
        result
    }

    pub(crate) fn fill(&mut self, buf: &mut [u8]) {
        let (chunks, rest) = buf.as_chunks_mut::<8>();
        for c in chunks {
            *c = self.next_u64().to_le_bytes();
        }
        if !rest.is_empty() {
            let v = self.next_u64().to_le_bytes();
            rest.copy_from_slice(&v[..rest.len()]);
        }
    }
}

// ---------------------------------------------------------------------------
// Positional file I/O
// ---------------------------------------------------------------------------

/// Reads exactly `buf.len()` bytes at `offset` without moving a file cursor.
#[cfg(unix)]
pub(crate) fn pread(file: &std::fs::File, offset: u64, buf: &mut [u8]) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(buf, offset)
}

/// Reads exactly `buf.len()` bytes at `offset` without moving a file cursor.
#[cfg(windows)]
pub(crate) fn pread(file: &std::fs::File, mut offset: u64, mut buf: &mut [u8]) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_read(buf, offset) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                buf = &mut buf[n..];
                offset += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Writes all of `data` at `offset`.
#[cfg(unix)]
pub(crate) fn pwrite(file: &std::fs::File, offset: u64, data: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.write_all_at(data, offset)
}

/// Writes all of `data` at `offset`.
#[cfg(windows)]
pub(crate) fn pwrite(file: &std::fs::File, mut offset: u64, mut data: &[u8]) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !data.is_empty() {
        match file.seek_write(data, offset) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => {
                data = &data[n..];
                offset += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Display name of a path (its file name).
pub(crate) fn display_name(path: &std::path::Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// Converts a progress/cancel check into the crate's error.
pub(crate) fn check_cancel(cancel: Option<&hexa_core::Cancel>) -> Result<()> {
    match cancel {
        Some(c) => c.check(),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums_match_known_vectors() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn guid_round_trip() {
        let s = "C12A7328-F81F-11D2-BA4B-00A0C93EC93B";
        let b = guid_bytes(s).unwrap();
        assert_eq!(b[0], 0x28);
        assert_eq!(guid_string(&b), s);
    }

    #[test]
    fn readers_never_panic() {
        assert_eq!(le32(&[1, 2], 0), 0);
        assert_eq!(le16(&[1, 2], 0), 0x0201);
        assert_eq!(be32(&[0, 0, 1, 0], 0), 256);
        assert_eq!(le64(&[], usize::MAX), 0);
        assert_eq!(slice(&[1, 2, 3], 2, 10), &[3]);
    }

    #[test]
    fn rng_is_deterministic_per_seed() {
        let mut a = Rng::from_seed(7);
        let mut b = Rng::from_seed(7);
        let mut x = [0u8; 13];
        let mut y = [0u8; 13];
        a.fill(&mut x);
        b.fill(&mut y);
        assert_eq!(x, y);
        assert_ne!(x, [0u8; 13]);
    }
}
