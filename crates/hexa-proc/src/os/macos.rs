//! macOS back end: Mach task ports (PROC-02, PROC-04).
//!
//! `task_for_pid` needs root or the `com.apple.security.cs.debugger`
//! entitlement for other processes; the calling task is always available
//! through `mach_task_self()`.

use mach2::kern_return::{
    KERN_INVALID_ADDRESS, KERN_PROTECTION_FAILURE, KERN_SUCCESS, kern_return_t,
};
use mach2::mach_port::mach_port_deallocate;
use mach2::message::mach_msg_type_number_t;
use mach2::port::{MACH_PORT_NULL, mach_port_t};
use mach2::traps::{mach_task_self, task_for_pid};
use mach2::vm::{
    mach_vm_protect, mach_vm_read_overwrite, mach_vm_region, mach_vm_region_recurse, mach_vm_write,
};
use mach2::vm_prot::{VM_PROT_COPY, VM_PROT_EXECUTE, VM_PROT_READ, VM_PROT_WRITE, vm_prot_t};
use mach2::vm_region::{
    SM_SHARED, SM_SHARED_ALIASED, SM_TRUESHARED, VM_REGION_BASIC_INFO_64, vm_region_basic_info_64,
    vm_region_info_t, vm_region_recurse_info_t, vm_region_submap_info_64,
};
use mach2::vm_statistics::{VM_MEMORY_DYLD_MALLOC, VM_MEMORY_MALLOC_PROB_GUARD, VM_MEMORY_STACK};

use crate::diagnostics::macos_guidance;
use crate::error::{Error, Result};
use crate::process::Arch;
use crate::region::{MemoryRegion, Protection, RegionKind, classify_images};

/// `PROC_PIDPATHINFO_MAXSIZE`.
const PATH_MAX: usize = 4096;
/// `PROC_FLAG_LP64` in `proc_bsdinfo::pbi_flags`.
const PROC_FLAG_LP64: u32 = 0x10;
/// `P_TRANSLATED` in `extern_proc::p_flag` (Rosetta 2).
const P_TRANSLATED: i32 = 0x0002_0000;
/// Offset of `kinfo_proc.kp_proc.p_flag` (after `p_un`, `p_vmspace`,
/// `p_sigacts`) on 64-bit macOS.
const P_FLAG_OFFSET: usize = 32;
const KERNEL_SPACE: u64 = 1 << 56;

#[derive(Debug)]
pub(crate) struct Handle {
    pid: u32,
    task: mach_port_t,
    /// Whether `task` is a send right we must release (not our own task).
    owned: bool,
}

fn process_exists(pid: u32) -> bool {
    // SAFETY: kill with signal 0 only checks for existence/permission.
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

fn os_error(operation: &'static str, kr: kern_return_t) -> Error {
    Error::Os {
        operation,
        code: i64::from(kr),
        message: format!("kern_return_t {kr}"),
    }
}

impl Handle {
    pub(crate) fn open(pid: u32, _write: bool) -> Result<Self> {
        if pid == std::process::id() {
            return Ok(Self {
                pid,
                // SAFETY: reads the process-global task port; no preconditions.
                task: unsafe { mach_task_self() },
                owned: false,
            });
        }
        let mut task: mach_port_t = MACH_PORT_NULL;
        // SAFETY: `task` is a valid out pointer; the call has no other
        // preconditions.
        let kr = unsafe { task_for_pid(mach_task_self(), pid as i32, &raw mut task) };
        if kr != KERN_SUCCESS || task == MACH_PORT_NULL {
            if !process_exists(pid) {
                return Err(Error::NoSuchProcess { pid });
            }
            let path = executable_path(pid);
            return Err(Error::PermissionDenied {
                pid,
                diagnostic: Box::new(macos_guidance(
                    pid,
                    "open process",
                    kr,
                    is_elevated(),
                    path.as_deref(),
                )),
            });
        }
        Ok(Self {
            pid,
            task,
            owned: true,
        })
    }

    fn map_error(&self, address: u64, kr: kern_return_t, operation: &'static str) -> Error {
        match kr {
            KERN_INVALID_ADDRESS | KERN_PROTECTION_FAILURE => Error::Unmapped { address },
            _ if !process_exists(self.pid) => Error::NoSuchProcess { pid: self.pid },
            _ => os_error(operation, kr),
        }
    }

    pub(crate) fn read(&self, address: u64, buf: &mut [u8]) -> Result<()> {
        let mut out = 0u64;
        // SAFETY: the kernel writes at most `buf.len()` bytes to `buf`, which
        // is valid and exclusively borrowed for the call.
        let kr = unsafe {
            mach_vm_read_overwrite(
                self.task,
                address,
                buf.len() as u64,
                buf.as_mut_ptr() as u64,
                &raw mut out,
            )
        };
        if kr != KERN_SUCCESS {
            return Err(self.map_error(address, kr, "mach_vm_read_overwrite"));
        }
        if out < buf.len() as u64 {
            return Err(Error::Unmapped {
                address: address + out,
            });
        }
        Ok(())
    }

    fn raw_write(&self, address: u64, data: &[u8]) -> kern_return_t {
        // SAFETY: `data` is valid for `data.len()` bytes; the kernel copies it
        // into the target task.
        unsafe {
            mach_vm_write(
                self.task,
                address,
                data.as_ptr() as usize,
                data.len() as mach_msg_type_number_t,
            )
        }
    }

    /// Top-level region containing or following `address`:
    /// `(start, size, protection)`.
    fn region_at(&self, address: u64) -> Option<(u64, u64, vm_prot_t)> {
        let mut start = address;
        let mut size = 0u64;
        let mut info = vm_region_basic_info_64::default();
        let mut count = vm_region_basic_info_64::count();
        let mut object = MACH_PORT_NULL;
        // SAFETY: all pointers are valid out parameters; `count` holds the
        // capacity of `info` in `int`s as the API requires.
        let kr = unsafe {
            mach_vm_region(
                self.task,
                &raw mut start,
                &raw mut size,
                VM_REGION_BASIC_INFO_64,
                (&raw mut info).cast::<i32>() as vm_region_info_t,
                &raw mut count,
                &raw mut object,
            )
        };
        (kr == KERN_SUCCESS).then_some((start, size, info.protection))
    }

    pub(crate) fn write(&self, address: u64, data: &[u8]) -> Result<()> {
        let kr = self.raw_write(address, data);
        if kr == KERN_SUCCESS {
            return Ok(());
        }
        if kr != KERN_PROTECTION_FAILURE && kr != KERN_INVALID_ADDRESS {
            return Err(self.map_error(address, kr, "mach_vm_write"));
        }
        // Read-only pages: make each touched region writable (copy-on-write
        // for shared file mappings), write, restore the old protection.
        let end = address + data.len() as u64;
        let mut pos = address;
        while pos < end {
            let Some((start, size, prot)) = self.region_at(pos) else {
                return Err(Error::Unmapped { address: pos });
            };
            if start > pos {
                return Err(Error::Unmapped { address: pos });
            }
            let chunk_end = end.min(start + size);
            let len = chunk_end - pos;
            let chunk = &data[(pos - address) as usize..(chunk_end - address) as usize];
            let want = prot | VM_PROT_READ | VM_PROT_WRITE | VM_PROT_COPY;
            // SAFETY: changes protection of the target task's pages only; the
            // range was just reported as mapped.
            let kr = unsafe { mach_vm_protect(self.task, pos, len, 0, want) };
            if kr != KERN_SUCCESS {
                return Err(os_error("mach_vm_protect", kr));
            }
            let kr = self.raw_write(pos, chunk);
            // SAFETY: as above; restores the protection read before.
            unsafe { mach_vm_protect(self.task, pos, len, 0, prot) };
            if kr != KERN_SUCCESS {
                return Err(self.map_error(pos, kr, "mach_vm_write"));
            }
            pos = chunk_end;
        }
        Ok(())
    }

    pub(crate) fn regions(&self) -> Result<Vec<MemoryRegion>> {
        let mut out: Vec<MemoryRegion> = Vec::new();
        let mut address = 0u64;
        let mut depth: u32 = 0;
        loop {
            let mut size = 0u64;
            let mut info = vm_region_submap_info_64::default();
            let mut count = vm_region_submap_info_64::count();
            // SAFETY: valid out pointers; `count` is the capacity of `info`.
            let kr = unsafe {
                mach_vm_region_recurse(
                    self.task,
                    &raw mut address,
                    &raw mut size,
                    &raw mut depth,
                    (&raw mut info).cast::<i32>() as vm_region_recurse_info_t,
                    &raw mut count,
                )
            };
            if kr == KERN_INVALID_ADDRESS {
                break;
            }
            if kr != KERN_SUCCESS {
                if out.is_empty() {
                    return Err(self.map_error(address, kr, "mach_vm_region_recurse"));
                }
                break;
            }
            if info.is_submap != 0 {
                depth += 1;
                continue;
            }
            if address >= KERNEL_SPACE {
                break;
            }
            let prot = info.protection;
            let tag = info.user_tag;
            let share_mode = info.share_mode;
            let mut region = MemoryRegion {
                start: address,
                size,
                protection: Protection {
                    read: prot & VM_PROT_READ != 0,
                    write: prot & VM_PROT_WRITE != 0,
                    execute: prot & VM_PROT_EXECUTE != 0,
                },
                kind: match tag {
                    1..=VM_MEMORY_MALLOC_PROB_GUARD | VM_MEMORY_DYLD_MALLOC => RegionKind::Heap,
                    VM_MEMORY_STACK => RegionKind::Stack,
                    _ => RegionKind::Private,
                },
                shared: matches!(share_mode, SM_SHARED | SM_TRUESHARED | SM_SHARED_ALIASED),
                path: region_filename(self.pid, address),
                label: None,
            };
            if region.path.is_some() {
                // File-backed; `classify_images` decides Image vs Mapped.
                region.kind = RegionKind::Private;
            }
            // Merge with the previous region when only the submap level split it.
            match out.last_mut() {
                Some(prev)
                    if prev.end() == region.start
                        && prev.protection == region.protection
                        && prev.kind == region.kind
                        && prev.shared == region.shared
                        && prev.path == region.path =>
                {
                    prev.size += region.size;
                }
                _ => out.push(region),
            }
            address = address.saturating_add(size);
            if size == 0 {
                break;
            }
        }
        classify_images(&mut out);
        Ok(out)
    }

    pub(crate) fn max_address(&self, pointer_size: u8) -> u64 {
        let limit = super::default_user_limit(pointer_size);
        let last = self
            .regions()
            .ok()
            .and_then(|r| r.last().map(MemoryRegion::end))
            .unwrap_or(0);
        limit.max(last)
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        if self.owned {
            // SAFETY: `task` is a send right obtained from task_for_pid that
            // this handle owns exclusively.
            unsafe { mach_port_deallocate(mach_task_self(), self.task) };
        }
    }
}

/// `PROC_PIDREGIONPATHINFO` flavor of `proc_pidinfo`.
const PROC_PIDREGIONPATHINFO: i32 = 8;

/// `struct proc_regioninfo` from `<sys/proc_info.h>` (not in `libc`). All
/// fields are declared to keep the C layout even though few are read.
#[repr(C)]
#[allow(dead_code, clippy::struct_field_names)]
#[derive(Clone, Copy)]
struct ProcRegionInfo {
    pri_protection: u32,
    pri_max_protection: u32,
    pri_inheritance: u32,
    pri_flags: u32,
    pri_offset: u64,
    pri_behavior: u32,
    pri_user_wired_count: u32,
    pri_user_tag: u32,
    pri_pages_resident: u32,
    pri_pages_shared_now_private: u32,
    pri_pages_swapped_out: u32,
    pri_pages_dirtied: u32,
    pri_ref_count: u32,
    pri_shadow_depth: u32,
    pri_share_mode: u32,
    pri_private_pages_resident: u32,
    pri_shared_pages_resident: u32,
    pri_obj_id: u32,
    pri_depth: u32,
    pri_address: u64,
    pri_size: u64,
}

/// `struct proc_regionwithpathinfo`.
#[repr(C)]
struct ProcRegionWithPathInfo {
    prp_prinfo: ProcRegionInfo,
    prp_vip: libc::vnode_info_path,
}

/// Backing file of the region containing `address`. The kernel reports the
/// first region at *or after* `address`, so the result is only used when that
/// region actually contains it (anonymous memory has no path).
fn region_filename(pid: u32, address: u64) -> Option<String> {
    let mut info = std::mem::MaybeUninit::<ProcRegionWithPathInfo>::zeroed();
    let size = std::mem::size_of::<ProcRegionWithPathInfo>() as i32;
    // SAFETY: `info` is valid for `size` bytes.
    let n = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            PROC_PIDREGIONPATHINFO,
            address,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if n != size {
        return None;
    }
    // SAFETY: fully written by the kernel (n == size) and zero-initialised;
    // all fields are plain integers.
    let info = unsafe { info.assume_init() };
    let r = info.prp_prinfo;
    if address < r.pri_address || address >= r.pri_address.saturating_add(r.pri_size) {
        return None;
    }
    let bytes: Vec<u8> = info
        .prp_vip
        .vip_path
        .iter()
        .flatten()
        .map(|&c| c as u8)
        .take_while(|&b| b != 0)
        .collect();
    (!bytes.is_empty()).then(|| String::from_utf8_lossy(&bytes).into_owned())
}

pub(crate) fn executable_path(pid: u32) -> Option<String> {
    let mut buf = vec![0u8; PATH_MAX];
    // SAFETY: `buf` is valid for `PATH_MAX` bytes.
    let n = unsafe { libc::proc_pidpath(pid as i32, buf.as_mut_ptr().cast(), PATH_MAX as u32) };
    let n = usize::try_from(n).ok().filter(|&n| n > 0)?;
    buf.truncate(n.min(PATH_MAX));
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Whether `pid` runs under Rosetta 2.
fn is_translated(pid: u32) -> bool {
    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROC,
        libc::KERN_PROC_PID,
        pid as i32,
    ];
    let mut buf = [0u8; 1024];
    let mut len = buf.len();
    // SAFETY: `mib` has 4 valid entries, `buf`/`len` describe a writable
    // buffer; no new value is set.
    let r = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            4,
            buf.as_mut_ptr().cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if r != 0 || len < P_FLAG_OFFSET + 4 {
        return false;
    }
    let mut flag = [0u8; 4];
    flag.copy_from_slice(&buf[P_FLAG_OFFSET..P_FLAG_OFFSET + 4]);
    i32::from_ne_bytes(flag) & P_TRANSLATED != 0
}

pub(crate) fn process_arch(pid: u32) -> Option<(Arch, u8)> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    // SAFETY: `info` is valid for `size` bytes.
    let n = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if n != size {
        return None;
    }
    // SAFETY: proc_pidinfo filled the whole struct (n == size), and it was
    // zero-initialised anyway (plain integers).
    let info = unsafe { info.assume_init() };
    if info.pbi_flags & PROC_FLAG_LP64 == 0 {
        return Some((Arch::X86, 32));
    }
    let arch = if cfg!(target_arch = "aarch64") && !is_translated(pid) {
        Arch::Arm64
    } else {
        Arch::X64
    };
    Some((arch, 64))
}

pub(crate) fn is_elevated() -> bool {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

pub(crate) fn debug_privilege_enabled() -> Option<bool> {
    None
}

#[allow(clippy::unnecessary_wraps)] // same signature as the Windows back end
pub(crate) fn enable_debug_privilege() -> Result<bool> {
    Ok(false)
}
