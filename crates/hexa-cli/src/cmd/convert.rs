//! `convert`: binary ↔ firmware text formats ↔ text dumps.

use std::path::{Path, PathBuf};

use clap::{Args, ValueEnum};
use hexa_analysis::dump::{DumpFormat, DumpTextFormat, detect_dump_format, dump, dump_range};
use hexa_analysis::firmware::{
    self, ExportOptions, FirmwareFormat, LineError, MAX_FLAT_LEN, Segment, StartAddress,
};
use hexa_core::{Cancel, Tree};
use serde_json::json;

use super::name_of;
use crate::error::{CliError, CliResult, Outcome};
use crate::io::{self, Sink, no_progress, open_tree, parse_u8, parse_u64, read_text};

/// Bytes sniffed to detect a text input format.
const SNIFF: u64 = 64 << 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    /// Raw binary.
    Bin,
    /// Intel HEX.
    #[value(alias = "hex", alias = "intel-hex")]
    Ihex,
    /// Motorola S-Record.
    #[value(alias = "s19", alias = "mot")]
    Srec,
    /// TI-TXT (MSP430).
    #[value(alias = "ti-txt")]
    Titxt,
    /// Tektronix extended hex.
    Tek,
    /// `xxd` dump.
    Xxd,
    /// `hexdump -C` dump.
    #[value(alias = "hexdump-c")]
    Hexdump,
    /// `od -A x -t x1z` dump (input only).
    Od,
    /// Plain hex digits (`xxd -p`).
    Plain,
    /// C header with a byte array (output only).
    #[value(alias = "c-array")]
    C,
    /// Markdown table (output only).
    Markdown,
    /// CSV rows (output only).
    Csv,
    /// Coloured HTML (output only).
    Html,
    /// Coloured RTF (output only).
    Rtf,
}

impl Format {
    fn firmware(self) -> Option<FirmwareFormat> {
        Some(match self {
            Self::Ihex => FirmwareFormat::IntelHex,
            Self::Srec => FirmwareFormat::SRecord,
            Self::Titxt => FirmwareFormat::TiTxt,
            Self::Tek => FirmwareFormat::TekHex,
            _ => return None,
        })
    }

    fn dump_text(self) -> Option<DumpTextFormat> {
        Some(match self {
            Self::Xxd => DumpTextFormat::Xxd,
            Self::Hexdump => DumpTextFormat::HexdumpC,
            Self::Od => DumpTextFormat::Od,
            Self::Plain => DumpTextFormat::PlainHex,
            _ => return None,
        })
    }

    fn from_ext(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "bin" | "img" | "rom" | "raw" => Self::Bin,
            "hex" | "ihex" | "ihx" | "h86" => Self::Ihex,
            "srec" | "s19" | "s28" | "s37" | "mot" | "s" => Self::Srec,
            "tek" => Self::Tek,
            "xxd" => Self::Xxd,
            "h" | "c" => Self::C,
            "md" => Self::Markdown,
            "csv" => Self::Csv,
            "html" | "htm" => Self::Html,
            "rtf" => Self::Rtf,
            _ => return None,
        })
    }
}

#[derive(Debug, Args)]
pub struct ConvertArgs {
    /// Input file (`-` = stdin).
    pub input: PathBuf,
    /// Output file (default: stdout).
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Input format (default: from the extension or the content).
    #[arg(long, value_enum)]
    pub from: Option<Format>,
    /// Output format (default: from the output extension).
    #[arg(long, value_enum)]
    pub to: Option<Format>,
    /// Address of the first byte of a binary input.
    #[arg(long, value_parser = parse_u64, default_value = "0")]
    pub base: u64,
    /// Byte filling gaps when a sparse image becomes binary.
    #[arg(long, value_parser = parse_u8, default_value = "0xFF")]
    pub fill: u8,
    /// Data bytes per record / line for firmware and plain-hex output.
    #[arg(long, default_value_t = 16)]
    pub record_size: usize,
    /// Execution start address written to firmware output.
    #[arg(long, value_parser = parse_u64)]
    pub start_address: Option<u64>,
    /// Array name for C output (default: from the input name).
    #[arg(long)]
    pub name: Option<String>,
    /// Convert even when some input lines are malformed (they are skipped).
    #[arg(long)]
    pub ignore_errors: bool,
}

enum Input {
    Tree(Tree),
    Segments {
        segments: Vec<Segment>,
        start: Option<StartAddress>,
        header: Option<String>,
    },
}

fn report_lines(kind: &str, lines: &[LineError]) {
    for e in lines.iter().take(20) {
        eprintln!("hexa-cli: {kind} line {}: {}", e.line, e.message);
    }
    if lines.len() > 20 {
        eprintln!("hexa-cli: … {} more", lines.len() - 20);
    }
}

fn sniff(path: &Path) -> CliResult<Format> {
    if io::is_std(path) {
        return Ok(Format::Bin);
    }
    let tree = open_tree(path)?;
    let head = tree.read_vec(0, tree.len().min(SNIFF) as usize)?;
    let text = match std::str::from_utf8(&head) {
        Ok(t) => t,
        // A cut multi-byte character at the end of the sniffed head.
        Err(e) if e.error_len().is_none() => {
            std::str::from_utf8(&head[..e.valid_up_to()]).unwrap_or_default()
        }
        Err(_) => return Ok(Format::Bin),
    };
    if let Some(f) = firmware::detect_format(text) {
        return Ok(match f {
            FirmwareFormat::IntelHex => Format::Ihex,
            FirmwareFormat::SRecord => Format::Srec,
            FirmwareFormat::TiTxt => Format::Titxt,
            FirmwareFormat::TekHex => Format::Tek,
        });
    }
    Ok(match detect_dump_format(text) {
        Some(DumpTextFormat::Xxd) => Format::Xxd,
        Some(DumpTextFormat::HexdumpC) => Format::Hexdump,
        Some(DumpTextFormat::Od) => Format::Od,
        Some(DumpTextFormat::PlainHex) => Format::Plain,
        None => Format::Bin,
    })
}

fn load(a: &ConvertArgs, from: Format) -> CliResult<Input> {
    if let Some(f) = from.firmware() {
        let text = read_text(&a.input)?;
        let img = firmware::import(&text, Some(f))?;
        if !img.errors.is_empty() {
            report_lines(
                if a.ignore_errors {
                    "warning:"
                } else {
                    "error:"
                },
                &img.errors,
            );
            if !a.ignore_errors {
                return Err(CliError::data(format!(
                    "{}: {} malformed record(s) (use --ignore-errors to skip them)",
                    a.input.display(),
                    img.errors.len()
                )));
            }
        }
        return Ok(Input::Segments {
            segments: img.segments,
            start: img.start,
            header: img.header,
        });
    }
    if let Some(f) = from.dump_text() {
        let text = read_text(&a.input)?;
        let imp = hexa_analysis::dump::import_dump(&text, Some(f))?;
        report_lines("warning:", &imp.warnings);
        if !imp.errors.is_empty() {
            report_lines(
                if a.ignore_errors {
                    "warning:"
                } else {
                    "error:"
                },
                &imp.errors,
            );
            if !a.ignore_errors {
                return Err(CliError::data(format!(
                    "{}: {} malformed line(s) (use --ignore-errors to skip them)",
                    a.input.display(),
                    imp.errors.len()
                )));
            }
        }
        return Ok(Input::Segments {
            segments: imp.segments,
            start: None,
            header: None,
        });
    }
    if from != Format::Bin {
        return Err(CliError::usage(format!(
            "{} is an output-only format",
            name_of_format(from)
        )));
    }
    Ok(Input::Tree(open_tree(&a.input)?))
}

fn name_of_format(f: Format) -> String {
    f.to_possible_value()
        .map_or_else(String::new, |v| v.get_name().to_owned())
}

/// Flattens sparse segments into `(address, bytes)`.
fn flatten(segments: &[Segment], fill: u8) -> CliResult<(u64, Vec<u8>)> {
    let flat = firmware::flatten(segments, fill, None, None, MAX_FLAT_LEN)?;
    Ok((flat.address, flat.data))
}

fn write_plain(sink: &mut Sink, bytes: &[u8], per_line: usize) -> CliResult<()> {
    let mut s = String::new();
    for line in bytes.chunks(per_line.max(1)) {
        s.push_str(&io::hex(line));
        s.push('\n');
        if s.len() > 1 << 20 {
            sink.write_all(s.as_bytes())?;
            s.clear();
        }
    }
    sink.write_all(s.as_bytes())
}

pub fn run(a: &ConvertArgs, json_out: bool) -> CliResult {
    let from = match a.from {
        Some(f) => f,
        None => match Format::from_ext(&a.input) {
            Some(f) if f.firmware().is_some() || f == Format::Bin || f == Format::Xxd => f,
            _ => sniff(&a.input)?,
        },
    };
    let to =
        a.to.or_else(|| a.output.as_deref().and_then(Format::from_ext))
            .ok_or_else(|| CliError::usage("cannot tell the output format; use --to"))?;
    if to == Format::Od {
        return Err(CliError::usage("od is an input-only format"));
    }
    let to_stdout = a.output.as_deref().is_none_or(io::is_std);
    if to_stdout && json_out {
        return Err(CliError::usage(
            "--json needs --output for the converted data",
        ));
    }
    let input = load(a, from)?;
    let mut sink = Sink::create(a.output.as_deref())?;
    let mut base = a.base;
    let written_len;

    if let Some(ff) = to.firmware() {
        let (segments, start, header) = match input {
            Input::Tree(t) => {
                let data = io::tree_bytes(&t, 0, t.len(), MAX_FLAT_LEN)?;
                (
                    vec![Segment {
                        address: a.base,
                        data,
                    }],
                    None,
                    None,
                )
            }
            Input::Segments {
                segments,
                start,
                header,
            } => (segments, start, header),
        };
        written_len = segments.iter().map(|s| s.data.len() as u64).sum();
        let opts = ExportOptions {
            record_size: a.record_size,
            start: a
                .start_address
                .map(|address| StartAddress::Linear { address })
                .or(start),
            header,
            ..ExportOptions::default()
        };
        let text = firmware::export(&segments, ff, &opts).map_err(|e| match e {
            hexa_core::Error::Invalid(m) => CliError::usage(m),
            e => e.into(),
        })?;
        sink.write_all(text.as_bytes())?;
    } else {
        let dump_format = match to {
            Format::Xxd => Some(DumpFormat::Xxd),
            Format::Hexdump => Some(DumpFormat::HexdumpC { squeeze: false }),
            Format::Markdown => Some(DumpFormat::Markdown { width: 16 }),
            Format::Csv => Some(DumpFormat::Csv { width: 16 }),
            Format::Html => Some(DumpFormat::Html),
            Format::Rtf => Some(DumpFormat::Rtf),
            Format::C => Some(DumpFormat::CHeader {
                name: a.name.clone().unwrap_or_else(|| {
                    a.input
                        .file_stem()
                        .map_or_else(|| "data".into(), |s| s.to_string_lossy().into_owned())
                }),
            }),
            _ => None,
        };
        match input {
            Input::Tree(t) => {
                written_len = t.len();
                match (to, dump_format) {
                    (Format::Bin, _) => sink.write_tree(&t, 0, t.len())?,
                    (Format::Plain, _) => {
                        let data = io::tree_bytes(&t, 0, t.len(), MAX_FLAT_LEN)?;
                        write_plain(&mut sink, &data, a.record_size)?;
                    }
                    (_, Some(df)) if a.base == 0 => dump_range(
                        &t,
                        0,
                        t.len(),
                        &df,
                        sink.writer(),
                        &Cancel::new(),
                        &no_progress,
                    )?,
                    (_, Some(df)) => {
                        let data = io::tree_bytes(&t, 0, t.len(), MAX_FLAT_LEN)?;
                        sink.write_all(dump(&data, a.base, &df)?.as_bytes())?;
                    }
                    (_, None) => unreachable!("every non-firmware output is handled"),
                }
            }
            Input::Segments { segments, .. } => {
                let (addr, data) = flatten(&segments, a.fill)?;
                base = addr;
                written_len = data.len() as u64;
                match (to, dump_format) {
                    (Format::Bin, _) => sink.write_all(&data)?,
                    (Format::Plain, _) => write_plain(&mut sink, &data, a.record_size)?,
                    (_, Some(df)) => sink.write_all(dump(&data, addr, &df)?.as_bytes())?,
                    (_, None) => unreachable!("every non-firmware output is handled"),
                }
                if to == Format::Bin && addr != 0 && !json_out {
                    eprintln!("hexa-cli: binary output starts at address 0x{addr:X}");
                }
            }
        }
    }
    sink.finish()?;
    if json_out {
        io::print_json(&json!({
            "from": name_of_format(from),
            "to": name_of_format(to),
            "bytes": written_len,
            "baseAddress": base,
            "output": a.output.as_ref().map(|p| p.display().to_string()),
            "firmware": to.firmware().map(|f| name_of(&f)),
        }))?;
    }
    Ok(Outcome::Ok)
}
