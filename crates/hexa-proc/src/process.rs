//! Process list (PROC-01).

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind, Users};

use crate::error::{Error, Result};
use crate::os;

/// CPU architecture of a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Arch {
    X86,
    X64,
    Arm,
    Arm64,
    RiscV64,
    Other,
}

/// One entry of the process list (PROC-01).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessInfo {
    pub pid: u32,
    pub parent_pid: Option<u32>,
    pub name: String,
    /// Full executable path (unknown for processes we may not inspect).
    pub path: Option<String>,
    /// Owning user name (or numeric id when it has no name).
    pub user: Option<String>,
    pub arch: Option<Arch>,
    /// 32 or 64.
    pub bits: Option<u8>,
    /// Resident memory in bytes.
    pub memory: u64,
    /// Virtual memory in bytes.
    pub virtual_memory: u64,
    /// Where the UI can load the icon from: the `.icns` of a macOS app
    /// bundle, or the executable itself on Windows (icon resource).
    pub icon_path: Option<String>,
    /// The Hexa process itself.
    pub is_self: bool,
}

/// Parses an ELF header (`e_ident` + `e_machine`) into architecture and
/// bitness.
pub fn elf_arch(header: &[u8]) -> Option<(Arch, u8)> {
    if header.len() < 20 || &header[..4] != b"\x7fELF" {
        return None;
    }
    let bits = match header[4] {
        1 => 32,
        2 => 64,
        _ => return None,
    };
    let machine = if header[5] == 2 {
        u16::from_be_bytes([header[18], header[19]])
    } else {
        u16::from_le_bytes([header[18], header[19]])
    };
    let arch = match machine {
        3 => Arch::X86,
        62 => Arch::X64,
        40 => Arch::Arm,
        183 => Arch::Arm64,
        243 if bits == 64 => Arch::RiscV64,
        _ => Arch::Other,
    };
    Some((arch, bits))
}

/// Finds the `.icns` of the app bundle containing `exe` (macOS).
fn bundle_icon(exe: &str, cache: &mut HashMap<String, Option<String>>) -> Option<String> {
    let idx = exe.find(".app/")?;
    let bundle = &exe[..idx + 4];
    if let Some(hit) = cache.get(bundle) {
        return hit.clone();
    }
    let resources = Path::new(bundle).join("Contents/Resources");
    let from_plist = std::fs::read_to_string(Path::new(bundle).join("Contents/Info.plist"))
        .ok()
        .and_then(|plist| plist_string(&plist, "CFBundleIconFile"))
        .map(|name| {
            if Path::new(&name).extension().is_some() {
                name
            } else {
                format!("{name}.icns")
            }
        });
    let icon = from_plist
        .into_iter()
        .chain(std::iter::once("AppIcon.icns".to_string()))
        .map(|name| resources.join(name))
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned());
    cache.insert(bundle.to_string(), icon.clone());
    icon
}

/// Value of `<key>key</key><string>value</string>` in an XML plist.
fn plist_string(plist: &str, key: &str) -> Option<String> {
    let tag = format!("<key>{key}</key>");
    let rest = &plist[plist.find(&tag)? + tag.len()..];
    let start = rest.find("<string>")? + "<string>".len();
    let end = rest[start..].find("</string>")?;
    let value = rest[start..start + end].trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn icon_path(exe: Option<&str>, cache: &mut HashMap<String, Option<String>>) -> Option<String> {
    let exe = exe?;
    if cfg!(target_os = "macos") {
        bundle_icon(exe, cache)
    } else if cfg!(windows) {
        Some(exe.to_string())
    } else {
        None
    }
}

fn refresh_kind() -> ProcessRefreshKind {
    ProcessRefreshKind::nothing()
        .with_memory()
        .with_exe(UpdateKind::OnlyIfNotSet)
        .with_user(UpdateKind::OnlyIfNotSet)
        .without_tasks()
}

fn collect(system: &System, pids: Option<&[Pid]>) -> Vec<ProcessInfo> {
    let users = Users::new_with_refreshed_list().ok();
    let me = std::process::id();
    let mut icons = HashMap::new();
    let mut out = Vec::new();
    for (pid, p) in system.processes() {
        if pids.is_some_and(|set| !set.contains(pid)) {
            continue;
        }
        let pid = pid.as_u32();
        let path = p
            .exe()
            .map(|e| e.to_string_lossy().into_owned())
            .filter(|e| !e.is_empty())
            .or_else(|| os::executable_path(pid));
        let mut name = p.name().to_string_lossy().into_owned();
        // Linux truncates `comm` to 15 bytes; prefer the full file name.
        if let Some(file) = path.as_deref().and_then(|p| Path::new(p).file_name()) {
            let file = file.to_string_lossy();
            if name.is_empty() || (file.starts_with(&name) && file.len() > name.len()) {
                name = file.into_owned();
            }
        }
        let user = p.user_id().map(|uid| {
            users
                .as_ref()
                .and_then(|u| u.get_user_by_id(uid))
                .map_or_else(|| uid.to_string(), |u| u.name().to_string())
        });
        let arch = os::process_arch(pid);
        out.push(ProcessInfo {
            pid,
            parent_pid: p.parent().map(Pid::as_u32),
            icon_path: icon_path(path.as_deref(), &mut icons),
            name,
            path,
            user,
            arch: arch.map(|a| a.0),
            bits: arch.map(|a| a.1),
            memory: p.memory(),
            virtual_memory: p.virtual_memory(),
            is_self: pid == me,
        });
    }
    out.sort_by_key(|p| p.pid);
    out
}

/// Lists all processes, sorted by PID (PROC-01).
pub fn list_processes() -> Result<Vec<ProcessInfo>> {
    let mut system = System::new().map_err(|e| Error::unsupported(e.to_string()))?;
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind());
    Ok(collect(&system, None))
}

/// Information about one process.
pub fn process_info(pid: u32) -> Result<ProcessInfo> {
    let mut system = System::new().map_err(|e| Error::unsupported(e.to_string()))?;
    let pids = [Pid::from_u32(pid)];
    system.refresh_processes_specifics(ProcessesToUpdate::Some(&pids), true, refresh_kind());
    collect(&system, Some(&pids))
        .pop()
        .ok_or(Error::NoSuchProcess { pid })
}

/// Search/filter options for the process list (PROC-01).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProcessFilter {
    /// Case-insensitive substring of the name or path, or a PID prefix.
    pub query: String,
    /// Only processes of this user.
    pub user: Option<String>,
    /// Only processes with this bitness.
    pub bits: Option<u8>,
    /// Hide processes without a known executable path (kernel threads,
    /// processes we may not inspect).
    pub hide_without_path: bool,
}

impl ProcessFilter {
    pub fn matches(&self, p: &ProcessInfo) -> bool {
        if self
            .user
            .as_ref()
            .is_some_and(|u| p.user.as_ref() != Some(u))
            || self.bits.is_some_and(|b| p.bits != Some(b))
            || (self.hide_without_path && p.path.is_none())
        {
            return false;
        }
        let q = self.query.trim();
        if q.is_empty() {
            return true;
        }
        if q.bytes().all(|b| b.is_ascii_digit()) && p.pid.to_string().starts_with(q) {
            return true;
        }
        let q = q.to_lowercase();
        p.name.to_lowercase().contains(&q)
            || p.path
                .as_ref()
                .is_some_and(|path| path.to_lowercase().contains(&q))
    }
}

/// Processes matching `filter` (PROC-01).
pub fn filter_processes<'a>(
    list: &'a [ProcessInfo],
    filter: &ProcessFilter,
) -> Vec<&'a ProcessInfo> {
    list.iter().filter(|p| filter.matches(p)).collect()
}

/// Sort keys for the process list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProcessSort {
    Pid,
    Name,
    User,
    Memory,
}

/// Sorts the list in place; ties are broken by PID.
pub fn sort_processes(list: &mut [ProcessInfo], key: ProcessSort, descending: bool) {
    list.sort_by(|a, b| {
        let ord = match key {
            ProcessSort::Pid => Ordering::Equal,
            ProcessSort::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            ProcessSort::User => a.user.cmp(&b.user),
            ProcessSort::Memory => a.memory.cmp(&b.memory),
        }
        .then(a.pid.cmp(&b.pid));
        if descending { ord.reverse() } else { ord }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_self() {
        let list = list_processes().unwrap();
        let me = std::process::id();
        let entry = list
            .iter()
            .find(|p| p.pid == me)
            .expect("own process listed");
        assert!(entry.is_self);
        assert_ne!(entry.name.len(), 0);
        assert!(entry.path.is_some());
        assert_eq!(entry.bits, Some((std::mem::size_of::<usize>() * 8) as u8));
        assert!(entry.memory > 0);

        let one = process_info(me).unwrap();
        assert_eq!(one.pid, me);
        assert!(matches!(
            process_info(u32::MAX - 7),
            Err(Error::NoSuchProcess { .. })
        ));

        let filter = ProcessFilter {
            query: me.to_string(),
            ..ProcessFilter::default()
        };
        assert!(filter_processes(&list, &filter).iter().any(|p| p.pid == me));
    }

    fn info(pid: u32, name: &str, memory: u64) -> ProcessInfo {
        ProcessInfo {
            pid,
            parent_pid: None,
            name: name.into(),
            path: Some(format!("/opt/{name}")),
            user: Some("alice".into()),
            arch: Some(Arch::X64),
            bits: Some(64),
            memory,
            virtual_memory: memory,
            icon_path: None,
            is_self: false,
        }
    }

    #[test]
    fn filter_and_sort() {
        let mut list = vec![
            info(30, "Game", 5),
            info(4, "editor", 50),
            info(12, "gamehelper", 1),
        ];
        let f = ProcessFilter {
            query: "GAME".into(),
            ..ProcessFilter::default()
        };
        assert_eq!(filter_processes(&list, &f).len(), 2);
        let f = ProcessFilter {
            user: Some("bob".into()),
            ..ProcessFilter::default()
        };
        assert_eq!(filter_processes(&list, &f).len(), 0);
        sort_processes(&mut list, ProcessSort::Memory, true);
        assert_eq!(list[0].pid, 4);
        sort_processes(&mut list, ProcessSort::Name, false);
        assert_eq!(list[0].name, "editor");
        sort_processes(&mut list, ProcessSort::Pid, false);
        assert_eq!(list[0].pid, 4);
    }

    #[test]
    fn elf_header() {
        let mut h = [0u8; 20];
        h[..4].copy_from_slice(b"\x7fELF");
        h[4] = 2;
        h[5] = 1;
        h[18] = 62;
        assert_eq!(elf_arch(&h), Some((Arch::X64, 64)));
        h[4] = 1;
        h[18] = 3;
        assert_eq!(elf_arch(&h), Some((Arch::X86, 32)));
        assert_eq!(elf_arch(b"MZ"), None);
    }

    #[test]
    fn plist_icon_key() {
        let plist = "<dict><key>CFBundleIconFile</key>\n  <string>AppIcon</string></dict>";
        assert_eq!(
            plist_string(plist, "CFBundleIconFile").as_deref(),
            Some("AppIcon")
        );
        assert_eq!(plist_string(plist, "Missing"), None);
    }
}
