//! Compression and decompression (XFRM-06). Every codec is pure Rust.
//!
//! Decompressed output is capped at [`MAX_OUTPUT_LEN`]; previews pass a
//! smaller cap and get a prefix of the output instead of an error.

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

use crate::error::{Result, TransformError};
use crate::types::MAX_OUTPUT_LEN;

/// Memory limit for LZMA dictionaries (guards against hostile headers).
const LZMA_MEM_LIMIT_KB: u32 = (MAX_OUTPUT_LEN / 1024 + 64 * 1024) as u32;

const fn d_level() -> u32 {
    6
}
const fn bz_level() -> u32 {
    9
}
const fn zstd_level() -> u32 {
    1
}
const fn brotli_quality() -> u32 {
    9
}
const fn brotli_window() -> u32 {
    22
}

/// A compression format with its options.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Compression {
    /// Raw DEFLATE (RFC 1951), level 0..=9.
    Deflate {
        #[serde(default = "d_level")]
        level: u32,
    },
    /// zlib (RFC 1950), level 0..=9.
    Zlib {
        #[serde(default = "d_level")]
        level: u32,
    },
    /// gzip (RFC 1952), level 0..=9; decoding reads all members.
    Gzip {
        #[serde(default = "d_level")]
        level: u32,
    },
    /// bzip2, level (block size) 1..=9; decoding reads all streams.
    Bzip2 {
        #[serde(default = "bz_level")]
        level: u32,
    },
    /// Legacy `.lzma` ("LZMA alone"), preset 0..=9.
    Lzma {
        #[serde(default = "d_level")]
        preset: u32,
    },
    /// `.xz` container, preset 0..=9; decoding reads concatenated streams.
    Xz {
        #[serde(default = "d_level")]
        preset: u32,
    },
    /// LZ4 frame format.
    Lz4,
    /// Raw LZ4 block. `size` is the decompressed size (required to decode).
    Lz4Block {
        #[serde(default)]
        size: usize,
    },
    /// Zstandard frame. Level 0 stores the data uncompressed; any other
    /// level uses the fastest strategy (the pure-Rust encoder implements
    /// only that one). Decoding reads all frames.
    Zstd {
        #[serde(default = "zstd_level")]
        level: u32,
    },
    /// Brotli, quality 0..=11, window 10..=24 bits.
    Brotli {
        #[serde(default = "brotli_quality")]
        quality: u32,
        #[serde(default = "brotli_window")]
        window: u32,
    },
    /// LZO1X (as produced by `lzo1x_1` / `lzo1x_999`), no container.
    Lzo,
    /// Raw Snappy block.
    Snappy,
    /// Snappy framing format (`sNaPpY` stream identifier).
    SnappyFramed,
}

impl Compression {
    /// Short display name.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Deflate { .. } => "deflate",
            Self::Zlib { .. } => "zlib",
            Self::Gzip { .. } => "gzip",
            Self::Bzip2 { .. } => "bzip2",
            Self::Lzma { .. } => "LZMA",
            Self::Xz { .. } => "xz",
            Self::Lz4 => "LZ4",
            Self::Lz4Block { .. } => "LZ4 block",
            Self::Zstd { .. } => "zstd",
            Self::Brotli { .. } => "Brotli",
            Self::Lzo => "LZO",
            Self::Snappy => "Snappy",
            Self::SnappyFramed => "Snappy framed",
        }
    }

    /// Checks parameter ranges.
    pub fn validate(&self) -> Result<()> {
        let range = |what: &str, v: u32, lo: u32, hi: u32| {
            if (lo..=hi).contains(&v) {
                Ok(())
            } else {
                Err(TransformError::param(format!(
                    "{} {what} must be {lo}..={hi}, got {v}",
                    self.name()
                )))
            }
        };
        match self {
            Self::Deflate { level } | Self::Zlib { level } | Self::Gzip { level } => {
                range("level", *level, 0, 9)
            }
            Self::Bzip2 { level } => range("level", *level, 1, 9),
            Self::Lzma { preset } | Self::Xz { preset } => range("preset", *preset, 0, 9),
            Self::Zstd { level } => range("level", *level, 0, 22),
            Self::Brotli { quality, window } => {
                range("quality", *quality, 0, 11)?;
                range("window", *window, 10, 24)
            }
            Self::Lz4Block { size } if *size > MAX_OUTPUT_LEN => {
                Err(TransformError::OutputTooLarge {
                    limit: MAX_OUTPUT_LEN,
                })
            }
            _ => Ok(()),
        }
    }
}

fn fail(format: &'static str, e: impl std::fmt::Display) -> TransformError {
    TransformError::CompressionFailed {
        format,
        reason: e.to_string(),
    }
}

/// Compresses `data`.
pub fn compress(c: &Compression, data: &[u8]) -> Result<Vec<u8>> {
    c.validate()?;
    let name = c.name();
    let flate = |level: u32| flate2::Compression::new(level);
    let mut out = Vec::new();
    match c {
        Compression::Deflate { level } => {
            flate2::read::DeflateEncoder::new(data, flate(*level))
                .read_to_end(&mut out)
                .map_err(|e| fail(name, e))?;
        }
        Compression::Zlib { level } => {
            flate2::read::ZlibEncoder::new(data, flate(*level))
                .read_to_end(&mut out)
                .map_err(|e| fail(name, e))?;
        }
        Compression::Gzip { level } => {
            flate2::read::GzEncoder::new(data, flate(*level))
                .read_to_end(&mut out)
                .map_err(|e| fail(name, e))?;
        }
        Compression::Bzip2 { level } => {
            bzip2::read::BzEncoder::new(data, bzip2::Compression::new(*level))
                .read_to_end(&mut out)
                .map_err(|e| fail(name, e))?;
        }
        Compression::Lzma { preset } => {
            let opts = lzma_rust2::LzmaOptions::with_preset(*preset);
            let mut w =
                lzma_rust2::LzmaWriter::new_use_header(&mut out, &opts, Some(data.len() as u64))
                    .map_err(|e| fail(name, e))?;
            w.write_all(data).map_err(|e| fail(name, e))?;
            w.finish().map_err(|e| fail(name, e))?;
        }
        Compression::Xz { preset } => {
            let opts = lzma_rust2::XzOptions::with_preset(*preset);
            let mut w = lzma_rust2::XzWriter::new(&mut out, opts).map_err(|e| fail(name, e))?;
            w.write_all(data).map_err(|e| fail(name, e))?;
            w.finish().map_err(|e| fail(name, e))?;
        }
        Compression::Lz4 => {
            let mut w = lz4_flex::frame::FrameEncoder::new(&mut out);
            w.write_all(data).map_err(|e| fail(name, e))?;
            w.finish().map_err(|e| fail(name, e))?;
        }
        Compression::Lz4Block { .. } => out = lz4_flex::block::compress(data),
        Compression::Zstd { level } => {
            let lvl = if *level == 0 {
                ruzstd::encoding::CompressionLevel::Uncompressed
            } else {
                ruzstd::encoding::CompressionLevel::Fastest
            };
            out = ruzstd::encoding::compress_to_vec(data, lvl);
        }
        Compression::Brotli { quality, window } => {
            brotli::CompressorReader::new(data, 1 << 16, *quality, *window)
                .read_to_end(&mut out)
                .map_err(|e| fail(name, e))?;
        }
        Compression::Lzo => {
            out = lzokay_native::compress(data).map_err(|e| fail(name, format!("{e:?}")))?;
        }
        Compression::Snappy => {
            out = snap::raw::Encoder::new()
                .compress_vec(data)
                .map_err(|e| fail(name, e))?;
        }
        Compression::SnappyFramed => {
            snap::read::FrameEncoder::new(data)
                .read_to_end(&mut out)
                .map_err(|e| fail(name, e))?;
        }
    }
    Ok(out)
}

/// Decompresses `data` (output capped at [`MAX_OUTPUT_LEN`]).
pub fn decompress(c: &Compression, data: &[u8]) -> Result<Vec<u8>> {
    decompress_capped(c, data, MAX_OUTPUT_LEN, false).map(|(v, _)| v)
}

/// Reads up to `cap` bytes; with `truncate` a longer stream yields its
/// first `cap` bytes and `true`, otherwise it is an error.
fn read_capped(
    format: &'static str,
    r: impl Read,
    cap: usize,
    truncate: bool,
) -> Result<(Vec<u8>, bool)> {
    let mut out = Vec::new();
    r.take(cap as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|e| TransformError::corrupt(format, e))?;
    finish_cap(out, cap, truncate)
}

fn finish_cap(mut out: Vec<u8>, cap: usize, truncate: bool) -> Result<(Vec<u8>, bool)> {
    if out.len() > cap {
        if truncate {
            out.truncate(cap);
            return Ok((out, true));
        }
        return Err(TransformError::OutputTooLarge { limit: cap });
    }
    Ok((out, false))
}

/// Decompresses at most `cap` bytes. Returns the output and whether it was
/// cut at `cap` (only possible with `truncate`).
pub(crate) fn decompress_capped(
    c: &Compression,
    data: &[u8],
    cap: usize,
    truncate: bool,
) -> Result<(Vec<u8>, bool)> {
    c.validate()?;
    let name = c.name();
    let cap = cap.min(MAX_OUTPUT_LEN);
    match c {
        Compression::Deflate { .. } => {
            read_capped(name, flate2::read::DeflateDecoder::new(data), cap, truncate)
        }
        Compression::Zlib { .. } => {
            read_capped(name, flate2::read::ZlibDecoder::new(data), cap, truncate)
        }
        Compression::Gzip { .. } => {
            read_capped(name, flate2::read::MultiGzDecoder::new(data), cap, truncate)
        }
        Compression::Bzip2 { .. } => {
            read_capped(name, bzip2::read::MultiBzDecoder::new(data), cap, truncate)
        }
        Compression::Lzma { .. } => {
            let r = lzma_rust2::LzmaReader::new_mem_limit(data, LZMA_MEM_LIMIT_KB, None)
                .map_err(|e| TransformError::corrupt(name, e))?;
            read_capped(name, r, cap, truncate)
        }
        Compression::Xz { .. } => {
            read_capped(name, lzma_rust2::XzReader::new(data, true), cap, truncate)
        }
        Compression::Lz4 => read_capped(
            name,
            lz4_flex::frame::FrameDecoder::new(data),
            cap,
            truncate,
        ),
        Compression::Lz4Block { size } => {
            if *size == 0 && !data.is_empty() {
                return Err(TransformError::param(
                    "LZ4 block decompression needs the decompressed size",
                ));
            }
            let mut out = vec![0u8; *size];
            let n = lz4_flex::block::decompress_into(data, &mut out)
                .map_err(|e| TransformError::corrupt(name, e))?;
            out.truncate(n);
            finish_cap(out, cap, truncate)
        }
        Compression::Zstd { .. } => {
            let mut out = Vec::new();
            let mut rest = data;
            // concatenated frames (and skippable frames) are all decoded
            while !rest.is_empty() && out.len() <= cap {
                let mut dec = ruzstd::decoding::StreamingDecoder::new(&mut rest)
                    .map_err(|e| TransformError::corrupt(name, e))?;
                let remaining = (cap + 1 - out.len()) as u64;
                (&mut dec)
                    .take(remaining)
                    .read_to_end(&mut out)
                    .map_err(|e| TransformError::corrupt(name, e))?;
            }
            finish_cap(out, cap, truncate)
        }
        Compression::Brotli { .. } => read_capped(
            name,
            brotli::Decompressor::new(data, 1 << 16),
            cap,
            truncate,
        ),
        Compression::Lzo => {
            // the compressor emits nothing for empty input
            if data.is_empty() {
                return Ok((Vec::new(), false));
            }
            let out = lzokay_native::decompress_all_bounded(data, MAX_OUTPUT_LEN).map_err(|e| {
                if matches!(e, lzokay_native::Error::OutputOverrun) {
                    TransformError::OutputTooLarge {
                        limit: MAX_OUTPUT_LEN,
                    }
                } else {
                    TransformError::corrupt(name, format!("{e:?}"))
                }
            })?;
            finish_cap(out, cap, truncate)
        }
        Compression::Snappy => {
            let len =
                snap::raw::decompress_len(data).map_err(|e| TransformError::corrupt(name, e))?;
            if len > cap && !truncate {
                return Err(TransformError::OutputTooLarge { limit: cap });
            }
            if len > MAX_OUTPUT_LEN {
                return Err(TransformError::OutputTooLarge {
                    limit: MAX_OUTPUT_LEN,
                });
            }
            let out = snap::raw::Decoder::new()
                .decompress_vec(data)
                .map_err(|e| TransformError::corrupt(name, e))?;
            finish_cap(out, cap, truncate)
        }
        Compression::SnappyFramed => {
            read_capped(name, snap::read::FrameDecoder::new(data), cap, truncate)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::HexBytes;

    fn all() -> Vec<Compression> {
        vec![
            Compression::Deflate { level: 6 },
            Compression::Deflate { level: 0 },
            Compression::Zlib { level: 9 },
            Compression::Gzip { level: 1 },
            Compression::Bzip2 { level: 9 },
            Compression::Lzma { preset: 6 },
            Compression::Xz { preset: 1 },
            Compression::Lz4,
            Compression::Zstd { level: 1 },
            Compression::Zstd { level: 0 },
            Compression::Brotli {
                quality: 5,
                window: 20,
            },
            Compression::Lzo,
            Compression::Snappy,
            Compression::SnappyFramed,
        ]
    }

    fn sample() -> Vec<u8> {
        let mut v = Vec::new();
        for i in 0..5000u32 {
            v.extend_from_slice(format!("record {i:05} value={} ", i * 7 % 13).as_bytes());
            v.push((i % 251) as u8);
        }
        v
    }

    fn hex(s: &str) -> Vec<u8> {
        HexBytes::parse(s).unwrap().0
    }

    #[test]
    fn roundtrip_all_codecs() {
        let data = sample();
        for c in all() {
            for input in [&data[..], b"", b"a"] {
                let z = compress(&c, input).unwrap_or_else(|e| panic!("{c:?}: {e}"));
                let back = decompress(&c, &z).unwrap_or_else(|e| panic!("{c:?}: {e}"));
                assert_eq!(back, input, "{c:?}");
            }
        }
        let z = compress(&Compression::Lz4Block { size: 0 }, &data).unwrap();
        assert!(z.len() < data.len());
        let back = decompress(&Compression::Lz4Block { size: data.len() }, &z).unwrap();
        assert_eq!(back, data);
    }

    #[test]
    fn known_streams_from_reference_tools() {
        let plain = b"hello hello hello\n";
        let cases = [
            (
                Compression::Zlib { level: 6 },
                "789ccb48cdc9c957c840905c0040b50687",
            ),
            (Compression::Deflate { level: 6 }, "cb48cdc9c957c840905c00"),
            (
                Compression::Zstd { level: 1 },
                "28b52ffd24126d00003868656c6c6f200a0100314a11a3aa74ce",
            ),
            (
                Compression::Lz4,
                "04224d186440a70f0000006368656c6c6f20060050656c6c6f0a00000000c659d85b",
            ),
            (
                Compression::Brotli {
                    quality: 9,
                    window: 22,
                },
                "2144000468656c6c6f2068656c6c6f2068656c6c6f0a03",
            ),
        ];
        for (c, z) in cases {
            assert_eq!(decompress(&c, &hex(z)).unwrap(), plain, "{c:?}");
        }
        let plain = b"hello hello hello";
        let lzma = "5d00008000ffffffffffffffff00341949ee8de94f7f35c5a3ffff78a40000";
        assert_eq!(
            decompress(&Compression::Lzma { preset: 6 }, &hex(lzma)).unwrap(),
            plain
        );
        let xz = "fd377a585a000004e6d6b4460200210116000000742fe5a3e00010000b5d00341949ee8de94f7e15e00000004b88e6da4480077600012711bc27f3901fb6f37d010000000004595a";
        assert_eq!(
            decompress(&Compression::Xz { preset: 6 }, &hex(xz)).unwrap(),
            plain
        );
        let bz =
            "425a68393141592653591931653d00000081000244a000219a68334d07338bb9229c28480c98b29e80";
        assert_eq!(
            decompress(&Compression::Bzip2 { level: 9 }, &hex(bz)).unwrap(),
            b"hello"
        );
        // LZO1X literal run + end marker
        assert_eq!(
            decompress(&Compression::Lzo, &hex("1461626311 0000")).unwrap(),
            b"abc"
        );
        // Snappy: varint length 3, literal tag
        assert_eq!(
            decompress(&Compression::Snappy, &hex("0308616263")).unwrap(),
            b"abc"
        );
        assert_eq!(
            compress(&Compression::Snappy, b"abc").unwrap(),
            hex("0308616263")
        );
        let framed = compress(&Compression::SnappyFramed, b"abc").unwrap();
        assert!(framed.starts_with(&hex("ff060000734e61507059")));
        // gzip from `gzip -n`
        let gz = compress(&Compression::Gzip { level: 6 }, b"x").unwrap();
        assert_eq!(&gz[..3], [0x1f, 0x8b, 0x08]);
    }

    #[test]
    fn gzip_multi_member() {
        let two = hex(
            "1f8b08000000000000034b4c4a0600c2412435030000001f8b08000000000000034b494d030061e1c40c03000000",
        );
        assert_eq!(
            decompress(&Compression::Gzip { level: 6 }, &two).unwrap(),
            b"abcdef"
        );
        let mut z = compress(&Compression::Zstd { level: 1 }, b"abc").unwrap();
        z.extend(compress(&Compression::Zstd { level: 1 }, b"def").unwrap());
        assert_eq!(
            decompress(&Compression::Zstd { level: 1 }, &z).unwrap(),
            b"abcdef"
        );
        let mut x = compress(&Compression::Xz { preset: 0 }, b"abc").unwrap();
        x.extend(compress(&Compression::Xz { preset: 0 }, b"def").unwrap());
        assert_eq!(
            decompress(&Compression::Xz { preset: 0 }, &x).unwrap(),
            b"abcdef"
        );
    }

    #[test]
    fn corrupt_input_is_an_error() {
        let junk = b"this is definitely not compressed data!!";
        for c in all() {
            if matches!(c, Compression::Lzo | Compression::Snappy) {
                // tiny raw formats may accept arbitrary bytes; must not panic
                let _ = decompress(&c, junk);
                continue;
            }
            assert!(decompress(&c, junk).is_err(), "{c:?} accepted junk");
        }
        let mut z = compress(&Compression::Gzip { level: 6 }, &sample()).unwrap();
        z.truncate(z.len() / 2);
        assert!(decompress(&Compression::Gzip { level: 6 }, &z).is_err());
        assert!(decompress(&Compression::Lz4Block { size: 0 }, b"x").is_err());
    }

    #[test]
    fn caps_and_validation() {
        let data = vec![0u8; 100_000];
        for c in all() {
            let z = compress(&c, &data).unwrap();
            let (p, cut) = decompress_capped(&c, &z, 1000, true).unwrap();
            assert_eq!((p.len(), cut), (1000, true), "{c:?}");
            assert!(matches!(
                decompress_capped(&c, &z, 1000, false),
                Err(TransformError::OutputTooLarge { .. })
            ));
        }
        assert!(compress(&Compression::Deflate { level: 10 }, b"").is_err());
        assert!(compress(&Compression::Bzip2 { level: 0 }, b"").is_err());
        assert!(
            compress(
                &Compression::Brotli {
                    quality: 5,
                    window: 30
                },
                b""
            )
            .is_err()
        );
    }

    #[test]
    fn serde_defaults() {
        let c: Compression = serde_json::from_str(r#"{"kind":"gzip"}"#).unwrap();
        assert_eq!(c, Compression::Gzip { level: 6 });
        let c: Compression = serde_json::from_str(r#"{"kind":"lz4Block","size":10}"#).unwrap();
        assert_eq!(c, Compression::Lz4Block { size: 10 });
        assert_eq!(
            serde_json::to_string(&Compression::SnappyFramed).unwrap(),
            r#"{"kind":"snappyFramed"}"#
        );
    }
}
