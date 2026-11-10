//! Alternate streams of a file (FILE-17): extended attributes on macOS and
//! Linux (including the macOS resource fork `com.apple.ResourceFork`) and
//! NTFS Alternate Data Streams on Windows.

use std::io;
use std::path::Path;

use serde::Serialize;

/// Extended attribute name of the macOS resource fork.
pub const RESOURCE_FORK: &str = "com.apple.ResourceFork";

/// What kind of stream an entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StreamKind {
    ExtendedAttribute,
    ResourceFork,
    AlternateDataStream,
}

/// One alternate stream of a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamInfo {
    /// Attribute name, or ADS name without `:` and `:$DATA`.
    pub name: String,
    pub size: u64,
    pub kind: StreamKind,
}

/// Lists the alternate streams of `path` (the main data stream excluded).
pub fn list_streams(path: &Path) -> io::Result<Vec<StreamInfo>> {
    imp::list(path)
}

/// Reads a whole stream.
pub fn read_stream(path: &Path, name: &str) -> io::Result<Vec<u8>> {
    imp::read(path, name)
}

/// Creates or replaces a stream.
pub fn write_stream(path: &Path, name: &str, data: &[u8]) -> io::Result<()> {
    imp::write(path, name, data)
}

/// Removes a stream.
pub fn remove_stream(path: &Path, name: &str) -> io::Result<()> {
    imp::remove(path, name)
}

#[cfg(unix)]
mod imp {
    use super::{RESOURCE_FORK, StreamInfo, StreamKind};
    use std::io;
    use std::path::Path;

    pub(super) fn list(path: &Path) -> io::Result<Vec<StreamInfo>> {
        let mut out = Vec::new();
        for name in xattr::list(path)? {
            let name = name.to_string_lossy().into_owned();
            let size = xattr::get(path, &name)?.map_or(0, |v| v.len() as u64);
            let kind = if name == RESOURCE_FORK {
                StreamKind::ResourceFork
            } else {
                StreamKind::ExtendedAttribute
            };
            out.push(StreamInfo { name, size, kind });
        }
        Ok(out)
    }

    pub(super) fn read(path: &Path, name: &str) -> io::Result<Vec<u8>> {
        xattr::get(path, name)?
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("no attribute {name}")))
    }

    pub(super) fn write(path: &Path, name: &str, data: &[u8]) -> io::Result<()> {
        xattr::set(path, name, data)
    }

    pub(super) fn remove(path: &Path, name: &str) -> io::Result<()> {
        xattr::remove(path, name)
    }
}

#[cfg(windows)]
mod imp {
    use super::{StreamInfo, StreamKind};
    use std::ffi::OsString;
    use std::io;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::{Path, PathBuf};
    use windows_sys::Win32::Foundation::{ERROR_HANDLE_EOF, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        FindClose, FindFirstStreamW, FindNextStreamW, FindStreamInfoStandard,
        WIN32_FIND_STREAM_DATA,
    };

    fn stream_path(path: &Path, name: &str) -> io::Result<PathBuf> {
        if name.is_empty() || name.contains(['\\', '/', ':']) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid stream name",
            ));
        }
        let mut s = path.as_os_str().to_owned();
        s.push(":");
        s.push(name);
        Ok(PathBuf::from(s))
    }

    /// `":name:$DATA"` → `name`; the unnamed main stream yields `None`.
    fn stream_name(data: &WIN32_FIND_STREAM_DATA) -> Option<String> {
        let len = data
            .cStreamName
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(data.cStreamName.len());
        let full = OsString::from_wide(&data.cStreamName[..len])
            .to_string_lossy()
            .into_owned();
        let inner = full.strip_prefix(':')?;
        let name = inner.strip_suffix(":$DATA").unwrap_or(inner);
        (!name.is_empty()).then(|| name.to_string())
    }

    pub(super) fn list(path: &Path) -> io::Result<Vec<StreamInfo>> {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: WIN32_FIND_STREAM_DATA is plain old data; all-zero is valid.
        let mut data: WIN32_FIND_STREAM_DATA = unsafe { std::mem::zeroed() };
        // SAFETY: `wide` is NUL-terminated and outlives the call; `data` is a
        // valid out buffer of the type FindStreamInfoStandard expects.
        let handle = unsafe {
            FindFirstStreamW(
                wide.as_ptr(),
                FindStreamInfoStandard,
                (&raw mut data).cast(),
                0,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            let err = io::Error::last_os_error();
            return if err.raw_os_error() == Some(ERROR_HANDLE_EOF as i32) {
                Ok(Vec::new())
            } else {
                Err(err)
            };
        }
        let mut out = Vec::new();
        loop {
            if let Some(name) = stream_name(&data) {
                out.push(StreamInfo {
                    name,
                    size: data.StreamSize as u64,
                    kind: StreamKind::AlternateDataStream,
                });
            }
            // SAFETY: `handle` is a live find handle and `data` a valid buffer.
            if unsafe { FindNextStreamW(handle, (&raw mut data).cast()) } == 0 {
                break;
            }
        }
        // SAFETY: `handle` came from FindFirstStreamW and is closed once.
        unsafe { FindClose(handle) };
        Ok(out)
    }

    pub(super) fn read(path: &Path, name: &str) -> io::Result<Vec<u8>> {
        std::fs::read(stream_path(path, name)?)
    }

    pub(super) fn write(path: &Path, name: &str, data: &[u8]) -> io::Result<()> {
        std::fs::write(stream_path(path, name)?, data)
    }

    pub(super) fn remove(path: &Path, name: &str) -> io::Result<()> {
        std::fs::remove_file(stream_path(path, name)?)
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use super::StreamInfo;
    use std::io;
    use std::path::Path;

    fn unsupported() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "alternate streams are not supported here",
        )
    }

    pub(super) fn list(_: &Path) -> io::Result<Vec<StreamInfo>> {
        Err(unsupported())
    }
    pub(super) fn read(_: &Path, _: &str) -> io::Result<Vec<u8>> {
        Err(unsupported())
    }
    pub(super) fn write(_: &Path, _: &str, _: &[u8]) -> io::Result<()> {
        Err(unsupported())
    }
    pub(super) fn remove(_: &Path, _: &str) -> io::Result<()> {
        Err(unsupported())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        std::fs::write(&path, b"main").unwrap();
        let name = if cfg!(target_os = "linux") {
            "user.hexa.test"
        } else {
            "hexa.test"
        };
        match write_stream(&path, name, b"stream data") {
            Ok(()) => {}
            // Some file systems (tmpfs on old kernels, FAT) lack streams.
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::Unsupported | io::ErrorKind::PermissionDenied
                ) =>
            {
                return;
            }
            Err(e) if e.raw_os_error().is_some() && cfg!(target_os = "linux") => return,
            Err(e) => panic!("{e}"),
        }
        let list = list_streams(&path).unwrap();
        let s = list.iter().find(|s| s.name == name).unwrap();
        assert_eq!(s.size, 11);
        assert_eq!(read_stream(&path, name).unwrap(), b"stream data");
        assert_eq!(std::fs::read(&path).unwrap(), b"main");
        remove_stream(&path, name).unwrap();
        assert!(!list_streams(&path).unwrap().iter().any(|s| s.name == name));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn resource_fork_is_classified() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r");
        std::fs::write(&path, b"").unwrap();
        write_stream(&path, RESOURCE_FORK, b"rsrc").unwrap();
        let list = list_streams(&path).unwrap();
        assert!(
            list.iter()
                .any(|s| s.kind == StreamKind::ResourceFork && s.size == 4)
        );
    }
}
