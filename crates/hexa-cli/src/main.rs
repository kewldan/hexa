//! Headless command-line interface of Hexa (SCR-06).
//!
//! `hexa-cli` exposes the engine without the GUI: file information, hashes,
//! search, strings, diff / three-way merge, binary patches (IPS, UPS, BPS,
//! VCDIFF, bsdiff), firmware and dump format conversion, transformation
//! recipes, executable analysis and disassembly, embedded-file carving,
//! structure templates rendered as JSON, disk / partition listing and the
//! privileged device helper used by the desktop app (NFR-10).
//!
//! Every command accepts the global `--json` flag for machine-readable
//! output on stdout. Diagnostics go to stderr. Files are opened lazily and
//! streamed, so multi-gigabyte inputs are fine for streaming commands.
//!
//! # Exit codes
//!
//! | Code | Meaning |
//! |------|---------|
//! | 0 | Success: no differences, pattern found, every check passed. |
//! | 1 | Negative answer: `diff` found differences; `search` / `embedded` found nothing; `partitions` found no table; `hash --expect`, `exe-patch --check`, `pe-checksum` or the `patch info` checksum failed; `merge3` had conflicts. |
//! | 2 | Usage error: invalid command line, option value, pattern or algorithm. |
//! | 3 | I/O error: an input could not be read or an output written. |
//! | 4 | Data error: malformed input (bad patch, firmware file, recipe, template …). |

mod cmd;
mod error;
mod io;

use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand};

use crate::error::{CliError, EXIT_USAGE};

const EXIT_CODES_HELP: &str = "\
Exit codes:
  0  success: no differences, pattern found, all checks passed
  1  negative answer: differences found (diff), nothing found (search,
     embedded, partitions), check failed (hash --expect, exe-patch --check,
     pe-checksum, patch info checksum), conflicts (merge3)
  2  usage error: invalid arguments, pattern or algorithm
  3  I/O error: cannot read an input or write an output
  4  data error: malformed input data or format";

/// Hexa hex editor engine on the command line.
#[derive(Debug, Parser)]
#[command(name = "hexa-cli", version, about, after_long_help = EXIT_CODES_HELP)]
struct Cli {
    /// Print machine-readable JSON on stdout.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Size, entropy, detected type and executable summary of a file.
    Info(cmd::info::InfoArgs),
    /// Checksums, CRCs, cryptographic, fast and fuzzy hashes.
    Hash(cmd::hash::HashArgs),
    /// Find hex / text / regex / numeric-sequence patterns (exit 1 if none).
    Search(cmd::search::SearchArgs),
    /// Extract ASCII / UTF-8 / UTF-16 strings.
    Strings(cmd::strings::StringsArgs),
    /// Compare two files (exit 1 if they differ).
    Diff(cmd::diff::DiffArgs),
    /// Three-way merge of binary files (exit 1 on conflicts).
    Merge3(cmd::diff::Merge3Args),
    /// Create, apply or inspect IPS / UPS / BPS / VCDIFF / bsdiff patches.
    #[command(subcommand)]
    Patch(cmd::diff::PatchCommand),
    /// Convert between binary, Intel HEX, S-Record, TI-TXT, Tektronix,
    /// xxd / hexdump / od / plain hex dumps and C arrays.
    Convert(cmd::convert::ConvertArgs),
    /// Apply a transformation recipe (JSON) to a file.
    Transform(cmd::transform::TransformArgs),
    /// Executable headers, sections, imports, exports, detection and
    /// disassembly (PE, ELF, Mach-O).
    Exe(cmd::exe::ExeArgs),
    /// Verify or fix the PE image checksum (exit 1 if wrong).
    PeChecksum(cmd::exe::PeChecksumArgs),
    /// Check, apply or revert a JSON byte patch list (RE-05).
    ExePatch(cmd::exe::ExePatchArgs),
    /// Find (and optionally carve out) embedded files.
    #[command(visible_alias = "carve")]
    Embedded(cmd::embedded::EmbeddedArgs),
    /// Apply a structure template and print the result as JSON.
    Template(cmd::template::TemplateArgs),
    /// List disks and volumes (read-only).
    Disks,
    /// Show the partition table of a disk, device or disk image.
    Partitions(cmd::sys::PartitionsArgs),
    /// Serve the privileged device helper protocol on stdin/stdout.
    #[command(hide = true)]
    Helper(cmd::sys::HelperArgs),
    /// Generate shell completions.
    Completions {
        /// Target shell.
        shell: clap_complete::Shell,
    },
}

fn run(cli: Cli) -> Result<error::Outcome, CliError> {
    let json = cli.json;
    match cli.command {
        Command::Info(a) => cmd::info::run(&a, json),
        Command::Hash(a) => cmd::hash::run(&a, json),
        Command::Search(a) => cmd::search::run(&a, json),
        Command::Strings(a) => cmd::strings::run(&a, json),
        Command::Diff(a) => cmd::diff::run_diff(&a, json),
        Command::Merge3(a) => cmd::diff::run_merge3(&a, json),
        Command::Patch(c) => cmd::diff::run_patch(&c, json),
        Command::Convert(a) => cmd::convert::run(&a, json),
        Command::Transform(a) => cmd::transform::run(&a, json),
        Command::Exe(a) => cmd::exe::run(&a, json),
        Command::PeChecksum(a) => cmd::exe::run_pe_checksum(&a, json),
        Command::ExePatch(a) => cmd::exe::run_exe_patch(&a, json),
        Command::Embedded(a) => cmd::embedded::run(&a, json),
        Command::Template(a) => cmd::template::run(&a, json),
        Command::Disks => cmd::sys::run_disks(json),
        Command::Partitions(a) => cmd::sys::run_partitions(&a, json),
        Command::Helper(a) => cmd::sys::run_helper(&a),
        Command::Completions { shell } => {
            let mut c = Cli::command();
            clap_complete::generate(shell, &mut c, "hexa-cli", &mut std::io::stdout());
            Ok(error::Outcome::Ok)
        }
    }
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            return if e.use_stderr() {
                ExitCode::from(EXIT_USAGE)
            } else {
                ExitCode::SUCCESS
            };
        }
    };
    match run(cli) {
        Ok(outcome) => outcome.exit_code(),
        Err(e) => {
            eprintln!("hexa-cli: {e}");
            ExitCode::from(e.code())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
