//! Detection plus key superblock fields for file systems that are not
//! browsed (DISK-05): HFS+/HFSX, APFS, Btrfs, XFS and UDF.

use hexa_core::Result;
use hexa_core::provider::Provider;

use super::{FsInfo, FsKind, InfoField};
use crate::util::{ascii, be16, be32, be64, le16, le32, le64, read_upto, utf16be, uuid_string};

/// Seconds between 1904-01-01 (HFS epoch) and 1970-01-01.
const HFS_EPOCH: i64 = 2_082_844_800;

/// Recognises HFS+, APFS, Btrfs, XFS and UDF.
pub fn probe_other(p: &dyn Provider) -> Result<Option<FsInfo>> {
    let head = read_upto(p, 0, 4096)?;
    if let Some(i) = apfs(&head) {
        return Ok(Some(i));
    }
    if let Some(i) = xfs(&head) {
        return Ok(Some(i));
    }
    if let Some(i) = hfs_plus(&head) {
        return Ok(Some(i));
    }
    let btrfs_sb = read_upto(p, 0x10000, 4096)?;
    if let Some(i) = btrfs(&btrfs_sb) {
        return Ok(Some(i));
    }
    udf(p)
}

fn hfs_plus(b: &[u8]) -> Option<FsInfo> {
    let h = b.get(1024..1536)?;
    let kind = match &h[0..2] {
        b"H+" => FsKind::HfsPlus,
        b"HX" => FsKind::Hfsx,
        _ => return None,
    };
    let block = be32(h, 40);
    if block == 0 || !block.is_power_of_two() {
        return None;
    }
    let total = u64::from(be32(h, 44));
    let free = u64::from(be32(h, 48));
    let date = |off: usize| {
        let t = i64::from(be32(h, off));
        if t == 0 {
            "-".to_string()
        } else {
            (t - HFS_EPOCH).to_string()
        }
    };
    Some(FsInfo {
        kind,
        label: None,
        uuid: None,
        block_size: block,
        total_size: total * u64::from(block),
        free_size: Some(free * u64::from(block)),
        fields: vec![
            InfoField::new("Volume header", "offset 1024", Some(1024)),
            InfoField::new("Version", be16(h, 2), Some(1026)),
            InfoField::new("Attributes", format!("{:#x}", be32(h, 4)), Some(1028)),
            InfoField::new("Last mounted by", ascii(&h[8..12]), Some(1032)),
            InfoField::new("Created (unix)", date(16), Some(1040)),
            InfoField::new("Modified (unix)", date(20), Some(1044)),
            InfoField::new("Files", be32(h, 32), Some(1056)),
            InfoField::new("Folders", be32(h, 36), Some(1060)),
            InfoField::new("Block size", block, Some(1064)),
            InfoField::new("Total blocks", total, Some(1068)),
            InfoField::new("Free blocks", free, Some(1072)),
        ],
    })
}

fn apfs(b: &[u8]) -> Option<FsInfo> {
    if b.get(32..36)? != b"NXSB" {
        return None;
    }
    let block = le32(b, 36);
    let count = le64(b, 40);
    Some(FsInfo {
        kind: FsKind::Apfs,
        label: None,
        uuid: Some(uuid_string(&b[72..88])),
        block_size: block,
        total_size: count * u64::from(block),
        free_size: None,
        fields: vec![
            InfoField::new("Container superblock", "NXSB", Some(32)),
            InfoField::new("Block size", block, Some(36)),
            InfoField::new("Block count", count, Some(40)),
            InfoField::new("Features", format!("{:#x}", le64(b, 48)), Some(48)),
            InfoField::new(
                "Read-only compatible features",
                format!("{:#x}", le64(b, 56)),
                Some(56),
            ),
            InfoField::new(
                "Incompatible features",
                format!("{:#x}", le64(b, 64)),
                Some(64),
            ),
            InfoField::new("Next object id", le64(b, 88), Some(88)),
            InfoField::new("Next transaction id", le64(b, 96), Some(96)),
            InfoField::new("Max file systems", le32(b, 180), Some(180)),
        ],
    })
}

fn btrfs(b: &[u8]) -> Option<FsInfo> {
    if b.get(0x40..0x48)? != b"_BHRfS_M" {
        return None;
    }
    let base = 0x10000u64;
    let sector = le32(b, 0x90);
    Some(FsInfo {
        kind: FsKind::Btrfs,
        label: Some(ascii(crate::util::slice(b, 0x12B, 256))).filter(|s| !s.is_empty()),
        uuid: Some(uuid_string(&b[0x20..0x30])),
        block_size: sector,
        total_size: le64(b, 0x70),
        free_size: Some(le64(b, 0x70).saturating_sub(le64(b, 0x78))),
        fields: vec![
            InfoField::new("Superblock", "offset 0x10000", Some(base)),
            InfoField::new("Generation", le64(b, 0x48), Some(base + 0x48)),
            InfoField::new(
                "Root tree",
                format!("{:#x}", le64(b, 0x50)),
                Some(base + 0x50),
            ),
            InfoField::new(
                "Chunk tree",
                format!("{:#x}", le64(b, 0x58)),
                Some(base + 0x58),
            ),
            InfoField::new("Total bytes", le64(b, 0x70), Some(base + 0x70)),
            InfoField::new("Bytes used", le64(b, 0x78), Some(base + 0x78)),
            InfoField::new("Devices", le64(b, 0x88), Some(base + 0x88)),
            InfoField::new("Sector size", sector, Some(base + 0x90)),
            InfoField::new("Node size", le32(b, 0x94), Some(base + 0x94)),
        ],
    })
}

fn xfs(b: &[u8]) -> Option<FsInfo> {
    if b.get(0..4)? != b"XFSB" {
        return None;
    }
    let block = be32(b, 4);
    let dblocks = be64(b, 8);
    Some(FsInfo {
        kind: FsKind::Xfs,
        label: Some(ascii(&b[108..120])).filter(|s| !s.is_empty()),
        uuid: Some(uuid_string(&b[32..48])),
        block_size: block,
        total_size: dblocks * u64::from(block),
        free_size: Some(be64(b, 144) * u64::from(block)),
        fields: vec![
            InfoField::new("Block size", block, Some(4)),
            InfoField::new("Data blocks", dblocks, Some(8)),
            InfoField::new("Log start", be64(b, 48), Some(48)),
            InfoField::new("Root inode", be64(b, 56), Some(56)),
            InfoField::new("AG blocks", be32(b, 84), Some(84)),
            InfoField::new("AG count", be32(b, 88), Some(88)),
            InfoField::new("Version", format!("{:#x}", be16(b, 100)), Some(100)),
            InfoField::new("Sector size", be16(b, 102), Some(102)),
            InfoField::new("Inode size", be16(b, 104), Some(104)),
            InfoField::new("Inodes", be64(b, 128), Some(128)),
        ],
    })
}

/// OSTA compressed Unicode `dstring` (last byte = used length).
fn dstring(b: &[u8]) -> String {
    let Some((&used, body)) = b.split_last() else {
        return String::new();
    };
    let body = &body[..usize::from(used).min(body.len())];
    match body.split_first() {
        Some((8, rest)) => rest.iter().map(|&c| c as char).collect(),
        Some((16, rest)) => utf16be(rest),
        _ => String::new(),
    }
}

fn udf(p: &dyn Provider) -> Result<Option<FsInfo>> {
    let vrs = read_upto(p, 32_768, 16 * 2048)?;
    let nsr = vrs
        .chunks(2048)
        .take_while(|s| s.len() >= 6)
        .find_map(|s| match &s[1..6] {
            b"NSR02" => Some(2),
            b"NSR03" => Some(3),
            _ => None,
        });
    let Some(nsr) = nsr else {
        return Ok(None);
    };
    let mut info = FsInfo {
        kind: FsKind::Udf,
        label: None,
        uuid: None,
        block_size: 2048,
        total_size: p.len(),
        free_size: None,
        fields: vec![InfoField::new(
            "NSR descriptor",
            format!("NSR0{nsr}"),
            Some(32_768),
        )],
    };
    let anchor = read_upto(p, 256 * 2048, 512)?;
    if le16(&anchor, 0) == 2 {
        let len = u64::from(le32(&anchor, 16));
        let loc = u64::from(le32(&anchor, 20));
        info.fields.push(InfoField::new(
            "Anchor volume descriptor",
            "sector 256",
            Some(256 * 2048),
        ));
        for i in 0..(len / 2048).min(64) {
            let off = (loc + i) * 2048;
            let d = read_upto(p, off, 2048)?;
            match le16(&d, 0) {
                1 => {
                    let id = dstring(crate::util::slice(&d, 24, 32));
                    info.fields
                        .push(InfoField::new("Volume identifier", &id, Some(off + 24)));
                    if info.label.is_none() && !id.is_empty() {
                        info.label = Some(id);
                    }
                }
                6 => {
                    let lb = le32(&d, 212);
                    if lb.is_power_of_two() {
                        info.block_size = lb;
                    }
                    let id = dstring(crate::util::slice(&d, 84, 128));
                    info.fields
                        .push(InfoField::new("Logical volume", &id, Some(off + 84)));
                    info.fields
                        .push(InfoField::new("Logical block size", lb, Some(off + 212)));
                    if !id.is_empty() {
                        info.label = Some(id);
                    }
                }
                8 | 0 => break,
                _ => {}
            }
        }
    }
    Ok(Some(info))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::probe_filesystem;
    use crate::fs::testutil::{mem, put, put16, put32, put64};

    #[test]
    fn detects_other_file_systems() {
        let mut hfs = vec![0u8; 8192];
        put(&mut hfs, 1024, b"H+");
        put(&mut hfs, 1024 + 40, &4096u32.to_be_bytes());
        put(&mut hfs, 1024 + 44, &100u32.to_be_bytes());
        put(&mut hfs, 1024 + 48, &40u32.to_be_bytes());
        let i = probe_filesystem(&mem(hfs)).unwrap().unwrap();
        assert_eq!(i.kind, FsKind::HfsPlus);
        assert_eq!(i.total_size, 409_600);
        assert_eq!(i.free_size, Some(163_840));

        let mut apfs = vec![0u8; 4096];
        put(&mut apfs, 32, b"NXSB");
        put32(&mut apfs, 36, 4096);
        put64(&mut apfs, 40, 1000);
        let i = probe_filesystem(&mem(apfs)).unwrap().unwrap();
        assert_eq!(i.kind, FsKind::Apfs);
        assert_eq!(i.total_size, 4_096_000);

        let mut xfs = vec![0u8; 4096];
        put(&mut xfs, 0, b"XFSB");
        put(&mut xfs, 4, &4096u32.to_be_bytes());
        put(&mut xfs, 8, &10u64.to_be_bytes());
        put(&mut xfs, 108, b"data");
        let i = probe_filesystem(&mem(xfs)).unwrap().unwrap();
        assert_eq!((i.kind, i.label.as_deref()), (FsKind::Xfs, Some("data")));

        let mut btrfs = vec![0u8; 0x11000];
        put(&mut btrfs, 0x10040, b"_BHRfS_M");
        put64(&mut btrfs, 0x10070, 1 << 30);
        put32(&mut btrfs, 0x10090, 4096);
        put(&mut btrfs, 0x1012B, b"pool");
        let i = probe_filesystem(&mem(btrfs)).unwrap().unwrap();
        assert_eq!((i.kind, i.label.as_deref()), (FsKind::Btrfs, Some("pool")));

        let mut udf = vec![0u8; 300 * 2048];
        put(&mut udf, 32_768 + 1, b"BEA01");
        put(&mut udf, 32_768 + 2048 + 1, b"NSR03");
        put16(&mut udf, 256 * 2048, 2);
        put32(&mut udf, 256 * 2048 + 16, 2 * 2048);
        put32(&mut udf, 256 * 2048 + 20, 260);
        put16(&mut udf, 260 * 2048, 6);
        put32(&mut udf, 260 * 2048 + 212, 2048);
        let mut id = vec![0u8; 128];
        id[0] = 8;
        id[1..7].copy_from_slice(b"MYDISC");
        id[127] = 7;
        put(&mut udf, 260 * 2048 + 84, &id);
        let i = probe_filesystem(&mem(udf)).unwrap().unwrap();
        assert_eq!((i.kind, i.label.as_deref()), (FsKind::Udf, Some("MYDISC")));

        assert!(probe_filesystem(&mem(vec![0u8; 70_000])).unwrap().is_none());
    }
}
