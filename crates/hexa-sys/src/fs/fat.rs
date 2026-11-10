//! FAT12/16/32 (DISK-05/06/07): directory tree with long names, cluster
//! chains as extents, and deleted directory entries.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, OnceLock};

use hexa_core::provider::Provider;
use hexa_core::{Cancel, Result};

use super::{
    Extent, FileEntry, FileSystem, FsInfo, FsKind, InfoField, Region, dos_time, push_extent,
    truncate_extents,
};
use crate::util::{ascii, byte, check_cancel, invalid, le16, le32, read_vec};

/// Directory entry attribute bits.
const ATTR_VOLUME: u8 = 0x08;
const ATTR_DIR: u8 = 0x10;
const ATTR_LFN: u8 = 0x0F;

/// A FAT12/16/32 volume.
#[derive(Debug)]
pub struct FatFs {
    p: Arc<dyn Provider>,
    kind: FsKind,
    bytes_per_sector: u32,
    cluster_size: u64,
    reserved_bytes: u64,
    fat_count: u32,
    fat_bytes: u64,
    root_dir_offset: u64,
    root_dir_bytes: u64,
    data_offset: u64,
    cluster_count: u32,
    root_cluster: u32,
    label: Option<String>,
    serial: u32,
    fat: OnceLock<Vec<u8>>,
}

impl FatFs {
    /// Whether `boot` looks like a FAT boot sector.
    pub fn detect(boot: &[u8]) -> bool {
        let bps = le16(boot, 11);
        let spc = byte(boot, 13);
        boot.len() >= 512
            && boot[510] == 0x55
            && boot[511] == 0xAA
            && matches!(bps, 512 | 1024 | 2048 | 4096)
            && spc.is_power_of_two()
            && le16(boot, 14) >= 1
            && matches!(byte(boot, 16), 1 | 2)
            && (le16(boot, 22) != 0 || le32(boot, 36) != 0)
            && (le16(boot, 19) != 0 || le32(boot, 32) != 0)
    }

    pub fn open(p: Arc<dyn Provider>) -> Result<Self> {
        let b = read_vec(p.as_ref(), 0, 512)?;
        if !Self::detect(&b) {
            return Err(invalid("not a FAT boot sector"));
        }
        let bps = u64::from(le16(&b, 11));
        let spc = u64::from(b[13]);
        let reserved = u64::from(le16(&b, 14));
        let fats = u64::from(b[16]);
        let root_entries = u64::from(le16(&b, 17));
        let total = match le16(&b, 19) {
            0 => u64::from(le32(&b, 32)),
            n => u64::from(n),
        };
        let fat_sectors = match le16(&b, 22) {
            0 => u64::from(le32(&b, 36)),
            n => u64::from(n),
        };
        let root_sectors = (root_entries * 32).div_ceil(bps);
        let data_start = reserved + fats * fat_sectors + root_sectors;
        if data_start >= total {
            return Err(invalid("FAT data area starts past the end"));
        }
        let clusters = (total - data_start) / spc;
        let kind = if clusters < 4085 {
            FsKind::Fat12
        } else if clusters < 65_525 {
            FsKind::Fat16
        } else {
            FsKind::Fat32
        };
        let (label_off, serial_off) = if kind == FsKind::Fat32 {
            (71, 67)
        } else {
            (43, 39)
        };
        let fs = Self {
            kind,
            bytes_per_sector: bps as u32,
            cluster_size: bps * spc,
            reserved_bytes: reserved * bps,
            fat_count: fats as u32,
            fat_bytes: fat_sectors * bps,
            root_dir_offset: (reserved + fats * fat_sectors) * bps,
            root_dir_bytes: root_sectors * bps,
            data_offset: data_start * bps,
            cluster_count: clusters as u32,
            root_cluster: if kind == FsKind::Fat32 {
                le32(&b, 44)
            } else {
                0
            },
            label: Some(ascii(&b[label_off..label_off + 11]))
                .filter(|s| !s.is_empty() && s != "NO NAME"),
            serial: le32(&b, serial_off),
            fat: OnceLock::new(),
            p,
        };
        Ok(fs)
    }

    pub fn kind(&self) -> FsKind {
        self.kind
    }

    pub fn cluster_size(&self) -> u64 {
        self.cluster_size
    }

    /// Byte offset of a data cluster.
    pub fn cluster_offset(&self, cluster: u32) -> u64 {
        self.data_offset + u64::from(cluster.saturating_sub(2)) * self.cluster_size
    }

    /// Data cluster holding a byte offset.
    pub fn cluster_at(&self, offset: u64) -> Option<u32> {
        if offset < self.data_offset {
            return None;
        }
        let c = (offset - self.data_offset) / self.cluster_size + 2;
        (c < u64::from(self.cluster_count) + 2).then_some(c as u32)
    }

    fn fat(&self) -> Result<&Vec<u8>> {
        if let Some(f) = self.fat.get() {
            return Ok(f);
        }
        let f = read_vec(
            self.p.as_ref(),
            self.reserved_bytes,
            self.fat_bytes as usize,
        )?;
        Ok(self.fat.get_or_init(|| f))
    }

    /// Raw FAT entry of `cluster`.
    pub fn fat_entry(&self, cluster: u32) -> Result<u32> {
        let fat = self.fat()?;
        let c = cluster as usize;
        Ok(match self.kind {
            FsKind::Fat12 => {
                let v = le16(fat, c + c / 2);
                u32::from(if c & 1 == 1 { v >> 4 } else { v & 0xFFF })
            }
            FsKind::Fat16 => u32::from(le16(fat, c * 2)),
            _ => le32(fat, c * 4) & 0x0FFF_FFFF,
        })
    }

    fn is_end(&self, v: u32) -> bool {
        match self.kind {
            FsKind::Fat12 => v >= 0xFF7,
            FsKind::Fat16 => v >= 0xFFF7,
            _ => v >= 0x0FFF_FFF7,
        }
    }

    /// The cluster chain starting at `first`.
    pub fn chain(&self, first: u32) -> Result<Vec<u32>> {
        let max = self.cluster_count + 2;
        let mut out = Vec::new();
        let mut c = first;
        let mut seen = HashSet::new();
        while c >= 2 && c < max {
            if !seen.insert(c) {
                break;
            }
            out.push(c);
            let next = self.fat_entry(c)?;
            if next < 2 || self.is_end(next) {
                break;
            }
            c = next;
        }
        Ok(out)
    }

    fn is_fixed_root(&self, dir: &FileEntry) -> bool {
        dir.id == 0 && self.kind != FsKind::Fat32
    }

    /// Bytes of a directory plus the physical offset of every cluster.
    fn dir_data(&self, dir: &FileEntry) -> Result<(Vec<u8>, Vec<u64>, u64)> {
        if self.is_fixed_root(dir) {
            let data = read_vec(
                self.p.as_ref(),
                self.root_dir_offset,
                self.root_dir_bytes as usize,
            )?;
            return Ok((data, vec![self.root_dir_offset], self.root_dir_bytes.max(1)));
        }
        let clusters = if dir.deleted {
            vec![dir.id as u32]
        } else {
            self.chain(dir.id as u32)?
        };
        let mut data = Vec::with_capacity(clusters.len() * self.cluster_size as usize);
        let mut locs = Vec::with_capacity(clusters.len());
        for c in clusters {
            let off = self.cluster_offset(c);
            if off + self.cluster_size > self.p.len() {
                break;
            }
            locs.push(off);
            data.extend(read_vec(self.p.as_ref(), off, self.cluster_size as usize)?);
        }
        Ok((data, locs, self.cluster_size))
    }

    fn parse_dir(&self, dir: &FileEntry, want_deleted: bool) -> Result<Vec<FileEntry>> {
        let (data, locs, chunk) = self.dir_data(dir)?;
        let mut out = Vec::new();
        let mut lfn: Vec<(u8, [u16; 13])> = Vec::new();
        for (i, e) in data.as_chunks::<32>().0.iter().enumerate() {
            let first = e[0];
            if first == 0 {
                break;
            }
            let deleted = first == 0xE5;
            let attr = e[11];
            if attr & 0x3F == ATTR_LFN {
                let mut units = [0u16; 13];
                for (k, pos) in [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30]
                    .iter()
                    .enumerate()
                {
                    units[k] = le16(e, *pos);
                }
                if deleted == want_deleted || !want_deleted {
                    lfn.push((e[13], units));
                }
                continue;
            }
            let parts = std::mem::take(&mut lfn);
            if deleted != want_deleted || attr & ATTR_VOLUME != 0 {
                continue;
            }
            let short = short_name(e, deleted);
            if short == "." || short == ".." {
                continue;
            }
            let sum = lfn_checksum(&e[0..11]);
            let long = if !parts.is_empty() && parts.iter().all(|(c, _)| *c == sum) {
                let units: Vec<u16> = parts
                    .iter()
                    .rev()
                    .flat_map(|(_, u)| u.iter().copied())
                    .take_while(|&u| u != 0 && u != 0xFFFF)
                    .collect();
                Some(String::from_utf16_lossy(&units))
            } else {
                None
            };
            let name = long.unwrap_or(short);
            let hi = if self.kind == FsKind::Fat32 {
                u32::from(le16(e, 20)) << 16
            } else {
                0
            };
            let cluster = hi | u32::from(le16(e, 26));
            let byte_pos = (i * 32) as u64;
            let loc = locs
                .get((byte_pos / chunk) as usize)
                .map(|l| l + byte_pos % chunk);
            out.push(FileEntry {
                id: u64::from(cluster),
                aux: 0,
                path: FileEntry::child_path(&dir.path, &name),
                name,
                is_dir: attr & ATTR_DIR != 0,
                size: u64::from(le32(e, 28)),
                deleted,
                metadata_offset: loc,
                modified: dos_time(le16(e, 24), le16(e, 22)),
                created: dos_time(le16(e, 16), le16(e, 14)),
                attributes: u32::from(attr),
            });
        }
        Ok(out)
    }
}

fn short_name(e: &[u8], deleted: bool) -> String {
    let lower_base = e[12] & 0x08 != 0;
    let lower_ext = e[12] & 0x10 != 0;
    let conv = |b: &[u8], lower: bool| -> String {
        let s: String = b
            .iter()
            .map(|&c| c as char)
            .collect::<String>()
            .trim_end()
            .to_string();
        if lower { s.to_lowercase() } else { s }
    };
    let mut base_bytes = e[0..8].to_vec();
    if deleted {
        base_bytes[0] = b'_';
    } else if base_bytes[0] == 0x05 {
        base_bytes[0] = 0xE5;
    }
    let base = conv(&base_bytes, lower_base);
    let ext = conv(&e[8..11], lower_ext);
    if ext.is_empty() {
        base
    } else {
        format!("{base}.{ext}")
    }
}

/// Checksum of an 8.3 name stored in its long-name entries.
pub(crate) fn lfn_checksum(name: &[u8]) -> u8 {
    name.iter().fold(0u8, |s, &c| {
        ((s & 1) << 7).wrapping_add(s >> 1).wrapping_add(c)
    })
}

impl FileSystem for FatFs {
    fn info(&self) -> FsInfo {
        let mut fields = vec![
            InfoField::new("Bytes per sector", self.bytes_per_sector, Some(11)),
            InfoField::new("Cluster size", self.cluster_size, Some(13)),
            InfoField::new("Reserved bytes", self.reserved_bytes, Some(14)),
            InfoField::new("FAT copies", self.fat_count, Some(16)),
            InfoField::new(
                "FAT size",
                self.fat_bytes,
                Some(if self.kind == FsKind::Fat32 { 36 } else { 22 }),
            ),
            InfoField::new(
                "FAT #1 offset",
                self.reserved_bytes,
                Some(self.reserved_bytes),
            ),
            InfoField::new("Data area offset", self.data_offset, Some(self.data_offset)),
            InfoField::new("Clusters", self.cluster_count, None),
            InfoField::new(
                "Serial number",
                format!("{:04X}-{:04X}", self.serial >> 16, self.serial & 0xFFFF),
                None,
            ),
        ];
        if self.kind == FsKind::Fat32 {
            fields.push(InfoField::new("Root cluster", self.root_cluster, Some(44)));
        } else {
            fields.push(InfoField::new(
                "Root directory offset",
                self.root_dir_offset,
                Some(self.root_dir_offset),
            ));
        }
        let free = self.fat().ok().map(|_| {
            (2..self.cluster_count + 2)
                .filter(|&c| self.fat_entry(c).is_ok_and(|v| v == 0))
                .count() as u64
                * self.cluster_size
        });
        FsInfo {
            kind: self.kind,
            label: self.label.clone(),
            uuid: Some(format!(
                "{:04X}-{:04X}",
                self.serial >> 16,
                self.serial & 0xFFFF
            )),
            block_size: self.cluster_size as u32,
            total_size: u64::from(self.cluster_count) * self.cluster_size,
            free_size: free,
            fields,
        }
    }

    fn provider(&self) -> &Arc<dyn Provider> {
        &self.p
    }

    fn root(&self) -> FileEntry {
        FileEntry::root(u64::from(self.root_cluster))
    }

    fn list_dir(&self, dir: &FileEntry) -> Result<Vec<FileEntry>> {
        if !dir.is_dir {
            return Err(invalid("not a directory"));
        }
        self.parse_dir(dir, false)
    }

    fn extents(&self, file: &FileEntry) -> Result<Vec<Extent>> {
        let mut out = Vec::new();
        if self.is_fixed_root(file) && file.is_dir {
            push_extent(&mut out, 0, Some(self.root_dir_offset), self.root_dir_bytes);
            return Ok(out);
        }
        if file.id < 2 {
            return Ok(out);
        }
        let clusters: Vec<u32> = if file.deleted {
            // Deleted files lost their chain; assume contiguous clusters.
            let n = file.size.div_ceil(self.cluster_size).max(1);
            let max = u64::from(self.cluster_count) + 2;
            (file.id..(file.id + n).min(max))
                .map(|c| c as u32)
                .collect()
        } else {
            self.chain(file.id as u32)?
        };
        for (i, c) in clusters.iter().enumerate() {
            push_extent(
                &mut out,
                i as u64 * self.cluster_size,
                Some(self.cluster_offset(*c)),
                self.cluster_size,
            );
        }
        if !file.is_dir {
            truncate_extents(&mut out, file.size);
        }
        Ok(out)
    }

    fn metadata_regions(&self) -> Result<Vec<Region>> {
        let mut r = vec![Region::meta(
            0,
            self.reserved_bytes,
            "Reserved sectors (boot sector)",
        )];
        for i in 0..u64::from(self.fat_count) {
            r.push(Region::meta(
                self.reserved_bytes + i * self.fat_bytes,
                self.fat_bytes,
                format!("FAT #{}", i + 1),
            ));
        }
        if self.root_dir_bytes > 0 {
            r.push(Region::meta(
                self.root_dir_offset,
                self.root_dir_bytes,
                "Root directory",
            ));
        }
        Ok(r)
    }

    fn describe_offset(&self, offset: u64) -> Option<String> {
        self.cluster_at(offset).map(|c| format!("cluster {c}"))
    }

    fn deleted(&self, cancel: Option<&Cancel>) -> Result<Vec<FileEntry>> {
        let mut out = Vec::new();
        let mut queue = VecDeque::from([self.root()]);
        let mut seen = HashSet::new();
        while let Some(dir) = queue.pop_front() {
            check_cancel(cancel)?;
            if !seen.insert(dir.id) || seen.len() > 1_000_000 {
                continue;
            }
            if let Ok(found) = self.parse_dir(&dir, true) {
                out.extend(found);
            }
            if let Ok(children) = self.parse_dir(&dir, false) {
                queue.extend(children.into_iter().filter(|c| c.is_dir && c.id >= 2));
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::fs::testutil::{mem, put, put16, put32, read_all};
    use crate::fs::{FsMap, RegionKind, open_filesystem};

    fn set12(fat: &mut [u8], c: usize, v: u16) {
        let at = c + c / 2;
        let mut cur = u16::from_le_bytes([fat[at], fat[at + 1]]);
        if c & 1 == 1 {
            cur = (cur & 0x000F) | (v << 4);
        } else {
            cur = (cur & 0xF000) | (v & 0xFFF);
        }
        fat[at..at + 2].copy_from_slice(&cur.to_le_bytes());
    }

    fn dirent(buf: &mut [u8], at: usize, name: &[u8; 11], attr: u8, cluster: u16, size: u32) {
        put(buf, at, name);
        buf[at + 11] = attr;
        put16(buf, at + 24, (40 << 9) | (5 << 5) | 0x11);
        put16(buf, at + 22, (12 << 11) | (30 << 5));
        put16(buf, at + 26, cluster);
        put32(buf, at + 28, size);
    }

    fn lfn(buf: &mut [u8], at: usize, seq: u8, chars: &[u16], sum: u8) {
        buf[at] = seq;
        buf[at + 11] = 0x0F;
        buf[at + 13] = sum;
        let pos = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
        for (k, p) in pos.iter().enumerate() {
            let u = match k.cmp(&chars.len()) {
                std::cmp::Ordering::Less => chars[k],
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 0xFFFF,
            };
            put16(buf, at + p, u);
        }
    }

    /// A 32 KiB FAT12 image: HELLO.TXT (2 clusters), a fragmented long-named
    /// file, SUB/INNER.BIN and a deleted DELETED.TXT.
    pub(crate) fn fat12_image() -> Vec<u8> {
        let mut d = vec![0u8; 64 * 512];
        d[0..3].copy_from_slice(&[0xEB, 0x3C, 0x90]);
        put(&mut d, 3, b"MSWIN4.1");
        put16(&mut d, 11, 512);
        d[13] = 1;
        put16(&mut d, 14, 1);
        d[16] = 2;
        put16(&mut d, 17, 16);
        put16(&mut d, 19, 64);
        d[21] = 0xF8;
        put16(&mut d, 22, 1);
        d[38] = 0x29;
        put32(&mut d, 39, 0x1234_ABCD);
        put(&mut d, 43, b"TESTVOL    ");
        put(&mut d, 54, b"FAT12   ");
        d[510] = 0x55;
        d[511] = 0xAA;
        let mut fat = vec![0u8; 512];
        set12(&mut fat, 0, 0xFF8);
        set12(&mut fat, 1, 0xFFF);
        set12(&mut fat, 2, 3);
        set12(&mut fat, 3, 0xFFF);
        set12(&mut fat, 4, 6);
        set12(&mut fat, 6, 0xFFF);
        set12(&mut fat, 7, 0xFFF);
        set12(&mut fat, 8, 0xFFF);
        put(&mut d, 512, &fat);
        put(&mut d, 1024, &fat);
        let root = 1536;
        dirent(&mut d, root, b"TESTVOL    ", 0x08, 0, 0);
        dirent(&mut d, root + 32, b"HELLO   TXT", 0x20, 2, 600);
        let short = *b"LONGFI~1TXT";
        let sum = lfn_checksum(&short);
        let name: Vec<u16> = "Long File Name.txt".encode_utf16().collect();
        lfn(&mut d, root + 64, 0x42, &name[13..], sum);
        lfn(&mut d, root + 96, 0x01, &name[..13], sum);
        dirent(&mut d, root + 128, &short, 0x20, 4, 900);
        dirent(&mut d, root + 160, b"SUB        ", 0x10, 7, 0);
        dirent(&mut d, root + 192, b"\xE5ELETED TXT", 0x20, 9, 700);
        let data = 2048;
        let cl = |c: usize| data + (c - 2) * 512;
        for (i, b) in d[cl(2)..cl(2) + 600].iter_mut().enumerate() {
            *b = b'A' + (i % 26) as u8;
        }
        d[cl(4)..cl(4) + 512].fill(b'x');
        d[cl(6)..cl(6) + 388].fill(b'y');
        dirent(&mut d, cl(7), b".          ", 0x10, 7, 0);
        dirent(&mut d, cl(7) + 32, b"..         ", 0x10, 0, 0);
        dirent(&mut d, cl(7) + 64, b"INNER   BIN", 0x20, 8, 10);
        put(&mut d, cl(8), b"0123456789");
        d[cl(9)..cl(9) + 700].fill(b'D');
        d
    }

    #[test]
    fn fat12_tree_extents_and_deleted() {
        let fs = open_filesystem(&mem(fat12_image())).unwrap();
        let info = fs.info();
        assert_eq!(info.kind, FsKind::Fat12);
        assert_eq!(info.label.as_deref(), Some("TESTVOL"));
        let root = fs.list_dir(&fs.root()).unwrap();
        let names: Vec<_> = root.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["HELLO.TXT", "Long File Name.txt", "SUB"]);
        assert_eq!(root[0].metadata_offset, Some(1536 + 32));
        assert!(root[0].modified.is_some());

        let hello = &root[0];
        let content = read_all(fs.as_ref(), hello);
        assert_eq!(content.len(), 600);
        assert_eq!(&content[..3], b"ABC");
        let ex = fs.extents(hello).unwrap();
        assert_eq!(
            ex,
            vec![Extent {
                logical: 0,
                physical: Some(2048),
                len: 600
            }]
        );

        let long = fs.lookup("/long file name.TXT").unwrap().unwrap();
        let ex = fs.extents(&long).unwrap();
        assert_eq!(ex.len(), 2);
        assert_eq!(ex[1].physical, Some(2048 + 4 * 512));
        let c = read_all(fs.as_ref(), &long);
        assert!(c[..512].iter().all(|&b| b == b'x') && c[512..].iter().all(|&b| b == b'y'));

        let inner = fs.lookup("SUB/INNER.BIN").unwrap().unwrap();
        assert_eq!(inner.path, "/SUB/INNER.BIN");
        assert_eq!(read_all(fs.as_ref(), &inner), b"0123456789");

        let del = fs.deleted(None).unwrap();
        assert_eq!(del.len(), 1);
        assert_eq!(del[0].name, "_ELETED.TXT");
        assert!(del[0].deleted);
        assert!(read_all(fs.as_ref(), &del[0]).iter().all(|&b| b == b'D'));

        let map = FsMap::build(fs.as_ref(), None).unwrap();
        let owner = map.owner_at(2048 + 4 * 512 + 10).unwrap();
        assert_eq!(owner.label, "/Long File Name.txt");
        assert_eq!(map.owner_at(600).unwrap().label, "FAT #1");
        assert_eq!(
            map.owner_at(2048 + 5 * 512).unwrap().kind,
            RegionKind::Directory
        );
        assert_eq!(fs.describe_offset(2048 + 512), Some("cluster 3".into()));
    }
}
