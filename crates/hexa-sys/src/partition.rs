//! Partition tables: MBR (with extended/logical chains), GPT and the Apple
//! Partition Map (DISK-04).

use std::sync::Arc;

use hexa_core::Result;
use hexa_core::provider::{Provider, RangeProvider};
use serde::Serialize;

use crate::util::{ascii, be16, be32, crc32, guid_string, invalid, le32, le64, read_vec, utf16le};

/// Kind of partition table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TableKind {
    Mbr,
    Gpt,
    Apm,
}

/// One partition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Partition {
    /// 1-based number (`sda5` → 5; logical MBR partitions start at 5).
    pub number: u32,
    /// Byte offset on the disk.
    pub offset: u64,
    /// Length in bytes.
    pub size: u64,
    pub start_lba: u64,
    /// `0x07`, a GPT type GUID, or an APM type string.
    pub type_id: String,
    /// Human-readable type.
    pub type_name: String,
    pub name: Option<String>,
    /// GPT unique partition GUID.
    pub guid: Option<String>,
    pub bootable: bool,
    /// Logical partition inside an MBR extended partition.
    pub logical: bool,
    /// GPT attribute bits / APM status.
    pub attributes: u64,
    /// Byte offset of the table entry describing this partition.
    pub entry_offset: u64,
}

/// Details of a GPT header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GptHeader {
    pub disk_guid: String,
    pub current_lba: u64,
    pub backup_lba: u64,
    pub first_usable_lba: u64,
    pub last_usable_lba: u64,
    pub entries_lba: u64,
    pub entry_count: u32,
    pub entry_size: u32,
    pub header_crc_ok: bool,
    pub entries_crc_ok: bool,
    /// The primary header was damaged and the backup was used.
    pub from_backup: bool,
}

/// A parsed partition table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PartitionTable {
    pub kind: TableKind,
    pub sector_size: u32,
    pub partitions: Vec<Partition>,
    pub gpt: Option<GptHeader>,
    /// MBR disk signature.
    pub disk_signature: Option<u32>,
    /// Non-fatal problems (bad CRC, loops in the EBR chain …).
    pub warnings: Vec<String>,
}

impl PartitionTable {
    /// A provider over one partition of `disk`.
    pub fn open_partition(
        disk: Arc<dyn Provider>,
        part: &Partition,
    ) -> std::io::Result<RangeProvider> {
        let size = part.size.min(disk.len().saturating_sub(part.offset));
        RangeProvider::new(disk, part.offset, size)
    }
}

/// Detects and parses the partition table of `disk`. `sector_size` of `None`
/// tries the provider's sector size, then 512 and 4096 for GPT.
pub fn read_partition_table(
    disk: &dyn Provider,
    sector_size: Option<u32>,
) -> Result<Option<PartitionTable>> {
    if disk.len() < 512 {
        return Ok(None);
    }
    let hint = sector_size.or(disk.capabilities().sector_size);
    // APM: driver descriptor "ER" at block 0, "PM" at block 1.
    let b0 = read_vec(disk, 0, 512)?;
    if (&b0[0..2] == b"ER" || (disk.len() >= 1024 && &read_vec(disk, 512, 2)? == b"PM"))
        && let Some(t) = read_apm(disk)?
    {
        return Ok(Some(t));
    }
    if b0[510] != 0x55 || b0[511] != 0xAA {
        return Ok(None);
    }
    let protective = (0..4).any(|i| b0[446 + i * 16 + 4] == 0xEE);
    if protective {
        let sizes: Vec<u32> = match hint {
            Some(s) => vec![s],
            None => vec![512, 4096],
        };
        for ss in sizes {
            if let Some(t) = read_gpt(disk, ss)? {
                return Ok(Some(t));
            }
        }
    }
    read_mbr(disk, hint.unwrap_or(512)).map(Some)
}

// ---------------------------------------------------------------------------
// MBR
// ---------------------------------------------------------------------------

fn is_extended(t: u8) -> bool {
    matches!(t, 0x05 | 0x0F | 0x85)
}

/// Parses an MBR with its extended/logical chain.
pub fn read_mbr(disk: &dyn Provider, sector_size: u32) -> Result<PartitionTable> {
    let ss = u64::from(sector_size);
    let mbr = read_vec(disk, 0, 512)?;
    if mbr[510] != 0x55 || mbr[511] != 0xAA {
        return Err(invalid("no MBR boot signature"));
    }
    let mut table = PartitionTable {
        kind: TableKind::Mbr,
        sector_size,
        partitions: Vec::new(),
        gpt: None,
        disk_signature: Some(le32(&mbr, 440)),
        warnings: Vec::new(),
    };
    let mut extended = None;
    for i in 0..4 {
        let e = 446 + i * 16;
        let kind = mbr[e + 4];
        let start = u64::from(le32(&mbr, e + 8));
        let count = u64::from(le32(&mbr, e + 12));
        if kind == 0 || count == 0 {
            continue;
        }
        table.partitions.push(mbr_partition(
            i as u32 + 1,
            &mbr[e..e + 16],
            start,
            ss,
            false,
            e as u64,
        ));
        if is_extended(kind) && extended.is_none() {
            extended = Some((start, count));
        }
    }
    if let Some((ext_start, ext_len)) = extended {
        let mut ebr_lba = ext_start;
        let mut number = 5;
        let mut seen = std::collections::HashSet::new();
        loop {
            if !seen.insert(ebr_lba) || seen.len() > 256 {
                table
                    .warnings
                    .push("loop in extended partition chain".into());
                break;
            }
            let off = ebr_lba * ss;
            if off + 512 > disk.len() {
                table
                    .warnings
                    .push(format!("EBR at LBA {ebr_lba} is past the end"));
                break;
            }
            let ebr = read_vec(disk, off, 512)?;
            if ebr[510] != 0x55 || ebr[511] != 0xAA {
                table
                    .warnings
                    .push(format!("EBR at LBA {ebr_lba} has no signature"));
                break;
            }
            let e = &ebr[446..462];
            if e[4] != 0 && le32(e, 12) != 0 {
                let start = ebr_lba + u64::from(le32(e, 8));
                table
                    .partitions
                    .push(mbr_partition(number, e, start, ss, true, off + 446));
                number += 1;
            }
            let link = &ebr[462..478];
            if !is_extended(link[4]) || le32(link, 12) == 0 {
                break;
            }
            let next = u64::from(le32(link, 8));
            if next >= ext_len {
                table
                    .warnings
                    .push("EBR link outside the extended partition".into());
                break;
            }
            ebr_lba = ext_start + next;
        }
    }
    Ok(table)
}

fn mbr_partition(
    number: u32,
    e: &[u8],
    start: u64,
    ss: u64,
    logical: bool,
    entry_offset: u64,
) -> Partition {
    let kind = e[4];
    Partition {
        number,
        offset: start * ss,
        size: u64::from(le32(e, 12)) * ss,
        start_lba: start,
        type_id: format!("0x{kind:02X}"),
        type_name: mbr_type_name(kind).to_string(),
        name: None,
        guid: None,
        bootable: e[0] == 0x80,
        logical,
        attributes: u64::from(e[0]),
        entry_offset,
    }
}

/// Name of an MBR partition type byte.
pub fn mbr_type_name(t: u8) -> &'static str {
    match t {
        0x00 => "Empty",
        0x01 => "FAT12",
        0x04 => "FAT16 <32M",
        0x05 => "Extended",
        0x06 => "FAT16",
        0x07 => "NTFS/exFAT/HPFS",
        0x0B => "FAT32 (CHS)",
        0x0C => "FAT32 (LBA)",
        0x0E => "FAT16 (LBA)",
        0x0F => "Extended (LBA)",
        0x11 => "Hidden FAT12",
        0x12 => "Compaq diagnostics / recovery",
        0x14 => "Hidden FAT16 <32M",
        0x16 => "Hidden FAT16",
        0x17 => "Hidden NTFS",
        0x1B => "Hidden FAT32",
        0x1C => "Hidden FAT32 (LBA)",
        0x1E => "Hidden FAT16 (LBA)",
        0x27 => "Windows RE",
        0x42 => "Windows dynamic disk",
        0x82 => "Linux swap / Solaris",
        0x83 => "Linux",
        0x85 => "Linux extended",
        0x8E => "Linux LVM",
        0xA5 => "FreeBSD",
        0xA6 => "OpenBSD",
        0xA8 => "Darwin UFS",
        0xA9 => "NetBSD",
        0xAB => "Darwin boot",
        0xAF => "HFS / HFS+",
        0xEE => "GPT protective",
        0xEF => "EFI System",
        0xFB => "VMware VMFS",
        0xFD => "Linux RAID autodetect",
        _ => "Unknown",
    }
}

// ---------------------------------------------------------------------------
// GPT
// ---------------------------------------------------------------------------

fn parse_gpt_header(h: &[u8]) -> Option<(GptHeader, bool)> {
    if &h[0..8] != b"EFI PART" {
        return None;
    }
    let size = le32(h, 12) as usize;
    if !(92..=h.len()).contains(&size) {
        return None;
    }
    let mut copy = h[..size].to_vec();
    copy[16..20].fill(0);
    let crc_ok = crc32(&copy) == le32(h, 16);
    Some((
        GptHeader {
            disk_guid: guid_string(&h[56..72]),
            current_lba: le64(h, 24),
            backup_lba: le64(h, 32),
            first_usable_lba: le64(h, 40),
            last_usable_lba: le64(h, 48),
            entries_lba: le64(h, 72),
            entry_count: le32(h, 80),
            entry_size: le32(h, 84),
            header_crc_ok: crc_ok,
            entries_crc_ok: false,
            from_backup: false,
        },
        crc_ok,
    ))
}

/// Parses a GPT using `sector_size`; falls back to the backup header at the
/// last LBA when the primary is damaged.
pub fn read_gpt(disk: &dyn Provider, sector_size: u32) -> Result<Option<PartitionTable>> {
    let ss = u64::from(sector_size);
    if disk.len() < ss * 3 {
        return Ok(None);
    }
    let mut warnings = Vec::new();
    let primary = parse_gpt_header(&read_vec(disk, ss, sector_size as usize)?);
    let last_lba = disk.len() / ss - 1;
    let (mut header, entries_crc) = match primary {
        Some((h, true)) => {
            let crc = le32(&read_vec(disk, ss, 92)?, 88);
            (h, crc)
        }
        other => {
            let backup_raw = read_vec(disk, last_lba * ss, sector_size as usize)?;
            match parse_gpt_header(&backup_raw) {
                Some((mut h, true)) => {
                    warnings.push("primary GPT header is damaged; using the backup".into());
                    h.from_backup = true;
                    (h, le32(&backup_raw, 88))
                }
                _ => match other {
                    Some((h, false)) => {
                        warnings.push("GPT header CRC mismatch".into());
                        let crc = le32(&read_vec(disk, ss, 92)?, 88);
                        (h, crc)
                    }
                    _ => return Ok(None),
                },
            }
        }
    };
    let esize = header.entry_size as usize;
    let count = header.entry_count.min(4096) as usize;
    if !(128..=4096).contains(&esize) {
        return Err(invalid(format!("unsupported GPT entry size {esize}")));
    }
    let entries_off = header.entries_lba * ss;
    let total = esize * count;
    if entries_off + total as u64 > disk.len() {
        return Err(invalid("GPT entry array is past the end of the disk"));
    }
    let entries = read_vec(disk, entries_off, total)?;
    header.entries_crc_ok = crc32(&entries) == entries_crc;
    if !header.entries_crc_ok {
        warnings.push("GPT partition entry array CRC mismatch".into());
    }
    let mut partitions = Vec::new();
    for i in 0..count {
        let e = &entries[i * esize..(i + 1) * esize];
        if e[..16].iter().all(|&b| b == 0) {
            continue;
        }
        let first = le64(e, 32);
        let last = le64(e, 40);
        let type_guid = guid_string(&e[0..16]);
        let attributes = le64(e, 48);
        partitions.push(Partition {
            number: i as u32 + 1,
            offset: first * ss,
            size: (last.saturating_sub(first) + 1) * ss,
            start_lba: first,
            type_name: gpt_type_name(&type_guid).unwrap_or("Unknown").to_string(),
            type_id: type_guid,
            name: Some(utf16le(&e[56..128])).filter(|s| !s.is_empty()),
            guid: Some(guid_string(&e[16..32])),
            bootable: attributes & 4 != 0,
            logical: false,
            attributes,
            entry_offset: entries_off + (i * esize) as u64,
        });
    }
    Ok(Some(PartitionTable {
        kind: TableKind::Gpt,
        sector_size,
        partitions,
        gpt: Some(header),
        disk_signature: None,
        warnings,
    }))
}

/// Name of a GPT partition type GUID.
pub fn gpt_type_name(guid: &str) -> Option<&'static str> {
    const TYPES: &[(&str, &str)] = &[
        ("C12A7328-F81F-11D2-BA4B-00A0C93EC93B", "EFI System"),
        (
            "024DEE41-33E7-11D3-9D69-0008C781F39F",
            "MBR partition scheme",
        ),
        ("21686148-6449-6E6F-744E-656564454649", "BIOS boot"),
        ("E3C9E316-0B5C-4DB8-817D-F92DF00215AE", "Microsoft reserved"),
        (
            "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7",
            "Microsoft basic data",
        ),
        ("5808C8AA-7E8F-42E0-85D2-E1E90434CFB3", "LDM metadata"),
        ("AF9B60A0-1431-4F62-BC68-3311714A69AD", "LDM data"),
        ("DE94BBA4-06D1-4D40-A16A-BFD50179D6AC", "Windows recovery"),
        ("E75CAF8F-F680-4CEE-AFA3-B001E56EFC2D", "Storage Spaces"),
        ("0FC63DAF-8483-4772-8E79-3D69D8477DE4", "Linux filesystem"),
        ("0657FD6D-A4AB-43C4-84E5-0933C84B4F4F", "Linux swap"),
        ("E6D6D379-F507-44C2-A23C-238F2A3DF928", "Linux LVM"),
        ("A19D880F-05FC-4D3B-A006-743F0F84911E", "Linux RAID"),
        ("933AC7E1-2EB4-4F13-B844-0E14E2AEF915", "Linux home"),
        (
            "4F68BCE3-E8CD-4DB1-96E7-FBCAF984B709",
            "Linux root (x86-64)",
        ),
        ("B921B045-1DF0-41C3-AF44-4C6F280D3FAE", "Linux root (ARM64)"),
        (
            "BC13C2FF-59E6-4262-A352-B275FD6F7172",
            "Linux extended boot",
        ),
        ("CA7D7CCB-63ED-4C53-861C-1742536059CC", "LUKS"),
        ("48465300-0000-11AA-AA11-00306543ECAC", "Apple HFS+"),
        ("7C3457EF-0000-11AA-AA11-00306543ECAC", "Apple APFS"),
        ("55465300-0000-11AA-AA11-00306543ECAC", "Apple UFS"),
        ("426F6F74-0000-11AA-AA11-00306543ECAC", "Apple boot"),
        ("52414944-0000-11AA-AA11-00306543ECAC", "Apple RAID"),
        ("4C616265-6C00-11AA-AA11-00306543ECAC", "Apple label"),
        ("69646961-6700-11AA-AA11-00306543ECAC", "Apple ISC"),
        ("52637672-7900-11AA-AA11-00306543ECAC", "Apple recovery"),
        ("516E7CB4-6ECF-11D6-8FF8-00022D09712B", "FreeBSD data"),
        ("83BD6B9D-7F41-11DC-BE0B-001560B84F0F", "FreeBSD boot"),
        ("516E7CB5-6ECF-11D6-8FF8-00022D09712B", "FreeBSD swap"),
        ("516E7CB6-6ECF-11D6-8FF8-00022D09712B", "FreeBSD UFS"),
        ("516E7CBA-6ECF-11D6-8FF8-00022D09712B", "FreeBSD ZFS"),
        ("6A898CC3-1DD2-11B2-99A6-080020736631", "ZFS / Apple ZFS"),
        ("AA31E02A-400F-11DB-9590-000C2911D1B8", "VMware VMFS"),
        ("FE3A2A5D-4F32-41A7-B725-ACCC3285A309", "ChromeOS kernel"),
        ("3CB8E202-3B7E-47DD-8A3C-7FF2A13CFCEC", "ChromeOS root"),
    ];
    TYPES
        .iter()
        .find(|(g, _)| g.eq_ignore_ascii_case(guid))
        .map(|(_, n)| *n)
}

// ---------------------------------------------------------------------------
// Apple Partition Map
// ---------------------------------------------------------------------------

/// Parses an Apple Partition Map.
pub fn read_apm(disk: &dyn Provider) -> Result<Option<PartitionTable>> {
    let b0 = read_vec(disk, 0, 512)?;
    let block = if &b0[0..2] == b"ER" {
        match be16(&b0, 2) {
            bs @ (512 | 1024 | 2048 | 4096) => u64::from(bs),
            _ => 512,
        }
    } else {
        512
    };
    if disk.len() < block * 2 {
        return Ok(None);
    }
    let first = read_vec(disk, block, 512)?;
    if &first[0..2] != b"PM" {
        return Ok(None);
    }
    let count = be32(&first, 4).min(256);
    let mut partitions = Vec::new();
    for i in 0..count {
        let off = block * (1 + u64::from(i));
        if off + 512 > disk.len() {
            break;
        }
        let e = read_vec(disk, off, 512)?;
        if &e[0..2] != b"PM" {
            break;
        }
        let start = u64::from(be32(&e, 8));
        let blocks = u64::from(be32(&e, 12));
        let kind = ascii(&e[48..80]);
        partitions.push(Partition {
            number: i + 1,
            offset: start * block,
            size: blocks * block,
            start_lba: start,
            type_name: apm_type_name(&kind).to_string(),
            type_id: kind,
            name: Some(ascii(&e[16..48])).filter(|s| !s.is_empty()),
            guid: None,
            bootable: be32(&e, 88) & 0x8 != 0,
            logical: false,
            attributes: u64::from(be32(&e, 88)),
            entry_offset: off,
        });
    }
    Ok(Some(PartitionTable {
        kind: TableKind::Apm,
        sector_size: block as u32,
        partitions,
        gpt: None,
        disk_signature: None,
        warnings: Vec::new(),
    }))
}

fn apm_type_name(t: &str) -> &'static str {
    match t {
        "Apple_partition_map" => "Partition map",
        "Apple_Driver" | "Apple_Driver43" | "Apple_Driver_ATA" | "Apple_Driver_ATAPI" => "Driver",
        "Apple_HFS" => "HFS / HFS+",
        "Apple_HFSX" => "HFSX",
        "Apple_UFS" => "UFS",
        "Apple_Free" => "Free space",
        "Apple_Patches" => "Patches",
        "Apple_Boot" => "Boot",
        "Apple_Bootstrap" => "Bootstrap",
        "Apple_APFS" => "APFS",
        "DOS_FAT_32" | "DOS_FAT_16" | "DOS_FAT_12" => "FAT",
        "Linux" | "Apple_UNIX_SVR2" => "Unix / Linux",
        _ => "Unknown",
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::util::guid_bytes;
    use hexa_core::provider::MemoryProvider;

    fn mbr_entry(buf: &mut [u8], at: usize, boot: u8, kind: u8, start: u32, count: u32) {
        buf[at] = boot;
        buf[at + 4] = kind;
        buf[at + 8..at + 12].copy_from_slice(&start.to_le_bytes());
        buf[at + 12..at + 16].copy_from_slice(&count.to_le_bytes());
    }

    #[test]
    fn mbr_with_logical_chain() {
        let mut d = vec![0u8; 512 * 400];
        d[510] = 0x55;
        d[511] = 0xAA;
        mbr_entry(&mut d, 446, 0x80, 0x0C, 1, 99);
        mbr_entry(&mut d, 462, 0, 0x0F, 100, 300);
        // EBR 1 at LBA 100: logical at +10 (len 50), link to +100.
        let e1 = 100 * 512;
        d[e1 + 510] = 0x55;
        d[e1 + 511] = 0xAA;
        mbr_entry(&mut d, e1 + 446, 0, 0x83, 10, 50);
        mbr_entry(&mut d, e1 + 462, 0, 0x05, 100, 120);
        // EBR 2 at LBA 200.
        let e2 = 200 * 512;
        d[e2 + 510] = 0x55;
        d[e2 + 511] = 0xAA;
        mbr_entry(&mut d, e2 + 446, 0, 0x07, 2, 40);
        let p = MemoryProvider::new("d", d);
        let t = read_partition_table(&p, None).unwrap().unwrap();
        assert_eq!(t.kind, TableKind::Mbr);
        assert_eq!(t.partitions.len(), 4);
        assert!(t.partitions[0].bootable);
        assert_eq!(t.partitions[0].type_name, "FAT32 (LBA)");
        let l1 = &t.partitions[2];
        assert!(l1.logical);
        assert_eq!(l1.number, 5);
        assert_eq!(l1.offset, 110 * 512);
        let l2 = &t.partitions[3];
        assert_eq!(l2.number, 6);
        assert_eq!(l2.offset, 202 * 512);
        assert_eq!(l2.size, 40 * 512);
        assert_eq!(t.warnings.len(), 0);
    }

    /// Builds a GPT disk of `sectors` 512-byte sectors with the given
    /// `(type guid, first, last, name)` partitions.
    pub(crate) fn gpt_disk(sectors: u64, parts: &[(&str, u64, u64, &str)]) -> Vec<u8> {
        let mut d = vec![0u8; (sectors * 512) as usize];
        d[510] = 0x55;
        d[511] = 0xAA;
        mbr_entry(&mut d, 446, 0, 0xEE, 1, (sectors - 1) as u32);
        let mut entries = vec![0u8; 128 * 128];
        for (i, (ty, first, last, name)) in parts.iter().enumerate() {
            let e = &mut entries[i * 128..(i + 1) * 128];
            e[0..16].copy_from_slice(&guid_bytes(ty).unwrap());
            e[16..32].copy_from_slice(&[i as u8 + 1; 16]);
            e[32..40].copy_from_slice(&first.to_le_bytes());
            e[40..48].copy_from_slice(&last.to_le_bytes());
            for (j, u) in name.encode_utf16().enumerate() {
                e[56 + j * 2..58 + j * 2].copy_from_slice(&u.to_le_bytes());
            }
        }
        let ecrc = crc32(&entries);
        let write_header = |d: &mut Vec<u8>, lba: u64, backup: u64, entries_lba: u64| {
            let mut h = vec![0u8; 92];
            h[0..8].copy_from_slice(b"EFI PART");
            h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
            h[12..16].copy_from_slice(&92u32.to_le_bytes());
            h[24..32].copy_from_slice(&lba.to_le_bytes());
            h[32..40].copy_from_slice(&backup.to_le_bytes());
            h[40..48].copy_from_slice(&34u64.to_le_bytes());
            h[48..56].copy_from_slice(&(sectors - 34).to_le_bytes());
            h[56..72].copy_from_slice(&[0xAB; 16]);
            h[72..80].copy_from_slice(&entries_lba.to_le_bytes());
            h[80..84].copy_from_slice(&128u32.to_le_bytes());
            h[84..88].copy_from_slice(&128u32.to_le_bytes());
            h[88..92].copy_from_slice(&ecrc.to_le_bytes());
            let crc = crc32(&h);
            h[16..20].copy_from_slice(&crc.to_le_bytes());
            let at = (lba * 512) as usize;
            d[at..at + 92].copy_from_slice(&h);
            let e = (entries_lba * 512) as usize;
            d[e..e + entries.len()].copy_from_slice(&entries);
        };
        write_header(&mut d, 1, sectors - 1, 2);
        write_header(&mut d, sectors - 1, 1, sectors - 33);
        d
    }

    #[test]
    fn gpt_and_backup_header() {
        let d = gpt_disk(
            200,
            &[
                ("C12A7328-F81F-11D2-BA4B-00A0C93EC93B", 34, 63, "EFI"),
                ("0FC63DAF-8483-4772-8E79-3D69D8477DE4", 64, 150, "root"),
            ],
        );
        let p = MemoryProvider::new("d", d.clone());
        let t = read_partition_table(&p, None).unwrap().unwrap();
        assert_eq!(t.kind, TableKind::Gpt);
        let h = t.gpt.as_ref().unwrap();
        assert!(h.header_crc_ok && h.entries_crc_ok && !h.from_backup);
        assert_eq!(t.partitions.len(), 2);
        assert_eq!(t.partitions[0].type_name, "EFI System");
        assert_eq!(t.partitions[1].name.as_deref(), Some("root"));
        assert_eq!(t.partitions[1].offset, 64 * 512);
        assert_eq!(t.partitions[1].size, 87 * 512);

        // Corrupt the primary header: the backup takes over.
        let mut bad = d;
        bad[512 + 40] ^= 0xFF;
        let p = MemoryProvider::new("d", bad);
        let t = read_partition_table(&p, None).unwrap().unwrap();
        assert!(t.gpt.as_ref().unwrap().from_backup);
        assert_eq!(t.partitions.len(), 2);
        assert_ne!(t.warnings.len(), 0);
    }

    #[test]
    fn apple_partition_map() {
        let mut d = vec![0u8; 512 * 64];
        d[0..2].copy_from_slice(b"ER");
        d[2..4].copy_from_slice(&512u16.to_be_bytes());
        let entry = |d: &mut Vec<u8>, i: usize, start: u32, len: u32, name: &str, ty: &str| {
            let o = 512 * (1 + i);
            d[o..o + 2].copy_from_slice(b"PM");
            d[o + 4..o + 8].copy_from_slice(&2u32.to_be_bytes());
            d[o + 8..o + 12].copy_from_slice(&start.to_be_bytes());
            d[o + 12..o + 16].copy_from_slice(&len.to_be_bytes());
            d[o + 16..o + 16 + name.len()].copy_from_slice(name.as_bytes());
            d[o + 48..o + 48 + ty.len()].copy_from_slice(ty.as_bytes());
        };
        entry(&mut d, 0, 1, 63, "Apple", "Apple_partition_map");
        entry(&mut d, 1, 10, 40, "Macintosh HD", "Apple_HFS");
        let p = MemoryProvider::new("d", d);
        let t = read_partition_table(&p, None).unwrap().unwrap();
        assert_eq!(t.kind, TableKind::Apm);
        assert_eq!(t.partitions.len(), 2);
        assert_eq!(t.partitions[1].type_name, "HFS / HFS+");
        assert_eq!(t.partitions[1].offset, 10 * 512);
        assert_eq!(t.partitions[1].name.as_deref(), Some("Macintosh HD"));
    }
}
