//! Repeating-key XOR and key recovery (XFRM-03).
//!
//! Recovery ranks candidates by two heuristics:
//! * **text** — the plaintext looks like English / printable ASCII
//!   (printable ratio + Bhattacharyya similarity of the letter histogram to
//!   English);
//! * **zero bytes** — binary data is usually full of `00` bytes, so the most
//!   frequent ciphertext byte of a column is likely the key byte.
//!
//! Repeating keys are found by estimating the key size with the normalised
//! Hamming distance between consecutive key-size blocks (sizes 1..=40), then
//! solving every column as a single-byte XOR.

use serde::{Deserialize, Serialize};

use crate::error::{Result, TransformError};
use crate::types::HexBytes;

/// Longest key size considered by [`guess_repeating_xor`].
pub const MAX_GUESS_KEY_LEN: usize = 40;

/// XORs `data` with `key` repeated from offset 0.
pub fn xor_key(data: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    if key.is_empty() {
        return Err(TransformError::param("XOR key must not be empty"));
    }
    Ok(data
        .iter()
        .zip(key.iter().cycle())
        .map(|(d, k)| d ^ k)
        .collect())
}

/// Which heuristic produced a key candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GuessMethod {
    /// Plaintext scores as English / printable text.
    Text,
    /// Key bytes are the most frequent ciphertext bytes (plaintext zeros).
    ZeroBytes,
}

/// A ranked key guess.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XorKeyCandidate {
    /// The key (hex string in JSON).
    pub key: HexBytes,
    /// Confidence in `0.0..=1.0`, higher is better.
    pub score: f64,
    /// The heuristic that produced the score.
    pub method: GuessMethod,
    /// Fraction of printable ASCII bytes in the decrypted data.
    pub printable_ratio: f64,
}

/// Normalised Hamming distance for one key size.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeySizeScore {
    pub key_size: usize,
    /// Average differing bits per byte between consecutive blocks; English
    /// `XORed` with the right key size is about 2–3, random data about 4.
    pub distance: f64,
}

/// English letter frequencies (a..z) and space, per character.
const ENGLISH: [f64; 27] = [
    0.0651, 0.0127, 0.0217, 0.0350, 0.1041, 0.0198, 0.0158, 0.0493, 0.0558, 0.0010, 0.0057, 0.0331,
    0.0199, 0.0564, 0.0596, 0.0137, 0.0009, 0.0497, 0.0515, 0.0729, 0.0225, 0.0080, 0.0171, 0.0014,
    0.0146, 0.0006, 0.1918,
];
/// Expected share of other printable characters (digits, punctuation, CR/LF).
const ENGLISH_OTHER: f64 = 0.03;

fn is_printable(b: u8) -> bool {
    (0x20..0x7F).contains(&b) || matches!(b, b'\t' | b'\n' | b'\r')
}

/// Scores a byte histogram as English text; returns `(score, printable)`.
fn text_score_hist(hist: &[usize; 256], total: usize) -> (f64, f64) {
    if total == 0 {
        return (0.0, 0.0);
    }
    let n = total as f64;
    let mut classes = [0usize; 27];
    let mut other_printable = 0usize;
    let mut printable = 0usize;
    for (b, &c) in hist.iter().enumerate() {
        if c == 0 {
            continue;
        }
        let b = b as u8;
        if is_printable(b) {
            printable += c;
        }
        match b {
            b'a'..=b'z' => classes[usize::from(b - b'a')] += c,
            b'A'..=b'Z' => classes[usize::from(b - b'A')] += c,
            b' ' => classes[26] += c,
            _ if is_printable(b) => other_printable += c,
            _ => {}
        }
    }
    let total_q: f64 = ENGLISH.iter().sum::<f64>() + ENGLISH_OTHER;
    let mut bc = 0.0;
    for (i, &c) in classes.iter().enumerate() {
        bc += (c as f64 / n * ENGLISH[i] / total_q).sqrt();
    }
    bc += (other_printable as f64 / n * ENGLISH_OTHER / total_q).sqrt();
    let printable_ratio = printable as f64 / n;
    (0.3 * printable_ratio + 0.7 * bc.min(1.0), printable_ratio)
}

fn histogram(data: impl IntoIterator<Item = u8>) -> ([usize; 256], usize) {
    let mut hist = [0usize; 256];
    let mut n = 0;
    for b in data {
        hist[usize::from(b)] += 1;
        n += 1;
    }
    (hist, n)
}

fn xored_hist(hist: &[usize; 256], key: u8) -> [usize; 256] {
    let mut out = [0usize; 256];
    for (b, &c) in hist.iter().enumerate() {
        out[b ^ usize::from(key)] = c;
    }
    out
}

/// Best single-byte key for a byte histogram (`(key, score, method)`).
fn best_single(hist: &[usize; 256], total: usize) -> (u8, f64, GuessMethod) {
    let mut best = (0u8, -1.0, GuessMethod::Text);
    for k in 0..=255u8 {
        let (s, _) = text_score_hist(&xored_hist(hist, k), total);
        if s > best.1 {
            best = (k, s, GuessMethod::Text);
        }
    }
    let (zk, zc) = hist
        .iter()
        .enumerate()
        .max_by_key(|&(b, &c)| (c, std::cmp::Reverse(b)))
        .map_or((0, 0), |(b, &c)| (b as u8, c));
    let zs = zc as f64 / total.max(1) as f64;
    if zs > best.1 {
        best = (zk, zs, GuessMethod::ZeroBytes);
    }
    best
}

fn score_plain(plain: &[u8]) -> (f64, GuessMethod, f64) {
    let (hist, n) = histogram(plain.iter().copied());
    let (ts, printable) = text_score_hist(&hist, n);
    let zs = hist[0] as f64 / n.max(1) as f64;
    if zs > ts {
        (zs, GuessMethod::ZeroBytes, printable)
    } else {
        (ts, GuessMethod::Text, printable)
    }
}

/// Ranks single-byte XOR keys (best first, at most `max_candidates`).
pub fn guess_single_byte_xor(data: &[u8], max_candidates: usize) -> Vec<XorKeyCandidate> {
    if data.is_empty() || max_candidates == 0 {
        return Vec::new();
    }
    let (hist, n) = histogram(data.iter().copied());
    let mut out: Vec<XorKeyCandidate> = (0..=255u8)
        .map(|k| {
            let h = xored_hist(&hist, k);
            let (ts, printable) = text_score_hist(&h, n);
            let zs = h[0] as f64 / n as f64;
            let (score, method) = if zs > ts {
                (zs, GuessMethod::ZeroBytes)
            } else {
                (ts, GuessMethod::Text)
            };
            XorKeyCandidate {
                key: HexBytes(vec![k]),
                score,
                method,
                printable_ratio: printable,
            }
        })
        .collect();
    out.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.key.0.cmp(&b.key.0)));
    out.truncate(max_candidates);
    out
}

/// Normalised Hamming distance for key sizes `1..=max_key_len` (capped at
/// [`MAX_GUESS_KEY_LEN`] and half the data length), smallest distance first.
pub fn estimate_key_sizes(data: &[u8], max_key_len: usize) -> Vec<KeySizeScore> {
    let max = max_key_len.min(MAX_GUESS_KEY_LEN).min(data.len() / 2);
    let mut out: Vec<KeySizeScore> = (1..=max)
        .map(|k| {
            let blocks = (data.len() / k).min(1 + 8192 / k).max(2);
            let mut bits = 0u64;
            let mut pairs = 0u64;
            for i in 0..blocks - 1 {
                let a = &data[i * k..(i + 1) * k];
                let b = &data[(i + 1) * k..(i + 2) * k];
                bits += a
                    .iter()
                    .zip(b)
                    .map(|(x, y)| u64::from((x ^ y).count_ones()))
                    .sum::<u64>();
                pairs += 1;
            }
            KeySizeScore {
                key_size: k,
                distance: bits as f64 / (pairs * k as u64) as f64,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        a.distance
            .total_cmp(&b.distance)
            .then(a.key_size.cmp(&b.key_size))
    });
    out
}

/// Shortest period of `key` (e.g. `ICEICE` -> `ICE`).
fn minimal_period(key: &[u8]) -> &[u8] {
    let n = key.len();
    for p in 1..n {
        if n.is_multiple_of(p) && key.chunks(p).all(|c| c == &key[..p]) {
            return &key[..p];
        }
    }
    key
}

/// Ranks repeating XOR keys of length `1..=max_key_len` (at most 40).
/// The best few key sizes by Hamming distance are solved column by column.
pub fn guess_repeating_xor(
    data: &[u8],
    max_key_len: usize,
    max_candidates: usize,
) -> Vec<XorKeyCandidate> {
    if data.len() < 2 || max_candidates == 0 {
        return guess_single_byte_xor(data, max_candidates);
    }
    let sizes = estimate_key_sizes(data, max_key_len);
    let mut out: Vec<XorKeyCandidate> = Vec::new();
    for ks in sizes.iter().take(max_candidates.max(6)) {
        let k = ks.key_size;
        let key: Vec<u8> = (0..k)
            .map(|col| {
                let (hist, n) = histogram(data.iter().skip(col).step_by(k).copied());
                best_single(&hist, n).0
            })
            .collect();
        let key = minimal_period(&key).to_vec();
        if out.iter().any(|c| c.key.0 == key) {
            continue;
        }
        let plain: Vec<u8> = data
            .iter()
            .zip(key.iter().cycle())
            .map(|(d, k)| d ^ k)
            .collect();
        let (score, method, printable) = score_plain(&plain);
        out.push(XorKeyCandidate {
            key: HexBytes(key),
            score,
            method,
            printable_ratio: printable,
        });
    }
    out.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.key.0.len().cmp(&b.key.0.len()))
    });
    out.truncate(max_candidates);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "It was a bright cold day in April, and the clocks were \
        striking thirteen. The hex editor opened the file and showed every byte in \
        neat columns, offsets on the left and characters on the right. A careful \
        reader can learn a great deal about a format just by looking at the patterns \
        that repeat: headers with magic numbers, tables of offsets that grow by a \
        fixed step, long runs of zeros used as padding, and strings of readable text \
        hidden between compressed blocks. When the data has been obscured with a \
        simple repeating key, the same patterns still leak through, because every \
        column of the key acts like a separate substitution cipher with a single \
        byte. Counting letters in each column and comparing the counts with the \
        usual frequencies of English quickly reveals which key byte was used, and \
        the whole message falls apart within a few milliseconds of work on any \
        modern machine. This is why nobody should rely on such a scheme to protect \
        anything of value, even though it remains popular in malware and in old \
        games that wanted to keep their dialogue away from curious players.";

    #[test]
    fn xor_key_basic() {
        assert_eq!(
            xor_key(b"\x00\x01\x02", &[0xFF, 0x0F]).unwrap(),
            [0xFF, 0x0E, 0xFD]
        );
        assert!(xor_key(b"abc", &[]).is_err());
    }

    #[test]
    fn cryptopals_single_byte() {
        let ct =
            HexBytes::parse("1b37373331363f78151b7f2b783431333d78397828372d363c78373e783a393b3736")
                .unwrap();
        let c = guess_single_byte_xor(&ct.0, 5);
        assert_eq!(c[0].key.0, b"X");
        assert_eq!(c[0].method, GuessMethod::Text);
        assert_eq!(
            xor_key(&ct.0, &c[0].key.0).unwrap(),
            b"Cooking MC's like a pound of bacon"
        );
        assert_eq!(c.len(), 5);
        assert!(c.windows(2).all(|w| w[0].score >= w[1].score));
    }

    #[test]
    fn zero_byte_heuristic() {
        let mut plain = vec![0u8; 400];
        for (i, b) in plain.iter_mut().enumerate().step_by(7) {
            *b = (i * 37 % 251) as u8 | 0x80;
        }
        let ct = xor_key(&plain, &[0xA5]).unwrap();
        let c = guess_single_byte_xor(&ct, 3);
        assert_eq!(c[0].key.0, [0xA5]);
        assert_eq!(c[0].method, GuessMethod::ZeroBytes);
    }

    #[test]
    fn repeating_key_text() {
        for key in [&b"ICE"[..], b"Hexa!Key", b"SECRETKEY42", b"k"] {
            let ct = xor_key(TEXT.as_bytes(), key).unwrap();
            let c = guess_repeating_xor(&ct, 40, 5);
            assert_eq!(c[0].key.0, key, "key {:?}", String::from_utf8_lossy(key));
            assert_eq!(xor_key(&ct, &c[0].key.0).unwrap(), TEXT.as_bytes());
        }
    }

    #[test]
    fn repeating_key_binary() {
        let mut plain = vec![0u8; 4096];
        for (i, b) in plain.iter_mut().enumerate() {
            if i % 5 == 0 {
                *b = (i.wrapping_mul(2_654_435_761) >> 7) as u8;
            }
        }
        let key = [0xDE, 0xAD, 0xBE, 0xEF, 0x42, 0x13];
        let ct = xor_key(&plain, &key).unwrap();
        let c = guess_repeating_xor(&ct, 40, 3);
        assert_eq!(c[0].key.0, key);
        assert_eq!(c[0].method, GuessMethod::ZeroBytes);
    }

    #[test]
    fn key_size_estimation() {
        let ct = xor_key(TEXT.as_bytes(), b"Hexa!Key").unwrap();
        let sizes = estimate_key_sizes(&ct, 40);
        assert_eq!(sizes.len(), 40);
        assert_eq!(sizes[0].key_size % 8, 0);
    }

    #[test]
    fn degenerate_inputs() {
        assert_eq!(guess_single_byte_xor(&[], 5), Vec::new());
        assert_eq!(guess_repeating_xor(&[1], 40, 5).len(), 5);
        assert_eq!(estimate_key_sizes(&[1, 2, 3], 40).len(), 1);
        assert_eq!(minimal_period(b"abab"), b"ab");
        assert_eq!(minimal_period(b"aba"), b"aba");
    }
}
