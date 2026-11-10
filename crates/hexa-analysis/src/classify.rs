//! Region classification (ANL-09): splits a range into blocks, labels each
//! block as zero / padding / text / code / compressed / encrypted / data
//! with a confidence, and merges neighbours with the same label.
//!
//! Heuristics:
//! * zero / padding — one byte value covers ≥ 97 % of the block;
//! * text — printable ratio of ASCII, UTF-8 or UTF-16 (LE/BE) ≥ 90–95 %,
//!   or NUL-separated ASCII string tables;
//! * code — x86 opcode bigram statistics (most frequent x86-64 opcode pairs),
//!   `AArch64` instruction top-byte distribution, ARM32 condition nibble;
//! * compressed / encrypted — entropy close to the maximum expected for the
//!   block size; a chi-square test against the uniform distribution
//!   separates encrypted (uniform) from compressed data;
//! * data — everything else.

use hexa_core::task::Progress;
use hexa_core::{Cancel, Result, Tree};
use serde::{Deserialize, Serialize};

use crate::util::{self, chi_square_of, entropy_of, invalid};

/// Label of a region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RegionKind {
    Zero,
    Padding,
    Text,
    Code,
    Compressed,
    Encrypted,
    Data,
}

/// Classification of one block or merged region.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Region {
    pub start: u64,
    pub end: u64,
    pub kind: RegionKind,
    /// Sub-kind: padding byte (`0xFF`), text encoding (`ascii`, `utf-8`,
    /// `utf-16le`, `utf-16be`), architecture (`x86`, `arm64`, `arm32`).
    pub detail: Option<String>,
    /// 0…1.
    pub confidence: f32,
    /// Bits per byte.
    pub entropy: f32,
}

/// Classification of a single buffer.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockClass {
    pub kind: RegionKind,
    pub detail: Option<String>,
    pub confidence: f32,
    pub entropy: f32,
}

/// The most frequent x86-64 opcode bigrams (measured on compiled code).
const X86_BIGRAMS: [[u8; 2]; 48] = [
    [0x48, 0x8D],
    [0x48, 0x89],
    [0x48, 0x8B],
    [0x4C, 0x89],
    [0x8D, 0x05],
    [0xFE, 0xFF],
    [0xFF, 0x48],
    [0x0F, 0x84],
    [0x4C, 0x8D],
    [0x85, 0xC0],
    [0x49, 0x89],
    [0x48, 0x83],
    [0x48, 0x85],
    [0x0F, 0x85],
    [0x41, 0x8B],
    [0x44, 0x89],
    [0x8D, 0x3D],
    [0x49, 0x8B],
    [0x31, 0xC0],
    [0x4C, 0x8B],
    [0x8D, 0x35],
    [0x89, 0xE5],
    [0x55, 0x48],
    [0x8D, 0x0D],
    [0x41, 0x89],
    [0x45, 0x31],
    [0x89, 0xC7],
    [0x89, 0xDF],
    [0x5D, 0xC3],
    [0x83, 0xF8],
    [0xC0, 0x0F],
    [0x08, 0x48],
    [0xFF, 0xE8],
    [0x89, 0x45],
    [0x31, 0xF6],
    [0x8B, 0x7D],
    [0x89, 0xF7],
    [0xFF, 0x4C],
    [0x8B, 0x45],
    [0x41, 0x83],
    [0xC0, 0x74],
    [0x0F, 0xB6],
    [0x5B, 0x41],
    [0x41, 0x5E],
    [0x83, 0xC4],
    [0x48, 0xFF],
    [0x85, 0xFF],
    [0x49, 0x63],
];

/// Most frequent `AArch64` instruction top bytes (bits 31…24).
const ARM64_TOP: [u8; 24] = [
    0x91, 0xF9, 0xB9, 0xAA, 0x94, 0x52, 0xA9, 0x54, 0x71, 0x14, 0x39, 0x34, 0x90, 0xD0, 0x97, 0xF0,
    0xB0, 0xB4, 0xD6, 0x12, 0x17, 0x6B, 0x1A, 0x35,
];

fn x86_table() -> &'static [bool] {
    static T: std::sync::OnceLock<Vec<bool>> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let mut t = vec![false; 1 << 16];
        for [a, b] in X86_BIGRAMS {
            t[usize::from(a) << 8 | usize::from(b)] = true;
        }
        t
    })
}

/// Share of positions that start a common x86 opcode bigram
/// (≈0.16 for x86-64 code, < 0.002 for random data or ARM code).
fn x86_score(data: &[u8]) -> f32 {
    if data.len() < 2 {
        return 0.0;
    }
    let t = x86_table();
    let hits = data
        .windows(2)
        .filter(|w| t[usize::from(w[0]) << 8 | usize::from(w[1])])
        .count();
    hits as f32 / data.len() as f32
}

/// Best (over the four alignments) share of 32-bit words matching `pred`.
fn word_score(data: &[u8], pred: impl Fn(u8) -> bool) -> f32 {
    let mut best = 0.0f32;
    for align in 0..4 {
        if data.len() < align + 4 {
            break;
        }
        let words = (data.len() - align) / 4;
        let hits = (0..words).filter(|i| pred(data[align + i * 4 + 3])).count();
        best = best.max(hits as f32 / words as f32);
    }
    best
}

fn is_text_byte(b: u8) -> bool {
    (0x20..0x7F).contains(&b) || matches!(b, b'\t' | b'\n' | b'\r')
}

fn printable_char(c: char) -> bool {
    !c.is_control() || matches!(c, '\t' | '\n' | '\r')
}

fn utf8_score(data: &[u8]) -> Option<f32> {
    let valid = match std::str::from_utf8(data) {
        Ok(s) => s,
        Err(e) if e.error_len().is_none() && data.len() - e.valid_up_to() < 4 => {
            std::str::from_utf8(&data[..e.valid_up_to()]).ok()?
        }
        Err(_) => return None,
    };
    if valid.is_ascii() {
        return None;
    }
    let (good, total) = valid.chars().fold((0usize, 0usize), |(g, t), c| {
        (g + usize::from(printable_char(c)), t + 1)
    });
    (total > 0).then(|| good as f32 / total as f32)
}

fn utf16_score(data: &[u8], big_endian: bool) -> f32 {
    let mut best = 0.0f32;
    for align in 0..2 {
        let units: Vec<u16> = data[align.min(data.len())..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&p| {
                if big_endian {
                    u16::from_be_bytes(p)
                } else {
                    u16::from_le_bytes(p)
                }
            })
            .collect();
        if units.is_empty() {
            continue;
        }
        let good = units
            .iter()
            .filter(|&&u| {
                u < 0x100 && is_text_byte(u as u8)
                    || char::from_u32(u32::from(u))
                        .is_some_and(|c| u >= 0x100 && (c.is_alphabetic() || c.is_whitespace()))
            })
            .count();
        // Require mostly ASCII-range units so random data does not qualify.
        let ascii = units.iter().filter(|&&u| u < 0x80).count();
        if ascii * 2 >= units.len() {
            // Prefer the natural (even) alignment on ties.
            let penalty = if align == 0 { 1.0 } else { 0.99 };
            best = best.max(penalty * good as f32 / units.len() as f32);
        }
    }
    best
}

/// Entropy expected from `n` uniformly random bytes (Miller–Madow bias).
fn random_entropy(n: usize) -> f64 {
    let n = n as f64;
    let k = 256f64.min(n);
    k.log2().min(8.0) - (k - 1.0) / (2.0 * n * std::f64::consts::LN_2)
}

/// Classifies one buffer (ANL-09).
pub fn classify_bytes(data: &[u8]) -> BlockClass {
    let mut counts = [0u64; 256];
    for &b in data {
        counts[usize::from(b)] += 1;
    }
    let n = data.len();
    let total = n as u64;
    let entropy = entropy_of(&counts, total) as f32;
    let class = |kind, detail: Option<&str>, confidence: f32| BlockClass {
        kind,
        detail: detail.map(str::to_string),
        confidence: confidence.clamp(0.0, 1.0),
        entropy,
    };
    if n == 0 {
        return class(RegionKind::Data, None, 0.0);
    }
    let (top, top_count) = counts
        .iter()
        .enumerate()
        .max_by_key(|(_, c)| **c)
        .map(|(b, c)| (b as u8, *c))
        .expect("256 entries");
    let top_ratio = top_count as f32 / n as f32;
    if top_ratio >= 0.97 {
        return if top == 0 {
            class(RegionKind::Zero, None, top_ratio)
        } else {
            class(
                RegionKind::Padding,
                Some(&format!("0x{top:02X}")),
                top_ratio,
            )
        };
    }

    let ascii = data.iter().filter(|&&b| is_text_byte(b)).count() as f32 / n as f32;
    if ascii >= 0.95 {
        return class(RegionKind::Text, Some("ascii"), ascii);
    }
    if let Some(s) = utf8_score(data).filter(|&s| s >= 0.95) {
        return class(RegionKind::Text, Some("utf-8"), s);
    }
    let le = utf16_score(data, false);
    let be = utf16_score(data, true);
    if le.max(be) >= 0.9 {
        return if le >= be {
            class(RegionKind::Text, Some("utf-16le"), le)
        } else {
            class(RegionKind::Text, Some("utf-16be"), be)
        };
    }

    // NUL-separated string tables (C string sections, resource names).
    let zeros = counts[0] as f32 / n as f32;
    if ascii >= 0.5 && ascii + zeros >= 0.95 {
        return class(RegionKind::Text, Some("ascii"), (ascii + zeros) * 0.9);
    }

    let expected = random_entropy(n);
    if n >= 64 && f64::from(entropy) >= expected - 0.25 {
        if n < 1024 {
            return class(RegionKind::Compressed, Some("compressed or encrypted"), 0.5);
        }
        // Chi-square with 255 d.o.f.: mean 255, sd ≈ 22.6 for uniform data
        // (cipher output); compressors usually leave more structure.
        let chi = chi_square_of(&counts, total) as f32;
        // Good compressors are close to uniform too, so the split is soft.
        return if chi < 290.0 {
            class(
                RegionKind::Encrypted,
                None,
                0.75 - (chi - 200.0).max(0.0) / 450.0,
            )
        } else {
            class(
                RegionKind::Compressed,
                None,
                (0.55 + (chi - 290.0) / 1000.0).min(0.9),
            )
        };
    }

    if n >= 64 {
        let x86 = x86_score(data);
        let arm64 = word_score(data, |b| ARM64_TOP.contains(&b));
        let arm32 = word_score(data, |b| b >> 4 == 0xE);
        let cands = [
            (x86 / 0.12, x86 >= 0.04, "x86"),
            ((arm64 - 0.1) / 0.6, arm64 >= 0.45, "arm64"),
            ((arm32 - 0.1) / 0.6, arm32 >= 0.5, "arm32"),
        ];
        if let Some((conf, _, arch)) = cands
            .iter()
            .filter(|c| c.1)
            .max_by(|a, b| a.0.total_cmp(&b.0))
        {
            return class(RegionKind::Code, Some(arch), conf.min(0.95));
        }
    }
    class(RegionKind::Data, None, 0.5)
}

/// Default classification block size.
pub const DEFAULT_BLOCK: u64 = 4096;

/// Classifies `[start, end)` in blocks of `block_size` (≥ 64) and merges
/// neighbouring blocks with the same kind and detail.
pub fn classify(
    tree: &Tree,
    start: u64,
    end: u64,
    block_size: u64,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<Vec<Region>> {
    if !(64..=util::CHUNK as u64).contains(&block_size) {
        return Err(invalid("classification block size must be 64 B … 4 MiB"));
    }
    let (start, end) = util::clamp(tree, start, end);
    let chunk = (util::CHUNK as u64 / block_size * block_size) as usize;
    let mut regions: Vec<Region> = Vec::new();
    util::scan(tree, start, end, chunk, cancel, progress, |at, data| {
        for (i, block) in data.chunks(block_size as usize).enumerate() {
            let c = classify_bytes(block);
            let s = at + i as u64 * block_size;
            let e = s + block.len() as u64;
            match regions.last_mut() {
                Some(r) if r.kind == c.kind && r.detail == c.detail && r.end == s => {
                    let (w0, w1) = ((r.end - r.start) as f32, block.len() as f32);
                    r.confidence = (r.confidence * w0 + c.confidence * w1) / (w0 + w1);
                    r.entropy = (r.entropy * w0 + c.entropy * w1) / (w0 + w1);
                    r.end = e;
                }
                _ => regions.push(Region {
                    start: s,
                    end: e,
                    kind: c.kind,
                    detail: c.detail,
                    confidence: c.confidence,
                    entropy: c.entropy,
                }),
            }
        }
        Ok(true)
    })?;
    Ok(regions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{noop, random_bytes, tree};

    /// Hand-assembled x86-64 functions (prologue, moves, calls, epilogue).
    fn x86_code(len: usize) -> Vec<u8> {
        let f: &[u8] = &[
            0x55, 0x48, 0x89, 0xE5, 0x41, 0x57, 0x41, 0x56, 0x53, 0x50, 0x48, 0x89, 0xFB, 0x48,
            0x8B, 0x7F, 0x08, 0x48, 0x85, 0xFF, 0x0F, 0x84, 0x20, 0x00, 0x00, 0x00, 0x4C, 0x8D,
            0x35, 0x10, 0x20, 0x00, 0x00, 0xE8, 0x40, 0x10, 0x00, 0x00, 0x85, 0xC0, 0x74, 0x0A,
            0x48, 0x8B, 0x45, 0xF0, 0x48, 0x89, 0xC7, 0x31, 0xF6, 0xE8, 0x00, 0x01, 0x00, 0x00,
            0x48, 0x83, 0xC4, 0x08, 0x5B, 0x41, 0x5E, 0x41, 0x5F, 0x5D, 0xC3, 0x0F, 0x1F, 0x00,
        ];
        f.iter().copied().cycle().take(len).collect()
    }

    /// `AArch64` words (stp, mov, bl, ldr, add, cbz, ldp, ret …), little-endian.
    fn arm64_code(len: usize) -> Vec<u8> {
        let words: [u32; 12] = [
            0xA9BF_7BFD,
            0x9100_03FD,
            0xAA00_03F3,
            0x9400_0010,
            0xF940_0660,
            0xB940_0A61,
            0x7100_043F,
            0x5400_0081,
            0x5280_0020,
            0xB400_0060,
            0xA8C1_7BFD,
            0xD65F_03C0,
        ];
        words
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .cycle()
            .take(len)
            .collect()
    }

    #[test]
    fn single_block_labels() {
        assert_eq!(classify_bytes(&[0; 4096]).kind, RegionKind::Zero);
        let pad = classify_bytes(&[0xFF; 4096]);
        assert_eq!(pad.kind, RegionKind::Padding);
        assert_eq!(pad.detail.as_deref(), Some("0xFF"));
        let text = b"The quick brown fox jumps over the lazy dog.\n".repeat(100);
        let c = classify_bytes(&text);
        assert_eq!(
            (c.kind, c.detail.as_deref()),
            (RegionKind::Text, Some("ascii"))
        );
        let table = b"open\0close\0read\0write\0\0\0".repeat(150);
        let c = classify_bytes(&table);
        assert_eq!(
            (c.kind, c.detail.as_deref()),
            (RegionKind::Text, Some("ascii"))
        );
        let utf8 = "Съешь же ещё этих мягких французских булок. ".repeat(60);
        let c = classify_bytes(utf8.as_bytes());
        assert_eq!(
            (c.kind, c.detail.as_deref()),
            (RegionKind::Text, Some("utf-8"))
        );
        let utf16: Vec<u8> = "Hello wide world! "
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        let c = classify_bytes(&utf16.repeat(100));
        assert_eq!(
            (c.kind, c.detail.as_deref()),
            (RegionKind::Text, Some("utf-16le"))
        );
        let utf16be: Vec<u8> = "Big endian text. "
            .encode_utf16()
            .flat_map(u16::to_be_bytes)
            .collect();
        let c = classify_bytes(&utf16be.repeat(100));
        assert_eq!(
            (c.kind, c.detail.as_deref()),
            (RegionKind::Text, Some("utf-16be"))
        );

        let c = classify_bytes(&random_bytes(1, 4096));
        assert_eq!(c.kind, RegionKind::Encrypted, "{c:?}");
        let c = classify_bytes(&x86_code(4096));
        assert_eq!(
            (c.kind, c.detail.as_deref()),
            (RegionKind::Code, Some("x86"))
        );
        let c = classify_bytes(&arm64_code(4096));
        assert_eq!(
            (c.kind, c.detail.as_deref()),
            (RegionKind::Code, Some("arm64"))
        );
        // A table of small integers is plain data.
        let table: Vec<u8> = (0..4096u32)
            .flat_map(|i| (i * 7 % 1000).to_le_bytes())
            .collect();
        assert_eq!(classify_bytes(&table[..4096]).kind, RegionKind::Data);
    }

    #[test]
    fn compressed_is_not_uniform() {
        // Deflate output: high entropy but a skewed byte distribution.
        let text: Vec<u8> = random_bytes(3, 200_000)
            .iter()
            .map(|b| b" etaoinshrdlucmfwyp"[usize::from(b % 19)])
            .collect();
        let z = miniz_oxide::deflate::compress_to_vec(&text, 6);
        let c = classify_bytes(&z[20_000..24_096]);
        assert!(
            matches!(c.kind, RegionKind::Compressed | RegionKind::Encrypted),
            "{c:?}"
        );
        assert!(c.entropy > 7.5);
    }

    #[test]
    fn regions_are_merged() {
        let mut data = vec![0u8; 8192];
        data.extend(b"plain ascii text in a file. ".repeat(300));
        data.truncate(8192 + 8192);
        data.extend(random_bytes(2, 16384));
        data.extend(vec![0xFF; 5000]);
        let regions = classify(
            &tree(&data),
            0,
            data.len() as u64,
            4096,
            &Cancel::new(),
            &noop,
        )
        .unwrap();
        let kinds: Vec<RegionKind> = regions.iter().map(|r| r.kind).collect();
        assert_eq!(
            kinds,
            [
                RegionKind::Zero,
                RegionKind::Text,
                RegionKind::Encrypted,
                RegionKind::Padding
            ]
        );
        assert_eq!(regions[0].end, 8192);
        assert_eq!(regions[3].end, data.len() as u64);
        assert!(regions.iter().all(|r| r.confidence > 0.5));
        assert!(classify(&tree(&data), 0, 10, 8, &Cancel::new(), &noop).is_err());
    }
}
