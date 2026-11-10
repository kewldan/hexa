//! Remote debugging helpers: LLDB and Android (PROC-09).
//!
//! Every remote target ends up as a GDB-protocol TCP endpoint that
//! [`GdbSource::connect`] opens.
//!
//! **LLDB** — `lldb-server gdbserver` speaks the GDB remote protocol
//! (plus `qMemoryRegionInfo`, which Hexa uses for the memory map):
//!
//! ```text
//! lldb-server gdbserver 127.0.0.1:5039 --attach <pid>
//! ```
//!
//! **Android** — the debug server runs on the device and adb forwards its
//! port to the host:
//!
//! 1. `adb [-s SERIAL] forward tcp:LOCAL tcp:REMOTE`
//! 2. start the server on the device, attached to the app:
//!    * debuggable app: `adb shell run-as <package> /data/local/tmp/lldb-server gdbserver 127.0.0.1:REMOTE --attach <pid>`
//!      (`gdbserver :REMOTE --attach <pid>` on older NDKs);
//!    * rooted device: `adb shell su -c '<server> … --attach <pid>'`;
//!    * the server binary comes from the NDK (`toolchains/llvm/prebuilt/*/lib/clang/*/lib/linux/<arch>/lldb-server`)
//!      and is pushed with `adb push lldb-server /data/local/tmp/`;
//! 3. connect to `127.0.0.1:LOCAL`;
//! 4. `adb forward --remove tcp:LOCAL` when done.
//!
//! [`AndroidTarget`] builds these command lines (so the UI can show them)
//! and can run them.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::gdb::{DEFAULT_TIMEOUT, GdbClient, GdbSource};

/// Which debug server to start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum DebugServer {
    /// `gdbserver :PORT --attach PID`.
    Gdbserver { path: String },
    /// `lldb-server gdbserver 127.0.0.1:PORT --attach PID`.
    LldbServer { path: String },
}

impl DebugServer {
    /// Arguments that start the server listening on `port`, attached to `pid`.
    pub fn args(&self, port: u16, pid: u32) -> Vec<String> {
        match self {
            Self::Gdbserver { path } => vec![
                path.clone(),
                format!(":{port}"),
                "--attach".into(),
                pid.to_string(),
            ],
            Self::LldbServer { path } => vec![
                path.clone(),
                "gdbserver".into(),
                format!("127.0.0.1:{port}"),
                "--attach".into(),
                pid.to_string(),
            ],
        }
    }
}

/// How the server is started on the device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RunAs {
    /// As the shell user (only works for the shell's own processes).
    Shell,
    /// `run-as <package>`: debuggable apps.
    Package { package: String },
    /// `su -c`: rooted devices.
    Root,
}

/// An `adb devices -l` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdbDevice {
    pub serial: String,
    /// `device`, `unauthorized`, `offline`, …
    pub state: String,
    pub model: Option<String>,
}

/// Parses `adb devices -l` output.
pub fn parse_adb_devices(output: &str) -> Vec<AdbDevice> {
    output
        .lines()
        .skip_while(|l| !l.starts_with("List of devices"))
        .skip(1)
        .filter_map(|line| {
            let mut it = line.split_whitespace();
            let serial = it.next()?.to_string();
            let state = it.next()?.to_string();
            let model = it
                .find_map(|kv| kv.strip_prefix("model:"))
                .map(str::to_string);
            Some(AdbDevice {
                serial,
                state,
                model,
            })
        })
        .collect()
}

/// Runs `adb` with `args`; returns stdout.
pub fn run_adb(adb: &Path, args: &[String]) -> Result<String> {
    let out = Command::new(adb).args(args).stdin(Stdio::null()).output()?;
    if !out.status.success() {
        return Err(Error::Os {
            operation: "adb",
            code: i64::from(out.status.code().unwrap_or(-1)),
            message: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Connected Android devices.
pub fn adb_devices(adb: &Path) -> Result<Vec<AdbDevice>> {
    Ok(parse_adb_devices(&run_adb(
        adb,
        &["devices".into(), "-l".into()],
    )?))
}

/// An Android device reached through adb (PROC-09).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AndroidTarget {
    /// Path of the `adb` executable (`"adb"` uses `PATH`).
    pub adb: PathBuf,
    /// Device serial; `None` = the only connected device.
    pub serial: Option<String>,
    /// Port on this machine.
    pub local_port: u16,
    /// Port the server listens on, on the device.
    pub remote_port: u16,
}

impl AndroidTarget {
    pub fn new(serial: Option<String>, port: u16) -> Self {
        Self {
            adb: PathBuf::from("adb"),
            serial,
            local_port: port,
            remote_port: port,
        }
    }

    fn base(&self) -> Vec<String> {
        match &self.serial {
            Some(s) => vec!["-s".into(), s.clone()],
            None => Vec::new(),
        }
    }

    /// `adb forward tcp:LOCAL tcp:REMOTE`.
    pub fn forward_args(&self) -> Vec<String> {
        let mut a = self.base();
        a.extend([
            "forward".into(),
            format!("tcp:{}", self.local_port),
            format!("tcp:{}", self.remote_port),
        ]);
        a
    }

    /// `adb forward --remove tcp:LOCAL`.
    pub fn remove_forward_args(&self) -> Vec<String> {
        let mut a = self.base();
        a.extend([
            "forward".into(),
            "--remove".into(),
            format!("tcp:{}", self.local_port),
        ]);
        a
    }

    /// `adb shell pidof <name>`.
    pub fn pidof_args(&self, process: &str) -> Vec<String> {
        let mut a = self.base();
        a.extend(["shell".into(), "pidof".into(), process.into()]);
        a
    }

    /// `adb shell [run-as PKG | su -c] <server> … --attach PID`.
    pub fn server_args(&self, server: &DebugServer, run_as: &RunAs, pid: u32) -> Vec<String> {
        let cmd = server.args(self.remote_port, pid);
        let mut a = self.base();
        a.push("shell".into());
        match run_as {
            RunAs::Shell => a.extend(cmd),
            RunAs::Package { package } => {
                a.extend(["run-as".into(), package.clone()]);
                a.extend(cmd);
            }
            RunAs::Root => {
                // adb joins its arguments with spaces for the device shell,
                // so the command must arrive as one quoted word.
                a.extend(["su".into(), "-c".into(), shell_join(&[shell_join(&cmd)])]);
            }
        }
        a
    }

    /// Full command lines of the whole flow, for display.
    pub fn describe(&self, server: &DebugServer, run_as: &RunAs, pid: u32) -> Vec<String> {
        let adb = self.adb.display().to_string();
        [
            self.forward_args(),
            self.server_args(server, run_as, pid),
            self.remove_forward_args(),
        ]
        .iter()
        .map(|args| format!("{adb} {}", shell_join(args)))
        .collect()
    }

    /// Runs `adb forward`.
    pub fn forward(&self) -> Result<()> {
        run_adb(&self.adb, &self.forward_args()).map(drop)
    }

    /// Runs `adb forward --remove`.
    pub fn remove_forward(&self) -> Result<()> {
        run_adb(&self.adb, &self.remove_forward_args()).map(drop)
    }

    /// PID of a process on the device by name or package.
    pub fn pidof(&self, process: &str) -> Result<Option<u32>> {
        let out = run_adb(&self.adb, &self.pidof_args(process))?;
        Ok(out.split_whitespace().next().and_then(|p| p.parse().ok()))
    }

    /// Starts the debug server on the device; it runs until the returned
    /// child (the `adb shell` session) is killed.
    pub fn spawn_server(&self, server: &DebugServer, run_as: &RunAs, pid: u32) -> Result<Child> {
        Ok(Command::new(&self.adb)
            .args(self.server_args(server, run_as, pid))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?)
    }

    /// Forwards the port, starts the server and connects, retrying the
    /// connection until `timeout` while the server starts up.
    pub fn attach(
        &self,
        server: &DebugServer,
        run_as: &RunAs,
        pid: u32,
        timeout: Duration,
    ) -> Result<(GdbSource, Child)> {
        self.forward()?;
        let mut child = self.spawn_server(server, run_as, pid)?;
        let deadline = Instant::now() + timeout;
        loop {
            match GdbClient::connect(("127.0.0.1", self.local_port), DEFAULT_TIMEOUT) {
                Ok(client) => return Ok((GdbSource::new(client)?, child)),
                Err(e) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    return Err(e);
                }
                Err(_) => {
                    if let Some(status) = child.try_wait()? {
                        return Err(Error::Os {
                            operation: "start debug server",
                            code: i64::from(status.code().unwrap_or(-1)),
                            message: "the debug server exited; is the app debuggable \
                                      (run-as) or the device rooted (su)?"
                                .into(),
                        });
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }
    }
}

/// Arguments for a local `lldb-server gdbserver` attached to `pid`.
pub fn lldb_server_args(lldb_server: &str, port: u16, pid: u32) -> Vec<String> {
    DebugServer::LldbServer {
        path: lldb_server.into(),
    }
    .args(port, pid)
}

/// Arguments for a local `gdbserver` attached to `pid`.
pub fn gdbserver_args(gdbserver: &str, port: u16, pid: u32) -> Vec<String> {
    DebugServer::Gdbserver {
        path: gdbserver.into(),
    }
    .args(port, pid)
}

/// Quotes arguments for a POSIX shell.
pub fn shell_join(args: &[String]) -> String {
    args.iter()
        .map(|a| {
            if !a.is_empty()
                && a.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_./:=@%+,".contains(&b))
            {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', r"'\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_adb_commands() {
        let t = AndroidTarget::new(Some("emulator-5554".into()), 5039);
        assert_eq!(
            t.forward_args(),
            ["-s", "emulator-5554", "forward", "tcp:5039", "tcp:5039"]
        );
        let lldb = DebugServer::LldbServer {
            path: "/data/local/tmp/lldb-server".into(),
        };
        let args = t.server_args(
            &lldb,
            &RunAs::Package {
                package: "com.example.game".into(),
            },
            1234,
        );
        assert_eq!(
            args,
            [
                "-s",
                "emulator-5554",
                "shell",
                "run-as",
                "com.example.game",
                "/data/local/tmp/lldb-server",
                "gdbserver",
                "127.0.0.1:5039",
                "--attach",
                "1234"
            ]
        );
        let root = t.server_args(
            &DebugServer::Gdbserver {
                path: "gdbserver".into(),
            },
            &RunAs::Root,
            7,
        );
        assert_eq!(root.last().unwrap(), "'gdbserver :5039 --attach 7'");
        let flow = t.describe(&lldb, &RunAs::Shell, 1);
        assert_eq!(flow.len(), 3);
        assert!(flow[2].ends_with("forward --remove tcp:5039"));
        assert_eq!(
            gdbserver_args("gdbserver", 1, 2),
            ["gdbserver", ":1", "--attach", "2"]
        );
        assert_eq!(lldb_server_args("lldb-server", 1, 2)[1], "gdbserver");
        assert_eq!(
            shell_join(&["a b".into(), "it's".into()]),
            r"'a b' 'it'\''s'"
        );
    }

    #[test]
    fn parses_devices() {
        let out = "* daemon started successfully\nList of devices attached\n\
                   emulator-5554          device product:sdk model:Pixel_7 device:emu\n\
                   R58M123                unauthorized usb:1-1\n\n";
        let d = parse_adb_devices(out);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].model.as_deref(), Some("Pixel_7"));
        assert_eq!(d[1].state, "unauthorized");
    }
}
