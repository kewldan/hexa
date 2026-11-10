//! Previews of a selection (ANL-16): images (PNG, JPEG, GIF, BMP, ICO,
//! WebP) → RGBA, WAV / raw PCM → waveform summary, STL / OBJ → triangle
//! mesh. Embedded-file detection lives in [`crate::embedded`].

use std::io::Cursor;

use hexa_core::task::Progress;
use hexa_core::{Cancel, Error, Result, Tree};
use serde::{Deserialize, Serialize};

use crate::util::{self, CHUNK, invalid};

// ------------------------------------------------------------------ images

/// Decoded image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    /// `png`, `jpeg`, `gif`, `bmp`, `ico` or `webp`.
    pub format: String,
    /// `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

/// Largest width / height accepted by [`decode_image`].
pub const MAX_IMAGE_DIMENSION: u32 = 16_384;
/// Allocation limit of the image decoder.
pub const MAX_IMAGE_ALLOC: u64 = 512 << 20;

fn format_name(f: image::ImageFormat) -> &'static str {
    match f {
        image::ImageFormat::Png => "png",
        image::ImageFormat::Jpeg => "jpeg",
        image::ImageFormat::Gif => "gif",
        image::ImageFormat::Bmp => "bmp",
        image::ImageFormat::Ico => "ico",
        image::ImageFormat::WebP => "webp",
        _ => "other",
    }
}

/// Detects the image format from its signature.
pub fn image_format(bytes: &[u8]) -> Option<&'static str> {
    image::guess_format(bytes).ok().map(format_name)
}

/// Decodes an image (first frame of animations) to RGBA.
pub fn decode_image(bytes: &[u8]) -> Result<DecodedImage> {
    let reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| invalid(format!("cannot read image: {e}")))?;
    let format = reader
        .format()
        .ok_or_else(|| invalid("unknown image format"))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_IMAGE_ALLOC);
    let mut reader = reader;
    reader.limits(limits);
    let img = reader
        .decode()
        .map_err(|e| invalid(format!("cannot decode image: {e}")))?
        .to_rgba8();
    Ok(DecodedImage {
        width: img.width(),
        height: img.height(),
        format: format_name(format).into(),
        rgba: img.into_raw(),
    })
}

/// Decodes the image stored at `[start, end)` of a snapshot.
pub fn decode_image_range(tree: &Tree, start: u64, end: u64) -> Result<DecodedImage> {
    decode_image(&util::read_all(tree, start, end, MAX_IMAGE_ALLOC)?)
}

// ------------------------------------------------------------------- audio

/// Sample encoding of PCM audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SampleFormat {
    /// Unsigned 8-bit (WAV).
    U8,
    S8,
    S16,
    S24,
    S32,
    F32,
    F64,
    /// G.711 A-law.
    ALaw,
    /// G.711 µ-law.
    MuLaw,
}

impl SampleFormat {
    pub fn bytes(self) -> usize {
        match self {
            Self::U8 | Self::S8 | Self::ALaw | Self::MuLaw => 1,
            Self::S16 => 2,
            Self::S24 => 3,
            Self::S32 | Self::F32 => 4,
            Self::F64 => 8,
        }
    }

    /// Decodes one sample to `-1.0 … 1.0`.
    pub fn decode(self, b: &[u8], big_endian: bool) -> f32 {
        let int = |n: usize| -> i64 {
            let mut v: u64 = 0;
            for i in 0..n {
                let byte = if big_endian { b[i] } else { b[n - 1 - i] };
                v = (v << 8) | u64::from(byte);
            }
            let shift = 64 - 8 * n as u32;
            ((v << shift) as i64) >> shift
        };
        match self {
            Self::U8 => (f32::from(b[0]) - 128.0) / 128.0,
            Self::S8 => f32::from(b[0] as i8) / 128.0,
            Self::S16 => int(2) as f32 / 32_768.0,
            Self::S24 => int(3) as f32 / 8_388_608.0,
            Self::S32 => (int(4) as f64 / 2_147_483_648.0) as f32,
            Self::F32 => {
                let a = b[..4].try_into().expect("4 bytes");
                if big_endian {
                    f32::from_be_bytes(a)
                } else {
                    f32::from_le_bytes(a)
                }
            }
            Self::F64 => {
                let a = b[..8].try_into().expect("8 bytes");
                (if big_endian {
                    f64::from_be_bytes(a)
                } else {
                    f64::from_le_bytes(a)
                }) as f32
            }
            Self::ALaw => f32::from(alaw_to_linear(b[0])) / 32_768.0,
            Self::MuLaw => f32::from(ulaw_to_linear(b[0])) / 32_768.0,
        }
    }
}

/// G.711 A-law expansion to 16-bit linear PCM.
pub fn alaw_to_linear(a: u8) -> i16 {
    let a = a ^ 0x55;
    let mut t = i16::from(a & 0x0F) << 4;
    let seg = (a & 0x70) >> 4;
    match seg {
        0 => t += 8,
        1 => t += 0x108,
        _ => {
            t += 0x108;
            t <<= seg - 1;
        }
    }
    if a & 0x80 != 0 { t } else { -t }
}

/// G.711 µ-law expansion to 16-bit linear PCM.
pub fn ulaw_to_linear(u: u8) -> i16 {
    let u = !u;
    let t = ((i32::from(u & 0x0F) << 3) + 0x84) << ((u & 0x70) >> 4);
    (if u & 0x80 != 0 { 0x84 - t } else { t - 0x84 }) as i16
}

/// Layout of interleaved PCM data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PcmFormat {
    pub sample: SampleFormat,
    pub channels: u16,
    pub sample_rate: u32,
    pub big_endian: bool,
}

/// Metadata of a WAV file.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WavInfo {
    pub format: PcmFormat,
    /// WAVE format tag (1 PCM, 3 float, 6 A-law, 7 µ-law, 0xFFFE extensible).
    pub format_tag: u16,
    pub bits_per_sample: u16,
    pub block_align: u16,
    /// Offset of the sample data relative to the start of the RIFF header.
    pub data_offset: u64,
    /// Length of the sample data (clamped to the available bytes).
    pub data_len: u64,
    pub frames: u64,
    pub duration_secs: f64,
}

/// Parses a RIFF/RIFX WAVE header. `read(offset, len)` returns up to `len`
/// bytes of the file at `offset`; `total` is the file length.
fn parse_wav_with(
    total: u64,
    read: &mut dyn FnMut(u64, usize) -> Result<Vec<u8>>,
) -> Result<WavInfo> {
    let head = read(0, 12)?;
    if head.len() < 12 || &head[8..12] != b"WAVE" {
        return Err(invalid("not a WAVE file"));
    }
    let big = match &head[..4] {
        b"RIFF" => false,
        b"RIFX" => true,
        _ => return Err(invalid("not a RIFF file")),
    };
    let u16v = |b: &[u8]| {
        if big {
            u16::from_be_bytes([b[0], b[1]])
        } else {
            u16::from_le_bytes([b[0], b[1]])
        }
    };
    let u32v = |b: &[u8]| {
        let a = [b[0], b[1], b[2], b[3]];
        if big {
            u32::from_be_bytes(a)
        } else {
            u32::from_le_bytes(a)
        }
    };
    let mut pos = 12u64;
    let mut fmt: Option<(u16, u16, u32, u16, u16)> = None;
    for _ in 0..1024 {
        let h = read(pos, 8)?;
        if h.len() < 8 {
            break;
        }
        let size = u64::from(u32v(&h[4..8]));
        let body = pos + 8;
        match &h[..4] {
            b"fmt " => {
                let f = read(body, size.min(64) as usize)?;
                if f.len() < 16 {
                    return Err(invalid("truncated fmt chunk"));
                }
                let mut tag = u16v(&f[0..2]);
                if tag == 0xFFFE && f.len() >= 26 {
                    tag = u16v(&f[24..26]);
                }
                fmt = Some((
                    tag,
                    u16v(&f[2..4]),
                    u32v(&f[4..8]),
                    u16v(&f[12..14]),
                    u16v(&f[14..16]),
                ));
            }
            b"data" => {
                let (tag, channels, rate, align, bits) =
                    fmt.ok_or_else(|| invalid("data chunk before fmt chunk"))?;
                let sample = match (tag, bits) {
                    (1, 8) => SampleFormat::U8,
                    (1, 16) => SampleFormat::S16,
                    (1, 24) => SampleFormat::S24,
                    (1, 32) => SampleFormat::S32,
                    (3, 32) => SampleFormat::F32,
                    (3, 64) => SampleFormat::F64,
                    (6, 8) => SampleFormat::ALaw,
                    (7, 8) => SampleFormat::MuLaw,
                    _ => {
                        return Err(invalid(format!(
                            "unsupported WAVE encoding (tag {tag}, {bits} bits)"
                        )));
                    }
                };
                if channels == 0 {
                    return Err(invalid("WAVE file has no channels"));
                }
                let available = total.saturating_sub(body);
                let len = if size == 0 || size == u64::from(u32::MAX) {
                    available
                } else {
                    size.min(available)
                };
                let frame = (sample.bytes() * usize::from(channels)) as u64;
                let frames = len / frame;
                return Ok(WavInfo {
                    format: PcmFormat {
                        sample,
                        channels,
                        sample_rate: rate,
                        big_endian: big,
                    },
                    format_tag: tag,
                    bits_per_sample: bits,
                    block_align: align,
                    data_offset: body,
                    data_len: len,
                    frames,
                    duration_secs: if rate == 0 {
                        0.0
                    } else {
                        frames as f64 / f64::from(rate)
                    },
                });
            }
            _ => {}
        }
        pos = body + size + (size & 1);
    }
    Err(invalid("WAVE file has no data chunk"))
}

/// Parses the header of a WAV file held in memory.
pub fn parse_wav(bytes: &[u8]) -> Result<WavInfo> {
    parse_wav_with(bytes.len() as u64, &mut |off, n| {
        let s = (off as usize).min(bytes.len());
        Ok(bytes[s..(s + n).min(bytes.len())].to_vec())
    })
}

/// Parses the header of a WAV file starting at `offset` of a snapshot;
/// the returned `data_offset` is relative to `offset`.
pub fn wav_info(tree: &Tree, offset: u64) -> Result<WavInfo> {
    let total = tree.len().saturating_sub(offset);
    parse_wav_with(total, &mut |off, n| Ok(tree.read_vec(offset + off, n)?))
}

/// Min/max per bucket and channel, for drawing a waveform.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Waveform {
    pub channels: u16,
    pub frames: u64,
    /// Frames per bucket (the last bucket may hold fewer).
    pub frames_per_bucket: u64,
    /// `min[channel][bucket]`, normalised to `-1 … 1`.
    pub min: Vec<Vec<f32>>,
    pub max: Vec<Vec<f32>>,
    /// Peak absolute amplitude per channel.
    pub peak: Vec<f32>,
}

#[derive(Debug)]
struct WaveAcc {
    fmt: PcmFormat,
    per: u64,
    frame: u64,
    min: Vec<Vec<f32>>,
    max: Vec<Vec<f32>>,
    peak: Vec<f32>,
}

impl WaveAcc {
    fn new(fmt: PcmFormat, frames: u64, buckets: usize) -> Result<Self> {
        if fmt.channels == 0 || buckets == 0 {
            return Err(invalid("waveform needs channels and buckets"));
        }
        let per = frames.div_ceil(buckets as u64).max(1);
        let n = frames.div_ceil(per) as usize;
        let ch = usize::from(fmt.channels);
        Ok(Self {
            fmt,
            per,
            frame: 0,
            min: vec![vec![0.0; n]; ch],
            max: vec![vec![0.0; n]; ch],
            peak: vec![0.0; ch],
        })
    }

    /// Feeds whole frames.
    fn feed(&mut self, data: &[u8]) {
        let ss = self.fmt.sample.bytes();
        let fs = ss * usize::from(self.fmt.channels);
        for frame in data.chunks_exact(fs) {
            let b = (self.frame / self.per) as usize;
            let first = self.frame.is_multiple_of(self.per);
            for (c, s) in frame.chunks_exact(ss).enumerate() {
                let v = self.fmt.sample.decode(s, self.fmt.big_endian);
                if !v.is_finite() {
                    continue;
                }
                if first {
                    self.min[c][b] = v;
                    self.max[c][b] = v;
                } else {
                    self.min[c][b] = self.min[c][b].min(v);
                    self.max[c][b] = self.max[c][b].max(v);
                }
                self.peak[c] = self.peak[c].max(v.abs());
            }
            self.frame += 1;
        }
    }

    fn finish(self) -> Waveform {
        Waveform {
            channels: self.fmt.channels,
            frames: self.frame,
            frames_per_bucket: self.per,
            min: self.min,
            max: self.max,
            peak: self.peak,
        }
    }
}

/// Waveform of interleaved PCM in memory.
pub fn waveform_bytes(data: &[u8], fmt: &PcmFormat, buckets: usize) -> Result<Waveform> {
    let frame = (fmt.sample.bytes() * usize::from(fmt.channels)).max(1);
    let mut acc = WaveAcc::new(*fmt, (data.len() / frame) as u64, buckets)?;
    acc.feed(data);
    Ok(acc.finish())
}

/// Waveform of interleaved PCM at `[start, start + len)` of a snapshot.
pub fn waveform(
    tree: &Tree,
    start: u64,
    len: u64,
    fmt: &PcmFormat,
    buckets: usize,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<Waveform> {
    let frame = (fmt.sample.bytes() * usize::from(fmt.channels)).max(1);
    let (start, end) = util::clamp(tree, start, start.saturating_add(len));
    let frames = (end - start) / frame as u64;
    let mut acc = WaveAcc::new(*fmt, frames, buckets)?;
    let chunk = CHUNK / frame * frame;
    let end = start + frames * frame as u64;
    util::scan(tree, start, end, chunk, cancel, progress, |_, c| {
        acc.feed(c);
        Ok(true)
    })?;
    Ok(acc.finish())
}

// -------------------------------------------------------------------- mesh

/// Triangle mesh with flat `xyz` positions and triangle indices.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Mesh {
    /// `stl-binary`, `stl-ascii` or `obj`.
    pub format: String,
    pub positions: Vec<f32>,
    /// Three indices per triangle.
    pub indices: Vec<u32>,
    pub bounds_min: [f32; 3],
    pub bounds_max: [f32; 3],
    pub vertex_count: u32,
    pub triangle_count: u32,
}

/// Largest triangle count accepted.
pub const MAX_TRIANGLES: u64 = 10_000_000;

fn finish_mesh(format: &str, positions: Vec<f32>, indices: Vec<u32>) -> Mesh {
    let mut lo = [f32::INFINITY; 3];
    let mut hi = [f32::NEG_INFINITY; 3];
    for p in positions.as_chunks::<3>().0 {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    if positions.is_empty() {
        lo = [0.0; 3];
        hi = [0.0; 3];
    }
    Mesh {
        format: format.into(),
        vertex_count: (positions.len() / 3) as u32,
        triangle_count: (indices.len() / 3) as u32,
        positions,
        indices,
        bounds_min: lo,
        bounds_max: hi,
    }
}

fn is_binary_stl(b: &[u8]) -> bool {
    if b.len() < 84 {
        return false;
    }
    let n = u64::from(u32::from_le_bytes(b[80..84].try_into().expect("4 bytes")));
    let need = 84 + 50 * n;
    need == b.len() as u64
        || (need <= b.len() as u64 && !b.trim_ascii_start().starts_with(b"solid"))
}

/// Parses an ASCII or binary STL file.
pub fn parse_stl(b: &[u8]) -> Result<Mesh> {
    if is_binary_stl(b) {
        let n = u64::from(u32::from_le_bytes(b[80..84].try_into().expect("4 bytes")));
        if n > MAX_TRIANGLES {
            return Err(Error::TooLarge {
                len: n,
                limit: MAX_TRIANGLES,
            });
        }
        let mut pos = Vec::with_capacity(n as usize * 9);
        for t in b[84..84 + 50 * n as usize].as_chunks::<50>().0 {
            for f in t[12..48].as_chunks::<4>().0 {
                pos.push(f32::from_le_bytes(*f));
            }
        }
        let idx = (0..n as u32 * 3).collect();
        return Ok(finish_mesh("stl-binary", pos, idx));
    }
    let text = std::str::from_utf8(b).map_err(|_| invalid("STL is neither binary nor ASCII"))?;
    if !text.trim_start().starts_with("solid") {
        return Err(invalid("not an STL file"));
    }
    let mut pos = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let mut it = line.split_whitespace();
        if it.next() == Some("vertex") {
            for _ in 0..3 {
                let v = it
                    .next()
                    .and_then(|s| s.parse::<f32>().ok())
                    .ok_or_else(|| invalid(format!("line {}: bad vertex", i + 1)))?;
                pos.push(v);
            }
            if pos.len() as u64 > MAX_TRIANGLES * 9 {
                return Err(Error::TooLarge {
                    len: pos.len() as u64 / 9,
                    limit: MAX_TRIANGLES,
                });
            }
        }
    }
    if pos.len() % 9 != 0 {
        return Err(invalid("STL facet with a vertex count other than 3"));
    }
    let idx = (0..(pos.len() / 3) as u32).collect();
    Ok(finish_mesh("stl-ascii", pos, idx))
}

/// Parses a Wavefront OBJ (positions and faces; polygons are fan-triangulated,
/// negative indices are relative, texture / normal references are ignored).
pub fn parse_obj(b: &[u8]) -> Result<Mesh> {
    let text = String::from_utf8_lossy(b);
    let mut pos: Vec<f32> = Vec::new();
    let mut idx: Vec<u32> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let lineno = i + 1;
        let mut it = line.split_whitespace();
        match it.next() {
            Some("v") => {
                for _ in 0..3 {
                    let v = it
                        .next()
                        .and_then(|s| s.parse::<f32>().ok())
                        .ok_or_else(|| invalid(format!("line {lineno}: bad vertex")))?;
                    pos.push(v);
                }
            }
            Some("f") => {
                let count = (pos.len() / 3) as i64;
                let face = it
                    .map(|tok| {
                        let n: i64 = tok
                            .split('/')
                            .next()
                            .and_then(|s| s.parse().ok())
                            .ok_or_else(|| {
                                invalid(format!("line {lineno}: bad face index {tok:?}"))
                            })?;
                        let k = if n < 0 { count + n } else { n - 1 };
                        if n == 0 || !(0..count).contains(&k) {
                            return Err(invalid(format!(
                                "line {lineno}: face index {n} out of range"
                            )));
                        }
                        Ok(k as u32)
                    })
                    .collect::<Result<Vec<u32>>>()?;
                if face.len() < 3 {
                    return Err(invalid(format!("line {lineno}: face needs 3 vertices")));
                }
                for k in 1..face.len() - 1 {
                    idx.extend([face[0], face[k], face[k + 1]]);
                }
                if idx.len() as u64 > MAX_TRIANGLES * 3 {
                    return Err(Error::TooLarge {
                        len: idx.len() as u64 / 3,
                        limit: MAX_TRIANGLES,
                    });
                }
            }
            _ => {}
        }
    }
    if pos.is_empty() {
        return Err(invalid("OBJ has no vertices"));
    }
    Ok(finish_mesh("obj", pos, idx))
}

/// Parses STL or OBJ, detected from the content.
pub fn parse_mesh(b: &[u8]) -> Result<Mesh> {
    let head = b.trim_ascii_start();
    if is_binary_stl(b) || head.starts_with(b"solid") {
        parse_stl(b)
    } else {
        parse_obj(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{noop, tree};

    fn encode(img: &image::DynamicImage, f: image::ImageFormat) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        img.write_to(&mut out, f).unwrap();
        out.into_inner()
    }

    fn sample_image() -> image::RgbaImage {
        image::RgbaImage::from_fn(3, 2, |x, y| {
            image::Rgba([x as u8 * 100, y as u8 * 200, 7, 255])
        })
    }

    #[test]
    fn decodes_lossless_formats() {
        let src = sample_image();
        for (f, name) in [
            (image::ImageFormat::Png, "png"),
            (image::ImageFormat::Bmp, "bmp"),
            (image::ImageFormat::Ico, "ico"),
            (image::ImageFormat::WebP, "webp"),
            (image::ImageFormat::Gif, "gif"),
        ] {
            let bytes = encode(&image::DynamicImage::ImageRgba8(src.clone()), f);
            assert_eq!(image_format(&bytes), Some(name));
            let d = decode_image(&bytes).unwrap();
            assert_eq!((d.width, d.height, d.format.as_str()), (3, 2, name));
            if name != "gif" {
                assert_eq!(d.rgba, src.as_raw().clone(), "{name}");
            }
        }
        let jpeg = encode(
            &image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                8,
                8,
                image::Rgb([200, 10, 10]),
            )),
            image::ImageFormat::Jpeg,
        );
        let d = decode_image(&jpeg).unwrap();
        assert_eq!((d.width, d.format.as_str()), (8, "jpeg"));
        assert!(d.rgba[0] > 150 && d.rgba[1] < 60);
        let mut padded = vec![0u8; 10];
        padded.extend(&encode(
            &image::DynamicImage::ImageRgba8(src),
            image::ImageFormat::Png,
        ));
        let r = decode_image_range(&tree(&padded), 10, padded.len() as u64).unwrap();
        assert_eq!(r.width, 3);
        assert!(decode_image(b"not an image").is_err());
    }

    fn wav(tag: u16, channels: u16, bits: u16, data: &[u8]) -> Vec<u8> {
        let mut w = Vec::new();
        w.extend(b"RIFF");
        w.extend((36 + 12 + data.len() as u32).to_le_bytes());
        w.extend(b"WAVE");
        w.extend(b"LIST");
        w.extend(4u32.to_le_bytes());
        w.extend(b"INFO");
        w.extend(b"fmt ");
        w.extend(16u32.to_le_bytes());
        w.extend(tag.to_le_bytes());
        w.extend(channels.to_le_bytes());
        w.extend(8000u32.to_le_bytes());
        let align = channels * bits / 8;
        w.extend((8000 * u32::from(align)).to_le_bytes());
        w.extend(align.to_le_bytes());
        w.extend(bits.to_le_bytes());
        w.extend(b"data");
        w.extend((data.len() as u32).to_le_bytes());
        w.extend(data);
        w
    }

    #[test]
    fn wav_header_and_waveform() {
        // Stereo 16-bit: left ramps, right is silent.
        let data: Vec<u8> = (0..8000i16)
            .flat_map(|i| {
                let l = (i - 4000) * 8;
                [l.to_le_bytes(), 0i16.to_le_bytes()].concat()
            })
            .collect();
        let file = wav(1, 2, 16, &data);
        let info = parse_wav(&file).unwrap();
        assert_eq!(info.format.sample, SampleFormat::S16);
        assert_eq!(info.format.channels, 2);
        assert_eq!(info.frames, 8000);
        assert!((info.duration_secs - 1.0).abs() < 1e-9);
        assert_eq!(info.data_offset, 56);
        let t = tree(&file);
        assert_eq!(wav_info(&t, 0).unwrap(), info);
        let w = waveform(
            &t,
            info.data_offset,
            info.data_len,
            &info.format,
            4,
            &Cancel::new(),
            &noop,
        )
        .unwrap();
        assert_eq!(w.frames_per_bucket, 2000);
        assert_eq!(w.min[0].len(), 4);
        assert!((w.min[0][0] - (-32000.0 / 32768.0)).abs() < 1e-6);
        assert!(w.max[0][3] > 0.97);
        assert_eq!((w.min[1][2], w.max[1][2], w.peak[1]), (0.0, 0.0, 0.0));
        assert_eq!(w, waveform_bytes(&data, &info.format, 4).unwrap());

        let f = parse_wav(&wav(3, 1, 32, &0.5f32.to_le_bytes())).unwrap();
        assert_eq!(f.format.sample, SampleFormat::F32);
        let m = parse_wav(&wav(7, 1, 8, &[0xFF])).unwrap();
        assert_eq!(m.format.sample, SampleFormat::MuLaw);
        assert!(parse_wav(b"RIFF\0\0\0\0AVI ").is_err());
        assert!(parse_wav(&wav(2, 1, 4, &[0])).is_err());
    }

    #[test]
    fn sample_decoding() {
        assert_eq!(SampleFormat::U8.decode(&[128], false), 0.0);
        assert_eq!(SampleFormat::S16.decode(&[0x00, 0x80], false), -1.0);
        assert_eq!(SampleFormat::S16.decode(&[0x40, 0x00], true), 0.5);
        assert_eq!(SampleFormat::S24.decode(&[0, 0, 0xC0], false), -0.5);
        assert_eq!(SampleFormat::S32.decode(&[0, 0, 0, 0x40], false), 0.5);
        assert_eq!(SampleFormat::F64.decode(&0.25f64.to_be_bytes(), true), 0.25);
        assert_eq!(ulaw_to_linear(0xFF), 0);
        assert_eq!(ulaw_to_linear(0x00), -32124);
        assert_eq!(ulaw_to_linear(0x80), 32124);
        assert_eq!(alaw_to_linear(0xD5), 8);
        assert_eq!(alaw_to_linear(0x55), -8);
        assert_eq!(alaw_to_linear(0xAA), 32256);
    }

    #[test]
    fn stl_and_obj() {
        let mut bin = vec![0u8; 80];
        bin.extend(2u32.to_le_bytes());
        for t in 0..2 {
            bin.extend([0u8; 12]);
            for v in [
                [0.0f32, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, t as f32 - 1.0],
            ] {
                for c in v {
                    bin.extend(c.to_le_bytes());
                }
            }
            bin.extend([0u8; 2]);
        }
        let m = parse_mesh(&bin).unwrap();
        assert_eq!(
            (m.format.as_str(), m.triangle_count, m.vertex_count),
            ("stl-binary", 2, 6)
        );
        assert_eq!(
            (m.bounds_min, m.bounds_max),
            ([0.0, 0.0, -1.0], [1.0, 1.0, 0.0])
        );

        let ascii = b"solid t\n facet normal 0 0 1\n  outer loop\n   vertex 0 0 0\n   vertex 2 0 0\n   vertex 0 3 0\n  endloop\n endfacet\nendsolid t\n";
        let m = parse_mesh(ascii).unwrap();
        assert_eq!((m.format.as_str(), m.triangle_count), ("stl-ascii", 1));
        assert_eq!(m.bounds_max, [2.0, 3.0, 0.0]);

        let obj =
            b"# quad\nv 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nvt 0 0\nf 1/1 2/1 3/1 4/1\nf -4 -3 -1\n";
        let m = parse_mesh(obj).unwrap();
        assert_eq!(m.indices, vec![0, 1, 2, 0, 2, 3, 0, 1, 3]);
        let err = parse_obj(b"v 0 0 0\nf 1 2 3\n").unwrap_err().to_string();
        assert!(err.contains("line 2"), "{err}");
        assert!(parse_obj(b"hello").is_err());
    }
}
