//! Checksum recomputation (RE-06).
//!
//! * PE: the optional-header `CheckSum` field, computed exactly like
//!   `CheckSumMappedFile` (16-bit one's-complement sum of the file with the
//!   field treated as zero, folded, plus the file length).
//! * ELF and Mach-O images carry no whole-file checksum; Mach-O code
//!   signatures are hash trees that need re-signing, not patching.
//! * A generic helper recomputes CRC-32 / Adler-32 / simple sums over a
//!   byte range and stores the value in a field.

use hexa_core::Tree;
use serde::{Deserialize, Serialize};

use crate::arch::Endian;
use crate::error::{ReError, Result};
use crate::exe::{ExecutableFormat, detect_format};
use crate::patch::{Patch, encode_uint};

/// Computes the PE image checksum of `data`, treating the 4 bytes at
/// `checksum_offset` as zero.
pub fn pe_checksum_raw(data: &[u8], checksum_offset: usize) -> u32 {
    let mut sum: u64 = 0;
    let mut i = 0;
    while i < data.len() {
        let lo = data[i];
        let hi = data.get(i + 1).copied().unwrap_or(0);
        let word = if i >= checksum_offset && i < checksum_offset + 4 {
            0
        } else {
            u64::from(u16::from_le_bytes([lo, hi]))
        };
        sum += word;
        sum = (sum & 0xffff) + (sum >> 16);
        i += 2;
    }
    sum = (sum & 0xffff) + (sum >> 16);
    (sum as u32).wrapping_add(data.len() as u32)
}

/// Stored and recomputed checksum of an executable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageChecksum {
    /// Format of the file.
    pub format: Option<ExecutableFormat>,
    /// File offset of the checksum field (PE only).
    pub field_offset: Option<u64>,
    /// Stored value.
    pub stored: Option<u32>,
    /// Recomputed value.
    pub computed: Option<u32>,
    /// Explanation for formats without a checksum.
    pub note: String,
}

impl ImageChecksum {
    /// Whether the stored value is correct (or there is none).
    pub fn is_valid(&self) -> bool {
        self.stored == self.computed
    }

    /// A patch writing the recomputed value, if it differs.
    pub fn fix(&self) -> Option<Patch> {
        let (off, stored, computed) = (self.field_offset?, self.stored?, self.computed?);
        (stored != computed).then(|| Patch {
            offset: off,
            original: stored.to_le_bytes().to_vec(),
            replacement: computed.to_le_bytes().to_vec(),
            comment: "PE checksum".into(),
        })
    }
}

/// Examines the image checksum of an executable (RE-06).
pub fn image_checksum(data: &[u8]) -> ImageChecksum {
    let format = detect_format(data);
    match format {
        Some(ExecutableFormat::Pe) => match crate::exe::pe_checksum_offset(data) {
            Some(off) => ImageChecksum {
                format,
                field_offset: Some(off as u64),
                stored: crate::util::le32(data, off),
                computed: Some(pe_checksum_raw(data, off)),
                note: "optional header CheckSum (CheckSumMappedFile algorithm)".into(),
            },
            None => ImageChecksum {
                format,
                field_offset: None,
                stored: None,
                computed: None,
                note: "PE optional header not found".into(),
            },
        },
        Some(ExecutableFormat::Elf) => ImageChecksum {
            format,
            field_offset: None,
            stored: None,
            computed: None,
            note: "ELF has no file checksum".into(),
        },
        Some(ExecutableFormat::MachO | ExecutableFormat::MachOFat) => ImageChecksum {
            format,
            field_offset: None,
            stored: None,
            computed: None,
            note: "Mach-O has no file checksum; a code signature must be re-created with codesign"
                .into(),
        },
        None => ImageChecksum {
            format,
            field_offset: None,
            stored: None,
            computed: None,
            note: "not an executable".into(),
        },
    }
}

/// Recomputes and writes the PE checksum in place; returns the new value.
pub fn fix_pe_checksum(data: &mut [u8]) -> Result<u32> {
    let off = crate::exe::pe_checksum_offset(data)
        .ok_or_else(|| ReError::Invalid("not a PE image".into()))?;
    let v = pe_checksum_raw(data, off);
    data[off..off + 4].copy_from_slice(&v.to_le_bytes());
    Ok(v)
}

/// [`image_checksum`] for a tree.
pub fn image_checksum_tree(tree: &Tree) -> Result<ImageChecksum> {
    Ok(image_checksum(&crate::tree_bytes(tree)?))
}

/// Generic checksum algorithms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChecksumKind {
    /// CRC-32 (IEEE 802.3, zlib).
    Crc32,
    /// Adler-32 (zlib).
    Adler32,
    /// Sum of bytes modulo 2^8.
    Sum8,
    /// Sum of bytes modulo 2^16.
    Sum16,
    /// Sum of bytes modulo 2^32.
    Sum32,
    /// Two's complement of the byte sum (Intel HEX / ACPI style), 8 bits.
    TwosComplement8,
    /// XOR of all bytes.
    Xor8,
}

impl ChecksumKind {
    /// Field width in bytes.
    pub fn width(self) -> usize {
        match self {
            ChecksumKind::Crc32 | ChecksumKind::Adler32 | ChecksumKind::Sum32 => 4,
            ChecksumKind::Sum16 => 2,
            ChecksumKind::Sum8 | ChecksumKind::TwosComplement8 | ChecksumKind::Xor8 => 1,
        }
    }
}

/// "Recompute checksum `kind` over `[range_start, range_end)` and store it
/// at `field_offset`".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChecksumSpec {
    /// Algorithm.
    pub kind: ChecksumKind,
    /// Start of the covered range.
    pub range_start: u64,
    /// End (exclusive) of the covered range.
    pub range_end: u64,
    /// Where the value is stored.
    pub field_offset: u64,
    /// Byte order of the stored value.
    pub endian: Endian,
}

struct Acc {
    kind: ChecksumKind,
    crc: crc32fast::Hasher,
    adler: adler2::Adler32,
    sum: u64,
    xor: u8,
}

impl Acc {
    fn new(kind: ChecksumKind) -> Self {
        Acc {
            kind,
            crc: crc32fast::Hasher::new(),
            adler: adler2::Adler32::new(),
            sum: 0,
            xor: 0,
        }
    }

    fn update(&mut self, b: &[u8]) {
        match self.kind {
            ChecksumKind::Crc32 => self.crc.update(b),
            ChecksumKind::Adler32 => self.adler.write_slice(b),
            ChecksumKind::Xor8 => self.xor = b.iter().fold(self.xor, |a, x| a ^ x),
            _ => {
                self.sum = b
                    .iter()
                    .fold(self.sum, |a, &x| a.wrapping_add(u64::from(x)));
            }
        }
    }

    fn finish(self) -> u32 {
        match self.kind {
            ChecksumKind::Crc32 => self.crc.finalize(),
            ChecksumKind::Adler32 => self.adler.checksum(),
            ChecksumKind::Sum8 => (self.sum & 0xff) as u32,
            ChecksumKind::Sum16 => (self.sum & 0xffff) as u32,
            ChecksumKind::Sum32 => self.sum as u32,
            ChecksumKind::TwosComplement8 => u32::from((self.sum as u8).wrapping_neg()),
            ChecksumKind::Xor8 => u32::from(self.xor),
        }
    }
}

/// Result of [`compute_checksum_field`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldUpdate {
    /// Computed checksum.
    pub value: u32,
    /// Patch writing it (original = current field bytes).
    pub patch: Patch,
    /// Whether the field already holds the value.
    pub up_to_date: bool,
}

fn validate(spec: &ChecksumSpec, len: u64) -> Result<()> {
    if spec.range_start > spec.range_end || spec.range_end > len {
        return Err(ReError::Invalid(
            "checksum range is outside the data".into(),
        ));
    }
    if spec.field_offset + spec.kind.width() as u64 > len {
        return Err(ReError::Invalid(
            "checksum field is outside the data".into(),
        ));
    }
    Ok(())
}

fn finish_update(spec: &ChecksumSpec, value: u32, original: Vec<u8>) -> FieldUpdate {
    let replacement = encode_uint(u64::from(value), spec.kind.width(), spec.endian);
    FieldUpdate {
        value,
        up_to_date: original == replacement,
        patch: Patch {
            offset: spec.field_offset,
            original,
            replacement,
            comment: format!(
                "{:?} over {:#x}..{:#x}",
                spec.kind, spec.range_start, spec.range_end
            ),
        },
    }
}

/// Computes `spec` over a tree (streaming, 1 MiB chunks). When the field
/// lies inside the range it is treated as zero, the usual convention.
pub fn compute_checksum_field(tree: &Tree, spec: &ChecksumSpec) -> Result<FieldUpdate> {
    validate(spec, tree.len())?;
    let width = spec.kind.width() as u64;
    let field = spec.field_offset..spec.field_offset + width;
    let mut acc = Acc::new(spec.kind);
    let mut pos = spec.range_start;
    let mut buf = vec![0u8; 1 << 20];
    while pos < spec.range_end {
        let n = (spec.range_end - pos).min(buf.len() as u64) as usize;
        tree.read(pos, &mut buf[..n])?;
        for (i, b) in buf[..n].iter_mut().enumerate() {
            if field.contains(&(pos + i as u64)) {
                *b = 0;
            }
        }
        acc.update(&buf[..n]);
        pos += n as u64;
    }
    let original = tree.read_vec(spec.field_offset, width as usize)?;
    Ok(finish_update(spec, acc.finish(), original))
}

/// [`compute_checksum_field`] over a byte slice.
pub fn compute_checksum_field_bytes(data: &[u8], spec: &ChecksumSpec) -> Result<FieldUpdate> {
    validate(spec, data.len() as u64)?;
    let width = spec.kind.width();
    let f = spec.field_offset as usize;
    let (s, e) = (spec.range_start as usize, spec.range_end as usize);
    let mut acc = Acc::new(spec.kind);
    if f + width <= s || f >= e {
        acc.update(&data[s..e]);
    } else {
        let mut copy = data[s..e].to_vec();
        for i in f.max(s)..(f + width).min(e) {
            copy[i - s] = 0;
        }
        acc.update(&copy);
    }
    Ok(finish_update(
        spec,
        acc.finish(),
        data[f..f + width].to_vec(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hexa_core::piece::Piece;

    /// Reference implementation (pefile's dword-based algorithm).
    fn pefile_checksum(data: &[u8], off: usize) -> u32 {
        let mut checksum: u64 = 0;
        let top: u64 = 1 << 32;
        let mut padded = data.to_vec();
        while !padded.len().is_multiple_of(4) {
            padded.push(0);
        }
        for i in 0..padded.len() / 4 {
            if i == off / 4 {
                continue;
            }
            let d = u64::from(u32::from_le_bytes(
                padded[i * 4..i * 4 + 4].try_into().unwrap(),
            ));
            checksum = (checksum & 0xffff_ffff) + d + (checksum >> 32);
            if checksum > top {
                checksum = (checksum & 0xffff_ffff) + (checksum >> 32);
            }
        }
        checksum = (checksum & 0xffff) + (checksum >> 16);
        checksum += checksum >> 16;
        checksum &= 0xffff;
        (checksum + data.len() as u64) as u32
    }

    #[test]
    fn pe_checksum_matches_reference() {
        let mut x: u32 = 12345;
        for len in [0usize, 1, 2, 3, 100, 1001, 4096, 70_001] {
            let data: Vec<u8> = (0..len)
                .map(|_| {
                    x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                    (x >> 16) as u8
                })
                .collect();
            let off = 0x40.min(len / 4 * 4);
            assert_eq!(
                pe_checksum_raw(&data, off),
                pefile_checksum(&data, off),
                "len {len}"
            );
        }
        // All 0xff words exercise the carry folding.
        let data = vec![0xffu8; 1000];
        assert_eq!(pe_checksum_raw(&data, 0x40), pefile_checksum(&data, 0x40));
    }

    #[test]
    fn checksum_field_is_ignored() {
        let mut a = vec![7u8; 256];
        let b = a.clone();
        a[0x58..0x5c].copy_from_slice(&[1, 2, 3, 4]);
        assert_eq!(pe_checksum_raw(&a, 0x58), pe_checksum_raw(&b, 0x58));
    }

    #[test]
    fn non_pe_formats() {
        let mut elf = vec![0x7f, b'E', b'L', b'F'];
        elf.resize(64, 0);
        let c = image_checksum(&elf);
        assert_eq!(c.format, Some(ExecutableFormat::Elf));
        assert!(c.is_valid());
        assert!(c.fix().is_none());
        let c = image_checksum(&[0xcf, 0xfa, 0xed, 0xfe]);
        assert!(c.note.contains("Mach-O"));
        assert!(image_checksum(b"hello").format.is_none());
        assert!(fix_pe_checksum(&mut [0u8; 8]).is_err());
    }

    #[test]
    fn generic_fields() {
        let mut data = b"123456789".to_vec();
        data.extend([0u8; 4]);
        let spec = ChecksumSpec {
            kind: ChecksumKind::Crc32,
            range_start: 0,
            range_end: 9,
            field_offset: 9,
            endian: Endian::Little,
        };
        let u = compute_checksum_field_bytes(&data, &spec).unwrap();
        assert_eq!(u.value, 0xcbf4_3926);
        assert_eq!(u.patch.replacement, vec![0x26, 0x39, 0xf4, 0xcb]);
        assert!(!u.up_to_date);
        let tree = Tree::from_piece(Piece::bytes(data.clone()));
        assert_eq!(compute_checksum_field(&tree, &spec).unwrap(), u);

        let spec = ChecksumSpec {
            kind: ChecksumKind::Adler32,
            endian: Endian::Big,
            ..spec
        };
        let u = compute_checksum_field_bytes(
            b"Wikipedia\0\0\0\0",
            &ChecksumSpec {
                range_end: 9,
                ..spec
            },
        )
        .unwrap();
        assert_eq!(u.value, 0x11e6_0398);
        assert_eq!(u.patch.replacement, vec![0x11, 0xe6, 0x03, 0x98]);

        // Field inside the range counts as zero; sums and two's complement.
        let data = [1u8, 2, 3, 0xaa, 4];
        let spec = ChecksumSpec {
            kind: ChecksumKind::TwosComplement8,
            range_start: 0,
            range_end: 5,
            field_offset: 3,
            endian: Endian::Little,
        };
        let u = compute_checksum_field_bytes(&data, &spec).unwrap();
        assert_eq!(u.value, u32::from(10u8.wrapping_neg()));
        let t = Tree::from_piece(Piece::bytes(data.to_vec()));
        assert_eq!(compute_checksum_field(&t, &spec).unwrap().value, u.value);
        let x = compute_checksum_field_bytes(
            &data,
            &ChecksumSpec {
                kind: ChecksumKind::Xor8,
                ..spec
            },
        )
        .unwrap();
        assert_eq!(x.value, 4);
        let s16 = compute_checksum_field_bytes(
            &[0xff; 300],
            &ChecksumSpec {
                kind: ChecksumKind::Sum16,
                range_start: 0,
                range_end: 298,
                field_offset: 298,
                endian: Endian::Big,
            },
        )
        .unwrap();
        assert_eq!(s16.value, (298 * 255) & 0xffff);
        assert!(
            compute_checksum_field_bytes(
                &data,
                &ChecksumSpec {
                    range_end: 9,
                    ..spec
                }
            )
            .is_err()
        );
    }
}
