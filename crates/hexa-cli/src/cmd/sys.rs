//! `disks`, `partitions` and the hidden `helper` (NFR-10).

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use clap::Args;
use hexa_core::provider::Provider;
use hexa_sys::{DeviceOpenOptions, DeviceProvider};

use super::name_of;
use crate::error::{CliError, CliResult, Outcome};
use crate::io::{self, human_size};

pub fn run_disks(json_out: bool) -> CliResult {
    let disks = hexa_sys::list_disks()?;
    if json_out {
        io::print_json(&disks)?;
        return Ok(Outcome::Ok);
    }
    let mut s = String::new();
    for d in &disks {
        let _ = writeln!(
            s,
            "{:<12} {:<24} {:<28} {}{}",
            d.id,
            d.path,
            human_size(d.size),
            d.model.as_deref().unwrap_or(""),
            if d.removable { " (removable)" } else { "" }
        );
        for v in &d.volumes {
            let _ = writeln!(
                s,
                "  {:<10} {:<24} {:<28} {} {} {}",
                v.id,
                v.path,
                human_size(v.size),
                v.file_system
                    .as_deref()
                    .or(v.content.as_deref())
                    .unwrap_or(""),
                v.label.as_deref().unwrap_or(""),
                v.mount_point.as_deref().unwrap_or("")
            );
        }
    }
    io::print(&s)?;
    Ok(Outcome::Ok)
}

#[derive(Debug, Args)]
pub struct PartitionsArgs {
    /// Disk device (`/dev/disk2`, `\\.\PhysicalDrive1`) or disk image
    /// (raw, E01, QCOW2, VHD(X), VMDK, DMG …).
    pub path: PathBuf,
    /// Logical sector size (default: detected).
    #[arg(long)]
    pub sector_size: Option<u32>,
}

fn is_device(path: &std::path::Path) -> bool {
    let s = path.to_string_lossy();
    s.starts_with("/dev/") || s.starts_with(r"\\.\")
}

pub fn run_partitions(a: &PartitionsArgs, json_out: bool) -> CliResult {
    let provider: Arc<dyn Provider> = if is_device(&a.path) {
        let opts = DeviceOpenOptions {
            writable: false,
            sector_size: a.sector_size,
        };
        Arc::new(
            DeviceProvider::open(&a.path, opts)
                .map_err(|e| CliError::from(e).context(a.path.display()))?,
        )
    } else {
        hexa_sys::open_image(&a.path).map_err(|e| CliError::from(e).context(a.path.display()))?
    };
    let table = hexa_sys::read_partition_table(provider.as_ref(), a.sector_size)?;
    if json_out {
        io::print_json(&table)?;
    } else if let Some(t) = &table {
        let mut s = format!(
            "{} partition table, {}-byte sectors\n",
            name_of(&t.kind).to_ascii_uppercase(),
            t.sector_size
        );
        for p in &t.partitions {
            let _ = writeln!(
                s,
                "  {:>3} {:>14X} {:<28} {:<28} {}{}",
                p.number,
                p.offset,
                human_size(p.size),
                p.type_name,
                p.name.as_deref().unwrap_or(""),
                if p.bootable { " (boot)" } else { "" }
            );
        }
        for w in &t.warnings {
            let _ = writeln!(s, "warning: {w}");
        }
        io::print(&s)?;
    } else {
        eprintln!("hexa-cli: no partition table found");
    }
    Ok(Outcome::from_ok(table.is_some()))
}

#[derive(Debug, Args)]
pub struct HelperArgs {
    /// Allow write requests (refused otherwise, whatever the client asks).
    #[arg(long)]
    pub allow_write: bool,
}

pub fn run_helper(a: &HelperArgs) -> CliResult {
    let stdin = std::io::stdin().lock();
    let stdout = std::io::stdout().lock();
    hexa_sys::helper_serve(stdin, stdout, a.allow_write)?;
    Ok(Outcome::Ok)
}
