//! System-level providers for Hexa: disks, partitions, file systems, disk
//! images, wiping, RAID reconstruction and alternate data streams.
//!
//! Spec coverage: DISK-01…DISK-12, FILE-14, FILE-17 and NFR-10. Unsafe code
//! is limited to OS calls (device ioctls, Windows stream enumeration) and
//! every block documents why it is sound.

pub mod concat;
pub mod device;
pub mod disks;
pub mod helper;
pub mod partition;
mod util;

pub use concat::{ConcatProvider, Segment};
pub use device::{DeviceGeometry, DeviceOpenOptions, DeviceProvider};
pub use helper::{HelperClient, HelperConnection, helper_serve};
