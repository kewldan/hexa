//! Searching (SRCH-01…05, SRCH-13…17).
//!
//! Every query is compiled to a [`Matcher`] that reports all match start
//! positions, overlapping ones included. Scanning reads the snapshot in chunks
//! with an overlap of the longest possible match, so results do not depend on
//! chunk boundaries. Searches run on a [`Tree`] snapshot and never block edits.

use std::fmt::Write as _;
use std::ops::ControlFlow;

use memchr::memmem;
use regex::bytes::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use crate::encoding;
use crate::error::{Error, Result};
use crate::task::{Cancel, Progress};
use crate::tree::Tree;

const CHUNK: usize = 4 << 20;
/// Longest match a regular expression may report.
pub const REGEX_MAX_MATCH: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NumType {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    F32,
    F64,
}

impl NumType {
    fn width(self) -> usize {
        match self {
            Self::I8 | Self::U8 => 1,
            Self::I16 | Self::U16 => 2,
            Self::I32 | Self::U32 | Self::F32 => 4,
            Self::I64 | Self::U64 | Self::F64 => 8,
        }
    }

    fn is_float(self) -> bool {
        matches!(self, Self::F32 | Self::F64)
    }

    fn is_signed(self) -> bool {
        matches!(self, Self::I8 | Self::I16 | Self::I32 | Self::I64)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EndianSel {
    Little,
    Big,
    Both,
}

/// What to search for.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Query {
    /// Hex bytes with `??` / `4?` wildcards and `{01xx0101}` bit masks (SRCH-01, 03).
    Hex { pattern: String },
    /// Text in one or more encodings at once (SRCH-02).
    Text {
        text: String,
        encodings: Vec<String>,
        case_sensitive: bool,
    },
    /// Regular expression over bytes, or over UTF-8 text when `unicode` (SRCH-04).
    Regex { pattern: String, unicode: bool },
    /// A number, or a range `value..=max`, with a float tolerance (SRCH-05).
    #[serde(rename_all = "camelCase")]
    Number {
        ty: NumType,
        endian: EndianSel,
        value: String,
        max: Option<String>,
        tolerance: Option<f64>,
    },
    /// Consecutive typed values, e.g. "u32 = 0x10, then u16 in 1..=5" (SRCH-06).
    Sequence { items: Vec<SeqItem> },
}

/// One element of a [`Query::Sequence`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeqItem {
    pub ty: NumType,
    pub big_endian: bool,
    /// `None` = any value (wildcard of the type's width).
    pub value: Option<String>,
    pub max: Option<String>,
    pub tolerance: Option<f64>,
}

/// One search hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Match {
    pub offset: u64,
    pub len: u64,
}

#[derive(Debug)]
pub enum Matcher {
    /// Boxed: on `x86_64` the SIMD searcher makes `Finder` ~300 bytes.
    Literal {
        finder: Box<memmem::Finder<'static>>,
    },
    Regex {
        re: Regex,
        max_len: usize,
    },
    Numeric(Numeric),
    Sequence(Vec<Numeric>),
}

#[derive(Debug)]
pub struct Numeric {
    ty: NumType,
    /// Wildcard element of a sequence: any value matches.
    any: bool,
    endians: Vec<bool>, // true = big endian
    lo: f64,
    hi: f64,
    int_lo: i128,
    int_hi: i128,
}

impl Numeric {
    fn matches(&self, bytes: &[u8]) -> bool {
        let w = self.ty.width();
        if bytes.len() < w {
            return false;
        }
        self.endians.iter().any(|&be| {
            let mut a = [0u8; 8];
            a[..w].copy_from_slice(&bytes[..w]);
            if be {
                a[..w].reverse();
            }
            let raw = u64::from_le_bytes(a);
            match self.ty {
                NumType::F32 => {
                    let v = f64::from(f32::from_bits(raw as u32));
                    v >= self.lo && v <= self.hi
                }
                NumType::F64 => {
                    let v = f64::from_bits(raw);
                    v >= self.lo && v <= self.hi
                }
                t => {
                    let shift = 64 - w * 8;
                    let v = if t.is_signed() {
                        i128::from(((raw << shift) as i64) >> shift)
                    } else {
                        i128::from(raw)
                    };
                    v >= self.int_lo && v <= self.int_hi
                }
            }
        })
    }
}

impl Matcher {
    /// First match starting at or after `at`: `(start, len)`.
    pub fn find_at(&self, hay: &[u8], at: usize) -> Option<(usize, usize)> {
        if at > hay.len() {
            return None;
        }
        match self {
            Self::Literal { finder } => {
                let n = finder.needle().len();
                finder.find(&hay[at..]).map(|i| (at + i, n))
            }
            Self::Regex { re, .. } => re.find_at(hay, at).map(|m| (m.start(), m.len())),
            Self::Numeric(n) => {
                let w = n.ty.width();
                (at..hay.len().saturating_sub(w - 1))
                    .find(|&i| n.matches(&hay[i..]))
                    .map(|i| (i, w))
            }
            Self::Sequence(items) => {
                let w: usize = items.iter().map(|n| n.ty.width()).sum();
                (at..(hay.len() + 1).saturating_sub(w))
                    .find(|&i| {
                        let mut o = i;
                        items.iter().all(|n| {
                            let ok = n.any || n.matches(&hay[o..]);
                            o += n.ty.width();
                            ok
                        })
                    })
                    .map(|i| (i, w))
            }
        }
    }

    pub fn max_len(&self) -> usize {
        match self {
            Self::Literal { finder } => finder.needle().len(),
            Self::Regex { max_len, .. } => *max_len,
            Self::Numeric(n) => n.ty.width(),
            Self::Sequence(items) => items.iter().map(|n| n.ty.width()).sum(),
        }
    }
}

fn hex_val(c: char) -> Option<u8> {
    c.to_digit(16).map(|d| d as u8)
}

/// Parses a hex pattern into `(value, mask)` pairs.
pub fn parse_hex_pattern(s: &str) -> Result<Vec<(u8, u8)>> {
    let mut out = Vec::new();
    let mut chars = s
        .chars()
        .filter(|c| !c.is_whitespace() && *c != ',')
        .peekable();
    while let Some(c) = chars.next() {
        if c == '{' {
            let bits: String = chars.by_ref().take_while(|&c| c != '}').collect();
            if bits.len() != 8 {
                return Err(Error::invalid("a bit mask needs 8 digits: {01xx0101}"));
            }
            let (mut v, mut m) = (0u8, 0u8);
            for b in bits.chars() {
                v <<= 1;
                m <<= 1;
                match b {
                    '0' => m |= 1,
                    '1' => {
                        v |= 1;
                        m |= 1;
                    }
                    'x' | 'X' | '?' => {}
                    _ => return Err(Error::invalid(format!("bad bit '{b}' in mask"))),
                }
            }
            out.push((v, m));
            continue;
        }
        if c == '0' && matches!(chars.peek(), Some('x' | 'X')) {
            chars.next();
            continue;
        }
        let d = chars
            .next()
            .ok_or_else(|| Error::invalid("odd number of hex digits"))?;
        let nib = |c: char| -> Result<(u8, u8)> {
            if c == '?' {
                Ok((0, 0))
            } else {
                hex_val(c)
                    .map(|v| (v, 0xF))
                    .ok_or_else(|| Error::invalid(format!("'{c}' is not a hex digit")))
            }
        };
        let (hv, hm) = nib(c)?;
        let (lv, lm) = nib(d)?;
        out.push(((hv << 4) | lv, (hm << 4) | lm));
    }
    if out.is_empty() {
        return Err(Error::invalid("empty pattern"));
    }
    Ok(out)
}

fn push_byte(re: &mut String, b: u8) {
    let _ = write!(re, "\\x{b:02X}");
}

fn push_masked(re: &mut String, v: u8, m: u8) {
    match m {
        0xFF => push_byte(re, v),
        0 => re.push_str("[\\x00-\\xFF]"),
        _ => {
            re.push('[');
            for b in 0..=255u8 {
                if b & m == v & m {
                    push_byte(re, b);
                }
            }
            re.push(']');
        }
    }
}

fn bytes_regex(pattern: &str, max_len: usize) -> Result<Matcher> {
    let re = RegexBuilder::new(pattern)
        .unicode(false)
        .dot_matches_new_line(true)
        .size_limit(64 << 20)
        .build()
        .map_err(|e| Error::invalid(e.to_string()))?;
    Ok(Matcher::Regex { re, max_len })
}

fn literal(bytes: &[u8]) -> Matcher {
    Matcher::Literal {
        finder: Box::new(memmem::Finder::new(bytes).into_owned()),
    }
}

fn parse_number(s: &str, ty: NumType) -> Result<(f64, i128)> {
    let t = s.trim().replace('_', "");
    if ty.is_float() {
        let v: f64 = t
            .parse()
            .map_err(|_| Error::invalid(format!("'{s}' is not a number")))?;
        Ok((v, 0))
    } else {
        let (neg, body) = t
            .strip_prefix('-')
            .map_or((false, t.as_str()), |r| (true, r));
        let v = if let Some(h) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
            i128::from_str_radix(h, 16)
        } else {
            body.parse::<i128>()
        }
        .map_err(|_| Error::invalid(format!("'{s}' is not an integer")))?;
        let v = if neg { -v } else { v };
        Ok((v as f64, v))
    }
}

fn int_bounds(ty: NumType) -> (i128, i128) {
    let bits = ty.width() as u32 * 8;
    if ty.is_signed() {
        (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
    } else {
        (0, (1i128 << bits) - 1)
    }
}

/// Compiles a query.
pub fn compile(q: &Query) -> Result<Matcher> {
    match q {
        Query::Hex { pattern } => {
            let parsed = parse_hex_pattern(pattern)?;
            if parsed.iter().all(|&(_, m)| m == 0xFF) {
                let bytes: Vec<u8> = parsed.iter().map(|&(v, _)| v).collect();
                return Ok(literal(&bytes));
            }
            let mut re = String::new();
            for &(v, m) in &parsed {
                push_masked(&mut re, v, m);
            }
            bytes_regex(&re, parsed.len())
        }
        Query::Text {
            text,
            encodings,
            case_sensitive,
        } => {
            if text.is_empty() {
                return Err(Error::invalid("empty text"));
            }
            let encs: Vec<&str> = if encodings.is_empty() {
                vec!["utf-8"]
            } else {
                encodings.iter().map(String::as_str).collect()
            };
            if *case_sensitive && encs.len() == 1 {
                return Ok(literal(&encoding::encode(text, encs[0])?));
            }
            let mut alternatives = Vec::new();
            let mut max_len = 0;
            for enc in encs {
                let mut seq = String::new();
                let mut len = 0;
                let mut ok = true;
                for c in text.chars() {
                    let mut variants = vec![c];
                    if !case_sensitive {
                        variants.extend(c.to_lowercase().take(1));
                        variants.extend(c.to_uppercase().take(1));
                    }
                    variants.sort_unstable();
                    variants.dedup();
                    let encoded: Vec<Vec<u8>> = variants
                        .iter()
                        .filter_map(|v| encoding::encode(&v.to_string(), enc).ok())
                        .collect();
                    if encoded.is_empty() {
                        ok = false;
                        break;
                    }
                    len += encoded.iter().map(Vec::len).max().unwrap_or(0);
                    seq.push_str("(?:");
                    for (i, e) in encoded.iter().enumerate() {
                        if i > 0 {
                            seq.push('|');
                        }
                        for &b in e {
                            push_byte(&mut seq, b);
                        }
                    }
                    seq.push(')');
                }
                if ok {
                    alternatives.push(seq);
                    max_len = max_len.max(len);
                }
            }
            if alternatives.is_empty() {
                return Err(Error::invalid(
                    "text cannot be encoded in the chosen encodings",
                ));
            }
            bytes_regex(&alternatives.join("|"), max_len)
        }
        Query::Regex { pattern, unicode } => {
            let re = RegexBuilder::new(pattern)
                .unicode(*unicode)
                .dot_matches_new_line(!*unicode)
                .size_limit(64 << 20)
                .build()
                .map_err(|e| Error::invalid(e.to_string()))?;
            Ok(Matcher::Regex {
                re,
                max_len: REGEX_MAX_MATCH,
            })
        }
        Query::Sequence { items } => {
            if items.is_empty() {
                return Err(Error::invalid("empty sequence"));
            }
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                let Some(value) = it.value.as_deref().filter(|v| !v.trim().is_empty()) else {
                    out.push(Numeric {
                        ty: it.ty,
                        any: true,
                        endians: vec![it.big_endian],
                        lo: 0.0,
                        hi: 0.0,
                        int_lo: 0,
                        int_hi: 0,
                    });
                    continue;
                };
                let (lo, ilo) = parse_number(value, it.ty)?;
                let (hi, ihi) = match it.max.as_deref() {
                    Some(m) if !m.trim().is_empty() => parse_number(m, it.ty)?,
                    _ => (lo, ilo),
                };
                let tol = it.tolerance.unwrap_or(0.0).abs();
                out.push(Numeric {
                    ty: it.ty,
                    any: false,
                    endians: vec![it.big_endian],
                    lo: lo.min(hi) - tol,
                    hi: hi.max(lo) + tol,
                    int_lo: ilo.min(ihi),
                    int_hi: ihi.max(ilo),
                });
            }
            Ok(Matcher::Sequence(out))
        }
        Query::Number {
            ty,
            endian,
            value,
            max,
            tolerance,
        } => {
            let (lo, ilo) = parse_number(value, *ty)?;
            let (hi, ihi) = match max {
                Some(m) if !m.trim().is_empty() => parse_number(m, *ty)?,
                _ => (lo, ilo),
            };
            let tol = tolerance.unwrap_or(0.0).abs();
            let endians = match endian {
                EndianSel::Little => vec![false],
                EndianSel::Big => vec![true],
                EndianSel::Both => vec![false, true],
            };
            if !ty.is_float() {
                let (min, maxv) = int_bounds(*ty);
                if ilo > ihi || ihi < min || ilo > maxv {
                    return Err(Error::invalid("value out of range for the type"));
                }
                if ilo == ihi {
                    // Exact integer: plain byte patterns are much faster.
                    let w = ty.width();
                    let mut pats: Vec<Vec<u8>> = endians
                        .iter()
                        .map(|&be| {
                            let mut b = (ilo as u128).to_le_bytes()[..w].to_vec();
                            if be {
                                b.reverse();
                            }
                            b
                        })
                        .collect();
                    pats.dedup();
                    if pats.len() == 1 {
                        return Ok(literal(&pats[0]));
                    }
                    let mut re = String::new();
                    for (i, p) in pats.iter().enumerate() {
                        if i > 0 {
                            re.push('|');
                        }
                        for &b in p {
                            push_byte(&mut re, b);
                        }
                    }
                    return bytes_regex(&re, w);
                }
            }
            Ok(Matcher::Numeric(Numeric {
                ty: *ty,
                any: false,
                endians,
                lo: lo.min(hi) - tol,
                hi: hi.max(lo) + tol,
                int_lo: ilo.min(ihi),
                int_hi: ihi.max(ilo),
            }))
        }
    }
}

/// Where and how to search (SRCH-13, SRCH-14).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scope {
    pub start: u64,
    pub end: u64,
    /// Only report matches at offsets that are multiples of this.
    pub align: u64,
}

impl Scope {
    pub fn whole(tree: &Tree) -> Self {
        Self {
            start: 0,
            end: tree.len(),
            align: 1,
        }
    }

    fn clamp(self, tree: &Tree) -> Self {
        let end = self.end.min(tree.len());
        Self {
            start: self.start.min(end),
            end,
            align: self.align.max(1),
        }
    }
}

/// Visits matches starting in `[from, to)`, forward.
fn scan_forward(
    tree: &Tree,
    m: &Matcher,
    from: u64,
    to: u64,
    limit: u64,
    align: u64,
    cancel: &Cancel,
    progress: Progress<'_>,
    mut on: impl FnMut(Match) -> ControlFlow<()>,
) -> Result<()> {
    let overlap = m.max_len().saturating_sub(1) as u64;
    let total = to.saturating_sub(from);
    let mut pos = from;
    let mut buf = Vec::new();
    while pos < to {
        cancel.check()?;
        let chunk_end = (pos + CHUNK as u64).min(to);
        let read_end = (chunk_end + overlap).min(limit);
        buf.resize((read_end - pos) as usize, 0);
        tree.read(pos, &mut buf)?;
        let report_until = (chunk_end - pos) as usize;
        let mut at = 0;
        while let Some((s, len)) = m.find_at(&buf, at) {
            if s >= report_until {
                break;
            }
            let offset = pos + s as u64;
            if offset.is_multiple_of(align) {
                let hit = Match {
                    offset,
                    len: len as u64,
                };
                if on(hit).is_break() {
                    return Ok(());
                }
            }
            at = s + 1;
        }
        pos = chunk_end;
        progress(pos - from, total);
    }
    Ok(())
}

/// Last match starting in `[from, to)`.
fn scan_backward(
    tree: &Tree,
    m: &Matcher,
    from: u64,
    to: u64,
    limit: u64,
    align: u64,
    cancel: &Cancel,
) -> Result<Option<Match>> {
    let overlap = m.max_len().saturating_sub(1) as u64;
    let mut end = to;
    let mut buf = Vec::new();
    while end > from {
        cancel.check()?;
        let start = end.saturating_sub(CHUNK as u64).max(from);
        let read_end = (end + overlap).min(limit);
        buf.resize((read_end - start) as usize, 0);
        tree.read(start, &mut buf)?;
        let report_until = (end - start) as usize;
        let mut last = None;
        let mut at = 0;
        while let Some((s, len)) = m.find_at(&buf, at) {
            if s >= report_until {
                break;
            }
            let offset = start + s as u64;
            if offset.is_multiple_of(align) {
                last = Some(Match {
                    offset,
                    len: len as u64,
                });
            }
            at = s + 1;
        }
        if last.is_some() {
            return Ok(last);
        }
        end = start;
    }
    Ok(None)
}

/// Finds the next (or previous) match relative to `cursor` (SRCH-13).
///
/// Forward: first match starting after `cursor` when `skip_current`, else at
/// or after it. Backward: last match starting before `cursor`.
pub fn find_next(
    tree: &Tree,
    matcher: &Matcher,
    scope: Scope,
    cursor: u64,
    backwards: bool,
    wrap: bool,
    cancel: &Cancel,
) -> Result<Option<Match>> {
    let s = scope.clamp(tree);
    let cursor = cursor.clamp(s.start, s.end);
    let mut found = None;
    if backwards {
        if let Some(m) = scan_backward(tree, matcher, s.start, cursor, s.end, s.align, cancel)? {
            return Ok(Some(m));
        }
        if wrap {
            return scan_backward(tree, matcher, cursor, s.end, s.end, s.align, cancel);
        }
        return Ok(None);
    }
    scan_forward(
        tree,
        matcher,
        cursor,
        s.end,
        s.end,
        s.align,
        cancel,
        &crate::task::no_progress,
        |m| {
            found = Some(m);
            ControlFlow::Break(())
        },
    )?;
    if found.is_none() && wrap {
        scan_forward(
            tree,
            matcher,
            s.start,
            cursor,
            s.end,
            s.align,
            cancel,
            &crate::task::no_progress,
            |m| {
                found = Some(m);
                ControlFlow::Break(())
            },
        )?;
    }
    Ok(found)
}

/// Streams every match in the scope (SRCH-16, SRCH-17). Stops after `max`.
/// Returns the number of matches reported.
pub fn find_all(
    tree: &Tree,
    matcher: &Matcher,
    scope: Scope,
    max: usize,
    cancel: &Cancel,
    progress: Progress<'_>,
    mut on: impl FnMut(Match),
) -> Result<usize> {
    let s = scope.clamp(tree);
    let mut n = 0;
    scan_forward(
        tree,
        matcher,
        s.start,
        s.end,
        s.end,
        s.align,
        cancel,
        progress,
        |m| {
            on(m);
            n += 1;
            if n >= max {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        },
    )?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::piece::Piece;

    fn tree(data: &[u8]) -> Tree {
        Tree::from_piece(Piece::bytes(data.to_vec()))
    }

    fn all(t: &Tree, q: &Query) -> Vec<u64> {
        let m = compile(q).unwrap();
        let mut v = Vec::new();
        find_all(
            t,
            &m,
            Scope::whole(t),
            usize::MAX,
            &Cancel::new(),
            &crate::task::no_progress,
            |x| {
                v.push(x.offset);
            },
        )
        .unwrap();
        v
    }

    fn hex(p: &str) -> Query {
        Query::Hex { pattern: p.into() }
    }

    #[test]
    fn hex_patterns_and_wildcards() {
        let t = tree(&[0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0xDE, 0xAF, 0xBE, 0x4F]);
        assert_eq!(all(&t, &hex("de ad")), [0]);
        assert_eq!(all(&t, &hex("DE ?? BE")), [0, 5]);
        assert_eq!(all(&t, &hex("DE A?")), [0, 5]);
        assert_eq!(all(&t, &hex("{01xx1111}")), [8]);
        assert_eq!(all(&t, &hex("0xBE,0xEF")), [2]);
        assert!(compile(&hex("ABC")).is_err());
    }

    #[test]
    fn overlapping_and_across_chunks() {
        let t = tree(b"AAAA");
        assert_eq!(all(&t, &hex("41 41")), [0, 1, 2]);
        let mut big = vec![0u8; CHUNK * 2 + 10];
        big[CHUNK - 1] = 0x12;
        big[CHUNK] = 0x34;
        big[CHUNK * 2 + 5] = 0x12;
        big[CHUNK * 2 + 6] = 0x34;
        let t = tree(&big);
        assert_eq!(
            all(&t, &hex("1234")),
            [CHUNK as u64 - 1, CHUNK as u64 * 2 + 5]
        );
    }

    #[test]
    fn text_in_several_encodings_case_insensitive() {
        let mut data = b"xx Hello ".to_vec();
        data.extend(encoding::encode("HELLO", "utf-16le").unwrap());
        data.extend(encoding::encode(" привет", "windows-1251").unwrap());
        let t = tree(&data);
        let q = Query::Text {
            text: "hello".into(),
            encodings: vec!["ascii".into(), "utf-16le".into()],
            case_sensitive: false,
        };
        assert_eq!(all(&t, &q), [3, 9]);
        let q = Query::Text {
            text: "ПРИВЕТ".into(),
            encodings: vec!["windows-1251".into()],
            case_sensitive: false,
        };
        assert_eq!(all(&t, &q).len(), 1);
    }

    #[test]
    fn numbers() {
        let mut data = Vec::new();
        data.extend(1000u32.to_le_bytes());
        data.extend(1000u32.to_be_bytes());
        data.extend(2.5f32.to_le_bytes());
        let t = tree(&data);
        let q = |value: &str, max: Option<&str>, endian, ty, tol| Query::Number {
            ty,
            endian,
            value: value.into(),
            max: max.map(Into::into),
            tolerance: tol,
        };
        assert_eq!(
            all(&t, &q("1000", None, EndianSel::Little, NumType::U32, None)),
            [0]
        );
        assert_eq!(
            all(&t, &q("1000", None, EndianSel::Both, NumType::U32, None)),
            [0, 4]
        );
        assert_eq!(
            all(
                &t,
                &q("2.4", None, EndianSel::Little, NumType::F32, Some(0.2))
            ),
            [8]
        );
        let r = all(
            &t,
            &q("999", Some("1001"), EndianSel::Little, NumType::U16, None),
        );
        assert_eq!(r, [0]);
    }

    #[test]
    fn regex_and_next_prev() {
        let t = tree(b"id=12; id=345; id=6");
        let q = Query::Regex {
            pattern: r"id=\d+".into(),
            unicode: false,
        };
        let m = compile(&q).unwrap();
        let c = Cancel::new();
        let s = Scope::whole(&t);
        assert_eq!(
            find_next(&t, &m, s, 1, false, false, &c)
                .unwrap()
                .unwrap()
                .offset,
            7
        );
        assert_eq!(
            find_next(&t, &m, s, 16, false, true, &c)
                .unwrap()
                .unwrap()
                .offset,
            0
        );
        assert_eq!(
            find_next(&t, &m, s, 7, true, false, &c)
                .unwrap()
                .unwrap()
                .offset,
            0
        );
        assert_eq!(
            find_next(&t, &m, s, 0, true, true, &c)
                .unwrap()
                .unwrap()
                .offset,
            15
        );
        assert!(find_next(&t, &m, s, 0, true, false, &c).unwrap().is_none());
    }

    #[test]
    fn typed_sequences() {
        let mut data = vec![0xAA];
        data.extend(0x10u32.to_le_bytes());
        data.extend(3u16.to_le_bytes());
        data.extend(0x10u32.to_le_bytes());
        data.extend(9u16.to_le_bytes());
        let t = tree(&data);
        let q = Query::Sequence {
            items: vec![
                SeqItem {
                    ty: NumType::U32,
                    big_endian: false,
                    value: Some("0x10".into()),
                    max: None,
                    tolerance: None,
                },
                SeqItem {
                    ty: NumType::U16,
                    big_endian: false,
                    value: Some("1".into()),
                    max: Some("5".into()),
                    tolerance: None,
                },
            ],
        };
        assert_eq!(all(&t, &q), [1]);
    }

    #[test]
    fn replacements_with_captures() {
        let t = tree(b"id=12; id=345;");
        let m = compile(&Query::Regex {
            pattern: r"id=(\d+)".into(),
            unicode: false,
        })
        .unwrap();
        let plan = plan_replacements(
            &t,
            &m,
            Scope::whole(&t),
            &Replacement::Template {
                template: b"<$1>".to_vec(),
            },
            100,
            &Cancel::new(),
        )
        .unwrap();
        let news: Vec<_> = plan.iter().map(|p| (p.offset, p.new.clone())).collect();
        assert_eq!(news, vec![(0, b"<12>".to_vec()), (7, b"<345>".to_vec())]);
        // Overlapping matches are skipped.
        let t = tree(b"AAAA");
        let m = compile(&hex("41 41")).unwrap();
        let plan = plan_replacements(
            &t,
            &m,
            Scope::whole(&t),
            &Replacement::Bytes { bytes: vec![0x42] },
            100,
            &Cancel::new(),
        )
        .unwrap();
        assert_eq!(plan.iter().map(|p| p.offset).collect::<Vec<_>>(), [0, 2]);
    }

    #[test]
    fn alignment_and_scope() {
        let t = tree(&[1, 1, 1, 1, 1, 1, 1, 1]);
        let m = compile(&hex("01")).unwrap();
        let mut v = Vec::new();
        let scope = Scope {
            start: 1,
            end: 7,
            align: 2,
        };
        find_all(
            &t,
            &m,
            scope,
            100,
            &Cancel::new(),
            &crate::task::no_progress,
            |x| v.push(x.offset),
        )
        .unwrap();
        assert_eq!(v, [2, 4, 6]);
    }
}

/// What to put in place of each match (SRCH-20, SRCH-21).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Replacement {
    /// Fixed bytes (may differ in length from the match).
    Bytes { bytes: Vec<u8> },
    /// For regex queries: `$0`, `$1`…`$9` and `${name}` expand to capture
    /// groups; other bytes are literal (`$$` is a dollar sign).
    Template { template: Vec<u8> },
}

/// A planned replacement, shown to the user before applying (SRCH-22).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Planned {
    pub offset: u64,
    pub len: u64,
    pub old: Vec<u8>,
    pub new: Vec<u8>,
}

fn expand_template(template: &[u8], caps: &regex::bytes::Captures<'_>) -> Vec<u8> {
    let mut out = Vec::with_capacity(template.len());
    let mut i = 0;
    while i < template.len() {
        let b = template[i];
        if b != b'$' || i + 1 >= template.len() {
            out.push(b);
            i += 1;
            continue;
        }
        let next = template[i + 1];
        if next == b'$' {
            out.push(b'$');
            i += 2;
        } else if next.is_ascii_digit() {
            if let Some(m) = caps.get(usize::from(next - b'0')) {
                out.extend_from_slice(m.as_bytes());
            }
            i += 2;
        } else if next == b'{' {
            let end = template[i + 2..].iter().position(|&c| c == b'}');
            if let Some(e) = end {
                let name = String::from_utf8_lossy(&template[i + 2..i + 2 + e]);
                let m = name
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| caps.get(n))
                    .or_else(|| caps.name(&name));
                if let Some(m) = m {
                    out.extend_from_slice(m.as_bytes());
                }
                i += e + 3;
            } else {
                out.push(b);
                i += 1;
            }
        } else {
            out.push(b);
            i += 1;
        }
    }
    out
}

/// Plans non-overlapping replacements of every match in the scope, at most
/// `max`. Nothing is changed; the caller applies them as one transaction.
pub fn plan_replacements(
    tree: &Tree,
    matcher: &Matcher,
    scope: Scope,
    replacement: &Replacement,
    max: usize,
    cancel: &Cancel,
) -> Result<Vec<Planned>> {
    let mut hits = Vec::new();
    let mut next_free = 0u64;
    find_all(
        tree,
        matcher,
        scope,
        usize::MAX,
        cancel,
        &crate::task::no_progress,
        |m| {
            if m.offset >= next_free && hits.len() < max {
                next_free = m.offset + m.len.max(1);
                hits.push(m);
            }
        },
    )?;
    hits.truncate(max);
    let mut out = Vec::with_capacity(hits.len());
    for m in hits {
        cancel.check()?;
        let old = tree.read_vec(m.offset, m.len as usize)?;
        let new = match (replacement, matcher) {
            (Replacement::Bytes { bytes }, _) => bytes.clone(),
            (Replacement::Template { template }, Matcher::Regex { re, .. }) => {
                match re.captures(&old) {
                    Some(c) => expand_template(template, &c),
                    None => template.clone(),
                }
            }
            (Replacement::Template { template }, _) => template.clone(),
        };
        out.push(Planned {
            offset: m.offset,
            len: m.len,
            old,
            new,
        });
    }
    Ok(out)
}
