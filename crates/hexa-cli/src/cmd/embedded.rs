//! `embedded` / `carve`: find embedded files and optionally extract them.

use std::fmt::Write as _;
use std::path::PathBuf;

use clap::Args;
use hexa_analysis::embedded::{EmbeddedOptions, KINDS, find_embedded};
use hexa_core::Cancel;
use serde_json::json;

use super::split_list;
use crate::error::{CliError, CliResult, Outcome};
use crate::io::{self, RangeArgs, no_progress, open_tree};

#[derive(Debug, Args)]
pub struct EmbeddedArgs {
    /// Input file (`-` = stdin).
    pub file: PathBuf,
    /// Only these kinds, comma separated (png, jpeg, zip, pdf, elf, pe …).
    #[arg(short, long, value_name = "LIST")]
    pub kinds: Vec<String>,
    /// Stop after this many results.
    #[arg(long, default_value_t = 10_000)]
    pub max: usize,
    /// Write every file with a known length to DIR as `<offset>.<kind>`.
    #[arg(short = 'x', long, value_name = "DIR")]
    pub extract: Option<PathBuf>,
    /// Do not skip the file found at offset 0 (the container itself).
    #[arg(long)]
    pub include_self: bool,
    #[command(flatten)]
    pub range: RangeArgs,
}

pub fn run(a: &EmbeddedArgs, json_out: bool) -> CliResult {
    let kinds = split_list(&a.kinds);
    if let Some(k) = kinds.iter().find(|k| !KINDS.contains(&k.as_str())) {
        return Err(CliError::usage(format!(
            "unknown kind `{k}` (one of: {})",
            KINDS.join(", ")
        )));
    }
    let opts = EmbeddedOptions {
        kinds,
        max_results: a.max.max(1),
        ..EmbeddedOptions::default()
    };
    let tree = open_tree(&a.file)?;
    let (start, end) = a.range.resolve(tree.len())?;
    let found: Vec<_> = find_embedded(&tree, start, end, &opts, &Cancel::new(), &no_progress)?
        .into_iter()
        .filter(|e| a.include_self || e.offset != 0)
        .collect();

    let mut extracted = Vec::new();
    if let Some(dir) = &a.extract {
        std::fs::create_dir_all(dir).map_err(|e| CliError::from(e).context(dir.display()))?;
        for e in &found {
            let Some(len) = e.len_guess else { continue };
            let end = e.offset.saturating_add(len).min(tree.len());
            let path = dir.join(format!("{:08X}.{}", e.offset, e.kind));
            let mut sink = io::Sink::create(Some(&path))?;
            sink.write_tree(&tree, e.offset, end)?;
            sink.finish()?;
            extracted.push(path.display().to_string());
        }
    }

    if json_out {
        io::print_json(&json!({ "found": found, "extracted": extracted }))?;
    } else {
        let mut s = String::new();
        for e in &found {
            let len = e
                .len_guess
                .map_or_else(|| "?".to_owned(), |l| l.to_string());
            let _ = writeln!(
                s,
                "0x{:08X} {:>12} {:<8} {}",
                e.offset, len, e.kind, e.description
            );
        }
        for p in &extracted {
            let _ = writeln!(s, "extracted {p}");
        }
        io::print(&s)?;
    }
    Ok(Outcome::from_ok(!found.is_empty()))
}
