//! Byte histogram and frequency table (ANL-07) and Shannon entropy over a
//! whole range or sliding windows (ANL-08).

use hexa_core::task::Progress;
use hexa_core::{Cancel, Error, Result, Tree};
use serde::{Deserialize, Serialize};

use crate::util::{self, CHUNK, Cursor, chi_square_of, entropy_of, invalid};

/// Byte histogram of a range with summary statistics (ANL-07).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Histogram {
    /// 256 counts, index = byte value.
    pub counts: Vec<u64>,
    pub total: u64,
    /// Number of byte values that occur at least once.
    pub distinct: u32,
    /// Shannon entropy in bits per byte (0…8).
    pub entropy: f64,
    /// Arithmetic mean of the byte values (127.5 for uniform data).
    pub mean: f64,
    /// Chi-square statistic against a uniform distribution (255 d.o.f.).
    pub chi_square: f64,
    /// Most frequent byte (lowest value on ties); `None` for empty input.
    pub most_common: Option<u8>,
    /// Least frequent byte among those present.
    pub least_common: Option<u8>,
}

impl Histogram {
    fn from_counts(counts: &[u64; 256]) -> Self {
        let total: u64 = counts.iter().sum();
        let sum: f64 = counts
            .iter()
            .enumerate()
            .map(|(b, &c)| b as f64 * c as f64)
            .sum();
        let present = || counts.iter().enumerate().filter(|(_, c)| **c > 0);
        let most_common = present()
            .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(&a.0)))
            .map(|(b, _)| b as u8);
        let least_common = present()
            .min_by(|a, b| a.1.cmp(b.1).then(a.0.cmp(&b.0)))
            .map(|(b, _)| b as u8);
        Self {
            counts: counts.to_vec(),
            total,
            distinct: present().count() as u32,
            entropy: entropy_of(counts, total),
            mean: if total == 0 { 0.0 } else { sum / total as f64 },
            chi_square: chi_square_of(counts, total),
            most_common,
            least_common,
        }
    }
}

fn count(counts: &mut [u64; 256], data: &[u8]) {
    for &b in data {
        counts[usize::from(b)] += 1;
    }
}

/// Histogram of a buffer.
pub fn histogram_bytes(data: &[u8]) -> Histogram {
    let mut counts = [0u64; 256];
    count(&mut counts, data);
    Histogram::from_counts(&counts)
}

/// Histogram of `[start, end)` of a snapshot.
pub fn histogram(
    tree: &Tree,
    start: u64,
    end: u64,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<Histogram> {
    let mut counts = [0u64; 256];
    util::scan(tree, start, end, CHUNK, cancel, progress, |_, chunk| {
        count(&mut counts, chunk);
        Ok(true)
    })?;
    Ok(Histogram::from_counts(&counts))
}

/// Shannon entropy (bits per byte) of a buffer (ANL-08).
pub fn entropy(data: &[u8]) -> f64 {
    histogram_bytes(data).entropy
}

/// Ordering of [`frequency_table`] rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FrequencySort {
    Byte,
    CountDesc,
    CountAsc,
}

/// One row of the frequency table.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrequencyRow {
    pub byte: u8,
    pub count: u64,
    /// Share of the total in percent.
    pub percent: f64,
    /// Printable character, or a C-style escape / `\xNN`.
    pub display: String,
}

fn display_byte(b: u8) -> String {
    match b {
        0 => "\\0".into(),
        b'\t' => "\\t".into(),
        b'\n' => "\\n".into(),
        b'\r' => "\\r".into(),
        0x20..=0x7E => (b as char).to_string(),
        _ => format!("\\x{b:02X}"),
    }
}

/// Frequency table built from a histogram (ANL-07). Ties keep byte order.
pub fn frequency_table(
    h: &Histogram,
    sort: FrequencySort,
    include_absent: bool,
) -> Vec<FrequencyRow> {
    let mut rows: Vec<FrequencyRow> = h
        .counts
        .iter()
        .enumerate()
        .filter(|(_, c)| include_absent || **c > 0)
        .map(|(b, &c)| FrequencyRow {
            byte: b as u8,
            count: c,
            percent: if h.total == 0 {
                0.0
            } else {
                c as f64 * 100.0 / h.total as f64
            },
            display: display_byte(b as u8),
        })
        .collect();
    match sort {
        FrequencySort::Byte => {}
        FrequencySort::CountDesc => rows.sort_by_key(|r| std::cmp::Reverse(r.count)),
        FrequencySort::CountAsc => rows.sort_by_key(|r| r.count),
    }
    rows
}

/// Entropy of consecutive windows, for charts and the minimap layer (ANL-08).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntropySeries {
    /// Offset of the first window.
    pub start: u64,
    pub window: u64,
    pub step: u64,
    /// `values[i]` is the entropy (bits/byte) of
    /// `[start + i*step, start + i*step + window)`.
    pub values: Vec<f32>,
}

/// Upper bound on the number of points one series may have.
pub const MAX_SERIES_POINTS: u64 = 1 << 20;

/// Entropy of `[s, s + window)` for `s = start, start + step, …` while the
/// window fits; a range shorter than `window` yields one shortened window.
pub fn entropy_series(
    tree: &Tree,
    start: u64,
    end: u64,
    window: u64,
    step: u64,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<EntropySeries> {
    if window == 0 || step == 0 {
        return Err(invalid("entropy window and step must be positive"));
    }
    let (start, end) = util::clamp(tree, start, end);
    let len = end - start;
    let window = window.min(len.max(1));
    let points = if len == 0 {
        0
    } else {
        (len - window) / step + 1
    };
    if points > MAX_SERIES_POINTS {
        return Err(Error::TooLarge {
            len: points,
            limit: MAX_SERIES_POINTS,
        });
    }
    let mut values = Vec::with_capacity(points as usize);
    let mut counts = [0u64; 256];
    let h = |counts: &[u64; 256]| entropy_of(counts, window) as f32;
    if step >= window {
        for i in 0..points {
            let s = start + i * step;
            counts = [0; 256];
            util::scan(tree, s, s + window, CHUNK, cancel, &util_noop, |_, c| {
                count(&mut counts, c);
                Ok(true)
            })?;
            values.push(h(&counts));
            progress(i + 1, points);
        }
    } else if points > 0 {
        let mut head = Cursor::new(tree, start, end, CHUNK);
        let mut tail = Cursor::new(tree, start, end, CHUNK);
        for _ in 0..window {
            let b = head.next_byte()?.expect("window fits");
            counts[usize::from(b)] += 1;
        }
        values.push(h(&counts));
        for i in 1..points {
            if i % 1024 == 0 {
                cancel.check()?;
                progress(i, points);
            }
            for _ in 0..step {
                let add = head.next_byte()?.expect("window fits");
                let sub = tail.next_byte()?.expect("window fits");
                counts[usize::from(add)] += 1;
                counts[usize::from(sub)] -= 1;
            }
            values.push(h(&counts));
        }
        progress(points, points);
    }
    Ok(EntropySeries {
        start,
        window,
        step,
        values,
    })
}

fn util_noop(_: u64, _: u64) {}

/// Entropy minimap: `points` adjacent windows covering the whole range.
pub fn entropy_minimap(
    tree: &Tree,
    start: u64,
    end: u64,
    points: u64,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<EntropySeries> {
    let (start, end) = util::clamp(tree, start, end);
    let size = (end - start).div_ceil(points.max(1)).max(1);
    entropy_series(tree, start, end, size, size, cancel, progress)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{noop, random_bytes, tree};

    #[test]
    fn histogram_statistics() {
        let h = histogram_bytes(b"aab");
        assert_eq!(h.total, 3);
        assert_eq!(h.counts[usize::from(b'a')], 2);
        assert_eq!(h.distinct, 2);
        assert_eq!(h.most_common, Some(b'a'));
        assert_eq!(h.least_common, Some(b'b'));
        assert!((h.entropy - 0.918_295_834).abs() < 1e-6);
        assert!((h.mean - (97.0 * 2.0 + 98.0) / 3.0).abs() < 1e-9);

        let all: Vec<u8> = (0..=255).collect();
        let h = histogram_bytes(&all);
        assert!((h.entropy - 8.0).abs() < 1e-12);
        assert!(h.chi_square.abs() < 1e-9);
        assert!((h.mean - 127.5).abs() < 1e-12);

        let empty = histogram_bytes(&[]);
        assert_eq!(empty.entropy, 0.0);
        assert_eq!(empty.most_common, None);
    }

    #[test]
    fn histogram_streams() {
        let data = random_bytes(1, 9_000_001);
        let t = tree(&data);
        let a = histogram(&t, 5, 8_999_000, &Cancel::new(), &noop).unwrap();
        assert_eq!(a, histogram_bytes(&data[5..8_999_000]));
        assert!(a.entropy > 7.99);
    }

    #[test]
    fn frequency_rows() {
        let h = histogram_bytes(b"\0aaab\n");
        let rows = frequency_table(&h, FrequencySort::CountDesc, false);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].byte, b'a');
        assert!((rows[0].percent - 50.0).abs() < 1e-9);
        assert_eq!(rows[1].display, "\\0");
        assert_eq!(frequency_table(&h, FrequencySort::Byte, true).len(), 256);
        let asc = frequency_table(&h, FrequencySort::CountAsc, false);
        assert_eq!(asc.last().unwrap().byte, b'a');
        assert_eq!(display_byte(0xFE), "\\xFE");
    }

    #[test]
    fn sliding_series_matches_direct_computation() {
        let mut data = random_bytes(2, 20_000);
        data[5_000..12_000].fill(0);
        let t = tree(&data);
        for (window, step) in [(1024, 256), (1000, 1000), (512, 2048), (4096, 1)] {
            let s = entropy_series(&t, 100, 19_900, window, step, &Cancel::new(), &noop).unwrap();
            let expected: Vec<f32> = (100..=19_900 - window as usize)
                .step_by(step as usize)
                .map(|p| entropy(&data[p..p + window as usize]) as f32)
                .collect();
            assert_eq!(s.values.len(), expected.len(), "{window}/{step}");
            for (a, b) in s.values.iter().zip(&expected) {
                assert!((a - b).abs() < 1e-4);
            }
        }
        let short = entropy_series(&t, 0, 10, 4096, 64, &Cancel::new(), &noop).unwrap();
        assert_eq!(short.values.len(), 1);
        assert_eq!(short.window, 10);
        assert!(entropy_series(&t, 0, 10, 0, 1, &Cancel::new(), &noop).is_err());
        assert_eq!(
            entropy_series(&tree(&[]), 0, 0, 16, 16, &Cancel::new(), &noop)
                .unwrap()
                .values
                .len(),
            0
        );
    }

    #[test]
    fn minimap_covers_range() {
        let data = random_bytes(3, 100_000);
        let m = entropy_minimap(&tree(&data), 0, 100_000, 100, &Cancel::new(), &noop).unwrap();
        assert_eq!(m.values.len(), 100);
        assert_eq!(m.window, 1000);
        assert!(m.values.iter().all(|&v| v > 7.5));
    }
}
