//! Cheat-Engine style value scanner (PROC-06).
//!
//! A [`Scanner`] runs a *first scan* over the selected regions (writable only
//! by default) and then any number of *next scans* that filter the previous
//! hits. Supported types are `u8..u64`, `i8..i64`, `f32`, `f64`, aligned to
//! the value size by default.
//!
//! Results are stored per block of at most [`DEFAULT_BLOCK_SIZE`] bytes in the
//! most compact of three forms:
//!
//! * **dense** — after an *unknown initial value* scan: just the bytes of the
//!   block; every aligned position is a hit (no per-hit overhead);
//! * **masked** — the block bytes plus one bit per aligned position;
//! * **sparse** — a `u32` offset plus the value bytes per hit.
//!
//! Scans report progress and honour [`Cancel`]; a cancelled scan leaves the
//! previous results untouched. Results are read back page by page
//! ([`Scanner::page`]), optionally with the current live value.

use std::fmt;
use std::ops::Range;

use hexa_core::Cancel;
use hexa_core::task::Progress;
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{Error, Result};
use crate::region::RegionFilter;
use crate::source::MemorySource;

/// Bytes read per scan step.
pub const DEFAULT_BLOCK_SIZE: u64 = 4 << 20;

/// Runs `$body` with `$T` bound to the Rust type of a [`ValueType`].
macro_rules! with_type {
    ($ty:expr, $T:ident => $body:expr) => {
        match $ty {
            ValueType::U8 => {
                type $T = u8;
                $body
            }
            ValueType::U16 => {
                type $T = u16;
                $body
            }
            ValueType::U32 => {
                type $T = u32;
                $body
            }
            ValueType::U64 => {
                type $T = u64;
                $body
            }
            ValueType::I8 => {
                type $T = i8;
                $body
            }
            ValueType::I16 => {
                type $T = i16;
                $body
            }
            ValueType::I32 => {
                type $T = i32;
                $body
            }
            ValueType::I64 => {
                type $T = i64;
                $body
            }
            ValueType::F32 => {
                type $T = f32;
                $body
            }
            ValueType::F64 => {
                type $T = f64;
                $body
            }
        }
    };
}

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

/// Scalar type to scan for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ValueType {
    U8,
    U16,
    U32,
    U64,
    I8,
    I16,
    I32,
    I64,
    F32,
    F64,
}

/// A scan operand or result value. Integers are exact (`i128` covers both
/// `u64` and `i64`). Serialized as a string so 64-bit values survive JSON.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Num {
    Int(i128),
    Float(f64),
}

impl Num {
    /// Parses decimal, `0x` hex (optionally negative) or floating point text.
    pub fn parse(text: &str) -> Result<Self> {
        let t = text.trim().replace('_', "");
        let (neg, body) = match t.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, t.strip_prefix('+').unwrap_or(&t)),
        };
        let int = if let Some(hex) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
            i128::from_str_radix(hex, 16).ok()
        } else {
            body.parse::<i128>().ok()
        };
        if let Some(v) = int {
            return Ok(Self::Int(if neg { -v } else { v }));
        }
        t.parse::<f64>()
            .map(Self::Float)
            .map_err(|_| Error::invalid(format!("not a number: {text:?}")))
    }

    pub fn as_f64(self) -> f64 {
        match self {
            Self::Int(i) => i as f64,
            Self::Float(f) => f,
        }
    }
}

impl fmt::Display for Num {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Int(i) => write!(f, "{i}"),
            Self::Float(v) => write!(f, "{v}"),
        }
    }
}

impl Serialize for Num {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Num {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = Num;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a number or a numeric string")
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Num, E> {
                Ok(Num::Int(i128::from(v)))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Num, E> {
                Ok(Num::Int(i128::from(v)))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<Num, E> {
                Ok(Num::Float(v))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Num, E> {
                Num::parse(v).map_err(E::custom)
            }
        }
        d.deserialize_any(V)
    }
}

impl ValueType {
    pub const fn size(self) -> usize {
        match self {
            Self::U8 | Self::I8 => 1,
            Self::U16 | Self::I16 => 2,
            Self::U32 | Self::I32 | Self::F32 => 4,
            Self::U64 | Self::I64 | Self::F64 => 8,
        }
    }

    pub const fn is_float(self) -> bool {
        matches!(self, Self::F32 | Self::F64)
    }

    fn int_bounds(self) -> (i128, i128) {
        match self {
            Self::U8 => (0, u8::MAX.into()),
            Self::U16 => (0, u16::MAX.into()),
            Self::U32 => (0, u32::MAX.into()),
            Self::U64 => (0, u64::MAX.into()),
            Self::I8 => (i8::MIN.into(), i8::MAX.into()),
            Self::I16 => (i16::MIN.into(), i16::MAX.into()),
            Self::I32 => (i32::MIN.into(), i32::MAX.into()),
            Self::I64 | Self::F32 | Self::F64 => (i64::MIN.into(), i64::MAX.into()),
        }
    }

    /// Converts `n` to this type's domain: floats for float types, in-range
    /// integers for integer types.
    pub fn normalize(self, n: Num) -> Result<Num> {
        if self.is_float() {
            return Ok(Num::Float(n.as_f64()));
        }
        let i = match n {
            Num::Int(i) => i,
            Num::Float(f) if f.fract() == 0.0 && f.is_finite() => f as i128,
            Num::Float(f) => return Err(Error::invalid(format!("{f} is not an integer"))),
        };
        let (lo, hi) = self.int_bounds();
        if i < lo || i > hi {
            return Err(Error::invalid(format!("{i} does not fit in {self:?}")));
        }
        Ok(Num::Int(i))
    }

    /// Parses text into a value of this type.
    pub fn parse(self, text: &str) -> Result<Num> {
        self.normalize(Num::parse(text)?)
    }

    /// Decodes `bytes` (at least [`Self::size`] long).
    pub fn decode(self, bytes: &[u8], big_endian: bool) -> Num {
        with_type!(self, T => T::load(bytes, big_endian).to_num())
    }

    /// Encodes `value` into `size()` bytes.
    pub fn encode(self, value: Num, big_endian: bool) -> Result<Vec<u8>> {
        let n = self.normalize(value)?;
        Ok(with_type!(self, T => T::from_num(n).store(big_endian)))
    }
}

/// Fixed-size scalars the scanner works on.
trait Scalar: Copy + PartialOrd {
    fn load(b: &[u8], big: bool) -> Self;
    fn store(self, big: bool) -> Vec<u8>;
    fn from_num(n: Num) -> Self;
    fn to_num(self) -> Num;
    fn approx_eq(self, other: Self, eps: f64) -> bool;
    /// `new - old == delta` (wrapping for integers, within `eps` for floats).
    fn delta_eq(new: Self, old: Self, delta: Self, eps: f64) -> bool;
}

macro_rules! int_scalar {
    ($($t:ty),*) => {$(
        impl Scalar for $t {
            #[inline]
            fn load(b: &[u8], big: bool) -> Self {
                let mut a = [0u8; std::mem::size_of::<$t>()];
                a.copy_from_slice(&b[..std::mem::size_of::<$t>()]);
                if big { <$t>::from_be_bytes(a) } else { <$t>::from_le_bytes(a) }
            }
            fn store(self, big: bool) -> Vec<u8> {
                if big { self.to_be_bytes().to_vec() } else { self.to_le_bytes().to_vec() }
            }
            fn from_num(n: Num) -> Self {
                match n {
                    Num::Int(i) => i as $t,
                    Num::Float(f) => f as $t,
                }
            }
            fn to_num(self) -> Num {
                Num::Int(i128::from(self))
            }
            #[inline]
            fn approx_eq(self, other: Self, _eps: f64) -> bool {
                self == other
            }
            #[inline]
            fn delta_eq(new: Self, old: Self, delta: Self, _eps: f64) -> bool {
                new.wrapping_sub(old) == delta
            }
        }
    )*};
}
int_scalar!(u8, u16, u32, u64, i8, i16, i32, i64);

macro_rules! float_scalar {
    ($($t:ty),*) => {$(
        impl Scalar for $t {
            #[inline]
            fn load(b: &[u8], big: bool) -> Self {
                let mut a = [0u8; std::mem::size_of::<$t>()];
                a.copy_from_slice(&b[..std::mem::size_of::<$t>()]);
                if big { <$t>::from_be_bytes(a) } else { <$t>::from_le_bytes(a) }
            }
            fn store(self, big: bool) -> Vec<u8> {
                if big { self.to_be_bytes().to_vec() } else { self.to_le_bytes().to_vec() }
            }
            fn from_num(n: Num) -> Self {
                n.as_f64() as $t
            }
            fn to_num(self) -> Num {
                Num::Float(f64::from(self))
            }
            #[inline]
            fn approx_eq(self, other: Self, eps: f64) -> bool {
                self == other || (f64::from(self) - f64::from(other)).abs() <= eps
            }
            #[inline]
            fn delta_eq(new: Self, old: Self, delta: Self, eps: f64) -> bool {
                (f64::from(new) - f64::from(old) - f64::from(delta)).abs() <= eps
            }
        }
    )*};
}
float_scalar!(f32, f64);

// ---------------------------------------------------------------------------
// Conditions
// ---------------------------------------------------------------------------

/// Condition of the first scan.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum FirstScan {
    Exact {
        value: Num,
    },
    /// Inclusive range.
    Range {
        min: Num,
        max: Num,
    },
    /// Unknown initial value: remember everything, filter with next scans.
    Unknown,
}

/// Condition of a next scan, comparing the current value with the operand
/// or with the value seen by the previous scan.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum NextScan {
    Equal {
        value: Num,
    },
    NotEqual {
        value: Num,
    },
    /// Inclusive range.
    Between {
        min: Num,
        max: Num,
    },
    Changed,
    Unchanged,
    Increased,
    Decreased,
    IncreasedBy {
        value: Num,
    },
    DecreasedBy {
        value: Num,
    },
}

#[derive(Debug, Clone, Copy)]
enum Pred<T> {
    Any,
    Eq(T),
    Ne(T),
    Between(T, T),
    Changed,
    Unchanged,
    Inc,
    Dec,
    IncBy(T),
    DecBy(T),
}

impl<T: Scalar> Pred<T> {
    fn first(c: &FirstScan, ty: ValueType) -> Result<Self> {
        let v = |n: Num| ty.normalize(n).map(T::from_num);
        Ok(match *c {
            FirstScan::Exact { value } => Self::Eq(v(value)?),
            FirstScan::Range { min, max } => Self::Between(v(min)?, v(max)?),
            FirstScan::Unknown => Self::Any,
        })
    }

    fn next(c: &NextScan, ty: ValueType) -> Result<Self> {
        let v = |n: Num| ty.normalize(n).map(T::from_num);
        Ok(match *c {
            NextScan::Equal { value } => Self::Eq(v(value)?),
            NextScan::NotEqual { value } => Self::Ne(v(value)?),
            NextScan::Between { min, max } => Self::Between(v(min)?, v(max)?),
            NextScan::Changed => Self::Changed,
            NextScan::Unchanged => Self::Unchanged,
            NextScan::Increased => Self::Inc,
            NextScan::Decreased => Self::Dec,
            NextScan::IncreasedBy { value } => Self::IncBy(v(value)?),
            NextScan::DecreasedBy { value } => Self::DecBy(v(value)?),
        })
    }

    #[inline]
    fn test(&self, new: &[u8], old: &[u8], big: bool, eps: f64) -> bool {
        let n = || T::load(new, big);
        let o = || T::load(old, big);
        match *self {
            Self::Any => true,
            Self::Eq(v) => n().approx_eq(v, eps),
            Self::Ne(v) => !n().approx_eq(v, eps),
            Self::Between(lo, hi) => {
                let x = n();
                x >= lo && x <= hi
            }
            Self::Changed => new != old,
            Self::Unchanged => new == old,
            Self::Inc => n() > o(),
            Self::Dec => n() < o(),
            Self::IncBy(d) => T::delta_eq(n(), o(), d, eps),
            Self::DecBy(d) => T::delta_eq(o(), n(), d, eps),
        }
    }
}

// ---------------------------------------------------------------------------
// Compact result storage
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct Layout {
    size: u64,
    align: u64,
}

impl Layout {
    fn first(&self, base: u64) -> u64 {
        base.div_ceil(self.align) * self.align
    }

    /// Aligned positions `p` with `base <= p < base + span` and
    /// `p + size <= base + len`.
    fn count(&self, base: u64, len: u64, span: u64) -> u64 {
        if len < self.size {
            return 0;
        }
        let first = self.first(base);
        let limit = (base + span).min(base + len - self.size + 1);
        if first >= limit {
            0
        } else {
            (limit - first - 1) / self.align + 1
        }
    }
}

#[derive(Debug, Clone)]
enum ChunkData {
    /// Every aligned position of the span is a hit.
    Dense { bytes: Vec<u8> },
    /// Hits are the aligned positions whose bit is set.
    Masked { bytes: Vec<u8>, mask: Vec<u64> },
    /// Hit `i` is at `base + offsets[i]` with value `values[i * size..]`.
    Sparse { offsets: Vec<u32>, values: Vec<u8> },
}

#[derive(Debug, Clone)]
struct Chunk {
    base: u64,
    hits: u64,
    data: ChunkData,
}

impl Chunk {
    fn memory(&self) -> u64 {
        match &self.data {
            ChunkData::Dense { bytes, .. } => bytes.len() as u64,
            ChunkData::Masked { bytes, mask, .. } => (bytes.len() + mask.len() * 8) as u64,
            ChunkData::Sparse { offsets, values } => (offsets.len() * 4 + values.len()) as u64,
        }
    }

    /// Calls `f(address, previous value bytes)` for hits `skip..`, stopping
    /// when `f` returns `false`.
    fn visit(&self, l: Layout, skip: u64, mut f: impl FnMut(u64, &[u8]) -> bool) {
        let size = l.size as usize;
        match &self.data {
            ChunkData::Dense { bytes, .. } => {
                let first = l.first(self.base);
                for i in skip..self.hits {
                    let addr = first + i * l.align;
                    let off = (addr - self.base) as usize;
                    if !f(addr, &bytes[off..off + size]) {
                        return;
                    }
                }
            }
            ChunkData::Masked { bytes, mask, .. } => {
                let first = l.first(self.base);
                let mut skip = skip;
                for (w, &word) in mask.iter().enumerate() {
                    let ones = u64::from(word.count_ones());
                    if skip >= ones {
                        skip -= ones;
                        continue;
                    }
                    let mut bits = word;
                    while bits != 0 {
                        let b = u64::from(bits.trailing_zeros());
                        bits &= bits - 1;
                        if skip > 0 {
                            skip -= 1;
                            continue;
                        }
                        let addr = first + (w as u64 * 64 + b) * l.align;
                        let off = (addr - self.base) as usize;
                        if !f(addr, &bytes[off..off + size]) {
                            return;
                        }
                    }
                }
            }
            ChunkData::Sparse { offsets, values } => {
                for i in skip as usize..offsets.len() {
                    let addr = self.base + u64::from(offsets[i]);
                    if !f(addr, &values[i * size..(i + 1) * size]) {
                        return;
                    }
                }
            }
        }
    }

    /// `[first hit, last hit + size)`.
    fn extent(&self, l: Layout) -> Option<Range<u64>> {
        if self.hits == 0 {
            return None;
        }
        let mut first = None;
        let last = match &self.data {
            ChunkData::Dense { .. } => l.first(self.base) + (self.hits - 1) * l.align,
            ChunkData::Masked { mask, .. } => {
                let w = mask.iter().rposition(|&w| w != 0)?;
                let b = 63 - u64::from(mask[w].leading_zeros());
                l.first(self.base) + (w as u64 * 64 + b) * l.align
            }
            ChunkData::Sparse { offsets, .. } => {
                first = Some(self.base + u64::from(offsets[0]));
                self.base + u64::from(*offsets.last()?)
            }
        };
        let first = first.unwrap_or_else(|| {
            let mut f = 0;
            self.visit(l, 0, |a, _| {
                f = a;
                false
            });
            f
        });
        Some(first..last + l.size)
    }

    /// Picks the smaller of sparse and masked storage for a next-scan result.
    fn build(l: Layout, base: u64, bytes: Vec<u8>, hit_addrs: &[u64]) -> Option<Self> {
        if hit_addrs.is_empty() {
            return None;
        }
        let size = l.size as usize;
        let len = bytes.len() as u64;
        let positions = l.count(base, len, len);
        let masked_cost = len + positions.div_ceil(64) * 8;
        let sparse_cost = hit_addrs.len() as u64 * (4 + l.size);
        let hits = hit_addrs.len() as u64;
        if sparse_cost <= masked_cost {
            let mut offsets = Vec::with_capacity(hit_addrs.len());
            let mut values = Vec::with_capacity(hit_addrs.len() * size);
            for &a in hit_addrs {
                let off = (a - base) as usize;
                offsets.push(off as u32);
                values.extend_from_slice(&bytes[off..off + size]);
            }
            return Some(Self {
                base,
                hits,
                data: ChunkData::Sparse { offsets, values },
            });
        }
        let first = l.first(base);
        let mut mask = vec![0u64; positions.div_ceil(64) as usize];
        for &a in hit_addrs {
            let i = (a - first) / l.align;
            mask[(i / 64) as usize] |= 1 << (i % 64);
        }
        Some(Self {
            base,
            hits,
            data: ChunkData::Masked { bytes, mask },
        })
    }
}

// ---------------------------------------------------------------------------
// Scanner
// ---------------------------------------------------------------------------

/// Scanner configuration (PROC-06).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ScanOptions {
    pub value_type: ValueType,
    /// Alignment of candidate addresses; `None` = the value size, `Some(1)`
    /// = every byte.
    pub alignment: Option<u32>,
    /// Which regions to scan (writable only by default).
    pub regions: RegionFilter,
    pub big_endian: bool,
    /// Tolerance for float comparisons (`Equal`, `IncreasedBy`, …).
    pub float_epsilon: f64,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            value_type: ValueType::I32,
            alignment: None,
            regions: RegionFilter::default(),
            big_endian: false,
            float_epsilon: 1e-4,
        }
    }
}

/// Outcome of one scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanSummary {
    /// Number of matching addresses.
    pub count: u64,
    /// Bytes read from the target by this scan.
    pub scanned_bytes: u64,
    /// Number of scans run so far in this session.
    pub scans: u32,
    /// Bytes the stored results take.
    pub memory_bytes: u64,
}

/// One result row.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanHit {
    pub address: u64,
    /// Value seen by the last scan.
    pub value: Num,
    /// Value now, when a live source was given and the address is readable.
    pub current: Option<Num>,
}

/// A scan session: first scan, next scans, paginated results (PROC-06).
#[derive(Debug, Clone)]
pub struct Scanner {
    options: ScanOptions,
    layout: Layout,
    block_size: u64,
    chunks: Vec<Chunk>,
    /// `prefix[i]` = hits in `chunks[..i]`.
    prefix: Vec<u64>,
    scans: u32,
}

impl Scanner {
    pub fn new(options: ScanOptions) -> Result<Self> {
        let size = options.value_type.size() as u64;
        let align = u64::from(options.alignment.unwrap_or(size as u32));
        if align == 0 {
            return Err(Error::invalid("alignment must be at least 1"));
        }
        Ok(Self {
            options,
            layout: Layout { size, align },
            block_size: DEFAULT_BLOCK_SIZE,
            chunks: Vec::new(),
            prefix: vec![0],
            scans: 0,
        })
    }

    pub fn options(&self) -> &ScanOptions {
        &self.options
    }

    /// Bytes read per step (default [`DEFAULT_BLOCK_SIZE`]); smaller blocks
    /// mean finer progress and cancellation.
    pub fn set_block_size(&mut self, bytes: u64) {
        self.block_size = bytes.clamp(64, u64::from(u32::MAX) / 2);
    }

    /// Number of current results.
    pub fn count(&self) -> u64 {
        *self.prefix.last().unwrap_or(&0)
    }

    /// Scans run so far (0 = no first scan yet).
    pub fn scans(&self) -> u32 {
        self.scans
    }

    /// Bytes held by the stored results.
    pub fn memory_bytes(&self) -> u64 {
        self.chunks.iter().map(Chunk::memory).sum()
    }

    /// Forgets all results.
    pub fn reset(&mut self) {
        self.chunks.clear();
        self.prefix = vec![0];
        self.scans = 0;
    }

    fn install(&mut self, chunks: Vec<Chunk>, scanned: u64) -> ScanSummary {
        let mut prefix = Vec::with_capacity(chunks.len() + 1);
        let mut total = 0;
        prefix.push(0);
        for c in &chunks {
            total += c.hits;
            prefix.push(total);
        }
        self.chunks = chunks;
        self.prefix = prefix;
        self.scans += 1;
        ScanSummary {
            count: total,
            scanned_bytes: scanned,
            scans: self.scans,
            memory_bytes: self.memory_bytes(),
        }
    }

    /// Runs the first scan, replacing any previous results.
    pub fn first_scan(
        &mut self,
        source: &dyn MemorySource,
        condition: &FirstScan,
        cancel: &Cancel,
        progress: Progress<'_>,
    ) -> Result<ScanSummary> {
        let ranges = self.options.regions.select(&source.regions()?);
        let (chunks, scanned) = with_type!(self.options.value_type, T => {
            let pred = Pred::<T>::first(condition, self.options.value_type)?;
            self.first_impl(source, &ranges, pred, cancel, progress)?
        });
        self.scans = 0;
        Ok(self.install(chunks, scanned))
    }

    fn first_impl<T: Scalar>(
        &self,
        source: &dyn MemorySource,
        ranges: &[Range<u64>],
        pred: Pred<T>,
        cancel: &Cancel,
        progress: Progress<'_>,
    ) -> Result<(Vec<Chunk>, u64)> {
        let l = self.layout;
        let (big, eps) = (self.options.big_endian, self.options.float_epsilon);
        let total: u64 = ranges.iter().map(|r| r.end - r.start).sum();
        let mut done = 0u64;
        let mut chunks = Vec::new();
        let mut buf = Vec::new();
        progress(0, total);
        for r in ranges {
            let mut s = r.start;
            while s < r.end {
                cancel.check()?;
                let block_end = (s + self.block_size).min(r.end);
                let ext_end = (block_end + l.size - 1).min(r.end);
                buf.resize((ext_end - s) as usize, 0);
                for v in source.read_partial(s, &mut buf) {
                    let bytes = &buf[(v.start - s) as usize..(v.end - s) as usize];
                    let span = block_end.min(v.end) - v.start;
                    let len = v.end - v.start;
                    if matches!(pred, Pred::Any) {
                        let hits = l.count(v.start, len, span);
                        if hits > 0 {
                            chunks.push(Chunk {
                                base: v.start,
                                hits,
                                data: ChunkData::Dense {
                                    bytes: bytes.to_vec(),
                                },
                            });
                        }
                        continue;
                    }
                    let mut offsets = Vec::new();
                    let mut values = Vec::new();
                    let mut p = l.first(v.start);
                    while p < v.start + span && p + l.size <= v.end {
                        let off = (p - v.start) as usize;
                        let val = &bytes[off..off + l.size as usize];
                        if pred.test(val, val, big, eps) {
                            offsets.push(off as u32);
                            values.extend_from_slice(val);
                        }
                        p += l.align;
                    }
                    if !offsets.is_empty() {
                        chunks.push(Chunk {
                            base: v.start,
                            hits: offsets.len() as u64,
                            data: ChunkData::Sparse { offsets, values },
                        });
                    }
                }
                done += block_end - s;
                progress(done, total);
                s = block_end;
            }
        }
        Ok((chunks, total))
    }

    /// Filters the current results (requires a first scan).
    pub fn next_scan(
        &mut self,
        source: &dyn MemorySource,
        condition: &NextScan,
        cancel: &Cancel,
        progress: Progress<'_>,
    ) -> Result<ScanSummary> {
        if self.scans == 0 {
            return Err(Error::invalid("run a first scan before a next scan"));
        }
        let (chunks, scanned) = with_type!(self.options.value_type, T => {
            let pred = Pred::<T>::next(condition, self.options.value_type)?;
            self.next_impl(source, pred, cancel, progress)?
        });
        Ok(self.install(chunks, scanned))
    }

    fn next_impl<T: Scalar>(
        &self,
        source: &dyn MemorySource,
        pred: Pred<T>,
        cancel: &Cancel,
        progress: Progress<'_>,
    ) -> Result<(Vec<Chunk>, u64)> {
        let l = self.layout;
        let size = l.size as usize;
        let (big, eps) = (self.options.big_endian, self.options.float_epsilon);
        let total = self.count();
        let mut done = 0u64;
        let mut scanned = 0u64;
        let mut out = Vec::new();
        progress(0, total);
        for chunk in &self.chunks {
            cancel.check()?;
            let Some(ext) = chunk.extent(l) else {
                continue;
            };
            let mut cur = vec![0u8; (ext.end - ext.start) as usize];
            let valid = source.read_partial(ext.start, &mut cur);
            scanned += ext.end - ext.start;
            let mut vi = 0usize;
            let mut hits = Vec::new();
            chunk.visit(l, 0, |addr, old| {
                while vi < valid.len() && valid[vi].end < addr + l.size {
                    vi += 1;
                }
                if vi < valid.len() && valid[vi].start <= addr {
                    let off = (addr - ext.start) as usize;
                    if pred.test(&cur[off..off + size], old, big, eps) {
                        hits.push(addr);
                    }
                }
                true
            });
            if let Some(c) = Chunk::build(l, ext.start, cur, &hits) {
                out.push(c);
            }
            done += chunk.hits;
            progress(done, total);
        }
        Ok((out, scanned))
    }

    /// Results `start..start + limit`. With `live`, also reads each current
    /// value.
    pub fn page(&self, live: Option<&dyn MemorySource>, start: u64, limit: usize) -> Vec<ScanHit> {
        let mut out = Vec::with_capacity(limit.min(4096));
        if limit == 0 || start >= self.count() {
            return out;
        }
        let ty = self.options.value_type;
        let big = self.options.big_endian;
        let mut ci = self.prefix.partition_point(|&p| p <= start) - 1;
        let mut skip = start - self.prefix[ci];
        let mut cur = vec![0u8; ty.size()];
        while ci < self.chunks.len() && out.len() < limit {
            self.chunks[ci].visit(self.layout, skip, |addr, old| {
                let current = live
                    .and_then(|s| s.read(addr, &mut cur).ok())
                    .map(|()| ty.decode(&cur, big));
                out.push(ScanHit {
                    address: addr,
                    value: ty.decode(old, big),
                    current,
                });
                out.len() < limit
            });
            ci += 1;
            skip = 0;
        }
        out
    }

    /// All result addresses (e.g. to feed a pointer scan).
    pub fn addresses(&self, limit: usize) -> Vec<u64> {
        let mut out = Vec::new();
        for c in &self.chunks {
            if out.len() >= limit {
                break;
            }
            c.visit(self.layout, 0, |a, _| {
                out.push(a);
                out.len() < limit
            });
        }
        out
    }

    /// Writes `value` (encoded as the scan type) to `address`.
    pub fn write_value(&self, source: &dyn MemorySource, address: u64, value: Num) -> Result<()> {
        let bytes = self
            .options
            .value_type
            .encode(value, self.options.big_endian)?;
        source.write(address, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::region::{MemoryRegion, Protection};
    use crate::source::MemorySnapshot;
    use hexa_core::task::no_progress;

    fn snapshot(regions: &[(u64, Vec<u8>, Protection)]) -> MemorySnapshot {
        let mut s = MemorySnapshot::new("synthetic", 8);
        for (start, bytes, prot) in regions {
            s.add_region(MemoryRegion::new(*start, 0, *prot), bytes.clone())
                .unwrap();
        }
        s
    }

    fn opts(ty: ValueType) -> ScanOptions {
        ScanOptions {
            value_type: ty,
            ..ScanOptions::default()
        }
    }

    fn put<T: Scalar>(s: &MemorySnapshot, addr: u64, v: T) {
        s.write(addr, &v.store(false)).unwrap();
    }

    #[test]
    fn num_parsing_and_serde() {
        assert_eq!(Num::parse("0x10").unwrap(), Num::Int(16));
        assert_eq!(Num::parse("-0x10").unwrap(), Num::Int(-16));
        assert_eq!(Num::parse("1.5").unwrap(), Num::Float(1.5));
        assert!(Num::parse("abc").is_err());
        assert!(ValueType::U8.parse("256").is_err());
        assert!(ValueType::I32.parse("1.5").is_err());
        assert_eq!(ValueType::F32.parse("2").unwrap(), Num::Float(2.0));
        assert_eq!(
            ValueType::U64.parse("18446744073709551615").unwrap(),
            Num::Int(u64::MAX.into())
        );
        let c: NextScan = deserialize_condition("IncreasedBy", "5");
        assert_eq!(c, NextScan::IncreasedBy { value: Num::Int(5) });
        assert_eq!(
            ValueType::I16.encode(Num::Int(-2), false).unwrap(),
            vec![0xfe, 0xff]
        );
        assert_eq!(ValueType::I16.decode(&[0xfe, 0xff], false), Num::Int(-2));
    }

    /// Builds a condition through serde's value model without a JSON crate.
    fn deserialize_condition(kind: &str, value: &str) -> NextScan {
        use serde::de::value::{Error as DeError, MapDeserializer};
        let kind = match kind {
            "IncreasedBy" => "increasedBy",
            k => k,
        };
        let map = vec![("kind", kind), ("value", value)];
        let d: MapDeserializer<'_, _, DeError> = MapDeserializer::new(map.into_iter());
        NextScan::deserialize(d).unwrap()
    }

    #[test]
    fn exact_scan_respects_alignment_and_writable() {
        let s = snapshot(&[
            (0x1000, vec![0; 0x100], Protection::RW),
            (0x2000, vec![0; 0x100], Protection::R),
        ]);
        put(&s, 0x1010, 1234u32);
        put(&s, 0x1021, 1234u32); // unaligned
        put(&s, 0x2010, 1234u32); // read-only region
        let mut sc = Scanner::new(opts(ValueType::U32)).unwrap();
        let r = sc
            .first_scan(
                &s,
                &FirstScan::Exact {
                    value: Num::Int(1234),
                },
                &Cancel::new(),
                &no_progress,
            )
            .unwrap();
        assert_eq!(r.count, 1);
        assert_eq!(sc.page(None, 0, 10)[0].address, 0x1010);

        let mut o = opts(ValueType::U32);
        o.alignment = Some(1);
        o.regions = RegionFilter::all();
        let mut sc = Scanner::new(o).unwrap();
        sc.first_scan(
            &s,
            &FirstScan::Exact {
                value: Num::Int(1234),
            },
            &Cancel::new(),
            &no_progress,
        )
        .unwrap();
        assert_eq!(sc.addresses(10), vec![0x1010, 0x1021, 0x2010]);
    }

    #[test]
    fn range_scan_and_negative_values() {
        let s = snapshot(&[(0x1000, vec![0; 0x40], Protection::RW)]);
        put(&s, 0x1000, -5i16);
        put(&s, 0x1002, 7i16);
        put(&s, 0x1004, 100i16);
        let mut sc = Scanner::new(opts(ValueType::I16)).unwrap();
        let r = sc
            .first_scan(
                &s,
                &FirstScan::Range {
                    min: Num::Int(-10),
                    max: Num::Int(10),
                },
                &Cancel::new(),
                &no_progress,
            )
            .unwrap();
        // -5, 7 and the 29 zero-valued slots.
        assert_eq!(r.count, 2 + 29);
        let hits = sc.page(None, 0, 2);
        assert_eq!(hits[0].value, Num::Int(-5));
        assert_eq!(hits[1].value, Num::Int(7));
    }

    #[test]
    fn unknown_then_next_scans() {
        let s = snapshot(&[
            (0x10_000, vec![0; 0x1000], Protection::RW),
            (0x20_000, vec![0; 0x1000], Protection::RW),
        ]);
        let mut sc = Scanner::new(opts(ValueType::U32)).unwrap();
        sc.set_block_size(256);
        let c = Cancel::new();
        let r = sc
            .first_scan(&s, &FirstScan::Unknown, &c, &no_progress)
            .unwrap();
        assert_eq!(r.count, 2 * 0x1000 / 4);
        // Dense storage: no per-hit overhead.
        assert_eq!(r.memory_bytes, 2 * 0x1000 + 2 * 15 * 3);

        put(&s, 0x10_100, 10u32);
        put(&s, 0x20_ffc, 3u32);
        let r = sc
            .next_scan(&s, &NextScan::Changed, &c, &no_progress)
            .unwrap();
        assert_eq!(r.count, 2);
        put(&s, 0x10_100, 15u32);
        let r = sc
            .next_scan(&s, &NextScan::Increased, &c, &no_progress)
            .unwrap();
        assert_eq!(r.count, 1);
        put(&s, 0x10_100, 12u32);
        let r = sc
            .next_scan(
                &s,
                &NextScan::DecreasedBy { value: Num::Int(3) },
                &c,
                &no_progress,
            )
            .unwrap();
        assert_eq!(r.count, 1);
        let hit = &sc.page(Some(&s), 0, 1)[0];
        assert_eq!(hit.address, 0x10_100);
        assert_eq!(hit.value, Num::Int(12));
        put(&s, 0x10_100, 20u32);
        assert_eq!(sc.page(Some(&s), 0, 1)[0].current, Some(Num::Int(20)));
        let r = sc
            .next_scan(
                &s,
                &NextScan::IncreasedBy { value: Num::Int(8) },
                &c,
                &no_progress,
            )
            .unwrap();
        assert_eq!(r.count, 1);
        let r = sc
            .next_scan(&s, &NextScan::Unchanged, &c, &no_progress)
            .unwrap();
        assert_eq!(r.count, 1);
        let r = sc
            .next_scan(
                &s,
                &NextScan::NotEqual {
                    value: Num::Int(20),
                },
                &c,
                &no_progress,
            )
            .unwrap();
        assert_eq!(r.count, 0);
    }

    #[test]
    fn unchanged_keeps_masked_storage_and_pagination() {
        let s = snapshot(&[(0x10_000, vec![0; 0x2000], Protection::RW)]);
        let mut sc = Scanner::new(opts(ValueType::U8)).unwrap();
        sc.set_block_size(0x800);
        let c = Cancel::new();
        sc.first_scan(&s, &FirstScan::Unknown, &c, &no_progress)
            .unwrap();
        put(&s, 0x10_005, 1u8);
        let r = sc
            .next_scan(&s, &NextScan::Unchanged, &c, &no_progress)
            .unwrap();
        assert_eq!(r.count, 0x2000 - 1);
        // Masked (≈ 1 byte + 1 bit per hit) beats sparse (5 bytes per hit).
        assert!(r.memory_bytes < 0x2000 * 2, "{}", r.memory_bytes);
        let page = sc.page(None, 0x7fe, 4);
        let addrs: Vec<u64> = page.iter().map(|h| h.address).collect();
        assert_eq!(addrs, vec![0x10_7ff, 0x10_800, 0x10_801, 0x10_802]);
        assert_eq!(sc.page(None, 4, 2)[0].address, 0x10_004);
        assert_eq!(sc.page(None, 5, 1)[0].address, 0x10_006);
        assert_eq!(sc.page(None, r.count, 5).len(), 0);
    }

    #[test]
    fn values_straddling_blocks_and_floats() {
        let s = snapshot(&[(0x1000, vec![0; 0x400], Protection::RW)]);
        put(&s, 0x10fe, 2.5f32); // crosses the 0x100 block boundary
        put(&s, 0x1200, 2.50001f32);
        let mut o = opts(ValueType::F32);
        o.alignment = Some(2);
        let mut sc = Scanner::new(o).unwrap();
        sc.set_block_size(0x100);
        let r = sc
            .first_scan(
                &s,
                &FirstScan::Exact {
                    value: Num::Float(2.5),
                },
                &Cancel::new(),
                &no_progress,
            )
            .unwrap();
        assert_eq!(sc.addresses(10), vec![0x10fe, 0x1200]);
        assert_eq!(r.count, 2);
    }

    #[test]
    fn cancel_keeps_previous_results() {
        let s = snapshot(&[(0x1000, vec![1; 0x1000], Protection::RW)]);
        let mut sc = Scanner::new(opts(ValueType::U8)).unwrap();
        sc.first_scan(
            &s,
            &FirstScan::Exact { value: Num::Int(1) },
            &Cancel::new(),
            &no_progress,
        )
        .unwrap();
        let cancel = Cancel::new();
        cancel.cancel();
        let err = sc.next_scan(&s, &NextScan::Changed, &cancel, &no_progress);
        assert!(matches!(err, Err(Error::Cancelled)));
        assert_eq!(sc.count(), 0x1000);
        let seen = std::sync::Mutex::new(Vec::new());
        sc.next_scan(&s, &NextScan::Unchanged, &Cancel::new(), &|d, t| {
            seen.lock().unwrap().push((d, t));
        })
        .unwrap();
        assert_eq!(seen.lock().unwrap().last(), Some(&(0x1000, 0x1000)));
        let mut fresh = Scanner::new(opts(ValueType::U8)).unwrap();
        assert!(
            fresh
                .next_scan(&s, &NextScan::Changed, &Cancel::new(), &no_progress)
                .is_err()
        );
    }

    #[test]
    fn scans_own_process() {
        use crate::source::{Access, ProcessMemory};
        let marker = std::sync::atomic::AtomicU64::new(0x5eed_1234_abcd_9876);
        let addr = (&raw const marker) as u64;
        let mem = ProcessMemory::open_self(Access::ReadWrite).unwrap();
        let mut o = opts(ValueType::U64);
        o.regions.address_range = Some((addr - 0x1000, addr + 0x1000));
        let mut sc = Scanner::new(o).unwrap();
        let c = Cancel::new();
        sc.first_scan(
            &mem,
            &FirstScan::Exact {
                value: Num::Int(0x5eed_1234_abcd_9876),
            },
            &c,
            &no_progress,
        )
        .unwrap();
        assert!(sc.addresses(100).contains(&addr));
        marker.store(77, std::sync::atomic::Ordering::SeqCst);
        sc.next_scan(
            &mem,
            &NextScan::Equal {
                value: Num::Int(77),
            },
            &c,
            &no_progress,
        )
        .unwrap();
        assert!(sc.addresses(100).contains(&addr));
        sc.write_value(&mem, addr, Num::Int(99)).unwrap();
        assert_eq!(marker.load(std::sync::atomic::Ordering::SeqCst), 99);
    }
}
