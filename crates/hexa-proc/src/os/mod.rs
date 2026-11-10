//! Per-OS back ends. Each module exposes the same items:
//!
//! * `Handle` — an opened process: `open`, `read`, `write`, `regions`,
//!   `max_address`;
//! * `executable_path`, `process_arch`, `is_elevated`,
//!   `debug_privilege_enabled`, `enable_debug_privilege`.
//!
//! All `unsafe` code of the crate lives here, next to the FFI call it wraps.

#[cfg(any(target_os = "linux", target_os = "android"))]
mod linux;
#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) use linux::*;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub(crate) use macos::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::*;

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    windows
)))]
mod other;
#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    windows
)))]
pub(crate) use other::*;

/// Default exclusive end of the user address space for a pointer width.
pub(crate) fn default_user_limit(pointer_size: u8) -> u64 {
    if pointer_size <= 4 {
        1 << 32
    } else if cfg!(target_arch = "aarch64") && !cfg!(target_os = "macos") {
        1 << 48
    } else {
        1 << 47
    }
}
