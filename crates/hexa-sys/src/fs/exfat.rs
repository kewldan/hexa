//! exFAT (DISK-05/06/07): directory entry sets, contiguous (`NoFatChain`)
//! and FAT-chained data, deleted entry sets.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use hexa_core::provider::Provider;
use hexa_core::{Cancel, Result};

use super::{
    Extent, FileEntry, FileSystem, FsInfo, FsKind, InfoField, Region, dos_time, push_extent,
    truncate_extents,
};
use crate::util::{check_cancel, invalid, le16, le32, le64, read_vec, utf16le};

/// `aux` bit: data is contiguous, the FAT is not used.
const NO_FAT_CHAIN: u64 = 1;

/// An exFAT volume.
#[derive(Debug)]
pub struct ExFatFs {
    p: Arc<dyn Provider>,
    sector_size: u64,
    cluster_size: u64,
    fat_offset: u64,
    fat_length: u64,
    fat_count: u32,
    heap_offset: u64,
    cluster_count: u32,
    root_cluster: u32,
    serial: u32,
    volume_length: u64,
    revision: u16,
    label: Option<String>,
}

impl ExFatFs {
    pub fn open(p: Arc<dyn Provider>) -> Result<Self> {
        let b = read_vec(p.as_ref(), 0, 512)?;
        if &b[3..11] != b"EXFAT   " {
            return Err(invalid("not an exFAT boot sector"));
        }
        let bps_shift = b[108];
        let spc_shift = b[109];
        if !(9..=12).contains(&bps_shift) || u32::from(bps_shift) + u32::from(spc_shift) > 25 {
            return Err(invalid("invalid exFAT geometry"));
        }
        let ss = 1u64 << bps_shift;
        let mut fs = Self {
            sector_size: ss,
            cluster_size: ss << spc_shift,
            fat_offset: u64::from(le32(&b, 80)) * ss,
            fat_length: u64::from(le32(&b, 84)) * ss,
            fat_count: u32::from(b[110]),
            heap_offset: u64::from(le32(&b, 88)) * ss,
            cluster_count: le32(&b, 92),
            root_cluster: le32(&b, 96),
            serial: le32(&b, 100),
            volume_length: le64(&b, 72) * ss,
            revision: le16(&b, 104),
            label: None,
            p,
        };
        // The volume label lives in the root directory.
        let root = fs.root();
        if let Ok(data) = fs.dir_bytes(&root) {
            for e in data.as_chunks::<32>().0 {
                if e[0] == 0 {
                    break;
                }
                if e[0] == 0x83 {
                    let n = usize::from(e[1]).min(11);
                    fs.label = Some(utf16le(&e[2..2 + n * 2])).filter(|s| !s.is_empty());
                }
            }
        }
        Ok(fs)
    }

    pub fn cluster_offset(&self, cluster: u32) -> u64 {
        self.heap_offset + u64::from(cluster.saturating_sub(2)) * self.cluster_size
    }

    fn fat_entry(&self, cluster: u32) -> Result<u32> {
        let mut b = [0u8; 4];
        self.p
            .read_at(self.fat_offset + u64::from(cluster) * 4, &mut b)?;
        Ok(u32::from_le_bytes(b))
    }

    /// Clusters of an entry: contiguous or following the FAT.
    fn clusters(&self, first: u32, len: u64, contiguous: bool) -> Result<Vec<u32>> {
        let max = self.cluster_count + 2;
        if first < 2 || first >= max {
            return Ok(Vec::new());
        }
        if contiguous {
            let n = len.div_ceil(self.cluster_size).max(1);
            return Ok(
                (u64::from(first)..(u64::from(first) + n).min(u64::from(max)))
                    .map(|c| c as u32)
                    .collect(),
            );
        }
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut c = first;
        while c >= 2 && c < max && seen.insert(c) {
            out.push(c);
            c = self.fat_entry(c)?;
        }
        Ok(out)
    }

    fn dir_clusters(&self, dir: &FileEntry) -> Result<Vec<u32>> {
        if dir.id == u64::from(self.root_cluster) && dir.path == "/" {
            return self.clusters(self.root_cluster, 0, false);
        }
        self.clusters(
            dir.id as u32,
            dir.size,
            dir.aux & NO_FAT_CHAIN != 0 || dir.deleted,
        )
    }

    fn dir_bytes(&self, dir: &FileEntry) -> Result<Vec<u8>> {
        Ok(self.dir_data(dir)?.0)
    }

    fn dir_data(&self, dir: &FileEntry) -> Result<(Vec<u8>, Vec<u64>)> {
        let mut data = Vec::new();
        let mut locs = Vec::new();
        for c in self.dir_clusters(dir)?.into_iter().take(1 << 16) {
            let off = self.cluster_offset(c);
            if off + self.cluster_size > self.p.len() {
                break;
            }
            locs.push(off);
            data.extend(read_vec(self.p.as_ref(), off, self.cluster_size as usize)?);
        }
        Ok((data, locs))
    }

    fn parse_dir(&self, dir: &FileEntry, want_deleted: bool) -> Result<Vec<FileEntry>> {
        let (data, locs) = self.dir_data(dir)?;
        let entries: Vec<&[u8]> = data.as_chunks::<32>().0.iter().map(|e| &e[..]).collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i < entries.len() {
            let e = entries[i];
            let t = e[0];
            if t == 0 {
                break;
            }
            let is_file = if want_deleted { t == 0x05 } else { t == 0x85 };
            if !is_file {
                i += 1;
                continue;
            }
            let secondary = usize::from(e[1]);
            let set = &entries[i + 1..(i + 1 + secondary).min(entries.len())];
            let stream_type = if want_deleted { 0x40 } else { 0xC0 };
            let name_type = if want_deleted { 0x41 } else { 0xC1 };
            let Some(stream) = set.first().filter(|s| s[0] == stream_type) else {
                i += 1;
                continue;
            };
            let name_len = usize::from(stream[3]);
            let mut name: String = set
                .iter()
                .skip(1)
                .take_while(|n| n[0] == name_type)
                .map(|n| utf16le(&n[2..32]))
                .collect();
            name = name.chars().take(name_len).collect();
            let attrs = le16(e, 4);
            let ts_mod = le32(e, 12);
            let ts_create = le32(e, 8);
            let byte_pos = (i * 32) as u64;
            let loc = locs
                .get((byte_pos / self.cluster_size) as usize)
                .map(|l| l + byte_pos % self.cluster_size);
            out.push(FileEntry {
                id: u64::from(le32(stream, 20)),
                aux: if stream[1] & 0x02 != 0 {
                    NO_FAT_CHAIN
                } else {
                    0
                },
                path: FileEntry::child_path(&dir.path, &name),
                name,
                is_dir: attrs & 0x10 != 0,
                size: le64(stream, 24),
                deleted: want_deleted,
                metadata_offset: loc,
                modified: dos_time((ts_mod >> 16) as u16, ts_mod as u16),
                created: dos_time((ts_create >> 16) as u16, ts_create as u16),
                attributes: u32::from(attrs),
            });
            i += 1 + secondary;
        }
        Ok(out)
    }
}

impl FileSystem for ExFatFs {
    fn info(&self) -> FsInfo {
        FsInfo {
            kind: FsKind::ExFat,
            label: self.label.clone(),
            uuid: Some(format!(
                "{:04X}-{:04X}",
                self.serial >> 16,
                self.serial & 0xFFFF
            )),
            block_size: self.cluster_size as u32,
            total_size: self.volume_length,
            free_size: None,
            fields: vec![
                InfoField::new("Bytes per sector", self.sector_size, Some(108)),
                InfoField::new("Cluster size", self.cluster_size, Some(109)),
                InfoField::new("FAT offset", self.fat_offset, Some(self.fat_offset)),
                InfoField::new("FAT length", self.fat_length, Some(84)),
                InfoField::new(
                    "Cluster heap offset",
                    self.heap_offset,
                    Some(self.heap_offset),
                ),
                InfoField::new("Cluster count", self.cluster_count, Some(92)),
                InfoField::new(
                    "Root directory cluster",
                    self.root_cluster,
                    Some(self.cluster_offset(self.root_cluster)),
                ),
                InfoField::new(
                    "Revision",
                    format!("{}.{:02}", self.revision >> 8, self.revision & 0xFF),
                    Some(104),
                ),
            ],
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
        let clusters = if file.is_dir && file.path == "/" {
            self.dir_clusters(file)?
        } else {
            self.clusters(
                file.id as u32,
                file.size,
                file.aux & NO_FAT_CHAIN != 0 || file.deleted,
            )?
        };
        let mut out = Vec::new();
        for (i, c) in clusters.iter().enumerate() {
            push_extent(
                &mut out,
                i as u64 * self.cluster_size,
                Some(self.cluster_offset(*c)),
                self.cluster_size,
            );
        }
        if !(file.is_dir && file.path == "/") {
            truncate_extents(&mut out, file.size);
        }
        Ok(out)
    }

    fn metadata_regions(&self) -> Result<Vec<Region>> {
        let mut r = vec![
            Region::meta(0, 12 * self.sector_size, "Main boot region"),
            Region::meta(
                12 * self.sector_size,
                12 * self.sector_size,
                "Backup boot region",
            ),
        ];
        for i in 0..u64::from(self.fat_count.max(1)) {
            r.push(Region::meta(
                self.fat_offset + i * self.fat_length,
                self.fat_length,
                format!("FAT #{}", i + 1),
            ));
        }
        let data = self.dir_bytes(&self.root())?;
        for e in data.as_chunks::<32>().0 {
            let label = match e[0] {
                0 => break,
                0x81 => "Allocation bitmap",
                0x82 => "Up-case table",
                _ => continue,
            };
            let first = le32(e, 20);
            let len = le64(e, 24);
            for c in self.clusters(first, len, false)? {
                r.push(Region::meta(
                    self.cluster_offset(c),
                    self.cluster_size,
                    label,
                ));
            }
        }
        Ok(r)
    }

    fn describe_offset(&self, offset: u64) -> Option<String> {
        (offset >= self.heap_offset)
            .then(|| (offset - self.heap_offset) / self.cluster_size + 2)
            .filter(|&c| c < u64::from(self.cluster_count) + 2)
            .map(|c| format!("cluster {c}"))
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
                queue.extend(children.into_iter().filter(|c| c.is_dir));
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::open_filesystem;
    use crate::fs::testutil::{mem, put, put16, put32, put64, read_all};

    fn entry_set(
        buf: &mut [u8],
        at: usize,
        deleted: bool,
        name: &str,
        attrs: u16,
        cluster: u32,
        size: u64,
        contiguous: bool,
    ) -> usize {
        let units: Vec<u16> = name.encode_utf16().collect();
        let names = units.len().div_ceil(15);
        let base = if deleted { 0x00 } else { 0x80 };
        buf[at] = base | 0x05;
        buf[at + 1] = (1 + names) as u8;
        put16(buf, at + 4, attrs);
        put32(buf, at + 12, ((44u32 << 9 | 3 << 5 | 9) << 16) | (8 << 11));
        let s = at + 32;
        buf[s] = base | 0x40;
        buf[s + 1] = 0x01 | if contiguous { 0x02 } else { 0 };
        buf[s + 3] = units.len() as u8;
        put64(buf, s + 8, size);
        put32(buf, s + 20, cluster);
        put64(buf, s + 24, size);
        for (k, chunk) in units.chunks(15).enumerate() {
            let n = s + 32 + k * 32;
            buf[n] = base | 0x41;
            for (j, u) in chunk.iter().enumerate() {
                put16(buf, n + 2 + j * 2, *u);
            }
        }
        at + 32 * (2 + names)
    }

    fn exfat_image() -> Vec<u8> {
        let mut d = vec![0u8; 64 * 512];
        put(&mut d, 3, b"EXFAT   ");
        put64(&mut d, 72, 64);
        put32(&mut d, 80, 24);
        put32(&mut d, 84, 1);
        put32(&mut d, 88, 32);
        put32(&mut d, 92, 32);
        put32(&mut d, 96, 2);
        put32(&mut d, 100, 0xCAFE_F00D);
        put16(&mut d, 104, 0x0100);
        d[108] = 9;
        d[109] = 0;
        d[110] = 1;
        d[510] = 0x55;
        d[511] = 0xAA;
        let fat = 24 * 512;
        let mut setf = |c: usize, v: u32| put32(&mut d, fat + c * 4, v);
        setf(2, 0xFFFF_FFFF);
        setf(3, 0xFFFF_FFFF);
        setf(4, 0xFFFF_FFFF);
        setf(7, 9);
        setf(9, 0xFFFF_FFFF);
        let cl = |c: usize| 32 * 512 + (c - 2) * 512;
        let mut at = cl(2);
        d[at] = 0x83;
        d[at + 1] = 4;
        for (j, u) in "DISK".encode_utf16().enumerate() {
            put16(&mut d, at + 2 + j * 2, u);
        }
        at += 32;
        d[at] = 0x81;
        put32(&mut d, at + 20, 3);
        put64(&mut d, at + 24, 4);
        at += 32;
        d[at] = 0x82;
        put32(&mut d, at + 20, 4);
        put64(&mut d, at + 24, 128);
        at += 32;
        at = entry_set(&mut d, at, false, "hello.txt", 0x20, 5, 700, true);
        at = entry_set(
            &mut d,
            at,
            false,
            "a fragmented file name.bin",
            0x20,
            7,
            1000,
            false,
        );
        at = entry_set(&mut d, at, false, "dir", 0x10, 10, 512, true);
        entry_set(&mut d, at, true, "gone.txt", 0x20, 12, 100, true);
        d[cl(5)..cl(5) + 700].fill(b'h');
        d[cl(7)..cl(7) + 512].fill(1);
        d[cl(9)..cl(9) + 488].fill(2);
        entry_set(&mut d, cl(10), false, "a.txt", 0x20, 11, 5, true);
        put(&mut d, cl(11), b"hello");
        d[cl(12)..cl(12) + 100].fill(b'g');
        d
    }

    #[test]
    fn exfat_listing_chains_and_deleted() {
        let fs = open_filesystem(&mem(exfat_image())).unwrap();
        let info = fs.info();
        assert_eq!(info.kind, FsKind::ExFat);
        assert_eq!(info.label.as_deref(), Some("DISK"));
        let root = fs.list_dir(&fs.root()).unwrap();
        let names: Vec<_> = root.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["hello.txt", "a fragmented file name.bin", "dir"]);
        assert_eq!(read_all(fs.as_ref(), &root[0]).len(), 700);
        let frag = fs.extents(&root[1]).unwrap();
        assert_eq!(frag.len(), 2);
        assert_eq!(frag[1].physical, Some(32 * 512 + 7 * 512));
        assert_eq!(frag[1].len, 488);
        let c = read_all(fs.as_ref(), &root[1]);
        assert!(c[..512].iter().all(|&b| b == 1) && c[512..].iter().all(|&b| b == 2));
        let a = fs.lookup("/dir/a.txt").unwrap().unwrap();
        assert_eq!(read_all(fs.as_ref(), &a), b"hello");
        assert!(root[0].modified.is_some());
        let del = fs.deleted(None).unwrap();
        assert_eq!(del.len(), 1);
        assert_eq!(del[0].name, "gone.txt");
        assert!(read_all(fs.as_ref(), &del[0]).iter().all(|&b| b == b'g'));
        let meta = fs.metadata_regions().unwrap();
        assert!(
            meta.iter()
                .any(|r| r.label == "Allocation bitmap" && r.start == 32 * 512 + 512)
        );
    }
}
