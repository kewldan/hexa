//! Writing a file the user has no permission for, through the OS's own
//! elevation prompt (FILE-05). Only a single copy runs elevated.

use std::path::Path;
use std::process::Command;

use crate::error::{CmdError, CmdResult};

#[cfg(target_os = "macos")]
fn command(src: &Path, dst: &Path) -> Command {
    fn quote(p: &Path) -> String {
        // Single-quote for the shell, then escape for the AppleScript string.
        let shell = format!("'{}'", p.to_string_lossy().replace('\'', r"'\''"));
        shell.replace('\\', r"\\").replace('"', "\\\"")
    }
    let script = format!(
        "do shell script \"/bin/cp -f {} {}\" with administrator privileges",
        quote(src),
        quote(dst)
    );
    let mut c = Command::new("/usr/bin/osascript");
    c.arg("-e").arg(script);
    c
}

#[cfg(all(unix, not(target_os = "macos")))]
fn command(src: &Path, dst: &Path) -> Command {
    let mut c = Command::new("pkexec");
    c.arg("/bin/cp").arg("-f").arg(src).arg(dst);
    c
}

#[cfg(windows)]
fn command(src: &Path, dst: &Path) -> Command {
    fn quote(p: &Path) -> String {
        p.to_string_lossy().replace('\'', "''")
    }
    let script = format!(
        "Start-Process -FilePath powershell -Verb RunAs -Wait -WindowStyle Hidden -ArgumentList \
         '-NoProfile','-Command','Copy-Item -LiteralPath ''{}'' -Destination ''{}'' -Force'",
        quote(src),
        quote(dst)
    );
    let mut c = Command::new("powershell");
    c.args(["-NoProfile", "-Command", &script]);
    c
}

pub fn copy_privileged(src: &Path, dst: &Path) -> CmdResult<()> {
    let out = command(src, dst).output().map_err(|e| {
        CmdError::new(
            "permission",
            format!("cannot start the elevation helper: {e}"),
        )
    })?;
    if out.status.success() {
        Ok(())
    } else {
        Err(CmdError::new(
            "permission",
            format!(
                "elevated write failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        ))
    }
}
