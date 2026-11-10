//! `exe`, `pe-checksum` and `exe-patch`: executable analysis (RE-01, RE-02,
//! RE-05, RE-06, RE-07) and disassembly (ANL-12).

use std::fmt::Write as _;
use std::path::PathBuf;

use clap::Args;
use hexa_re::Arch;
use hexa_re::disasm::{DisasmOptions, Instruction, disassemble};
use hexa_re::exe::ExecutableInfo;
use hexa_re::patch::{PatchList, PatchState};
use serde_json::{Value, json};

use super::name_of;
use crate::error::{CliError, CliResult, Outcome};
use crate::io::{self, open_tree, parse_u64, read_text};

/// Executables are parsed in memory; larger inputs are refused.
const MAX_EXE: u64 = 1 << 30;

#[derive(Debug, Args)]
pub struct ExeArgs {
    /// Executable (PE, ELF, Mach-O, universal binary).
    pub file: PathBuf,
    /// Show header structures.
    #[arg(long)]
    pub headers: bool,
    /// Show sections and segments.
    #[arg(long)]
    pub sections: bool,
    /// Show imports and needed libraries.
    #[arg(long)]
    pub imports: bool,
    /// Show exports.
    #[arg(long)]
    pub exports: bool,
    /// Show symbols.
    #[arg(long)]
    pub symbols: bool,
    /// Show compiler / packer / runtime detection (RE-07).
    #[arg(long)]
    pub detect: bool,
    /// Show everything above.
    #[arg(short, long)]
    pub all: bool,
    /// Disassemble at a virtual address (`entry` = entry point; with
    /// --offset the value is a file offset).
    #[arg(long, value_name = "ADDR", num_args = 0..=1, default_missing_value = "entry")]
    pub disasm: Option<String>,
    /// Treat the --disasm address as a file offset.
    #[arg(long, requires = "disasm")]
    pub offset: bool,
    /// Number of instructions to disassemble.
    #[arg(short = 'n', long, default_value_t = 32)]
    pub count: usize,
    /// Architecture override / raw-code architecture (`x86`, `x86_64`,
    /// `arm`, `thumb`, `arm64`, `mips32`, `ppc32`, `riscv64`, `wasm` …). With a non-executable
    /// input the file is disassembled as raw code.
    #[arg(long)]
    pub arch: Option<String>,
    /// Virtual address of offset 0 for raw code.
    #[arg(long, value_parser = parse_u64, default_value = "0")]
    pub base: u64,
    /// AT&T syntax for x86.
    #[arg(long)]
    pub att: bool,
}

fn parse_arch(s: &str) -> CliResult<Arch> {
    serde_json::from_value(json!(s.to_ascii_lowercase())).map_err(|_| {
        let names: Vec<String> = Arch::catalog().iter().map(|a| name_of(&a.arch)).collect();
        CliError::usage(format!(
            "unknown architecture `{s}` (one of: {})",
            names.join(", ")
        ))
    })
}

fn load(file: &std::path::Path) -> CliResult<Vec<u8>> {
    let tree = open_tree(file)?;
    io::tree_bytes(&tree, 0, tree.len(), MAX_EXE)
}

pub fn run(a: &ExeArgs, json_out: bool) -> CliResult {
    let data = load(&a.file)?;
    let arch_override = a.arch.as_deref().map(parse_arch).transpose()?;
    let info = match hexa_re::exe::parse(&data) {
        Ok(i) => Some(i),
        Err(hexa_re::ReError::UnknownFormat) if arch_override.is_some() => None,
        Err(e) => return Err(e.into()),
    };

    let disasm = match &a.disasm {
        Some(addr) => Some(run_disasm(a, &data, info.as_ref(), arch_override, addr)?),
        None => None,
    };

    let all = a.all;
    let mut out = serde_json::Map::new();
    if let Some(info) = &info {
        let wanted_any = a.headers || a.sections || a.imports || a.exports || a.symbols;
        if json_out && (all || !wanted_any) && disasm.is_none() {
            out.insert("executable".into(), serde_json::to_value(info)?);
        } else {
            out.insert("format".into(), json!(name_of(&info.format)));
            out.insert("bits".into(), json!(info.bits));
            out.insert("machine".into(), json!(info.machine));
            out.insert("arch".into(), json!(info.arch.map(|x| name_of(&x))));
            out.insert("kind".into(), json!(info.kind));
            out.insert("entryPoint".into(), json!(info.entry_point));
            out.insert("imageBase".into(), json!(info.image_base()));
            if all || a.headers {
                out.insert("headers".into(), serde_json::to_value(&info.headers)?);
            }
            if all || a.sections {
                out.insert("sections".into(), serde_json::to_value(&info.sections)?);
                out.insert("segments".into(), serde_json::to_value(&info.segments)?);
            }
            if all || a.imports {
                out.insert("libraries".into(), serde_json::to_value(&info.libraries)?);
                out.insert("imports".into(), serde_json::to_value(&info.imports)?);
            }
            if all || a.exports {
                out.insert("exports".into(), serde_json::to_value(&info.exports)?);
            }
            if all || a.symbols {
                out.insert("symbols".into(), serde_json::to_value(&info.symbols)?);
            }
        }
    }
    if all || a.detect {
        out.insert(
            "detected".into(),
            serde_json::to_value(hexa_re::detect::detect(&data))?,
        );
    }
    if let Some(d) = &disasm {
        out.insert("disassembly".into(), serde_json::to_value(d)?);
    }

    if json_out {
        io::print_json(&Value::Object(out))?;
        return Ok(Outcome::Ok);
    }
    let mut s = String::new();
    let show_summary = disasm.is_none() || a.all;
    if let (Some(info), true) = (&info, show_summary) {
        render_info(&mut s, info, a);
    }
    if let Some(Value::Array(found)) = out.get("detected") {
        let _ = writeln!(s, "\nDetected:");
        for f in found {
            let _ = writeln!(
                s,
                "  {:<10} {} {}  ({})",
                f["category"].as_str().unwrap_or_default(),
                f["name"].as_str().unwrap_or_default(),
                f["version"].as_str().unwrap_or_default(),
                f["confidence"].as_str().unwrap_or_default()
            );
        }
    }
    if let Some(d) = &disasm {
        if show_summary && info.is_some() {
            s.push('\n');
        }
        for i in d {
            let _ = writeln!(
                s,
                "{:016X}  {:<24} {}",
                i.address,
                io::hex(&i.bytes),
                i.text()
            );
        }
    }
    io::print(&s)?;
    Ok(Outcome::Ok)
}

fn render_info(s: &mut String, info: &ExecutableInfo, a: &ExeArgs) {
    let all = a.all;
    let _ = writeln!(
        s,
        "Format:      {} {}-bit {} ({})",
        name_of(&info.format),
        info.bits,
        info.machine,
        info.kind
    );
    if let Some(ep) = info.entry_point {
        let _ = writeln!(s, "Entry point: 0x{ep:X}");
    }
    let _ = writeln!(s, "Image base:  0x{:X}", info.image_base());
    if let Some(i) = &info.interpreter {
        let _ = writeln!(s, "Interpreter: {i}");
    }
    for w in &info.warnings {
        let _ = writeln!(s, "Warning:     {w}");
    }
    let nothing_selected = !(a.headers || a.sections || a.imports || a.exports || a.symbols);
    if all || a.headers {
        let _ = writeln!(s, "\nHeaders:");
        for h in &info.headers {
            let _ = writeln!(s, "  {:>10X} {:>8X}  {}", h.offset, h.size, h.name);
        }
    }
    if all || a.sections || nothing_selected {
        let _ = writeln!(
            s,
            "\nSections:\n  {:<20} {:>16} {:>10} {:>10} {:>10} perm",
            "name", "vaddr", "vsize", "offset", "size"
        );
        for x in &info.sections {
            let _ = writeln!(
                s,
                "  {:<20} {:>16X} {:>10X} {:>10X} {:>10X} {}",
                x.name, x.virtual_address, x.virtual_size, x.offset, x.file_size, x.permissions
            );
        }
    }
    if all || a.sections {
        let _ = writeln!(s, "\nSegments:");
        for x in &info.segments {
            let _ = writeln!(
                s,
                "  {:<20} {:>16X} {:>10X} {:>10X} {:>10X} {}",
                x.name, x.virtual_address, x.virtual_size, x.offset, x.file_size, x.permissions
            );
        }
    }
    if all || a.imports {
        let _ = writeln!(s, "\nLibraries:");
        for l in &info.libraries {
            let _ = writeln!(s, "  {l}");
        }
        let _ = writeln!(s, "\nImports:");
        for i in &info.imports {
            let _ = writeln!(
                s,
                "  {:<24} {}",
                i.library.as_deref().unwrap_or("-"),
                i.demangled.as_deref().unwrap_or(&i.name)
            );
        }
    }
    if all || a.exports {
        let _ = writeln!(s, "\nExports:");
        for e in &info.exports {
            let _ = writeln!(
                s,
                "  {:>16X}  {}",
                e.address,
                e.demangled.as_deref().unwrap_or(&e.name)
            );
        }
    }
    if all || a.symbols {
        let _ = writeln!(s, "\nSymbols:");
        for x in &info.symbols {
            let _ = writeln!(
                s,
                "  {:>16X} {:>8} {:<8} {}",
                x.address,
                x.size,
                x.kind,
                x.demangled.as_deref().unwrap_or(&x.name)
            );
        }
    }
}

fn run_disasm(
    a: &ExeArgs,
    data: &[u8],
    info: Option<&ExecutableInfo>,
    arch_override: Option<Arch>,
    addr: &str,
) -> CliResult<Vec<Instruction>> {
    let arch = arch_override
        .or_else(|| info.and_then(|i| i.arch))
        .ok_or_else(|| CliError::usage("unknown architecture; pass --arch"))?;
    let (offset, va) = if let Some(info) = info {
        if addr == "entry" {
            let va = info
                .entry_point
                .ok_or_else(|| CliError::data("the executable has no entry point"))?;
            let off = info
                .entry_offset
                .or_else(|| info.va_to_offset(va))
                .ok_or_else(|| CliError::data(format!("entry point 0x{va:X} is not mapped")))?;
            (off, va)
        } else {
            let v = parse_u64(addr).map_err(CliError::usage)?;
            if a.offset {
                (v, info.offset_to_va(v).unwrap_or(v))
            } else {
                let off = info.va_to_offset(v).ok_or_else(|| {
                    CliError::usage(format!("address 0x{v:X} is not backed by file data"))
                })?;
                (off, v)
            }
        }
    } else {
        // Raw code: ADDR is a virtual address relative to --base unless it
        // is below the base or --offset is given.
        let v = if addr == "entry" {
            0
        } else {
            parse_u64(addr).map_err(CliError::usage)?
        };
        if a.offset || v < a.base {
            (v, a.base.wrapping_add(v))
        } else {
            (v - a.base, v)
        }
    };
    let start = usize::try_from(offset).unwrap_or(usize::MAX);
    if start >= data.len() {
        return Err(CliError::usage(format!(
            "offset 0x{offset:X} is beyond the end of the file"
        )));
    }
    let want = a.count.max(1);
    let end = data
        .len()
        .min(start.saturating_add(want.saturating_mul(16)));
    let opts = DisasmOptions {
        max_instructions: want,
        att_syntax: a.att,
    };
    Ok(disassemble(arch, &data[start..end], va, &opts)?)
}

#[derive(Debug, Args)]
pub struct PeChecksumArgs {
    /// PE image.
    pub file: PathBuf,
    /// Write the image with the recomputed checksum.
    #[arg(long)]
    pub fix: bool,
    /// Output for --fix.
    #[arg(short, long, requires = "fix")]
    pub output: Option<PathBuf>,
    /// With --fix: overwrite the input file.
    #[arg(long, requires = "fix", conflicts_with = "output")]
    pub in_place: bool,
}

pub fn run_pe_checksum(a: &PeChecksumArgs, json_out: bool) -> CliResult {
    let mut data = load(&a.file)?;
    let c = hexa_re::checksum::image_checksum(&data);
    if a.fix {
        let new = hexa_re::checksum::fix_pe_checksum(&mut data)?;
        let out = if a.in_place {
            a.file.clone()
        } else {
            a.output
                .clone()
                .ok_or_else(|| CliError::usage("--fix needs --output or --in-place"))?
        };
        io::write_bytes_to(&data, Some(&out))?;
        if json_out {
            io::print_json(&json!({ "before": c, "written": new }))?;
        } else {
            io::print(&format!(
                "checksum 0x{new:08X} written to {}\n",
                out.display()
            ))?;
        }
        return Ok(Outcome::Ok);
    }
    if json_out {
        let mut v = serde_json::to_value(&c)?;
        v["valid"] = json!(c.is_valid());
        io::print_json(&v)?;
    } else {
        let hex = |v: Option<u32>| v.map_or_else(|| "-".to_owned(), |v| format!("0x{v:08X}"));
        let mut s = format!(
            "stored {}  computed {}  {}\n",
            hex(c.stored),
            hex(c.computed),
            if c.is_valid() { "OK" } else { "MISMATCH" }
        );
        if !c.note.is_empty() {
            let _ = writeln!(s, "{}", c.note);
        }
        io::print(&s)?;
    }
    Ok(Outcome::from_ok(c.is_valid()))
}

#[derive(Debug, Args)]
pub struct ExePatchArgs {
    /// File to patch.
    pub file: PathBuf,
    /// Patch list (JSON: `{"patches":[{"offset":…,"original":"…","replacement":"…"}]}`).
    pub patches: PathBuf,
    /// Only report the state of every patch; exit 1 unless every patch is
    /// either applicable or already applied.
    #[arg(long, conflicts_with_all = ["revert", "force"])]
    pub check: bool,
    /// Restore the original bytes instead of applying.
    #[arg(long)]
    pub revert: bool,
    /// Apply even where the original bytes do not match.
    #[arg(long)]
    pub force: bool,
    /// Output file (default: stdout).
    #[arg(short, long)]
    pub output: Option<PathBuf>,
}

pub fn run_exe_patch(a: &ExePatchArgs, json_out: bool) -> CliResult {
    let list = PatchList::from_json(&read_text(&a.patches)?)?;
    let tree = open_tree(&a.file)?;
    if a.check {
        let states = list.states_tree(&tree)?;
        let ok = states
            .iter()
            .all(|s| matches!(s, PatchState::Original | PatchState::Applied));
        if json_out {
            let rows: Vec<Value> = list
                .patches
                .iter()
                .zip(&states)
                .map(|(p, s)| json!({"offset": p.offset, "len": p.original.len(), "state": name_of(s), "comment": p.comment}))
                .collect();
            io::print_json(&json!({ "ok": ok, "patches": rows }))?;
        } else {
            let mut s = String::new();
            for (p, st) in list.patches.iter().zip(&states) {
                let _ = writeln!(
                    s,
                    "0x{:08X} {:>6}  {:<10} {}",
                    p.offset,
                    p.original.len(),
                    name_of(st),
                    p.comment
                );
            }
            io::print(&s)?;
        }
        return Ok(Outcome::from_ok(ok));
    }
    let to_stdout = a.output.as_deref().is_none_or(io::is_std);
    if to_stdout && json_out {
        return Err(CliError::usage(
            "--json needs --output for the patched data",
        ));
    }
    if a.revert {
        let mut data = io::tree_bytes(&tree, 0, tree.len(), MAX_EXE)?;
        list.revert(&mut data)?;
        io::write_bytes_to(&data, a.output.as_deref())?;
    } else {
        let out = list.apply_to_tree(&tree, a.force)?;
        io::write_tree_to(&out, a.output.as_deref())?;
    }
    if json_out {
        io::print_json(&json!({ "patches": list.patches.len(), "reverted": a.revert }))?;
    }
    Ok(Outcome::Ok)
}
