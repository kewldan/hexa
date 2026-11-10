//! DIFF-12 / IO-05: patch formats — IPS, UPS, BPS, VCDIFF (xdelta3
//! compatible) and bsdiff — creation, application and inspection.
//!
//! Application returns a [`Tree`] that references the source and the patch
//! instead of copying them, so large targets cost little memory.

mod bps;
mod bsdiff;
mod io;
mod ips;
mod ops;
mod ups;
mod vcdiff;

use std::sync::Arc;

use hexa_core::task::Progress;
use hexa_core::{Cancel, Result, Tree};
use serde::{Deserialize, Serialize};

/// Supported patch formats.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PatchFormat {
    /// International Patching System, with RLE records and the truncation
    /// extension.
    Ips,
    /// Universal Patching System (XOR records, CRC-32 checksums).
    Ups,
    /// Beat patch (`SourceRead` / `TargetRead` / `SourceCopy` /
    /// `TargetCopy`, CRC-32 checksums).
    Bps,
    /// RFC 3284 VCDIFF as written by xdelta3 (no secondary compression).
    Vcdiff,
    /// `BSDIFF40` with bzip2 blocks.
    Bsdiff,
}

impl PatchFormat {
    pub const ALL: [Self; 5] = [Self::Ips, Self::Ups, Self::Bps, Self::Vcdiff, Self::Bsdiff];

    /// Usual file extension.
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Ips => "ips",
            Self::Ups => "ups",
            Self::Bps => "bps",
            Self::Vcdiff => "xdelta",
            Self::Bsdiff => "bsdiff",
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Ips => "IPS",
            Self::Ups => "UPS",
            Self::Bps => "BPS",
            Self::Vcdiff => "VCDIFF (xdelta3)",
            Self::Bsdiff => "bsdiff",
        }
    }
}

/// Options for [`create_patch`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CreateOptions {
    /// VCDIFF: add the xdelta3 Adler-32 checksum to every window.
    pub vcdiff_checksum: bool,
    /// BPS: metadata stored in the patch (usually XML).
    pub bps_metadata: String,
}

/// Options for [`apply_patch`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ApplyOptions {
    /// Verify source, target and patch checksums and sizes where the format
    /// stores them (on by default).
    pub verify: bool,
}

impl Default for ApplyOptions {
    fn default() -> Self {
        Self { verify: true }
    }
}

/// Outcome of [`apply_patch`].
#[derive(Clone, Debug)]
pub struct Applied {
    pub format: PatchFormat,
    /// The patched data, referencing the source and the patch.
    pub target: Tree,
    /// UPS only: the patch was applied in reverse (the input matched the
    /// patch's target, so the result is its source).
    pub reversed: bool,
}

/// Header information of a patch, for display before applying.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchInfo {
    pub format: Option<PatchFormat>,
    pub source_size: Option<u64>,
    pub target_size: Option<u64>,
    pub source_crc32: Option<u32>,
    pub target_crc32: Option<u32>,
    /// Whether the patch's own checksum matches (UPS, BPS).
    pub patch_checksum_ok: Option<bool>,
    pub metadata: Option<String>,
}

/// Recognises a patch by its magic bytes.
pub fn detect_format(patch: &[u8]) -> Option<PatchFormat> {
    if patch.starts_with(b"PATCH") {
        Some(PatchFormat::Ips)
    } else if patch.starts_with(b"UPS1") {
        Some(PatchFormat::Ups)
    } else if patch.starts_with(b"BPS1") {
        Some(PatchFormat::Bps)
    } else if patch.starts_with(&vcdiff::MAGIC) {
        Some(PatchFormat::Vcdiff)
    } else if patch.starts_with(bsdiff::MAGIC) {
        Some(PatchFormat::Bsdiff)
    } else {
        None
    }
}

/// Reads the header (and footer) of a patch without applying it.
pub fn inspect_patch(patch: &[u8]) -> Result<PatchInfo> {
    let format = detect_format(patch).ok_or_else(|| io::invalid("unknown patch format"))?;
    let mut info = match format {
        PatchFormat::Ips => ips::inspect(patch)?,
        PatchFormat::Ups => ups::inspect(patch)?,
        PatchFormat::Bps => bps::inspect(patch)?,
        PatchFormat::Vcdiff => vcdiff::inspect(patch)?,
        PatchFormat::Bsdiff => bsdiff::inspect(patch)?,
    };
    info.format = Some(format);
    Ok(info)
}

/// Creates a patch turning `source` into `target`.
pub fn create_patch(
    format: PatchFormat,
    source: &Tree,
    target: &Tree,
    opts: &CreateOptions,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<Vec<u8>> {
    match format {
        PatchFormat::Ips => ips::create(source, target, cancel, progress),
        PatchFormat::Ups => ups::create(source, target, cancel, progress),
        PatchFormat::Bps => bps::create(source, target, &opts.bps_metadata, cancel, progress),
        PatchFormat::Vcdiff => {
            vcdiff::create(source, target, opts.vcdiff_checksum, cancel, progress)
        }
        PatchFormat::Bsdiff => bsdiff::create(source, target, cancel, progress),
    }
}

/// Applies a patch to `source`; the format is detected from the patch
/// unless `format` is given.
pub fn apply_patch(
    format: Option<PatchFormat>,
    patch: &[u8],
    source: &Tree,
    opts: &ApplyOptions,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<Applied> {
    let format = match format {
        Some(f) => f,
        None => detect_format(patch).ok_or_else(|| io::invalid("unknown patch format"))?,
    };
    let shared: Arc<[u8]> = patch.into();
    let mut reversed = false;
    let target = match format {
        PatchFormat::Ips => ips::apply(&shared, source, cancel, progress)?,
        PatchFormat::Ups => {
            let (t, rev) = ups::apply(&shared, source, opts.verify, cancel, progress)?;
            reversed = rev;
            t
        }
        PatchFormat::Bps => bps::apply(&shared, source, opts.verify, cancel, progress)?,
        PatchFormat::Vcdiff => vcdiff::apply(&shared, source, opts.verify, cancel, progress)?,
        PatchFormat::Bsdiff => bsdiff::apply(&shared, source, cancel, progress)?,
    };
    Ok(Applied {
        format,
        target,
        reversed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use proptest::prelude::*;

    fn roundtrip(format: PatchFormat, src: &[u8], tgt: &[u8]) -> Vec<u8> {
        let (s, t) = (tree(src), tree(tgt));
        let c = Cancel::new();
        let opts = CreateOptions {
            vcdiff_checksum: true,
            bps_metadata: "<meta/>".into(),
        };
        let p = create_patch(format, &s, &t, &opts, &c, &noprog).unwrap();
        assert_eq!(detect_format(&p), Some(format));
        inspect_patch(&p).unwrap();
        let out = apply_patch(None, &p, &s, &ApplyOptions::default(), &c, &noprog).unwrap();
        assert_eq!(out.format, format);
        assert!(bytes(&out.target) == tgt, "{format:?} roundtrip failed");
        p
    }

    fn mutate(base: &[u8], edits: &[(u16, u8, Vec<u8>)]) -> Vec<u8> {
        let mut out = base.to_vec();
        for (pos, del, ins) in edits {
            let at = *pos as usize % (out.len() + 1);
            let end = (at + *del as usize % 64).min(out.len());
            out.splice(at..end, ins.iter().copied());
        }
        out
    }

    #[test]
    fn all_formats_roundtrip_fixed_cases() {
        let a = noise(9, 70_000);
        let mut b = mutate(
            &a,
            &[(100, 10, b"hello".to_vec()), (40_000, 0, vec![0; 300])],
        );
        b.extend_from_slice(&a[1000..5000]);
        let cases: Vec<(Vec<u8>, Vec<u8>)> = vec![
            (vec![], vec![]),
            (vec![], b"new".to_vec()),
            (b"old".to_vec(), vec![]),
            (b"same".to_vec(), b"same".to_vec()),
            (a.clone(), b.clone()),
            (b, a),
        ];
        for f in PatchFormat::ALL {
            for (s, t) in &cases {
                roundtrip(f, s, t);
            }
        }
    }

    #[test]
    fn patches_are_compact() {
        let a = noise(10, 200_000);
        let mut b = a.clone();
        b.splice(150_000..150_000, b"ins".iter().copied());
        b.drain(60_000..60_020);
        b.splice(5000..5003, b"xyz".iter().copied());
        for f in [PatchFormat::Bps, PatchFormat::Vcdiff, PatchFormat::Bsdiff] {
            let p = roundtrip(f, &a, &b);
            assert!(p.len() < 2000, "{f:?}: {} bytes", p.len());
        }
    }

    #[test]
    fn unknown_and_garbage_patches_fail() {
        let s = tree(b"source");
        let c = Cancel::new();
        let o = ApplyOptions::default();
        assert!(apply_patch(None, b"nope", &s, &o, &c, &noprog).is_err());
        assert!(inspect_patch(b"nope").is_err());
        for magic in [
            &b"PATCH"[..],
            b"UPS1",
            b"BPS1",
            &vcdiff::MAGIC,
            bsdiff::MAGIC,
        ] {
            let mut p = magic.to_vec();
            p.extend_from_slice(&[0xFF; 40]);
            assert!(
                apply_patch(None, &p, &s, &o, &c, &noprog).is_err(),
                "{magic:?}"
            );
            p.truncate(magic.len() + 1);
            assert!(
                apply_patch(None, &p, &s, &o, &c, &noprog).is_err(),
                "{magic:?}"
            );
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]
        #[test]
        fn roundtrip_random(seed in any::<u64>(), len in 0usize..3000,
                            edits in proptest::collection::vec(
                                (any::<u16>(), any::<u8>(), proptest::collection::vec(any::<u8>(), 0..40)), 0..6),
                            unrelated in any::<bool>()) {
            let a = noise(seed, len);
            let b = if unrelated { noise(seed ^ 1, len / 2) } else { mutate(&a, &edits) };
            for f in PatchFormat::ALL {
                roundtrip(f, &a, &b);
            }
        }

        #[test]
        fn roundtrip_low_entropy(a in proptest::collection::vec(0u8..3, 0..600),
                                 b in proptest::collection::vec(0u8..3, 0..600)) {
            for f in PatchFormat::ALL {
                roundtrip(f, &a, &b);
            }
        }
    }
}
