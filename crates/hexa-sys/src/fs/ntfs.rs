//! NTFS (DISK-05/06/07): boot sector, MFT records with update-sequence
//! fixups, attribute lists, `$FILE_NAME`, data runs, directory listing via
//! `$INDEX_ROOT`/`$INDEX_ALLOCATION`, MFT-record navigation and deleted
//! (not in use) MFT records.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use hexa_core::provider::Provider;
use hexa_core::{Cancel, Result};

use super::{
    Extent, FileEntry, FileSystem, FsInfo, FsKind, InfoField, Region, filetime, push_extent,
    truncate_extents,
};
use crate::util::{check_cancel, invalid, le16, le32, le64, read_vec, slice, utf16le};

const AT_STANDARD_INFORMATION: u32 = 0x10;
const AT_ATTRIBUTE_LIST: u32 = 0x20;
const AT_FILE_NAME: u32 = 0x30;
const AT_VOLUME_NAME: u32 = 0x60;
const AT_VOLUME_INFORMATION: u32 = 0x70;
const AT_DATA: u32 = 0x80;
const AT_INDEX_ROOT: u32 = 0x90;
const AT_INDEX_ALLOCATION: u32 = 0xA0;
const REF_MASK: u64 = 0xFFFF_FFFF_FFFF;
/// Root directory record.
pub const ROOT_RECORD: u64 = 5;
const SYSTEM_FILES: [&str; 12] = [
    "$MFT", "$MFTMirr", "$LogFile", "$Volume", "$AttrDef", "$Root", "$Bitmap", "$Boot", "$BadClus",
    "$Secure", "$UpCase", "$Extend",
];

/// A data run in clusters (`lcn: None` = sparse).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Run {
    vcn: u64,
    lcn: Option<u64>,
    len: u64,
}

/// One parsed attribute.
#[derive(Debug, Clone)]
struct Attr {
    kind: u32,
    name: String,
    /// Record holding the attribute.
    record: u64,
    /// Resident value: offset inside the record and length.
    resident: Option<(usize, usize)>,
    /// Resident value bytes (copied from the fixed-up record).
    value: Vec<u8>,
    runs: Vec<Run>,
    start_vcn: u64,
    real_size: u64,
}

/// Decoded `$FILE_NAME`.
#[derive(Debug, Clone)]
struct FileName {
    parent: u64,
    created: u64,
    modified: u64,
    flags: u32,
    namespace: u8,
    name: String,
}

fn parse_file_name(k: &[u8]) -> Option<FileName> {
    if k.len() < 66 {
        return None;
    }
    let n = usize::from(k[64]);
    let raw = slice(k, 66, n * 2);
    let units: Vec<u16> = raw
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    Some(FileName {
        parent: le64(k, 0) & REF_MASK,
        created: le64(k, 8),
        modified: le64(k, 16),
        flags: le32(k, 56),
        namespace: k[65],
        name: String::from_utf16_lossy(&units),
    })
}

/// Applies the update sequence array; `false` when the record is torn.
fn apply_fixup(buf: &mut [u8]) -> bool {
    let usa = usize::from(le16(buf, 4));
    let count = usize::from(le16(buf, 6));
    if count == 0 || usa + count * 2 > buf.len() {
        return false;
    }
    let usn = [buf[usa], buf[usa + 1]];
    for i in 1..count {
        let pos = i * 512 - 2;
        if pos + 2 > buf.len() {
            break;
        }
        if buf[pos..pos + 2] != usn {
            return false;
        }
        buf[pos] = buf[usa + i * 2];
        buf[pos + 1] = buf[usa + i * 2 + 1];
    }
    true
}

fn decode_runs(data: &[u8], start_vcn: u64) -> Vec<Run> {
    let mut runs = Vec::new();
    let mut pos = 0;
    let mut vcn = start_vcn;
    let mut lcn: i64 = 0;
    while pos < data.len() {
        let h = data[pos];
        if h == 0 {
            break;
        }
        let ls = usize::from(h & 0xF);
        let os = usize::from(h >> 4);
        if ls == 0 || ls > 8 || os > 8 || pos + 1 + ls + os > data.len() {
            break;
        }
        let mut len = 0u64;
        for i in 0..ls {
            len |= u64::from(data[pos + 1 + i]) << (8 * i);
        }
        let run_lcn = if os == 0 {
            None
        } else {
            let mut off = 0i64;
            for i in 0..os {
                off |= i64::from(data[pos + 1 + ls + i]) << (8 * i);
            }
            let shift = 64 - 8 * os as u32;
            off = (off << shift) >> shift;
            lcn = lcn.wrapping_add(off);
            (lcn >= 0).then_some(lcn as u64)
        };
        runs.push(Run {
            vcn,
            lcn: run_lcn,
            len,
        });
        vcn += len;
        pos += 1 + ls + os;
    }
    runs
}

/// An NTFS volume.
#[derive(Debug)]
pub struct NtfsFs {
    p: Arc<dyn Provider>,
    bytes_per_sector: u64,
    cluster_size: u64,
    total_sectors: u64,
    mft_lcn: u64,
    mftmirr_lcn: u64,
    record_size: u64,
    index_block_size: u64,
    serial: u64,
    mft_runs: Vec<Run>,
    mft_size: u64,
    label: Option<String>,
    version: Option<(u8, u8)>,
}

fn size_field(v: u8, cluster: u64) -> u64 {
    let s = v as i8;
    if s < 0 {
        1u64 << (-i32::from(s)).min(31)
    } else {
        u64::from(v) * cluster
    }
}

impl NtfsFs {
    pub fn open(p: Arc<dyn Provider>) -> Result<Self> {
        let b = read_vec(p.as_ref(), 0, 512)?;
        if &b[3..11] != b"NTFS    " {
            return Err(invalid("not an NTFS boot sector"));
        }
        let bps = u64::from(le16(&b, 11));
        let spc_raw = b[13];
        let spc = if spc_raw > 0x80 {
            1u64 << (256 - u32::from(spc_raw)).min(31)
        } else {
            u64::from(spc_raw)
        };
        if !matches!(bps, 256 | 512 | 1024 | 2048 | 4096) || spc == 0 {
            return Err(invalid("invalid NTFS geometry"));
        }
        let cluster = bps * spc;
        let record_size = size_field(b[64], cluster);
        if !(256..=65_536).contains(&record_size) {
            return Err(invalid("invalid MFT record size"));
        }
        let mut fs = Self {
            bytes_per_sector: bps,
            cluster_size: cluster,
            total_sectors: le64(&b, 40),
            mft_lcn: le64(&b, 48),
            mftmirr_lcn: le64(&b, 56),
            record_size,
            index_block_size: size_field(b[68], cluster),
            serial: le64(&b, 72),
            mft_runs: Vec::new(),
            mft_size: 0,
            label: None,
            version: None,
            p,
        };
        // Bootstrap: record 0 sits at the MFT start; its $DATA maps the rest.
        let n = record_size.div_ceil(cluster);
        fs.mft_runs = vec![Run {
            vcn: 0,
            lcn: Some(fs.mft_lcn),
            len: n.max(1),
        }];
        let attrs = fs.attributes(0)?.1;
        let (runs, size) = Self::nonresident_data(&attrs, AT_DATA, "");
        if runs.is_empty() {
            return Err(invalid("$MFT has no data runs"));
        }
        fs.mft_runs = runs;
        fs.mft_size = size;
        if let Ok((_, vol)) = fs.attributes(3) {
            for a in &vol {
                if a.kind == AT_VOLUME_NAME {
                    fs.label = Some(utf16le(&a.value)).filter(|s| !s.is_empty());
                }
                if a.kind == AT_VOLUME_INFORMATION && a.value.len() >= 10 {
                    fs.version = Some((a.value[8], a.value[9]));
                }
            }
        }
        Ok(fs)
    }

    pub fn record_size(&self) -> u64 {
        self.record_size
    }

    fn map(&self, runs: &[Run], byte_pos: u64) -> Option<u64> {
        let vcn = byte_pos / self.cluster_size;
        runs.iter()
            .find(|r| vcn >= r.vcn && vcn < r.vcn + r.len)
            .and_then(|r| {
                r.lcn
                    .map(|l| (l + vcn - r.vcn) * self.cluster_size + byte_pos % self.cluster_size)
            })
    }

    /// Byte offset of MFT record `n` (navigation target).
    pub fn record_offset(&self, n: u64) -> Option<u64> {
        self.map(&self.mft_runs, n * self.record_size)
    }

    /// MFT record containing `offset`.
    pub fn record_at(&self, offset: u64) -> Option<u64> {
        let lcn = offset / self.cluster_size;
        self.mft_runs.iter().find_map(|r| {
            let l = r.lcn?;
            (lcn >= l && lcn < l + r.len).then(|| {
                ((r.vcn + lcn - l) * self.cluster_size + offset % self.cluster_size)
                    / self.record_size
            })
        })
    }

    /// Reads `len` bytes at `pos` of a non-resident stream.
    fn read_stream(&self, runs: &[Run], pos: u64, len: usize) -> Result<Vec<u8>> {
        let mut out = vec![0u8; len];
        let mut done = 0usize;
        while done < len {
            let at = pos + done as u64;
            let n = ((self.cluster_size - at % self.cluster_size) as usize).min(len - done);
            if let Some(phys) = self.map(runs, at) {
                self.p.read_at(phys, &mut out[done..done + n])?;
            }
            done += n;
        }
        Ok(out)
    }

    /// Raw MFT record `n` with fixups applied.
    fn read_record(&self, n: u64) -> Result<Vec<u8>> {
        let mut rec = self.read_stream(
            &self.mft_runs,
            n * self.record_size,
            self.record_size as usize,
        )?;
        if &rec[0..4] != b"FILE" {
            return Err(invalid(format!("MFT record {n} has no FILE signature")));
        }
        if !apply_fixup(&mut rec) {
            return Err(invalid(format!("MFT record {n} failed the fixup check")));
        }
        Ok(rec)
    }

    fn parse_attrs(rec: &[u8], record: u64) -> Vec<Attr> {
        let mut out = Vec::new();
        let mut off = usize::from(le16(rec, 20));
        let used = (le32(rec, 24) as usize).min(rec.len());
        while off + 16 <= used {
            let kind = le32(rec, off);
            if kind == 0xFFFF_FFFF || kind == 0 {
                break;
            }
            let len = le32(rec, off + 4) as usize;
            if len < 16 || off + len > used {
                break;
            }
            let a = &rec[off..off + len];
            let name_len = usize::from(a[9]);
            let name = utf16le(slice(a, usize::from(le16(a, 10)), name_len * 2));
            let attr = if a[8] == 0 {
                let vlen = le32(a, 16) as usize;
                let voff = usize::from(le16(a, 20));
                Attr {
                    kind,
                    name,
                    record,
                    resident: Some((off + voff, vlen)),
                    value: slice(a, voff, vlen).to_vec(),
                    runs: Vec::new(),
                    start_vcn: 0,
                    real_size: vlen as u64,
                }
            } else {
                let start_vcn = le64(a, 16);
                let rl = usize::from(le16(a, 32));
                Attr {
                    kind,
                    name,
                    record,
                    resident: None,
                    value: Vec::new(),
                    runs: decode_runs(slice(a, rl, len.saturating_sub(rl)), start_vcn),
                    start_vcn,
                    real_size: le64(a, 48),
                }
            };
            out.push(attr);
            off += len;
        }
        out
    }

    /// The base record of `n` and all its attributes, following an
    /// `$ATTRIBUTE_LIST` into extension records.
    fn attributes(&self, n: u64) -> Result<(Vec<u8>, Vec<Attr>)> {
        let rec = self.read_record(n)?;
        let mut attrs = Self::parse_attrs(&rec, n);
        let list = attrs.iter().find(|a| a.kind == AT_ATTRIBUTE_LIST).cloned();
        if let Some(list) = list {
            let data = if list.resident.is_some() {
                list.value
            } else {
                self.read_stream(&list.runs, 0, list.real_size.min(1 << 20) as usize)?
            };
            let mut seen = HashSet::from([n]);
            let mut pos = 0;
            while pos + 26 <= data.len() {
                let len = usize::from(le16(&data, pos + 4));
                if len < 26 {
                    break;
                }
                let ext = le64(&data, pos + 16) & REF_MASK;
                if seen.insert(ext)
                    && let Ok(r) = self.read_record(ext)
                {
                    attrs.extend(Self::parse_attrs(&r, ext));
                }
                pos += len;
            }
        }
        Ok((rec, attrs))
    }

    /// Runs and real size of a non-resident stream spread over attributes.
    fn nonresident_data(attrs: &[Attr], kind: u32, name: &str) -> (Vec<Run>, u64) {
        let mut parts: Vec<&Attr> = attrs
            .iter()
            .filter(|a| a.kind == kind && a.name == name && a.resident.is_none())
            .collect();
        parts.sort_by_key(|a| a.start_vcn);
        let size = parts
            .iter()
            .find(|a| a.start_vcn == 0)
            .map_or(0, |a| a.real_size);
        (
            parts.iter().flat_map(|a| a.runs.iter().copied()).collect(),
            size,
        )
    }

    fn best_name(attrs: &[Attr]) -> Option<FileName> {
        let names: Vec<FileName> = attrs
            .iter()
            .filter(|a| a.kind == AT_FILE_NAME)
            .filter_map(|a| parse_file_name(&a.value))
            .collect();
        names
            .iter()
            .find(|f| f.namespace != 2)
            .or_else(|| names.first())
            .cloned()
    }

    fn data_size(attrs: &[Attr]) -> u64 {
        match attrs
            .iter()
            .find(|a| a.kind == AT_DATA && a.name.is_empty())
        {
            Some(a) if a.resident.is_some() => a.value.len() as u64,
            Some(_) => Self::nonresident_data(attrs, AT_DATA, "").1,
            None => 0,
        }
    }

    fn entry(
        &self,
        n: u64,
        rec: &[u8],
        attrs: &[Attr],
        name: Option<&FileName>,
        parent_path: &str,
    ) -> FileEntry {
        let si = attrs.iter().find(|a| a.kind == AT_STANDARD_INFORMATION);
        let name_s = name
            .as_ref()
            .map_or_else(|| format!("[record {n}]"), |f| f.name.clone());
        let is_dir = le16(rec, 22) & 2 != 0;
        FileEntry {
            id: n,
            aux: u64::from(le16(rec, 16)),
            path: FileEntry::child_path(parent_path, &name_s),
            name: name_s,
            is_dir,
            size: if is_dir { 0 } else { Self::data_size(attrs) },
            deleted: le16(rec, 22) & 1 == 0,
            metadata_offset: self.record_offset(n),
            modified: si
                .and_then(|a| filetime(le64(&a.value, 8)))
                .or_else(|| name.as_ref().and_then(|f| filetime(f.modified))),
            created: si
                .and_then(|a| filetime(le64(&a.value, 0)))
                .or_else(|| name.as_ref().and_then(|f| filetime(f.created))),
            attributes: si.map_or_else(
                || name.as_ref().map_or(0, |f| f.flags),
                |a| le32(&a.value, 32),
            ),
        }
    }

    fn index_entries(buf: &[u8], start: usize, end: usize, out: &mut Vec<(u64, FileName)>) {
        let mut pos = start;
        let end = end.min(buf.len());
        while pos + 16 <= end {
            let elen = usize::from(le16(buf, pos + 8));
            let klen = usize::from(le16(buf, pos + 10));
            let flags = le16(buf, pos + 12);
            if flags & 2 != 0 || elen < 16 {
                break;
            }
            if let Some(f) = parse_file_name(slice(buf, pos + 16, klen)) {
                out.push((le64(buf, pos), f));
            }
            pos += elen;
        }
    }

    fn dir_index(&self, attrs: &[Attr]) -> Result<Vec<(u64, FileName)>> {
        let mut found = Vec::new();
        let root = attrs
            .iter()
            .find(|a| a.kind == AT_INDEX_ROOT && a.name == "$I30")
            .ok_or_else(|| invalid("directory has no $I30 index"))?;
        let v = &root.value;
        let block_size = match u64::from(le32(v, 8)) {
            0 => self.index_block_size,
            n => n,
        };
        Self::index_entries(
            v,
            16 + le32(v, 16) as usize,
            16 + le32(v, 20) as usize,
            &mut found,
        );
        let (runs, size) = Self::nonresident_data(attrs, AT_INDEX_ALLOCATION, "$I30");
        if !runs.is_empty() && block_size >= 512 {
            let blocks = size.div_ceil(block_size).min(1 << 16);
            for i in 0..blocks {
                let mut blk = self.read_stream(&runs, i * block_size, block_size as usize)?;
                if &blk[0..4] != b"INDX" || !apply_fixup(&mut blk) {
                    continue;
                }
                let start = 0x18 + le32(&blk, 0x18) as usize;
                let end = 0x18 + le32(&blk, 0x1C) as usize;
                Self::index_entries(&blk, start, end, &mut found);
            }
        }
        Ok(found)
    }

    fn data_attr<'a>(attrs: &'a [Attr], file: &FileEntry) -> Option<&'a Attr> {
        let kind = if file.is_dir {
            AT_INDEX_ALLOCATION
        } else {
            AT_DATA
        };
        let name = if file.is_dir { "$I30" } else { "" };
        attrs.iter().find(|a| a.kind == kind && a.name == name)
    }

    fn path_of(&self, mut parent: u64, cache: &mut HashMap<u64, String>) -> String {
        let mut parts = Vec::new();
        let mut guard = 0;
        while parent != ROOT_RECORD && guard < 64 {
            guard += 1;
            if let Some(p) = cache.get(&parent) {
                parts.push(p.clone());
                break;
            }
            let Ok((_, attrs)) = self.attributes(parent) else {
                parts.push(format!("[record {parent}]"));
                break;
            };
            let Some(f) = Self::best_name(&attrs) else {
                break;
            };
            parts.push(f.name);
            parent = f.parent;
        }
        parts.reverse();
        let path = format!("/{}", parts.join("/"));
        path.replace("//", "/")
    }
}

impl FileSystem for NtfsFs {
    fn info(&self) -> FsInfo {
        let mut fields = vec![
            InfoField::new("Bytes per sector", self.bytes_per_sector, Some(11)),
            InfoField::new("Cluster size", self.cluster_size, Some(13)),
            InfoField::new("Total sectors", self.total_sectors, Some(40)),
            InfoField::new("$MFT cluster", self.mft_lcn, Some(48)),
            InfoField::new("$MFTMirr cluster", self.mftmirr_lcn, Some(56)),
            InfoField::new("MFT record size", self.record_size, Some(64)),
            InfoField::new("Index block size", self.index_block_size, Some(68)),
            InfoField::new("Serial number", format!("{:016X}", self.serial), Some(72)),
            InfoField::new(
                "MFT records",
                self.mft_size / self.record_size,
                self.record_offset(0),
            ),
            InfoField::new(
                "Root directory",
                "MFT record 5",
                self.record_offset(ROOT_RECORD),
            ),
        ];
        if let Some((major, minor)) = self.version {
            fields.push(InfoField::new(
                "NTFS version",
                format!("{major}.{minor}"),
                self.record_offset(3),
            ));
        }
        FsInfo {
            kind: FsKind::Ntfs,
            label: self.label.clone(),
            uuid: Some(format!("{:016X}", self.serial)),
            block_size: self.cluster_size as u32,
            total_size: self.total_sectors * self.bytes_per_sector,
            free_size: None,
            fields,
        }
    }

    fn provider(&self) -> &Arc<dyn Provider> {
        &self.p
    }

    fn root(&self) -> FileEntry {
        let mut r = FileEntry::root(ROOT_RECORD);
        r.metadata_offset = self.record_offset(ROOT_RECORD);
        r
    }

    fn list_dir(&self, dir: &FileEntry) -> Result<Vec<FileEntry>> {
        let (_, attrs) = self.attributes(dir.id)?;
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for (r, f) in self.dir_index(&attrs)? {
            let n = r & REF_MASK;
            let seq = (r >> 48) as u16;
            if f.namespace == 2 || n == dir.id || !seen.insert(n) {
                continue;
            }
            let Ok((rec, child)) = self.attributes(n) else {
                continue;
            };
            // Stale entries in unused index blocks point at reused records.
            if le16(&rec, 22) & 1 == 0 || (seq != 0 && le16(&rec, 16) != seq) {
                continue;
            }
            out.push(self.entry(n, &rec, &child, Some(&f), &dir.path));
        }
        Ok(out)
    }

    fn extents(&self, file: &FileEntry) -> Result<Vec<Extent>> {
        let (_, attrs) = self.attributes(file.id)?;
        let mut out = Vec::new();
        let Some(a) = Self::data_attr(&attrs, file) else {
            return Ok(out);
        };
        if let Some((off, len)) = a.resident {
            if let Some(base) = self.record_offset(a.record) {
                push_extent(&mut out, 0, Some(base + off as u64), len as u64);
            }
            return Ok(out);
        }
        let (kind, name) = (a.kind, a.name.clone());
        let (runs, size) = Self::nonresident_data(&attrs, kind, &name);
        for r in runs {
            push_extent(
                &mut out,
                r.vcn * self.cluster_size,
                r.lcn.map(|l| l * self.cluster_size),
                r.len * self.cluster_size,
            );
        }
        truncate_extents(&mut out, size);
        Ok(out)
    }

    fn read_file(&self, file: &FileEntry, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let (_, attrs) = self.attributes(file.id)?;
        if let Some(a) = Self::data_attr(&attrs, file)
            && a.resident.is_some()
        {
            let v = &a.value;
            if offset >= v.len() as u64 {
                return Ok(0);
            }
            let n = (v.len() - offset as usize).min(buf.len());
            buf[..n].copy_from_slice(&v[offset as usize..offset as usize + n]);
            return Ok(n);
        }
        super::read_via_extents(
            self.p.as_ref(),
            &self.extents(file)?,
            file.size,
            offset,
            buf,
        )
    }

    fn metadata_regions(&self) -> Result<Vec<Region>> {
        let mut r = vec![Region::meta(0, self.bytes_per_sector, "Boot sector")];
        let mirr = self.mftmirr_lcn * self.cluster_size;
        r.push(Region::meta(mirr, 4 * self.record_size, "$MFTMirr"));
        for r0 in &self.mft_runs {
            if let Some(l) = r0.lcn {
                r.push(Region::meta(
                    l * self.cluster_size,
                    r0.len * self.cluster_size,
                    "$MFT",
                ));
            }
        }
        for (n, name) in SYSTEM_FILES.iter().enumerate().skip(1) {
            let mut e = FileEntry::root(n as u64);
            e.is_dir = n as u64 == ROOT_RECORD;
            if let Ok(exts) = self.extents(&e) {
                for x in exts {
                    if let Some(p) = x.physical
                        && !(n == 1 && p == mirr)
                    {
                        r.push(Region::meta(p, x.len, *name));
                    }
                }
            }
        }
        Ok(r)
    }

    fn describe_offset(&self, offset: u64) -> Option<String> {
        self.record_at(offset).map(|n| format!("MFT record {n}"))
    }

    fn deleted(&self, cancel: Option<&Cancel>) -> Result<Vec<FileEntry>> {
        let count = (self.mft_size / self.record_size).min(1 << 24);
        let mut out = Vec::new();
        let mut cache = HashMap::new();
        for n in 16..count {
            if n % 1024 == 0 {
                check_cancel(cancel)?;
            }
            let Ok(rec) = self.read_record(n) else {
                continue;
            };
            if le16(&rec, 22) & 1 != 0 || le64(&rec, 32) != 0 {
                continue;
            }
            let attrs = Self::parse_attrs(&rec, n);
            let Some(name) = Self::best_name(&attrs) else {
                continue;
            };
            let parent = self.path_of(name.parent, &mut cache);
            let mut e = self.entry(n, &rec, &attrs, Some(&name), &parent);
            e.deleted = true;
            out.push(e);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::testutil::{mem, put, put16, put32, put64, read_all};
    use crate::fs::{FsMap, open_filesystem};

    const CS: u64 = 512;
    const MFT_LCN: u64 = 16;

    fn align8(n: usize) -> usize {
        n.div_ceil(8) * 8
    }

    fn resident(kind: u32, name: &str, value: &[u8]) -> Vec<u8> {
        let name16: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let voff = align8(24 + name16.len());
        let len = align8(voff + value.len());
        let mut a = vec![0u8; len];
        put32(&mut a, 0, kind);
        put32(&mut a, 4, len as u32);
        a[9] = (name16.len() / 2) as u8;
        put16(&mut a, 10, 24);
        put(&mut a, 24, &name16);
        put32(&mut a, 16, value.len() as u32);
        put16(&mut a, 20, voff as u16);
        put(&mut a, voff, value);
        a
    }

    fn nonresident(kind: u32, name: &str, runs: &[u8], clusters: u64, real: u64) -> Vec<u8> {
        let name16: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let rl = align8(64 + name16.len());
        let len = align8(rl + runs.len() + 1);
        let mut a = vec![0u8; len];
        put32(&mut a, 0, kind);
        put32(&mut a, 4, len as u32);
        a[8] = 1;
        a[9] = (name16.len() / 2) as u8;
        put16(&mut a, 10, 64);
        put(&mut a, 64, &name16);
        put64(&mut a, 24, clusters.saturating_sub(1));
        put16(&mut a, 32, rl as u16);
        put64(&mut a, 40, clusters * CS);
        put64(&mut a, 48, real);
        put64(&mut a, 56, real);
        put(&mut a, rl, runs);
        a
    }

    fn file_name(parent: u64, name: &str, dir: bool, size: u64) -> Vec<u8> {
        let mut v = vec![0u8; 66];
        put64(&mut v, 0, parent | (1 << 48));
        put64(&mut v, 8, 132_000_000_000_000_000);
        put64(&mut v, 16, 132_000_000_000_000_000);
        put64(&mut v, 48, size);
        put32(&mut v, 56, if dir { 0x1000_0000 } else { 0x20 });
        let u: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
        v[64] = (u.len() / 2) as u8;
        v[65] = 1;
        v.extend(u);
        v
    }

    fn index_entry(record: u64, key: &[u8], flags: u16, subnode: Option<u64>) -> Vec<u8> {
        let mut len = align8(16 + key.len());
        if subnode.is_some() {
            len += 8;
        }
        let mut e = vec![0u8; len];
        put64(&mut e, 0, record | if record != 0 { 1 << 48 } else { 0 });
        put16(&mut e, 8, len as u16);
        put16(&mut e, 10, key.len() as u16);
        put16(&mut e, 12, flags);
        put(&mut e, 16, key);
        if let Some(vcn) = subnode {
            put64(&mut e, len - 8, vcn);
        }
        e
    }

    fn last_entry(subnode: Option<u64>) -> Vec<u8> {
        index_entry(0, &[], 2 | u16::from(subnode.is_some()), subnode)
    }

    fn index_root(entries: &[Vec<u8>], large: bool) -> Vec<u8> {
        let body: Vec<u8> = entries.concat();
        let mut v = vec![0u8; 32];
        put32(&mut v, 0, AT_FILE_NAME);
        put32(&mut v, 4, 1);
        put32(&mut v, 8, 1024);
        v[12] = 2;
        put32(&mut v, 16, 16);
        put32(&mut v, 20, (16 + body.len()) as u32);
        put32(&mut v, 24, (16 + body.len()) as u32);
        v[28] = u8::from(large);
        v.extend(body);
        v
    }

    fn fixup(buf: &mut [u8], usa: usize) {
        put16(buf, 4, usa as u16);
        let count = buf.len() / 512 + 1;
        put16(buf, 6, count as u16);
        put16(buf, usa, 0x0001);
        for i in 1..count {
            let pos = i * 512 - 2;
            buf[usa + i * 2] = buf[pos];
            buf[usa + i * 2 + 1] = buf[pos + 1];
            put16(buf, pos, 0x0001);
        }
    }

    fn record(d: &mut [u8], n: u64, flags: u16, attrs: &[Vec<u8>]) {
        let mut r = vec![0u8; 1024];
        put(&mut r, 0, b"FILE");
        put16(&mut r, 16, 1);
        put16(&mut r, 18, 1);
        put16(&mut r, 20, 0x38);
        put16(&mut r, 22, flags);
        let mut at = 0x38;
        for a in attrs {
            put(&mut r, at, a);
            at += a.len();
        }
        put32(&mut r, at, 0xFFFF_FFFF);
        put32(&mut r, 24, (at + 8) as u32);
        put32(&mut r, 28, 1024);
        put32(&mut r, 44, n as u32);
        fixup(&mut r, 0x30);
        let off = (MFT_LCN * CS + n * 1024) as usize;
        put(d, off, &r);
    }

    fn ntfs_image() -> Vec<u8> {
        let mut d = vec![0u8; 256 * 512];
        put(&mut d, 3, b"NTFS    ");
        put16(&mut d, 11, 512);
        d[13] = 1;
        put64(&mut d, 40, 256);
        put64(&mut d, 48, MFT_LCN);
        put64(&mut d, 56, 100);
        d[64] = 0xF6;
        d[68] = 0xF6;
        put64(&mut d, 72, 0x1122_3344_5566_7788);
        d[510] = 0x55;
        d[511] = 0xAA;

        record(
            &mut d,
            0,
            1,
            &[
                resident(AT_FILE_NAME, "", &file_name(5, "$MFT", false, 32 * 1024)),
                nonresident(AT_DATA, "", &[0x11, 64, MFT_LCN as u8], 64, 32 * 1024),
            ],
        );
        let label: Vec<u8> = "NTFSVOL"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        let mut volinfo = vec![0u8; 12];
        volinfo[8] = 3;
        volinfo[9] = 1;
        record(
            &mut d,
            3,
            1,
            &[
                resident(AT_VOLUME_NAME, "", &label),
                resident(AT_VOLUME_INFORMATION, "", &volinfo),
            ],
        );

        // Root: INDEX_ROOT → subnode VCN 0 in INDEX_ALLOCATION at LCN 120.
        record(
            &mut d,
            5,
            3,
            &[
                resident(AT_FILE_NAME, "", &file_name(5, ".", true, 0)),
                resident(
                    AT_INDEX_ROOT,
                    "$I30",
                    &index_root(&[last_entry(Some(0))], true),
                ),
                nonresident(AT_INDEX_ALLOCATION, "$I30", &[0x11, 2, 120], 2, 1024),
            ],
        );
        let mut indx = vec![0u8; 1024];
        put(&mut indx, 0, b"INDX");
        let entries = [
            index_entry(17, &file_name(5, "big.bin", false, 1800), 0, None),
            index_entry(18, &file_name(5, "Dir", true, 0), 0, None),
            index_entry(16, &file_name(5, "small.txt", false, 8), 0, None),
            index_entry(99, &file_name(5, "stale.txt", false, 1), 0, None),
            last_entry(None),
        ]
        .concat();
        put32(&mut indx, 0x18, 0x28);
        put32(&mut indx, 0x1C, (0x28 + entries.len()) as u32);
        put32(&mut indx, 0x20, 1000);
        put(&mut indx, 0x40, &entries);
        fixup(&mut indx, 0x28);
        put(&mut d, 120 * 512, &indx);

        record(
            &mut d,
            16,
            1,
            &[
                resident(AT_FILE_NAME, "", &file_name(5, "small.txt", false, 8)),
                resident(AT_DATA, "", b"hi there"),
            ],
        );
        // big.bin: 2 clusters at 130, a sparse cluster, 1 cluster at 140.
        record(
            &mut d,
            17,
            1,
            &[
                resident(AT_FILE_NAME, "", &file_name(5, "big.bin", false, 1800)),
                nonresident(
                    AT_DATA,
                    "",
                    &[0x21, 2, 130, 0, 0x01, 1, 0x11, 1, 10],
                    4,
                    1800,
                ),
            ],
        );
        record(
            &mut d,
            18,
            3,
            &[
                resident(AT_FILE_NAME, "", &file_name(5, "Dir", true, 0)),
                resident(
                    AT_INDEX_ROOT,
                    "$I30",
                    &index_root(
                        &[
                            index_entry(19, &file_name(18, "inner.txt", false, 5), 0, None),
                            last_entry(None),
                        ],
                        false,
                    ),
                ),
            ],
        );
        record(
            &mut d,
            19,
            1,
            &[
                resident(AT_FILE_NAME, "", &file_name(18, "inner.txt", false, 5)),
                resident(AT_DATA, "", b"inner"),
            ],
        );
        record(
            &mut d,
            20,
            0,
            &[
                resident(AT_FILE_NAME, "", &file_name(18, "old.doc", false, 3)),
                resident(AT_DATA, "", b"old"),
            ],
        );
        d[130 * 512..132 * 512].fill(b'a');
        d[140 * 512..141 * 512].fill(b'b');
        d
    }

    #[test]
    fn ntfs_listing_runs_resident_and_deleted() {
        let fs = open_filesystem(&mem(ntfs_image())).unwrap();
        let info = fs.info();
        assert_eq!(info.kind, FsKind::Ntfs);
        assert_eq!(info.label.as_deref(), Some("NTFSVOL"));
        assert!(info.fields.iter().any(|f| f.value == "3.1"));
        let root = fs.list_dir(&fs.root()).unwrap();
        let names: Vec<_> = root.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["big.bin", "Dir", "small.txt"]);
        assert_eq!(root[2].metadata_offset, Some(MFT_LCN * CS + 16 * 1024));
        assert_eq!(read_all(fs.as_ref(), &root[2]), b"hi there");
        let ex = fs.extents(&root[2]).unwrap();
        assert_eq!(ex.len(), 1);
        assert!(ex[0].physical.unwrap() > MFT_LCN * CS + 16 * 1024);

        let big = &root[0];
        assert_eq!(big.size, 1800);
        let ex = fs.extents(big).unwrap();
        assert_eq!(
            ex,
            vec![
                Extent {
                    logical: 0,
                    physical: Some(130 * 512),
                    len: 1024
                },
                Extent {
                    logical: 1024,
                    physical: None,
                    len: 512
                },
                Extent {
                    logical: 1536,
                    physical: Some(140 * 512),
                    len: 264
                },
            ]
        );
        let c = read_all(fs.as_ref(), big);
        assert!(c[..1024].iter().all(|&b| b == b'a'));
        assert!(c[1024..1536].iter().all(|&b| b == 0));
        assert!(c[1536..].iter().all(|&b| b == b'b'));

        let inner = fs.lookup("/Dir/inner.txt").unwrap().unwrap();
        assert_eq!(read_all(fs.as_ref(), &inner), b"inner");

        let del = fs.deleted(None).unwrap();
        assert_eq!(del.len(), 1);
        assert_eq!(del[0].path, "/Dir/old.doc");
        assert_eq!(read_all(fs.as_ref(), &del[0]), b"old");

        assert_eq!(
            fs.describe_offset(MFT_LCN * CS + 17 * 1024 + 7),
            Some("MFT record 17".into())
        );
        let map = FsMap::build(fs.as_ref(), None).unwrap();
        assert_eq!(map.owner_at(140 * 512).unwrap().label, "/big.bin");
        assert_eq!(map.owner_at(MFT_LCN * CS + 3).unwrap().label, "$MFT");
        assert_eq!(map.owner_at(120 * 512 + 5).unwrap().label, "/");
    }
}
