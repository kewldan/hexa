//! Firmware image formats (IO-01) and flattening of sparse images (IO-02).
//!
//! Import parses Intel HEX (all record types, extended segment / linear
//! addressing, start addresses), Motorola S-Record (S0–S9), TI-TXT and
//! Tektronix extended hex into sparse [`Segment`]s, validating checksums
//! and collecting line-numbered errors instead of failing on the first one
//! (records with errors are skipped). Export writes the same formats from
//! segments (or bytes plus a base address) with a configurable record size.

use std::fmt::Write as _;

use hexa_core::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::util::invalid;

/// Contiguous bytes at an address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    pub address: u64,
    pub data: Vec<u8>,
}

/// A problem on one input line (1-based).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LineError {
    pub line: usize,
    pub message: String,
}

/// Execution start address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum StartAddress {
    /// Intel HEX record 03 (`CS:IP`).
    Segment { cs: u16, ip: u16 },
    /// Intel HEX record 05, S-Record S7–S9, Tektronix termination record.
    Linear { address: u64 },
}

/// Supported text firmware formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FirmwareFormat {
    IntelHex,
    SRecord,
    TiTxt,
    TekHex,
}

/// Result of an import.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FirmwareImage {
    pub format: FirmwareFormat,
    /// Sorted by address, non-overlapping, adjacent data merged.
    pub segments: Vec<Segment>,
    pub start: Option<StartAddress>,
    /// S-Record S0 header text.
    pub header: Option<String>,
    pub errors: Vec<LineError>,
    /// Records parsed successfully.
    pub records: usize,
}

/// Collects data pieces and builds merged segments.
#[derive(Debug, Default)]
struct Builder {
    pieces: Vec<(u64, Vec<u8>, usize)>,
    errors: Vec<LineError>,
    records: usize,
}

impl Builder {
    fn err(&mut self, line: usize, message: impl Into<String>) {
        self.errors.push(LineError {
            line,
            message: message.into(),
        });
    }

    fn data(&mut self, address: u64, data: Vec<u8>, line: usize) {
        if !data.is_empty() {
            self.pieces.push((address, data, line));
        }
    }

    fn finish(
        mut self,
        format: FirmwareFormat,
        start: Option<StartAddress>,
        header: Option<String>,
    ) -> FirmwareImage {
        let (segments, overlaps) = merge(std::mem::take(&mut self.pieces));
        for (line, address) in overlaps {
            self.err(line, format!("data at 0x{address:X} overlaps earlier data"));
        }
        self.errors.sort_by_key(|e| e.line);
        FirmwareImage {
            format,
            segments,
            start,
            header,
            errors: self.errors,
            records: self.records,
        }
    }
}

/// Sorts pieces by address and merges them; returns overlapping pieces as
/// `(line, address)` (the later piece in address order wins).
pub(crate) fn merge(mut pieces: Vec<(u64, Vec<u8>, usize)>) -> (Vec<Segment>, Vec<(usize, u64)>) {
    pieces.sort_by_key(|p| p.0);
    let mut out: Vec<Segment> = Vec::new();
    let mut overlaps = Vec::new();
    for (addr, data, line) in pieces {
        match out.last_mut() {
            Some(s) if addr <= s.address + s.data.len() as u64 => {
                let rel = (addr - s.address) as usize;
                if rel < s.data.len() {
                    overlaps.push((line, addr));
                }
                let keep = (s.data.len() - rel).min(data.len());
                s.data[rel..rel + keep].copy_from_slice(&data[..keep]);
                s.data.extend_from_slice(&data[keep..]);
            }
            _ => out.push(Segment {
                address: addr,
                data,
            }),
        }
    }
    (out, overlaps)
}

/// Merges arbitrary segments (sorted, overlaps resolved in favour of later
/// addresses).
pub fn normalize_segments(segments: &[Segment]) -> Vec<Segment> {
    merge(
        segments
            .iter()
            .map(|s| (s.address, s.data.clone(), 0))
            .collect(),
    )
    .0
}

fn hex_bytes(s: &str) -> std::result::Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) {
        return Err("odd number of hex digits".into());
    }
    if !s.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err("invalid hex digit".into());
    }
    Ok((0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("validated hex"))
        .collect())
}

fn be_value(b: &[u8]) -> u64 {
    b.iter().fold(0, |acc, &x| (acc << 8) | u64::from(x))
}

/// Guesses the format from the first non-empty line.
pub fn detect_format(text: &str) -> Option<FirmwareFormat> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    match line.as_bytes()[0] {
        b':' => Some(FirmwareFormat::IntelHex),
        b'S' | b's' if line.len() > 1 && line.as_bytes()[1].is_ascii_digit() => {
            Some(FirmwareFormat::SRecord)
        }
        b'@' => Some(FirmwareFormat::TiTxt),
        b'%' => Some(FirmwareFormat::TekHex),
        _ => None,
    }
}

/// Imports a firmware file, detecting the format unless given.
pub fn import(text: &str, format: Option<FirmwareFormat>) -> Result<FirmwareImage> {
    let format = format
        .or_else(|| detect_format(text))
        .ok_or_else(|| invalid("unknown firmware format"))?;
    Ok(match format {
        FirmwareFormat::IntelHex => parse_intel_hex(text),
        FirmwareFormat::SRecord => parse_srec(text),
        FirmwareFormat::TiTxt => parse_ti_txt(text),
        FirmwareFormat::TekHex => parse_tek_hex(text),
    })
}

// ---------------------------------------------------------------- Intel HEX

/// Parses Intel HEX (I8HEX / I16HEX / I32HEX).
pub fn parse_intel_hex(text: &str) -> FirmwareImage {
    let mut b = Builder::default();
    let mut start = None;
    // Segment base (record 02) or linear base (record 04).
    let mut segment_mode = false;
    let mut base = 0u64;
    let mut eof = false;
    let mut last = 0;
    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        last = line;
        let l = raw.trim();
        if l.is_empty() {
            continue;
        }
        if eof {
            b.err(line, "data after end-of-file record");
            break;
        }
        let Some(body) = l.strip_prefix(':') else {
            b.err(line, "record does not start with ':'");
            continue;
        };
        let bytes = match hex_bytes(body) {
            Ok(v) => v,
            Err(e) => {
                b.err(line, e);
                continue;
            }
        };
        if bytes.len() < 5 || bytes.len() != usize::from(bytes[0]) + 5 {
            b.err(line, "record length does not match its byte count");
            continue;
        }
        let sum = bytes.iter().fold(0u8, |a, &x| a.wrapping_add(x));
        if sum != 0 {
            let body_sum = bytes[..bytes.len() - 1]
                .iter()
                .fold(0u8, |a, &x| a.wrapping_add(x));
            b.err(
                line,
                format!(
                    "checksum mismatch: record has {:02X}, expected {:02X}",
                    bytes[bytes.len() - 1],
                    body_sum.wrapping_neg()
                ),
            );
            continue;
        }
        let count = usize::from(bytes[0]);
        let offset = u64::from(u16::from_be_bytes([bytes[1], bytes[2]]));
        let ty = bytes[3];
        let data = &bytes[4..4 + count];
        let need = |n: usize, b: &mut Builder| {
            if count == n {
                true
            } else {
                b.err(line, format!("record type {ty:02X} needs {n} data bytes"));
                false
            }
        };
        match ty {
            0x00 => {
                if segment_mode {
                    // Offsets wrap inside the 64 KiB segment.
                    let first = (0x1_0000 - offset as usize).min(data.len());
                    b.data(base + offset, data[..first].to_vec(), line);
                    b.data(base, data[first..].to_vec(), line);
                } else {
                    b.data((base + offset) & 0xFFFF_FFFF, data.to_vec(), line);
                }
            }
            0x01 => eof = true,
            0x02 => {
                if need(2, &mut b) {
                    segment_mode = true;
                    base = be_value(data) << 4;
                }
            }
            0x03 => {
                if need(4, &mut b) {
                    start = Some(StartAddress::Segment {
                        cs: u16::from_be_bytes([data[0], data[1]]),
                        ip: u16::from_be_bytes([data[2], data[3]]),
                    });
                }
            }
            0x04 => {
                if need(2, &mut b) {
                    segment_mode = false;
                    base = be_value(data) << 16;
                }
            }
            0x05 => {
                if need(4, &mut b) {
                    start = Some(StartAddress::Linear {
                        address: be_value(data),
                    });
                }
            }
            other => {
                b.err(line, format!("unknown record type {other:02X}"));
                continue;
            }
        }
        b.records += 1;
    }
    if !eof {
        b.err(last + 1, "missing end-of-file record");
    }
    b.finish(FirmwareFormat::IntelHex, start, None)
}

/// How [`export_intel_hex`] addresses data above 64 KiB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IntelAddressing {
    /// Record 04 (up to 4 GiB).
    #[default]
    Linear,
    /// Record 02 (up to 1 MiB).
    Segment,
}

/// Export options shared by all formats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ExportOptions {
    /// Data bytes per record (Intel HEX ≤ 255, S-Record ≤ 250, TI-TXT any,
    /// Tektronix ≤ 100).
    pub record_size: usize,
    pub start: Option<StartAddress>,
    /// S-Record S0 header.
    pub header: Option<String>,
    pub intel_addressing: IntelAddressing,
    /// S-Record address bytes (2, 3 or 4); `None` picks the smallest that fits.
    pub srec_address_bytes: Option<u8>,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            record_size: 16,
            start: None,
            header: None,
            intel_addressing: IntelAddressing::Linear,
            srec_address_bytes: None,
        }
    }
}

fn hex_upper(out: &mut String, bytes: &[u8]) {
    for b in bytes {
        let _ = write!(out, "{b:02X}");
    }
}

fn intel_record(out: &mut String, ty: u8, offset: u16, data: &[u8]) {
    let mut rec = vec![data.len() as u8];
    rec.extend(offset.to_be_bytes());
    rec.push(ty);
    rec.extend_from_slice(data);
    let sum = rec.iter().fold(0u8, |a, &x| a.wrapping_add(x));
    rec.push(sum.wrapping_neg());
    out.push(':');
    hex_upper(out, &rec);
    out.push('\n');
}

fn check_record_size(size: usize, max: usize) -> Result<()> {
    if (1..=max).contains(&size) {
        Ok(())
    } else {
        Err(invalid(format!("record size must be 1…{max}")))
    }
}

/// Writes Intel HEX.
pub fn export_intel_hex(segments: &[Segment], opts: &ExportOptions) -> Result<String> {
    check_record_size(opts.record_size, 255)?;
    let limit: u64 = match opts.intel_addressing {
        IntelAddressing::Linear => 1 << 32,
        IntelAddressing::Segment => 1 << 20,
    };
    let mut out = String::new();
    let mut current_base: Option<u64> = None;
    for s in normalize_segments(segments) {
        if s.address + s.data.len() as u64 > limit {
            return Err(invalid(format!(
                "address 0x{:X} does not fit Intel HEX addressing",
                s.address + s.data.len() as u64 - 1
            )));
        }
        let mut addr = s.address;
        let mut rest = &s.data[..];
        while !rest.is_empty() {
            let base = addr & !0xFFFF;
            if current_base != Some(base) && !(current_base.is_none() && base == 0) {
                match opts.intel_addressing {
                    IntelAddressing::Linear => {
                        intel_record(&mut out, 0x04, 0, &((base >> 16) as u16).to_be_bytes());
                    }
                    IntelAddressing::Segment => {
                        intel_record(&mut out, 0x02, 0, &((base >> 4) as u16).to_be_bytes());
                    }
                }
            }
            current_base = Some(base);
            let to_boundary = (base + 0x1_0000 - addr) as usize;
            let n = opts.record_size.min(rest.len()).min(to_boundary);
            intel_record(&mut out, 0x00, (addr & 0xFFFF) as u16, &rest[..n]);
            rest = &rest[n..];
            addr += n as u64;
        }
    }
    match opts.start {
        Some(StartAddress::Segment { cs, ip }) => {
            let mut d = cs.to_be_bytes().to_vec();
            d.extend(ip.to_be_bytes());
            intel_record(&mut out, 0x03, 0, &d);
        }
        Some(StartAddress::Linear { address }) => {
            let a = u32::try_from(address).map_err(|_| invalid("start address exceeds 32 bits"))?;
            intel_record(&mut out, 0x05, 0, &a.to_be_bytes());
        }
        None => {}
    }
    intel_record(&mut out, 0x01, 0, &[]);
    Ok(out)
}

// ----------------------------------------------------------------- S-Record

fn srec_addr_len(ty: u8) -> Option<usize> {
    match ty {
        b'0' | b'1' | b'5' | b'9' => Some(2),
        b'2' | b'6' | b'8' => Some(3),
        b'3' | b'7' => Some(4),
        _ => None,
    }
}

/// Parses Motorola S-Records (S0–S9).
pub fn parse_srec(text: &str) -> FirmwareImage {
    let mut b = Builder::default();
    let mut start = None;
    let mut header = None;
    let mut data_records = 0u64;
    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        let l = raw.trim();
        if l.is_empty() {
            continue;
        }
        let lb = l.as_bytes();
        if lb.len() < 4 || !matches!(lb[0], b'S' | b's') {
            b.err(line, "record does not start with 'S'");
            continue;
        }
        let ty = lb[1];
        let Some(alen) = srec_addr_len(ty) else {
            b.err(line, format!("unsupported record type S{}", ty as char));
            continue;
        };
        let bytes = match hex_bytes(&l[2..]) {
            Ok(v) => v,
            Err(e) => {
                b.err(line, e);
                continue;
            }
        };
        let count = usize::from(bytes[0]);
        if bytes.len() != count + 1 || count < alen + 1 {
            b.err(line, "record length does not match its byte count");
            continue;
        }
        let sum = bytes[..bytes.len() - 1]
            .iter()
            .fold(0u8, |a, &x| a.wrapping_add(x));
        let expected = !sum;
        let got = bytes[bytes.len() - 1];
        if got != expected {
            b.err(
                line,
                format!("checksum mismatch: record has {got:02X}, expected {expected:02X}"),
            );
            continue;
        }
        let address = be_value(&bytes[1..=alen]);
        let data = &bytes[1 + alen..bytes.len() - 1];
        match ty {
            b'0' => header = Some(String::from_utf8_lossy(data).into_owned()),
            b'1' | b'2' | b'3' => {
                data_records += 1;
                b.data(address, data.to_vec(), line);
            }
            b'5' | b'6' => {
                if address != data_records {
                    b.err(
                        line,
                        format!(
                            "record count {address} does not match {data_records} data records"
                        ),
                    );
                }
            }
            _ => start = Some(StartAddress::Linear { address }),
        }
        b.records += 1;
    }
    b.finish(FirmwareFormat::SRecord, start, header)
}

fn srec_record(out: &mut String, ty: u8, address: u64, alen: usize, data: &[u8]) {
    let mut rec = vec![(alen + data.len() + 1) as u8];
    rec.extend(&address.to_be_bytes()[8 - alen..]);
    rec.extend_from_slice(data);
    let sum = rec.iter().fold(0u8, |a, &x| a.wrapping_add(x));
    rec.push(!sum);
    out.push('S');
    out.push(ty as char);
    hex_upper(out, &rec);
    out.push('\n');
}

/// Writes Motorola S-Records.
pub fn export_srec(segments: &[Segment], opts: &ExportOptions) -> Result<String> {
    let segments = normalize_segments(segments);
    let max_end = segments
        .iter()
        .map(|s| s.address + s.data.len() as u64)
        .max()
        .unwrap_or(0);
    let start_addr = match opts.start {
        Some(StartAddress::Linear { address }) => address,
        Some(StartAddress::Segment { cs, ip }) => u64::from(cs) * 16 + u64::from(ip),
        None => 0,
    };
    let needed = max_end.saturating_sub(1).max(start_addr);
    let alen = match opts.srec_address_bytes {
        Some(n @ 2..=4) => usize::from(n),
        Some(_) => return Err(invalid("S-Record address size must be 2, 3 or 4 bytes")),
        None if needed <= 0xFFFF => 2,
        None if needed <= 0xFF_FFFF => 3,
        None => 4,
    };
    if needed >= 1u64 << (8 * alen) {
        return Err(invalid(format!(
            "address 0x{needed:X} needs more than {alen} bytes"
        )));
    }
    check_record_size(opts.record_size, 254 - alen)?;
    let (data_ty, end_ty) = match alen {
        2 => (b'1', b'9'),
        3 => (b'2', b'8'),
        _ => (b'3', b'7'),
    };
    let mut out = String::new();
    if let Some(h) = &opts.header {
        srec_record(&mut out, b'0', 0, 2, &h.as_bytes()[..h.len().min(252)]);
    }
    let mut count = 0u64;
    for s in &segments {
        for (i, chunk) in s.data.chunks(opts.record_size).enumerate() {
            srec_record(
                &mut out,
                data_ty,
                s.address + (i * opts.record_size) as u64,
                alen,
                chunk,
            );
            count += 1;
        }
    }
    if count <= 0xFFFF {
        srec_record(&mut out, b'5', count, 2, &[]);
    } else if count <= 0xFF_FFFF {
        srec_record(&mut out, b'6', count, 3, &[]);
    }
    srec_record(&mut out, end_ty, start_addr, alen, &[]);
    Ok(out)
}

// ------------------------------------------------------------------- TI-TXT

/// Parses TI-TXT (`@ADDR` sections, hex bytes, `q` terminator).
pub fn parse_ti_txt(text: &str) -> FirmwareImage {
    let mut b = Builder::default();
    let mut addr: Option<u64> = None;
    let mut ended = false;
    let mut last = 0;
    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        last = line;
        let l = raw.trim();
        if l.is_empty() {
            continue;
        }
        if ended {
            b.err(line, "data after 'q' terminator");
            break;
        }
        if l.eq_ignore_ascii_case("q") {
            ended = true;
            b.records += 1;
            continue;
        }
        if let Some(a) = l.strip_prefix('@') {
            match u64::from_str_radix(a.trim(), 16) {
                Ok(v) => {
                    addr = Some(v);
                    b.records += 1;
                }
                Err(_) => b.err(line, format!("invalid address {a:?}")),
            }
            continue;
        }
        let Some(at) = addr else {
            b.err(line, "data before the first '@address'");
            continue;
        };
        let mut data = Vec::new();
        let mut bad = None;
        for tok in l.split_whitespace() {
            if let (2, Ok(v)) = (tok.len(), u8::from_str_radix(tok, 16)) {
                data.push(v);
            } else {
                bad = Some(tok);
                break;
            }
        }
        if let Some(tok) = bad {
            b.err(line, format!("invalid byte {tok:?}"));
            continue;
        }
        addr = Some(at + data.len() as u64);
        b.data(at, data, line);
        b.records += 1;
    }
    if !ended {
        b.err(last + 1, "missing 'q' terminator");
    }
    b.finish(FirmwareFormat::TiTxt, None, None)
}

/// Writes TI-TXT.
pub fn export_ti_txt(segments: &[Segment], opts: &ExportOptions) -> Result<String> {
    check_record_size(opts.record_size, 4096)?;
    let mut out = String::new();
    for s in normalize_segments(segments) {
        let _ = writeln!(out, "@{:04X}", s.address);
        for chunk in s.data.chunks(opts.record_size) {
            let line: Vec<String> = chunk.iter().map(|b| format!("{b:02X}")).collect();
            out.push_str(&line.join(" "));
            out.push('\n');
        }
    }
    out.push_str("q\n");
    Ok(out)
}

// ------------------------------------------------------- Tektronix extended

fn tek_digit_sum(s: &str) -> u32 {
    s.chars().filter_map(|c| c.to_digit(16)).sum()
}

/// Parses Tektronix extended hex (`%LLTCC…`; data type 6, termination 8,
/// symbol records 3 are checked and ignored).
pub fn parse_tek_hex(text: &str) -> FirmwareImage {
    let mut b = Builder::default();
    let mut start = None;
    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        let l = raw.trim();
        if l.is_empty() {
            continue;
        }
        let Some(body) = l.strip_prefix('%') else {
            b.err(line, "record does not start with '%'");
            continue;
        };
        if body.len() < 6 || !body.is_ascii() {
            b.err(line, "record too short");
            continue;
        }
        let Ok(len) = usize::from_str_radix(&body[..2], 16) else {
            b.err(line, "invalid record length");
            continue;
        };
        if len != body.len() {
            b.err(
                line,
                format!(
                    "record length {len} does not match {} characters",
                    body.len()
                ),
            );
            continue;
        }
        let ty = &body[2..3];
        let Ok(checksum) = u32::from_str_radix(&body[3..5], 16) else {
            b.err(line, "invalid checksum field");
            continue;
        };
        let sum = (tek_digit_sum(&body[..3]) + tek_digit_sum(&body[5..])) & 0xFF;
        if sum != checksum {
            b.err(
                line,
                format!("checksum mismatch: record has {checksum:02X}, expected {sum:02X}"),
            );
            continue;
        }
        if ty == "3" {
            b.records += 1;
            continue;
        }
        let rest = &body[5..];
        let alen = match rest
            .get(..1)
            .and_then(|d| usize::from_str_radix(d, 16).ok())
        {
            Some(0) => 16,
            Some(n) => n,
            None => {
                b.err(line, "invalid address length");
                continue;
            }
        };
        let Some(addr_str) = rest.get(1..=alen) else {
            b.err(line, "record shorter than its address");
            continue;
        };
        let Ok(address) = u64::from_str_radix(addr_str, 16) else {
            b.err(line, "invalid address");
            continue;
        };
        match ty {
            "6" => match hex_bytes(&rest[1 + alen..]) {
                Ok(data) => b.data(address, data, line),
                Err(e) => {
                    b.err(line, e);
                    continue;
                }
            },
            "8" => start = Some(StartAddress::Linear { address }),
            other => {
                b.err(line, format!("unknown record type {other}"));
                continue;
            }
        }
        b.records += 1;
    }
    b.finish(FirmwareFormat::TekHex, start, None)
}

fn tek_record(out: &mut String, ty: char, address: u64, data: &[u8]) {
    let addr = format!("{address:X}");
    let alen = addr.len();
    let mut body = String::new();
    // Address length digit (16 is written as 0).
    let _ = write!(body, "{:X}{addr}", alen % 16);
    hex_upper(&mut body, data);
    let len = 2 + 1 + 2 + body.len();
    let head = format!("{len:02X}{ty}");
    let sum = (tek_digit_sum(&head) + tek_digit_sum(&body)) & 0xFF;
    let _ = writeln!(out, "%{head}{sum:02X}{body}");
}

/// Writes Tektronix extended hex.
pub fn export_tek_hex(segments: &[Segment], opts: &ExportOptions) -> Result<String> {
    check_record_size(opts.record_size, 100)?;
    let mut out = String::new();
    for s in normalize_segments(segments) {
        for (i, chunk) in s.data.chunks(opts.record_size).enumerate() {
            tek_record(
                &mut out,
                '6',
                s.address + (i * opts.record_size) as u64,
                chunk,
            );
        }
    }
    let start = match opts.start {
        Some(StartAddress::Linear { address }) => address,
        Some(StartAddress::Segment { cs, ip }) => u64::from(cs) * 16 + u64::from(ip),
        None => 0,
    };
    tek_record(&mut out, '8', start, &[]);
    Ok(out)
}

/// Exports segments in any supported format.
pub fn export(
    segments: &[Segment],
    format: FirmwareFormat,
    opts: &ExportOptions,
) -> Result<String> {
    match format {
        FirmwareFormat::IntelHex => export_intel_hex(segments, opts),
        FirmwareFormat::SRecord => export_srec(segments, opts),
        FirmwareFormat::TiTxt => export_ti_txt(segments, opts),
        FirmwareFormat::TekHex => export_tek_hex(segments, opts),
    }
}

/// Exports a byte range (a selection or a whole file) at `base` address.
pub fn export_bytes(
    data: &[u8],
    base: u64,
    format: FirmwareFormat,
    opts: &ExportOptions,
) -> Result<String> {
    export(
        &[Segment {
            address: base,
            data: data.to_vec(),
        }],
        format,
        opts,
    )
}

// ------------------------------------------------------------------- IO-02

/// A linear image produced by [`flatten`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Flattened {
    /// Address of `data[0]`.
    pub address: u64,
    pub data: Vec<u8>,
}

/// Default size limit of [`flatten`].
pub const MAX_FLAT_LEN: u64 = 256 << 20;

/// Flattens sparse segments into one buffer (IO-02). Gaps are filled with
/// `fill`; `start` / `end` (exclusive) default to the lowest and highest
/// addresses and clip the data; `max_len` guards against huge gaps.
pub fn flatten(
    segments: &[Segment],
    fill: u8,
    start: Option<u64>,
    end: Option<u64>,
    max_len: u64,
) -> Result<Flattened> {
    let segs = normalize_segments(segments);
    let lo = start.unwrap_or_else(|| segs.first().map_or(0, |s| s.address));
    let hi = end.unwrap_or_else(|| segs.last().map_or(lo, |s| s.address + s.data.len() as u64));
    if hi < lo {
        return Err(invalid("end address is below start address"));
    }
    let len = hi - lo;
    if len > max_len {
        return Err(Error::TooLarge {
            len,
            limit: max_len,
        });
    }
    let mut data = vec![fill; len as usize];
    for s in &segs {
        let s_end = s.address + s.data.len() as u64;
        let a = s.address.max(lo);
        let b = s_end.min(hi);
        if a < b {
            data[(a - lo) as usize..(b - lo) as usize]
                .copy_from_slice(&s.data[(a - s.address) as usize..(b - s.address) as usize]);
        }
    }
    Ok(Flattened { address: lo, data })
}

#[cfg(test)]
mod tests {
    use super::*;

    const IHEX: &str = ":10010000214601360121470136007EFE09D2190140\n\
:100110002146017E17C20001FF5F16002148011928\n\
:10012000194E79234623965778239EDA3F01B2CAA7\n\
:100130003F0156702B5E712B722B732146013421C7\n\
:00000001FF\n";

    #[test]
    fn intel_hex_roundtrip() {
        let img = import(IHEX, None).unwrap();
        assert_eq!(img.format, FirmwareFormat::IntelHex);
        assert!(img.errors.is_empty(), "{:?}", img.errors);
        assert_eq!(img.segments.len(), 1);
        assert_eq!(img.segments[0].address, 0x100);
        assert_eq!(img.segments[0].data.len(), 64);
        assert_eq!(img.records, 5);
        let out = export_intel_hex(&img.segments, &ExportOptions::default()).unwrap();
        assert_eq!(out, IHEX);
    }

    #[test]
    fn intel_hex_extended_addressing_and_errors() {
        let segs = vec![
            Segment {
                address: 0x0001_FFF8,
                data: (0..16).collect(),
            },
            Segment {
                address: 0x0800_0000,
                data: vec![0xAA; 3],
            },
        ];
        let opts = ExportOptions {
            start: Some(StartAddress::Linear {
                address: 0x0800_0000,
            }),
            ..ExportOptions::default()
        };
        let text = export_intel_hex(&segs, &opts).unwrap();
        assert!(text.contains(":020000040001F9\n"));
        assert!(text.contains(":020000040002F8\n"));
        assert!(text.contains(":0400000508000000EF\n"));
        let back = parse_intel_hex(&text);
        assert!(back.errors.is_empty(), "{:?}", back.errors);
        assert_eq!(back.segments, segs);
        assert_eq!(back.start, opts.start);

        let seg_opts = ExportOptions {
            intel_addressing: IntelAddressing::Segment,
            start: Some(StartAddress::Segment {
                cs: 0x1234,
                ip: 0x10,
            }),
            ..ExportOptions::default()
        };
        let small = vec![Segment {
            address: 0x2_0010,
            data: vec![1, 2, 3],
        }];
        let text = export_intel_hex(&small, &seg_opts).unwrap();
        assert!(text.starts_with(":020000022000DC\n"));
        let back = parse_intel_hex(&text);
        assert_eq!(back.segments, small);
        assert_eq!(back.start, seg_opts.start);
        assert!(export_intel_hex(&segs, &seg_opts).is_err());

        let bad = ":0300000001020304\n:02000000AABB98\nxyz\n:0100000001FF\n";
        let img = parse_intel_hex(bad);
        let lines: Vec<usize> = img.errors.iter().map(|e| e.line).collect();
        assert_eq!(lines, [1, 2, 3, 4, 5]);
        assert!(img.errors[1].message.contains("checksum"));
        assert!(img.errors[4].message.contains("end-of-file"));
    }

    #[test]
    fn intel_segment_wrap() {
        // Segment base 0x1000:0, data at offset 0xFFFE wraps to 0x10000.
        let text = ":020000021000EC\n:04FFFE000102030400\n:00000001FF\n";
        let mut fixed = String::new();
        for l in text.lines() {
            // Recompute checksums so the test only exercises wrapping.
            let mut b = hex_bytes(&l[1..l.len() - 2]).unwrap();
            let sum = b.iter().fold(0u8, |a, &x| a.wrapping_add(x));
            b.push(sum.wrapping_neg());
            fixed.push(':');
            hex_upper(&mut fixed, &b);
            fixed.push('\n');
        }
        let img = parse_intel_hex(&fixed);
        assert!(img.errors.is_empty(), "{:?}", img.errors);
        assert_eq!(
            img.segments,
            vec![
                Segment {
                    address: 0x10000,
                    data: vec![3, 4]
                },
                Segment {
                    address: 0x1FFFE,
                    data: vec![1, 2]
                },
            ]
        );
    }

    const SREC: &str = "S00F000068656C6C6F202020202000003C\n\
S11F00007C0802A6900100049421FFF07C6C1B787C8C23783C6000003863000026\n\
S11F001C4BFFFFE5398000007D83637880010014382100107C0803A64E800020E9\n\
S111003848656C6C6F20776F726C642E0A0042\n\
S5030003F9\n\
S9030000FC\n";

    #[test]
    fn srec_roundtrip() {
        let img = import(SREC, None).unwrap();
        assert_eq!(img.format, FirmwareFormat::SRecord);
        assert!(img.errors.is_empty(), "{:?}", img.errors);
        assert_eq!(img.header.as_deref(), Some("hello     \0\0"));
        assert_eq!(img.segments[0].data.len(), 70);
        assert_eq!(img.start, Some(StartAddress::Linear { address: 0 }));
        let opts = ExportOptions {
            record_size: 28,
            header: img.header.clone(),
            start: img.start,
            ..ExportOptions::default()
        };
        assert_eq!(export_srec(&img.segments, &opts).unwrap(), SREC);
    }

    #[test]
    fn srec_wide_addresses_and_errors() {
        let segs = vec![Segment {
            address: 0x1234_5678,
            data: vec![9; 40],
        }];
        let text = export_srec(&segs, &ExportOptions::default()).unwrap();
        assert!(text.lines().next().unwrap().starts_with("S3"));
        assert!(text.ends_with("S70500000000FA\n"));
        let back = parse_srec(&text);
        assert!(back.errors.is_empty(), "{:?}", back.errors);
        assert_eq!(back.segments, segs);
        let s2 = export_srec(
            &[Segment {
                address: 0x10000,
                data: vec![1],
            }],
            &ExportOptions::default(),
        )
        .unwrap();
        assert!(s2.starts_with("S205010000"));
        assert!(
            export_srec(
                &segs,
                &ExportOptions {
                    srec_address_bytes: Some(2),
                    ..ExportOptions::default()
                }
            )
            .is_err()
        );

        let bad = "S1030000FC\nS1030000FB\nS4030000FC\nS5030005F7\n";
        let img = parse_srec(bad);
        let msgs: Vec<(usize, bool)> = img
            .errors
            .iter()
            .map(|e| (e.line, e.message.contains("checksum")))
            .collect();
        assert_eq!(msgs, [(2, true), (3, false), (4, false)]);
    }

    const TI: &str = "@F000\n31 40 00 03 B2 40 80 5A 20 01 D2 D3 22 00 D2 E3\n21 00 3F 40 E8 FD 1F 83 FE 23 F9 3F\n@FFFE\n00 F0\nq\n";

    #[test]
    fn ti_txt_roundtrip() {
        let img = import(TI, None).unwrap();
        assert!(img.errors.is_empty(), "{:?}", img.errors);
        assert_eq!(img.segments.len(), 2);
        assert_eq!(img.segments[0].data.len(), 28);
        assert_eq!(
            img.segments[1],
            Segment {
                address: 0xFFFE,
                data: vec![0, 0xF0]
            }
        );
        assert_eq!(
            export_ti_txt(&img.segments, &ExportOptions::default()).unwrap(),
            TI
        );
        let bad = parse_ti_txt("00 11\n@10\n0G\n");
        let lines: Vec<usize> = bad.errors.iter().map(|e| e.line).collect();
        assert_eq!(lines, [1, 3, 4]);
    }

    #[test]
    fn tek_hex_known_record_and_roundtrip() {
        let img = import("%1A626810000000202020202020\n", None).unwrap();
        assert!(img.errors.is_empty(), "{:?}", img.errors);
        assert_eq!(
            img.segments,
            vec![Segment {
                address: 0x1000_0000,
                data: vec![0x20; 6]
            }]
        );
        let segs = vec![
            Segment {
                address: 0,
                data: (0..40).collect(),
            },
            Segment {
                address: 0x00AB_CDEF,
                data: vec![0xFF, 0],
            },
        ];
        let opts = ExportOptions {
            start: Some(StartAddress::Linear { address: 0x100 }),
            ..ExportOptions::default()
        };
        let text = export(&segs, FirmwareFormat::TekHex, &opts).unwrap();
        let back = parse_tek_hex(&text);
        assert!(back.errors.is_empty(), "{:?}\n{text}", back.errors);
        assert_eq!(back.segments, segs);
        assert_eq!(back.start, opts.start);
        let bad = parse_tek_hex("%1A626910000000202020202020\n%1B626810000000202020202020\n");
        assert_eq!(bad.errors.len(), 2);
        assert!(bad.errors[0].message.contains("checksum"));
    }

    #[test]
    fn overlaps_detection_and_flatten() {
        let text = ":0400000001020304F2\n:02000200AABB97\n:00000001FF\n";
        let img = parse_intel_hex(text);
        assert_eq!(
            img.segments,
            vec![Segment {
                address: 0,
                data: vec![1, 2, 0xAA, 0xBB]
            }]
        );
        assert_eq!(img.errors.len(), 1);
        assert!(img.errors[0].message.contains("overlaps"));
        assert!(import("hello", None).is_err());

        let segs = vec![
            Segment {
                address: 0x10,
                data: vec![1, 2],
            },
            Segment {
                address: 0x14,
                data: vec![3],
            },
        ];
        let f = flatten(&segs, 0xFF, None, None, MAX_FLAT_LEN).unwrap();
        assert_eq!((f.address, f.data), (0x10, vec![1, 2, 0xFF, 0xFF, 3]));
        let f = flatten(&segs, 0, Some(0x0E), Some(0x12), MAX_FLAT_LEN).unwrap();
        assert_eq!(f.data, vec![0, 0, 1, 2]);
        let f = flatten(&segs, 0, Some(0x11), Some(0x16), MAX_FLAT_LEN).unwrap();
        assert_eq!(f.data, vec![2, 0, 0, 3, 0]);
        assert!(flatten(&segs, 0, None, None, 3).is_err());
        assert!(flatten(&segs, 0, Some(5), Some(4), 100).is_err());
        let bytes = export_bytes(
            &[1, 2, 3],
            0x8000,
            FirmwareFormat::IntelHex,
            &ExportOptions::default(),
        )
        .unwrap();
        assert_eq!(bytes, ":0380000001020377\n:00000001FF\n");
    }
}
