//! File shredder (FILE-14): overwrite a file in N passes with `fsync` after
//! each, rename it to a random name, then delete it.

use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use hexa_core::task::Progress;
use hexa_core::{Cancel, Result};
use serde::{Deserialize, Serialize};

use crate::util::{Rng, check_cancel, invalid};
use crate::wipe::{PassStream, WipeMethod, WipePass};

/// Shredding parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShredOptions {
    pub passes: Vec<WipePass>,
    /// Rename to a random name before deleting (hides the original name in
    /// the directory entry).
    pub rename: bool,
    pub delete: bool,
}

impl Default for ShredOptions {
    fn default() -> Self {
        Self {
            passes: WipeMethod::Dod3Pass.passes(),
            rename: true,
            delete: true,
        }
    }
}

/// What the shredder did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShredReport {
    pub passes: u32,
    pub bytes_per_pass: u64,
    /// Name the file had right before deletion (or now, when kept).
    pub final_path: PathBuf,
    pub deleted: bool,
}

fn random_name(len: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = Rng::from_seed(Rng::os_seed());
    (0..len.clamp(8, 64))
        .map(|_| ALPHABET[(rng.next_u64() % ALPHABET.len() as u64) as usize] as char)
        .collect()
}

/// Shreds `path` (FILE-14).
pub fn shred_file(
    path: &Path,
    opts: &ShredOptions,
    progress: Progress<'_>,
    cancel: Option<&Cancel>,
) -> Result<ShredReport> {
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_file() {
        return Err(invalid("only regular files can be shredded"));
    }
    if opts.passes.is_empty() {
        return Err(invalid("no shred passes"));
    }
    let len = meta.len();
    let total = len * opts.passes.len() as u64;
    let mut file = OpenOptions::new().write(true).open(path)?;
    let mut buf = vec![0u8; 1 << 20];
    let mut done = 0u64;
    for pass in &opts.passes {
        let mut stream = PassStream::new(pass.clone());
        file.seek(SeekFrom::Start(0))?;
        let mut rel = 0u64;
        while rel < len {
            check_cancel(cancel)?;
            let n = ((len - rel) as usize).min(buf.len());
            stream.fill(rel, &mut buf[..n]);
            file.write_all(&buf[..n])?;
            rel += n as u64;
            done += n as u64;
            progress(done, total);
        }
        file.sync_all()?;
    }
    drop(file);
    let mut final_path = path.to_path_buf();
    if opts.rename {
        let name_len = path.file_name().map_or(12, std::ffi::OsStr::len);
        let renamed = path.with_file_name(random_name(name_len));
        std::fs::rename(path, &renamed)?;
        final_path = renamed;
    }
    if opts.delete {
        std::fs::remove_file(&final_path)?;
    }
    Ok(ShredReport {
        passes: opts.passes.len() as u32,
        bytes_per_pass: len,
        final_path,
        deleted: opts.delete,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hexa_core::task::no_progress;

    #[test]
    fn overwrites_renames_and_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.txt");
        std::fs::write(&path, vec![b'S'; 5000]).unwrap();
        let keep = ShredOptions {
            passes: vec![WipePass::Random, WipePass::Fill { byte: 0x5A }],
            rename: true,
            delete: false,
        };
        let r = shred_file(&path, &keep, &no_progress, None).unwrap();
        assert!(!path.exists());
        assert_eq!(std::fs::read(&r.final_path).unwrap(), vec![0x5A; 5000]);
        assert_ne!(r.final_path.file_name().unwrap(), "secret.txt");

        let r = shred_file(&r.final_path, &ShredOptions::default(), &no_progress, None).unwrap();
        assert!(r.deleted && !r.final_path.exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
