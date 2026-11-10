//! Process memory for Hexa: process list, memory maps, read/write, value
//! scanner, pointer scan, GDB remote (spec section "Process memory and
//! debugging", PROC-01…PROC-10).
//!
//! | Spec | Module |
//! |------|--------|
//! | PROC-01 process list | [`process`] |
//! | PROC-02 memory regions | [`region`], [`source::ProcessMemory::regions`] |
//! | PROC-03 address-space provider | [`provider`] |
//! | PROC-04 read/write, freezing | [`source`], [`freezer`] |
//! | PROC-05 live view | [`provider::changed_bytes`] |
//! | PROC-06 value scanner | [`scan`] |
//! | PROC-07 pointer scan | [`pointer`] |
//! | PROC-08 dumps | [`dump`] |
//! | PROC-09 GDB remote, LLDB, Android | [`gdb`], [`remote`] |
//! | PROC-10 permission diagnostics | [`diagnostics`] |
//!
//! Everything above the OS layer works on the [`MemorySource`] trait, so it
//! runs the same on a local process, a GDB remote target or an in-memory
//! [`MemorySnapshot`]. All `unsafe` code is confined to the private `os`
//! module.

pub mod diagnostics;
pub mod dump;
pub mod error;
pub mod freezer;
pub mod gdb;
mod os;
pub mod pointer;
pub mod process;
pub mod provider;
pub mod region;
pub mod remote;
pub mod scan;
pub mod source;

pub use diagnostics::{AccessReport, Diagnostic, Platform, Privileges};
pub use error::{Error, Result, is_unmapped, proc_error};
pub use freezer::{Freezer, FrozenValue};
pub use gdb::{GdbClient, GdbSource};
pub use pointer::{PointerChain, PointerMap, PointerScanOptions};
pub use process::{Arch, ProcessFilter, ProcessInfo, list_processes, process_info};
pub use provider::{AddressSpaceProvider, changed_bytes};
pub use region::{MemoryRegion, Module, Protection, RegionFilter, RegionKind};
pub use scan::{FirstScan, NextScan, Num, ScanOptions, Scanner, ValueType};
pub use source::{Access, MemorySnapshot, MemorySource, ProcessMemory};
