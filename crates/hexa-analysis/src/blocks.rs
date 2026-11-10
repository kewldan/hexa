//! Per-block hashes, verification against expected values and the search
//! for a range that produces a given checksum (ANL-05).

use hexa_core::hash::{CRC_PRESETS, Crc, CrcParams, HashResult};
use hexa_core::task::Progress;
use hexa_core::{Cancel, Error, Result, Tree};
use serde::{Deserialize, Serialize};

use crate::hash::{self, LocalHasher};
use crate::util::{self, invalid};

/// Hashes of one block.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockHash {
    pub index: u64,
    pub offset: u64,
    pub len: u64,
    /// One result per requested algorithm, in request order.
    pub values: Vec<HashResult>,
    /// Comparison of the first algorithm's value with the expected value for
    /// this block; `None` when no expected value was supplied.
    pub matches: Option<bool>,
}

/// Upper bound on the number of blocks per call.
pub const MAX_BLOCKS: u64 = 1 << 22;

/// Normalises a digest for comparison: trims, drops `0x`, spaces, `-` and
/// `_` separators, lower-cases hex and strips leading zeros. Non-hex values
/// (ssdeep, TLSH) are compared verbatim apart from surrounding whitespace.
fn normalize(v: &str) -> String {
    let t = v.trim();
    let t = t
        .strip_prefix("0x")
        .or_else(|| t.strip_prefix("0X"))
        .unwrap_or(t);
    let compact: String = t
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-' && *c != '_')
        .collect();
    if !compact.is_empty() && compact.chars().all(|c| c.is_ascii_hexdigit()) {
        let lower = compact.to_ascii_lowercase();
        let trimmed = lower.trim_start_matches('0');
        if trimmed.is_empty() {
            "0".into()
        } else {
            trimmed.into()
        }
    } else {
        t.to_string()
    }
}

/// Whether a computed digest equals an expected one (see [`normalize`]).
pub fn value_matches(actual: &str, expected: &str) -> bool {
    normalize(actual) == normalize(expected)
}

/// Hashes `[start, end)` in blocks of `block_size` with every algorithm in
/// `ids` (this crate's or `hexa_core::hash` ids). The last block may be
/// shorter. `expected[i]`, when present, is compared with block `i`'s first
/// value.
pub fn block_hashes(
    tree: &Tree,
    start: u64,
    end: u64,
    block_size: u64,
    ids: &[String],
    custom: Option<CrcParams>,
    expected: &[String],
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<Vec<BlockHash>> {
    if block_size == 0 {
        return Err(invalid("block size must be positive"));
    }
    if ids.is_empty() {
        return Err(invalid("no hash algorithm selected"));
    }
    let (start, end) = util::clamp(tree, start, end);
    let count = (end - start).div_ceil(block_size);
    if count > MAX_BLOCKS {
        return Err(Error::TooLarge {
            len: count,
            limit: MAX_BLOCKS,
        });
    }
    let mut out = Vec::with_capacity(count as usize);
    for index in 0..count {
        cancel.check()?;
        let offset = start + index * block_size;
        let len = block_size.min(end - offset);
        let values = hash::hash_range(tree, offset, offset + len, ids, custom, cancel, &noop)?;
        let matches = expected
            .get(index as usize)
            .map(|e| value_matches(&values[0].value, e));
        out.push(BlockHash {
            index,
            offset,
            len,
            values,
            matches,
        });
        progress(offset + len - start, end - start);
    }
    Ok(out)
}

fn noop(_: u64, _: u64) {}

/// Parameters of [`find_hash_range`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RangeSearch {
    /// Any algorithm id (this crate, `hexa_core::hash`, `crc:custom`).
    pub algorithm: String,
    /// Expected digest (hex for numeric algorithms).
    pub target: String,
    /// Candidate start offsets: `start_min..=start_max`.
    pub start_min: u64,
    pub start_max: u64,
    /// Candidate (exclusive) end offsets: `end_min..=end_max`.
    pub end_min: u64,
    pub end_max: u64,
    /// Minimum range length (at least 1).
    pub min_len: u64,
    /// Stop after this many matches.
    pub max_matches: usize,
    /// Maximum number of `(start, end)` candidates to evaluate.
    pub budget: u64,
    pub custom: Option<CrcParams>,
}

/// One range whose digest equals the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RangeMatch {
    pub start: u64,
    pub end: u64,
}

/// Outcome of [`find_hash_range`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RangeSearchResult {
    pub matches: Vec<RangeMatch>,
    /// Candidates evaluated.
    pub evaluated: u64,
    /// `true` when every candidate was evaluated (no budget / match cut-off).
    pub complete: bool,
}

/// Bytes loaded for a search; the windows must fit into memory.
pub const MAX_SEARCH_SPAN: u64 = 64 << 20;

enum Engine {
    Crc(Crc, u64),
    Local(LocalHasher),
    /// Recompute the digest of every window (non-incremental algorithms).
    Whole,
}

fn crc_params(id: &str, custom: Option<CrcParams>) -> Option<Result<CrcParams>> {
    let name = id.strip_prefix("crc:")?;
    if name == "custom" {
        return Some(custom.ok_or_else(|| invalid("custom CRC needs parameters")));
    }
    Some(
        CRC_PRESETS
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.params)
            .ok_or_else(|| invalid(format!("unknown CRC {name}"))),
    )
}

/// Searches start/end windows for a range whose digest equals `target`
/// ("find the range that produces this CRC32"). CRCs and this crate's
/// streaming hashes are evaluated incrementally (O(1) per candidate end);
/// other algorithms recompute each window, so keep `budget` small for them.
pub fn find_hash_range(
    tree: &Tree,
    search: &RangeSearch,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<RangeSearchResult> {
    let s = search;
    if s.start_min > s.start_max || s.end_min > s.end_max {
        return Err(invalid("empty start or end window"));
    }
    let end_max = s.end_max.min(tree.len());
    let span = end_max.saturating_sub(s.start_min);
    if span > MAX_SEARCH_SPAN {
        return Err(Error::TooLarge {
            len: span,
            limit: MAX_SEARCH_SPAN,
        });
    }
    let data = tree.read_vec(s.start_min, span as usize)?;
    let base = s.start_min;
    let min_len = s.min_len.max(1);
    let crc = crc_params(&s.algorithm, s.custom).transpose()?;
    let crc_target = if crc.is_some() {
        Some(
            u64::from_str_radix(&normalize(&s.target), 16)
                .map_err(|_| invalid("CRC target must be hex"))?,
        )
    } else {
        None
    };
    let ids = [s.algorithm.clone()];
    // Validate the id once.
    hash::hash_bytes(&[], &ids, s.custom)?;

    let mut result = RangeSearchResult {
        matches: Vec::new(),
        evaluated: 0,
        complete: true,
    };
    let starts = s.start_max.min(end_max.saturating_sub(min_len));
    let total_starts = (starts + 1).saturating_sub(s.start_min);
    let mut start = s.start_min;
    'outer: while start <= starts {
        cancel.check()?;
        let first_end = s.end_min.max(start + min_len);
        if first_end > end_max {
            start += 1;
            continue;
        }
        let rel = |o: u64| (o - base) as usize;
        let mut engine = if let (Some(p), Some(t)) = (crc, crc_target) {
            let mut c = Crc::new(p)?;
            c.update(&data[rel(start)..rel(first_end)]);
            Engine::Crc(c, t)
        } else {
            let mut peek = |_: u64, _: usize| Ok(Vec::new());
            match LocalHasher::new(&s.algorithm, 0, &mut peek) {
                Some(h) if h.as_ref().is_ok_and(LocalHasher::is_incremental) => {
                    let mut h = h?;
                    h.update(&data[rel(start)..rel(first_end)]);
                    Engine::Local(h)
                }
                _ => Engine::Whole,
            }
        };
        let mut end = first_end;
        loop {
            if result.evaluated >= s.budget {
                result.complete = false;
                break 'outer;
            }
            result.evaluated += 1;
            let hit = match &engine {
                Engine::Crc(c, t) => c.finish() == *t,
                Engine::Local(h) => value_matches(&h.finish(), &s.target),
                Engine::Whole => {
                    let v = hash::hash_bytes(&data[rel(start)..rel(end)], &ids, s.custom)?;
                    value_matches(&v[0].value, &s.target)
                }
            };
            if hit {
                result.matches.push(RangeMatch { start, end });
                if result.matches.len() >= s.max_matches.max(1) {
                    result.complete = false;
                    break 'outer;
                }
            }
            if end >= end_max {
                break;
            }
            let next = &data[rel(end)..=rel(end)];
            match &mut engine {
                Engine::Crc(c, _) => c.update(next),
                Engine::Local(h) => h.update(next),
                Engine::Whole => {}
            }
            end += 1;
            if result.evaluated.is_multiple_of(65_536) {
                cancel.check()?;
            }
        }
        progress(start + 1 - s.start_min, total_starts);
        start += 1;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{noop, random_bytes, tree};

    fn vals(v: &[HashResult]) -> Vec<(String, String)> {
        v.iter().map(|r| (r.id.clone(), r.value.clone())).collect()
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn blocks_and_expected_values() {
        let data = random_bytes(1, 10_000);
        let t = tree(&data);
        let algos = ids(&["crc:CRC-32/ISO-HDLC", "xxh64", "sha1"]);
        let first = hash::hash_bytes(&data[..4096], &algos, None).unwrap();
        let expected = vec![
            format!("0x{}", first[0].value.to_lowercase()),
            "DEADBEEF".into(),
        ];
        let blocks = block_hashes(
            &t,
            0,
            10_000,
            4096,
            &algos,
            None,
            &expected,
            &Cancel::new(),
            &noop,
        )
        .unwrap();
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[2].len, 10_000 - 8192);
        assert_eq!(vals(&blocks[0].values), vals(&first));
        assert_eq!(blocks[0].matches, Some(true));
        assert_eq!(blocks[1].matches, Some(false));
        assert_eq!(blocks[2].matches, None);
        assert_eq!(
            vals(&blocks[2].values),
            vals(&hash::hash_bytes(&data[8192..], &algos, None).unwrap())
        );
        assert!(block_hashes(&t, 0, 1, 0, &algos, None, &[], &Cancel::new(), &noop).is_err());
    }

    #[test]
    fn normalisation() {
        assert!(value_matches("00CBF43926", "0xcbf43926"));
        assert!(value_matches("de ad be ef", "DEADBEEF"));
        assert!(!value_matches("3:abc:d", "3:ABC:d"));
        assert!(value_matches("0", "0x0000"));
    }

    fn search(algorithm: &str, target: String) -> RangeSearch {
        RangeSearch {
            algorithm: algorithm.into(),
            target,
            start_min: 0,
            start_max: 200,
            end_min: 0,
            end_max: 400,
            min_len: 1,
            max_matches: 10,
            budget: u64::MAX,
            custom: None,
        }
    }

    #[test]
    fn finds_crc_range() {
        let mut data = random_bytes(4, 400);
        data[150..159].copy_from_slice(b"123456789");
        let t = tree(&data);
        let r = find_hash_range(
            &t,
            &search("crc:CRC-32/ISO-HDLC", "CBF43926".into()),
            &Cancel::new(),
            &noop,
        )
        .unwrap();
        assert!(r.complete);
        assert_eq!(
            r.matches,
            vec![RangeMatch {
                start: 150,
                end: 159
            }]
        );
    }

    #[test]
    fn finds_local_and_core_hash_ranges() {
        let data = random_bytes(5, 400);
        let t = tree(&data);
        for algo in ["xxh32", "fnv1a-64", "md5", "city64"] {
            let target = hash::hash_bytes(&data[77..120], &[algo.into()], None).unwrap()[0]
                .value
                .clone();
            let mut s = search(algo, target);
            s.start_min = 70;
            s.start_max = 80;
            s.end_min = 110;
            s.end_max = 130;
            let r = find_hash_range(&t, &s, &Cancel::new(), &noop).unwrap();
            assert_eq!(
                r.matches,
                vec![RangeMatch {
                    start: 77,
                    end: 120
                }],
                "{algo}"
            );
            assert_eq!(r.evaluated, 11 * 21);
        }
    }

    #[test]
    fn budget_and_errors() {
        let t = tree(&random_bytes(6, 400));
        let mut s = search("crc:CRC-32/ISO-HDLC", "1".into());
        s.budget = 100;
        let r = find_hash_range(&t, &s, &Cancel::new(), &noop).unwrap();
        assert!(!r.complete);
        assert_eq!(r.evaluated, 100);
        assert!(find_hash_range(&t, &search("bogus", "1".into()), &Cancel::new(), &noop).is_err());
        assert!(
            find_hash_range(
                &t,
                &search("crc:CRC-32/ISO-HDLC", "xyz".into()),
                &Cancel::new(),
                &noop
            )
            .is_err()
        );
    }
}
