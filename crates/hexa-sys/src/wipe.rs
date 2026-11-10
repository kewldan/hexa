//! Wiping (DISK-10): zeros, random, `DoD` 5220.22-M (3 passes), Gutmann-lite
//! with a configurable number of passes, custom pass lists, and a verify
//! pass over the final pattern.

use hexa_core::provider::Provider;
use hexa_core::task::Progress;
use hexa_core::{Cancel, Result};
use serde::{Deserialize, Serialize};

use crate::util::{Rng, check_cancel, invalid};

/// One overwrite pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum WipePass {
    /// Every byte set to `byte`.
    Fill { byte: u8 },
    /// `bytes` repeated from the start of the range.
    Pattern { bytes: Vec<u8> },
    /// Pseudo-random data from an OS-seeded generator.
    Random,
}

/// A wiping scheme.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "method")]
pub enum WipeMethod {
    Zeros,
    Random,
    /// `DoD` 5220.22-M: 0x00, 0xFF, random.
    Dod3Pass,
    /// Gutmann-lite: random, the first `passes - 2` Gutmann patterns, random
    /// (35 passes = the full Gutmann sequence).
    Gutmann {
        passes: u32,
    },
    Custom {
        passes: Vec<WipePass>,
    },
}

/// The 27 deterministic Gutmann patterns (passes 5–31).
const GUTMANN: [&[u8]; 27] = [
    &[0x55],
    &[0xAA],
    &[0x92, 0x49, 0x24],
    &[0x49, 0x24, 0x92],
    &[0x24, 0x92, 0x49],
    &[0x00],
    &[0x11],
    &[0x22],
    &[0x33],
    &[0x44],
    &[0x55],
    &[0x66],
    &[0x77],
    &[0x88],
    &[0x99],
    &[0xAA],
    &[0xBB],
    &[0xCC],
    &[0xDD],
    &[0xEE],
    &[0xFF],
    &[0x92, 0x49, 0x24],
    &[0x49, 0x24, 0x92],
    &[0x24, 0x92, 0x49],
    &[0x6D, 0xB6, 0xDB],
    &[0xB6, 0xDB, 0x6D],
    &[0xDB, 0x6D, 0xB6],
];

fn pattern(bytes: &[u8]) -> WipePass {
    match bytes {
        [b] => WipePass::Fill { byte: *b },
        _ => WipePass::Pattern {
            bytes: bytes.to_vec(),
        },
    }
}

impl WipeMethod {
    /// The passes of this method, in order.
    pub fn passes(&self) -> Vec<WipePass> {
        match self {
            Self::Zeros => vec![WipePass::Fill { byte: 0 }],
            Self::Random => vec![WipePass::Random],
            Self::Dod3Pass => vec![
                WipePass::Fill { byte: 0x00 },
                WipePass::Fill { byte: 0xFF },
                WipePass::Random,
            ],
            Self::Gutmann { passes } => {
                let n = (*passes).clamp(1, 35) as usize;
                if n >= 35 {
                    let mut v = vec![WipePass::Random; 4];
                    v.extend(GUTMANN.iter().map(|p| pattern(p)));
                    v.extend(vec![WipePass::Random; 4]);
                    return v;
                }
                if n == 1 {
                    return vec![WipePass::Random];
                }
                let mut v = vec![WipePass::Random];
                v.extend(GUTMANN.iter().take(n - 2).map(|p| pattern(p)));
                v.push(WipePass::Random);
                v
            }
            Self::Custom { passes } => passes.clone(),
        }
    }
}

/// Produces the bytes of one pass block by block; recreating it with the
/// same seed yields the same stream (used by verify).
#[derive(Debug, Clone)]
pub(crate) struct PassStream {
    pass: WipePass,
    rng: Rng,
    seed: u64,
}

impl PassStream {
    pub(crate) fn new(pass: WipePass) -> Self {
        let seed = Rng::os_seed();
        Self {
            pass,
            rng: Rng::from_seed(seed),
            seed,
        }
    }

    pub(crate) fn restart(&self) -> Self {
        Self {
            pass: self.pass.clone(),
            rng: Rng::from_seed(self.seed),
            seed: self.seed,
        }
    }

    /// Fills `buf` with the bytes at `rel` (offset from the range start).
    /// Must be called in increasing, contiguous order.
    pub(crate) fn fill(&mut self, rel: u64, buf: &mut [u8]) {
        match &self.pass {
            WipePass::Fill { byte } => buf.fill(*byte),
            WipePass::Pattern { bytes } if !bytes.is_empty() => {
                let n = bytes.len() as u64;
                for (i, b) in buf.iter_mut().enumerate() {
                    *b = bytes[((rel + i as u64) % n) as usize];
                }
            }
            WipePass::Pattern { .. } => buf.fill(0),
            WipePass::Random => self.rng.fill(buf),
        }
    }
}

/// What to wipe and how.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WipeOptions {
    pub method: WipeMethod,
    /// Re-read and compare the final pass.
    pub verify: bool,
    pub start: u64,
    /// Bytes to wipe; `None` = to the end.
    pub len: Option<u64>,
    pub block_size: usize,
}

impl Default for WipeOptions {
    fn default() -> Self {
        Self {
            method: WipeMethod::Zeros,
            verify: true,
            start: 0,
            len: None,
            block_size: 1 << 20,
        }
    }
}

/// Result of a wipe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WipeReport {
    pub passes: u32,
    pub bytes_written: u64,
    pub verified: Option<bool>,
    pub first_mismatch: Option<u64>,
}

/// Overwrites a range of `dst` (DISK-10). Progress counts every pass and
/// the verify pass.
pub fn wipe(
    dst: &dyn Provider,
    opts: &WipeOptions,
    progress: Progress<'_>,
    cancel: Option<&Cancel>,
) -> Result<WipeReport> {
    if !dst.capabilities().writable {
        return Err(hexa_core::Error::ReadOnly);
    }
    let start = opts.start;
    let len = opts.len.unwrap_or(dst.len().saturating_sub(start));
    if start.checked_add(len).is_none_or(|e| e > dst.len()) {
        return Err(invalid("wipe range is past the end"));
    }
    let passes = opts.method.passes();
    if passes.is_empty() {
        return Err(invalid("no wipe passes"));
    }
    let bs = align_block(opts.block_size, dst.capabilities().sector_size);
    let total = len * passes.len() as u64 + if opts.verify { len } else { 0 };
    let mut done = 0u64;
    let mut buf = vec![0u8; bs];
    let mut last = None;
    for pass in &passes {
        let mut stream = PassStream::new(pass.clone());
        let replay = stream.restart();
        let mut rel = 0u64;
        while rel < len {
            check_cancel(cancel)?;
            let n = ((len - rel) as usize).min(bs);
            stream.fill(rel, &mut buf[..n]);
            dst.write_at(start + rel, &buf[..n])?;
            rel += n as u64;
            done += n as u64;
            progress(done, total);
        }
        dst.flush()?;
        last = Some(replay);
    }
    let mut report = WipeReport {
        passes: passes.len() as u32,
        bytes_written: len * passes.len() as u64,
        verified: None,
        first_mismatch: None,
    };
    if opts.verify
        && let Some(mut expect) = last
    {
        dst.invalidate();
        let mut want = vec![0u8; bs];
        let mut rel = 0u64;
        while rel < len {
            check_cancel(cancel)?;
            let n = ((len - rel) as usize).min(bs);
            expect.fill(rel, &mut want[..n]);
            dst.read_at(start + rel, &mut buf[..n])?;
            if let Some(i) = buf[..n].iter().zip(&want[..n]).position(|(a, b)| a != b) {
                report.first_mismatch = Some(start + rel + i as u64);
                break;
            }
            rel += n as u64;
            done += n as u64;
            progress(done, total);
        }
        report.verified = Some(report.first_mismatch.is_none());
    }
    Ok(report)
}

/// Rounds a block size to a multiple of the sector size.
pub(crate) fn align_block(block: usize, sector: Option<u32>) -> usize {
    let ss = sector.unwrap_or(1).max(1) as usize;
    (block.max(ss) / ss) * ss
}

#[cfg(test)]
mod tests {
    use super::*;
    use hexa_core::provider::MemoryProvider;
    use hexa_core::task::no_progress;

    #[test]
    fn method_pass_lists() {
        assert_eq!(WipeMethod::Dod3Pass.passes().len(), 3);
        assert_eq!(WipeMethod::Gutmann { passes: 35 }.passes().len(), 35);
        let lite = WipeMethod::Gutmann { passes: 7 }.passes();
        assert_eq!(lite.len(), 7);
        assert_eq!(lite[0], WipePass::Random);
        assert_eq!(lite[1], WipePass::Fill { byte: 0x55 });
        assert_eq!(
            lite[3],
            WipePass::Pattern {
                bytes: vec![0x92, 0x49, 0x24]
            }
        );
        assert_eq!(lite[6], WipePass::Random);
    }

    #[test]
    fn wipes_range_and_verifies() {
        let p = MemoryProvider::new("d", vec![0xEE; 10_000]);
        let opts = WipeOptions {
            method: WipeMethod::Custom {
                passes: vec![
                    WipePass::Random,
                    WipePass::Pattern {
                        bytes: vec![1, 2, 3],
                    },
                ],
            },
            start: 100,
            len: Some(5000),
            block_size: 777,
            ..WipeOptions::default()
        };
        let r = wipe(&p, &opts, &no_progress, None).unwrap();
        assert_eq!(r.verified, Some(true));
        assert_eq!(r.bytes_written, 10_000);
        let mut buf = vec![0u8; 10_000];
        p.read_at(0, &mut buf).unwrap();
        assert_eq!(buf[99], 0xEE);
        assert_eq!(&buf[100..106], &[1, 2, 3, 1, 2, 3]);
        assert_eq!(buf[5100], 0xEE);

        let dod = WipeOptions {
            method: WipeMethod::Dod3Pass,
            block_size: 4096,
            ..WipeOptions::default()
        };
        let r = wipe(&p, &dod, &no_progress, None).unwrap();
        assert_eq!((r.passes, r.verified), (3, Some(true)));
        p.read_at(0, &mut buf).unwrap();
        assert!(memchr::memchr_iter(0xEE, &buf).count() < 200);
    }
}
