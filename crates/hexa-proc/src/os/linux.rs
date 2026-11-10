//! Linux / Android back end: `/proc/<pid>/maps`, `process_vm_readv`,
//! `/proc/<pid>/mem` (PROC-02, PROC-04).

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;

use crate::diagnostics::linux_guidance;
use crate::error::{Error, Result};
use crate::process::{Arch, elf_arch};
use crate::region::{MemoryRegion, parse_proc_maps};

/// Regions at or above this address are kernel-provided (`[vsyscall]`) and
/// are not part of the user address space the provider exposes.
const KERNEL_SPACE: u64 = 1 << 56;

#[derive(Debug)]
pub(crate) struct Handle {
    pid: u32,
    mem: File,
    writable: bool,
}

fn map_open_error(pid: u32, e: &io::Error, operation: &str) -> Error {
    let errno = e.raw_os_error().unwrap_or(0);
    match errno {
        libc::ENOENT | libc::ESRCH => Error::NoSuchProcess { pid },
        libc::EACCES | libc::EPERM => Error::PermissionDenied {
            pid,
            diagnostic: Box::new(linux_guidance(
                pid,
                operation,
                errno,
                crate::diagnostics::read_ptrace_scope(),
                is_elevated(),
            )),
        },
        _ => Error::Os {
            operation: "open /proc/<pid>/mem",
            code: i64::from(errno),
            message: e.to_string(),
        },
    }
}

impl Handle {
    pub(crate) fn open(pid: u32, write: bool) -> Result<Self> {
        let path = format!("/proc/{pid}/mem");
        let mem = OpenOptions::new()
            .read(true)
            .write(write)
            .open(&path)
            .map_err(|e| map_open_error(pid, &e, "open process"))?;
        Ok(Self {
            pid,
            mem,
            writable: write,
        })
    }

    /// Fast path: one `process_vm_readv` syscall. Returns bytes copied.
    fn read_vm(&self, address: u64, buf: &mut [u8]) -> usize {
        let local = libc::iovec {
            iov_base: buf.as_mut_ptr().cast(),
            iov_len: buf.len(),
        };
        let remote = libc::iovec {
            iov_base: address as usize as *mut libc::c_void,
            iov_len: buf.len(),
        };
        // SAFETY: `local` describes `buf`, which is valid for `buf.len()`
        // writable bytes for the duration of the call; `remote` refers to the
        // other process and is only dereferenced by the kernel.
        let n = unsafe {
            libc::process_vm_readv(
                self.pid as libc::pid_t,
                &raw const local,
                1,
                &raw const remote,
                1,
                0,
            )
        };
        usize::try_from(n).unwrap_or(0)
    }

    pub(crate) fn read(&self, address: u64, buf: &mut [u8]) -> Result<()> {
        let mut done = self.read_vm(address, buf);
        // Slow path: `/proc/<pid>/mem` also reads pages without PROT_READ
        // (ptrace semantics) and works where process_vm_readv is disabled.
        while done < buf.len() {
            let pos = address + done as u64;
            if pos >= i64::MAX as u64 {
                return Err(Error::Unmapped { address: pos });
            }
            match self.mem.read_at(&mut buf[done..], pos) {
                Ok(0) => return Err(Error::Unmapped { address: pos }),
                Ok(n) => done += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(self.map_rw_error(pos, &e, "read memory")),
            }
        }
        Ok(())
    }

    pub(crate) fn write(&self, address: u64, data: &[u8]) -> Result<()> {
        if !self.writable {
            return Err(Error::unsupported("process was opened read-only"));
        }
        // `/proc/<pid>/mem` writes go through FOLL_FORCE, so read-only and
        // code pages can be patched without changing their protection.
        let mut done = 0usize;
        while done < data.len() {
            let pos = address + done as u64;
            if pos >= i64::MAX as u64 {
                return Err(Error::Unmapped { address: pos });
            }
            match self.mem.write_at(&data[done..], pos) {
                Ok(0) => return Err(Error::Unmapped { address: pos }),
                Ok(n) => done += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(self.map_rw_error(pos, &e, "write memory")),
            }
        }
        Ok(())
    }

    fn map_rw_error(&self, address: u64, e: &io::Error, operation: &str) -> Error {
        match e.raw_os_error() {
            Some(libc::EIO | libc::EFAULT | libc::EINVAL) => Error::Unmapped { address },
            Some(libc::ESRCH) => Error::NoSuchProcess { pid: self.pid },
            _ => map_open_error(self.pid, e, operation),
        }
    }

    pub(crate) fn regions(&self) -> Result<Vec<MemoryRegion>> {
        let text = std::fs::read_to_string(format!("/proc/{}/maps", self.pid))
            .map_err(|e| map_open_error(self.pid, &e, "read memory map"))?;
        let mut regions = parse_proc_maps(&text);
        regions.retain(|r| r.start < KERNEL_SPACE);
        Ok(regions)
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

pub(crate) fn executable_path(pid: u32) -> Option<String> {
    std::fs::read_link(format!("/proc/{pid}/exe"))
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

pub(crate) fn process_arch(pid: u32) -> Option<(Arch, u8)> {
    let file = File::open(format!("/proc/{pid}/exe")).ok()?;
    let mut header = [0u8; 20];
    file.read_exact_at(&mut header, 0).ok()?;
    elf_arch(&header)
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
