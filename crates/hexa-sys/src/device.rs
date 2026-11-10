//! Raw block devices as providers (DISK-02, DISK-03).
//!
//! Raw devices only accept I/O at sector-aligned offsets and lengths, so every
//! read is widened to whole sectors and unaligned writes are done as
//! read-modify-write of the covering sectors. Devices open read-only unless
//! writing is requested explicitly.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use hexa_core::provider::{Capabilities, Provider};
use parking_lot::Mutex;

use crate::util::{check_range, pread, pwrite};

/// Sector size used when the OS cannot tell (regular files, images).
pub const DEFAULT_SECTOR_SIZE: u32 = 512;

/// How to open a raw device.
#[derive(Debug, Clone, Copy, Default)]
pub struct DeviceOpenOptions {
    /// Open for writing (DISK-03). Off by default.
    pub writable: bool,
    /// Overrides the detected sector size.
    pub sector_size: Option<u32>,
}

/// Size and logical sector size of a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceGeometry {
    pub size: u64,
    pub sector_size: u32,
}

/// A raw disk, partition or regular file accessed in whole sectors.
#[derive(Debug)]
pub struct DeviceProvider {
    file: File,
    path: PathBuf,
    len: u64,
    sector_size: u32,
    writable: bool,
    /// Serialises read-modify-write cycles.
    rmw: Mutex<()>,
}

impl DeviceProvider {
    /// Opens `path` (`/dev/rdisk2`, `/dev/sdb`, `\\.\PhysicalDrive1`, or a
    /// regular file for testing).
    pub fn open(path: impl AsRef<Path>, opts: DeviceOpenOptions) -> io::Result<Self> {
        let path = path.as_ref();
        let file = OpenOptions::new()
            .read(true)
            .write(opts.writable)
            .open(path)?;
        let geometry = query_geometry(&file)?;
        let sector_size = opts.sector_size.unwrap_or(geometry.sector_size);
        if sector_size == 0 || !sector_size.is_power_of_two() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid sector size {sector_size}"),
            ));
        }
        Ok(Self {
            file,
            path: path.to_path_buf(),
            len: geometry.size,
            sector_size,
            writable: opts.writable,
            rmw: Mutex::new(()),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn sector_size(&self) -> u32 {
        self.sector_size
    }

    /// `[start, end)` widened to whole sectors and clamped to the device.
    fn aligned(&self, offset: u64, len: usize) -> (u64, u64) {
        let ss = u64::from(self.sector_size);
        let start = offset / ss * ss;
        let end = (offset + len as u64).div_ceil(ss) * ss;
        (start, end.min(self.len))
    }
}

impl Provider for DeviceProvider {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        check_range(offset, buf.len(), self.len)?;
        if buf.is_empty() {
            return Ok(());
        }
        let (start, end) = self.aligned(offset, buf.len());
        if start == offset && end == offset + buf.len() as u64 {
            return pread(&self.file, offset, buf);
        }
        let mut tmp = vec![0u8; (end - start) as usize];
        pread(&self.file, start, &mut tmp)?;
        let at = (offset - start) as usize;
        buf.copy_from_slice(&tmp[at..at + buf.len()]);
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            readable: true,
            writable: self.writable,
            resizable: false,
            sector_size: Some(self.sector_size),
            live: false,
        }
    }

    fn name(&self) -> String {
        self.path.display().to_string()
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> io::Result<()> {
        if !self.writable {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "device is open read-only",
            ));
        }
        check_range(offset, data.len(), self.len)?;
        if data.is_empty() {
            return Ok(());
        }
        let (start, end) = self.aligned(offset, data.len());
        if start == offset && end == offset + data.len() as u64 {
            return pwrite(&self.file, offset, data);
        }
        let _guard = self.rmw.lock();
        let mut tmp = vec![0u8; (end - start) as usize];
        pread(&self.file, start, &mut tmp)?;
        let at = (offset - start) as usize;
        tmp[at..at + data.len()].copy_from_slice(data);
        pwrite(&self.file, start, &tmp)
    }

    fn flush(&self) -> io::Result<()> {
        if self.writable {
            self.file.sync_all()?;
        }
        Ok(())
    }
}

/// Size and sector size of an open device or file (ioctl `DKIOCGETBLOCKCOUNT`
/// on macOS, `BLKGETSIZE64` on Linux, `IOCTL_DISK_GET_LENGTH_INFO` on Windows;
/// regular files report their length and 512-byte sectors).
pub fn query_geometry(file: &File) -> io::Result<DeviceGeometry> {
    let meta = file.metadata()?;
    if meta.is_file() {
        return Ok(DeviceGeometry {
            size: meta.len(),
            sector_size: DEFAULT_SECTOR_SIZE,
        });
    }
    os::geometry(file)
}

#[cfg(target_os = "macos")]
mod os {
    use super::DeviceGeometry;
    use std::fs::File;
    use std::io;
    use std::os::fd::AsRawFd;

    // _IOR('d', 24, uint32_t) and _IOR('d', 25, uint64_t) from <sys/disk.h>.
    const DKIOCGETBLOCKSIZE: libc::c_ulong = 0x4004_6418;
    const DKIOCGETBLOCKCOUNT: libc::c_ulong = 0x4008_6419;

    pub(super) fn geometry(file: &File) -> io::Result<DeviceGeometry> {
        let fd = file.as_raw_fd();
        let mut block_size: u32 = 0;
        let mut block_count: u64 = 0;
        // SAFETY: `fd` is a valid open descriptor owned by `file`, and each
        // ioctl writes exactly one integer of the pointed-to type.
        let r1 = unsafe { libc::ioctl(fd, DKIOCGETBLOCKSIZE, &raw mut block_size) };
        if r1 != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: as above.
        let r2 = unsafe { libc::ioctl(fd, DKIOCGETBLOCKCOUNT, &raw mut block_count) };
        if r2 != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(DeviceGeometry {
            size: block_count * u64::from(block_size),
            sector_size: block_size,
        })
    }
}

#[cfg(target_os = "linux")]
mod os {
    use super::DeviceGeometry;
    use std::fs::File;
    use std::io;
    use std::os::fd::AsRawFd;

    // _IOR(0x12, 114, size_t) and _IO(0x12, 104) from <linux/fs.h>.
    const BLKGETSIZE64: libc::c_ulong = 0x8008_1272;
    const BLKSSZGET: libc::c_ulong = 0x1268;

    pub(super) fn geometry(file: &File) -> io::Result<DeviceGeometry> {
        let fd = file.as_raw_fd();
        let mut size: u64 = 0;
        let mut sector: libc::c_int = 0;
        // SAFETY: `fd` is a valid open descriptor owned by `file`, and each
        // ioctl writes exactly one integer of the pointed-to type.
        let r1 = unsafe { libc::ioctl(fd, BLKGETSIZE64 as _, &raw mut size) };
        if r1 != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: as above.
        let r2 = unsafe { libc::ioctl(fd, BLKSSZGET as _, &raw mut sector) };
        let sector_size = if r2 == 0 && sector > 0 {
            sector as u32
        } else {
            super::DEFAULT_SECTOR_SIZE
        };
        Ok(DeviceGeometry { size, sector_size })
    }
}

#[cfg(windows)]
mod os {
    use super::DeviceGeometry;
    use std::ffi::c_void;
    use std::fs::File;
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{
        DISK_GEOMETRY, GET_LENGTH_INFORMATION, IOCTL_DISK_GET_DRIVE_GEOMETRY,
        IOCTL_DISK_GET_LENGTH_INFO,
    };

    /// Issues a buffered-output `DeviceIoControl` that fills one `T`.
    fn control<T: Default>(file: &File, code: u32) -> io::Result<T> {
        let mut out = T::default();
        let mut returned = 0u32;
        // SAFETY: the handle is owned by `file`; the output buffer is a valid,
        // writable `T` whose exact size is passed, and no input buffer is used.
        let ok = unsafe {
            DeviceIoControl(
                file.as_raw_handle(),
                code,
                std::ptr::null(),
                0,
                (&raw mut out).cast::<c_void>(),
                std::mem::size_of::<T>() as u32,
                &raw mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(out)
    }

    pub(super) fn geometry(file: &File) -> io::Result<DeviceGeometry> {
        let length: GET_LENGTH_INFORMATION = control(file, IOCTL_DISK_GET_LENGTH_INFO)?;
        let sector_size = control::<DISK_GEOMETRY>(file, IOCTL_DISK_GET_DRIVE_GEOMETRY)
            .map_or(super::DEFAULT_SECTOR_SIZE, |g| g.BytesPerSector);
        Ok(DeviceGeometry {
            size: length.Length as u64,
            sector_size,
        })
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod os {
    use super::DeviceGeometry;
    use std::fs::File;
    use std::io::{self, Seek, SeekFrom};

    pub(super) fn geometry(file: &File) -> io::Result<DeviceGeometry> {
        let mut f = file;
        let size = f.seek(SeekFrom::End(0))?;
        Ok(DeviceGeometry {
            size,
            sector_size: super::DEFAULT_SECTOR_SIZE,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn unaligned_io_on_file_backed_device() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        let data: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        tmp.write_all(&data).unwrap();
        let ro = DeviceProvider::open(tmp.path(), DeviceOpenOptions::default()).unwrap();
        assert_eq!(ro.len(), 4096);
        assert_eq!(ro.capabilities().sector_size, Some(512));
        let mut buf = [0u8; 700];
        ro.read_at(300, &mut buf).unwrap();
        assert_eq!(&buf[..], &data[300..1000]);
        assert!(ro.write_at(0, b"x").is_err());

        let rw = DeviceProvider::open(
            tmp.path(),
            DeviceOpenOptions {
                writable: true,
                sector_size: Some(1024),
            },
        )
        .unwrap();
        rw.write_at(1020, b"HELLO").unwrap();
        rw.write_at(2048, &[7u8; 1024]).unwrap();
        rw.flush().unwrap();
        let back = std::fs::read(tmp.path()).unwrap();
        assert_eq!(&back[1020..1025], b"HELLO");
        assert_eq!(&back[1015..1020], &data[1015..1020]);
        assert_eq!(&back[1025..1030], &data[1025..1030]);
        assert!(back[2048..3072].iter().all(|&b| b == 7));
    }
}
