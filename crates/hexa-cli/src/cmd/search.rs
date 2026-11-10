//! `search`: hex / text / regex / number / sequence search.

use std::fmt::Write as _;
use std::path::PathBuf;

use clap::{ArgGroup, Args, ValueEnum};
use hexa_core::Cancel;
use hexa_core::search::{EndianSel, NumType, Query, Scope, SeqItem, compile, find_all};
use serde_json::json;

use super::split_list;
use crate::error::{CliError, CliResult, Outcome};
use crate::io::{self, RangeArgs, no_progress, open_tree, parse_u64};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum EndianArg {
    Little,
    Big,
    Both,
}

#[derive(Debug, Args)]
#[command(group(ArgGroup::new("pattern").required(true)
    .args(["hex", "text", "regex", "number", "seq"])))]
pub struct SearchArgs {
    /// Input file (`-` = stdin).
    pub file: PathBuf,
    /// Hex bytes; `??` / `4?` wildcards, `{01xx0101}` bit masks.
    #[arg(long, value_name = "PATTERN")]
    pub hex: Option<String>,
    /// Text, encoded with each of --encoding.
    #[arg(long, value_name = "TEXT")]
    pub text: Option<String>,
    /// Regular expression over bytes (or UTF-8 text with --unicode).
    #[arg(long, value_name = "REGEX")]
    pub regex: Option<String>,
    /// A number (or `MIN..MAX`) of --type.
    #[arg(long, value_name = "VALUE", allow_hyphen_values = true)]
    pub number: Option<String>,
    /// Typed sequence, e.g. `u32le=0x10,u16be=1..5,u8` (a bare type is a
    /// wildcard; types i8…u64, f32, f64; `le`/`be` suffix, `~TOL` for
    /// floats).
    #[arg(long, value_name = "ITEMS", allow_hyphen_values = true)]
    pub seq: Option<String>,
    /// Text encodings, comma separated (utf-8, utf-16le, latin1 …).
    #[arg(long, value_name = "IDS", default_value = "utf-8")]
    pub encoding: Vec<String>,
    /// Case-insensitive text search.
    #[arg(short = 'i', long)]
    pub ignore_case: bool,
    /// Regex works on UTF-8 text instead of raw bytes.
    #[arg(long)]
    pub unicode: bool,
    /// Number type for --number.
    #[arg(long = "type", value_name = "TYPE", default_value = "u32")]
    pub ty: String,
    /// Byte order for --number.
    #[arg(long, value_enum, default_value_t = EndianArg::Little)]
    pub endian: EndianArg,
    /// Float tolerance for --number.
    #[arg(long)]
    pub tolerance: Option<f64>,
    /// Only report matches at multiples of this.
    #[arg(long, value_parser = parse_u64, default_value = "1")]
    pub align: u64,
    /// Stop after this many matches.
    #[arg(long, value_parser = parse_u64, default_value = "1000000")]
    pub max: u64,
    /// Print only the number of matches (counts all of them).
    #[arg(short = 'c', long)]
    pub count: bool,
    #[command(flatten)]
    pub range: RangeArgs,
}

fn num_type(s: &str) -> CliResult<NumType> {
    serde_json::from_value(json!(s.to_ascii_lowercase()))
        .map_err(|_| CliError::usage(format!("unknown number type `{s}`")))
}

/// Parses one `--seq` item: `TYPE[le|be][=VALUE[..MAX]][~TOL]`.
fn seq_item(item: &str) -> CliResult<SeqItem> {
    let (head, rest) = match item.split_once('=') {
        Some((h, r)) => (h.trim(), Some(r.trim())),
        None => (item.trim(), None),
    };
    let (ty, big_endian) = if let Some(t) = head.strip_suffix("be") {
        (t, true)
    } else if let Some(t) = head.strip_suffix("le") {
        (t, false)
    } else {
        (head, false)
    };
    let ty = num_type(ty)?;
    let (mut value, mut max, mut tolerance) = (None, None, None);
    if let Some(r) = rest {
        let (r, tol) = match r.split_once('~') {
            Some((v, t)) => (v, Some(t)),
            None => (r, None),
        };
        if let Some(t) = tol {
            tolerance = Some(
                t.parse::<f64>()
                    .map_err(|_| CliError::usage(format!("bad tolerance `{t}`")))?,
            );
        }
        match r.split_once("..") {
            Some((lo, hi)) => {
                value = Some(lo.trim().to_owned());
                max = Some(hi.trim_start_matches('=').trim().to_owned());
            }
            None => value = Some(r.to_owned()),
        }
    }
    Ok(SeqItem {
        ty,
        big_endian,
        value,
        max,
        tolerance,
    })
}

fn query(a: &SearchArgs) -> CliResult<Query> {
    if let Some(p) = &a.hex {
        return Ok(Query::Hex { pattern: p.clone() });
    }
    if let Some(t) = &a.text {
        return Ok(Query::Text {
            text: t.clone(),
            encodings: split_list(&a.encoding),
            case_sensitive: !a.ignore_case,
        });
    }
    if let Some(r) = &a.regex {
        return Ok(Query::Regex {
            pattern: r.clone(),
            unicode: a.unicode,
        });
    }
    if let Some(n) = &a.number {
        let (value, max) = match n.split_once("..") {
            Some((lo, hi)) => (lo.to_owned(), Some(hi.trim_start_matches('=').to_owned())),
            None => (n.clone(), None),
        };
        return Ok(Query::Number {
            ty: num_type(&a.ty)?,
            endian: match a.endian {
                EndianArg::Little => EndianSel::Little,
                EndianArg::Big => EndianSel::Big,
                EndianArg::Both => EndianSel::Both,
            },
            value,
            max,
            tolerance: a.tolerance,
        });
    }
    let s = a.seq.as_deref().unwrap_or_default();
    let items = s
        .split(',')
        .filter(|i| !i.trim().is_empty())
        .map(seq_item)
        .collect::<CliResult<Vec<_>>>()?;
    if items.is_empty() {
        return Err(CliError::usage("--seq needs at least one item"));
    }
    Ok(Query::Sequence { items })
}

pub fn run(a: &SearchArgs, json_out: bool) -> CliResult {
    let q = query(a)?;
    let matcher = compile(&q).map_err(crate::error::invalid_is_usage)?;
    let tree = open_tree(&a.file)?;
    let (start, end) = a.range.resolve(tree.len())?;
    let scope = Scope {
        start,
        end,
        align: a.align.max(1),
    };
    let max = if a.count {
        usize::MAX
    } else {
        usize::try_from(a.max.max(1)).unwrap_or(usize::MAX)
    };
    let mut matches = Vec::new();
    let mut count = 0usize;
    let n = find_all(
        &tree,
        &matcher,
        scope,
        max,
        &Cancel::new(),
        &no_progress,
        |m| {
            count += 1;
            if !a.count {
                matches.push(m);
            }
        },
    )?;
    let truncated = !a.count && n >= max;
    if json_out {
        if a.count {
            io::print_json(&json!({ "count": count }))?;
        } else {
            io::print_json(&json!({
                "count": count,
                "truncated": truncated,
                "matches": matches,
            }))?;
        }
    } else if a.count {
        io::print(&format!("{count}\n"))?;
    } else {
        let mut s = String::new();
        for m in &matches {
            let _ = writeln!(s, "0x{:08X}\t{}", m.offset, m.len);
        }
        io::print(&s)?;
        if truncated {
            eprintln!("hexa-cli: stopped after {max} matches (use --max)");
        }
    }
    Ok(Outcome::from_ok(count > 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seq_items() {
        let i = seq_item("u32be=0x10..0x20").unwrap();
        assert_eq!(i.ty, NumType::U32);
        assert!(i.big_endian);
        assert_eq!(i.value.as_deref(), Some("0x10"));
        assert_eq!(i.max.as_deref(), Some("0x20"));
        let w = seq_item("u8").unwrap();
        assert!(w.value.is_none());
        let f = seq_item("f32le=1.5~0.01").unwrap();
        assert_eq!(f.tolerance, Some(0.01));
        assert!(seq_item("u7").is_err());
    }
}
