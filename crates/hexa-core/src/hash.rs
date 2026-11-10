//! Checksums, CRCs and cryptographic hashes (ANL-01, ANL-02).

use digest::DynDigest;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::task::{Cancel, Progress};
use crate::tree::Tree;

/// Rocksoft-model CRC parameters (width 8…64).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrcParams {
    pub width: u8,
    pub poly: u64,
    pub init: u64,
    pub refin: bool,
    pub refout: bool,
    pub xorout: u64,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct CrcPreset {
    pub name: &'static str,
    #[serde(flatten)]
    pub params: CrcParams,
    /// CRC of `b"123456789"`.
    pub check: u64,
}

const fn p(
    name: &'static str,
    width: u8,
    poly: u64,
    init: u64,
    refin: bool,
    refout: bool,
    xorout: u64,
    check: u64,
) -> CrcPreset {
    CrcPreset {
        name,
        params: CrcParams {
            width,
            poly,
            init,
            refin,
            refout,
            xorout,
        },
        check,
    }
}

const F8: u64 = 0xFF;
const F16: u64 = 0xFFFF;
const F32: u64 = 0xFFFF_FFFF;
const F64: u64 = u64::MAX;

/// CRC catalogue (reveng.sourceforge.io parameters).
#[rustfmt::skip]
pub const CRC_PRESETS: &[CrcPreset] = &[
    p("CRC-8/SMBUS", 8, 0x07, 0, false, false, 0, 0xF4),
    p("CRC-8/AUTOSAR", 8, 0x2F, F8, false, false, F8, 0xDF),
    p("CRC-8/BLUETOOTH", 8, 0xA7, 0, true, true, 0, 0x26),
    p("CRC-8/CDMA2000", 8, 0x9B, F8, false, false, 0, 0xDA),
    p("CRC-8/DARC", 8, 0x39, 0, true, true, 0, 0x15),
    p("CRC-8/DVB-S2", 8, 0xD5, 0, false, false, 0, 0xBC),
    p("CRC-8/GSM-A", 8, 0x1D, 0, false, false, 0, 0x37),
    p("CRC-8/GSM-B", 8, 0x49, 0, false, false, F8, 0x94),
    p("CRC-8/HITAG", 8, 0x1D, F8, false, false, 0, 0xB4),
    p("CRC-8/I-432-1", 8, 0x07, 0, false, false, 0x55, 0xA1),
    p("CRC-8/I-CODE", 8, 0x1D, 0xFD, false, false, 0, 0x7E),
    p("CRC-8/LTE", 8, 0x9B, 0, false, false, 0, 0xEA),
    p("CRC-8/MAXIM-DOW", 8, 0x31, 0, true, true, 0, 0xA1),
    p("CRC-8/MIFARE-MAD", 8, 0x1D, 0xC7, false, false, 0, 0x99),
    p("CRC-8/NRSC-5", 8, 0x31, F8, false, false, 0, 0xF7),
    p("CRC-8/OPENSAFETY", 8, 0x2F, 0, false, false, 0, 0x3E),
    p("CRC-8/ROHC", 8, 0x07, F8, true, true, 0, 0xD0),
    p("CRC-8/SAE-J1850", 8, 0x1D, F8, false, false, F8, 0x4B),
    p("CRC-8/TECH-3250", 8, 0x1D, F8, true, true, 0, 0x97),
    p("CRC-8/WCDMA", 8, 0x9B, 0, true, true, 0, 0x25),
    p("CRC-10/ATM", 10, 0x233, 0, false, false, 0, 0x199),
    p("CRC-11/FLEXRAY", 11, 0x385, 0x1A, false, false, 0, 0x5A3),
    p("CRC-12/DECT", 12, 0x80F, 0, false, false, 0, 0xF5B),
    p("CRC-12/UMTS", 12, 0x80F, 0, false, true, 0, 0xDAF),
    p("CRC-15/CAN", 15, 0x4599, 0, false, false, 0, 0x059E),
    p("CRC-16/ARC", 16, 0x8005, 0, true, true, 0, 0xBB3D),
    p("CRC-16/CDMA2000", 16, 0xC867, F16, false, false, 0, 0x4C06),
    p("CRC-16/CMS", 16, 0x8005, F16, false, false, 0, 0xAEE7),
    p("CRC-16/DDS-110", 16, 0x8005, 0x800D, false, false, 0, 0x9ECF),
    p("CRC-16/DECT-R", 16, 0x0589, 0, false, false, 1, 0x007E),
    p("CRC-16/DECT-X", 16, 0x0589, 0, false, false, 0, 0x007F),
    p("CRC-16/DNP", 16, 0x3D65, 0, true, true, F16, 0xEA82),
    p("CRC-16/EN-13757", 16, 0x3D65, 0, false, false, F16, 0xC2B7),
    p("CRC-16/GENIBUS", 16, 0x1021, F16, false, false, F16, 0xD64E),
    p("CRC-16/GSM", 16, 0x1021, 0, false, false, F16, 0xCE3C),
    p("CRC-16/IBM-3740 (CCITT-FALSE)", 16, 0x1021, F16, false, false, 0, 0x29B1),
    p("CRC-16/IBM-SDLC (X-25)", 16, 0x1021, F16, true, true, F16, 0x906E),
    p("CRC-16/ISO-IEC-14443-3-A", 16, 0x1021, 0xC6C6, true, true, 0, 0xBF05),
    p("CRC-16/KERMIT", 16, 0x1021, 0, true, true, 0, 0x2189),
    p("CRC-16/LJ1200", 16, 0x6F63, 0, false, false, 0, 0xBDF4),
    p("CRC-16/M17", 16, 0x5935, F16, false, false, 0, 0x772B),
    p("CRC-16/MAXIM-DOW", 16, 0x8005, 0, true, true, F16, 0x44C2),
    p("CRC-16/MCRF4XX", 16, 0x1021, F16, true, true, 0, 0x6F91),
    p("CRC-16/MODBUS", 16, 0x8005, F16, true, true, 0, 0x4B37),
    p("CRC-16/NRSC-5", 16, 0x080B, F16, true, true, 0, 0xA066),
    p("CRC-16/OPENSAFETY-A", 16, 0x5935, 0, false, false, 0, 0x5D38),
    p("CRC-16/PROFIBUS", 16, 0x1DCF, F16, false, false, F16, 0xA819),
    p("CRC-16/RIELLO", 16, 0x1021, 0xB2AA, true, true, 0, 0x63D0),
    p("CRC-16/SPI-FUJITSU", 16, 0x1021, 0x1D0F, false, false, 0, 0xE5CC),
    p("CRC-16/T10-DIF", 16, 0x8BB7, 0, false, false, 0, 0xD0DB),
    p("CRC-16/TELEDISK", 16, 0xA097, 0, false, false, 0, 0x0FB3),
    p("CRC-16/TMS37157", 16, 0x1021, 0x89EC, true, true, 0, 0x26B1),
    p("CRC-16/UMTS", 16, 0x8005, 0, false, false, 0, 0xFEE8),
    p("CRC-16/USB", 16, 0x8005, F16, true, true, F16, 0xB4C8),
    p("CRC-16/XMODEM", 16, 0x1021, 0, false, false, 0, 0x31C3),
    p("CRC-24/BLE", 24, 0x0006_5B, 0x55_5555, true, true, 0, 0xC2_5A56),
    p("CRC-24/FLEXRAY-A", 24, 0x5D_6DCB, 0xFE_DCBA, false, false, 0, 0x79_79BD),
    p("CRC-24/LTE-A", 24, 0x86_4CFB, 0, false, false, 0, 0xCD_E703),
    p("CRC-24/LTE-B", 24, 0x80_0063, 0, false, false, 0, 0x23_EF52),
    p("CRC-24/OPENPGP", 24, 0x86_4CFB, 0xB7_04CE, false, false, 0, 0x21_CF02),
    p("CRC-32/ISO-HDLC", 32, 0x04C1_1DB7, F32, true, true, F32, 0xCBF4_3926),
    p("CRC-32/AIXM", 32, 0x8141_41AB, 0, false, false, 0, 0x3010_BF7F),
    p("CRC-32/AUTOSAR", 32, 0xF4AC_FB13, F32, true, true, F32, 0x1697_D06A),
    p("CRC-32/BASE91-D", 32, 0xA833_982B, F32, true, true, F32, 0x8731_5576),
    p("CRC-32/BZIP2", 32, 0x04C1_1DB7, F32, false, false, F32, 0xFC89_1918),
    p("CRC-32/CD-ROM-EDC", 32, 0x8001_801B, 0, true, true, 0, 0x6EC2_EDC4),
    p("CRC-32/CKSUM", 32, 0x04C1_1DB7, 0, false, false, F32, 0x765E_7680),
    p("CRC-32/ISCSI (CRC-32C)", 32, 0x1EDC_6F41, F32, true, true, F32, 0xE306_9283),
    p("CRC-32/JAMCRC", 32, 0x04C1_1DB7, F32, true, true, 0, 0x340B_C6D9),
    p("CRC-32/MPEG-2", 32, 0x04C1_1DB7, F32, false, false, 0, 0x0376_E6E7),
    p("CRC-32/XFER", 32, 0x0000_00AF, 0, false, false, 0, 0xBD0B_E338),
    p("CRC-40/GSM", 40, 0x00_0482_0009, 0, false, false, 0xFF_FFFF_FFFF, 0xD4_164F_C646),
    p("CRC-64/ECMA-182", 64, 0x42F0_E1EB_A9EA_3693, 0, false, false, 0, 0x6C40_DF5F_0B49_7347),
    p("CRC-64/GO-ISO", 64, 0x1B, F64, true, true, F64, 0xB909_56C7_75A4_1001),
    p("CRC-64/MS", 64, 0x259C_84CB_A642_6349, F64, true, true, 0, 0x75D4_B74F_024E_CEEA),
    p("CRC-64/NVME", 64, 0xAD93_D235_94C9_3659, F64, true, true, F64, 0xAE8B_1486_0A79_9888),
    p("CRC-64/REDIS", 64, 0xAD93_D235_94C9_35A9, 0, true, true, 0, 0xE9C6_D914_C4B8_D9CA),
    p("CRC-64/WE", 64, 0x42F0_E1EB_A9EA_3693, F64, false, false, F64, 0x62EC_59E3_F1A4_F00A),
    p("CRC-64/XZ", 64, 0x42F0_E1EB_A9EA_3693, F64, true, true, F64, 0x995D_C9BB_DF19_39FA),
];

fn reflect(mut v: u64, width: u8) -> u64 {
    let mut r = 0;
    for _ in 0..width {
        r = (r << 1) | (v & 1);
        v >>= 1;
    }
    r
}

fn mask(width: u8) -> u64 {
    if width == 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    }
}

/// Table-driven CRC engine.
#[derive(Debug, Clone)]
pub struct Crc {
    params: CrcParams,
    table: Box<[u64; 256]>,
    reg: u64,
}

impl Crc {
    pub fn new(params: CrcParams) -> Result<Self> {
        if !(8..=64).contains(&params.width) {
            return Err(Error::invalid("CRC width must be 8…64"));
        }
        let w = params.width;
        let m = mask(w);
        let mut table = Box::new([0u64; 256]);
        if params.refin {
            let poly = reflect(params.poly & m, w);
            for (b, slot) in table.iter_mut().enumerate() {
                let mut c = b as u64;
                for _ in 0..8 {
                    c = if c & 1 == 1 { (c >> 1) ^ poly } else { c >> 1 };
                }
                *slot = c;
            }
        } else {
            let top = 1u64 << (w - 1);
            for (b, slot) in table.iter_mut().enumerate() {
                let mut c = (b as u64) << (w - 8);
                for _ in 0..8 {
                    c = if c & top == 0 {
                        c << 1
                    } else {
                        (c << 1) ^ params.poly
                    };
                }
                *slot = c & m;
            }
        }
        let reg = if params.refin {
            reflect(params.init & m, w)
        } else {
            params.init & m
        };
        Ok(Self { params, table, reg })
    }

    pub fn update(&mut self, data: &[u8]) {
        let w = self.params.width;
        let m = mask(w);
        let mut r = self.reg;
        if self.params.refin {
            for &b in data {
                r = self.table[((r ^ u64::from(b)) & 0xFF) as usize] ^ (r >> 8);
            }
        } else {
            for &b in data {
                let idx = ((r >> (w - 8)) ^ u64::from(b)) & 0xFF;
                r = (self.table[idx as usize] ^ (r << 8)) & m;
            }
        }
        self.reg = r;
    }

    pub fn finish(&self) -> u64 {
        let w = self.params.width;
        let r = if self.params.refin == self.params.refout {
            self.reg
        } else {
            reflect(self.reg, w)
        };
        (r ^ self.params.xorout) & mask(w)
    }
}

/// One algorithm the user can pick.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlgoInfo {
    pub id: String,
    pub name: String,
    pub group: &'static str,
}

const CHECKSUMS: &[(&str, &str)] = &[
    ("sum8", "Sum-8"),
    ("sum16", "Sum-16"),
    ("sum32", "Sum-32"),
    ("xor8", "XOR-8"),
    ("adler32", "Adler-32"),
    ("fletcher16", "Fletcher-16"),
    ("fletcher32", "Fletcher-32"),
];

const CRYPTO: &[(&str, &str)] = &[
    ("md4", "MD4"),
    ("md5", "MD5"),
    ("sha1", "SHA-1"),
    ("sha224", "SHA-224"),
    ("sha256", "SHA-256"),
    ("sha384", "SHA-384"),
    ("sha512", "SHA-512"),
    ("sha3-224", "SHA3-224"),
    ("sha3-256", "SHA3-256"),
    ("sha3-384", "SHA3-384"),
    ("sha3-512", "SHA3-512"),
    ("blake2b", "BLAKE2b-512"),
    ("blake2s", "BLAKE2s-256"),
    ("blake3", "BLAKE3"),
    ("ripemd160", "RIPEMD-160"),
    ("whirlpool", "Whirlpool"),
    ("tiger", "Tiger"),
];

/// Every available algorithm. CRC ids are `crc:<preset name>`.
pub fn algorithms() -> Vec<AlgoInfo> {
    let mut v = Vec::new();
    for (id, name) in CHECKSUMS {
        v.push(AlgoInfo {
            id: (*id).into(),
            name: (*name).into(),
            group: "checksum",
        });
    }
    for preset in CRC_PRESETS {
        v.push(AlgoInfo {
            id: format!("crc:{}", preset.name),
            name: preset.name.into(),
            group: "crc",
        });
    }
    for (id, name) in CRYPTO {
        v.push(AlgoInfo {
            id: (*id).into(),
            name: (*name).into(),
            group: "crypto",
        });
    }
    v
}

enum Hasher {
    Sum(u8, u64),
    Xor8(u8),
    Adler32(u32, u32),
    Fletcher16(u16, u16),
    Fletcher32 { a: u32, b: u32, odd: Option<u8> },
    Crc(Crc),
    Digest(Box<dyn DynDigest + Send>),
    Blake3(Box<blake3::Hasher>),
}

impl Hasher {
    fn new(id: &str, custom: Option<CrcParams>) -> Result<Self> {
        Ok(match id {
            "sum8" => Self::Sum(8, 0),
            "sum16" => Self::Sum(16, 0),
            "sum32" => Self::Sum(32, 0),
            "xor8" => Self::Xor8(0),
            "adler32" => Self::Adler32(1, 0),
            "fletcher16" => Self::Fletcher16(0, 0),
            "fletcher32" => Self::Fletcher32 {
                a: 0,
                b: 0,
                odd: None,
            },
            "crc:custom" => Self::Crc(Crc::new(
                custom.ok_or_else(|| Error::invalid("custom CRC needs parameters"))?,
            )?),
            "md4" => Self::Digest(Box::new(md4::Md4::default())),
            "md5" => Self::Digest(Box::new(md5::Md5::default())),
            "sha1" => Self::Digest(Box::new(sha1::Sha1::default())),
            "sha224" => Self::Digest(Box::new(sha2::Sha224::default())),
            "sha256" => Self::Digest(Box::new(sha2::Sha256::default())),
            "sha384" => Self::Digest(Box::new(sha2::Sha384::default())),
            "sha512" => Self::Digest(Box::new(sha2::Sha512::default())),
            "sha3-224" => Self::Digest(Box::new(sha3::Sha3_224::default())),
            "sha3-256" => Self::Digest(Box::new(sha3::Sha3_256::default())),
            "sha3-384" => Self::Digest(Box::new(sha3::Sha3_384::default())),
            "sha3-512" => Self::Digest(Box::new(sha3::Sha3_512::default())),
            "blake2b" => Self::Digest(Box::new(blake2::Blake2b512::default())),
            "blake2s" => Self::Digest(Box::new(blake2::Blake2s256::default())),
            "ripemd160" => Self::Digest(Box::new(ripemd::Ripemd160::default())),
            "whirlpool" => Self::Digest(Box::new(whirlpool::Whirlpool::default())),
            "tiger" => Self::Digest(Box::new(tiger::Tiger::default())),
            "blake3" => Self::Blake3(Box::new(blake3::Hasher::new())),
            other => {
                let name = other
                    .strip_prefix("crc:")
                    .ok_or_else(|| Error::invalid(format!("unknown algorithm {other}")))?;
                let preset = CRC_PRESETS
                    .iter()
                    .find(|p| p.name == name)
                    .ok_or_else(|| Error::invalid(format!("unknown CRC {name}")))?;
                Self::Crc(Crc::new(preset.params)?)
            }
        })
    }

    fn update(&mut self, data: &[u8]) {
        match self {
            Self::Sum(_, s) => {
                *s = data
                    .iter()
                    .fold(*s, |acc, &b| acc.wrapping_add(u64::from(b)));
            }
            Self::Xor8(x) => *x = data.iter().fold(*x, |acc, &b| acc ^ b),
            Self::Adler32(a, b) => {
                for chunk in data.chunks(5552) {
                    for &byte in chunk {
                        *a += u32::from(byte);
                        *b += *a;
                    }
                    *a %= 65521;
                    *b %= 65521;
                }
            }
            Self::Fletcher16(a, b) => {
                for &byte in data {
                    *a = (*a + u16::from(byte)) % 255;
                    *b = (*b + *a) % 255;
                }
            }
            Self::Fletcher32 { a, b, odd } => {
                let mut push = |w: u16| {
                    *a = (*a + u32::from(w)) % 65535;
                    *b = (*b + *a) % 65535;
                };
                let mut rest = data;
                if let Some(lo) = odd.take() {
                    if let Some((&hi, r)) = rest.split_first() {
                        push(u16::from_le_bytes([lo, hi]));
                        rest = r;
                    } else {
                        *odd = Some(lo);
                    }
                }
                let (pairs, tail) = rest.as_chunks::<2>();
                for &pair in pairs {
                    push(u16::from_le_bytes(pair));
                }
                if let [last] = tail {
                    *odd = Some(*last);
                }
            }
            Self::Crc(c) => c.update(data),
            Self::Digest(d) => d.update(data),
            Self::Blake3(h) => {
                h.update(data);
            }
        }
    }

    fn finish(self) -> String {
        match self {
            Self::Sum(bits, s) => {
                let v = s & mask(bits);
                format!("{v:0width$X}", width = usize::from(bits / 4))
            }
            Self::Xor8(x) => format!("{x:02X}"),
            Self::Adler32(a, b) => format!("{:08X}", (b << 16) | a),
            Self::Fletcher16(a, b) => format!("{:04X}", (b << 8) | a),
            Self::Fletcher32 { mut a, mut b, odd } => {
                if let Some(lo) = odd {
                    a = (a + u32::from(lo)) % 65535;
                    b = (b + a) % 65535;
                }
                format!("{:08X}", (b << 16) | a)
            }
            Self::Crc(c) => {
                let w = usize::from(c.params.width).div_ceil(4);
                format!("{:0w$X}", c.finish())
            }
            Self::Digest(d) => hex_lower(&d.finalize()),
            Self::Blake3(h) => h.finalize().to_hex().to_string(),
        }
    }
}

fn hex_lower(b: &[u8]) -> String {
    use std::fmt::Write;
    b.iter()
        .fold(String::with_capacity(b.len() * 2), |mut s, x| {
            let _ = write!(s, "{x:02x}");
            s
        })
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HashResult {
    pub id: String,
    pub value: String,
}

/// Hashes bytes directly.
pub fn hash_bytes(
    data: &[u8],
    ids: &[String],
    custom: Option<CrcParams>,
) -> Result<Vec<HashResult>> {
    let mut hashers = ids
        .iter()
        .map(|id| Hasher::new(id, custom).map(|h| (id.clone(), h)))
        .collect::<Result<Vec<_>>>()?;
    for (_, h) in &mut hashers {
        h.update(data);
    }
    Ok(hashers
        .into_iter()
        .map(|(id, h)| HashResult {
            id,
            value: h.finish(),
        })
        .collect())
}

/// Hashes `[start, end)` of a snapshot with all algorithms in one pass.
pub fn hash_range(
    tree: &Tree,
    start: u64,
    end: u64,
    ids: &[String],
    custom: Option<CrcParams>,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<Vec<HashResult>> {
    let end = end.min(tree.len());
    let mut hashers = ids
        .iter()
        .map(|id| Hasher::new(id, custom).map(|h| (id.clone(), h)))
        .collect::<Result<Vec<_>>>()?;
    let mut buf = vec![0u8; 1 << 20];
    let mut at = start;
    while at < end {
        cancel.check()?;
        let n = (end - at).min(buf.len() as u64) as usize;
        tree.read(at, &mut buf[..n])?;
        for (_, h) in &mut hashers {
            h.update(&buf[..n]);
        }
        at += n as u64;
        progress(at - start, end - start);
    }
    Ok(hashers
        .into_iter()
        .map(|(id, h)| HashResult {
            id,
            value: h.finish(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(id: &str, data: &[u8]) -> String {
        hash_bytes(data, &[id.to_string()], None)
            .unwrap()
            .remove(0)
            .value
    }

    #[test]
    fn crc_presets_match_check_values() {
        for preset in CRC_PRESETS {
            let mut c = Crc::new(preset.params).unwrap();
            c.update(b"1234");
            c.update(b"56789");
            assert_eq!(c.finish(), preset.check, "{}", preset.name);
        }
    }

    #[test]
    fn checksums() {
        let d = b"123456789";
        assert_eq!(one("sum8", d), "DD");
        assert_eq!(one("xor8", d), "31");
        assert_eq!(one("adler32", b"Wikipedia"), "11E60398");
        assert_eq!(one("fletcher16", b"abcde"), "C8F0");
        assert_eq!(one("fletcher32", b"abcde"), "F04FC729");
        assert_eq!(one("fletcher32", b"abcdef"), "56502D2A");
    }

    #[test]
    fn crypto_hashes() {
        assert_eq!(one("md5", b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            one("sha1", b"abc"),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            one("sha256", b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(one("md4", b"abc"), "a448017aaf21d8525fc10ae87aa6729d");
        assert_eq!(
            one("ripemd160", b"abc"),
            "8eb208f7e05d987a9b044a8e98c6b087f15a0bfc"
        );
        assert_eq!(
            one("blake3", b""),
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
        for (id, _) in CRYPTO {
            assert_ne!(one(id, b"x"), "");
        }
    }

    #[test]
    fn range_hash_streams() {
        let data: Vec<u8> = (0..3_000_000u32).map(|i| i as u8).collect();
        let t = Tree::from_piece(crate::piece::Piece::bytes(data.clone()));
        let ids = vec!["crc:CRC-32/ISO-HDLC".to_string(), "sha256".into()];
        let a = hash_range(
            &t,
            10,
            2_500_000,
            &ids,
            None,
            &Cancel::new(),
            &crate::task::no_progress,
        )
        .unwrap();
        let b = hash_bytes(&data[10..2_500_000], &ids, None).unwrap();
        assert_eq!(a[0].value, b[0].value);
        assert_eq!(a[1].value, b[1].value);
    }
}
