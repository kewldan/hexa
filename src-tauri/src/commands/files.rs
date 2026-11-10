//! Multi-file operations: find in files and open tabs (SRCH-23, SRCH-24),
//! split and concatenate (FILE-13).

use std::path::{Path, PathBuf};

use hexa_core::search::{self, Query, Scope};
use hexa_core::{Document, EditMode, OpenOptions};
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State};

use super::doc::opened_doc;
use crate::error::{CmdError, CmdResult};
use crate::state::{AppState, DocInfo};

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum FilesEvent {
    Hits {
        path: String,
        offsets: Vec<u64>,
        len: u64,
    },
    Progress {
        files: u64,
        current: String,
    },
    Skipped {
        path: String,
        reason: String,
    },
    Done {
        files: u64,
        hits: u64,
        truncated: bool,
    },
}

/// `*` and `?` wildcards, case-insensitive; `;`/`,` separate alternatives.
fn mask_matches(mask: &str, name: &str) -> bool {
    fn rec(p: &[char], s: &[char]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some('*') => (0..=s.len()).any(|i| rec(&p[1..], &s[i..])),
            Some('?') => !s.is_empty() && rec(&p[1..], &s[1..]),
            Some(c) => s.first().is_some_and(|x| x == c) && rec(&p[1..], &s[1..]),
        }
    }
    let name: Vec<char> = name.to_lowercase().chars().collect();
    mask.split([';', ','])
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .any(|m| rec(&m.to_lowercase().chars().collect::<Vec<_>>(), &name))
        || mask.trim().is_empty()
}

fn collect(dir: &Path, mask: &str, recursive: bool, out: &mut Vec<PathBuf>, limit: usize) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for e in entries {
        if out.len() >= limit {
            return;
        }
        let Ok(ft) = e.file_type() else { continue };
        let p = e.path();
        if ft.is_dir() {
            if recursive && !ft.is_symlink() {
                collect(&p, mask, recursive, out, limit);
            }
        } else if ft.is_file() && mask_matches(mask, &e.file_name().to_string_lossy()) {
            out.push(p);
        }
    }
}

/// Searches every matching file under `root` (SRCH-23).
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn files_search(
    app: AppHandle,
    state: State<'_, AppState>,
    root: PathBuf,
    mask: String,
    recursive: bool,
    query: Query,
    max_per_file: Option<usize>,
    max_files: Option<usize>,
    channel: Channel<FilesEvent>,
) -> CmdResult<u64> {
    let matcher = search::compile(&query)?;
    let (job, cancel) = state.start_job();
    std::thread::spawn(move || {
        let mut paths = Vec::new();
        let limit = max_files.unwrap_or(100_000);
        collect(&root, &mask, recursive, &mut paths, limit + 1);
        let truncated = paths.len() > limit;
        paths.truncate(limit);
        let per_file = max_per_file.unwrap_or(1000);
        let (mut files, mut hits) = (0u64, 0u64);
        for p in paths {
            if cancel.is_cancelled() {
                break;
            }
            files += 1;
            let shown = p.to_string_lossy().into_owned();
            if files % 16 == 1 {
                let _ = channel.send(FilesEvent::Progress {
                    files,
                    current: shown.clone(),
                });
            }
            let doc = match Document::open(
                &p,
                &OpenOptions {
                    mode: EditMode::ReadOnly,
                    ..OpenOptions::default()
                },
            ) {
                Ok(d) => d,
                Err(e) => {
                    let _ = channel.send(FilesEvent::Skipped {
                        path: shown,
                        reason: e.to_string(),
                    });
                    continue;
                }
            };
            let tree = doc.snapshot();
            let mut offsets = Vec::new();
            let mut len = 0;
            let r = search::find_all(
                &tree,
                &matcher,
                Scope::whole(&tree),
                per_file,
                &cancel,
                &hexa_core::task::no_progress,
                |m| {
                    len = m.len;
                    offsets.push(m.offset);
                },
            );
            if r.is_ok() && !offsets.is_empty() {
                hits += offsets.len() as u64;
                let _ = channel.send(FilesEvent::Hits {
                    path: shown,
                    offsets,
                    len,
                });
            }
        }
        let _ = channel.send(FilesEvent::Done {
            files,
            hits,
            truncated,
        });
        app.state::<AppState>().end_job(job);
    });
    Ok(job)
}

/// Writes `[start, end)` of a document as numbered parts of `part_size`
/// bytes next to `base` (`name.001`, `name.002` …) (FILE-13).
#[tauri::command]
pub async fn file_split(
    state: State<'_, AppState>,
    id: u64,
    part_size: u64,
    base: PathBuf,
) -> CmdResult<Vec<String>> {
    if part_size == 0 {
        return Err(CmdError::invalid("part size must be positive"));
    }
    let tree = state.get(id)?.view.read().tree.clone();
    let parts = tree.len().div_ceil(part_size);
    if parts > 10_000 {
        return Err(CmdError::invalid("more than 10 000 parts"));
    }
    let (job, cancel) = state.start_job();
    let r = tauri::async_runtime::spawn_blocking(move || {
        let mut out = Vec::new();
        let width = parts.to_string().len().max(3);
        let mut buf = vec![0u8; 4 << 20];
        for i in 0..parts {
            cancel.check()?;
            let path = PathBuf::from(format!("{}.{:0width$}", base.display(), i + 1));
            let mut f = std::io::BufWriter::new(std::fs::File::create(&path)?);
            let (mut pos, end) = (i * part_size, ((i + 1) * part_size).min(tree.len()));
            while pos < end {
                let n = ((end - pos) as usize).min(buf.len());
                tree.read(pos, &mut buf[..n])?;
                std::io::Write::write_all(&mut f, &buf[..n])?;
                pos += n as u64;
            }
            std::io::Write::flush(&mut f)?;
            out.push(path.to_string_lossy().into_owned());
        }
        Ok::<_, hexa_core::Error>(out)
    })
    .await
    .map_err(|e| CmdError::new("internal", e.to_string()))?;
    state.end_job(job);
    Ok(r?)
}

/// Opens the concatenation of several files as a new buffer; nothing is
/// copied until it is saved (FILE-13).
#[tauri::command]
pub fn files_concat(
    app: AppHandle,
    state: State<'_, AppState>,
    paths: Vec<PathBuf>,
) -> CmdResult<DocInfo> {
    let mut doc = Document::new_empty();
    for p in &paths {
        let pos = doc.len();
        doc.insert_file(pos, p)?;
    }
    Ok(opened_doc(&app, &state, doc))
}

#[cfg(test)]
mod tests {
    use super::mask_matches;

    #[test]
    fn masks() {
        assert!(mask_matches("*.bin", "FIRMWARE.BIN"));
        assert!(mask_matches("*.bin; *.rom", "a.rom"));
        assert!(mask_matches("", "anything"));
        assert!(mask_matches("data?.dat", "data1.dat"));
        assert!(!mask_matches("*.bin", "a.exe"));
    }
}
