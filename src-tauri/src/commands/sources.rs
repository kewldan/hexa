//! Disks, disk images, partitions and file systems, archives, alternate
//! streams and remote sources (DISK-01…12, FILE-14…17, CORE-02).
//!
//! Every source becomes an ordinary document over its provider; this module
//! remembers what each document was opened from (`src_info`) so the UI can
//! offer the matching tools, and keeps per-document caches (partition
//! layout, opened file systems, sector maps, live captures).

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use hexa_core::provider::{Capabilities, Provider};
use hexa_core::task::Progress;
use hexa_core::{Cancel, ChangeKind, Document};
use hexa_net::archive::Archive;
use hexa_net::sftp::pinned_fingerprint;
use hexa_net::{
    ArchiveEntry, ArchiveKind, HttpOptions, HttpProvider, S3Options, S3Provider, SftpOptions,
    SftpProvider, StreamOptions, StreamProvider, StreamStatus,
};
use hexa_sys::carve::{self, CarveOptions, CarvedFile};
use hexa_sys::clone::{CloneOptions, clone_provider, clone_to_file};
use hexa_sys::fs::{Extent, Region};
use hexa_sys::image::E01Options;
use hexa_sys::shred::{ShredOptions, shred_file};
use hexa_sys::streams::{self, StreamInfo};
use hexa_sys::wipe::{WipeMethod, WipeOptions, wipe};
use hexa_sys::{
    DeviceOpenOptions, DeviceProvider, DiskInfo, FileEntry, FileSystem, FsInfo, FsMap,
    PartitionTable, RaidConfig, RaidProvider, UndoJournal, WriteGuard, create_e01, list_disks,
    open_filesystem, open_image, probe_filesystem, read_partition_table,
};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, Manager, State};

use super::doc::opened_doc;
use crate::error::{CmdError, CmdResult};
use crate::state::{AppState, DocEntry, DocInfo};

/// Largest document written back into an archive or a stream at once.
const MAX_REPACK: u64 = 1 << 30;
/// Largest change set committed to a source in one go.
const MAX_COMMIT: u64 = 256 << 20;
/// Bytes of each pending write shown in the preview.
const PREVIEW_BYTES: usize = 64;

// ---------------------------------------------------------------- registry

/// What a document was opened from.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceInfo {
    /// `disk`, `image`, `raid`, `archiveEntry`, `fileStream`, `capture`,
    /// `http`, `s3`, `sftp`, `process`, `gdb`.
    pub kind: &'static str,
    pub label: String,
    pub path: Option<String>,
    pub writable: bool,
    pub pid: Option<u32>,
}

impl SourceInfo {
    pub fn new(kind: &'static str, label: impl Into<String>) -> Self {
        Self {
            kind,
            label: label.into(),
            path: None,
            writable: false,
            pid: None,
        }
    }

    fn path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    fn writable(mut self, writable: bool) -> Self {
        self.writable = writable;
        self
    }
}

/// A file system opened on a volume of a document.
#[derive(Debug)]
pub struct Volume {
    /// Offset of the volume in the document.
    offset: u64,
    size: u64,
    name: String,
    fs: Box<dyn FileSystem>,
    map: RwLock<Option<FsMap>>,
}

type VolumeKey = (u64, Option<usize>);

/// Per-document state of the sources of this module.
#[derive(Debug, Default)]
pub struct SourceState {
    infos: Mutex<HashMap<u64, SourceInfo>>,
    tables: Mutex<HashMap<u64, Option<PartitionTable>>>,
    volumes: Mutex<HashMap<VolumeKey, Arc<Volume>>>,
    archive_entries: Mutex<HashMap<u64, (PathBuf, String)>>,
    file_streams: Mutex<HashMap<u64, (PathBuf, String)>>,
    captures: Mutex<HashMap<u64, Arc<StreamProvider>>>,
}

impl SourceState {
    fn forget(&self, id: u64) {
        self.infos.lock().remove(&id);
        self.tables.lock().remove(&id);
        self.volumes.lock().retain(|k, _| k.0 != id);
        self.archive_entries.lock().remove(&id);
        self.file_streams.lock().remove(&id);
        if let Some(c) = self.captures.lock().remove(&id) {
            c.stop();
        }
    }
}

/// Records where document `id` came from and drops the records once the
/// document is closed.
pub(crate) fn register(app: &AppHandle, id: u64, info: SourceInfo) {
    app.state::<SourceState>().infos.lock().insert(id, info);
    let app = app.clone();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(1));
            if app.state::<AppState>().get(id).is_err() {
                app.state::<SourceState>().forget(id);
                super::memory::forget(&app, id);
                break;
            }
        }
    });
}

/// Opens `provider` as a new document and records its source.
pub(crate) fn open_provider(
    app: &AppHandle,
    provider: Arc<dyn Provider>,
    fixed_size: bool,
    info: SourceInfo,
) -> DocInfo {
    let state = app.state::<AppState>();
    let doc = Document::from_provider(provider, fixed_size);
    let di = opened_doc(app, &state, doc);
    register(app, di.id, info);
    di
}

/// Tells every window that document `id` changed behind its back.
pub(crate) fn notify_updated(app: &AppHandle, id: u64) {
    let _ = app.emit("hexa://doc-updated", serde_json::json!({ "id": id }));
}

#[tauri::command]
pub fn src_info(sources: State<'_, SourceState>, id: u64) -> Option<SourceInfo> {
    sources.infos.lock().get(&id).cloned()
}

// ---------------------------------------------------------------- jobs

/// Events of a background job started by this module.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum JobEvent {
    Progress { done: u64, total: u64 },
    Done { result: Value },
    Error { error: CmdError },
}

struct Throttle {
    channel: Channel<JobEvent>,
    last: Mutex<Option<Instant>>,
}

impl Throttle {
    fn report(&self, done: u64, total: u64) {
        let mut last = self.last.lock();
        if done < total && last.is_some_and(|t| t.elapsed() < Duration::from_millis(60)) {
            return;
        }
        *last = Some(Instant::now());
        let _ = self.channel.send(JobEvent::Progress { done, total });
    }
}

/// Runs `f` on a new thread as a cancellable job; returns the job id.
pub(crate) fn run_job(
    app: &AppHandle,
    channel: Channel<JobEvent>,
    f: impl FnOnce(&Cancel, Progress<'_>) -> CmdResult<Value> + Send + 'static,
) -> u64 {
    let (job, cancel) = app.state::<AppState>().start_job();
    let app = app.clone();
    std::thread::spawn(move || {
        let throttle = Throttle {
            channel: channel.clone(),
            last: Mutex::new(None),
        };
        let progress = |done: u64, total: u64| throttle.report(done, total);
        let r = f(&cancel, &progress);
        let _ = channel.send(match r {
            Ok(result) => JobEvent::Done { result },
            Err(error) => JobEvent::Error { error },
        });
        app.state::<AppState>().end_job(job);
    });
    job
}

pub(crate) fn json<T: Serialize>(v: &T) -> CmdResult<Value> {
    serde_json::to_value(v).map_err(|e| CmdError::new("internal", e.to_string()))
}

/// Runs blocking work off the main thread.
pub(crate) async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> CmdResult<T> + Send + 'static,
) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| CmdError::new("internal", e.to_string()))?
}

fn net_err(e: hexa_net::Error) -> CmdError {
    use hexa_net::Error as E;
    let kind = match &e {
        E::AuthFailed { .. }
        | E::HttpStatus {
            status: 401 | 403, ..
        } => "auth",
        E::HttpStatus { status: 404, .. } | E::EntryNotFound(_) => "notFound",
        E::InvalidUrl(_) | E::InvalidKey(_) => "invalid",
        E::TooLarge { .. } => "tooLarge",
        E::Unsupported(_) => "unsupported",
        E::HostKeyRejected { .. } => "hostKey",
        E::Io(io) if io.kind() == io::ErrorKind::PermissionDenied => "permission",
        _ => "net",
    };
    CmdError::new(kind, e.to_string())
}

fn to_io(e: hexa_core::Error) -> io::Error {
    match e {
        hexa_core::Error::Io(e) => e,
        other => io::Error::other(other.to_string()),
    }
}

fn base_of(app: &AppHandle, id: u64) -> CmdResult<(Arc<DocEntry>, Arc<dyn Provider>)> {
    let entry = app.state::<AppState>().get(id)?;
    let base = entry.view.read().base.clone();
    Ok((entry, base))
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// Re-creates the document over its (changed) base and tells the windows.
fn reload_from_base(app: &AppHandle, entry: &DocEntry) -> DocInfo {
    let mut doc = entry.doc.lock();
    let base = doc.base().clone();
    let fixed = doc.fixed_size();
    let mode = doc.mode();
    base.invalidate();
    *doc = Document::from_provider(base, fixed);
    let _ = doc.set_mode(mode);
    entry.publish(&doc);
    let info = entry.info(&doc);
    drop(doc);
    notify_updated(app, entry.id);
    info
}

// ---------------------------------------------------------------- disks (DISK-01…03)

#[tauri::command]
pub async fn disk_list() -> CmdResult<Vec<DiskInfo>> {
    blocking(|| Ok(list_disks()?)).await
}

/// Opens a raw disk or partition; read-only unless `writable` (DISK-02/03).
#[tauri::command]
pub async fn disk_open(
    app: AppHandle,
    path: String,
    writable: bool,
    sector_size: Option<u32>,
) -> CmdResult<DocInfo> {
    blocking(move || {
        let dev = DeviceProvider::open(
            &path,
            DeviceOpenOptions {
                writable,
                sector_size,
            },
        )?;
        let info = SourceInfo::new("disk", path.clone())
            .path(path)
            .writable(writable);
        Ok(open_provider(&app, Arc::new(dev), true, info))
    })
    .await
}

/// Opens a disk image (VHD, VHDX, VMDK, QCOW2, DMG, E01, raw / split raw)
/// as the virtual disk it contains (DISK-08).
#[tauri::command]
pub async fn image_open(app: AppHandle, path: PathBuf) -> CmdResult<DocInfo> {
    blocking(move || {
        let provider = open_image(&path)?;
        let info = SourceInfo::new("image", file_name(&path)).path(path.to_string_lossy());
        Ok(open_provider(&app, provider, true, info))
    })
    .await
}

/// Assembles RAID 0/5 from open documents in array order; `None` marks a
/// missing RAID 5 member (DISK-11).
#[tauri::command]
pub fn raid_open(
    app: AppHandle,
    members: Vec<Option<u64>>,
    config: RaidConfig,
) -> CmdResult<DocInfo> {
    let providers = members
        .iter()
        .map(|m| m.map(|id| base_of(&app, id).map(|(_, b)| b)).transpose())
        .collect::<CmdResult<Vec<_>>>()?;
    let raid = RaidProvider::new(providers, config)?;
    let info = SourceInfo::new("raid", format!("RAID ({} дисков)", members.len()));
    Ok(open_provider(&app, Arc::new(raid), true, info))
}

// ---------------------------------------------------------------- partitions & file systems (DISK-04…07)

/// One browsable area: a partition or the whole document.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeView {
    /// Partition index; `None` = the whole document.
    pub index: Option<usize>,
    pub offset: u64,
    pub size: u64,
    pub name: String,
    pub fs: Option<FsInfo>,
    pub browsable: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskLayout {
    pub size: u64,
    pub table: Option<PartitionTable>,
    pub volumes: Vec<VolumeView>,
}

fn table_of(app: &AppHandle, id: u64, base: &dyn Provider) -> CmdResult<Option<PartitionTable>> {
    let sources = app.state::<SourceState>();
    if let Some(t) = sources.tables.lock().get(&id) {
        return Ok(t.clone());
    }
    let t = read_partition_table(base, None)?;
    sources.tables.lock().insert(id, t.clone());
    Ok(t)
}

fn volume_provider(
    base: &Arc<dyn Provider>,
    table: Option<&PartitionTable>,
    index: Option<usize>,
) -> CmdResult<(Arc<dyn Provider>, u64, String)> {
    match (table, index) {
        (_, None) => Ok((base.clone(), 0, "Весь диск".into())),
        (Some(t), Some(i)) => {
            let part = t
                .partitions
                .get(i)
                .ok_or_else(|| CmdError::invalid(format!("нет раздела {i}")))?;
            let p = PartitionTable::open_partition(base.clone(), part)?;
            let name = part
                .name
                .clone()
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| format!("Раздел {} · {}", part.number, part.type_name));
            Ok((Arc::new(p), part.offset, name))
        }
        (None, Some(_)) => Err(CmdError::invalid("таблица разделов не найдена")),
    }
}

/// Partition table and the file system of every partition (DISK-04/05).
#[tauri::command]
pub async fn disk_layout(app: AppHandle, id: u64) -> CmdResult<DiskLayout> {
    blocking(move || {
        let (_, base) = base_of(&app, id)?;
        let table = table_of(&app, id, base.as_ref())?;
        let mut volumes = Vec::new();
        let indices: Vec<Option<usize>> = match &table {
            Some(t) => (0..t.partitions.len()).map(Some).collect(),
            None => vec![None],
        };
        for index in indices {
            let (p, offset, name) = volume_provider(&base, table.as_ref(), index)?;
            let fs = probe_filesystem(&p).ok().flatten();
            volumes.push(VolumeView {
                index,
                offset,
                size: p.len(),
                name,
                browsable: fs.as_ref().is_some_and(|f| f.kind.browsable()),
                fs,
            });
        }
        Ok(DiskLayout {
            size: base.len(),
            table,
            volumes,
        })
    })
    .await
}

fn volume(app: &AppHandle, id: u64, index: Option<usize>) -> CmdResult<Arc<Volume>> {
    let sources = app.state::<SourceState>();
    if let Some(v) = sources.volumes.lock().get(&(id, index)) {
        return Ok(v.clone());
    }
    let (_, base) = base_of(app, id)?;
    let table = table_of(app, id, base.as_ref())?;
    let (p, offset, name) = volume_provider(&base, table.as_ref(), index)?;
    let fs = open_filesystem(&p)?;
    let v = Arc::new(Volume {
        offset,
        size: p.len(),
        name,
        fs,
        map: RwLock::new(None),
    });
    sources.volumes.lock().insert((id, index), v.clone());
    Ok(v)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FsListing {
    pub dir: FileEntry,
    pub entries: Vec<FileEntry>,
}

/// Lists a directory; `dir` of `None` = the root (DISK-05).
#[tauri::command]
pub async fn fs_list(
    app: AppHandle,
    id: u64,
    volume: Option<usize>,
    dir: Option<FileEntry>,
) -> CmdResult<FsListing> {
    blocking(move || {
        let v = self::volume(&app, id, volume)?;
        let dir = dir.unwrap_or_else(|| v.fs.root());
        let mut entries: Vec<FileEntry> =
            v.fs.list_dir(&dir)?
                .into_iter()
                .filter(|e| e.name != "." && e.name != "..")
                .collect();
        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok(FsListing { dir, entries })
    })
    .await
}

/// Deleted entries that can still be located (DISK-07).
#[tauri::command]
pub async fn fs_deleted(
    app: AppHandle,
    id: u64,
    volume: Option<usize>,
) -> CmdResult<Vec<FileEntry>> {
    blocking(move || Ok(self::volume(&app, id, volume)?.fs.deleted(None)?)).await
}

/// File → extents, with physical offsets relative to the document (DISK-06).
#[tauri::command]
pub async fn fs_extents(
    app: AppHandle,
    id: u64,
    volume: Option<usize>,
    file: FileEntry,
) -> CmdResult<Vec<Extent>> {
    blocking(move || {
        let v = self::volume(&app, id, volume)?;
        Ok(v.fs
            .extents(&file)?
            .into_iter()
            .map(|e| Extent {
                physical: e.physical.map(|p| p + v.offset),
                ..e
            })
            .collect())
    })
    .await
}

/// Builds the sector → owner map of a volume in the background (DISK-06).
#[tauri::command]
pub fn fs_map_build(
    app: AppHandle,
    id: u64,
    volume: Option<usize>,
    channel: Channel<JobEvent>,
) -> CmdResult<u64> {
    let v = self::volume(&app, id, volume)?;
    Ok(run_job(&app, channel, move |cancel, _| {
        let map = FsMap::build(v.fs.as_ref(), Some(cancel))?;
        let result = serde_json::json!({
            "files": map.file_regions().len(),
            "metadata": map.metadata_regions().len(),
        });
        *v.map.write() = Some(map);
        Ok(result)
    }))
}

/// Who owns the byte at `offset` (DISK-06).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnerInfo {
    pub volume: Option<usize>,
    pub volume_name: String,
    /// A map of the volume has been built.
    pub mapped: bool,
    /// Owning region, with `start` relative to the document.
    pub region: Option<Region>,
    pub detail: Option<String>,
}

#[tauri::command]
pub fn fs_owner_at(sources: State<'_, SourceState>, id: u64, offset: u64) -> Option<OwnerInfo> {
    let volumes: Vec<(Option<usize>, Arc<Volume>)> = sources
        .volumes
        .lock()
        .iter()
        .filter(|(k, _)| k.0 == id)
        .map(|(k, v)| (k.1, v.clone()))
        .collect();
    // Partitions win over a whole-disk volume.
    let best = volumes
        .iter()
        .filter(|(_, v)| offset >= v.offset && offset < v.offset + v.size)
        .min_by_key(|(_, v)| v.size);
    let (index, v) = best?;
    let rel = offset - v.offset;
    let map = v.map.read();
    let region = map.as_ref().and_then(|m| m.owner_at(rel)).map(|r| Region {
        start: r.start + v.offset,
        ..r.clone()
    });
    Some(OwnerInfo {
        volume: *index,
        volume_name: v.name.clone(),
        mapped: map.is_some(),
        detail: v.fs.describe_offset(rel),
        region,
    })
}

/// A file inside a file system, read through its extents.
#[derive(Debug)]
struct FsFile {
    volume: Arc<Volume>,
    entry: FileEntry,
}

impl Provider for FsFile {
    fn len(&self) -> u64 {
        self.entry.size
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let n = self
            .volume
            .fs
            .read_file(&self.entry, offset, buf)
            .map_err(to_io)?;
        if n < buf.len() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "read past the end of the file",
            ));
        }
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::READ_ONLY
    }

    fn name(&self) -> String {
        self.entry.name.clone()
    }
}

/// Opens a file of the file system as a read-only document (DISK-05/07).
#[tauri::command]
pub fn fs_open_file(
    app: AppHandle,
    id: u64,
    volume: Option<usize>,
    file: FileEntry,
) -> CmdResult<DocInfo> {
    let v = self::volume(&app, id, volume)?;
    let info = SourceInfo::new("fsFile", file.path.clone()).path(file.path.clone());
    Ok(open_provider(
        &app,
        Arc::new(FsFile {
            volume: v,
            entry: file,
        }),
        true,
        info,
    ))
}

/// Copies a file of the file system to `dest` (DISK-05/07).
#[tauri::command]
pub fn fs_extract(
    app: AppHandle,
    id: u64,
    volume: Option<usize>,
    file: FileEntry,
    dest: PathBuf,
    channel: Channel<JobEvent>,
) -> CmdResult<u64> {
    let v = self::volume(&app, id, volume)?;
    Ok(run_job(&app, channel, move |cancel, progress| {
        let src = FsFile {
            volume: v,
            entry: file,
        };
        let r = clone_to_file(
            &src,
            &dest,
            &CloneOptions::default(),
            progress,
            Some(cancel),
        )?;
        json(&r)
    }))
}

/// Signature carving over `[start, end)` (DISK-07).
#[tauri::command]
pub fn disk_carve(
    app: AppHandle,
    id: u64,
    start: u64,
    end: Option<u64>,
    alignment: u64,
    channel: Channel<JobEvent>,
) -> CmdResult<u64> {
    let (_, base) = base_of(&app, id)?;
    Ok(run_job(&app, channel, move |cancel, progress| {
        let opts = CarveOptions {
            start,
            end,
            alignment: alignment.max(1),
            max_results: 10_000,
        };
        let found: Vec<CarvedFile> = carve::carve(
            base.as_ref(),
            carve::SIGNATURES,
            &opts,
            progress,
            Some(cancel),
        )?;
        json(&found)
    }))
}

// ---------------------------------------------------------------- imaging, cloning, wiping (DISK-08…10)

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct E01Meta {
    pub case_number: String,
    pub evidence_number: String,
    pub description: String,
    pub examiner: String,
    pub notes: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CloneTarget {
    /// Raw (dd) image file.
    Raw { path: PathBuf },
    /// `EnCase` E01 image with MD5.
    E01 { path: PathBuf, meta: E01Meta },
    /// Another open document opened writable (disk to disk).
    Doc { id: u64 },
}

/// Images or clones document `id` (its source, without unsaved edits).
#[tauri::command]
pub fn disk_clone(
    app: AppHandle,
    id: u64,
    target: CloneTarget,
    verify: bool,
    channel: Channel<JobEvent>,
) -> CmdResult<u64> {
    let (_, src) = base_of(&app, id)?;
    let dst = match &target {
        CloneTarget::Doc { id: dst } => {
            if *dst == id {
                return Err(CmdError::invalid("нельзя клонировать диск сам в себя"));
            }
            let (entry, base) = base_of(&app, *dst)?;
            if !base.capabilities().writable {
                return Err(CmdError::new(
                    "readOnly",
                    "целевой диск открыт только для чтения",
                ));
            }
            Some((entry, base))
        }
        _ => None,
    };
    let app2 = app.clone();
    Ok(run_job(&app, channel, move |cancel, progress| {
        let opts = CloneOptions {
            verify,
            ..CloneOptions::default()
        };
        match target {
            CloneTarget::Raw { path } => json(&clone_to_file(
                src.as_ref(),
                &path,
                &opts,
                progress,
                Some(cancel),
            )?),
            CloneTarget::E01 { path, meta } => {
                let e01 = E01Options {
                    case_number: meta.case_number,
                    evidence_number: meta.evidence_number,
                    description: meta.description,
                    examiner: meta.examiner,
                    notes: meta.notes,
                    ..E01Options::default()
                };
                json(&create_e01(
                    src.as_ref(),
                    &path,
                    &e01,
                    progress,
                    Some(cancel),
                )?)
            }
            CloneTarget::Doc { .. } => {
                let (entry, base) = dst.expect("checked above");
                let r = clone_provider(src.as_ref(), base.as_ref(), &opts, progress, Some(cancel));
                reload_from_base(&app2, &entry);
                json(&r?)
            }
        }
    }))
}

/// Overwrites a range of a writable source (DISK-10).
#[tauri::command]
pub fn disk_wipe(
    app: AppHandle,
    id: u64,
    method: WipeMethod,
    verify: bool,
    start: u64,
    len: Option<u64>,
    channel: Channel<JobEvent>,
) -> CmdResult<u64> {
    let (entry, base) = base_of(&app, id)?;
    if !base.capabilities().writable {
        return Err(CmdError::new(
            "readOnly",
            "источник открыт только для чтения — откройте его для записи",
        ));
    }
    let app2 = app.clone();
    Ok(run_job(&app, channel, move |cancel, progress| {
        let opts = WipeOptions {
            method,
            verify,
            start,
            len,
            ..WipeOptions::default()
        };
        let r = wipe(base.as_ref(), &opts, progress, Some(cancel));
        reload_from_base(&app2, &entry);
        json(&r?)
    }))
}

/// Overwrites, renames and deletes a file (FILE-14).
#[tauri::command]
pub fn file_shred(
    app: AppHandle,
    path: PathBuf,
    method: WipeMethod,
    rename: bool,
    delete: bool,
    channel: Channel<JobEvent>,
) -> CmdResult<u64> {
    if app
        .state::<AppState>()
        .all()
        .iter()
        .any(|e| e.doc.lock().path() == Some(path.as_path()))
    {
        return Err(CmdError::new(
            "busy",
            "файл открыт в Hexa — закройте вкладку перед уничтожением",
        ));
    }
    Ok(run_job(&app, channel, move |cancel, progress| {
        let opts = ShredOptions {
            passes: method.passes(),
            rename,
            delete,
        };
        json(&shred_file(&path, &opts, progress, Some(cancel))?)
    }))
}

// ---------------------------------------------------------------- write-back with preview and journal (DISK-12)

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteView {
    pub offset: u64,
    pub len: u64,
    pub old: Vec<u8>,
    pub new: Vec<u8>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitPreview {
    pub writes: Vec<WriteView>,
    pub total: u64,
    pub count: usize,
}

fn staged(entry: &DocEntry) -> CmdResult<WriteGuard> {
    let doc = entry.doc.lock();
    let base = doc.base().clone();
    if !base.capabilities().writable {
        return Err(CmdError::new(
            "readOnly",
            "источник открыт только для чтения",
        ));
    }
    if doc.len() != base.len() {
        return Err(CmdError::new(
            "fixedSize",
            "размер изменился: в источник можно записать только замену байтов",
        ));
    }
    let spans = doc.changes_in(0, doc.len());
    let mut total = 0u64;
    let mut guard = WriteGuard::new(base);
    for s in spans.iter().filter(|s| s.kind == ChangeKind::Modified) {
        total += s.end - s.start;
        if total > MAX_COMMIT {
            return Err(CmdError::new("tooLarge", "слишком много изменений за раз"));
        }
        let mut buf = vec![0u8; (s.end - s.start) as usize];
        doc.read(s.start, &mut buf)?;
        guard.stage(s.start, &buf)?;
    }
    Ok(guard)
}

/// What committing the edits of `id` to its source would write.
#[tauri::command]
pub async fn src_commit_preview(app: AppHandle, id: u64) -> CmdResult<CommitPreview> {
    blocking(move || {
        let entry = app.state::<AppState>().get(id)?;
        let guard = staged(&entry)?;
        let pending = guard.pending();
        Ok(CommitPreview {
            total: guard.total_bytes(),
            count: pending.len(),
            writes: pending
                .iter()
                .take(500)
                .map(|w| WriteView {
                    offset: w.offset,
                    len: w.new.len() as u64,
                    old: w.old.iter().take(PREVIEW_BYTES).copied().collect(),
                    new: w.new.iter().take(PREVIEW_BYTES).copied().collect(),
                })
                .collect(),
        })
    })
    .await
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitResult {
    pub bytes: u64,
    /// Undo journal of the overwritten bytes.
    pub journal: Option<String>,
    pub info: DocInfo,
}

fn journal_path(app: &AppHandle, id: u64) -> PathBuf {
    let dir = app.state::<AppState>().config_dir.read().join("journals");
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    dir.join(format!("source-{id}-{ts}.json"))
}

/// Writes the edits of `id` into its source and keeps an undo journal.
#[tauri::command]
pub async fn src_commit(app: AppHandle, id: u64) -> CmdResult<CommitResult> {
    blocking(move || {
        let entry = app.state::<AppState>().get(id)?;
        let guard = staged(&entry)?;
        let bytes = guard.total_bytes();
        let journal: UndoJournal = guard.commit()?;
        let path = journal_path(&app, id);
        let saved = std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))
            .ok()
            .and_then(|()| journal.save(&path).ok())
            .map(|()| path.to_string_lossy().into_owned());
        let info = reload_from_base(&app, &entry);
        Ok(CommitResult {
            bytes,
            journal: saved,
            info,
        })
    })
    .await
}

/// Restores the bytes recorded in an undo journal.
#[tauri::command]
pub async fn src_rollback(app: AppHandle, id: u64, journal: PathBuf) -> CmdResult<DocInfo> {
    blocking(move || {
        let (entry, base) = base_of(&app, id)?;
        UndoJournal::load(&journal)?.rollback(base.as_ref())?;
        Ok(reload_from_base(&app, &entry))
    })
    .await
}

// ---------------------------------------------------------------- alternate streams (FILE-17)

#[tauri::command]
pub fn file_streams(path: PathBuf) -> CmdResult<Vec<StreamInfo>> {
    Ok(streams::list_streams(&path)?)
}

#[tauri::command]
pub fn file_stream_open(app: AppHandle, path: PathBuf, name: String) -> CmdResult<DocInfo> {
    let data = streams::read_stream(&path, &name)?;
    let label = format!("{}:{name}", file_name(&path));
    let state = app.state::<AppState>();
    let doc = Document::from_bytes(label.clone(), data);
    let di = opened_doc(&app, &state, doc);
    app.state::<SourceState>()
        .file_streams
        .lock()
        .insert(di.id, (path.clone(), name));
    register(
        &app,
        di.id,
        SourceInfo::new("fileStream", label)
            .path(path.to_string_lossy())
            .writable(true),
    );
    Ok(di)
}

/// Writes document `id` back into the stream it was opened from.
#[tauri::command]
pub fn file_stream_save(app: AppHandle, id: u64) -> CmdResult<()> {
    let (path, name) = app
        .state::<SourceState>()
        .file_streams
        .lock()
        .get(&id)
        .cloned()
        .ok_or_else(|| CmdError::invalid("документ открыт не из потока файла"))?;
    let data = doc_bytes(&app, id)?;
    streams::write_stream(&path, &name, &data)?;
    Ok(())
}

#[tauri::command]
pub fn file_stream_remove(path: PathBuf, name: String) -> CmdResult<()> {
    Ok(streams::remove_stream(&path, &name)?)
}

fn doc_bytes(app: &AppHandle, id: u64) -> CmdResult<Vec<u8>> {
    let entry = app.state::<AppState>().get(id)?;
    let tree = entry.view.read().tree.clone();
    if tree.len() > MAX_REPACK {
        return Err(CmdError::new("tooLarge", "документ слишком большой"));
    }
    let mut buf = vec![0u8; tree.len() as usize];
    tree.read(0, &mut buf)?;
    Ok(buf)
}

// ---------------------------------------------------------------- archives (FILE-15)

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveListing {
    pub kind: ArchiveKind,
    pub entries: Vec<ArchiveEntry>,
}

#[tauri::command]
pub async fn archive_list(path: PathBuf) -> CmdResult<ArchiveListing> {
    blocking(move || {
        let a = Archive::open(&path).map_err(net_err)?;
        Ok(ArchiveListing {
            kind: a.kind(),
            entries: a.entries().to_vec(),
        })
    })
    .await
}

#[tauri::command]
pub async fn archive_open_entry(app: AppHandle, path: PathBuf, name: String) -> CmdResult<DocInfo> {
    blocking(move || {
        let a = Archive::open(&path).map_err(net_err)?;
        let provider = a.open_entry(&name).map_err(net_err)?;
        let label = provider.name();
        let di = open_provider(
            &app,
            provider,
            false,
            SourceInfo::new("archiveEntry", label).path(path.to_string_lossy()),
        );
        app.state::<SourceState>()
            .archive_entries
            .lock()
            .insert(di.id, (path, name));
        Ok(di)
    })
    .await
}

/// Writes the document back into its archive (a copy at `dest`, or the
/// archive itself, replaced atomically).
#[tauri::command]
pub async fn archive_repack(app: AppHandle, id: u64, dest: Option<PathBuf>) -> CmdResult<String> {
    blocking(move || {
        let (path, name) = app
            .state::<SourceState>()
            .archive_entries
            .lock()
            .get(&id)
            .cloned()
            .ok_or_else(|| CmdError::invalid("документ открыт не из архива"))?;
        let data = doc_bytes(&app, id)?;
        let dst = dest.unwrap_or_else(|| path.clone());
        let a = Archive::open(&path).map_err(net_err)?;
        a.repack(&name, &data, &dst).map_err(net_err)?;
        Ok(dst.to_string_lossy().into_owned())
    })
    .await
}

// ---------------------------------------------------------------- remote sources (CORE-02, FILE-16)

#[tauri::command]
pub async fn net_open_http(
    app: AppHandle,
    url: String,
    options: Option<HttpOptions>,
) -> CmdResult<DocInfo> {
    blocking(move || {
        let p = HttpProvider::open(&url, &options.unwrap_or_default()).map_err(net_err)?;
        let info = SourceInfo::new("http", url.clone()).path(url);
        Ok(open_provider(&app, Arc::new(p), true, info))
    })
    .await
}

#[tauri::command]
pub async fn net_open_s3(app: AppHandle, options: S3Options) -> CmdResult<DocInfo> {
    blocking(move || {
        let p = S3Provider::open(&options).map_err(net_err)?;
        let label = format!("s3://{}/{}", options.bucket, options.key);
        let info = SourceInfo::new("s3", label.clone()).path(label);
        Ok(open_provider(&app, Arc::new(p), true, info))
    })
    .await
}

/// Outcome of an SFTP connection attempt.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SftpOpen {
    Opened {
        info: DocInfo,
    },
    /// The server key is unknown (or changed): ask the user, then retry
    /// with `fingerprint`.
    HostKey {
        host: String,
        fingerprint: String,
        changed: bool,
    },
}

fn known_hosts_path(app: &AppHandle) -> PathBuf {
    app.state::<AppState>()
        .config_dir
        .read()
        .join("known_hosts.json")
}

fn known_hosts(app: &AppHandle) -> HashMap<String, String> {
    std::fs::read_to_string(known_hosts_path(app))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Opens a file over SFTP. Without `fingerprint`, only a key remembered
/// from an earlier confirmation is trusted.
#[tauri::command]
pub async fn net_open_sftp(
    app: AppHandle,
    options: SftpOptions,
    fingerprint: Option<String>,
) -> CmdResult<SftpOpen> {
    blocking(move || {
        let host_id = format!("{}:{}", options.host, options.port);
        let mut hosts = known_hosts(&app);
        let known = hosts.get(&host_id).cloned();
        let trusted = fingerprint.clone().or_else(|| known.clone());
        let check = match &trusted {
            Some(fp) => pinned_fingerprint(fp.clone()),
            None => Arc::new(|_: &hexa_net::HostKeyInfo| false),
        };
        match SftpProvider::connect(&options, check) {
            Ok(p) => {
                if let Some(fp) = fingerprint
                    && known.as_deref() != Some(fp.as_str())
                {
                    hosts.insert(host_id, fp);
                    let path = known_hosts_path(&app);
                    if let Some(dir) = path.parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    if let Ok(text) = serde_json::to_string_pretty(&hosts) {
                        let _ = std::fs::write(path, text);
                    }
                }
                let label = format!("sftp://{}@{}{}", options.user, options.host, options.path);
                let info = SourceInfo::new("sftp", label.clone())
                    .path(label)
                    .writable(options.writable);
                Ok(SftpOpen::Opened {
                    info: open_provider(&app, Arc::new(p), !options.writable, info),
                })
            }
            Err(hexa_net::Error::HostKeyRejected { host, fingerprint }) => Ok(SftpOpen::HostKey {
                host,
                changed: known.is_some_and(|k| k != fingerprint),
                fingerprint,
            }),
            Err(e) => Err(net_err(e)),
        }
    })
    .await
}

// ---------------------------------------------------------------- network capture (CORE-02)

/// Starts capturing UDP datagrams or a TCP stream into a new document that
/// grows while data arrives.
#[tauri::command]
pub async fn net_capture_start(app: AppHandle, options: StreamOptions) -> CmdResult<DocInfo> {
    blocking(move || {
        let p = Arc::new(StreamProvider::new(options));
        p.start()?;
        let label = p.name();
        let di = open_provider(&app, p.clone(), false, SourceInfo::new("capture", label));
        app.state::<SourceState>()
            .captures
            .lock()
            .insert(di.id, p.clone());
        let id = di.id;
        std::thread::spawn(move || follow_capture(&app, id, &p));
        Ok(di)
    })
    .await
}

/// Re-creates the capture document whenever the buffer length changes.
fn follow_capture(app: &AppHandle, id: u64, p: &Arc<StreamProvider>) {
    loop {
        std::thread::sleep(Duration::from_millis(300));
        let Ok(entry) = app.state::<AppState>().get(id) else {
            p.stop();
            return;
        };
        let mut doc = entry.doc.lock();
        if p.len() != doc.len() && !doc.is_modified() {
            *doc = Document::from_provider(p.clone(), false);
            entry.publish(&doc);
            drop(doc);
            notify_updated(app, id);
        }
    }
}

fn capture(app: &AppHandle, id: u64) -> CmdResult<Arc<StreamProvider>> {
    app.state::<SourceState>()
        .captures
        .lock()
        .get(&id)
        .cloned()
        .ok_or_else(|| CmdError::invalid("документ не является захватом сети"))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureView {
    pub status: StreamStatus,
    pub segments: Vec<hexa_net::stream::Segment>,
}

#[tauri::command]
pub fn net_capture_status(app: AppHandle, id: u64) -> CmdResult<CaptureView> {
    let p = capture(&app, id)?;
    let segments = p.segments();
    let skip = segments.len().saturating_sub(500);
    Ok(CaptureView {
        status: p.status(),
        segments: segments.into_iter().skip(skip).collect(),
    })
}

/// `stop`, `resume` or `clear`.
#[tauri::command]
pub fn net_capture_control(app: AppHandle, id: u64, action: String) -> CmdResult<()> {
    let p = capture(&app, id)?;
    match action.as_str() {
        "stop" => p.stop(),
        "resume" => p.start()?,
        "clear" => p.clear(),
        other => return Err(CmdError::invalid(format!("unknown action {other}"))),
    }
    Ok(())
}
