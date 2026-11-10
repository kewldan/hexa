//! Fallback for operating systems without process-memory support.

use crate::error::{Error, Result};
use crate::process::Arch;
use crate::region::MemoryRegion;

#[derive(Debug)]
pub(crate) struct Handle;

fn unsupported() -> Error {
    Error::unsupported("process memory access is not implemented for this OS")
}

impl Handle {
    pub(crate) fn open(_pid: u32, _write: bool) -> Result<Self> {
        Err(unsupported())
    }

    pub(crate) fn read(&self, _address: u64, _buf: &mut [u8]) -> Result<()> {
        Err(unsupported())
    }

    pub(crate) fn write(&self, _address: u64, _data: &[u8]) -> Result<()> {
        Err(unsupported())
    }

    pub(crate) fn regions(&self) -> Result<Vec<MemoryRegion>> {
        Err(unsupported())
    }

    pub(crate) fn max_address(&self, pointer_size: u8) -> u64 {
        super::default_user_limit(pointer_size)
    }
}

pub(crate) fn executable_path(_pid: u32) -> Option<String> {
    None
}

pub(crate) fn process_arch(_pid: u32) -> Option<(Arch, u8)> {
    None
}

pub(crate) fn is_elevated() -> bool {
    false
}

pub(crate) fn debug_privilege_enabled() -> Option<bool> {
    None
}

#[allow(clippy::unnecessary_wraps)] // same signature as the Windows back end
pub(crate) fn enable_debug_privilege() -> Result<bool> {
    Ok(false)
}
