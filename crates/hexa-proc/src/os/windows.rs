//! Windows back end: `OpenProcess`, `VirtualQueryEx`, `Read/WriteProcessMemory`
//! and PSAPI module names (PROC-02, PROC-04, PROC-10).

use std::ffi::c_void;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, ERROR_NOACCESS,
    ERROR_NOT_ALL_ASSIGNED, ERROR_PARTIAL_COPY, GetLastError, HANDLE, HMODULE, LUID,
};
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, GetTokenInformation, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW,
    PRIVILEGE_SET, PrivilegeCheck, SE_DEBUG_NAME, SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES,
    TOKEN_ELEVATION, TOKEN_PRIVILEGES, TOKEN_QUERY, TokenElevation,
};
use windows_sys::Win32::Storage::FileSystem::{GetLogicalDriveStringsW, QueryDosDeviceW};
use windows_sys::Win32::System::Diagnostics::Debug::{
    FlushInstructionCache, ReadProcessMemory, WriteProcessMemory,
};
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_IMAGE, MEM_MAPPED, MEMORY_BASIC_INFORMATION, PAGE_EXECUTE, PAGE_EXECUTE_READ,
    PAGE_EXECUTE_READWRITE, PAGE_EXECUTE_WRITECOPY, PAGE_GUARD, PAGE_NOACCESS, PAGE_READONLY,
    PAGE_READWRITE, PAGE_WRITECOPY, VirtualProtectEx, VirtualQueryEx,
};
use windows_sys::Win32::System::ProcessStatus::{
    EnumProcessModulesEx, GetMappedFileNameW, GetModuleFileNameExW, GetModuleInformation,
    LIST_MODULES_ALL, MODULEINFO,
};
use windows_sys::Win32::System::SystemInformation::{
    GetSystemInfo, IMAGE_FILE_MACHINE, IMAGE_FILE_MACHINE_AMD64, IMAGE_FILE_MACHINE_ARM64,
    IMAGE_FILE_MACHINE_ARMNT, IMAGE_FILE_MACHINE_I386, IMAGE_FILE_MACHINE_UNKNOWN, SYSTEM_INFO,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, IsWow64Process2, OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32,
    PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_OPERATION,
    PROCESS_VM_READ, PROCESS_VM_WRITE, QueryFullProcessImageNameW,
};

use crate::diagnostics::windows_guidance;
use crate::error::{Error, Result};
use crate::process::Arch;
use crate::region::{MemoryRegion, Protection, RegionKind};

/// An owned process handle. Stored as an integer so the type is `Send + Sync`
/// (kernel handles may be used from any thread).
#[derive(Debug)]
struct OwnedHandle(usize);

impl OwnedHandle {
    fn open(pid: u32, access: u32) -> std::result::Result<Self, u32> {
        // SAFETY: plain FFI call; a null result is checked below.
        let h = unsafe { OpenProcess(access, 0, pid) };
        if h.is_null() {
            // SAFETY: no preconditions.
            Err(unsafe { GetLastError() })
        } else {
            Ok(Self(h as usize))
        }
    }

    fn raw(&self) -> HANDLE {
        self.0 as HANDLE
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by OpenProcess and is owned.
        unsafe { CloseHandle(self.raw()) };
    }
}

#[derive(Debug)]
pub(crate) struct Handle {
    pid: u32,
    process: OwnedHandle,
    wow64: bool,
}

fn last_error() -> u32 {
    // SAFETY: no preconditions.
    unsafe { GetLastError() }
}

fn wide_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

impl Handle {
    pub(crate) fn open(pid: u32, write: bool) -> Result<Self> {
        let mut access = PROCESS_QUERY_INFORMATION | PROCESS_VM_READ;
        if write {
            access |= PROCESS_VM_WRITE | PROCESS_VM_OPERATION;
        }
        let handle = OwnedHandle::open(pid, access).map_err(|code| match code {
            ERROR_INVALID_PARAMETER => Error::NoSuchProcess { pid },
            ERROR_ACCESS_DENIED => Error::PermissionDenied {
                pid,
                diagnostic: Box::new(windows_guidance(pid, "open process", code, is_elevated())),
            },
            _ => Error::Os {
                operation: "OpenProcess",
                code: i64::from(code),
                message: format!("Win32 error {code}"),
            },
        })?;
        let wow64 =
            machine(handle.raw()).is_some_and(|(process, _)| process != IMAGE_FILE_MACHINE_UNKNOWN);
        Ok(Self {
            pid,
            process: handle,
            wow64,
        })
    }

    fn map_error(&self, address: u64, code: u32, operation: &'static str) -> Error {
        match code {
            ERROR_PARTIAL_COPY | ERROR_NOACCESS | ERROR_INVALID_PARAMETER => {
                Error::Unmapped { address }
            }
            ERROR_ACCESS_DENIED => Error::PermissionDenied {
                pid: self.pid,
                diagnostic: Box::new(windows_guidance(self.pid, operation, code, is_elevated())),
            },
            _ => Error::Os {
                operation,
                code: i64::from(code),
                message: format!("Win32 error {code}"),
            },
        }
    }

    pub(crate) fn read(&self, address: u64, buf: &mut [u8]) -> Result<()> {
        let mut read = 0usize;
        // SAFETY: `buf` is valid for `buf.len()` writable bytes; the remote
        // address is only dereferenced by the kernel.
        let ok = unsafe {
            ReadProcessMemory(
                self.process.raw(),
                address as usize as *const c_void,
                buf.as_mut_ptr().cast(),
                buf.len(),
                &raw mut read,
            )
        };
        if ok == 0 || read < buf.len() {
            let code = if ok == 0 {
                last_error()
            } else {
                ERROR_PARTIAL_COPY
            };
            return Err(self.map_error(address + read as u64, code, "ReadProcessMemory"));
        }
        Ok(())
    }

    fn raw_write(&self, address: u64, data: &[u8]) -> std::result::Result<(), u32> {
        let mut written = 0usize;
        // SAFETY: `data` is valid for `data.len()` bytes.
        let ok = unsafe {
            WriteProcessMemory(
                self.process.raw(),
                address as usize as *const c_void,
                data.as_ptr().cast(),
                data.len(),
                &raw mut written,
            )
        };
        if ok == 0 {
            Err(last_error())
        } else if written < data.len() {
            Err(ERROR_PARTIAL_COPY)
        } else {
            Ok(())
        }
    }

    pub(crate) fn write(&self, address: u64, data: &[u8]) -> Result<()> {
        let Err(code) = self.raw_write(address, data) else {
            return Ok(());
        };
        if code != ERROR_NOACCESS && code != ERROR_PARTIAL_COPY {
            return Err(self.map_error(address, code, "WriteProcessMemory"));
        }
        // Read-only or code pages: make them writable, write, restore.
        let mut old = 0u32;
        // SAFETY: changes the protection of the target's pages only.
        let ok = unsafe {
            VirtualProtectEx(
                self.process.raw(),
                address as usize as *const c_void,
                data.len(),
                PAGE_EXECUTE_READWRITE,
                &raw mut old,
            )
        };
        if ok == 0 {
            return Err(self.map_error(address, last_error(), "VirtualProtectEx"));
        }
        let result = self.raw_write(address, data);
        let mut ignored = 0u32;
        // SAFETY: restores the protection read above; flushing the
        // instruction cache has no memory-safety preconditions.
        unsafe {
            VirtualProtectEx(
                self.process.raw(),
                address as usize as *const c_void,
                data.len(),
                old,
                &raw mut ignored,
            );
            FlushInstructionCache(
                self.process.raw(),
                address as usize as *const c_void,
                data.len(),
            );
        }
        result.map_err(|code| self.map_error(address, code, "WriteProcessMemory"))
    }

    fn module_ranges(&self) -> Vec<(u64, u64, String)> {
        let mut modules: Vec<HMODULE> = vec![std::ptr::null_mut(); 1024];
        loop {
            let mut needed = 0u32;
            let bytes = (modules.len() * std::mem::size_of::<HMODULE>()) as u32;
            // SAFETY: `modules` is valid for `bytes` bytes.
            let ok = unsafe {
                EnumProcessModulesEx(
                    self.process.raw(),
                    modules.as_mut_ptr(),
                    bytes,
                    &raw mut needed,
                    LIST_MODULES_ALL,
                )
            };
            if ok == 0 {
                return Vec::new();
            }
            let count = needed as usize / std::mem::size_of::<HMODULE>();
            if count <= modules.len() {
                modules.truncate(count);
                break;
            }
            modules.resize(count, std::ptr::null_mut());
        }
        let mut out = Vec::with_capacity(modules.len());
        for &m in &modules {
            let mut info = MODULEINFO {
                lpBaseOfDll: std::ptr::null_mut(),
                SizeOfImage: 0,
                EntryPoint: std::ptr::null_mut(),
            };
            // SAFETY: `info` is a valid out parameter of the given size.
            let ok = unsafe {
                GetModuleInformation(
                    self.process.raw(),
                    m,
                    &raw mut info,
                    std::mem::size_of::<MODULEINFO>() as u32,
                )
            };
            if ok == 0 {
                continue;
            }
            let mut name = vec![0u16; 1024];
            // SAFETY: `name` is valid for `name.len()` UTF-16 units.
            let n = unsafe {
                GetModuleFileNameExW(self.process.raw(), m, name.as_mut_ptr(), name.len() as u32)
            };
            if n == 0 {
                continue;
            }
            let base = info.lpBaseOfDll as usize as u64;
            out.push((
                base,
                base + u64::from(info.SizeOfImage),
                wide_to_string(&name),
            ));
        }
        out.sort_by_key(|m| m.0);
        out
    }

    fn mapped_file_name(&self, address: u64, devices: &[(String, String)]) -> Option<String> {
        let mut name = vec![0u16; 1024];
        // SAFETY: `name` is valid for `name.len()` UTF-16 units.
        let n = unsafe {
            GetMappedFileNameW(
                self.process.raw(),
                address as usize as *const c_void,
                name.as_mut_ptr(),
                name.len() as u32,
            )
        };
        if n == 0 {
            return None;
        }
        let device_path = wide_to_string(&name);
        Some(
            devices
                .iter()
                .find_map(|(drive, device)| {
                    device_path
                        .strip_prefix(device.as_str())
                        .filter(|rest| rest.starts_with('\\'))
                        .map(|rest| format!("{drive}{rest}"))
                })
                .unwrap_or(device_path),
        )
    }

    pub(crate) fn regions(&self) -> Result<Vec<MemoryRegion>> {
        let modules = self.module_ranges();
        let devices = dos_devices();
        let limit = self.max_address(if self.wow64 { 4 } else { 8 });
        let mut out = Vec::new();
        let mut address = 0u64;
        while address < limit {
            // SAFETY: MEMORY_BASIC_INFORMATION is plain data; all-zero is valid.
            let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
            // SAFETY: `info` is a valid out parameter of the given size.
            let n = unsafe {
                VirtualQueryEx(
                    self.process.raw(),
                    address as usize as *const c_void,
                    &raw mut info,
                    std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
                )
            };
            if n == 0 {
                if out.is_empty() {
                    return Err(self.map_error(address, last_error(), "VirtualQueryEx"));
                }
                break;
            }
            let start = info.BaseAddress as usize as u64;
            let size = info.RegionSize as u64;
            if size == 0 {
                break;
            }
            if info.State == MEM_COMMIT {
                let kind = match info.Type {
                    MEM_IMAGE => RegionKind::Image,
                    MEM_MAPPED => RegionKind::Mapped,
                    _ => RegionKind::Private,
                };
                let path = match kind {
                    RegionKind::Image => modules
                        .iter()
                        .find(|(b, e, _)| start >= *b && start < *e)
                        .map(|(_, _, p)| p.clone())
                        .or_else(|| self.mapped_file_name(start, &devices)),
                    RegionKind::Mapped => self.mapped_file_name(start, &devices),
                    _ => None,
                };
                out.push(MemoryRegion {
                    start,
                    size,
                    protection: protection(info.Protect),
                    kind,
                    shared: kind == RegionKind::Mapped,
                    path,
                    label: None,
                });
            }
            address = start.saturating_add(size);
        }
        Ok(out)
    }

    pub(crate) fn max_address(&self, pointer_size: u8) -> u64 {
        let mut info = SYSTEM_INFO::default();
        // SAFETY: `info` is a valid out parameter.
        unsafe { GetSystemInfo(&raw mut info) };
        let max = info.lpMaximumApplicationAddress as usize as u64 + 1;
        if self.wow64 || pointer_size <= 4 {
            max.min(super::default_user_limit(4))
        } else {
            max
        }
    }
}

fn protection(p: u32) -> Protection {
    if p & PAGE_GUARD != 0 || p & PAGE_NOACCESS != 0 {
        return Protection::NONE;
    }
    let base = p & 0xff;
    Protection {
        read: matches!(
            base,
            PAGE_READONLY
                | PAGE_READWRITE
                | PAGE_WRITECOPY
                | PAGE_EXECUTE_READ
                | PAGE_EXECUTE_READWRITE
                | PAGE_EXECUTE_WRITECOPY
        ),
        write: matches!(
            base,
            PAGE_READWRITE | PAGE_WRITECOPY | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY
        ),
        execute: matches!(
            base,
            PAGE_EXECUTE | PAGE_EXECUTE_READ | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY
        ),
    }
}

/// `("C:", "\Device\HarddiskVolume3")` pairs for translating
/// `GetMappedFileNameW` device paths.
fn dos_devices() -> Vec<(String, String)> {
    let mut drives = vec![0u16; 512];
    // SAFETY: `drives` is valid for `drives.len()` UTF-16 units.
    let n = unsafe { GetLogicalDriveStringsW(drives.len() as u32, drives.as_mut_ptr()) } as usize;
    if n == 0 || n > drives.len() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for drive in drives[..n].split(|&c| c == 0).filter(|d| !d.is_empty()) {
        // "C:\" -> "C:"
        let letter: Vec<u16> = drive
            .iter()
            .copied()
            .take_while(|&c| c != u16::from(b'\\'))
            .chain(std::iter::once(0))
            .collect();
        let mut target = vec![0u16; 1024];
        // SAFETY: `letter` is NUL-terminated, `target` valid for its length.
        let m =
            unsafe { QueryDosDeviceW(letter.as_ptr(), target.as_mut_ptr(), target.len() as u32) };
        if m == 0 {
            continue;
        }
        out.push((wide_to_string(&letter), wide_to_string(&target)));
    }
    out
}

/// `(process machine, native machine)`; process is `UNKNOWN` unless WOW64.
fn machine(handle: HANDLE) -> Option<(IMAGE_FILE_MACHINE, IMAGE_FILE_MACHINE)> {
    let mut process: IMAGE_FILE_MACHINE = 0;
    let mut native: IMAGE_FILE_MACHINE = 0;
    // SAFETY: valid out pointers.
    let ok = unsafe { IsWow64Process2(handle, &raw mut process, &raw mut native) };
    (ok != 0).then_some((process, native))
}

fn arch_of(m: IMAGE_FILE_MACHINE) -> Option<(Arch, u8)> {
    match m {
        IMAGE_FILE_MACHINE_I386 => Some((Arch::X86, 32)),
        IMAGE_FILE_MACHINE_AMD64 => Some((Arch::X64, 64)),
        IMAGE_FILE_MACHINE_ARM64 => Some((Arch::Arm64, 64)),
        IMAGE_FILE_MACHINE_ARMNT => Some((Arch::Arm, 32)),
        _ => None,
    }
}

pub(crate) fn process_arch(pid: u32) -> Option<(Arch, u8)> {
    let h = OwnedHandle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION).ok()?;
    let (process, native) = machine(h.raw())?;
    if process == IMAGE_FILE_MACHINE_UNKNOWN {
        arch_of(native)
    } else {
        arch_of(process)
    }
}

pub(crate) fn executable_path(pid: u32) -> Option<String> {
    let h = OwnedHandle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION).ok()?;
    let mut buf = vec![0u16; 32_768];
    let mut len = buf.len() as u32;
    // SAFETY: `buf` is valid for `len` UTF-16 units.
    let ok = unsafe {
        QueryFullProcessImageNameW(h.raw(), PROCESS_NAME_WIN32, buf.as_mut_ptr(), &raw mut len)
    };
    (ok != 0).then(|| String::from_utf16_lossy(&buf[..len as usize]))
}

/// The current process token, opened with `access`.
fn own_token(access: u32) -> Option<OwnedHandle> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: GetCurrentProcess returns a pseudo handle; `token` is a valid
    // out pointer.
    let ok = unsafe { OpenProcessToken(GetCurrentProcess(), access, &raw mut token) };
    (ok != 0 && !token.is_null()).then(|| OwnedHandle(token as usize))
}

pub(crate) fn is_elevated() -> bool {
    let Some(token) = own_token(TOKEN_QUERY) else {
        return false;
    };
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut len = 0u32;
    // SAFETY: `elevation` is a valid buffer of the stated size.
    let ok = unsafe {
        GetTokenInformation(
            token.raw(),
            TokenElevation,
            (&raw mut elevation).cast(),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &raw mut len,
        )
    };
    ok != 0 && elevation.TokenIsElevated != 0
}

fn debug_luid() -> Option<LUID> {
    let mut luid = LUID {
        LowPart: 0,
        HighPart: 0,
    };
    // SAFETY: SE_DEBUG_NAME is a static NUL-terminated string.
    let ok = unsafe { LookupPrivilegeValueW(std::ptr::null(), SE_DEBUG_NAME, &raw mut luid) };
    (ok != 0).then_some(luid)
}

pub(crate) fn debug_privilege_enabled() -> Option<bool> {
    let token = own_token(TOKEN_QUERY)?;
    let luid = debug_luid()?;
    let mut set = PRIVILEGE_SET {
        PrivilegeCount: 1,
        Control: 1, // PRIVILEGE_SET_ALL_NECESSARY
        Privilege: [LUID_AND_ATTRIBUTES {
            Luid: luid,
            Attributes: 0,
        }],
    };
    let mut result = 0;
    // SAFETY: valid in/out pointers.
    let ok = unsafe { PrivilegeCheck(token.raw(), &raw mut set, &raw mut result) };
    (ok != 0).then_some(result != 0)
}

/// Enables `SeDebugPrivilege` in the current token. Returns whether it is
/// now enabled (only possible when running elevated).
pub(crate) fn enable_debug_privilege() -> Result<bool> {
    let Some(token) = own_token(TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY) else {
        return Err(Error::Os {
            operation: "OpenProcessToken",
            code: i64::from(last_error()),
            message: "cannot open the process token".into(),
        });
    };
    let Some(luid) = debug_luid() else {
        return Ok(false);
    };
    let privileges = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [LUID_AND_ATTRIBUTES {
            Luid: luid,
            Attributes: SE_PRIVILEGE_ENABLED,
        }],
    };
    // SAFETY: `privileges` is a valid TOKEN_PRIVILEGES; no previous state
    // is requested.
    let ok = unsafe {
        AdjustTokenPrivileges(
            token.raw(),
            0,
            &raw const privileges,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    Ok(ok != 0 && last_error() != ERROR_NOT_ALL_ASSIGNED)
}
