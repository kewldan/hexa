//! Comparison, merging and binary patches (DIFF-01…12, IO-05).

use std::path::PathBuf;

use hexa_core::marks::{Mark, MarkKind};
use hexa_core::{Cancel, Change, Document, Tree};
use hexa_diff::{
    ApplyOptions, ConflictResolution, CreateOptions, DiffOptions, DiffResult, ExportFormat,
    MergeOptions, MergeResult, PatchFormat, PatchInfo, Region,
};
use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State};

use super::doc::{EditResult, mutate, opened_doc};
use crate::error::{CmdError, CmdResult};
use crate::state::{AppState, DocInfo};

/// Largest patch file read into memory.
const MAX_PATCH: u64 = 1 << 30;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum DiffEvent {
    Progress { done: u64, total: u64 },
    Done { result: DiffResult },
    Error { error: CmdError },
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct RangeIn {
    pub start: u64,
    pub end: u64,
}

fn tree_of(state: &AppState, id: u64) -> CmdResult<Tree> {
    Ok(state.get(id)?.view.read().tree.clone())
}

/// Compares two documents (or ranges of them) in the background (DIFF-01…04).
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn diff_run(
    app: AppHandle,
    state: State<'_, AppState>,
    a: u64,
    b: u64,
    opts: Option<DiffOptions>,
    range_a: Option<RangeIn>,
    range_b: Option<RangeIn>,
    channel: Channel<DiffEvent>,
) -> CmdResult<u64> {
    let ta = tree_of(&state, a)?;
    let tb = tree_of(&state, b)?;
    let opts = opts.unwrap_or_default();
    let (job, cancel) = state.start_job();
    std::thread::spawn(move || {
        let progress = |done: u64, total: u64| {
            let _ = channel.send(DiffEvent::Progress { done, total });
        };
        let r = match (range_a, range_b) {
            (None, None) => hexa_diff::diff(&ta, &tb, &opts, &cancel, &progress),
            (ra, rb) => {
                let ra = ra.map_or(0..ta.len(), |r| r.start..r.end);
                let rb = rb.map_or(0..tb.len(), |r| r.start..r.end);
                hexa_diff::diff_ranges(&ta, ra, &tb, rb, &opts, &cancel, &progress)
            }
        };
        let _ = channel.send(match r {
            Ok(result) => DiffEvent::Done { result },
            Err(e) => DiffEvent::Error { error: e.into() },
        });
        app.state::<AppState>().end_job(job);
    });
    Ok(job)
}

/// Text, CSV or JSON report of a comparison (DIFF-07).
#[tauri::command]
pub fn diff_export(
    result: DiffResult,
    format: ExportFormat,
    path: Option<PathBuf>,
) -> CmdResult<String> {
    let text = hexa_diff::export_string(&result, format);
    if let Some(p) = path {
        std::fs::write(p, &text)?;
    }
    Ok(text)
}

/// Copies regions of `from` over the matching regions of `to` (DIFF-09).
#[tauri::command]
pub fn diff_apply(
    state: State<'_, AppState>,
    to: u64,
    from: u64,
    mut regions: Vec<Region>,
) -> CmdResult<EditResult> {
    let src = tree_of(&state, from)?;
    regions.sort_by_key(|r| r.a.start);
    if regions.is_empty() {
        return Err(CmdError::invalid("nothing to apply"));
    }
    let start = regions[0].a.start;
    let end = regions.iter().map(|r| r.a.end()).max().unwrap_or(start);
    let delta: i128 = regions
        .iter()
        .map(|r| i128::from(r.b.len) - i128::from(r.a.len))
        .sum();
    mutate(&state, to, |d| {
        d.transaction("Merge regions", |d| {
            for r in regions.iter().rev() {
                d.replace_tree(
                    r.a.start,
                    r.a.end(),
                    &src.slice(r.b.start, r.b.end()),
                    "Merge",
                )?;
            }
            Ok(())
        })?;
        Ok(Some(Change {
            start,
            removed: end - start,
            inserted: (i128::from(end - start) + delta) as u64,
        }))
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeOut {
    pub info: DocInfo,
    pub result: MergeResult,
}

/// Three-way merge into a new buffer; conflicts become annotations (DIFF-09).
#[tauri::command]
pub async fn merge3(
    app: AppHandle,
    state: State<'_, AppState>,
    base: u64,
    ours: u64,
    theirs: u64,
    conflict: Option<ConflictResolution>,
) -> CmdResult<MergeOut> {
    let state = state.inner();
    let (tb, to, tt) = (
        tree_of(state, base)?,
        tree_of(state, ours)?,
        tree_of(state, theirs)?,
    );
    let (job, cancel) = state.start_job();
    let opts = MergeOptions {
        conflict: conflict.unwrap_or_default(),
        ..MergeOptions::default()
    };
    let r = tauri::async_runtime::spawn_blocking(move || {
        hexa_diff::merge3(&tb, &to, &tt, &opts, &cancel, &|_, _| {})
            .map(|res| (res.to_tree(&tb, &to, &tt), res))
    })
    .await
    .map_err(|e| CmdError::new("internal", e.to_string()))?;
    state.end_job(job);
    let (tree, result) = r?;
    let mut doc = Document::from_tree("Merged", tree);
    for (i, c) in result.conflicts.iter().enumerate() {
        doc.add_mark(Mark {
            kind: MarkKind::Annotation,
            start: c.output.start,
            end: c.output.end(),
            name: format!("Конфликт {}", i + 1),
            color: "red".into(),
            note: format!(
                "База 0x{:X}+{}, наши 0x{:X}+{}, их 0x{:X}+{}",
                c.base.start, c.base.len, c.ours.start, c.ours.len, c.theirs.start, c.theirs.len
            ),
            ..Mark::default()
        });
    }
    let info = opened_doc(&app, state, doc);
    Ok(MergeOut { info, result })
}

fn read_patch(path: &PathBuf) -> CmdResult<Vec<u8>> {
    if std::fs::metadata(path)?.len() > MAX_PATCH {
        return Err(CmdError::invalid("patch file is too large"));
    }
    Ok(std::fs::read(path)?)
}

#[tauri::command]
pub fn patch_inspect(path: PathBuf) -> CmdResult<PatchInfo> {
    Ok(hexa_diff::inspect_patch(&read_patch(&path)?)?)
}

/// Writes a patch that turns `source` into `target` (DIFF-12, IO-05).
#[tauri::command]
pub async fn patch_create(
    state: State<'_, AppState>,
    source: u64,
    target: u64,
    format: PatchFormat,
    path: PathBuf,
    opts: Option<CreateOptions>,
) -> CmdResult<u64> {
    let state = state.inner();
    let (ts, tt) = (tree_of(state, source)?, tree_of(state, target)?);
    let (job, cancel) = state.start_job();
    let opts = opts.unwrap_or_default();
    let r = tauri::async_runtime::spawn_blocking(move || {
        let bytes = hexa_diff::create_patch(format, &ts, &tt, &opts, &cancel, &|_, _| {})?;
        std::fs::write(&path, &bytes)?;
        Ok::<_, hexa_core::Error>(bytes.len() as u64)
    })
    .await
    .map_err(|e| CmdError::new("internal", e.to_string()))?;
    state.end_job(job);
    Ok(r?)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchApplied {
    pub info: DocInfo,
    pub format: PatchFormat,
    pub reversed: bool,
}

/// Applies a patch file to a document; the result opens as a new buffer.
#[tauri::command]
pub async fn patch_apply(
    app: AppHandle,
    state: State<'_, AppState>,
    id: u64,
    path: PathBuf,
    format: Option<PatchFormat>,
    verify: Option<bool>,
) -> CmdResult<PatchApplied> {
    let state = state.inner();
    let src = tree_of(state, id)?;
    let patch = read_patch(&path)?;
    let opts = ApplyOptions {
        verify: verify.unwrap_or(true),
    };
    let cancel = Cancel::default();
    let applied = tauri::async_runtime::spawn_blocking(move || {
        hexa_diff::apply_patch(format, &patch, &src, &opts, &cancel, &|_, _| {})
    })
    .await
    .map_err(|e| CmdError::new("internal", e.to_string()))??;
    let name = path
        .file_stem()
        .map_or_else(|| "Patched".into(), |s| s.to_string_lossy().into_owned());
    let info = opened_doc(&app, state, Document::from_tree(name, applied.target));
    Ok(PatchApplied {
        info,
        format: applied.format,
        reversed: applied.reversed,
    })
}
