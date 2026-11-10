//! Enumerating disks, partitions and volumes (DISK-01).
//!
//! Each OS has a parser for captured tool output plus a thin runner:
//! - macOS: `diskutil list -plist` and `diskutil info -plist <id>`,
//! - Linux: `lsblk -J -b -O` (partition offsets from `/sys/class/block`),
//! - Windows: `Get-Disk`, `Get-Partition`, `Get-Volume` as JSON.

use std::process::Command;

use hexa_core::Result;
use serde::Serialize;
use serde_json::Value as Json;

use crate::util::invalid;

/// A physical (or virtual) disk with its partitions/volumes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskInfo {
    /// OS identifier (`disk2`, `sda`, `2`).
    pub id: String,
    /// Path to open raw (`/dev/rdisk2`, `/dev/sda`, `\\.\PhysicalDrive2`).
    pub path: String,
    pub size: u64,
    pub model: Option<String>,
    pub sector_size: u32,
    pub removable: bool,
    pub internal: Option<bool>,
    /// Synthesised by the OS (APFS container, loop device …).
    pub virtual_disk: bool,
    /// `GUID_partition_scheme`, `gpt`, `MBR` …
    pub partition_scheme: Option<String>,
    pub volumes: Vec<VolumeInfo>,
}

/// A partition or volume on a [`DiskInfo`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeInfo {
    pub id: String,
    pub path: String,
    /// Byte offset on the parent disk, when known.
    pub offset: Option<u64>,
    pub size: u64,
    /// Partition type / content hint (`Apple_APFS`, `EFI`, `Basic` …).
    pub content: Option<String>,
    pub file_system: Option<String>,
    pub label: Option<String>,
    pub mount_point: Option<String>,
}

/// Lists the disks of this machine (DISK-01).
pub fn list_disks() -> Result<Vec<DiskInfo>> {
    imp::list()
}

fn run(cmd: &str, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new(cmd).args(args).output()?;
    if !out.status.success() {
        return Err(invalid(format!(
            "{cmd} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(out.stdout)
}

fn non_empty(s: Option<&str>) -> Option<String> {
    s.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{DiskInfo, Result, parse_diskutil, run};

    pub(super) fn list() -> Result<Vec<DiskInfo>> {
        let list = run("diskutil", &["list", "-plist"])?;
        parse_diskutil(&list, &|id| run("diskutil", &["info", "-plist", id]).ok())
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{DiskInfo, Result, parse_lsblk, run};

    pub(super) fn list() -> Result<Vec<DiskInfo>> {
        let out = run("lsblk", &["-J", "-b", "-O"])?;
        let mut disks = parse_lsblk(&String::from_utf8_lossy(&out))?;
        for d in &mut disks {
            for v in &mut d.volumes {
                if v.offset.is_none() {
                    // /sys reports the start in 512-byte units regardless of
                    // the logical sector size.
                    let p = format!("/sys/class/block/{}/start", v.id);
                    v.offset = std::fs::read_to_string(p)
                        .ok()
                        .and_then(|s| s.trim().parse::<u64>().ok())
                        .map(|s| s * 512);
                }
            }
        }
        Ok(disks)
    }
}

#[cfg(windows)]
mod imp {
    use super::{DiskInfo, Result, parse_windows_storage, run};

    pub(super) const SCRIPT: &str = "$ErrorActionPreference='Stop';\
$d=@(Get-Disk | Select-Object Number,FriendlyName,Model,Size,LogicalSectorSize,\
@{n='BusType';e={\"$($_.BusType)\"}},@{n='PartitionStyle';e={\"$($_.PartitionStyle)\"}},IsBoot,IsSystem);\
$p=@(Get-Partition | Select-Object DiskNumber,PartitionNumber,Offset,Size,\
@{n='DriveLetter';e={\"$($_.DriveLetter)\"}},@{n='Type';e={\"$($_.Type)\"}},GptType,AccessPaths);\
$v=@(Get-Volume | Select-Object @{n='DriveLetter';e={\"$($_.DriveLetter)\"}},FileSystem,FileSystemLabel,Size,Path);\
@{disks=$d;partitions=$p;volumes=$v} | ConvertTo-Json -Depth 4 -Compress";

    pub(super) fn list() -> Result<Vec<DiskInfo>> {
        let out = run(
            "powershell",
            &["-NoProfile", "-NonInteractive", "-Command", SCRIPT],
        )?;
        parse_windows_storage(&String::from_utf8_lossy(&out))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod imp {
    use super::{DiskInfo, Result};

    pub(super) fn list() -> Result<Vec<DiskInfo>> {
        Err(hexa_core::Error::Unsupported)
    }
}

// ---------------------------------------------------------------------------
// macOS
// ---------------------------------------------------------------------------

fn plist_dict(data: &[u8]) -> Result<plist::Dictionary> {
    let v: plist::Value =
        plist::from_bytes(data).map_err(|e| invalid(format!("bad plist: {e}")))?;
    v.into_dictionary()
        .ok_or_else(|| invalid("plist root is not a dictionary"))
}

fn pstr<'a>(d: &'a plist::Dictionary, key: &str) -> Option<&'a str> {
    d.get(key).and_then(plist::Value::as_string)
}

fn pu64(d: &plist::Dictionary, key: &str) -> Option<u64> {
    d.get(key).and_then(|v| {
        v.as_unsigned_integer()
            .or_else(|| v.as_signed_integer().map(|i| i as u64))
    })
}

fn pbool(d: &plist::Dictionary, key: &str) -> Option<bool> {
    d.get(key).and_then(plist::Value::as_boolean)
}

fn parray<'a>(d: &'a plist::Dictionary, key: &str) -> impl Iterator<Item = &'a plist::Dictionary> {
    d.get(key)
        .and_then(plist::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(plist::Value::as_dictionary)
}

fn mac_volume(entry: &plist::Dictionary, info: Option<&plist::Dictionary>) -> VolumeInfo {
    let id = pstr(entry, "DeviceIdentifier")
        .unwrap_or_default()
        .to_string();
    let get = |k: &str| non_empty(pstr(entry, k).or_else(|| info.and_then(|i| pstr(i, k))));
    VolumeInfo {
        path: format!("/dev/r{id}"),
        offset: info.and_then(|i| pu64(i, "PartitionMapPartitionOffset")),
        size: pu64(entry, "Size")
            .or_else(|| info.and_then(|i| pu64(i, "TotalSize")))
            .unwrap_or(0),
        content: get("Content"),
        file_system: info.and_then(|i| {
            non_empty(pstr(i, "FilesystemName").or_else(|| pstr(i, "FilesystemType")))
        }),
        label: get("VolumeName"),
        mount_point: get("MountPoint"),
        id,
    }
}

/// Parses `diskutil list -plist`; `info` returns `diskutil info -plist <id>`
/// output for a device (or `None` to skip the details).
pub fn parse_diskutil(
    list: &[u8],
    info: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> Result<Vec<DiskInfo>> {
    let root = plist_dict(list)?;
    let info_of = |id: &str| info(id).and_then(|b| plist_dict(&b).ok());
    let mut disks = Vec::new();
    for entry in parray(&root, "AllDisksAndPartitions") {
        let id = pstr(entry, "DeviceIdentifier")
            .unwrap_or_default()
            .to_string();
        let dinfo = info_of(&id);
        let di = dinfo.as_ref();
        let mut disk = DiskInfo {
            path: format!("/dev/r{id}"),
            size: pu64(entry, "Size")
                .or_else(|| di.and_then(|i| pu64(i, "TotalSize")))
                .unwrap_or(0),
            model: di.and_then(|i| {
                non_empty(pstr(i, "MediaName"))
                    .or_else(|| non_empty(pstr(i, "IORegistryEntryName")))
            }),
            sector_size: di.and_then(|i| pu64(i, "DeviceBlockSize")).unwrap_or(512) as u32,
            removable: di.is_some_and(|i| {
                pbool(i, "Removable").unwrap_or(false)
                    || pbool(i, "RemovableMedia").unwrap_or(false)
                    || pbool(i, "Ejectable").unwrap_or(false)
            }),
            internal: di.and_then(|i| pbool(i, "Internal")),
            virtual_disk: entry.contains_key("APFSVolumes")
                || di.and_then(|i| pstr(i, "VirtualOrPhysical")) == Some("Virtual"),
            partition_scheme: non_empty(pstr(entry, "Content")),
            volumes: Vec::new(),
            id,
        };
        for part in parray(entry, "Partitions").chain(parray(entry, "APFSVolumes")) {
            let pid = pstr(part, "DeviceIdentifier").unwrap_or_default();
            let pinfo = info_of(pid);
            disk.volumes.push(mac_volume(part, pinfo.as_ref()));
        }
        // A whole-disk file system (no partition map) is its own volume.
        if disk.volumes.is_empty()
            && (entry.contains_key("VolumeName") || entry.contains_key("MountPoint"))
        {
            let mut v = mac_volume(entry, di);
            v.offset = Some(0);
            disk.volumes.push(v);
            disk.partition_scheme = None;
        }
        disks.push(disk);
    }
    Ok(disks)
}

// ---------------------------------------------------------------------------
// Linux
// ---------------------------------------------------------------------------

fn ju64(v: &Json, key: &str) -> Option<u64> {
    match v.get(key)? {
        Json::Number(n) => n.as_u64(),
        Json::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn jbool(v: &Json, key: &str) -> Option<bool> {
    match v.get(key)? {
        Json::Bool(b) => Some(*b),
        Json::Number(n) => Some(n.as_u64() != Some(0)),
        Json::String(s) => Some(matches!(s.trim(), "1" | "true" | "True")),
        _ => None,
    }
}

fn jstr(v: &Json, key: &str) -> Option<String> {
    match v.get(key)? {
        Json::String(s) => non_empty(Some(s.trim_matches('\0'))),
        Json::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn lsblk_mount(v: &Json) -> Option<String> {
    jstr(v, "mountpoint").or_else(|| {
        v.get("mountpoints")?
            .as_array()?
            .iter()
            .filter_map(Json::as_str)
            .find(|s| !s.is_empty())
            .map(str::to_string)
    })
}

fn lsblk_children(v: &Json, sector: u64, out: &mut Vec<VolumeInfo>) {
    for c in v
        .get("children")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
    {
        let name = jstr(c, "name").unwrap_or_default();
        out.push(VolumeInfo {
            path: jstr(c, "path").unwrap_or_else(|| format!("/dev/{name}")),
            offset: ju64(c, "start").map(|s| s * sector),
            size: ju64(c, "size").unwrap_or(0),
            content: jstr(c, "parttypename").or_else(|| jstr(c, "type")),
            file_system: jstr(c, "fstype"),
            label: jstr(c, "label").or_else(|| jstr(c, "partlabel")),
            mount_point: lsblk_mount(c),
            id: name,
        });
        lsblk_children(c, sector, out);
    }
}

/// Parses `lsblk -J -b -O` (old versions print every value as a string).
pub fn parse_lsblk(json: &str) -> Result<Vec<DiskInfo>> {
    let root: Json =
        serde_json::from_str(json).map_err(|e| invalid(format!("bad lsblk JSON: {e}")))?;
    let devs = root
        .get("blockdevices")
        .and_then(Json::as_array)
        .ok_or_else(|| invalid("lsblk output has no blockdevices"))?;
    let mut disks = Vec::new();
    for d in devs {
        let name = jstr(d, "name").unwrap_or_default();
        let sector = ju64(d, "log-sec").unwrap_or(512);
        let kind = jstr(d, "type").unwrap_or_default();
        let mut disk = DiskInfo {
            path: jstr(d, "path").unwrap_or_else(|| format!("/dev/{name}")),
            size: ju64(d, "size").unwrap_or(0),
            model: jstr(d, "model"),
            sector_size: sector as u32,
            removable: jbool(d, "rm").unwrap_or(false) || jbool(d, "hotplug").unwrap_or(false),
            internal: None,
            virtual_disk: kind == "loop",
            partition_scheme: jstr(d, "pttype"),
            volumes: Vec::new(),
            id: name.clone(),
        };
        if let Some(fs) = jstr(d, "fstype") {
            disk.volumes.push(VolumeInfo {
                id: name,
                path: disk.path.clone(),
                offset: Some(0),
                size: disk.size,
                content: Some(kind),
                file_system: Some(fs),
                label: jstr(d, "label"),
                mount_point: lsblk_mount(d),
            });
        }
        lsblk_children(d, sector, &mut disk.volumes);
        disks.push(disk);
    }
    Ok(disks)
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

/// `ConvertTo-Json` turns one-element arrays into objects; undo that.
fn as_list(v: Option<&Json>) -> Vec<&Json> {
    match v {
        Some(Json::Array(a)) => a.iter().collect(),
        Some(Json::Null) | None => Vec::new(),
        Some(other) => vec![other],
    }
}

/// Parses the JSON produced by the `Get-Disk`/`Get-Partition`/`Get-Volume`
/// script: `{"disks":[…],"partitions":[…],"volumes":[…]}`.
pub fn parse_windows_storage(json: &str) -> Result<Vec<DiskInfo>> {
    let root: Json = serde_json::from_str(json.trim_start_matches('\u{feff}'))
        .map_err(|e| invalid(format!("bad PowerShell JSON: {e}")))?;
    let volumes = as_list(root.get("volumes"));
    let mut disks = Vec::new();
    for d in as_list(root.get("disks")) {
        let number = ju64(d, "Number").unwrap_or(0);
        let bus = jstr(d, "BusType").unwrap_or_default();
        let mut disk = DiskInfo {
            id: number.to_string(),
            path: format!(r"\\.\PhysicalDrive{number}"),
            size: ju64(d, "Size").unwrap_or(0),
            model: jstr(d, "FriendlyName").or_else(|| jstr(d, "Model")),
            sector_size: ju64(d, "LogicalSectorSize").unwrap_or(512) as u32,
            removable: matches!(bus.as_str(), "USB" | "SD" | "MMC"),
            internal: Some(!matches!(bus.as_str(), "USB" | "SD" | "MMC" | "1394")),
            virtual_disk: matches!(bus.as_str(), "File Backed Virtual" | "Virtual"),
            partition_scheme: jstr(d, "PartitionStyle"),
            volumes: Vec::new(),
        };
        for p in as_list(root.get("partitions"))
            .into_iter()
            .filter(|p| ju64(p, "DiskNumber") == Some(number))
        {
            let letter = jstr(p, "DriveLetter");
            let access: Vec<String> = as_list(p.get("AccessPaths"))
                .into_iter()
                .filter_map(|a| a.as_str().map(str::to_string))
                .collect();
            let vol = volumes.iter().find(|v| {
                jstr(v, "Path")
                    .is_some_and(|path| access.iter().any(|a| a.eq_ignore_ascii_case(&path)))
                    || (letter.is_some() && jstr(v, "DriveLetter") == letter)
            });
            let pn = ju64(p, "PartitionNumber").unwrap_or(0);
            disk.volumes.push(VolumeInfo {
                id: format!("{number}:{pn}"),
                path: vol.and_then(|v| jstr(v, "Path")).map_or_else(
                    || format!(r"\\?\GLOBALROOT\Device\Harddisk{number}\Partition{pn}"),
                    |p| p.trim_end_matches('\\').to_string(),
                ),
                offset: ju64(p, "Offset"),
                size: ju64(p, "Size").unwrap_or(0),
                content: jstr(p, "Type").or_else(|| jstr(p, "GptType")),
                file_system: vol.and_then(|v| jstr(v, "FileSystem")),
                label: vol.and_then(|v| jstr(v, "FileSystemLabel")),
                mount_point: letter.map(|l| format!("{l}:\\")),
            });
        }
        disks.push(disk);
    }
    Ok(disks)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>AllDisksAndPartitions</key><array>
 <dict><key>Content</key><string>GUID_partition_scheme</string>
  <key>DeviceIdentifier</key><string>disk0</string>
  <key>Partitions</key><array>
   <dict><key>Content</key><string>EFI</string><key>DeviceIdentifier</key><string>disk0s1</string><key>Size</key><integer>209715200</integer></dict>
   <dict><key>Content</key><string>Apple_APFS</string><key>DeviceIdentifier</key><string>disk0s2</string><key>Size</key><integer>1000000000</integer></dict>
  </array>
  <key>Size</key><integer>1209715200</integer></dict>
 <dict><key>APFSPhysicalStores</key><array><dict><key>DeviceIdentifier</key><string>disk0s2</string></dict></array>
  <key>APFSVolumes</key><array>
   <dict><key>DeviceIdentifier</key><string>disk1s1</string><key>MountPoint</key><string>/</string><key>Size</key><integer>1000000000</integer><key>VolumeName</key><string>Macintosh HD</string></dict>
  </array>
  <key>Content</key><string>EF57347C-0000-11AA-AA11-00306543ECAC</string>
  <key>DeviceIdentifier</key><string>disk1</string><key>Size</key><integer>1000000000</integer></dict>
 <dict><key>Content</key><string></string><key>DeviceIdentifier</key><string>disk4</string>
  <key>MountPoint</key><string>/Volumes/STICK</string><key>Size</key><integer>8000000</integer><key>VolumeName</key><string>STICK</string></dict>
</array></dict></plist>"#;

    fn info(id: &str) -> Option<Vec<u8>> {
        let body = match id {
            "disk0" => {
                "<key>DeviceBlockSize</key><integer>4096</integer><key>MediaName</key><string>APPLE SSD</string><key>Internal</key><true/><key>Removable</key><false/>"
            }
            "disk0s2" => {
                "<key>PartitionMapPartitionOffset</key><integer>209735680</integer><key>FilesystemName</key><string></string>"
            }
            "disk1s1" => "<key>FilesystemName</key><string>APFS</string>",
            "disk4" => {
                "<key>DeviceBlockSize</key><integer>512</integer><key>Ejectable</key><true/><key>FilesystemName</key><string>MS-DOS FAT32</string><key>Internal</key><false/>"
            }
            _ => return None,
        };
        Some(format!(r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict>{body}</dict></plist>"#).into_bytes())
    }

    #[test]
    fn diskutil_plist() {
        let disks = parse_diskutil(LIST.as_bytes(), &info).unwrap();
        assert_eq!(disks.len(), 3);
        let d0 = &disks[0];
        assert_eq!(d0.path, "/dev/rdisk0");
        assert_eq!(d0.sector_size, 4096);
        assert_eq!(d0.model.as_deref(), Some("APPLE SSD"));
        assert_eq!(d0.internal, Some(true));
        assert_eq!(d0.volumes.len(), 2);
        assert_eq!(d0.volumes[1].offset, Some(209_735_680));
        assert_eq!(d0.volumes[0].content.as_deref(), Some("EFI"));
        assert!(disks[1].virtual_disk);
        assert_eq!(disks[1].volumes[0].mount_point.as_deref(), Some("/"));
        assert_eq!(disks[1].volumes[0].file_system.as_deref(), Some("APFS"));
        let stick = &disks[2];
        assert!(stick.removable);
        assert_eq!(stick.volumes.len(), 1);
        assert_eq!(
            stick.volumes[0].file_system.as_deref(),
            Some("MS-DOS FAT32")
        );
        assert_eq!(stick.volumes[0].label.as_deref(), Some("STICK"));
    }

    #[test]
    fn lsblk_json_new_and_old_styles() {
        let new = r#"{"blockdevices":[{"name":"sda","path":"/dev/sda","size":500107862016,"type":"disk",
          "model":"Samsung SSD","rm":false,"hotplug":false,"log-sec":512,"pttype":"gpt","fstype":null,
          "mountpoints":[null],"children":[
            {"name":"sda1","path":"/dev/sda1","size":536870912,"type":"part","start":2048,"fstype":"vfat",
             "label":"EFI","parttypename":"EFI System","mountpoints":["/boot/efi"]},
            {"name":"sda2","path":"/dev/sda2","size":499570991104,"type":"part","start":1050624,"fstype":"ext4",
             "label":null,"mountpoints":["/"]}]}]}"#;
        let d = parse_lsblk(new).unwrap();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].model.as_deref(), Some("Samsung SSD"));
        assert_eq!(d[0].volumes.len(), 2);
        assert_eq!(d[0].volumes[0].offset, Some(2048 * 512));
        assert_eq!(d[0].volumes[0].mount_point.as_deref(), Some("/boot/efi"));
        assert_eq!(d[0].volumes[1].file_system.as_deref(), Some("ext4"));

        let old = r#"{"blockdevices":[{"name":"sdb","size":"8004304896","type":"disk","model":"Flash   ",
          "rm":"1","log-sec":"512","fstype":"vfat","label":"USB","mountpoint":"/media/usb"}]}"#;
        let d = parse_lsblk(old).unwrap();
        assert_eq!(d[0].path, "/dev/sdb");
        assert!(d[0].removable);
        assert_eq!(d[0].size, 8_004_304_896);
        assert_eq!(d[0].model.as_deref(), Some("Flash"));
        assert_eq!(d[0].volumes[0].mount_point.as_deref(), Some("/media/usb"));
    }

    #[test]
    fn windows_storage_json() {
        let json = r#"{"disks":{"Number":0,"FriendlyName":"NVMe Disk","Size":512110190592,
            "LogicalSectorSize":512,"BusType":"NVMe","PartitionStyle":"GPT"},
          "partitions":[
            {"DiskNumber":0,"PartitionNumber":1,"Offset":1048576,"Size":104857600,"DriveLetter":"\u0000",
             "Type":"System","AccessPaths":"\\\\?\\Volume{aaaa}\\"},
            {"DiskNumber":0,"PartitionNumber":2,"Offset":122683392,"Size":511000000000,"DriveLetter":"C",
             "Type":"Basic","AccessPaths":["C:\\","\\\\?\\Volume{bbbb}\\"]}],
          "volumes":[{"DriveLetter":"C","FileSystem":"NTFS","FileSystemLabel":"Windows","Size":511000000000,
             "Path":"\\\\?\\Volume{bbbb}\\"},
            {"DriveLetter":"","FileSystem":"FAT32","FileSystemLabel":"","Size":104857600,"Path":"\\\\?\\Volume{aaaa}\\"}]}"#;
        let d = parse_windows_storage(json).unwrap();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].path, r"\\.\PhysicalDrive0");
        assert!(!d[0].removable);
        let v = &d[0].volumes;
        assert_eq!(v[0].file_system.as_deref(), Some("FAT32"));
        assert_eq!(v[0].mount_point, None);
        assert_eq!(v[1].mount_point.as_deref(), Some("C:\\"));
        assert_eq!(v[1].label.as_deref(), Some("Windows"));
        assert_eq!(v[1].offset, Some(122_683_392));
    }
}
