//! Expert Witness Format (E01/EWF-E01, DISK-08): a multi-segment reader
//! (header, volume/disk, sectors, table, hash/digest sections, zlib chunks)
//! and a single-segment EWF v1 writer with zlib chunks and an MD5 hash
//! section ("create E01").

use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use flate2::Compression;
use flate2::write::ZlibEncoder;
use hexa_core::provider::{Capabilities, FileOpenOptions, FileProvider, Provider};
use hexa_core::task::Progress;
use hexa_core::{Cancel, Result};
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};

use super::{BlockCache, for_blocks, inflate};
use crate::clone::hex;
use crate::util::{adler32, check_cancel, check_range, invalid, le32, le64, read_upto, read_vec};

const SIGNATURE: &[u8; 8] = b"EVF\x09\x0d\x0a\xff\x00";
const DESC: usize = 76;
/// Chunks per sectors/table section (limit of common acquisition tools).
const TABLE_CHUNKS: usize = 16_375;

#[derive(Debug, Clone, Copy)]
struct ChunkLoc {
    segment: usize,
    offset: u64,
    size: u64,
    compressed: bool,
}

/// An E01 image (all segments) as a provider.
#[derive(Debug)]
pub struct EwfProvider {
    segments: Vec<Arc<dyn Provider>>,
    chunk_size: u64,
    len: u64,
    chunks: Vec<ChunkLoc>,
    md5: Option<[u8; 16]>,
    header: Vec<(String, String)>,
    cache: BlockCache,
}

fn section_type(d: &[u8]) -> String {
    let end = d[..16].iter().position(|&c| c == 0).unwrap_or(16);
    String::from_utf8_lossy(&d[..end]).into_owned()
}

/// Display name of an EWF header key.
fn header_key(k: &str) -> &str {
    match k {
        "c" => "Case number",
        "n" => "Evidence number",
        "a" => "Description",
        "e" => "Examiner",
        "t" => "Notes",
        "av" => "Acquisition software version",
        "ov" => "Acquisition OS",
        "m" => "Acquired",
        "u" => "System date",
        "p" => "Password hash",
        "r" => "Compression",
        other => other,
    }
}

/// Parses the `header` section text (tab-separated keys and values).
fn parse_header(text: &str) -> Vec<(String, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let Some(ki) = lines
        .iter()
        .position(|l| l.trim_end() == "main")
        .map(|i| i + 1)
    else {
        return Vec::new();
    };
    let (Some(keys), Some(values)) = (lines.get(ki), lines.get(ki + 1)) else {
        return Vec::new();
    };
    keys.split('\t')
        .zip(values.split('\t'))
        .map(|(k, v)| {
            (
                header_key(k.trim()).to_string(),
                v.trim_end_matches('\r').to_string(),
            )
        })
        .collect()
}

impl EwfProvider {
    /// Opens `image.E01` and every following segment (`.E02` … `.E99`,
    /// `.EAA` …) next to it.
    pub fn open_path(first: &Path) -> Result<Self> {
        let mut segments: Vec<Arc<dyn Provider>> = Vec::new();
        for path in segment_paths(first) {
            if !path.exists() {
                break;
            }
            segments.push(Arc::new(FileProvider::open(
                &path,
                FileOpenOptions::default(),
            )?));
        }
        if segments.is_empty() {
            return Err(invalid("E01 segment not found"));
        }
        Self::open(segments)
    }

    /// Opens already-opened segments in order.
    pub fn open(segments: Vec<Arc<dyn Provider>>) -> Result<Self> {
        let mut chunk_size = 0u64;
        let mut sectors = 0u64;
        let mut chunks = Vec::new();
        let mut md5 = None;
        let mut header = Vec::new();
        for (si, seg) in segments.iter().enumerate() {
            let fh = read_vec(seg.as_ref(), 0, 13)?;
            if &fh[0..8] != SIGNATURE {
                return Err(invalid(format!("segment {} is not an EWF file", si + 1)));
            }
            let mut off = 13u64;
            let mut last_sectors_end = 0u64;
            let mut guard = 0;
            while off + DESC as u64 <= seg.len() && guard < 1_000_000 {
                guard += 1;
                let d = read_vec(seg.as_ref(), off, DESC)?;
                let kind = section_type(&d);
                let next = le64(&d, 16);
                let size = le64(&d, 24);
                let data_off = off + DESC as u64;
                let data_len = size.saturating_sub(DESC as u64);
                match kind.as_str() {
                    "header" if header.is_empty() => {
                        let raw =
                            read_upto(seg.as_ref(), data_off, data_len.min(1 << 20) as usize)?;
                        let mut text = Vec::new();
                        if flate2::read::ZlibDecoder::new(&raw[..])
                            .read_to_end(&mut text)
                            .is_ok()
                        {
                            header = parse_header(&String::from_utf8_lossy(&text));
                        }
                    }
                    "volume" | "disk" | "data" => {
                        let v = read_vec(seg.as_ref(), data_off, 24)?;
                        let spc = u64::from(le32(&v, 8));
                        let bps = u64::from(le32(&v, 12));
                        if chunk_size == 0 {
                            chunk_size = spc * bps;
                            sectors = le64(&v, 16);
                            if chunk_size == 0 || chunk_size > 64 << 20 {
                                return Err(invalid("bad EWF chunk geometry"));
                            }
                            sectors = sectors.min(u64::MAX / bps.max(1));
                            if bps != 512 {
                                sectors = sectors * bps / 512;
                            }
                        }
                    }
                    "sectors" => last_sectors_end = off + size,
                    "table" => {
                        let t = read_vec(seg.as_ref(), data_off, 24)?;
                        let count = le32(&t, 0) as usize;
                        let base = le64(&t, 8);
                        if count > 1 << 24 {
                            return Err(invalid("implausible EWF table"));
                        }
                        let raw = read_vec(seg.as_ref(), data_off + 24, count * 4)?;
                        let entries: Vec<u32> = raw
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .map(|c| u32::from_le_bytes(*c))
                            .collect();
                        for (i, &e) in entries.iter().enumerate() {
                            let start = base + u64::from(e & 0x7FFF_FFFF);
                            let end = entries
                                .get(i + 1)
                                .map_or(last_sectors_end, |&n| base + u64::from(n & 0x7FFF_FFFF));
                            chunks.push(ChunkLoc {
                                segment: si,
                                offset: start,
                                size: end.saturating_sub(start),
                                compressed: e & 0x8000_0000 != 0,
                            });
                        }
                    }
                    "hash" => {
                        let h = read_vec(seg.as_ref(), data_off, 16)?;
                        md5 = Some(h.try_into().expect("16 bytes"));
                    }
                    "digest" if md5.is_none() => {
                        let h = read_vec(seg.as_ref(), data_off, 16)?;
                        md5 = Some(h.try_into().expect("16 bytes"));
                    }
                    "done" | "next" => break,
                    _ => {}
                }
                if next <= off {
                    break;
                }
                off = next;
            }
        }
        if chunk_size == 0 {
            return Err(invalid("EWF image has no volume section"));
        }
        Ok(Self {
            segments,
            chunk_size,
            len: sectors * 512,
            chunks,
            md5,
            header,
            cache: BlockCache::new(32),
        })
    }

    /// MD5 stored by the acquiring tool (lowercase hex).
    pub fn stored_md5(&self) -> Option<String> {
        self.md5.map(|m| hex(&m))
    }

    /// Case metadata from the `header` section.
    pub fn case_info(&self) -> &[(String, String)] {
        &self.header
    }

    pub fn chunk_size(&self) -> u64 {
        self.chunk_size
    }

    fn chunk(&self, idx: u64) -> io::Result<Arc<[u8]>> {
        self.cache.get_or_load(idx, || {
            let c = self
                .chunks
                .get(idx as usize)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing EWF chunk"))?;
            let seg = &self.segments[c.segment];
            let raw = read_upto(
                seg.as_ref(),
                c.offset,
                c.size.min(self.chunk_size + 1024) as usize,
            )?;
            if c.compressed {
                inflate(&raw, self.chunk_size as usize, true)
            } else {
                let mut v = raw;
                v.resize(self.chunk_size as usize, 0);
                Ok(v)
            }
        })
    }
}

impl Provider for EwfProvider {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        check_range(offset, buf.len(), self.len)?;
        for_blocks(offset, buf, self.chunk_size, |idx, within, dst| {
            let data = self.chunk(idx)?;
            dst.copy_from_slice(&data[within..within + dst.len()]);
            Ok(())
        })
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            sector_size: Some(512),
            ..Capabilities::READ_ONLY
        }
    }

    fn name(&self) -> String {
        self.segments[0].name()
    }

    fn invalidate(&self) {
        self.cache.clear();
    }
}

/// Segment file names after `first`: `.E01` … `.E99`, `.EAA` … `.EZZ`.
fn segment_paths(first: &Path) -> Vec<PathBuf> {
    let ext = first.extension().and_then(|e| e.to_str()).unwrap_or("E01");
    let lead = ext.chars().next().unwrap_or('E');
    let mut names: Vec<String> = (1..=99).map(|n| format!("{lead}{n:02}")).collect();
    for a in b'A'..=b'Z' {
        for b in b'A'..=b'Z' {
            names.push(format!("{lead}{}{}", a as char, b as char));
        }
    }
    let lower = ext.chars().all(|c| !c.is_ascii_uppercase());
    let start = names
        .iter()
        .position(|n| n.eq_ignore_ascii_case(ext))
        .unwrap_or(0);
    names[start..]
        .iter()
        .map(|n| first.with_extension(if lower { n.to_lowercase() } else { n.clone() }))
        .collect()
}

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

/// Case metadata and parameters for [`create_e01`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct E01Options {
    pub case_number: String,
    pub evidence_number: String,
    pub description: String,
    pub examiner: String,
    pub notes: String,
    /// zlib level 0–9.
    pub compression: u32,
    pub sectors_per_chunk: u32,
}

impl Default for E01Options {
    fn default() -> Self {
        Self {
            case_number: String::new(),
            evidence_number: String::new(),
            description: String::new(),
            examiner: String::new(),
            notes: String::new(),
            compression: 6,
            sectors_per_chunk: 64,
        }
    }
}

/// Result of [`create_e01`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct E01Report {
    /// Bytes of media written (source length rounded up to whole sectors).
    pub bytes: u64,
    pub chunks: u64,
    pub file_size: u64,
    /// MD5 of the media, also stored in the hash section.
    pub md5: String,
}

fn descriptor(kind: &str, size: u64, next: u64) -> [u8; DESC] {
    let mut d = [0u8; DESC];
    d[..kind.len()].copy_from_slice(kind.as_bytes());
    d[16..24].copy_from_slice(&next.to_le_bytes());
    d[24..32].copy_from_slice(&size.to_le_bytes());
    let c = adler32(&d[..72]);
    d[72..76].copy_from_slice(&c.to_le_bytes());
    d
}

struct SectionWriter<W: Write + Seek> {
    w: W,
    pos: u64,
}

impl<W: Write + Seek> SectionWriter<W> {
    /// Writes a complete section whose next section follows immediately.
    fn section(&mut self, kind: &str, body: &[u8]) -> io::Result<()> {
        let size = (DESC + body.len()) as u64;
        let d = descriptor(kind, size, self.pos + size);
        self.w.write_all(&d)?;
        self.w.write_all(body)?;
        self.pos += size;
        Ok(())
    }
}

fn header_text(o: &E01Options) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let days = now.div_euclid(86_400);
    let secs = now.rem_euclid(86_400);
    // Civil date from days since the epoch.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let date = format!(
        "{year} {month} {day} {} {} {}",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    );
    let clean = |s: &str| s.replace(['\t', '\n', '\r'], " ");
    let level = match o.compression {
        0 => "n",
        1..=5 => "f",
        _ => "b",
    };
    format!(
        "1\nmain\nc\tn\ta\te\tt\tav\tov\tm\tu\tp\tr\n{}\t{}\t{}\t{}\t{}\tHexa {}\t{}\t{date}\t{date}\t0\t{level}\n\n",
        clean(&o.case_number),
        clean(&o.evidence_number),
        clean(&o.description),
        clean(&o.examiner),
        clean(&o.notes),
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
    )
}

fn zlib(data: &[u8], level: u32) -> io::Result<Vec<u8>> {
    let mut e = ZlibEncoder::new(Vec::new(), Compression::new(level.min(9)));
    e.write_all(data)?;
    e.finish()
}

/// Writes `src` as a single-segment EWF-E01 image (DISK-08).
pub fn create_e01(
    src: &dyn Provider,
    dest: &Path,
    opts: &E01Options,
    progress: Progress<'_>,
    cancel: Option<&Cancel>,
) -> Result<E01Report> {
    let spc = opts.sectors_per_chunk.clamp(1, 32_768);
    let chunk = u64::from(spc) * 512;
    let sectors = src.len().div_ceil(512);
    let media = sectors * 512;
    let chunk_count = media.div_ceil(chunk);
    if chunk_count > u64::from(u32::MAX) {
        return Err(invalid("image too large for EWF v1"));
    }
    let file = File::create(dest)?;
    let mut w = SectionWriter {
        w: BufWriter::new(file),
        pos: 0,
    };
    w.w.write_all(SIGNATURE)?;
    w.w.write_all(&[1, 1, 0, 0, 0])?;
    w.pos = 13;
    w.section("header", &zlib(header_text(opts).as_bytes(), 9)?)?;

    let mut vol = vec![0u8; 1052];
    vol[0] = 1;
    vol[4..8].copy_from_slice(&(chunk_count as u32).to_le_bytes());
    vol[8..12].copy_from_slice(&spc.to_le_bytes());
    vol[12..16].copy_from_slice(&512u32.to_le_bytes());
    vol[16..24].copy_from_slice(&sectors.to_le_bytes());
    vol[0x34] = match opts.compression {
        0 => 0,
        1..=5 => 1,
        _ => 2,
    };
    vol[0x38..0x3C].copy_from_slice(&spc.to_le_bytes());
    let c = adler32(&vol[..1048]);
    vol[1048..1052].copy_from_slice(&c.to_le_bytes());
    w.section("volume", &vol)?;

    let mut md5 = Md5::new();
    let mut buf = vec![0u8; chunk as usize];
    let mut done_chunks = 0u64;
    while done_chunks < chunk_count {
        // Sectors section: descriptor written after its size is known.
        // Table offsets are 31-bit and relative to the section start.
        let sec_start = w.pos;
        w.w.write_all(&[0u8; DESC])?;
        w.pos += DESC as u64;
        let mut entries = Vec::new();
        while done_chunks < chunk_count
            && entries.len() < TABLE_CHUNKS
            && w.pos - sec_start + chunk + 4 < 0x7FFF_FFFF
        {
            check_cancel(cancel)?;
            let off = done_chunks * chunk;
            let n = (media - off).min(chunk) as usize;
            let avail = src.len().saturating_sub(off).min(n as u64) as usize;
            src.read_at(off, &mut buf[..avail])?;
            buf[avail..n].fill(0);
            md5.update(&buf[..n]);
            let rel = w.pos - sec_start;
            let compressed = if opts.compression > 0 {
                let z = zlib(&buf[..n], opts.compression)?;
                (z.len() < n).then_some(z)
            } else {
                None
            };
            if let Some(z) = compressed {
                w.w.write_all(&z)?;
                w.pos += z.len() as u64;
                entries.push(rel as u32 | 0x8000_0000);
            } else {
                w.w.write_all(&buf[..n])?;
                w.w.write_all(&adler32(&buf[..n]).to_le_bytes())?;
                w.pos += n as u64 + 4;
                entries.push(rel as u32);
            }
            done_chunks += 1;
            progress(done_chunks * chunk, chunk_count * chunk);
        }
        let end = w.pos;
        w.w.seek(SeekFrom::Start(sec_start))?;
        w.w.write_all(&descriptor("sectors", end - sec_start, end))?;
        w.w.seek(SeekFrom::Start(end))?;
        let mut table = vec![0u8; 24];
        table[0..4].copy_from_slice(&(entries.len() as u32).to_le_bytes());
        table[8..16].copy_from_slice(&sec_start.to_le_bytes());
        let c = adler32(&table[..20]);
        table[20..24].copy_from_slice(&c.to_le_bytes());
        let raw: Vec<u8> = entries.iter().flat_map(|e| e.to_le_bytes()).collect();
        table.extend_from_slice(&raw);
        table.extend_from_slice(&adler32(&raw).to_le_bytes());
        w.section("table", &table)?;
        w.section("table2", &table)?;
    }
    let digest: [u8; 16] = md5.finalize().into();
    let mut hash = vec![0u8; 36];
    hash[..16].copy_from_slice(&digest);
    let c = adler32(&hash[..32]);
    hash[32..36].copy_from_slice(&c.to_le_bytes());
    w.section("hash", &hash)?;
    // The done section points at itself.
    let done = descriptor("done", DESC as u64, w.pos);
    w.w.write_all(&done)?;
    w.pos += DESC as u64;
    let file = w.w.into_inner().map_err(io::IntoInnerError::into_error)?;
    file.sync_all()?;
    Ok(E01Report {
        bytes: media,
        chunks: chunk_count,
        file_size: w.pos,
        md5: hex(&digest),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::{ImageFormat, detect_format, open_image};
    use crate::util::Rng;
    use hexa_core::provider::MemoryProvider;
    use hexa_core::task::no_progress;

    #[test]
    fn e01_round_trip() {
        let mut data = vec![0u8; 300_000];
        Rng::from_seed(1).fill(&mut data[100_000..150_000]);
        data[200_000..]
            .iter_mut()
            .enumerate()
            .for_each(|(i, b)| *b = (i % 7) as u8);
        let src = MemoryProvider::new("disk", data.clone());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("evidence.E01");
        let opts = E01Options {
            case_number: "2026-001".into(),
            examiner: "QA".into(),
            ..E01Options::default()
        };
        let report = create_e01(&src, &path, &opts, &no_progress, None).unwrap();
        assert_eq!(report.bytes, 300_032);
        assert_eq!(report.chunks, 300_032u64.div_ceil(32_768));
        assert!(report.file_size < 200_000);

        let p = open_image(&path).unwrap();
        let file = FileProvider::open(&path, FileOpenOptions::default()).unwrap();
        assert_eq!(detect_format(&file).unwrap(), ImageFormat::Ewf);
        assert_eq!(p.len(), 300_032);
        let mut out = vec![0u8; 300_032];
        p.read_at(0, &mut out).unwrap();
        assert_eq!(&out[..300_000], &data[..]);
        assert!(out[300_000..].iter().all(|&b| b == 0));

        let ewf = EwfProvider::open_path(&path).unwrap();
        assert_eq!(ewf.stored_md5().unwrap(), report.md5);
        assert_eq!(report.md5, hex(&Md5::digest(&out)));
        assert!(
            ewf.case_info()
                .iter()
                .any(|(k, v)| k == "Case number" && v == "2026-001")
        );
        assert!(
            ewf.case_info()
                .iter()
                .any(|(k, v)| k == "Examiner" && v == "QA")
        );
    }

    #[test]
    fn uncompressed_chunks_and_segment_names() {
        let data: Vec<u8> = (0..70_000u32).map(|i| (i * 13) as u8).collect();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("raw.e01");
        let opts = E01Options {
            compression: 0,
            sectors_per_chunk: 16,
            ..E01Options::default()
        };
        create_e01(
            &MemoryProvider::new("d", data.clone()),
            &path,
            &opts,
            &no_progress,
            None,
        )
        .unwrap();
        let p = EwfProvider::open_path(&path).unwrap();
        let mut out = vec![0u8; 70_000];
        p.read_at(0, &mut out).unwrap();
        assert_eq!(out, data);
        let names = segment_paths(Path::new("/x/img.E99"));
        assert_eq!(names[1], PathBuf::from("/x/img.EAA"));
        assert_eq!(segment_paths(Path::new("a.e01"))[1], PathBuf::from("a.e02"));
    }
}
