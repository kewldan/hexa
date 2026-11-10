//! Fuzzy hashes (ANL-04): ssdeep-compatible CTPH and TLSH.
//!
//! * ssdeep: reproduces `libfuzzy` 2.14 (`fuzzy_hash_buf` with default flags,
//!   i.e. no sequence elimination and a truncated second part) and
//!   `fuzzy_compare` (score 0–100).
//! * TLSH: 128 buckets, 1-byte checksum, version prefix `T1` (the default
//!   TLSH 4.x configuration) and the reference `totalDiff` distance.
//!
//! Both are also available through [`crate::hash`] as ids `ssdeep` / `tlsh`.

use hexa_core::Result;

use crate::util::invalid;

// ------------------------------------------------------------------ ssdeep

const ROLLING_WINDOW: usize = 7;
const MIN_BLOCKSIZE: u64 = 3;
const HASH_PRIME: u32 = 0x0100_0193;
const HASH_INIT: u32 = 0x2802_1967;
const NUM_BLOCKHASHES: usize = 31;
const SPAMSUM_LENGTH: usize = 64;
const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn block_size(i: usize) -> u64 {
    MIN_BLOCKSIZE << i
}

fn sum_hash(c: u8, h: u32) -> u32 {
    h.wrapping_mul(HASH_PRIME) ^ u32::from(c)
}

#[derive(Debug, Clone)]
struct BlockHash {
    digest: Vec<u8>,
    h: u32,
    half_h: u32,
}

impl BlockHash {
    fn new(h: u32, half_h: u32) -> Self {
        Self {
            digest: Vec::with_capacity(SPAMSUM_LENGTH),
            h,
            half_h,
        }
    }
}

/// Streaming ssdeep generator. The total input length must be known up
/// front (as with `fuzzy_set_total_input_length`), which the tree-based
/// callers always know.
#[derive(Debug, Clone)]
pub struct SsdeepHasher {
    bh: Vec<BlockHash>,
    bh_start: usize,
    total: u64,
    window: [u8; ROLLING_WINDOW],
    h1: u32,
    h2: u32,
    h3: u32,
    n: usize,
}

impl SsdeepHasher {
    pub fn new(total_len: u64) -> Self {
        Self {
            bh: vec![BlockHash::new(HASH_INIT, HASH_INIT)],
            bh_start: 0,
            total: total_len,
            window: [0; ROLLING_WINDOW],
            h1: 0,
            h2: 0,
            h3: 0,
            n: 0,
        }
    }

    fn roll(&mut self, c: u8) -> u32 {
        let c32 = u32::from(c);
        self.h2 = self
            .h2
            .wrapping_sub(self.h1)
            .wrapping_add(ROLLING_WINDOW as u32 * c32);
        self.h1 = self
            .h1
            .wrapping_add(c32)
            .wrapping_sub(u32::from(self.window[self.n % ROLLING_WINDOW]));
        self.window[self.n % ROLLING_WINDOW] = c;
        self.n = self.n.wrapping_add(1);
        self.h3 = (self.h3 << 5) ^ c32;
        self.roll_sum()
    }

    fn roll_sum(&self) -> u32 {
        self.h1.wrapping_add(self.h2).wrapping_add(self.h3)
    }

    fn try_fork(&mut self) {
        if self.bh.len() >= NUM_BLOCKHASHES {
            return;
        }
        let last = self.bh.last().expect("at least one block hash");
        let next = BlockHash::new(last.h, last.half_h);
        self.bh.push(next);
    }

    fn try_reduce(&mut self) {
        if self.bh.len() - self.bh_start < 2 {
            return;
        }
        if block_size(self.bh_start) * SPAMSUM_LENGTH as u64 >= self.total {
            return;
        }
        if self.bh[self.bh_start + 1].digest.len() < SPAMSUM_LENGTH / 2 {
            return;
        }
        self.bh_start += 1;
    }

    fn step(&mut self, c: u8) {
        let h = u64::from(self.roll(c));
        for b in &mut self.bh[self.bh_start..] {
            b.h = sum_hash(c, b.h);
            b.half_h = sum_hash(c, b.half_h);
        }
        let mut i = self.bh_start;
        while i < self.bh.len() {
            let bs = block_size(i);
            if h % bs != bs - 1 {
                break;
            }
            if self.bh[i].digest.is_empty() {
                self.try_fork();
            }
            let b = &mut self.bh[i];
            if b.digest.len() < SPAMSUM_LENGTH - 1 {
                b.digest.push(B64[(b.h % 64) as usize]);
                b.h = HASH_INIT;
                if b.digest.len() < SPAMSUM_LENGTH / 2 {
                    b.half_h = HASH_INIT;
                }
            } else {
                self.try_reduce();
            }
            i += 1;
        }
    }

    pub fn update(&mut self, data: &[u8]) {
        for &c in data {
            self.step(c);
        }
    }

    /// The `blocksize:hash1:hash2` digest.
    pub fn digest(&self) -> String {
        let h = self.roll_sum();
        let mut bi = self.bh_start;
        while block_size(bi) * (SPAMSUM_LENGTH as u64) < self.total && bi < NUM_BLOCKHASHES - 1 {
            bi += 1;
        }
        while bi >= self.bh.len() {
            bi -= 1;
        }
        while bi > self.bh_start && self.bh[bi].digest.len() < SPAMSUM_LENGTH / 2 {
            bi -= 1;
        }
        let mut out = format!("{}:", block_size(bi));
        let b = &self.bh[bi];
        out.extend(b.digest.iter().map(|&c| c as char));
        if h != 0 {
            out.push(B64[(b.h % 64) as usize] as char);
        }
        out.push(':');
        if bi < self.bh.len() - 1 {
            let b2 = &self.bh[bi + 1];
            let n = b2.digest.len().min(SPAMSUM_LENGTH / 2 - 1);
            out.extend(b2.digest[..n].iter().map(|&c| c as char));
            if h != 0 {
                out.push(B64[(b2.half_h % 64) as usize] as char);
            }
        } else if h != 0 {
            let v = if bi == 0 { b.h } else { b.half_h };
            out.push(B64[(v % 64) as usize] as char);
        }
        out
    }
}

/// ssdeep digest of a buffer.
pub fn ssdeep(data: &[u8]) -> String {
    let mut h = SsdeepHasher::new(data.len() as u64);
    h.update(data);
    h.digest()
}

struct ParsedSsdeep {
    block_size: u64,
    s1: Vec<u8>,
    s2: Vec<u8>,
}

/// Collapses runs of more than three identical characters to three.
fn eliminate_sequences(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for (i, &c) in s.iter().enumerate() {
        if i < 3 || c != s[i - 1] || c != s[i - 2] || c != s[i - 3] {
            out.push(c);
        }
    }
    out
}

fn parse_ssdeep(s: &str) -> Result<ParsedSsdeep> {
    let s = s.trim();
    let mut parts = s.splitn(3, ':');
    let bs = parts.next().unwrap_or_default();
    let block_size: u64 = bs
        .parse()
        .map_err(|_| invalid(format!("bad ssdeep block size {bs:?}")))?;
    let s1 = parts
        .next()
        .ok_or_else(|| invalid("ssdeep hash needs two ':' separators"))?;
    let rest = parts
        .next()
        .ok_or_else(|| invalid("ssdeep hash needs two ':' separators"))?;
    // The second part may be followed by `,"filename"`.
    let s2 = rest.split(',').next().unwrap_or_default();
    let valid = |p: &str| p.bytes().all(|c| B64.contains(&c)) && p.len() <= SPAMSUM_LENGTH;
    if !valid(s1) || !valid(s2) {
        return Err(invalid("ssdeep hash contains invalid characters"));
    }
    Ok(ParsedSsdeep {
        block_size,
        s1: eliminate_sequences(s1.as_bytes()),
        s2: eliminate_sequences(s2.as_bytes()),
    })
}

fn has_common_substring(a: &[u8], b: &[u8]) -> bool {
    if a.len() < ROLLING_WINDOW || b.len() < ROLLING_WINDOW {
        return false;
    }
    a.windows(ROLLING_WINDOW)
        .any(|w| b.windows(ROLLING_WINDOW).any(|v| v == w))
}

/// Edit distance with insert/remove cost 1 and replace cost 2 (`edit_distn`).
fn edit_distance(a: &[u8], b: &[u8]) -> u32 {
    let mut prev: Vec<u32> = (0..=b.len() as u32).collect();
    let mut cur = vec![0u32; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        cur[0] = i as u32 + 1;
        for (j, &cb) in b.iter().enumerate() {
            let replace = prev[j] + if ca == cb { 0 } else { 2 };
            cur[j + 1] = replace.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

fn score_strings(a: &[u8], b: &[u8], block_size: u64) -> u32 {
    if !has_common_substring(a, b) {
        return 0;
    }
    let total = (a.len() + b.len()) as u64;
    let mut score = u64::from(edit_distance(a, b)) * SPAMSUM_LENGTH as u64 / total;
    score = 100 * score / SPAMSUM_LENGTH as u64;
    if score >= 100 {
        return 0;
    }
    score = 100 - score;
    let threshold = (99 + ROLLING_WINDOW as u64) / ROLLING_WINDOW as u64 * MIN_BLOCKSIZE;
    if block_size >= threshold {
        return score as u32;
    }
    let cap = block_size / MIN_BLOCKSIZE * a.len().min(b.len()) as u64;
    score.min(cap) as u32
}

/// Similarity of two ssdeep digests, 0 (unrelated) … 100 (identical),
/// matching `fuzzy_compare`.
pub fn ssdeep_compare(a: &str, b: &str) -> Result<u32> {
    let x = parse_ssdeep(a)?;
    let y = parse_ssdeep(b)?;
    let (bs1, bs2) = (x.block_size, y.block_size);
    if bs1 != bs2 && Some(bs1) != bs2.checked_mul(2) && Some(bs2) != bs1.checked_mul(2) {
        return Ok(0);
    }
    if bs1 == bs2 && x.s1 == y.s1 && x.s2 == y.s2 {
        return Ok(100);
    }
    Ok(if bs1 == bs2 {
        let s1 = score_strings(&x.s1, &y.s1, bs1);
        let s2 = score_strings(&x.s2, &y.s2, bs1.saturating_mul(2));
        s1.max(s2)
    } else if Some(bs1) == bs2.checked_mul(2) {
        score_strings(&x.s1, &y.s2, bs1)
    } else {
        score_strings(&x.s2, &y.s1, bs2)
    })
}

// -------------------------------------------------------------------- TLSH

/// Pearson substitution table of the TLSH reference implementation.
#[rustfmt::skip]
const V_TABLE: [u8; 256] = [
    0x01, 0x57, 0x31, 0x0c, 0xb0, 0xb2, 0x66, 0xa6, 0x79, 0xc1, 0x06, 0x54, 0xf9, 0xe6, 0x2c, 0xa3,
    0x0e, 0xc5, 0xd5, 0xb5, 0xa1, 0x55, 0xda, 0x50, 0x40, 0xef, 0x18, 0xe2, 0xec, 0x8e, 0x26, 0xc8,
    0x6e, 0xb1, 0x68, 0x67, 0x8d, 0xfd, 0xff, 0x32, 0x4d, 0x65, 0x51, 0x12, 0x2d, 0x60, 0x1f, 0xde,
    0x19, 0x6b, 0xbe, 0x46, 0x56, 0xed, 0xf0, 0x22, 0x48, 0xf2, 0x14, 0xd6, 0xf4, 0xe3, 0x95, 0xeb,
    0x61, 0xea, 0x39, 0x16, 0x3c, 0xfa, 0x52, 0xaf, 0xd0, 0x05, 0x7f, 0xc7, 0x6f, 0x3e, 0x87, 0xf8,
    0xae, 0xa9, 0xd3, 0x3a, 0x42, 0x9a, 0x6a, 0xc3, 0xf5, 0xab, 0x11, 0xbb, 0xb6, 0xb3, 0x00, 0xf3,
    0x84, 0x38, 0x94, 0x4b, 0x80, 0x85, 0x9e, 0x64, 0x82, 0x7e, 0x5b, 0x0d, 0x99, 0xf6, 0xd8, 0xdb,
    0x77, 0x44, 0xdf, 0x4e, 0x53, 0x58, 0xc9, 0x63, 0x7a, 0x0b, 0x5c, 0x20, 0x88, 0x72, 0x34, 0x0a,
    0x8a, 0x1e, 0x30, 0xb7, 0x9c, 0x23, 0x3d, 0x1a, 0x8f, 0x4a, 0xfb, 0x5e, 0x81, 0xa2, 0x3f, 0x98,
    0xaa, 0x07, 0x73, 0xa7, 0xf1, 0xce, 0x03, 0x96, 0x37, 0x3b, 0x97, 0xdc, 0x5a, 0x35, 0x17, 0x83,
    0x7d, 0xad, 0x0f, 0xee, 0x4f, 0x5f, 0x59, 0x10, 0x69, 0x89, 0xe1, 0xe0, 0xd9, 0xa0, 0x25, 0x7b,
    0x76, 0x49, 0x02, 0x9d, 0x2e, 0x74, 0x09, 0x91, 0x86, 0xe4, 0xcf, 0xd4, 0xca, 0xd7, 0x45, 0xe5,
    0x1b, 0xbc, 0x43, 0x7c, 0xa8, 0xfc, 0x2a, 0x04, 0x1d, 0x6c, 0x15, 0xf7, 0x13, 0xcd, 0x27, 0xcb,
    0xe9, 0x28, 0xba, 0x93, 0xc6, 0xc0, 0x9b, 0x21, 0xa4, 0xbf, 0x62, 0xcc, 0xa5, 0xb4, 0x75, 0x4c,
    0x8c, 0x24, 0xd2, 0xac, 0x29, 0x36, 0x9f, 0x08, 0xb9, 0xe8, 0x71, 0xc4, 0xe7, 0x2f, 0x92, 0x78,
    0x33, 0x41, 0x1c, 0x90, 0xfe, 0xdd, 0x5d, 0xbd, 0xc2, 0x8b, 0x70, 0x2b, 0x47, 0x6d, 0xb8, 0xd1,
];

/// Upper bounds of the TLSH length encoding (`topval` of the reference).
#[allow(clippy::unreadable_literal)]
#[rustfmt::skip]
const TOP_VALUE: [u32; 170] = [
    1, 2, 3, 5, 7, 11, 17, 25,
    38, 57, 86, 129, 194, 291, 437, 656,
    854, 1110, 1443, 1876, 2439, 3171, 3475, 3823,
    4205, 4626, 5088, 5597, 6157, 6772, 7450, 8195,
    9014, 9916, 10907, 11998, 13198, 14518, 15970, 17567,
    19323, 21256, 23382, 25720, 28292, 31121, 34233, 37656,
    41422, 45564, 50121, 55133, 60646, 66711, 73382, 80721,
    88793, 97672, 107439, 118183, 130002, 143002, 157302, 173032,
    190335, 209369, 230306, 253337, 278670, 306538, 337191, 370911,
    408002, 448802, 493682, 543050, 597356, 657091, 722800, 795081,
    874589, 962048, 1058252, 1164078, 1280486, 1408534, 1549388, 1704327,
    1874759, 2062236, 2268459, 2495305, 2744836, 3019320, 3321252, 3653374,
    4018711, 4420582, 4862641, 5348905, 5883796, 6472176, 7119394, 7831333,
    8614467, 9475909, 10423501, 11465851, 12612437, 13873681, 15261050, 16787154,
    18465870, 20312458, 22343706, 24578077, 27035886, 29739474, 32713425, 35984770,
    39583245, 43541573, 47895730, 52685306, 57953837, 63749221, 70124148, 77136564,
    84850228, 93335252, 102668779, 112935659, 124229227, 136652151, 150317384, 165349128,
    181884040, 200072456, 220079703, 242087671, 266296456, 292926096, 322218735, 354440623,
    389884688, 428873168, 471760495, 518936559, 570830240, 627913311, 690704607, 759775136,
    835752671, 919327967, 1011260767, 1112386880, 1223623232, 1345985727, 1480584256, 1628642751,
    1791507135, 1970657856, 2167723648, 2384496256, 2622945920, 2885240448, 3173764736, 3491141248,
    3840255616, 4224281216,
];

const TLSH_MIN_LEN: u64 = 50;
const BUCKETS: usize = 128;
const CODE_SIZE: usize = BUCKETS / 4;

fn b_mapping(salt: u8, i: u8, j: u8, k: u8) -> u8 {
    let mut h = V_TABLE[usize::from(salt)];
    h = V_TABLE[usize::from(h ^ i)];
    h = V_TABLE[usize::from(h ^ j)];
    V_TABLE[usize::from(h ^ k)]
}

fn swap_nibbles(b: u8) -> u8 {
    b.rotate_left(4)
}

fn l_capturing(len: u64) -> u8 {
    let len = u32::try_from(len).unwrap_or(u32::MAX);
    TOP_VALUE.partition_point(|&t| t < len) as u8
}

/// Streaming TLSH generator (128 buckets, 1-byte checksum).
#[derive(Debug, Clone)]
pub struct TlshHasher {
    window: [u8; 5],
    fed: u64,
    checksum: u8,
    buckets: [u32; 256],
}

impl Default for TlshHasher {
    fn default() -> Self {
        Self {
            window: [0; 5],
            fed: 0,
            checksum: 0,
            buckets: [0; 256],
        }
    }
}

impl TlshHasher {
    pub fn update(&mut self, data: &[u8]) {
        for &c in data {
            let j = (self.fed % 5) as usize;
            self.window[j] = c;
            if self.fed >= 4 {
                let w = &self.window;
                let a = w[j];
                let b1 = w[(j + 4) % 5];
                let b2 = w[(j + 3) % 5];
                let b3 = w[(j + 2) % 5];
                let b4 = w[(j + 1) % 5];
                self.checksum = b_mapping(0, a, b1, self.checksum);
                for r in [
                    b_mapping(2, a, b1, b2),
                    b_mapping(3, a, b1, b3),
                    b_mapping(5, a, b2, b3),
                    b_mapping(7, a, b2, b4),
                    b_mapping(11, a, b1, b4),
                    b_mapping(13, a, b3, b4),
                ] {
                    self.buckets[usize::from(r)] += 1;
                }
            }
            self.fed += 1;
        }
    }

    /// The `T1…` digest, or `None` when the input is shorter than 50 bytes
    /// or too uniform (the reference prints `TNULL`).
    pub fn digest(&self) -> Option<String> {
        if self.fed < TLSH_MIN_LEN {
            return None;
        }
        let mut sorted: Vec<u32> = self.buckets[..BUCKETS].to_vec();
        sorted.sort_unstable();
        let q1 = sorted[BUCKETS / 4 - 1];
        let q2 = sorted[BUCKETS / 2 - 1];
        let q3 = sorted[BUCKETS * 3 / 4 - 1];
        let nonzero = self.buckets[..BUCKETS].iter().filter(|&&c| c > 0).count();
        if nonzero <= BUCKETS / 2 || q3 == 0 {
            return None;
        }
        let mut code = [0u8; CODE_SIZE];
        for (i, slot) in code.iter_mut().enumerate() {
            let mut h = 0u8;
            for j in 0..4 {
                let k = self.buckets[4 * i + j];
                let v = if q3 < k {
                    3
                } else if q2 < k {
                    2
                } else {
                    u8::from(q1 < k)
                };
                h |= v << (j * 2);
            }
            *slot = h;
        }
        let ratio = |q: u32| ((q.wrapping_mul(100) as f32 / q3 as f32) as u32 % 16) as u8;
        let q_byte = ratio(q1) | (ratio(q2) << 4);
        let mut bytes = Vec::with_capacity(3 + CODE_SIZE);
        bytes.push(swap_nibbles(self.checksum));
        bytes.push(swap_nibbles(l_capturing(self.fed)));
        bytes.push(swap_nibbles(q_byte));
        bytes.extend(code.iter().rev());
        let mut s = String::from("T1");
        s.push_str(&crate::util::hex_lower(&bytes).to_ascii_uppercase());
        Some(s)
    }
}

/// TLSH digest of a buffer.
pub fn tlsh(data: &[u8]) -> Option<String> {
    let mut h = TlshHasher::default();
    h.update(data);
    h.digest()
}

struct ParsedTlsh {
    checksum: u8,
    l: u8,
    q1: u8,
    q2: u8,
    code: [u8; CODE_SIZE],
}

fn parse_tlsh(s: &str) -> Result<ParsedTlsh> {
    let s = s.trim();
    let hex = s
        .strip_prefix("T1")
        .or_else(|| s.strip_prefix("t1"))
        .unwrap_or(s);
    if hex.len() != 2 * (3 + CODE_SIZE) {
        return Err(invalid(format!(
            "TLSH digest must have {} hex digits",
            2 * (3 + CODE_SIZE)
        )));
    }
    let mut bytes = [0u8; 3 + CODE_SIZE];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16)
            .map_err(|_| invalid("TLSH digest is not hex"))?;
    }
    let q = swap_nibbles(bytes[2]);
    let mut code = [0u8; CODE_SIZE];
    code.copy_from_slice(&bytes[3..]);
    Ok(ParsedTlsh {
        checksum: swap_nibbles(bytes[0]),
        l: swap_nibbles(bytes[1]),
        q1: q & 0x0F,
        q2: q >> 4,
        code,
    })
}

fn mod_diff(x: u32, y: u32, range: u32) -> u32 {
    let (dl, dr) = if y > x {
        (y - x, x + range - y)
    } else {
        (x - y, y + range - x)
    };
    dl.min(dr)
}

/// TLSH distance (`totalDiff`): 0 for identical inputs, growing with
/// dissimilarity (typically < 100 for related files).
pub fn tlsh_diff(a: &str, b: &str, include_length: bool) -> Result<u32> {
    let x = parse_tlsh(a)?;
    let y = parse_tlsh(b)?;
    let mut diff = 0;
    if include_length {
        let d = mod_diff(u32::from(x.l), u32::from(y.l), 256);
        diff += if d <= 1 { d } else { d * 12 };
    }
    for (p, q) in [(x.q1, y.q1), (x.q2, y.q2)] {
        let d = mod_diff(u32::from(p), u32::from(q), 16);
        diff += if d <= 1 { d } else { (d - 1) * 12 };
    }
    if x.checksum != y.checksum {
        diff += 1;
    }
    for (&p, &q) in x.code.iter().zip(&y.code) {
        for k in 0..4 {
            let d = ((p >> (2 * k)) & 3).abs_diff((q >> (2 * k)) & 3);
            diff += if d == 3 { 6 } else { u32::from(d) };
        }
    }
    Ok(diff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::random_bytes;

    #[test]
    fn ssdeep_known_answers() {
        // `data/examples/hello.txt` of the ffuzzy test suite.
        assert_eq!(ssdeep(b"Hello, World!\n"), "3:aaX8v:aV");
        // python-ssdeep README pair (hashes and score of libfuzzy).
        let h1 = "3:AXGBicFlgVNhBGcL6wCrFQEv:AXGHsNhxLsr2C";
        let h2 = "3:AXGBicFlIHBGcL6wCrFQEv:AXGH6xLsr2C";
        assert_eq!(ssdeep_compare(h1, h2).unwrap(), 22);
        assert_eq!(ssdeep_compare(h1, h1).unwrap(), 100);
        assert_eq!(ssdeep(b""), "3::");
        assert_eq!(
            ssdeep_compare(
                "6:3ll7QzDkmJmMHkQoO/llSZEnEuLszmbMAWn:VqDk5QtLbW",
                "6:3ll7QzDkmQjmMoDHglHOxPWT0lT0lT0lB:VqDk+n"
            )
            .unwrap(),
            46
        );
    }

    #[test]
    fn ssdeep_matches_independent_implementation() {
        for (seed, len) in [(1, 10), (2, 300), (3, 5_000), (4, 70_000), (5, 1_000_000)] {
            let mut data = random_bytes(seed, len);
            // Mix in some structure so block sizes vary.
            for (i, b) in data.iter_mut().enumerate().step_by(3) {
                *b = (i % 7) as u8;
            }
            let ours = ssdeep(&data);
            let theirs = ssdeep::hash_buf(&data).unwrap().to_string();
            assert_eq!(ours, theirs, "len {len}");
            let mut stream = SsdeepHasher::new(len as u64);
            for c in data.chunks(4093) {
                stream.update(c);
            }
            assert_eq!(stream.digest(), ours);
        }
        // Compare scores agree on related inputs.
        let base = random_bytes(9, 20_000);
        let mut edited = base.clone();
        edited[5_000..5_400].fill(0x41);
        edited.splice(12_000..12_000, random_bytes(10, 777));
        let (a, b) = (ssdeep(&base), ssdeep(&edited));
        let theirs = ssdeep::compare(&a, &b).unwrap();
        assert_eq!(ssdeep_compare(&a, &b).unwrap(), theirs);
        assert!(theirs > 0);
    }

    #[test]
    fn ssdeep_compare_rejects_garbage_and_mismatched_sizes() {
        assert!(ssdeep_compare("x:a:b", "3:a:b").is_err());
        assert!(ssdeep_compare("3:ab", "3:a:b").is_err());
        assert_eq!(
            ssdeep_compare("3:ABCDEFGH:AB", "24:ABCDEFGH:AB").unwrap(),
            0
        );
        assert_eq!(eliminate_sequences(b"aaaaabbbc"), b"aaabbbc");
    }

    #[test]
    fn tlsh_matches_independent_implementation() {
        for (seed, len) in [(1, 50), (2, 300), (3, 5_000), (4, 100_000)] {
            let data: Vec<u8> = random_bytes(seed, len)
                .iter()
                .enumerate()
                .map(|(i, &b)| if i % 4 == 0 { b'a' + (i % 13) as u8 } else { b })
                .collect();
            let ours = tlsh(&data).unwrap();
            let theirs = tlsh::hash_buf(&data).unwrap().to_string();
            assert_eq!(ours, theirs, "len {len}");
            assert_eq!(ours.len(), 72);
            assert!(ours.starts_with("T1"));
        }
        let a = random_bytes(5, 10_000);
        let mut b = a.clone();
        b[100..900].fill(7);
        let (ha, hb) = (tlsh(&a).unwrap(), tlsh(&b).unwrap());
        let theirs = tlsh::compare(&ha, &hb).unwrap();
        assert_eq!(tlsh_diff(&ha, &hb, true).unwrap(), theirs);
        assert_eq!(tlsh_diff(&ha, &ha, true).unwrap(), 0);
        assert!(tlsh_diff(&ha, &hb, false).unwrap() <= theirs);
    }

    #[test]
    fn tlsh_rejects_short_or_uniform_input() {
        assert_eq!(tlsh(&[1; 49]), None);
        assert_eq!(tlsh(&[0; 4096]), None);
        assert!(tlsh_diff("T1AB", "T1AB", true).is_err());
        assert_eq!(l_capturing(1), 0);
        assert_eq!(l_capturing(656), 15);
        assert_eq!(l_capturing(657), 16);
    }
}
