//! `strings`: extract ASCII / UTF-8 / UTF-16 strings.

use std::path::PathBuf;

use clap::Args;
use hexa_analysis::strings::{
    StringEncoding, StringsOptions, extract_strings, strings_to_csv, strings_to_text,
};
use hexa_core::Cancel;

use super::split_list;
use crate::error::{CliError, CliResult, Outcome};
use crate::io::{self, RangeArgs, no_progress, open_tree};

#[derive(Debug, Args)]
pub struct StringsArgs {
    /// Input file (`-` = stdin).
    pub file: PathBuf,
    /// Minimum length in characters.
    #[arg(short = 'n', long, default_value_t = 4)]
    pub min_len: usize,
    /// Encodings, comma separated: ascii, utf8, utf16le, utf16be (default:
    /// all).
    #[arg(short, long, value_name = "LIST")]
    pub encoding: Vec<String>,
    /// Stop after this many strings.
    #[arg(long, default_value_t = 100_000)]
    pub max: usize,
    /// Keep only strings containing this text (case-insensitive).
    #[arg(long)]
    pub contains: Option<String>,
    /// Keep only strings matching this regular expression.
    #[arg(long)]
    pub regex: Option<String>,
    /// Allow tab / CR / LF inside strings.
    #[arg(long)]
    pub whitespace: bool,
    /// CSV output (`offset,length,encoding,text`).
    #[arg(long)]
    pub csv: bool,
    #[command(flatten)]
    pub range: RangeArgs,
}

fn encoding(s: &str) -> CliResult<StringEncoding> {
    Ok(
        match s.to_ascii_lowercase().replace(['-', '_'], "").as_str() {
            "ascii" => StringEncoding::Ascii,
            "utf8" => StringEncoding::Utf8,
            "utf16le" | "utf16" => StringEncoding::Utf16Le,
            "utf16be" => StringEncoding::Utf16Be,
            _ => return Err(CliError::usage(format!("unknown string encoding `{s}`"))),
        },
    )
}

pub fn run(a: &StringsArgs, json_out: bool) -> CliResult {
    let mut opts = StringsOptions {
        min_len: a.min_len.max(1),
        max_results: a.max.max(1),
        include_whitespace: a.whitespace,
        contains: a.contains.clone(),
        regex: a.regex.clone(),
        ..StringsOptions::default()
    };
    let encodings = split_list(&a.encoding);
    if !encodings.is_empty() {
        opts.encodings = encodings
            .iter()
            .map(|e| encoding(e))
            .collect::<CliResult<_>>()?;
    }
    let tree = open_tree(&a.file)?;
    let (start, end) = a.range.resolve(tree.len())?;
    let res = extract_strings(&tree, start, end, &opts, &Cancel::new(), &no_progress)
        .map_err(crate::error::invalid_is_usage)?;
    if json_out {
        io::print_json(&res)?;
    } else if a.csv {
        io::print(&strings_to_csv(&res.strings))?;
    } else {
        io::print(&strings_to_text(&res.strings))?;
    }
    if res.truncated {
        eprintln!(
            "hexa-cli: stopped after {} strings (use --max)",
            opts.max_results
        );
    }
    Ok(Outcome::Ok)
}
