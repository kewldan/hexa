//! Input / output helpers shared by the commands.
//!
//! Files are opened lazily through [`Document::open`] and handled as
//! [`Tree`] snapshots, so commands that stream (hash, search, diff, dumps …)
//! never read a whole file into memory. `-` names stdin (read into memory)
//! or stdout.

use std::fs::File;
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use clap::Args;
use hexa_core::{Document, EditMode, OpenOptions, Tree};
use serde::Serialize;

use crate::error::{CliError, CliResult};

/// Chunk size for streaming copies.
const CHUNK: usize = 1 << 20;

/// `true` when `path` means stdin / stdout.
pub fn is_std(path: &Path) -> bool {
    path.as_os_str() == "-"
}

/// Opens a file (or `-` for stdin) as a read-only snapshot.
pub fn open_tree(path: &Path) -> CliResult<Tree> {
    if is_std(path) {
        let mut data = Vec::new();
        io::stdin().read_to_end(&mut data)?;
        return Ok(Document::from_bytes("stdin", data).snapshot());
    }
    let opts = OpenOptions {
        mode: EditMode::ReadOnly,
        ..OpenOptions::default()
    };
    let doc = Document::open(path, &opts).map_err(|e| match e {
        hexa_core::Error::Invalid(m) => CliError::io(m),
        e => CliError::from(e).context(path.display()),
    })?;
    Ok(doc.snapshot())
}

/// Reads a whole file (or stdin) into memory.
pub fn read_bytes(path: &Path) -> CliResult<Vec<u8>> {
    if is_std(path) {
        let mut data = Vec::new();
        io::stdin().read_to_end(&mut data)?;
        return Ok(data);
    }
    std::fs::read(path).map_err(|e| CliError::from(e).context(path.display()))
}

/// Reads a whole text file (or stdin).
pub fn read_text(path: &Path) -> CliResult<String> {
    let bytes = read_bytes(path)?;
    String::from_utf8(bytes)
        .map_err(|_| CliError::data(format!("{}: not valid UTF-8 text", path.display())))
}

/// Reads `[start, end)` of a snapshot into memory, refusing more than
/// `limit` bytes.
pub fn tree_bytes(tree: &Tree, start: u64, end: u64, limit: u64) -> CliResult<Vec<u8>> {
    let len = end.saturating_sub(start);
    if len > limit {
        return Err(CliError::data(format!(
            "range of {len} bytes is too large for this operation (limit {limit})"
        )));
    }
    Ok(tree.read_vec(start, len as usize)?)
}

/// Where command output goes: a file (written atomically) or stdout.
pub struct Sink {
    target: Option<PathBuf>,
    tmp: Option<PathBuf>,
    w: BufWriter<Box<dyn Write>>,
}

impl std::fmt::Debug for Sink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sink")
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

impl Sink {
    /// `None` or `-` = stdout. Files are written to a temporary sibling and
    /// renamed on [`Sink::finish`], so an output may also be an input.
    pub fn create(path: Option<&Path>) -> CliResult<Self> {
        match path.filter(|p| !is_std(p)) {
            None => Ok(Self {
                target: None,
                tmp: None,
                w: BufWriter::new(Box::new(io::stdout())),
            }),
            Some(p) => {
                let name = p
                    .file_name()
                    .map_or_else(|| "out".into(), |n| n.to_string_lossy().into_owned());
                let tmp = p.with_file_name(format!(".{name}.hexa-{}.tmp", std::process::id()));
                let f = File::create(&tmp).map_err(|e| CliError::from(e).context(p.display()))?;
                Ok(Self {
                    target: Some(p.to_path_buf()),
                    tmp: Some(tmp),
                    w: BufWriter::new(Box::new(f)),
                })
            }
        }
    }

    pub fn writer(&mut self) -> &mut dyn Write {
        &mut self.w
    }

    pub fn write_all(&mut self, data: &[u8]) -> CliResult<()> {
        self.w.write_all(data).map_err(io_err)
    }

    /// Streams `[start, end)` of a snapshot.
    pub fn write_tree(&mut self, tree: &Tree, start: u64, end: u64) -> CliResult<()> {
        let mut buf = vec![0u8; CHUNK];
        let mut at = start;
        while at < end {
            let n = (end - at).min(CHUNK as u64) as usize;
            tree.read(at, &mut buf[..n])?;
            self.w.write_all(&buf[..n]).map_err(io_err)?;
            at += n as u64;
        }
        Ok(())
    }

    /// Flushes and moves the temporary file into place.
    pub fn finish(mut self) -> CliResult<()> {
        match self.w.flush() {
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe && self.target.is_none() => {}
            r => r.map_err(io_err)?,
        }
        if let (Some(tmp), Some(target)) = (self.tmp.take(), self.target.as_ref()) {
            drop(std::mem::replace(
                &mut self.w,
                BufWriter::new(Box::new(io::sink())),
            ));
            std::fs::rename(&tmp, target)
                .map_err(|e| CliError::from(e).context(target.display()))?;
        }
        Ok(())
    }
}

impl Drop for Sink {
    fn drop(&mut self) {
        if let Some(tmp) = self.tmp.take() {
            let _ = std::fs::remove_file(tmp);
        }
    }
}

fn io_err(e: io::Error) -> CliError {
    CliError::from(e)
}

/// Writes a snapshot to `path` (or stdout).
pub fn write_tree_to(tree: &Tree, path: Option<&Path>) -> CliResult<()> {
    let mut s = Sink::create(path)?;
    s.write_tree(tree, 0, tree.len())?;
    s.finish()
}

/// Writes bytes to `path` (or stdout).
pub fn write_bytes_to(data: &[u8], path: Option<&Path>) -> CliResult<()> {
    let mut s = Sink::create(path)?;
    s.write_all(data)?;
    s.finish()
}

/// Prints text to stdout; a closed pipe (`| head`) is not an error.
pub fn print(text: &str) -> CliResult<()> {
    let mut out = io::stdout().lock();
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        Err(e) if e.kind() != io::ErrorKind::BrokenPipe => Err(e.into()),
        _ => Ok(()),
    }
}

/// Prints a value as pretty JSON followed by a newline.
pub fn print_json(value: &impl Serialize) -> CliResult<()> {
    let mut s = serde_json::to_string_pretty(value)?;
    s.push('\n');
    print(&s)
}

/// Parses `123`, `0x7B`, `0o173`, `0b1111011` (underscores allowed).
pub fn parse_u64(s: &str) -> Result<u64, String> {
    let t = s.trim().replace('_', "");
    let (digits, radix) = if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        (h, 16)
    } else if let Some(b) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        (b, 2)
    } else if let Some(o) = t.strip_prefix("0o").or_else(|| t.strip_prefix("0O")) {
        (o, 8)
    } else {
        (t.as_str(), 10)
    };
    u64::from_str_radix(digits, radix).map_err(|_| format!("`{s}` is not a valid number"))
}

/// Parses a byte value (`0xFF`, `255`).
pub fn parse_u8(s: &str) -> Result<u8, String> {
    let v = parse_u64(s)?;
    u8::try_from(v).map_err(|_| format!("`{s}` does not fit in a byte"))
}

/// A byte range of the input.
#[derive(Debug, Clone, Copy, Default, Args)]
pub struct RangeArgs {
    /// Start offset (decimal or 0x hex).
    #[arg(long, value_parser = parse_u64, value_name = "OFFSET")]
    pub start: Option<u64>,
    /// End offset, exclusive (default: end of file).
    #[arg(long, value_parser = parse_u64, value_name = "OFFSET", conflicts_with = "length")]
    pub end: Option<u64>,
    /// Length of the range instead of --end.
    #[arg(long, value_parser = parse_u64, value_name = "BYTES")]
    pub length: Option<u64>,
}

impl RangeArgs {
    /// Resolves the range against a file length.
    pub fn resolve(&self, len: u64) -> CliResult<(u64, u64)> {
        let start = self.start.unwrap_or(0);
        let end = match (self.end, self.length) {
            (Some(e), _) => e,
            (None, Some(l)) => start.saturating_add(l),
            (None, None) => len,
        };
        if start > end || end > len {
            return Err(CliError::usage(format!(
                "range 0x{start:X}..0x{end:X} is outside of the input (length {len})"
            )));
        }
        Ok((start, end))
    }
}

/// Formats a byte count as `1234 (1.2 KiB)`.
pub fn human_size(n: u64) -> String {
    const UNITS: [&str; 5] = ["KiB", "MiB", "GiB", "TiB", "PiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64 / 1024.0;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    format!("{n} B ({v:.1} {})", UNITS[u])
}

/// Lower-case hex of bytes.
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// A progress sink that ignores updates.
pub fn no_progress(_: u64, _: u64) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(parse_u64("0x10"), Ok(16));
        assert_eq!(parse_u64("1_000"), Ok(1000));
        assert_eq!(parse_u64("0b101"), Ok(5));
        assert_eq!(parse_u64("0o17"), Ok(15));
        assert!(parse_u64("zz").is_err());
        assert!(parse_u8("256").is_err());
    }

    #[test]
    fn ranges() {
        let r = RangeArgs {
            start: Some(2),
            end: None,
            length: Some(3),
        };
        assert_eq!(r.resolve(10).unwrap(), (2, 5));
        assert!(r.resolve(4).is_err());
        assert_eq!(RangeArgs::default().resolve(7).unwrap(), (0, 7));
    }
}
