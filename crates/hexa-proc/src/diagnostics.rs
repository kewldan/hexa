//! Permission diagnostics (PROC-10).
//!
//! Reading another process' memory needs privileges that differ per OS. When
//! the OS refuses, the raw error code is turned into a [`Diagnostic`] that
//! says *why* and *what to do*:
//!
//! * **macOS** — `task_for_pid` needs root or the
//!   `com.apple.security.cs.debugger` entitlement, and the target must not be
//!   hardened/SIP-protected (system binaries can never be read while SIP is
//!   on).
//! * **Linux** — the Yama LSM (`/proc/sys/kernel/yama/ptrace_scope`) limits
//!   ptrace-style access; `CAP_SYS_PTRACE` or root overrides levels 1–2,
//!   level 3 forbids it entirely until reboot.
//! * **Windows** — other users' and elevated processes need Hexa to run as
//!   administrator (which enables `SeDebugPrivilege`); protected processes
//!   (PPL: anti-malware, `csrss.exe`, …) cannot be opened at all.

use serde::Serialize;

use crate::os;

/// Operating system family a diagnostic was produced for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Platform {
    Linux,
    Macos,
    Windows,
    Other,
}

impl Platform {
    /// The platform Hexa is running on.
    pub const fn current() -> Self {
        if cfg!(any(target_os = "linux", target_os = "android")) {
            Self::Linux
        } else if cfg!(target_os = "macos") {
            Self::Macos
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Other
        }
    }
}

/// Human readable explanation of an access failure (PROC-10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub platform: Platform,
    pub pid: u32,
    /// What was attempted ("open process", "read memory", …).
    pub operation: String,
    /// Raw OS code (`errno`, `kern_return_t`, Win32 error).
    pub os_code: Option<i64>,
    /// One-line explanation.
    pub summary: String,
    /// Concrete things the user can do, in order of preference.
    pub steps: Vec<String>,
    /// The target cannot be accessed even with full privileges (SIP, PPL).
    pub protected: bool,
    /// Restarting Hexa elevated (root / administrator) would likely help.
    pub needs_elevation: bool,
}

/// Privileges Hexa currently runs with (PROC-10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Privileges {
    pub platform: Platform,
    /// Root (Unix) or an elevated token (Windows).
    pub elevated: bool,
    /// Yama `ptrace_scope` (Linux only).
    pub ptrace_scope: Option<u8>,
    /// Whether `SeDebugPrivilege` is enabled (Windows only).
    pub debug_privilege: Option<bool>,
}

/// Result of probing access to one process (PROC-10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessReport {
    pub pid: u32,
    pub readable: bool,
    pub writable: bool,
    /// Why access is limited, when it is.
    pub diagnostic: Option<Diagnostic>,
}

/// Current privilege state.
pub fn privileges() -> Privileges {
    Privileges {
        platform: Platform::current(),
        elevated: os::is_elevated(),
        ptrace_scope: read_ptrace_scope(),
        debug_privilege: os::debug_privilege_enabled(),
    }
}

/// Enables `SeDebugPrivilege` for Hexa (Windows; requires running as
/// administrator). Returns whether it is enabled; always `false` elsewhere.
/// Call once at startup before opening other users' processes.
pub fn enable_debug_privilege() -> crate::Result<bool> {
    os::enable_debug_privilege()
}

/// Reads `/proc/sys/kernel/yama/ptrace_scope` (Linux; `None` elsewhere or when
/// Yama is not built in).
pub fn read_ptrace_scope() -> Option<u8> {
    if Platform::current() != Platform::Linux {
        return None;
    }
    std::fs::read_to_string("/proc/sys/kernel/yama/ptrace_scope")
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

/// Opens `pid` for reading and for writing and reports what worked.
pub fn check_access(pid: u32) -> AccessReport {
    use crate::source::{Access, ProcessMemory};
    let read = ProcessMemory::open(pid, Access::Read);
    let write = ProcessMemory::open(pid, Access::ReadWrite);
    let diagnostic = match (&read, &write) {
        (Err(e), _) | (Ok(_), Err(e)) => Some(
            e.diagnostic()
                .cloned()
                .unwrap_or_else(|| generic(pid, "open process", None, e.to_string())),
        ),
        _ => None,
    };
    AccessReport {
        pid,
        readable: read.is_ok(),
        writable: write.is_ok(),
        diagnostic,
    }
}

/// Builds guidance for an OS error code on the current platform, gathering
/// the environment (privileges, `ptrace_scope`, target path) itself.
pub fn diagnose(pid: u32, operation: &str, code: i64) -> Diagnostic {
    let elevated = os::is_elevated();
    match Platform::current() {
        Platform::Linux => {
            linux_guidance(pid, operation, code as i32, read_ptrace_scope(), elevated)
        }
        Platform::Macos => {
            let path = os::executable_path(pid);
            macos_guidance(pid, operation, code as i32, elevated, path.as_deref())
        }
        Platform::Windows => windows_guidance(pid, operation, code as u32, elevated),
        Platform::Other => generic(
            pid,
            operation,
            Some(code),
            "process memory access is not supported on this OS".into(),
        ),
    }
}

fn generic(pid: u32, operation: &str, code: Option<i64>, summary: String) -> Diagnostic {
    Diagnostic {
        platform: Platform::current(),
        pid,
        operation: operation.into(),
        os_code: code,
        summary,
        steps: Vec::new(),
        protected: false,
        needs_elevation: false,
    }
}

const EPERM: i32 = 1;
const ESRCH: i32 = 3;
const EIO: i32 = 5;
const EACCES: i32 = 13;
const EFAULT: i32 = 14;

/// Linux guidance for an `errno` (PROC-10).
pub fn linux_guidance(
    pid: u32,
    operation: &str,
    errno: i32,
    ptrace_scope: Option<u8>,
    elevated: bool,
) -> Diagnostic {
    let mut d = Diagnostic {
        platform: Platform::Linux,
        pid,
        operation: operation.into(),
        os_code: Some(i64::from(errno)),
        summary: String::new(),
        steps: Vec::new(),
        protected: false,
        needs_elevation: false,
    };
    match errno {
        EPERM | EACCES => match ptrace_scope {
            Some(3) => {
                d.summary = "ptrace is disabled system-wide \
                                 (kernel.yama.ptrace_scope = 3)"
                    .into();
                d.protected = true;
                d.steps.push(
                    "Scope 3 cannot be lowered at runtime; reboot with a lower \
                         kernel.yama.ptrace_scope in /etc/sysctl.d/"
                        .into(),
                );
            }
            Some(scope @ (1 | 2)) if !elevated => {
                d.summary = format!(
                    "Yama ptrace_scope = {scope} only allows debugging \
                         {} processes",
                    if scope == 1 {
                        "descendant"
                    } else {
                        "with CAP_SYS_PTRACE"
                    }
                );
                d.needs_elevation = true;
                d.steps.push("Run Hexa with sudo (root), or".into());
                d.steps.push(
                    "grant the capability: sudo setcap cap_sys_ptrace=eip <path to hexa>".into(),
                );
                if scope == 1 {
                    d.steps.push(
                        "or temporarily allow it: \
                             echo 0 | sudo tee /proc/sys/kernel/yama/ptrace_scope"
                            .into(),
                    );
                }
            }
            _ if !elevated => {
                d.summary = "the process belongs to another user or is not dumpable".into();
                d.needs_elevation = true;
                d.steps
                    .push("Run Hexa as root (sudo) or as the owner of the process".into());
                d.steps
                    .push("Or grant CAP_SYS_PTRACE: sudo setcap cap_sys_ptrace=eip <hexa>".into());
            }
            _ => {
                d.summary = "access denied even as root; a security module (SELinux, \
                                 AppArmor) or a non-dumpable target refuses ptrace access"
                    .into();
                d.protected = true;
                d.steps
                    .push("Check the audit log (journalctl -k, ausearch -m avc)".into());
            }
        },
        ESRCH => {
            d.summary = "the process exited".into();
            d.steps.push("Refresh the process list".into());
        }
        EIO | EFAULT => {
            d.summary = "the address is not mapped in the target".into();
        }
        _ => {
            d.summary = format!("{operation} failed (errno {errno})");
        }
    }
    d
}

const KERN_INVALID_ADDRESS: i32 = 1;
const KERN_PROTECTION_FAILURE: i32 = 2;
const KERN_INVALID_ARGUMENT: i32 = 4;
const KERN_FAILURE: i32 = 5;

/// Whether a macOS executable lives in a SIP-protected location. Such
/// processes cannot be inspected while System Integrity Protection is on.
pub fn is_sip_protected_path(path: &str) -> bool {
    const PROTECTED: [&str; 5] = ["/System/", "/usr/", "/bin/", "/sbin/", "/Library/Apple/"];
    PROTECTED.iter().any(|p| path.starts_with(p)) && !path.starts_with("/usr/local/")
}

/// macOS guidance for a `kern_return_t` (PROC-10).
pub fn macos_guidance(
    pid: u32,
    operation: &str,
    kern_return: i32,
    elevated: bool,
    exe_path: Option<&str>,
) -> Diagnostic {
    let mut d = Diagnostic {
        platform: Platform::Macos,
        pid,
        operation: operation.into(),
        os_code: Some(i64::from(kern_return)),
        summary: String::new(),
        steps: Vec::new(),
        protected: false,
        needs_elevation: false,
    };
    match kern_return {
        KERN_FAILURE | KERN_INVALID_ARGUMENT if exe_path.is_some_and(is_sip_protected_path) => {
            d.summary = "the process is protected by System Integrity Protection (SIP)".into();
            d.protected = true;
            d.steps
                .push("SIP-protected system processes cannot be read; pick another process".into());
            d.steps.push(
                "For research only: disable SIP from Recovery (csrutil disable) — not recommended"
                    .into(),
            );
        }
        KERN_FAILURE | KERN_INVALID_ARGUMENT if !elevated => {
            d.summary = "task_for_pid was refused: reading another process needs root \
                         or the debugger entitlement"
                .into();
            d.needs_elevation = true;
            d.steps.push("Run Hexa with sudo, or".into());
            d.steps.push(
                "sign Hexa with the com.apple.security.cs.debugger entitlement and enable \
                 Developer Mode (DevToolsSecurity -enable)"
                    .into(),
            );
            d.steps.push(
                "Apps built with the hardened runtime can only be read if they carry the \
                 com.apple.security.get-task-allow entitlement"
                    .into(),
            );
        }
        KERN_FAILURE | KERN_INVALID_ARGUMENT => {
            d.summary = "task_for_pid was refused even as root; the target uses the hardened \
                         runtime or is a platform binary"
                .into();
            d.protected = true;
            d.steps.push(
                "Only apps with the com.apple.security.get-task-allow entitlement (debug \
                 builds) or with SIP disabled can be read"
                    .into(),
            );
        }
        KERN_INVALID_ADDRESS => {
            d.summary = "the address is not mapped in the target".into();
        }
        KERN_PROTECTION_FAILURE => {
            d.summary = "the memory protection forbids this access".into();
            d.steps
                .push("The page may be read-only code signed memory; try another address".into());
        }
        _ => {
            d.summary = format!("{operation} failed (kern_return_t {kern_return})");
        }
    }
    d
}

const ERROR_ACCESS_DENIED: u32 = 5;
const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_PARTIAL_COPY: u32 = 299;
const ERROR_NOACCESS: u32 = 998;

/// Windows guidance for a Win32 error code (PROC-10).
pub fn windows_guidance(pid: u32, operation: &str, error: u32, elevated: bool) -> Diagnostic {
    let mut d = Diagnostic {
        platform: Platform::Windows,
        pid,
        operation: operation.into(),
        os_code: Some(i64::from(error)),
        summary: String::new(),
        steps: Vec::new(),
        protected: false,
        needs_elevation: false,
    };
    match error {
        ERROR_ACCESS_DENIED if !elevated => {
            d.summary = "the process runs as another user or elevated".into();
            d.needs_elevation = true;
            d.steps
                .push("Restart Hexa with \"Run as administrator\"".into());
            d.steps.push(
                "Hexa then enables SeDebugPrivilege, which allows opening other users' processes"
                    .into(),
            );
        }
        ERROR_ACCESS_DENIED => {
            d.summary = "the process is protected (PPL / protected process): \
                         even administrators cannot read it"
                .into();
            d.protected = true;
            d.steps.push(
                "Anti-malware services, csrss.exe, lsass.exe (with LSA protection) and DRM \
                 processes cannot be opened"
                    .into(),
            );
        }
        ERROR_INVALID_PARAMETER => {
            d.summary = "the process exited".into();
            d.steps.push("Refresh the process list".into());
        }
        ERROR_PARTIAL_COPY | ERROR_NOACCESS => {
            d.summary = "the address is not mapped or not accessible in the target".into();
        }
        _ => {
            d.summary = format!("{operation} failed (Win32 error {error})");
        }
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_scope_guidance() {
        let d = linux_guidance(42, "open process", EPERM, Some(1), false);
        assert!(d.needs_elevation);
        assert!(d.summary.contains("ptrace_scope = 1"));
        assert!(d.steps.iter().any(|s| s.contains("cap_sys_ptrace")));
        let d = linux_guidance(42, "open process", EACCES, Some(3), true);
        assert!(d.protected);
        let d = linux_guidance(42, "open process", ESRCH, None, false);
        assert_eq!(d.summary, "the process exited");
    }

    #[test]
    fn macos_guidance_mentions_entitlement_and_sip() {
        let d = macos_guidance(
            1,
            "open process",
            KERN_FAILURE,
            false,
            Some("/Applications/X.app"),
        );
        assert!(d.needs_elevation);
        assert!(
            d.steps
                .iter()
                .any(|s| s.contains("com.apple.security.cs.debugger"))
        );
        let d = macos_guidance(
            1,
            "open process",
            KERN_FAILURE,
            true,
            Some("/usr/libexec/foo"),
        );
        assert!(d.protected);
        assert!(d.summary.contains("SIP"));
        assert!(!is_sip_protected_path("/usr/local/bin/x"));
    }

    #[test]
    fn windows_guidance_distinguishes_ppl() {
        let d = windows_guidance(4, "open process", ERROR_ACCESS_DENIED, false);
        assert!(d.needs_elevation);
        let d = windows_guidance(4, "open process", ERROR_ACCESS_DENIED, true);
        assert!(d.protected);
    }

    #[test]
    fn privileges_and_self_access() {
        let p = privileges();
        assert_eq!(p.platform, Platform::current());
        let report = check_access(std::process::id());
        assert!(report.readable, "{report:?}");
    }
}
