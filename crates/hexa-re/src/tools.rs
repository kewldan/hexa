//! External tool integration helpers (RE-09).
//!
//! Launching tools is the application's job; this module writes the
//! selection to a temporary file and describes known tools as command
//! templates. Placeholders in templates:
//!
//! | Placeholder | Meaning |
//! |-------------|---------|
//! | `{file}` | path of the temporary file |
//! | `{dir}` | directory containing it |
//! | `{name}` | file name without directory |
//! | `{base64}` | selection encoded as standard base64 |
//! | `{base64url}` | selection encoded as URL-safe base64 without padding |
//! | `{tool}` | user-configured install path of the tool |

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::error::{ReError, Result};

/// How a tool receives the selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolLaunch {
    /// Run a command line (program + arguments).
    Command,
    /// Open a URL in the browser.
    Url,
}

/// A known external tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalTool {
    /// Stable id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Launch style.
    pub launch: ToolLaunch,
    /// Program (with placeholders) for [`ToolLaunch::Command`], URL
    /// template for [`ToolLaunch::Url`].
    pub program: String,
    /// Argument templates.
    pub args: Vec<String>,
    /// Suggested temp-file extension.
    pub extension: String,
    /// Short note shown in the UI.
    pub note: String,
}

fn tool(
    id: &str,
    name: &str,
    launch: ToolLaunch,
    program: &str,
    args: &[&str],
    extension: &str,
    note: &str,
) -> ExternalTool {
    ExternalTool {
        id: id.into(),
        name: name.into(),
        launch,
        program: program.into(),
        args: args.iter().map(|s| (*s).to_string()).collect(),
        extension: extension.into(),
        note: note.into(),
    }
}

/// Built-in tool templates (RE-09).
pub fn known_tools() -> Vec<ExternalTool> {
    use ToolLaunch::{Command, Url};
    vec![
        tool(
            "ghidra-headless",
            "Ghidra (headless analysis)",
            Command,
            "{tool}/support/analyzeHeadless",
            &[
                "{dir}",
                "hexa_{name}",
                "-import",
                "{file}",
                "-deleteProject",
            ],
            "bin",
            "Imports and auto-analyses the selection in a throw-away project; \
             use analyzeHeadless.bat on Windows.",
        ),
        tool(
            "ghidra",
            "Ghidra",
            Command,
            "{tool}/ghidraRun",
            &["{file}"],
            "bin",
            "Starts the Ghidra GUI with the selection.",
        ),
        tool(
            "ida",
            "IDA Pro",
            Command,
            "{tool}/ida",
            &["{file}"],
            "bin",
            "Opens the selection in IDA (ida64 on older versions).",
        ),
        tool(
            "ida-batch",
            "IDA Pro (batch)",
            Command,
            "{tool}/idat",
            &["-B", "{file}"],
            "bin",
            "Batch auto-analysis producing an .i64 database next to the file.",
        ),
        tool(
            "binaryninja",
            "Binary Ninja",
            Command,
            "{tool}/binaryninja",
            &["{file}"],
            "bin",
            "Opens the selection in Binary Ninja.",
        ),
        tool(
            "radare2",
            "radare2",
            Command,
            "r2",
            &["-A", "{file}"],
            "bin",
            "Analyses the selection in radare2.",
        ),
        tool(
            "cutter",
            "Cutter",
            Command,
            "cutter",
            &["{file}"],
            "bin",
            "Opens the selection in Cutter.",
        ),
        tool(
            "cyberchef",
            "CyberChef",
            Url,
            "https://gchq.github.io/CyberChef/#input={base64url}",
            &[],
            "bin",
            "Opens CyberChef with the selection as input (keep selections small: \
             the data travels in the URL fragment and is not sent to the server).",
        ),
        tool(
            "file",
            "file(1)",
            Command,
            "file",
            &["-b", "{file}"],
            "bin",
            "Identifies the selection with libmagic.",
        ),
    ]
}

fn sanitize_ext(ext: &str) -> String {
    let e: String = ext
        .trim_start_matches('.')
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(16)
        .collect();
    if e.is_empty() { "bin".into() } else { e }
}

/// Writes `bytes` to a new file in the system temp directory
/// (`hexa-selection-<pid>-<n>.<ext>`) and returns its path (RE-09).
pub fn write_selection_temp(bytes: &[u8], ext: &str) -> Result<PathBuf> {
    write_selection_in(&std::env::temp_dir(), bytes, ext)
}

/// [`write_selection_temp`] into a chosen directory.
pub fn write_selection_in(dir: &Path, bytes: &[u8], ext: &str) -> Result<PathBuf> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let ext = sanitize_ext(ext);
    for _ in 0..1000 {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!("hexa-selection-{}-{n}.{ext}", std::process::id()));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                f.write_all(bytes)?;
                f.flush()?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    Err(ReError::Invalid(
        "could not create a unique temp file".into(),
    ))
}

/// A tool invocation with every placeholder substituted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolInvocation {
    /// Launch style.
    pub launch: ToolLaunch,
    /// Program path or URL.
    pub program: String,
    /// Arguments.
    pub args: Vec<String>,
}

/// Substitutes placeholders for `tool` (`tool_dir` replaces `{tool}`).
pub fn render_invocation(
    tool: &ExternalTool,
    file: &Path,
    bytes: &[u8],
    tool_dir: &str,
) -> ToolInvocation {
    let dir = file
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let render = |t: &str| -> String {
        let mut s = t
            .replace("{file}", &file.display().to_string())
            .replace("{dir}", &dir)
            .replace("{name}", &name)
            .replace("{tool}", tool_dir.trim_end_matches(['/', '\\']));
        if s.contains("{base64url}") {
            s = s.replace(
                "{base64url}",
                &base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes),
            );
        }
        if s.contains("{base64}") {
            s = s.replace(
                "{base64}",
                &base64::engine::general_purpose::STANDARD.encode(bytes),
            );
        }
        s
    };
    ToolInvocation {
        launch: tool.launch,
        program: render(&tool.program),
        args: tool.args.iter().map(|a| render(a)).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_files_are_unique() {
        let dir = tempfile::tempdir().unwrap();
        let a = write_selection_in(dir.path(), b"abc", ".exe").unwrap();
        let b = write_selection_in(dir.path(), b"def", "../../evil").unwrap();
        assert_ne!(a, b);
        assert_eq!(std::fs::read(&a).unwrap(), b"abc");
        assert_eq!(a.extension().unwrap(), "exe");
        assert_eq!(b.extension().unwrap(), "evil");
        assert_eq!(b.parent().unwrap(), dir.path());
        let t = write_selection_temp(b"x", "").unwrap();
        assert_eq!(t.extension().unwrap(), "bin");
        std::fs::remove_file(t).unwrap();
    }

    #[test]
    fn templates_render() {
        let tools = known_tools();
        let chef = tools.iter().find(|t| t.id == "cyberchef").unwrap();
        let inv = render_invocation(chef, Path::new("/tmp/x.bin"), &[0xfb, 0xff], "");
        assert_eq!(inv.program, "https://gchq.github.io/CyberChef/#input=-_8");
        assert_eq!(inv.launch, ToolLaunch::Url);
        let gh = tools.iter().find(|t| t.id == "ghidra-headless").unwrap();
        let inv = render_invocation(gh, Path::new("/tmp/sel.bin"), b"", "/opt/ghidra/");
        assert_eq!(inv.program, "/opt/ghidra/support/analyzeHeadless");
        assert_eq!(
            inv.args,
            vec![
                "/tmp",
                "hexa_sel.bin",
                "-import",
                "/tmp/sel.bin",
                "-deleteProject"
            ]
        );
        assert!(tools.iter().any(|t| t.id == "ida"));
        let json = serde_json::to_string(&tools).unwrap();
        assert!(json.contains("\"launch\":\"url\""));
    }
}
