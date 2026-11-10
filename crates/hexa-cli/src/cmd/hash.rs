//! `hash`: checksums, CRCs, cryptographic, fast and fuzzy hashes.

use std::fmt::Write as _;
use std::path::PathBuf;

use clap::Args;
use hexa_core::Cancel;
use serde_json::json;

use super::split_list;
use crate::error::{CliError, CliResult, Outcome};
use crate::io::{self, RangeArgs, no_progress, open_tree};

/// Algorithms used when neither `-a` nor `--expect` is given.
const DEFAULT: &[&str] = &["md5", "sha1", "sha256", "crc32"];

/// Short aliases for common CRC presets.
const ALIASES: &[(&str, &str)] = &[
    ("crc8", "crc:CRC-8/SMBUS"),
    ("crc16", "crc:CRC-16/ARC"),
    ("crc16-ccitt", "crc:CRC-16/IBM-3740 (CCITT-FALSE)"),
    ("crc16-modbus", "crc:CRC-16/MODBUS"),
    ("crc16-xmodem", "crc:CRC-16/XMODEM"),
    ("crc32", "crc:CRC-32/ISO-HDLC"),
    ("crc32c", "crc:CRC-32/ISCSI (CRC-32C)"),
    ("crc32-bzip2", "crc:CRC-32/BZIP2"),
    ("crc32-mpeg2", "crc:CRC-32/MPEG-2"),
    ("crc64", "crc:CRC-64/XZ"),
    ("crc64-ecma", "crc:CRC-64/ECMA-182"),
];

#[derive(Debug, Args)]
pub struct HashArgs {
    /// Input files (`-` = stdin).
    #[arg(required_unless_present = "list")]
    pub files: Vec<PathBuf>,
    /// Algorithms, comma separated (default: md5,sha1,sha256,crc32). See
    /// --list; CRC presets are `crc:<name>`, e.g. `crc:CRC-16/MODBUS`.
    #[arg(short, long = "algo", value_name = "IDS")]
    pub algos: Vec<String>,
    /// Expected value `ID=HEX` (repeatable); exit 1 on any mismatch.
    #[arg(long, value_name = "ID=HEX")]
    pub expect: Vec<String>,
    /// List the available algorithms and exit.
    #[arg(long)]
    pub list: bool,
    #[command(flatten)]
    pub range: RangeArgs,
}

/// Every algorithm id (core + analysis), plus the aliases.
fn known_ids() -> Vec<hexa_core::hash::AlgoInfo> {
    let mut v = hexa_core::hash::algorithms();
    v.extend(hexa_analysis::hash::algorithms());
    v
}

fn resolve(id: &str, known: &[hexa_core::hash::AlgoInfo]) -> CliResult<String> {
    let lower = id.to_ascii_lowercase();
    if let Some((_, full)) = ALIASES.iter().find(|(a, _)| *a == lower) {
        return Ok((*full).to_owned());
    }
    if let Some(a) = known.iter().find(|a| a.id.eq_ignore_ascii_case(id)) {
        return Ok(a.id.clone());
    }
    Err(CliError::usage(format!(
        "unknown hash algorithm `{id}` (see `hexa-cli hash --list`)"
    )))
}

fn normalize_hex(v: &str) -> String {
    let v = v.trim();
    let v = v
        .strip_prefix("0x")
        .or_else(|| v.strip_prefix("0X"))
        .unwrap_or(v);
    v.to_ascii_lowercase()
}

pub fn run(a: &HashArgs, json_out: bool) -> CliResult {
    let known = known_ids();
    if a.list {
        if json_out {
            io::print_json(&json!({ "algorithms": known, "aliases": ALIASES
                .iter()
                .map(|(a, f)| json!({"alias": a, "id": f}))
                .collect::<Vec<_>>() }))?;
        } else {
            let mut s = String::new();
            for x in &known {
                let _ = writeln!(s, "{:<10} {:<36} {}", x.group, x.id, x.name);
            }
            for (alias, full) in ALIASES {
                let _ = writeln!(s, "{:<10} {alias:<36} = {full}", "alias");
            }
            io::print(&s)?;
        }
        return Ok(Outcome::Ok);
    }

    let mut names = split_list(&a.algos);
    let mut expects: Vec<(String, String)> = Vec::new();
    for e in &a.expect {
        let (name, value) = e
            .split_once('=')
            .ok_or_else(|| CliError::usage(format!("--expect `{e}` must be ID=HEX")))?;
        names.push(name.trim().to_owned());
        expects.push((resolve(name.trim(), &known)?, normalize_hex(value)));
    }
    if names.is_empty() {
        names = DEFAULT.iter().map(|s| (*s).to_owned()).collect();
    }
    // (display name, resolved id)
    let mut algos: Vec<(String, String)> = Vec::new();
    for name in names {
        let id = resolve(&name, &known)?;
        if !algos.iter().any(|(_, i)| *i == id) {
            algos.push((name, id));
        }
    }
    let ids: Vec<String> = algos.iter().map(|(_, id)| id.clone()).collect();

    let cancel = Cancel::new();
    let mut files = Vec::new();
    let mut mismatches = Vec::new();
    let mut text = String::new();
    for path in &a.files {
        let tree = open_tree(path)?;
        let (start, end) = a.range.resolve(tree.len())?;
        let results =
            hexa_analysis::hash::hash_range(&tree, start, end, &ids, None, &cancel, &no_progress)?;
        let mut hashes = Vec::new();
        for ((name, id), r) in algos.iter().zip(&results) {
            hashes.push(json!({ "id": name, "algorithm": id, "value": r.value }));
            let _ = writeln!(text, "{name:<12} {}  {}", r.value, path.display());
            for (eid, want) in &expects {
                if eid == id && normalize_hex(&r.value) != *want {
                    mismatches.push(json!({
                        "file": path.display().to_string(),
                        "id": name,
                        "expected": want,
                        "actual": r.value,
                    }));
                    eprintln!(
                        "hexa-cli: {}: {name} mismatch: expected {want}, got {}",
                        path.display(),
                        r.value
                    );
                }
            }
        }
        files.push(json!({
            "file": path.display().to_string(),
            "start": start,
            "end": end,
            "hashes": hashes,
        }));
    }
    let ok = mismatches.is_empty();
    if json_out {
        let mut v = json!({ "files": files });
        if !expects.is_empty() {
            v["expect"] = json!({ "ok": ok, "mismatches": mismatches });
        }
        io::print_json(&v)?;
    } else {
        io::print(&text)?;
    }
    Ok(Outcome::from_ok(ok))
}
