//! `info`: size, entropy, detected type and executable summary.

use std::fmt::Write as _;
use std::path::PathBuf;

use clap::Args;
use hexa_core::Cancel;
use serde_json::{Value, json};

use super::name_of;
use crate::error::{CliResult, Outcome};
use crate::io::{self, human_size, no_progress, open_tree};

/// Executables larger than this are not parsed by `info`.
const MAX_EXE: u64 = 512 << 20;
/// Bytes inspected for magic-based detection.
const HEAD: u64 = 64 << 10;

#[derive(Debug, Args)]
pub struct InfoArgs {
    /// Input file (`-` = stdin).
    pub file: PathBuf,
    /// Skip the entropy / histogram pass over the whole file.
    #[arg(long)]
    pub no_entropy: bool,
    /// Also classify regions (zero, text, code, compressed …) in blocks of
    /// this size.
    #[arg(long, value_name = "BLOCK", num_args = 0..=1, default_missing_value = "4096",
          value_parser = io::parse_u64)]
    pub classify: Option<u64>,
}

pub fn run(a: &InfoArgs, json_out: bool) -> CliResult {
    let tree = open_tree(&a.file)?;
    let len = tree.len();
    let cancel = Cancel::new();
    let head = tree.read_vec(0, HEAD.min(len) as usize)?;
    let file_name = a.file.file_name().map(|n| n.to_string_lossy().into_owned());

    let mut out = serde_json::Map::new();
    out.insert("file".into(), json!(a.file.display().to_string()));
    out.insert("size".into(), json!(len));

    if !a.no_entropy {
        let h = hexa_analysis::stats::histogram(&tree, 0, len, &cancel, &no_progress)?;
        out.insert("entropy".into(), json!(h.entropy));
        out.insert("distinctBytes".into(), json!(h.distinct));
        out.insert("mostCommonByte".into(), json!(h.most_common));
    }

    // Magic-based detection of the first file at offset 0.
    let first = hexa_analysis::embedded::find_embedded(
        &tree,
        0,
        len.min(HEAD),
        &hexa_analysis::embedded::EmbeddedOptions::default(),
        &cancel,
        &no_progress,
    )?
    .into_iter()
    .find(|e| e.offset == 0);
    let mut types: Vec<String> = Vec::new();
    if let Some(e) = &first {
        types.push(e.kind.clone());
        out.insert("description".into(), json!(e.description));
    }
    if let Some(f) = hexa_diff::detect_format(&head) {
        types.push(format!("patch:{}", name_of(&f)));
    }
    if let Ok(text) = std::str::from_utf8(&head)
        && let Some(f) = hexa_analysis::firmware::detect_format(text)
    {
        types.push(format!("firmware:{}", name_of(&f)));
    }
    out.insert("types".into(), json!(types));
    let templates = hexa_template::library::detect(&head, file_name.as_deref());
    out.insert("templates".into(), json!(templates));

    let exe_format = hexa_re::exe::detect_format(&head);
    if exe_format.is_some() && len <= MAX_EXE {
        let data = io::tree_bytes(&tree, 0, len, MAX_EXE)?;
        match hexa_re::exe::parse(&data) {
            Ok(info) => {
                let findings: Vec<Value> = hexa_re::detect::detect(&data)
                    .into_iter()
                    .filter(|f| f.confidence > hexa_re::detect::Confidence::Low)
                    .map(|f| {
                        json!({
                            "name": f.name,
                            "category": name_of(&f.category),
                            "version": f.version,
                            "confidence": name_of(&f.confidence),
                        })
                    })
                    .collect();
                out.insert(
                    "executable".into(),
                    json!({
                        "format": name_of(&info.format),
                        "bits": info.bits,
                        "endian": name_of(&info.endian),
                        "machine": info.machine,
                        "arch": info.arch.map(|a| name_of(&a)),
                        "kind": info.kind,
                        "entryPoint": info.entry_point,
                        "entryOffset": info.entry_offset,
                        "imageBase": info.image_base(),
                        "sections": info.sections.len(),
                        "segments": info.segments.len(),
                        "imports": info.imports.len(),
                        "exports": info.exports.len(),
                        "symbols": info.symbols.len(),
                        "libraries": info.libraries,
                        "slices": info.slices.iter().map(|s| &s.arch_name).collect::<Vec<_>>(),
                        "detected": findings,
                        "warnings": info.warnings,
                    }),
                );
            }
            Err(e) => {
                out.insert("executableError".into(), json!(e.to_string()));
            }
        }
    }

    if let Some(block) = a.classify {
        let regions =
            hexa_analysis::classify::classify(&tree, 0, len, block, &cancel, &no_progress)
                .map_err(crate::error::invalid_is_usage)?;
        out.insert("regions".into(), serde_json::to_value(regions)?);
    }

    let out = Value::Object(out);
    if json_out {
        io::print_json(&out)?;
    } else {
        io::print(&render(&out))?;
    }
    Ok(Outcome::Ok)
}

fn render(v: &Value) -> String {
    let mut s = String::new();
    let get = |k: &str| v.get(k).filter(|x| !x.is_null());
    let _ = writeln!(s, "File:        {}", v["file"].as_str().unwrap_or_default());
    let _ = writeln!(
        s,
        "Size:        {}",
        human_size(v["size"].as_u64().unwrap_or(0))
    );
    if let Some(e) = get("entropy").and_then(Value::as_f64) {
        let _ = writeln!(
            s,
            "Entropy:     {e:.4} bits/byte ({} distinct byte values)",
            v["distinctBytes"]
        );
    }
    if let Some(t) = get("types")
        .and_then(Value::as_array)
        .filter(|t| !t.is_empty())
    {
        let names: Vec<&str> = t.iter().filter_map(Value::as_str).collect();
        let _ = writeln!(s, "Type:        {}", names.join(", "));
    }
    if let Some(d) = get("description").and_then(Value::as_str) {
        let _ = writeln!(s, "Description: {d}");
    }
    if let Some(t) = get("templates")
        .and_then(Value::as_array)
        .filter(|t| !t.is_empty())
    {
        let names: Vec<&str> = t.iter().filter_map(Value::as_str).collect();
        let _ = writeln!(s, "Templates:   {}", names.join(", "));
    }
    if let Some(x) = get("executable").filter(|x| x["format"] == "machOFat") {
        let slices: Vec<&str> = x["slices"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let _ = writeln!(s, "Executable:  universal binary ({})", slices.join(", "));
    } else if let Some(x) = get("executable") {
        let _ = writeln!(
            s,
            "Executable:  {} {}-bit {} {} ({})",
            x["format"].as_str().unwrap_or_default(),
            x["bits"],
            x["endian"].as_str().unwrap_or_default(),
            x["machine"].as_str().unwrap_or_default(),
            x["kind"].as_str().unwrap_or_default(),
        );
        if let Some(ep) = x["entryPoint"].as_u64() {
            let _ = writeln!(s, "Entry point: 0x{ep:X}");
        }
        let _ = writeln!(
            s,
            "Contents:    {} sections, {} segments, {} imports, {} exports, {} symbols",
            x["sections"], x["segments"], x["imports"], x["exports"], x["symbols"]
        );
        if let Some(d) = x["detected"].as_array().filter(|d| !d.is_empty()) {
            let names: Vec<String> = d
                .iter()
                .map(|f| match f["version"].as_str() {
                    Some(ver) => format!("{} {ver}", f["name"].as_str().unwrap_or_default()),
                    None => f["name"].as_str().unwrap_or_default().to_owned(),
                })
                .collect();
            let _ = writeln!(s, "Detected:    {}", names.join(", "));
        }
    }
    if let Some(e) = get("executableError").and_then(Value::as_str) {
        let _ = writeln!(s, "Executable:  parse error: {e}");
    }
    if let Some(r) = get("regions").and_then(Value::as_array) {
        let _ = writeln!(s, "Regions:");
        for x in r {
            let _ = writeln!(
                s,
                "  {:>12X}..{:<12X} {:<10} {}",
                x["start"].as_u64().unwrap_or(0),
                x["end"].as_u64().unwrap_or(0),
                x["kind"].as_str().unwrap_or_default(),
                x["detail"].as_str().unwrap_or_default()
            );
        }
    }
    s
}
