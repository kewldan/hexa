//! `diff`, `merge3` and `patch create|apply|info`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand, ValueEnum};
use hexa_core::Cancel;
use hexa_diff::{
    ApplyOptions, ConflictResolution, CreateOptions, DiffMode, DiffOptions, ExportFormat,
    MergeOptions, PatchFormat,
};
use serde_json::json;

use super::name_of;
use crate::error::{CliError, CliResult, Outcome};
use crate::io::{self, no_progress, open_tree, parse_u64, read_bytes, write_tree_to};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DiffFormat {
    Text,
    Csv,
    Json,
}

#[derive(Debug, Args)]
pub struct DiffArgs {
    /// First file (A).
    pub a: PathBuf,
    /// Second file (B).
    pub b: PathBuf,
    /// Report format (`--json` implies `json`).
    #[arg(short, long, value_enum, default_value_t = DiffFormat::Text)]
    pub format: DiffFormat,
    /// Print only the statistics.
    #[arg(long)]
    pub stats: bool,
    /// Compare offset by offset instead of detecting insertions/deletions.
    #[arg(long)]
    pub bytewise: bool,
    /// Merge differences separated by fewer equal bytes than this.
    #[arg(long, value_parser = parse_u64, default_value = "0")]
    pub merge_gap: u64,
    /// Maximum number of regions to report (statistics cover everything).
    #[arg(long, default_value_t = 1_000_000)]
    pub max_regions: usize,
    /// Only compare; print nothing (the exit code tells the result).
    #[arg(short, long)]
    pub quiet: bool,
}

fn diff_options(bytewise: bool, merge_gap: u64) -> DiffOptions {
    DiffOptions {
        mode: if bytewise {
            DiffMode::Bytewise
        } else {
            DiffMode::ShiftAware
        },
        merge_gap,
        ..DiffOptions::default()
    }
}

pub fn run_diff(a: &DiffArgs, json_out: bool) -> CliResult {
    let ta = open_tree(&a.a)?;
    let tb = open_tree(&a.b)?;
    let opts = DiffOptions {
        max_regions: Some(a.max_regions),
        ..diff_options(a.bytewise, a.merge_gap)
    };
    let res = hexa_diff::diff(&ta, &tb, &opts, &Cancel::new(), &no_progress)?;
    let same = res.stats.differences == 0 && res.stats.len_a == res.stats.len_b;
    if a.quiet {
        return Ok(Outcome::from_ok(same));
    }
    let format = if json_out { DiffFormat::Json } else { a.format };
    let text = match (format, a.stats) {
        (DiffFormat::Json, true) => {
            let mut s = serde_json::to_string_pretty(&json!({
                "identical": same,
                "stats": res.stats,
            }))?;
            s.push('\n');
            s
        }
        (DiffFormat::Json, false) => {
            let mut v = serde_json::to_value(&res)?;
            v["identical"] = json!(same);
            let mut s = serde_json::to_string_pretty(&v)?;
            s.push('\n');
            s
        }
        (DiffFormat::Csv, false) => hexa_diff::export_string(&res, ExportFormat::Csv),
        (_, true) => {
            let mut only = res.clone();
            only.regions.clear();
            let full = hexa_diff::export_string(&only, ExportFormat::Text);
            // The text report starts with the statistics block.
            full.split("\n\n").next().unwrap_or_default().to_owned() + "\n"
        }
        (DiffFormat::Text, false) => hexa_diff::export_string(&res, ExportFormat::Text),
    };
    io::print(&text)?;
    if res.truncated {
        eprintln!(
            "hexa-cli: region list truncated at {} (use --max-regions)",
            a.max_regions
        );
    }
    Ok(Outcome::from_ok(same))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ConflictArg {
    Ours,
    Theirs,
    Base,
}

#[derive(Debug, Args)]
pub struct Merge3Args {
    /// Common ancestor.
    pub base: PathBuf,
    /// Our version.
    pub ours: PathBuf,
    /// Their version.
    pub theirs: PathBuf,
    /// Output file (default: stdout).
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// What conflicting regions hold in the output.
    #[arg(long, value_enum, default_value_t = ConflictArg::Ours)]
    pub conflict: ConflictArg,
    /// Compare offset by offset instead of detecting insertions/deletions.
    #[arg(long)]
    pub bytewise: bool,
}

pub fn run_merge3(a: &Merge3Args, json_out: bool) -> CliResult {
    let base = open_tree(&a.base)?;
    let ours = open_tree(&a.ours)?;
    let theirs = open_tree(&a.theirs)?;
    let opts = MergeOptions {
        diff: diff_options(a.bytewise, 0),
        conflict: match a.conflict {
            ConflictArg::Ours => ConflictResolution::Ours,
            ConflictArg::Theirs => ConflictResolution::Theirs,
            ConflictArg::Base => ConflictResolution::Base,
        },
    };
    let res = hexa_diff::merge3(&base, &ours, &theirs, &opts, &Cancel::new(), &no_progress)?;
    let merged = res.to_tree(&base, &ours, &theirs);
    let to_stdout = a.output.as_deref().is_none_or(io::is_std);
    if to_stdout && json_out {
        return Err(CliError::usage("--json needs --output for the merged data"));
    }
    write_tree_to(&merged, a.output.as_deref())?;
    if json_out {
        io::print_json(&json!({
            "len": res.len,
            "conflicts": res.conflicts,
            "segments": res.segments.len(),
        }))?;
    }
    for c in &res.conflicts {
        eprintln!(
            "hexa-cli: conflict at base 0x{:X}+{} (ours 0x{:X}+{}, theirs 0x{:X}+{}) -> output 0x{:X}",
            c.base.start,
            c.base.len,
            c.ours.start,
            c.ours.len,
            c.theirs.start,
            c.theirs.len,
            c.output.start
        );
    }
    Ok(Outcome::from_ok(res.conflicts.is_empty()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PatchFormatArg {
    Ips,
    Ups,
    Bps,
    #[value(alias = "xdelta")]
    Vcdiff,
    Bsdiff,
}

impl From<PatchFormatArg> for PatchFormat {
    fn from(f: PatchFormatArg) -> Self {
        match f {
            PatchFormatArg::Ips => Self::Ips,
            PatchFormatArg::Ups => Self::Ups,
            PatchFormatArg::Bps => Self::Bps,
            PatchFormatArg::Vcdiff => Self::Vcdiff,
            PatchFormatArg::Bsdiff => Self::Bsdiff,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum PatchCommand {
    /// Create a patch turning SOURCE into TARGET.
    Create {
        source: PathBuf,
        target: PathBuf,
        /// Patch file to write.
        #[arg(short, long)]
        output: PathBuf,
        /// Patch format (default: from the output extension).
        #[arg(short, long, value_enum)]
        format: Option<PatchFormatArg>,
        /// VCDIFF: add Adler-32 checksums.
        #[arg(long)]
        checksum: bool,
        /// BPS: metadata to embed.
        #[arg(long, default_value = "")]
        metadata: String,
    },
    /// Apply PATCH to SOURCE.
    Apply {
        source: PathBuf,
        patch: PathBuf,
        /// Output file (default: stdout).
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Patch format (default: detected).
        #[arg(short, long, value_enum)]
        format: Option<PatchFormatArg>,
        /// Do not verify checksums and sizes.
        #[arg(long)]
        no_verify: bool,
    },
    /// Show the header of a patch.
    Info { patch: PathBuf },
}

fn format_from_ext(path: &Path) -> Option<PatchFormat> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    PatchFormat::ALL
        .into_iter()
        .find(|f| f.extension() == ext)
        .or(match ext.as_str() {
            "vcdiff" | "xd3" => Some(PatchFormat::Vcdiff),
            "bsd" | "bdiff" => Some(PatchFormat::Bsdiff),
            _ => None,
        })
}

pub fn run_patch(c: &PatchCommand, json_out: bool) -> CliResult {
    let cancel = Cancel::new();
    match c {
        PatchCommand::Create {
            source,
            target,
            output,
            format,
            checksum,
            metadata,
        } => {
            let format = format
                .map(PatchFormat::from)
                .or_else(|| format_from_ext(output))
                .ok_or_else(|| {
                    CliError::usage(
                        "cannot tell the patch format from the output name; use --format",
                    )
                })?;
            let src = open_tree(source)?;
            let dst = open_tree(target)?;
            let opts = CreateOptions {
                vcdiff_checksum: *checksum,
                bps_metadata: metadata.clone(),
            };
            let patch = hexa_diff::create_patch(format, &src, &dst, &opts, &cancel, &no_progress)?;
            io::write_bytes_to(&patch, Some(output))?;
            if json_out {
                io::print_json(&json!({
                    "format": name_of(&format),
                    "size": patch.len(),
                    "output": output.display().to_string(),
                }))?;
            }
            Ok(Outcome::Ok)
        }
        PatchCommand::Apply {
            source,
            patch,
            output,
            format,
            no_verify,
        } => {
            let src = open_tree(source)?;
            let data = read_bytes(patch)?;
            let opts = ApplyOptions { verify: !no_verify };
            let applied = hexa_diff::apply_patch(
                format.map(PatchFormat::from),
                &data,
                &src,
                &opts,
                &cancel,
                &no_progress,
            )?;
            let to_stdout = output.as_deref().is_none_or(io::is_std);
            if to_stdout && json_out {
                return Err(CliError::usage(
                    "--json needs --output for the patched data",
                ));
            }
            write_tree_to(&applied.target, output.as_deref())?;
            if json_out {
                io::print_json(&json!({
                    "format": name_of(&applied.format),
                    "size": applied.target.len(),
                    "reversed": applied.reversed,
                }))?;
            }
            Ok(Outcome::Ok)
        }
        PatchCommand::Info { patch } => {
            let data = read_bytes(patch)?;
            let info = hexa_diff::inspect_patch(&data)?;
            if json_out {
                io::print_json(&info)?;
            } else {
                let mut s = String::new();
                let opt = |v: Option<u64>| v.map_or_else(|| "-".to_owned(), |v| v.to_string());
                let crc = |v: Option<u32>| v.map_or_else(|| "-".to_owned(), |v| format!("{v:08x}"));
                let _ = writeln!(
                    s,
                    "Format:        {}",
                    info.format.map_or("unknown", PatchFormat::name)
                );
                let _ = writeln!(s, "Source size:   {}", opt(info.source_size));
                let _ = writeln!(s, "Target size:   {}", opt(info.target_size));
                let _ = writeln!(s, "Source CRC32:  {}", crc(info.source_crc32));
                let _ = writeln!(s, "Target CRC32:  {}", crc(info.target_crc32));
                if let Some(ok) = info.patch_checksum_ok {
                    let _ = writeln!(s, "Patch CRC:     {}", if ok { "ok" } else { "BAD" });
                }
                if let Some(m) = info.metadata.as_deref().filter(|m| !m.is_empty()) {
                    let _ = writeln!(s, "Metadata:      {m}");
                }
                io::print(&s)?;
            }
            Ok(Outcome::from_ok(info.patch_checksum_ok != Some(false)))
        }
    }
}
