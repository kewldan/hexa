//! Process memory (PROC-01…10): process list, address-space documents,
//! regions and modules, value scanner, freezing, pointer scan, dumps, GDB
//! remote targets and permission diagnostics.
//!
//! Scanner sessions, frozen values and pointer-scan results live here, keyed
//! by the id of the process document, and are dropped with it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use hexa_core::provider::Provider;
use hexa_proc::diagnostics::{self, Diagnostic, Privileges};
use hexa_proc::dump::{dump_module, dump_range};
use hexa_proc::freezer::DEFAULT_INTERVAL;
use hexa_proc::pointer::{pointer_scan, rescan, resolve_chain};
use hexa_proc::region::modules;
use hexa_proc::scan::ScanHit;
use hexa_proc::{
    Access, AddressSpaceProvider, FirstScan, Freezer, GdbSource, MemoryRegion, MemorySource,
    Module, NextScan, PointerChain, PointerScanOptions, ProcessInfo, ScanOptions, Scanner,
    ValueType, list_processes,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State};

use super::sources::{JobEvent, SourceInfo, blocking, json, open_provider, run_job};
use crate::error::{CmdError, CmdResult};
use crate::state::DocInfo;

/// Live state of one process document.
#[derive(Debug)]
pub struct ProcEntry {
    provider: Arc<AddressSpaceProvider>,
    scanner: Mutex<Option<Scanner>>,
    scanning: AtomicBool,
    freezer: Mutex<Option<Freezer>>,
    frozen_types: Mutex<HashMap<u64, ValueType>>,
    chains: Mutex<Vec<PointerChain>>,
}

impl ProcEntry {
    fn source(&self) -> &dyn MemorySource {
        self.provider.source().as_ref()
    }
}

#[derive(Debug, Default)]
pub struct MemState {
    procs: Mutex<HashMap<u64, Arc<ProcEntry>>>,
}

/// Drops the sessions of a closed document (stops its freezer).
pub(crate) fn forget(app: &AppHandle, id: u64) {
    let removed = app.state::<MemState>().procs.lock().remove(&id);
    drop(removed);
}

pub(crate) fn proc_err(e: hexa_proc::Error) -> CmdError {
    use hexa_proc::Error as E;
    let kind = match &e {
        E::NoSuchProcess { .. } => "notFound",
        E::PermissionDenied { .. } => "permission",
        E::Unmapped { .. } => "unmapped",
        E::Cancelled => "cancelled",
        E::Invalid(_) => "invalid",
        E::Unsupported(_) => "unsupported",
        E::Protocol(_) => "protocol",
        E::Io(_) | E::Os { .. } => "io",
    };
    CmdError::new(kind, e.to_string())
}

fn entry(mem: &MemState, id: u64) -> CmdResult<Arc<ProcEntry>> {
    mem.procs
        .lock()
        .get(&id)
        .cloned()
        .ok_or_else(|| CmdError::invalid("документ не является памятью процесса"))
}

fn adopt_space(app: &AppHandle, provider: AddressSpaceProvider, info: SourceInfo) -> DocInfo {
    let provider = Arc::new(provider);
    let di = open_provider(app, provider.clone(), true, info);
    app.state::<MemState>().procs.lock().insert(
        di.id,
        Arc::new(ProcEntry {
            provider,
            scanner: Mutex::new(None),
            scanning: AtomicBool::new(false),
            freezer: Mutex::new(None),
            frozen_types: Mutex::new(HashMap::new()),
            chains: Mutex::new(Vec::new()),
        }),
    );
    di
}

// ---------------------------------------------------------------- processes (PROC-01, PROC-03, PROC-10)

#[tauri::command]
pub async fn proc_list() -> CmdResult<Vec<ProcessInfo>> {
    blocking(|| list_processes().map_err(proc_err)).await
}

#[tauri::command]
pub fn proc_privileges() -> Privileges {
    diagnostics::privileges()
}

/// Outcome of opening a process.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ProcOpen {
    Opened {
        info: DocInfo,
    },
    /// The OS refused access; the diagnostic says how to fix it (PROC-10).
    Denied {
        diagnostic: Diagnostic,
    },
}

/// Opens the address space of `pid` as a fixed-size document (PROC-03).
#[tauri::command]
pub async fn proc_open(app: AppHandle, pid: u32, writable: bool) -> CmdResult<ProcOpen> {
    blocking(move || {
        let access = if writable {
            Access::ReadWrite
        } else {
            Access::Read
        };
        match AddressSpaceProvider::open_process(pid, access) {
            Ok(p) => {
                let mut info = SourceInfo::new("process", p.name());
                info.pid = Some(pid);
                info.writable = writable;
                Ok(ProcOpen::Opened {
                    info: adopt_space(&app, p, info),
                })
            }
            Err(e) => match e.diagnostic() {
                Some(d) => Ok(ProcOpen::Denied {
                    diagnostic: d.clone(),
                }),
                None => Err(proc_err(e)),
            },
        }
    })
    .await
}

/// Connects to a GDB remote stub (`gdbserver`, QEMU, `debugserver`) and
/// opens its memory (PROC-09).
#[tauri::command]
pub async fn gdb_connect(app: AppHandle, address: String) -> CmdResult<DocInfo> {
    blocking(move || {
        let src = GdbSource::connect(address.as_str()).map_err(proc_err)?;
        let p = AddressSpaceProvider::new(Arc::new(src)).map_err(proc_err)?;
        let mut info = SourceInfo::new("gdb", format!("gdb {address}"));
        info.writable = true;
        info.path = Some(address);
        Ok(adopt_space(&app, p, info))
    })
    .await
}

// ---------------------------------------------------------------- regions & modules (PROC-02)

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemMap {
    pub regions: Vec<MemoryRegion>,
    pub modules: Vec<Module>,
    /// Unmapped ranges the grid folds away.
    pub gaps: Vec<(u64, u64)>,
    pub pointer_size: u8,
}

#[tauri::command]
pub async fn mem_map(app: AppHandle, id: u64, refresh: bool) -> CmdResult<MemMap> {
    blocking(move || {
        let e = entry(&app.state::<MemState>(), id)?;
        let regions = if refresh {
            e.provider.refresh_regions().map_err(proc_err)?
        } else {
            e.provider.regions()
        };
        Ok(MemMap {
            modules: modules(&regions),
            gaps: e.provider.gaps().iter().map(|r| (r.start, r.end)).collect(),
            pointer_size: e.source().pointer_size(),
            regions,
        })
    })
    .await
}

// ---------------------------------------------------------------- value scanner (PROC-06)

/// Starts a new scan session with a first scan.
#[tauri::command]
pub fn scan_first(
    app: AppHandle,
    mem: State<'_, MemState>,
    id: u64,
    options: ScanOptions,
    condition: FirstScan,
    channel: Channel<JobEvent>,
) -> CmdResult<u64> {
    let e = entry(&mem, id)?;
    if e.scanning.swap(true, Ordering::AcqRel) {
        return Err(CmdError::new("busy", "сканирование уже идёт"));
    }
    Ok(run_job(&app, channel, move |cancel, progress| {
        let r = (|| {
            let mut sc = Scanner::new(options).map_err(proc_err)?;
            let sum = sc
                .first_scan(e.source(), &condition, cancel, progress)
                .map_err(proc_err)?;
            *e.scanner.lock() = Some(sc);
            json(&sum)
        })();
        e.scanning.store(false, Ordering::Release);
        r
    }))
}

/// Filters the results of the current session.
#[tauri::command]
pub fn scan_next(
    app: AppHandle,
    mem: State<'_, MemState>,
    id: u64,
    condition: NextScan,
    channel: Channel<JobEvent>,
) -> CmdResult<u64> {
    let e = entry(&mem, id)?;
    if e.scanning.swap(true, Ordering::AcqRel) {
        return Err(CmdError::new("busy", "сканирование уже идёт"));
    }
    let Some(mut sc) = e.scanner.lock().take() else {
        e.scanning.store(false, Ordering::Release);
        return Err(CmdError::invalid("сначала выполните первое сканирование"));
    };
    Ok(run_job(&app, channel, move |cancel, progress| {
        let r = sc
            .next_scan(e.source(), &condition, cancel, progress)
            .map_err(proc_err);
        *e.scanner.lock() = Some(sc);
        e.scanning.store(false, Ordering::Release);
        json(&r?)
    }))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanPage {
    pub count: u64,
    pub scans: u32,
    pub busy: bool,
    pub value_type: Option<ValueType>,
    pub hits: Vec<ScanHit>,
}

/// Results `start..start + limit`, with current values when `live`.
#[tauri::command]
pub async fn scan_page(
    app: AppHandle,
    id: u64,
    start: u64,
    limit: usize,
    live: bool,
) -> CmdResult<ScanPage> {
    blocking(move || {
        let e = entry(&app.state::<MemState>(), id)?;
        let busy = e.scanning.load(Ordering::Acquire);
        let guard = e.scanner.lock();
        let Some(sc) = guard.as_ref() else {
            return Ok(ScanPage {
                count: 0,
                scans: 0,
                busy,
                value_type: None,
                hits: Vec::new(),
            });
        };
        let live = live.then(|| e.source());
        Ok(ScanPage {
            count: sc.count(),
            scans: sc.scans(),
            busy,
            value_type: Some(sc.options().value_type),
            hits: sc.page(live, start, limit.min(1000)),
        })
    })
    .await
}

#[tauri::command]
pub fn scan_reset(mem: State<'_, MemState>, id: u64) -> CmdResult<()> {
    let e = entry(&mem, id)?;
    *e.scanner.lock() = None;
    Ok(())
}

/// Writes `value` encoded as `value_type` at `address` (PROC-04).
#[tauri::command]
pub fn mem_write_value(
    mem: State<'_, MemState>,
    id: u64,
    address: u64,
    value_type: ValueType,
    value: String,
) -> CmdResult<()> {
    let e = entry(&mem, id)?;
    let bytes = value_type
        .encode(value_type.parse(&value).map_err(proc_err)?, false)
        .map_err(proc_err)?;
    e.source().write(address, &bytes).map_err(proc_err)
}

// ---------------------------------------------------------------- freezing (PROC-04)

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrozenView {
    pub id: u64,
    pub address: u64,
    pub label: Option<String>,
    pub enabled: bool,
    pub writes: u64,
    pub last_error: Option<String>,
    pub value_type: Option<ValueType>,
    pub value: Option<String>,
    pub current: Option<String>,
}

#[tauri::command]
pub fn mem_freeze(
    mem: State<'_, MemState>,
    id: u64,
    address: u64,
    value_type: ValueType,
    value: String,
    label: Option<String>,
) -> CmdResult<u64> {
    let e = entry(&mem, id)?;
    let bytes = value_type
        .encode(value_type.parse(&value).map_err(proc_err)?, false)
        .map_err(proc_err)?;
    let mut freezer = e.freezer.lock();
    if freezer.is_none() {
        *freezer =
            Some(Freezer::start(e.provider.source().clone(), DEFAULT_INTERVAL).map_err(proc_err)?);
    }
    let fid = freezer
        .as_ref()
        .expect("started above")
        .freeze(address, bytes, label)
        .map_err(proc_err)?;
    e.frozen_types.lock().insert(fid, value_type);
    Ok(fid)
}

#[tauri::command]
pub fn mem_frozen(mem: State<'_, MemState>, id: u64) -> CmdResult<Vec<FrozenView>> {
    let e = entry(&mem, id)?;
    let list = e
        .freezer
        .lock()
        .as_ref()
        .map(Freezer::list)
        .unwrap_or_default();
    let types = e.frozen_types.lock();
    Ok(list
        .into_iter()
        .map(|f| {
            let ty = types.get(&f.id).copied();
            let current = ty.and_then(|t| {
                let mut buf = vec![0u8; t.size()];
                e.source()
                    .read(f.address, &mut buf)
                    .ok()
                    .map(|()| t.decode(&buf, false).to_string())
            });
            FrozenView {
                id: f.id,
                address: f.address,
                label: f.label,
                enabled: f.enabled,
                writes: f.writes,
                last_error: f.last_error,
                value: ty.map(|t| t.decode(&f.bytes, false).to_string()),
                value_type: ty,
                current,
            }
        })
        .collect())
}

/// `remove`, `enable` or `disable` a frozen value.
#[tauri::command]
pub fn mem_freeze_control(
    mem: State<'_, MemState>,
    id: u64,
    frozen: u64,
    action: String,
) -> CmdResult<bool> {
    let e = entry(&mem, id)?;
    let freezer = e.freezer.lock();
    let Some(f) = freezer.as_ref() else {
        return Ok(false);
    };
    Ok(match action.as_str() {
        "remove" => {
            e.frozen_types.lock().remove(&frozen);
            f.unfreeze(frozen)
        }
        "enable" => f.set_enabled(frozen, true),
        "disable" => f.set_enabled(frozen, false),
        other => return Err(CmdError::invalid(format!("unknown action {other}"))),
    })
}

// ---------------------------------------------------------------- pointer scan (PROC-07)

#[tauri::command]
pub fn ptr_scan(
    app: AppHandle,
    mem: State<'_, MemState>,
    id: u64,
    target: u64,
    options: PointerScanOptions,
    channel: Channel<JobEvent>,
) -> CmdResult<u64> {
    let e = entry(&mem, id)?;
    Ok(run_job(&app, channel, move |cancel, progress| {
        let chains =
            pointer_scan(e.source(), target, &options, cancel, progress).map_err(proc_err)?;
        let n = chains.len();
        *e.chains.lock() = chains;
        Ok(serde_json::json!({ "count": n }))
    }))
}

/// Keeps the chains that now lead to `target` (after a restart of the
/// target, for instance).
#[tauri::command]
pub async fn ptr_rescan(app: AppHandle, id: u64, target: u64) -> CmdResult<usize> {
    blocking(move || {
        let e = entry(&app.state::<MemState>(), id)?;
        let chains = e.chains.lock().clone();
        let kept =
            rescan(e.source(), &chains, target, &hexa_core::Cancel::new()).map_err(proc_err)?;
        let n = kept.len();
        *e.chains.lock() = kept;
        Ok(n)
    })
    .await
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainView {
    pub chain: PointerChain,
    pub text: String,
    /// Where the chain leads now.
    pub resolved: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainPage {
    pub total: usize,
    pub chains: Vec<ChainView>,
}

#[tauri::command]
pub async fn ptr_results(
    app: AppHandle,
    id: u64,
    start: usize,
    limit: usize,
) -> CmdResult<ChainPage> {
    blocking(move || {
        let e = entry(&app.state::<MemState>(), id)?;
        let chains = e.chains.lock();
        let mods = modules(&e.provider.regions());
        Ok(ChainPage {
            total: chains.len(),
            chains: chains
                .iter()
                .skip(start)
                .take(limit.min(1000))
                .map(|c| ChainView {
                    text: c.to_string(),
                    resolved: resolve_chain(e.source(), c, &mods).ok(),
                    chain: c.clone(),
                })
                .collect(),
        })
    })
    .await
}

// ---------------------------------------------------------------- dumps (PROC-08)

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum DumpWhat {
    Range { start: u64, len: u64 },
    Module { name: String },
}

#[tauri::command]
pub fn mem_dump(
    app: AppHandle,
    mem: State<'_, MemState>,
    id: u64,
    what: DumpWhat,
    path: PathBuf,
    channel: Channel<JobEvent>,
) -> CmdResult<u64> {
    let e = entry(&mem, id)?;
    Ok(run_job(&app, channel, move |cancel, progress| {
        let r = match what {
            DumpWhat::Range { start, len } => {
                dump_range(e.source(), start, len, &path, cancel, progress)
            }
            DumpWhat::Module { name } => dump_module(e.source(), &name, &path, cancel, progress),
        }
        .map_err(proc_err)?;
        json(&r)
    }))
}
