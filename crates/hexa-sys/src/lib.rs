//! System-level providers for Hexa: disks, partitions, file systems, disk
//! images, wiping, RAID reconstruction and alternate data streams.
//!
//! Spec coverage: DISK-01…DISK-12, FILE-14, FILE-17 and NFR-10. Unsafe code
//! is limited to OS calls (device ioctls, Windows stream enumeration) and
//! every block documents why it is sound.

pub mod carve;
pub mod clone;
pub mod concat;
pub mod device;
pub mod disks;
pub mod fs;
pub mod guard;
pub mod helper;
pub mod image;
pub mod partition;
pub mod raid;
pub mod shred;
pub mod streams;
mod util;
pub mod wipe;

pub use concat::{ConcatProvider, Segment};
pub use device::{DeviceGeometry, DeviceOpenOptions, DeviceProvider};
pub use disks::{DiskInfo, VolumeInfo, list_disks};
pub use fs::{FileEntry, FileSystem, FsInfo, FsKind, FsMap, open_filesystem, probe_filesystem};
pub use guard::{UndoJournal, WriteGuard};
pub use helper::{HelperClient, HelperConnection, helper_serve};
pub use image::{ImageFormat, create_e01, detect_format, open_image};
pub use partition::{Partition, PartitionTable, read_partition_table};
pub use raid::{RaidConfig, RaidLevel, RaidProvider};
