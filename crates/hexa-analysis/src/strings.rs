//! Strings extraction (ANL-11): ASCII, UTF-8 and UTF-16LE/BE runs streamed
//! over a snapshot, with a minimum length, result cap, substring / regex
//! filters and CSV / text export.

use std::fmt::Write as _;

use hexa_core::task::Progress;
use hexa_core::{Cancel, Result, Tree};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::util::{self, CHUNK, invalid};

/// Encodings the scanner understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StringEncoding {
    Ascii,
    Utf8,
    Utf16Le,
    Utf16Be,
}

/// Options of [`extract_strings`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StringsOptions {
    pub encodings: Vec<StringEncoding>,
    /// Minimum length in characters.
    pub min_len: usize,
    /// Stop after this many strings (the result is then `truncated`).
    pub max_results: usize,
    /// Longer strings are reported with their full byte length but the text
    /// is cut to this many characters.
    pub max_text_len: usize,
    /// Allow `\t`, `\n`, `\r` inside strings (space is always allowed).
    pub include_whitespace: bool,
    /// UTF-16: accept any script and surrogate pairs; by default only
    /// ASCII, Latin-1, Latin Extended-A, Greek and Cyrillic (fewer false
    /// positives in binary data).
    pub utf16_any_script: bool,
    /// Keep strings containing this text (case-insensitive).
    pub contains: Option<String>,
    /// Keep strings matching this regular expression.
    pub regex: Option<String>,
}

impl Default for StringsOptions {
    fn default() -> Self {
        Self {
            encodings: vec![
                StringEncoding::Ascii,
                StringEncoding::Utf8,
                StringEncoding::Utf16Le,
                StringEncoding::Utf16Be,
            ],
            min_len: 4,
            max_results: 100_000,
            max_text_len: 4096,
            include_whitespace: false,
            utf16_any_script: false,
            contains: None,
            regex: None,
        }
    }
}

/// One extracted string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FoundString {
    pub offset: u64,
    /// Length in bytes.
    pub len: u64,
    pub encoding: StringEncoding,
    pub text: String,
}

/// Result of [`extract_strings`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StringsResult {
    /// Sorted by offset.
    pub strings: Vec<FoundString>,
    /// `true` when the scan stopped at `max_results`.
    pub truncated: bool,
}

#[derive(Debug)]
struct Run {
    start: u64,
    bytes: u64,
    chars: usize,
    text: String,
    non_ascii: bool,
}

impl Run {
    fn new() -> Self {
        Self {
            start: 0,
            bytes: 0,
            chars: 0,
            text: String::new(),
            non_ascii: false,
        }
    }

    fn push(&mut self, at: u64, c: char, nbytes: u64, max_text: usize) {
        if self.chars == 0 {
            self.start = at;
        }
        self.bytes += nbytes;
        self.chars += 1;
        self.non_ascii |= !c.is_ascii();
        if self.chars <= max_text {
            self.text.push(c);
        }
    }
}

#[derive(Debug)]
struct Ctx<'a> {
    opts: &'a StringsOptions,
    out: Vec<FoundString>,
    need_non_ascii_utf8: bool,
}

impl Ctx<'_> {
    fn end(&mut self, run: &mut Run, enc: StringEncoding) {
        if run.chars >= self.opts.min_len.max(1)
            && (enc != StringEncoding::Utf8 || !self.need_non_ascii_utf8 || run.non_ascii)
        {
            self.out.push(FoundString {
                offset: run.start,
                len: run.bytes,
                encoding: enc,
                text: std::mem::take(&mut run.text),
            });
        }
        *run = Run::new();
    }

    fn ascii_ok(&self, b: u8) -> bool {
        (0x20..0x7F).contains(&b)
            || (self.opts.include_whitespace && matches!(b, b'\t' | b'\n' | b'\r'))
    }

    fn char_ok(&self, c: char) -> bool {
        if c.is_ascii() {
            return self.ascii_ok(c as u8);
        }
        !c.is_control() && !matches!(u32::from(c), 0xE000..=0xF8FF | 0xFFF0..=0xFFFF)
    }

    fn utf16_ok(&self, c: char) -> bool {
        if c.is_ascii() {
            return self.ascii_ok(c as u8);
        }
        if self.opts.utf16_any_script {
            return self.char_ok(c);
        }
        matches!(u32::from(c), 0xA0..=0x017F | 0x0370..=0x04FF) && self.char_ok(c)
    }
}

#[derive(Debug)]
struct AsciiScan(Run);

impl AsciiScan {
    fn feed(&mut self, ctx: &mut Ctx<'_>, at: u64, b: u8) {
        if ctx.ascii_ok(b) {
            self.0.push(at, b as char, 1, ctx.opts.max_text_len);
        } else {
            ctx.end(&mut self.0, StringEncoding::Ascii);
        }
    }
}

#[derive(Debug)]
struct Utf8Scan {
    run: Run,
    seq: [u8; 4],
    have: usize,
    need: usize,
    seq_start: u64,
}

impl Utf8Scan {
    fn feed(&mut self, ctx: &mut Ctx<'_>, at: u64, b: u8) {
        if self.need > 0 {
            if (0x80..0xC0).contains(&b) {
                self.seq[self.have] = b;
                self.have += 1;
                if self.have == self.need {
                    let n = self.need;
                    self.need = 0;
                    match std::str::from_utf8(&self.seq[..n])
                        .ok()
                        .and_then(|s| s.chars().next())
                    {
                        Some(c) if ctx.char_ok(c) => {
                            self.run
                                .push(self.seq_start, c, n as u64, ctx.opts.max_text_len);
                        }
                        _ => ctx.end(&mut self.run, StringEncoding::Utf8),
                    }
                }
                return;
            }
            self.need = 0;
            ctx.end(&mut self.run, StringEncoding::Utf8);
        }
        let need = match b {
            0x00..=0x7F => {
                if ctx.ascii_ok(b) {
                    self.run.push(at, b as char, 1, ctx.opts.max_text_len);
                } else {
                    ctx.end(&mut self.run, StringEncoding::Utf8);
                }
                return;
            }
            0xC2..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF4 => 4,
            _ => {
                ctx.end(&mut self.run, StringEncoding::Utf8);
                return;
            }
        };
        self.seq[0] = b;
        self.have = 1;
        self.need = need;
        self.seq_start = at;
    }
}

#[derive(Debug)]
struct Utf16Scan {
    run: Run,
    big_endian: bool,
    parity: u64,
    first: Option<(u64, u8)>,
    high: Option<(u64, u16)>,
}

impl Utf16Scan {
    fn enc(&self) -> StringEncoding {
        if self.big_endian {
            StringEncoding::Utf16Be
        } else {
            StringEncoding::Utf16Le
        }
    }

    fn feed(&mut self, ctx: &mut Ctx<'_>, at: u64, b: u8) {
        if at % 2 == self.parity {
            self.first = Some((at, b));
            return;
        }
        let Some((unit_at, a)) = self.first.take() else {
            return;
        };
        let unit = if self.big_endian {
            u16::from_be_bytes([a, b])
        } else {
            u16::from_le_bytes([a, b])
        };
        let enc = self.enc();
        if let Some((hi_at, hi)) = self.high.take() {
            if (0xDC00..0xE000).contains(&unit) {
                let cp = 0x10000 + ((u32::from(hi) - 0xD800) << 10) + (u32::from(unit) - 0xDC00);
                if let Some(c) = char::from_u32(cp).filter(|&c| ctx.utf16_ok(c)) {
                    self.run.push(hi_at, c, 4, ctx.opts.max_text_len);
                    return;
                }
            }
            ctx.end(&mut self.run, enc);
        }
        if (0xD800..0xDC00).contains(&unit) && ctx.opts.utf16_any_script {
            self.high = Some((unit_at, unit));
            return;
        }
        match char::from_u32(u32::from(unit)).filter(|&c| ctx.utf16_ok(c)) {
            Some(c) => self.run.push(unit_at, c, 2, ctx.opts.max_text_len),
            None => ctx.end(&mut self.run, enc),
        }
    }

    fn flush(&mut self, ctx: &mut Ctx<'_>) {
        let enc = self.enc();
        ctx.end(&mut self.run, enc);
    }
}

#[derive(Debug)]
struct Scanner<'a> {
    ctx: Ctx<'a>,
    ascii: Option<AsciiScan>,
    utf8: Option<Utf8Scan>,
    utf16: Vec<Utf16Scan>,
}

impl<'a> Scanner<'a> {
    fn new(opts: &'a StringsOptions) -> Self {
        let has = |e| opts.encodings.contains(&e);
        let mut utf16 = Vec::new();
        for (enc, be) in [
            (StringEncoding::Utf16Le, false),
            (StringEncoding::Utf16Be, true),
        ] {
            if has(enc) {
                for parity in 0..2 {
                    utf16.push(Utf16Scan {
                        run: Run::new(),
                        big_endian: be,
                        parity,
                        first: None,
                        high: None,
                    });
                }
            }
        }
        Self {
            ctx: Ctx {
                opts,
                out: Vec::new(),
                need_non_ascii_utf8: has(StringEncoding::Ascii),
            },
            ascii: has(StringEncoding::Ascii).then(|| AsciiScan(Run::new())),
            utf8: has(StringEncoding::Utf8).then(|| Utf8Scan {
                run: Run::new(),
                seq: [0; 4],
                have: 0,
                need: 0,
                seq_start: 0,
            }),
            utf16,
        }
    }

    fn feed(&mut self, base: u64, data: &[u8]) {
        for (i, &b) in data.iter().enumerate() {
            let at = base + i as u64;
            if let Some(s) = &mut self.ascii {
                s.feed(&mut self.ctx, at, b);
            }
            if let Some(s) = &mut self.utf8 {
                s.feed(&mut self.ctx, at, b);
            }
            for s in &mut self.utf16 {
                s.feed(&mut self.ctx, at, b);
            }
        }
    }

    fn finish(mut self) -> Vec<FoundString> {
        if let Some(s) = &mut self.ascii {
            self.ctx.end(&mut s.0, StringEncoding::Ascii);
        }
        if let Some(s) = &mut self.utf8 {
            self.ctx.end(&mut s.run, StringEncoding::Utf8);
        }
        for s in &mut self.utf16 {
            s.flush(&mut self.ctx);
        }
        self.ctx.out
    }
}

struct Filter {
    contains: Option<String>,
    regex: Option<Regex>,
}

impl Filter {
    fn new(opts: &StringsOptions) -> Result<Self> {
        let regex = opts
            .regex
            .as_deref()
            .filter(|r| !r.is_empty())
            .map(Regex::new)
            .transpose()
            .map_err(|e| invalid(format!("invalid regex: {e}")))?;
        Ok(Self {
            contains: opts
                .contains
                .as_deref()
                .filter(|c| !c.is_empty())
                .map(str::to_lowercase),
            regex,
        })
    }

    fn keep(&self, s: &FoundString) -> bool {
        self.contains
            .as_ref()
            .is_none_or(|c| s.text.to_lowercase().contains(c))
            && self.regex.as_ref().is_none_or(|r| r.is_match(&s.text))
    }
}

/// Sorts by offset and drops duplicates: ASCII strings inside a reported
/// UTF-8 string, and the odd-offset reading of UTF-16 text that is also
/// found one byte away in the other byte order (LE at `n+1` == BE at `n`).
fn finalize(mut v: Vec<FoundString>) -> Vec<FoundString> {
    v.sort_by_key(|s| (s.offset, s.encoding as u8));
    let is16 = |e| matches!(e, StringEncoding::Utf16Le | StringEncoding::Utf16Be);
    let mut drop = vec![false; v.len()];
    for i in 1..v.len() {
        let (a, b) = (&v[i - 1], &v[i]);
        if is16(a.encoding)
            && is16(b.encoding)
            && a.encoding != b.encoding
            && b.offset == a.offset + 1
            && a.text == b.text
        {
            drop[if a.offset % 2 == 1 { i - 1 } else { i }] = true;
        }
    }
    let mut keep = drop.iter().map(|d| !d);
    v.retain(|_| keep.next().unwrap_or(true));
    let mut utf8_end = 0u64;
    v.retain(|s| match s.encoding {
        StringEncoding::Utf8 => {
            utf8_end = utf8_end.max(s.offset + s.len);
            true
        }
        StringEncoding::Ascii => s.offset + s.len > utf8_end,
        _ => true,
    });
    v
}

/// Extracts strings from a buffer; offsets start at `base`.
pub fn strings_bytes(data: &[u8], base: u64, opts: &StringsOptions) -> Result<StringsResult> {
    let filter = Filter::new(opts)?;
    let mut sc = Scanner::new(opts);
    sc.feed(base, data);
    let mut v: Vec<FoundString> = sc.finish().into_iter().filter(|s| filter.keep(s)).collect();
    v = finalize(v);
    let truncated = v.len() > opts.max_results;
    v.truncate(opts.max_results);
    Ok(StringsResult {
        strings: v,
        truncated,
    })
}

/// Extracts strings from `[start, end)` of a snapshot (ANL-11).
pub fn extract_strings(
    tree: &Tree,
    start: u64,
    end: u64,
    opts: &StringsOptions,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<StringsResult> {
    let filter = Filter::new(opts)?;
    let mut sc = Scanner::new(opts);
    let mut kept = Vec::new();
    let mut truncated = false;
    util::scan(tree, start, end, CHUNK, cancel, progress, |at, chunk| {
        sc.feed(at, chunk);
        kept.extend(sc.ctx.out.drain(..).filter(|s| filter.keep(s)));
        if kept.len() > opts.max_results {
            truncated = true;
            return Ok(false);
        }
        Ok(true)
    })?;
    kept.extend(sc.finish().into_iter().filter(|s| filter.keep(s)));
    let mut v = finalize(kept);
    truncated |= v.len() > opts.max_results;
    v.truncate(opts.max_results);
    Ok(StringsResult {
        strings: v,
        truncated,
    })
}

fn encoding_name(e: StringEncoding) -> &'static str {
    match e {
        StringEncoding::Ascii => "ascii",
        StringEncoding::Utf8 => "utf-8",
        StringEncoding::Utf16Le => "utf-16le",
        StringEncoding::Utf16Be => "utf-16be",
    }
}

/// CSV export: `offset,length,encoding,text` (RFC 4180 quoting).
pub fn strings_to_csv(strings: &[FoundString]) -> String {
    let mut s = String::from("offset,length,encoding,text\r\n");
    for f in strings {
        let _ = write!(
            s,
            "0x{:X},{},{},\"{}\"\r\n",
            f.offset,
            f.len,
            encoding_name(f.encoding),
            f.text.replace('"', "\"\"")
        );
    }
    s
}

/// Plain text export like `strings -t x`: offset, a space, the text.
pub fn strings_to_text(strings: &[FoundString]) -> String {
    let mut s = String::new();
    for f in strings {
        let text = f.text.replace('\n', "\\n").replace('\r', "\\r");
        let _ = writeln!(s, "{:8x} {text}", f.offset);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{noop, random_bytes, tree};

    fn texts(r: &StringsResult) -> Vec<(u64, StringEncoding, &str)> {
        r.strings
            .iter()
            .map(|s| (s.offset, s.encoding, s.text.as_str()))
            .collect()
    }

    fn sample() -> Vec<u8> {
        let mut d = vec![0u8, 1, 2];
        d.extend(b"hello world");
        d.extend([0, 0xFF]);
        d.extend("привет мир".as_bytes());
        d.push(0);
        d.extend("Wide".encode_utf16().flat_map(u16::to_le_bytes));
        d.extend([0, 0]);
        d.extend("Big!".encode_utf16().flat_map(u16::to_be_bytes));
        d.extend([0, 0, 0x02, b'a', b'b', 0x03]);
        d
    }

    #[test]
    fn all_encodings() {
        let d = sample();
        let r = strings_bytes(&d, 0x100, &StringsOptions::default()).unwrap();
        assert_eq!(
            texts(&r),
            vec![
                (0x103, StringEncoding::Ascii, "hello world"),
                (0x110, StringEncoding::Utf8, "привет мир"),
                (0x124, StringEncoding::Utf16Le, "Wide"),
                (0x12E, StringEncoding::Utf16Be, "Big!"),
            ]
        );
        assert_eq!(r.strings[1].len, 19);
        assert_eq!(r.strings[2].len, 8);
        assert!(!r.truncated);
    }

    #[test]
    fn filters_and_limits() {
        let d = sample();
        let mut o = StringsOptions {
            contains: Some("WORLD".into()),
            ..StringsOptions::default()
        };
        assert_eq!(strings_bytes(&d, 0, &o).unwrap().strings.len(), 1);
        o.contains = None;
        o.regex = Some("^[A-Z]".into());
        let r = strings_bytes(&d, 0, &o).unwrap();
        assert_eq!(r.strings.len(), 2);
        o.regex = Some("(".into());
        assert!(strings_bytes(&d, 0, &o).is_err());
        let o = StringsOptions {
            min_len: 12,
            ..StringsOptions::default()
        };
        assert_eq!(strings_bytes(&d, 0, &o).unwrap().strings.len(), 0);
        let o = StringsOptions {
            encodings: vec![StringEncoding::Utf8],
            ..StringsOptions::default()
        };
        // Without the ASCII scanner, UTF-8 reports pure-ASCII runs too.
        assert_eq!(strings_bytes(&d, 0, &o).unwrap().strings.len(), 2);
        let o = StringsOptions {
            max_results: 1,
            ..StringsOptions::default()
        };
        let r = strings_bytes(&d, 0, &o).unwrap();
        assert!(r.truncated);
        assert_eq!(r.strings.len(), 1);
        let o = StringsOptions {
            max_text_len: 3,
            ..StringsOptions::default()
        };
        let r = strings_bytes(b"abcdefgh", 0, &o).unwrap();
        assert_eq!((r.strings[0].text.as_str(), r.strings[0].len), ("abc", 8));
    }

    #[test]
    fn whitespace_and_surrogates() {
        let o = StringsOptions {
            include_whitespace: true,
            ..StringsOptions::default()
        };
        let r = strings_bytes(b"\x00line one\nline two\x00", 0, &o).unwrap();
        assert_eq!(r.strings[0].text, "line one\nline two");
        let emoji: Vec<u8> = "ab😀cd".encode_utf16().flat_map(u16::to_le_bytes).collect();
        let o = StringsOptions {
            encodings: vec![StringEncoding::Utf16Le],
            utf16_any_script: true,
            ..StringsOptions::default()
        };
        let r = strings_bytes(&emoji, 0, &o).unwrap();
        assert_eq!(r.strings[0].text, "ab😀cd");
        assert_eq!(r.strings[0].len, 12);
    }

    #[test]
    fn streaming_matches_buffer_across_chunks() {
        let mut d = random_bytes(1, 9_000_000);
        // Plant strings across the 4 MiB chunk boundary.
        let b = CHUNK - 3;
        d[b - 1] = 0;
        d[b..b + 10].copy_from_slice(b"boundary!!");
        d[b + 10] = 0;
        let u: Vec<u8> = "граница".as_bytes().to_vec();
        let b2 = 2 * CHUNK - 5;
        d[b2 - 1] = 0;
        d[b2..b2 + u.len()].copy_from_slice(&u);
        d[b2 + u.len()] = 0;
        let o = StringsOptions {
            min_len: 7,
            ..StringsOptions::default()
        };
        let t = tree(&d);
        let a = extract_strings(&t, 0, d.len() as u64, &o, &Cancel::new(), &noop).unwrap();
        let m = strings_bytes(&d, 0, &o).unwrap();
        assert_eq!(a, m);
        assert!(
            a.strings
                .iter()
                .any(|s| s.offset == b as u64 && s.text == "boundary!!")
        );
        assert!(
            a.strings
                .iter()
                .any(|s| s.offset == b2 as u64 && s.text == "граница")
        );
    }

    #[test]
    fn exports() {
        let r = strings_bytes(b"say \"hi\" now\x00", 16, &StringsOptions::default()).unwrap();
        assert_eq!(
            strings_to_csv(&r.strings),
            "offset,length,encoding,text\r\n0x10,12,ascii,\"say \"\"hi\"\" now\"\r\n"
        );
        assert_eq!(strings_to_text(&r.strings), "      10 say \"hi\" now\n");
    }
}
