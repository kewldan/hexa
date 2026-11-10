//! ISO 9660 (DISK-05/06) with Joliet (UCS-2) and Rock Ridge `NM` names.

use std::sync::Arc;

use hexa_core::Result;
use hexa_core::provider::Provider;

use super::{
    Extent, FileEntry, FileSystem, FsInfo, FsKind, InfoField, Region, push_extent, unix_time,
};
use crate::util::{ascii, invalid, le16, le32, read_vec, utf16be};

const SECTOR: u64 = 2048;
const VD_START: u64 = 16 * SECTOR;

/// An ISO 9660 volume.
#[derive(Debug)]
pub struct IsoFs {
    p: Arc<dyn Provider>,
    block: u64,
    volume_blocks: u64,
    label: Option<String>,
    system_id: String,
    publisher: String,
    application: String,
    root_lba: u64,
    root_len: u64,
    joliet: bool,
    /// Offset of every volume descriptor.
    descriptors: Vec<u64>,
    path_tables: Vec<(u64, u64, &'static str)>,
}

impl IsoFs {
    pub fn detect(p: &dyn Provider) -> Result<bool> {
        if p.len() < VD_START + SECTOR {
            return Ok(false);
        }
        let mut b = [0u8; 6];
        p.read_at(VD_START, &mut b)?;
        Ok(&b[1..6] == b"CD001")
    }

    pub fn open(p: Arc<dyn Provider>) -> Result<Self> {
        let mut pvd = None;
        let mut svd = None;
        let mut descriptors = Vec::new();
        for i in 0..64u64 {
            let off = VD_START + i * SECTOR;
            if off + SECTOR > p.len() {
                break;
            }
            let vd = read_vec(p.as_ref(), off, SECTOR as usize)?;
            if &vd[1..6] != b"CD001" {
                break;
            }
            descriptors.push(off);
            match vd[0] {
                1 if pvd.is_none() => pvd = Some(vd),
                2 if matches!(&vd[88..91], b"%/@" | b"%/C" | b"%/E") => svd = Some(vd),
                255 => break,
                _ => {}
            }
        }
        let pvd = pvd.ok_or_else(|| invalid("no ISO 9660 primary volume descriptor"))?;
        let block = u64::from(le16(&pvd, 128)).max(512);
        let joliet = svd.is_some();
        let active = svd.as_ref().unwrap_or(&pvd);
        let root = &active[156..190];
        let label = if joliet {
            utf16be(&active[40..72]).trim_end().to_string()
        } else {
            ascii(&pvd[40..72])
        };
        let pt_size = u64::from(le32(&pvd, 132));
        let mut path_tables = Vec::new();
        for (off, name) in [
            (140usize, "Path table (L)"),
            (144, "Optional path table (L)"),
        ] {
            let lba = u64::from(le32(&pvd, off));
            if lba != 0 {
                path_tables.push((lba * block, pt_size, name));
            }
        }
        for (off, name) in [
            (148usize, "Path table (M)"),
            (152, "Optional path table (M)"),
        ] {
            let lba = u64::from(crate::util::be32(&pvd, off));
            if lba != 0 {
                path_tables.push((lba * block, pt_size, name));
            }
        }
        Ok(Self {
            block,
            volume_blocks: u64::from(le32(&pvd, 80)),
            label: Some(label).filter(|s| !s.is_empty()),
            system_id: ascii(&pvd[8..40]),
            publisher: ascii(&pvd[318..446]),
            application: ascii(&pvd[574..702]),
            root_lba: u64::from(le32(root, 2)),
            root_len: u64::from(le32(root, 10)),
            joliet,
            descriptors,
            path_tables,
            p,
        })
    }

    pub fn is_joliet(&self) -> bool {
        self.joliet
    }

    fn record_name(&self, rec: &[u8]) -> String {
        let name_len = usize::from(rec[32]);
        let raw = &rec[33..(33 + name_len).min(rec.len())];
        if !self.joliet {
            // Rock Ridge alternate name in the system use area.
            let mut su = 33 + name_len + usize::from(name_len % 2 == 0);
            let mut rr = String::new();
            while su + 4 <= rec.len() {
                let len = usize::from(rec[su + 2]);
                if len < 4 || su + len > rec.len() {
                    break;
                }
                if &rec[su..su + 2] == b"NM" && len > 5 {
                    rr.push_str(&String::from_utf8_lossy(&rec[su + 5..su + len]));
                }
                su += len;
            }
            if !rr.is_empty() {
                return rr;
            }
        }
        let mut name = if self.joliet {
            utf16be(raw)
        } else {
            raw.iter().map(|&c| c as char).collect()
        };
        if let Some(i) = name.rfind(';') {
            name.truncate(i);
        }
        if name.ends_with('.') {
            name.pop();
        }
        name
    }
}

fn iso_time(t: &[u8]) -> Option<i64> {
    if t.len() < 7 || t[0] == 0 {
        return None;
    }
    let local = unix_time(
        1900 + i64::from(t[0]),
        i64::from(t[1]),
        i64::from(t[2]),
        i64::from(t[3]),
        i64::from(t[4]),
        i64::from(t[5]),
    )?;
    Some(local - i64::from(t[6] as i8) * 15 * 60)
}

impl FileSystem for IsoFs {
    fn info(&self) -> FsInfo {
        let pvd = VD_START;
        FsInfo {
            kind: FsKind::Iso9660,
            label: self.label.clone(),
            uuid: None,
            block_size: self.block as u32,
            total_size: self.volume_blocks * self.block,
            free_size: Some(0),
            fields: vec![
                InfoField::new("Primary volume descriptor", "sector 16", Some(pvd)),
                InfoField::new("System identifier", &self.system_id, Some(pvd + 8)),
                InfoField::new("Volume space (blocks)", self.volume_blocks, Some(pvd + 80)),
                InfoField::new("Logical block size", self.block, Some(pvd + 128)),
                InfoField::new("Publisher", &self.publisher, Some(pvd + 318)),
                InfoField::new("Application", &self.application, Some(pvd + 574)),
                InfoField::new("Joliet", self.joliet, None),
                InfoField::new(
                    "Root directory",
                    format!("LBA {}", self.root_lba),
                    Some(self.root_lba * self.block),
                ),
            ],
        }
    }

    fn provider(&self) -> &Arc<dyn Provider> {
        &self.p
    }

    fn root(&self) -> FileEntry {
        let mut r = FileEntry::root(self.root_lba);
        r.size = self.root_len;
        r
    }

    fn list_dir(&self, dir: &FileEntry) -> Result<Vec<FileEntry>> {
        if !dir.is_dir {
            return Err(invalid("not a directory"));
        }
        let start = dir.id * self.block;
        let len = dir
            .size
            .min(16 << 20)
            .min(self.p.len().saturating_sub(start));
        let data = read_vec(self.p.as_ref(), start, len as usize)?;
        let mut out: Vec<FileEntry> = Vec::new();
        let mut pos = 0usize;
        while pos < data.len() {
            let rl = usize::from(data[pos]);
            if rl == 0 {
                // Records never cross sectors; skip to the next one.
                pos = (pos / SECTOR as usize + 1) * SECTOR as usize;
                continue;
            }
            if rl < 34 || pos + rl > data.len() {
                break;
            }
            let rec = &data[pos..pos + rl];
            let name_len = rec[32];
            let flags = rec[25];
            let is_special = name_len == 1 && (rec[33] == 0 || rec[33] == 1);
            if !is_special {
                let name = self.record_name(rec);
                let lba = u64::from(le32(rec, 2));
                let size = u64::from(le32(rec, 10));
                // Multi-extent files: later records extend the previous one.
                let continues = out
                    .last()
                    .is_some_and(|l: &FileEntry| l.name == name && l.attributes & 0x80 != 0);
                if continues {
                    let last = out.last_mut().expect("checked");
                    last.size += size;
                    last.attributes = u32::from(flags);
                } else {
                    out.push(FileEntry {
                        id: lba,
                        aux: 0,
                        path: FileEntry::child_path(&dir.path, &name),
                        name,
                        is_dir: flags & 0x02 != 0,
                        size,
                        deleted: false,
                        metadata_offset: Some(start + pos as u64),
                        modified: iso_time(&rec[18..25]),
                        created: None,
                        attributes: u32::from(flags),
                    });
                }
            }
            pos += rl;
        }
        Ok(out)
    }

    fn extents(&self, file: &FileEntry) -> Result<Vec<Extent>> {
        let mut out = Vec::new();
        push_extent(&mut out, 0, Some(file.id * self.block), file.size);
        Ok(out)
    }

    fn metadata_regions(&self) -> Result<Vec<Region>> {
        let mut r = vec![Region::meta(0, VD_START, "System area")];
        for (i, &off) in self.descriptors.iter().enumerate() {
            r.push(Region::meta(off, SECTOR, format!("Volume descriptor {i}")));
        }
        for &(off, len, name) in &self.path_tables {
            r.push(Region::meta(off, len, name));
        }
        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::open_filesystem;
    use crate::fs::testutil::{mem, put, put16, put32, read_all};

    fn rec(lba: u32, size: u32, flags: u8, name: &[u8], su: &[u8]) -> Vec<u8> {
        let pad = usize::from(name.len().is_multiple_of(2));
        let mut len = 33 + name.len() + pad + su.len();
        len += len % 2;
        let mut r = vec![0u8; len];
        r[0] = len as u8;
        put32(&mut r, 2, lba);
        r[6..10].copy_from_slice(&lba.to_be_bytes());
        put32(&mut r, 10, size);
        r[14..18].copy_from_slice(&size.to_be_bytes());
        r[18..25].copy_from_slice(&[120, 1, 2, 3, 4, 5, 0]);
        r[25] = flags;
        put16(&mut r, 28, 1);
        r[32] = name.len() as u8;
        r[33..33 + name.len()].copy_from_slice(name);
        r[33 + name.len() + pad..33 + name.len() + pad + su.len()].copy_from_slice(su);
        r
    }

    fn ucs2(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_be_bytes).collect()
    }

    fn write_dir(d: &mut [u8], lba: usize, self_lba: u32, parent: u32, recs: &[Vec<u8>]) {
        let mut at = lba * 2048;
        for r in [
            rec(self_lba, 2048, 2, &[0], &[]),
            rec(parent, 2048, 2, &[1], &[]),
        ]
        .iter()
        .chain(recs)
        {
            put(d, at, r);
            at += r.len();
        }
    }

    fn iso_image(joliet: bool) -> Vec<u8> {
        let mut d = vec![0u8; 27 * 2048];
        let vd = |d: &mut Vec<u8>, sector: usize, kind: u8| {
            let at = sector * 2048;
            d[at] = kind;
            put(d, at + 1, b"CD001");
            d[at + 6] = 1;
            at
        };
        let p = vd(&mut d, 16, 1);
        put(&mut d, p + 8, b"LINUX");
        put(&mut d, p + 40, b"MYDISC");
        put32(&mut d, p + 80, 27);
        put16(&mut d, p + 128, 2048);
        let root = rec(19, 2048, 2, &[0], &[]);
        put(&mut d, p + 156, &root);
        let mut next = 17;
        if joliet {
            let s = vd(&mut d, 17, 2);
            put(&mut d, s + 40, &ucs2("Joliet Disc"));
            put(&mut d, s + 88, b"%/E");
            put16(&mut d, s + 128, 2048);
            put(&mut d, s + 156, &rec(21, 2048, 2, &[0], &[]));
            next = 18;
        }
        vd(&mut d, next, 255);
        let mut nm = b"NM\x0a\x01\x00".to_vec();
        nm.extend_from_slice(b"rr name");
        nm[2] = nm.len() as u8;
        write_dir(
            &mut d,
            19,
            19,
            19,
            &[
                rec(20, 2048, 2, b"SUBDIR", &[]),
                rec(23, 5000, 0, b"LONGNAME.TXT;1", &nm),
            ],
        );
        write_dir(&mut d, 20, 20, 19, &[rec(26, 3, 0, b"A.BIN;1", &[])]);
        write_dir(
            &mut d,
            21,
            21,
            21,
            &[
                rec(22, 2048, 2, &ucs2("Sub Directory"), &[]),
                rec(23, 5000, 0, &ucs2("Long Name File.txt;1"), &[]),
            ],
        );
        write_dir(&mut d, 22, 22, 21, &[rec(26, 3, 0, &ucs2("a.bin;1"), &[])]);
        d[23 * 2048..23 * 2048 + 5000].fill(b'L');
        put(&mut d, 26 * 2048, b"abc");
        d
    }

    #[test]
    fn iso_with_joliet() {
        let fs = open_filesystem(&mem(iso_image(true))).unwrap();
        let info = fs.info();
        assert_eq!(info.kind, FsKind::Iso9660);
        assert_eq!(info.label.as_deref(), Some("Joliet Disc"));
        let root = fs.list_dir(&fs.root()).unwrap();
        let names: Vec<_> = root.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Sub Directory", "Long Name File.txt"]);
        assert_eq!(root[1].modified, unix_time(2020, 1, 2, 3, 4, 5));
        assert_eq!(read_all(fs.as_ref(), &root[1]), vec![b'L'; 5000]);
        let a = fs.lookup("/Sub Directory/a.bin").unwrap().unwrap();
        assert_eq!(read_all(fs.as_ref(), &a), b"abc");
        assert_eq!(fs.extents(&a).unwrap()[0].physical, Some(26 * 2048));
    }

    #[test]
    fn iso_plain_and_rock_ridge() {
        let fs = open_filesystem(&mem(iso_image(false))).unwrap();
        assert_eq!(fs.info().label.as_deref(), Some("MYDISC"));
        let root = fs.list_dir(&fs.root()).unwrap();
        let names: Vec<_> = root.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["SUBDIR", "rr name"]);
        let a = fs.lookup("/SUBDIR/A.BIN").unwrap().unwrap();
        assert_eq!(a.size, 3);
        assert!(
            fs.metadata_regions()
                .unwrap()
                .iter()
                .any(|r| r.label == "Volume descriptor 0")
        );
    }
}
