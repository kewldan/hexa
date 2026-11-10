//! Signature carving over a provider (DISK-07): finds files by header and
//! end marker (or a size stored in the header) in unallocated or raw data.

use hexa_core::provider::Provider;
use hexa_core::task::Progress;
use hexa_core::{Cancel, Result};
use memchr::memmem;
use serde::Serialize;

use crate::util::{be16, be32, check_cancel, le16, read_upto};

/// How the end of a carved file is found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CarveEnd {
    /// First `bytes` after the header; `extra` trailing bytes are included.
    Footer { bytes: &'static [u8], extra: u64 },
    /// ZIP end-of-central-directory record plus its comment.
    ZipEocd,
    /// SQLite page size × page count from the database header.
    SqliteHeader,
}

/// One file signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Signature {
    pub name: &'static str,
    pub extension: &'static str,
    pub header: &'static [u8],
    pub end: CarveEnd,
    /// Upper bound on a carved file; also the size when no end is found.
    pub max_size: u64,
}

/// The built-in signature table.
pub const SIGNATURES: &[Signature] = &[
    Signature {
        name: "JPEG image",
        extension: "jpg",
        header: b"\xFF\xD8\xFF",
        end: CarveEnd::Footer {
            bytes: b"\xFF\xD9",
            extra: 0,
        },
        max_size: 64 << 20,
    },
    Signature {
        name: "PNG image",
        extension: "png",
        header: b"\x89PNG\r\n\x1A\n",
        end: CarveEnd::Footer {
            bytes: b"IEND\xAE\x42\x60\x82",
            extra: 0,
        },
        max_size: 64 << 20,
    },
    Signature {
        name: "GIF image",
        extension: "gif",
        header: b"GIF89a",
        end: CarveEnd::Footer {
            bytes: b"\x00\x3B",
            extra: 0,
        },
        max_size: 16 << 20,
    },
    Signature {
        name: "GIF image",
        extension: "gif",
        header: b"GIF87a",
        end: CarveEnd::Footer {
            bytes: b"\x00\x3B",
            extra: 0,
        },
        max_size: 16 << 20,
    },
    Signature {
        name: "PDF document",
        extension: "pdf",
        header: b"%PDF-",
        end: CarveEnd::Footer {
            bytes: b"%%EOF",
            extra: 0,
        },
        max_size: 256 << 20,
    },
    Signature {
        name: "ZIP archive",
        extension: "zip",
        header: b"PK\x03\x04",
        end: CarveEnd::ZipEocd,
        max_size: 1 << 30,
    },
    Signature {
        name: "SQLite database",
        extension: "sqlite",
        header: b"SQLite format 3\0",
        end: CarveEnd::SqliteHeader,
        max_size: 4 << 30,
    },
];

/// A carved file candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CarvedFile {
    pub kind: String,
    pub extension: String,
    pub offset: u64,
    pub size: u64,
    /// The end marker / header size was found (otherwise `size` is a guess).
    pub complete: bool,
}

/// Carving parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CarveOptions {
    pub start: u64,
    pub end: Option<u64>,
    /// Only consider headers at multiples of this (1 = anywhere, 512 =
    /// sector starts, which is much faster and avoids embedded matches).
    pub alignment: u64,
    pub max_results: usize,
}

impl Default for CarveOptions {
    fn default() -> Self {
        Self {
            start: 0,
            end: None,
            alignment: 1,
            max_results: 100_000,
        }
    }
}

const CHUNK: usize = 1 << 20;

/// Scans `p` for files matching `sigs` (DISK-07).
pub fn carve(
    p: &dyn Provider,
    sigs: &[Signature],
    opts: &CarveOptions,
    progress: Progress<'_>,
    cancel: Option<&Cancel>,
) -> Result<Vec<CarvedFile>> {
    let end = opts.end.unwrap_or(p.len()).min(p.len());
    let align = opts.alignment.max(1);
    let mut first_byte = [false; 256];
    for s in sigs {
        if let Some(&b) = s.header.first() {
            first_byte[usize::from(b)] = true;
        }
    }
    let max_header = sigs.iter().map(|s| s.header.len()).max().unwrap_or(1);
    let mut out = Vec::new();
    let mut pos = opts.start;
    let mut skip_until = opts.start;
    let total = end.saturating_sub(opts.start);
    while pos < end {
        check_cancel(cancel)?;
        let chunk = read_upto(p, pos, CHUNK + max_header)?;
        let scan_len = chunk.len().min(CHUNK).min((end - pos) as usize);
        let mut i = 0usize;
        while i < scan_len {
            let abs = pos + i as u64;
            if !abs.is_multiple_of(align) {
                i += (align - abs % align) as usize;
                continue;
            }
            if abs >= skip_until && first_byte[usize::from(chunk[i])] {
                for s in sigs {
                    if chunk[i..].starts_with(s.header) {
                        let (size, complete) = find_end(p, s, abs, end)?;
                        out.push(CarvedFile {
                            kind: s.name.to_string(),
                            extension: s.extension.to_string(),
                            offset: abs,
                            size,
                            complete,
                        });
                        if complete {
                            skip_until = abs + size;
                        }
                        if out.len() >= opts.max_results {
                            return Ok(out);
                        }
                        break;
                    }
                }
            }
            i += align as usize;
        }
        pos += scan_len as u64;
        progress(pos - opts.start, total);
    }
    Ok(out)
}

fn find_end(p: &dyn Provider, s: &Signature, start: u64, end: u64) -> Result<(u64, bool)> {
    let limit = (start + s.max_size).min(end);
    let fallback = (limit - start, false);
    match s.end {
        CarveEnd::SqliteHeader => {
            let h = read_upto(p, start, 100)?;
            let page = match be16(&h, 16) {
                1 => 65_536,
                n => u64::from(n),
            };
            let size = page * u64::from(be32(&h, 28));
            if size > 0 && start + size <= end {
                Ok((size, true))
            } else {
                Ok(fallback)
            }
        }
        CarveEnd::Footer { bytes, extra } => {
            Ok(
                search(p, start + s.header.len() as u64, limit, bytes)?.map_or(fallback, |at| {
                    ((at + bytes.len() as u64 + extra).min(end) - start, true)
                }),
            )
        }
        CarveEnd::ZipEocd => {
            let Some(at) = search(p, start + 4, limit, b"PK\x05\x06")? else {
                return Ok(fallback);
            };
            let rec = read_upto(p, at, 22)?;
            let size = at + 22 + u64::from(le16(&rec, 20)) - start;
            Ok((size.min(end - start), rec.len() == 22))
        }
    }
}

/// First offset of `needle` in `[from, to)`.
fn search(p: &dyn Provider, from: u64, to: u64, needle: &[u8]) -> Result<Option<u64>> {
    let finder = memmem::Finder::new(needle);
    let mut pos = from;
    while pos < to {
        let len = ((to - pos) as usize).min(CHUNK + needle.len() - 1);
        let buf = read_upto(p, pos, len)?;
        if buf.len() < needle.len() {
            break;
        }
        if let Some(i) = finder.find(&buf) {
            return Ok(Some(pos + i as u64));
        }
        pos += (buf.len() - needle.len() + 1) as u64;
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hexa_core::provider::MemoryProvider;
    use hexa_core::task::no_progress;

    #[test]
    fn carves_known_formats() {
        let mut d = vec![0x11u8; 40_000];
        let put = |d: &mut Vec<u8>, at: usize, b: &[u8]| d[at..at + b.len()].copy_from_slice(b);
        put(&mut d, 1000, b"\xFF\xD8\xFF\xE0JFIF");
        put(&mut d, 1500, b"\xFF\xD9");
        put(&mut d, 5000, b"\x89PNG\r\n\x1A\n");
        put(&mut d, 5100, b"IEND\xAE\x42\x60\x82");
        put(&mut d, 9000, b"PK\x03\x04");
        put(&mut d, 9300, b"PK\x05\x06");
        put(&mut d, 9320, &3u16.to_le_bytes());
        put(&mut d, 12_288, b"SQLite format 3\0");
        put(&mut d, 12_288 + 16, &1024u16.to_be_bytes());
        put(&mut d, 12_288 + 28, &2u32.to_be_bytes());
        put(&mut d, 39_000, b"%PDF-1.7");
        let p = MemoryProvider::new("d", d);
        let found = carve(&p, SIGNATURES, &CarveOptions::default(), &no_progress, None).unwrap();
        let summary: Vec<_> = found
            .iter()
            .map(|f| (f.extension.as_str(), f.offset, f.size, f.complete))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("jpg", 1000, 502, true),
                ("png", 5000, 108, true),
                ("zip", 9000, 325, true),
                ("sqlite", 12_288, 2048, true),
                ("pdf", 39_000, 1000, false),
            ]
        );

        let aligned = carve(
            &p,
            SIGNATURES,
            &CarveOptions {
                alignment: 4096,
                ..CarveOptions::default()
            },
            &no_progress,
            None,
        )
        .unwrap();
        assert_eq!(aligned.len(), 1);
        assert_eq!(aligned[0].extension, "sqlite");
    }
}
