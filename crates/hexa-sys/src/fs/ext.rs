//! ext2/3/4 (DISK-05/06): superblock, group descriptors, inodes (extent
//! trees, indirect blocks, inline data), directory listing and inode-table
//! navigation.

use std::sync::Arc;

use hexa_core::Result;
use hexa_core::provider::Provider;

use super::{
    Extent, FileEntry, FileSystem, FsInfo, FsKind, InfoField, Region, push_extent, truncate_extents,
};
use crate::util::{ascii, invalid, le16, le32, read_vec, uuid_string};

const EXT_MAGIC: u16 = 0xEF53;
const INCOMPAT_FILETYPE: u32 = 0x2;
const INCOMPAT_EXTENTS: u32 = 0x40;
const INCOMPAT_64BIT: u32 = 0x80;
const INCOMPAT_FLEX_BG: u32 = 0x200;
const COMPAT_HAS_JOURNAL: u32 = 0x4;
const EXTENTS_FL: u32 = 0x8_0000;
const INLINE_DATA_FL: u32 = 0x1000_0000;
const S_IFMT: u16 = 0xF000;
const S_IFDIR: u16 = 0x4000;
const S_IFLNK: u16 = 0xA000;

#[derive(Debug, Clone, Copy)]
struct Group {
    block_bitmap: u64,
    inode_bitmap: u64,
    inode_table: u64,
}

/// An ext2/3/4 volume.
#[derive(Debug)]
pub struct ExtFs {
    p: Arc<dyn Provider>,
    kind: FsKind,
    block_size: u64,
    blocks_count: u64,
    free_blocks: u64,
    inodes_count: u32,
    first_data_block: u64,
    blocks_per_group: u32,
    inodes_per_group: u32,
    inode_size: u64,
    desc_size: u64,
    feature_compat: u32,
    feature_incompat: u32,
    feature_ro_compat: u32,
    label: Option<String>,
    uuid: String,
    last_mounted: String,
    groups: Vec<Group>,
}

/// Decoded fields of an inode.
#[derive(Debug, Clone)]
struct Inode {
    mode: u16,
    size: u64,
    flags: u32,
    blocks_lo: u32,
    mtime: i64,
    crtime: Option<i64>,
    block: Vec<u8>,
}

impl ExtFs {
    pub fn detect(p: &dyn Provider) -> Result<bool> {
        if p.len() < 2048 {
            return Ok(false);
        }
        let mut m = [0u8; 2];
        p.read_at(1024 + 56, &mut m)?;
        Ok(u16::from_le_bytes(m) == EXT_MAGIC)
    }

    pub fn open(p: Arc<dyn Provider>) -> Result<Self> {
        let sb = read_vec(p.as_ref(), 1024, 1024)?;
        if le16(&sb, 56) != EXT_MAGIC {
            return Err(invalid("no ext superblock"));
        }
        let log = le32(&sb, 24);
        if log > 6 {
            return Err(invalid("invalid ext block size"));
        }
        let block_size = 1024u64 << log;
        let incompat = le32(&sb, 96);
        let compat = le32(&sb, 92);
        let is64 = incompat & INCOMPAT_64BIT != 0;
        let blocks_count = u64::from(le32(&sb, 4))
            | if is64 {
                u64::from(le32(&sb, 0x150)) << 32
            } else {
                0
            };
        let free_blocks = u64::from(le32(&sb, 12))
            | if is64 {
                u64::from(le32(&sb, 0x158)) << 32
            } else {
                0
            };
        let rev = le32(&sb, 76);
        let inode_size = if rev == 0 {
            128
        } else {
            u64::from(le16(&sb, 88))
        };
        let desc_size = if is64 {
            u64::from(le16(&sb, 254)).max(32)
        } else {
            32
        };
        let blocks_per_group = le32(&sb, 32);
        let inodes_per_group = le32(&sb, 40);
        if blocks_per_group == 0
            || inodes_per_group == 0
            || inode_size < 128
            || !inode_size.is_power_of_two()
        {
            return Err(invalid("invalid ext geometry"));
        }
        let first_data_block = u64::from(le32(&sb, 20));
        let group_count =
            (blocks_count.saturating_sub(first_data_block)).div_ceil(u64::from(blocks_per_group));
        if group_count > 1 << 22 {
            return Err(invalid("implausible ext group count"));
        }
        let gdt = read_vec(
            p.as_ref(),
            (first_data_block + 1) * block_size,
            (group_count * desc_size) as usize,
        )?;
        let groups = gdt
            .chunks_exact(desc_size as usize)
            .map(|d| {
                let hi = |off: usize| {
                    if is64 && desc_size >= 64 {
                        u64::from(le32(d, off)) << 32
                    } else {
                        0
                    }
                };
                Group {
                    block_bitmap: u64::from(le32(d, 0)) | hi(0x20),
                    inode_bitmap: u64::from(le32(d, 4)) | hi(0x24),
                    inode_table: u64::from(le32(d, 8)) | hi(0x28),
                }
            })
            .collect();
        let kind = if incompat & (INCOMPAT_EXTENTS | INCOMPAT_64BIT | INCOMPAT_FLEX_BG) != 0 {
            FsKind::Ext4
        } else if compat & COMPAT_HAS_JOURNAL != 0 {
            FsKind::Ext3
        } else {
            FsKind::Ext2
        };
        Ok(Self {
            kind,
            block_size,
            blocks_count,
            free_blocks,
            inodes_count: le32(&sb, 0),
            first_data_block,
            blocks_per_group,
            inodes_per_group,
            inode_size,
            desc_size,
            feature_compat: compat,
            feature_incompat: incompat,
            feature_ro_compat: le32(&sb, 100),
            label: Some(ascii(&sb[120..136])).filter(|s| !s.is_empty()),
            uuid: uuid_string(&sb[104..120]),
            last_mounted: ascii(&sb[136..200]),
            groups,
            p,
        })
    }

    pub fn block_size(&self) -> u64 {
        self.block_size
    }

    /// Byte offset of inode `ino` (navigation target).
    pub fn inode_offset(&self, ino: u32) -> Option<u64> {
        if ino == 0 || ino > self.inodes_count {
            return None;
        }
        let idx = ino - 1;
        let g = self.groups.get((idx / self.inodes_per_group) as usize)?;
        Some(
            g.inode_table * self.block_size
                + u64::from(idx % self.inodes_per_group) * self.inode_size,
        )
    }

    /// The inode whose on-disk record contains `offset`.
    pub fn inode_at(&self, offset: u64) -> Option<u32> {
        let table_len = u64::from(self.inodes_per_group) * self.inode_size;
        self.groups.iter().enumerate().find_map(|(gi, g)| {
            let start = g.inode_table * self.block_size;
            (offset >= start && offset < start + table_len).then(|| {
                gi as u32 * self.inodes_per_group + ((offset - start) / self.inode_size) as u32 + 1
            })
        })
    }

    fn inode(&self, ino: u32) -> Result<Inode> {
        let off = self
            .inode_offset(ino)
            .ok_or_else(|| invalid(format!("bad inode {ino}")))?;
        let raw = read_vec(self.p.as_ref(), off, self.inode_size.min(256) as usize)?;
        let extra = if self.inode_size > 128 {
            u64::from(le16(&raw, 128))
        } else {
            0
        };
        Ok(Inode {
            mode: le16(&raw, 0),
            size: u64::from(le32(&raw, 4)) | (u64::from(le32(&raw, 108)) << 32),
            flags: le32(&raw, 32),
            blocks_lo: le32(&raw, 28),
            mtime: i64::from(le32(&raw, 16)),
            crtime: (extra >= 0x18)
                .then(|| i64::from(le32(&raw, 0x90)))
                .filter(|&t| t != 0),
            block: raw[40..100].to_vec(),
        })
    }

    fn inline_location(&self, ino: u32, inode: &Inode) -> Option<u64> {
        let fast_symlink = inode.mode & S_IFMT == S_IFLNK
            && inode.size < 60
            && inode.flags & EXTENTS_FL == 0
            && inode.blocks_lo == 0;
        (inode.flags & INLINE_DATA_FL != 0 || fast_symlink)
            .then(|| self.inode_offset(ino).unwrap_or(0) + 40)
    }

    /// `(logical block, physical block, count)` runs of an inode.
    fn block_runs(&self, inode: &Inode) -> Result<Vec<(u64, u64, u64)>> {
        let mut runs = Vec::new();
        let needed = inode.size.div_ceil(self.block_size);
        if inode.flags & EXTENTS_FL != 0 {
            self.walk_extents(&inode.block, 0, &mut runs)?;
        } else {
            let mut lblk = 0u64;
            for i in 0..12 {
                let b = u64::from(le32(&inode.block, i * 4));
                if lblk >= needed {
                    break;
                }
                if b != 0 {
                    runs.push((lblk, b, 1));
                }
                lblk += 1;
            }
            for level in 1..=3u32 {
                if lblk >= needed {
                    break;
                }
                let b = u64::from(le32(&inode.block, (11 + level as usize) * 4));
                self.walk_indirect(b, level, &mut lblk, needed, &mut runs)?;
            }
        }
        runs.sort_by_key(|r| r.0);
        Ok(runs)
    }

    fn walk_indirect(
        &self,
        block: u64,
        level: u32,
        lblk: &mut u64,
        needed: u64,
        runs: &mut Vec<(u64, u64, u64)>,
    ) -> Result<()> {
        let per = self.block_size / 4;
        let span = per.pow(level - 1);
        if block == 0 || block >= self.blocks_count {
            *lblk += per * span;
            return Ok(());
        }
        let ptrs = read_vec(
            self.p.as_ref(),
            block * self.block_size,
            self.block_size as usize,
        )?;
        for i in 0..per as usize {
            if *lblk >= needed {
                break;
            }
            let b = u64::from(le32(&ptrs, i * 4));
            if level == 1 {
                if b != 0 {
                    runs.push((*lblk, b, 1));
                }
                *lblk += 1;
            } else {
                self.walk_indirect(b, level - 1, lblk, needed, runs)?;
            }
        }
        Ok(())
    }

    fn walk_extents(
        &self,
        node: &[u8],
        depth_guard: u32,
        runs: &mut Vec<(u64, u64, u64)>,
    ) -> Result<()> {
        if le16(node, 0) != 0xF30A || depth_guard > 6 {
            return Err(invalid("bad extent header"));
        }
        let entries = usize::from(le16(node, 2));
        let depth = le16(node, 6);
        for i in 0..entries {
            let e = 12 + i * 12;
            if e + 12 > node.len() {
                break;
            }
            if depth == 0 {
                let mut len = u64::from(le16(node, e + 4));
                if len > 32_768 {
                    len -= 32_768;
                }
                let start = (u64::from(le16(node, e + 6)) << 32) | u64::from(le32(node, e + 8));
                runs.push((u64::from(le32(node, e)), start, len));
            } else {
                let leaf = (u64::from(le16(node, e + 8)) << 32) | u64::from(le32(node, e + 4));
                if leaf >= self.blocks_count {
                    continue;
                }
                let child = read_vec(
                    self.p.as_ref(),
                    leaf * self.block_size,
                    self.block_size as usize,
                )?;
                self.walk_extents(&child, depth_guard + 1, runs)?;
            }
        }
        Ok(())
    }

    fn inode_extents(&self, ino: u32, inode: &Inode) -> Result<Vec<Extent>> {
        let mut out = Vec::new();
        if let Some(at) = self.inline_location(ino, inode) {
            push_extent(&mut out, 0, Some(at), inode.size.min(60));
            return Ok(out);
        }
        for (l, p, n) in self.block_runs(inode)? {
            push_extent(
                &mut out,
                l * self.block_size,
                Some(p * self.block_size),
                n * self.block_size,
            );
        }
        truncate_extents(&mut out, inode.size);
        Ok(out)
    }

    fn entry_for(&self, ino: u32, name: String, parent: &str) -> FileEntry {
        let inode = self.inode(ino).ok();
        FileEntry {
            id: u64::from(ino),
            aux: 0,
            path: FileEntry::child_path(parent, &name),
            name,
            is_dir: inode.as_ref().is_some_and(|i| i.mode & S_IFMT == S_IFDIR),
            size: inode.as_ref().map_or(0, |i| i.size),
            deleted: false,
            metadata_offset: self.inode_offset(ino),
            modified: inode.as_ref().map(|i| i.mtime),
            created: inode.as_ref().and_then(|i| i.crtime),
            attributes: inode.as_ref().map_or(0, |i| u32::from(i.mode)),
        }
    }
}

impl FileSystem for ExtFs {
    fn info(&self) -> FsInfo {
        let sb = 1024;
        FsInfo {
            kind: self.kind,
            label: self.label.clone(),
            uuid: Some(self.uuid.clone()),
            block_size: self.block_size as u32,
            total_size: self.blocks_count * self.block_size,
            free_size: Some(self.free_blocks * self.block_size),
            fields: vec![
                InfoField::new("Superblock", "offset 1024", Some(sb)),
                InfoField::new("Inodes", self.inodes_count, Some(sb)),
                InfoField::new("Blocks", self.blocks_count, Some(sb + 4)),
                InfoField::new("Free blocks", self.free_blocks, Some(sb + 12)),
                InfoField::new("First data block", self.first_data_block, Some(sb + 20)),
                InfoField::new("Block size", self.block_size, Some(sb + 24)),
                InfoField::new("Blocks per group", self.blocks_per_group, Some(sb + 32)),
                InfoField::new("Inodes per group", self.inodes_per_group, Some(sb + 40)),
                InfoField::new("Inode size", self.inode_size, Some(sb + 88)),
                InfoField::new("Group descriptor size", self.desc_size, None),
                InfoField::new(
                    "Groups",
                    self.groups.len(),
                    Some((self.first_data_block + 1) * self.block_size),
                ),
                InfoField::new(
                    "Compatible features",
                    format!("{:#x}", self.feature_compat),
                    Some(sb + 92),
                ),
                InfoField::new(
                    "Incompatible features",
                    format!("{:#x}", self.feature_incompat),
                    Some(sb + 96),
                ),
                InfoField::new(
                    "Read-only compatible features",
                    format!("{:#x}", self.feature_ro_compat),
                    Some(sb + 100),
                ),
                InfoField::new("Last mounted on", &self.last_mounted, Some(sb + 136)),
                InfoField::new("Root inode", 2, self.inode_offset(2)),
            ],
        }
    }

    fn provider(&self) -> &Arc<dyn Provider> {
        &self.p
    }

    fn root(&self) -> FileEntry {
        let mut r = FileEntry::root(2);
        r.metadata_offset = self.inode_offset(2);
        r
    }

    fn list_dir(&self, dir: &FileEntry) -> Result<Vec<FileEntry>> {
        let ino = dir.id as u32;
        let inode = self.inode(ino)?;
        if inode.mode & S_IFMT != S_IFDIR {
            return Err(invalid("not a directory"));
        }
        let mut out = Vec::new();
        let filetype = self.feature_incompat & INCOMPAT_FILETYPE != 0;
        let (data, start) = if inode.flags & INLINE_DATA_FL != 0 {
            (inode.block.clone(), 4)
        } else {
            let exts = self.inode_extents(ino, &inode)?;
            let size = inode.size.min(64 << 20);
            let mut buf = vec![0u8; size as usize];
            let n = super::read_via_extents(self.p.as_ref(), &exts, size, 0, &mut buf)?;
            buf.truncate(n);
            (buf, 0)
        };
        let mut pos = start;
        while pos + 8 <= data.len() {
            let child = le32(&data, pos);
            let rec_len = usize::from(le16(&data, pos + 4));
            let name_len = usize::from(data[pos + 6])
                | if filetype {
                    0
                } else {
                    usize::from(data[pos + 7]) << 8
                };
            if rec_len < 8 || pos + rec_len > data.len() {
                break;
            }
            if child != 0 && name_len > 0 && pos + 8 + name_len <= data.len() {
                let name = String::from_utf8_lossy(&data[pos + 8..pos + 8 + name_len]).into_owned();
                if name != "." && name != ".." {
                    out.push(self.entry_for(child, name, &dir.path));
                }
            }
            pos += rec_len;
        }
        Ok(out)
    }

    fn extents(&self, file: &FileEntry) -> Result<Vec<Extent>> {
        let ino = file.id as u32;
        let inode = self.inode(ino)?;
        self.inode_extents(ino, &inode)
    }

    fn metadata_regions(&self) -> Result<Vec<Region>> {
        let bs = self.block_size;
        let mut r = vec![Region::meta(1024, 1024, "Superblock")];
        if bs > 2048 {
            r.push(Region::meta(0, 1024, "Boot sector"));
        }
        let gdt_len = (self.groups.len() as u64 * self.desc_size).div_ceil(bs) * bs;
        r.push(Region::meta(
            (self.first_data_block + 1) * bs,
            gdt_len,
            "Group descriptors",
        ));
        let table_len = u64::from(self.inodes_per_group) * self.inode_size;
        for (i, g) in self.groups.iter().enumerate() {
            r.push(Region::meta(
                g.block_bitmap * bs,
                bs,
                format!("Block bitmap (group {i})"),
            ));
            r.push(Region::meta(
                g.inode_bitmap * bs,
                bs,
                format!("Inode bitmap (group {i})"),
            ));
            r.push(Region::meta(
                g.inode_table * bs,
                table_len,
                format!("Inode table (group {i})"),
            ));
        }
        if self.feature_compat & COMPAT_HAS_JOURNAL != 0
            && let Ok(inode) = self.inode(8)
            && let Ok(exts) = self.inode_extents(8, &inode)
        {
            for e in exts {
                if let Some(p) = e.physical {
                    r.push(Region::meta(p, e.len, "Journal"));
                }
            }
        }
        Ok(r)
    }

    fn describe_offset(&self, offset: u64) -> Option<String> {
        self.inode_at(offset).map(|i| format!("inode {i}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::open_filesystem;
    use crate::fs::testutil::{mem, put, put16, put32, read_all};

    const BS: usize = 1024;

    fn inode(d: &mut [u8], ino: usize, mode: u16, size: u32, flags: u32, block: &[u8]) {
        let at = 5 * BS + (ino - 1) * 128;
        put16(d, at, mode);
        put32(d, at + 4, size);
        put32(d, at + 16, 1_600_000_000);
        put16(d, at + 26, 1);
        put32(d, at + 28, size.div_ceil(512));
        put32(d, at + 32, flags);
        put(d, at + 40, block);
    }

    fn dirent(d: &mut [u8], at: usize, ino: u32, rec: u16, ft: u8, name: &str) -> usize {
        put32(d, at, ino);
        put16(d, at + 4, rec);
        d[at + 6] = name.len() as u8;
        d[at + 7] = ft;
        put(d, at + 8, name.as_bytes());
        at + rec as usize
    }

    fn ptrs(list: &[u32]) -> Vec<u8> {
        list.iter().flat_map(|b| b.to_le_bytes()).collect()
    }

    fn ext_image() -> Vec<u8> {
        let mut d = vec![0u8; 64 * BS];
        let sb = 1024;
        put32(&mut d, sb, 16);
        put32(&mut d, sb + 4, 64);
        put32(&mut d, sb + 12, 10);
        put32(&mut d, sb + 20, 1);
        put32(&mut d, sb + 32, 8192);
        put32(&mut d, sb + 40, 16);
        put16(&mut d, sb + 56, EXT_MAGIC);
        put32(&mut d, sb + 76, 1);
        put32(&mut d, sb + 84, 11);
        put16(&mut d, sb + 88, 128);
        put32(&mut d, sb + 96, INCOMPAT_FILETYPE | INCOMPAT_EXTENTS);
        put(&mut d, sb + 104, &[0x11; 16]);
        put(&mut d, sb + 120, b"testfs");
        let gd = 2 * BS;
        put32(&mut d, gd, 3);
        put32(&mut d, gd + 4, 4);
        put32(&mut d, gd + 8, 5);

        inode(&mut d, 2, 0x41ED, 1024, 0, &ptrs(&[7]));
        inode(&mut d, 11, 0x41ED, 1024, 0, &ptrs(&[8]));
        inode(&mut d, 12, 0x81A4, 1500, 0, &ptrs(&[9, 10]));
        let mut ext = vec![0u8; 60];
        put16(&mut ext, 0, 0xF30A);
        put16(&mut ext, 2, 2);
        put16(&mut ext, 4, 4);
        put32(&mut ext, 12, 0);
        put16(&mut ext, 16, 2);
        put32(&mut ext, 20, 11);
        put32(&mut ext, 24, 2);
        put16(&mut ext, 28, 1);
        put32(&mut ext, 32, 20);
        inode(&mut d, 13, 0x81A4, 2500, EXTENTS_FL, &ext);
        let mut ind: Vec<u32> = (30..42).collect();
        ind.push(13);
        inode(&mut d, 14, 0x81A4, 14 * 1024 - 100, 0, &ptrs(&ind));
        put(&mut d, 13 * BS, &ptrs(&[42, 43]));
        inode(&mut d, 15, 0xA1FF, 10, 0, b"direct.txt");
        d[(15 - 1) * 128 + 5 * BS + 28..(15 - 1) * 128 + 5 * BS + 32].fill(0);

        let mut at = 7 * BS;
        at = dirent(&mut d, at, 2, 12, 2, ".");
        at = dirent(&mut d, at, 2, 12, 2, "..");
        at = dirent(&mut d, at, 11, 12, 2, "sub");
        at = dirent(&mut d, at, 12, 20, 1, "direct.txt");
        at = dirent(&mut d, at, 13, 20, 1, "extents.bin");
        at = dirent(&mut d, at, 14, 20, 1, "indirect.dat");
        dirent(&mut d, at, 15, (8 * BS - at) as u16, 7, "link");
        let at = dirent(&mut d, 8 * BS, 11, 12, 2, ".");
        dirent(&mut d, at, 2, (BS - 12) as u16, 2, "..");

        d[9 * BS..9 * BS + 1500].fill(b'd');
        d[11 * BS..13 * BS].fill(b'e');
        d[20 * BS..21 * BS].fill(b'E');
        for b in 30..44 {
            d[b * BS..(b + 1) * BS].fill(b as u8);
        }
        d
    }

    #[test]
    fn ext_tree_extents_inline_and_inodes() {
        let fs = open_filesystem(&mem(ext_image())).unwrap();
        let info = fs.info();
        assert_eq!(info.kind, FsKind::Ext4);
        assert_eq!(info.label.as_deref(), Some("testfs"));
        let root = fs.list_dir(&fs.root()).unwrap();
        let names: Vec<_> = root.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            ["sub", "direct.txt", "extents.bin", "indirect.dat", "link"]
        );
        assert!(root[0].is_dir);
        assert_eq!(root[1].metadata_offset, Some(5 * 1024 + 11 * 128));
        assert_eq!(root[1].modified, Some(1_600_000_000));
        assert_eq!(read_all(fs.as_ref(), &root[1]), vec![b'd'; 1500]);

        let ex = fs.extents(&root[2]).unwrap();
        assert_eq!(ex.len(), 2);
        assert_eq!(
            ex[1],
            Extent {
                logical: 2048,
                physical: Some(20 * 1024),
                len: 452
            }
        );
        let c = read_all(fs.as_ref(), &root[2]);
        assert!(c[..2048].iter().all(|&b| b == b'e') && c[2048..].iter().all(|&b| b == b'E'));

        let c = read_all(fs.as_ref(), &root[3]);
        assert_eq!(c.len(), 14 * 1024 - 100);
        assert_eq!(c[12 * 1024], 42);
        assert_eq!(c[13 * 1024], 43);
        assert_eq!(fs.extents(&root[3]).unwrap().len(), 1);

        assert_eq!(read_all(fs.as_ref(), &root[4]), b"direct.txt");
        assert_eq!(fs.list_dir(&root[0]).unwrap().len(), 0);
        assert_eq!(
            fs.describe_offset(5 * 1024 + 12 * 128 + 5),
            Some("inode 13".into())
        );
        let meta = fs.metadata_regions().unwrap();
        assert!(
            meta.iter()
                .any(|r| r.label == "Inode table (group 0)" && r.len == 2048)
        );
    }
}
