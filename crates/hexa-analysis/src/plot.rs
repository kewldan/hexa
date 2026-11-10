//! Plot data (ANL-17): interprets a range as an array of integers or floats
//! and downsamples it to min/max buckets for charts.

use hexa_core::task::Progress;
use hexa_core::{Cancel, Result, Tree};
use serde::{Deserialize, Serialize};

use crate::util::{self, CHUNK, invalid};

/// Element type of the plotted array.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NumType {
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
    U64,
    I64,
    F32,
    F64,
}

impl NumType {
    pub fn size(self) -> usize {
        match self {
            Self::U8 | Self::I8 => 1,
            Self::U16 | Self::I16 => 2,
            Self::U32 | Self::I32 | Self::F32 => 4,
            Self::U64 | Self::I64 | Self::F64 => 8,
        }
    }

    /// Decodes one element (`b.len() == self.size()`).
    pub fn decode(self, b: &[u8], big_endian: bool) -> f64 {
        macro_rules! rd {
            ($t:ty) => {{
                let a = b.try_into().expect("element size");
                if big_endian {
                    <$t>::from_be_bytes(a)
                } else {
                    <$t>::from_le_bytes(a)
                }
            }};
        }
        match self {
            Self::U8 => f64::from(b[0]),
            Self::I8 => f64::from(b[0] as i8),
            Self::U16 => f64::from(rd!(u16)),
            Self::I16 => f64::from(rd!(i16)),
            Self::U32 => f64::from(rd!(u32)),
            Self::I32 => f64::from(rd!(i32)),
            Self::U64 => rd!(u64) as f64,
            Self::I64 => rd!(i64) as f64,
            Self::F32 => f64::from(rd!(f32)),
            Self::F64 => rd!(f64),
        }
    }
}

/// Options of [`plot_series`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlotOptions {
    pub num_type: NumType,
    pub big_endian: bool,
    /// Number of output buckets (chart width); at least 1.
    pub buckets: usize,
    /// Bytes from one element to the next (default: the element size), for
    /// plotting one field of an array of records.
    pub stride: Option<u64>,
}

/// Min/max of a run of consecutive elements. Non-finite values (NaN, ±inf)
/// are skipped; `min`/`max` are `None` if the bucket has no finite value.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlotBucket {
    /// Index of the first element.
    pub first: u64,
    pub count: u64,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

/// Downsampled series.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlotSeries {
    /// Total number of elements.
    pub count: u64,
    pub buckets: Vec<PlotBucket>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// Mean of the finite values.
    pub mean: Option<f64>,
    /// Elements that were NaN or infinite.
    pub non_finite: u64,
}

#[derive(Debug)]
struct Acc {
    count: u64,
    nbuckets: u64,
    buckets: Vec<PlotBucket>,
    index: u64,
    sum: f64,
    finite: u64,
    min: Option<f64>,
    max: Option<f64>,
}

impl Acc {
    fn push(&mut self, v: f64) {
        let b =
            (u128::from(self.index) * u128::from(self.nbuckets) / u128::from(self.count)) as usize;
        if b == self.buckets.len() {
            self.buckets.push(PlotBucket {
                first: self.index,
                count: 0,
                min: None,
                max: None,
            });
        }
        let bucket = self.buckets.last_mut().expect("bucket exists");
        bucket.count += 1;
        if v.is_finite() {
            bucket.min = Some(bucket.min.map_or(v, |m| m.min(v)));
            bucket.max = Some(bucket.max.map_or(v, |m| m.max(v)));
            self.min = Some(self.min.map_or(v, |m| m.min(v)));
            self.max = Some(self.max.map_or(v, |m| m.max(v)));
            self.sum += v;
            self.finite += 1;
        }
        self.index += 1;
    }

    fn finish(self) -> PlotSeries {
        PlotSeries {
            count: self.count,
            buckets: self.buckets,
            min: self.min,
            max: self.max,
            mean: (self.finite > 0).then(|| self.sum / self.finite as f64),
            non_finite: self.count - self.finite,
        }
    }
}

/// Interprets `[start, end)` as an array and returns at most
/// `opts.buckets` min/max buckets (one element per bucket when the array is
/// short enough).
pub fn plot_series(
    tree: &Tree,
    start: u64,
    end: u64,
    opts: &PlotOptions,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<PlotSeries> {
    let size = opts.num_type.size() as u64;
    let stride = opts.stride.unwrap_or(size);
    if stride < size || stride > CHUNK as u64 {
        return Err(invalid(
            "stride must be at least the element size and at most 4 MiB",
        ));
    }
    if opts.buckets == 0 {
        return Err(invalid("at least one bucket is needed"));
    }
    let (start, end) = util::clamp(tree, start, end);
    let len = end - start;
    let count = if len < size {
        0
    } else {
        (len - size) / stride + 1
    };
    let mut acc = Acc {
        count,
        nbuckets: count.min(opts.buckets as u64),
        buckets: Vec::new(),
        index: 0,
        sum: 0.0,
        finite: 0,
        min: None,
        max: None,
    };
    if count > 0 {
        let span = (count - 1) * stride + size;
        let per_chunk = (CHUNK as u64 / stride).max(1);
        let mut first = 0u64;
        while first < count {
            cancel.check()?;
            let n = per_chunk.min(count - first);
            let off = start + first * stride;
            let bytes = ((n - 1) * stride + size) as usize;
            let buf = tree.read_vec(off, bytes)?;
            for i in 0..n as usize {
                let at = i * stride as usize;
                acc.push(
                    opts.num_type
                        .decode(&buf[at..at + size as usize], opts.big_endian),
                );
            }
            first += n;
            progress((first * stride).min(span), span);
        }
    }
    Ok(acc.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{noop, tree};

    fn run(data: &[u8], num_type: NumType, big_endian: bool, buckets: usize) -> PlotSeries {
        let o = PlotOptions {
            num_type,
            big_endian,
            buckets,
            stride: None,
        };
        plot_series(&tree(data), 0, data.len() as u64, &o, &Cancel::new(), &noop).unwrap()
    }

    #[test]
    fn decodes_all_types() {
        let b = [0xFF, 0xFE, 0xFD, 0xFC, 0xFB, 0xFA, 0xF9, 0xF8];
        assert_eq!(NumType::U8.decode(&b[..1], false), 255.0);
        assert_eq!(NumType::I8.decode(&b[..1], false), -1.0);
        assert_eq!(NumType::U16.decode(&b[..2], true), 65534.0);
        assert_eq!(NumType::I16.decode(&b[..2], false), -257.0);
        assert_eq!(NumType::U32.decode(&[1, 0, 0, 0], false), 1.0);
        assert_eq!(
            NumType::I32.decode(&b[..4], true),
            f64::from(0xFFFE_FDFC_u32 as i32)
        );
        assert_eq!(NumType::U64.decode(&[0, 0, 0, 0, 0, 0, 0, 1], true), 1.0);
        assert_eq!(NumType::I64.decode(&b, false), i64::from_le_bytes(b) as f64);
        assert_eq!(NumType::F32.decode(&1.5f32.to_le_bytes(), false), 1.5);
        assert_eq!(NumType::F64.decode(&(-2.25f64).to_be_bytes(), true), -2.25);
    }

    #[test]
    fn raw_and_downsampled() {
        let raw = run(&[3, 1, 2], NumType::U8, false, 10);
        assert_eq!(raw.count, 3);
        assert_eq!(raw.buckets.len(), 3);
        assert_eq!(raw.buckets[1].min, Some(1.0));
        assert_eq!(raw.mean, Some(2.0));

        let data: Vec<u8> = (0..1000u16).flat_map(u16::to_le_bytes).collect();
        let s = run(&data, NumType::U16, false, 10);
        assert_eq!(s.count, 1000);
        assert_eq!(s.buckets.len(), 10);
        assert_eq!(s.buckets[3].first, 300);
        assert_eq!(
            (s.buckets[3].min, s.buckets[3].max),
            (Some(300.0), Some(399.0))
        );
        assert_eq!((s.min, s.max), (Some(0.0), Some(999.0)));
        assert_eq!(s.buckets.iter().map(|b| b.count).sum::<u64>(), 1000);
    }

    #[test]
    fn floats_skip_non_finite_and_trailing_bytes() {
        let mut d: Vec<u8> = [1.0f32, f32::NAN, -4.0, f32::INFINITY]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        d.extend([1, 2]); // incomplete element is ignored
        let s = run(&d, NumType::F32, false, 2);
        assert_eq!(s.count, 4);
        assert_eq!(s.non_finite, 2);
        assert_eq!(s.buckets[0].max, Some(1.0));
        assert_eq!(s.buckets[1].min, Some(-4.0));
        assert_eq!(s.mean, Some(-1.5));
        let empty = run(&[], NumType::F64, false, 5);
        assert_eq!((empty.count, empty.min), (0, None));
    }

    #[test]
    fn stride_and_errors() {
        // Records of {u16 id, u8 value}; plot the value.
        let d = [0, 0, 5, 0, 0, 7, 0, 0, 9];
        let o = PlotOptions {
            num_type: NumType::U8,
            big_endian: false,
            buckets: 100,
            stride: Some(3),
        };
        let s = plot_series(&tree(&d), 2, 9, &o, &Cancel::new(), &noop).unwrap();
        let vals: Vec<_> = s.buckets.iter().map(|b| b.min.unwrap()).collect();
        assert_eq!(vals, [5.0, 7.0, 9.0]);
        let bad = PlotOptions {
            stride: Some(1),
            num_type: NumType::U32,
            ..o.clone()
        };
        assert!(plot_series(&tree(&d), 0, 9, &bad, &Cancel::new(), &noop).is_err());
        let bad = PlotOptions { buckets: 0, ..o };
        assert!(plot_series(&tree(&d), 0, 9, &bad, &Cancel::new(), &noop).is_err());
    }
}
