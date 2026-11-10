//! DIFF-07: region list export as text, CSV and JSON.

use std::fmt::Write as _;
use std::io::Write;

use hexa_core::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::{DiffResult, KindStats, RegionKind};

/// Export format of a region list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExportFormat {
    /// Human readable report with statistics and a hex offset table.
    Text,
    /// `index,kind,aStart,aLen,bStart,bLen` rows with a header line.
    Csv,
    /// The serialised [`DiffResult`].
    Json,
}

impl ExportFormat {
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Text => "txt",
            Self::Csv => "csv",
            Self::Json => "json",
        }
    }
}

fn kind_line(out: &mut String, kind: RegionKind, s: &KindStats) {
    let _ = writeln!(
        out,
        "  {:<9} {:>10} regions, {:>14} bytes in A, {:>14} bytes in B",
        kind.name(),
        s.count,
        s.bytes_a,
        s.bytes_b
    );
}

/// DIFF-07: writes `result` in `format`.
pub fn export(result: &DiffResult, format: ExportFormat, out: &mut dyn Write) -> Result<()> {
    match format {
        ExportFormat::Json => {
            serde_json::to_writer_pretty(&mut *out, result)
                .map_err(|e| Error::Invalid(e.to_string()))?;
            out.write_all(b"\n")?;
        }
        ExportFormat::Csv => {
            out.write_all(b"index,kind,aStart,aLen,bStart,bLen\n")?;
            for (i, r) in result.regions.iter().enumerate() {
                writeln!(
                    out,
                    "{},{},{},{},{},{}",
                    i + 1,
                    r.kind.name(),
                    r.a.start,
                    r.a.len,
                    r.b.start,
                    r.b.len
                )?;
            }
        }
        ExportFormat::Text => {
            let s = &result.stats;
            let mut head = String::new();
            let _ = writeln!(head, "A: {} bytes, B: {} bytes", s.len_a, s.len_b);
            let _ = writeln!(
                head,
                "Match: {:.2}% ({} equal bytes), {} differences",
                s.match_percent, s.equal_bytes, s.differences
            );
            for kind in [
                RegionKind::Changed,
                RegionKind::Inserted,
                RegionKind::Deleted,
                RegionKind::Equal,
            ] {
                kind_line(&mut head, kind, s.kind(kind));
            }
            if result.truncated {
                let _ = writeln!(
                    head,
                    "Region list truncated to {} entries",
                    result.regions.len()
                );
            }
            let _ = writeln!(
                head,
                "\n{:>8}  {:<9} {:>18} {:>12} {:>18} {:>12}",
                "#", "kind", "A offset", "A length", "B offset", "B length"
            );
            out.write_all(head.as_bytes())?;
            for (i, r) in result.regions.iter().enumerate() {
                writeln!(
                    out,
                    "{:>8}  {:<9} {:>#18x} {:>12} {:>#18x} {:>12}",
                    i + 1,
                    r.kind.name(),
                    r.a.start,
                    r.a.len,
                    r.b.start,
                    r.b.len
                )?;
            }
        }
    }
    Ok(())
}

/// [`export`] into a string.
pub fn export_string(result: &DiffResult, format: ExportFormat) -> String {
    let mut buf = Vec::new();
    export(result, format, &mut buf).expect("writing to a Vec cannot fail");
    String::from_utf8(buf).expect("exports are UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use crate::{DiffMode, DiffOptions, diff};
    use hexa_core::Cancel;

    fn sample() -> DiffResult {
        let opts = DiffOptions {
            mode: DiffMode::Bytewise,
            ..DiffOptions::default()
        };
        diff(
            &tree(b"abcdef"),
            &tree(b"abXdefgh"),
            &opts,
            &Cancel::new(),
            &noprog,
        )
        .unwrap()
    }

    #[test]
    fn csv() {
        let s = export_string(&sample(), ExportFormat::Csv);
        assert_eq!(
            s,
            "index,kind,aStart,aLen,bStart,bLen\n1,changed,2,1,2,1\n2,inserted,6,0,6,2\n"
        );
    }

    #[test]
    fn text() {
        let s = export_string(&sample(), ExportFormat::Text);
        assert!(s.contains("A: 6 bytes, B: 8 bytes"));
        assert!(s.contains("Match: 62.50%"));
        assert!(s.contains("changed"));
        assert!(s.contains("0x2"));
        assert_eq!(s.lines().count(), 10);
    }

    #[test]
    fn json_roundtrip() {
        let r = sample();
        let s = export_string(&r, ExportFormat::Json);
        assert!(s.contains("\"matchPercent\""));
        let back: DiffResult = serde_json::from_str(&s).unwrap();
        assert_eq!(back, r);
    }
}
