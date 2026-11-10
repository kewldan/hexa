//! Memory regions and modules (PROC-02, PROC-03).

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::ops::Range;

use serde::{Deserialize, Serialize};

/// Page protection of a region.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Protection {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}

impl Protection {
    pub const NONE: Self = Self {
        read: false,
        write: false,
        execute: false,
    };
    pub const R: Self = Self {
        read: true,
        write: false,
        execute: false,
    };
    pub const RW: Self = Self {
        read: true,
        write: true,
        execute: false,
    };
    pub const RX: Self = Self {
        read: true,
        write: false,
        execute: true,
    };

    /// Parses the first three characters of an `rwx`-style string.
    pub fn from_rwx(s: &str) -> Self {
        let b = s.as_bytes();
        Self {
            read: b.first() == Some(&b'r'),
            write: b.get(1) == Some(&b'w'),
            execute: b.get(2) == Some(&b'x'),
        }
    }
}

impl fmt::Display for Protection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let c = |on: bool, ch: char| if on { ch } else { '-' };
        write!(
            f,
            "{}{}{}",
            c(self.read, 'r'),
            c(self.write, 'w'),
            c(self.execute, 'x')
        )
    }
}

/// What backs a region, as far as the OS tells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RegionKind {
    /// Executable image (main binary, shared library, DLL).
    Image,
    /// File mapping or shared memory that is not an image.
    Mapped,
    /// Anonymous private memory.
    Private,
    Stack,
    Heap,
    Unknown,
}

/// One contiguous range of the target's address space (PROC-02).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryRegion {
    pub start: u64,
    pub size: u64,
    pub protection: Protection,
    pub kind: RegionKind,
    /// Shared with other processes (shared mappings, shared memory).
    pub shared: bool,
    /// Backing file (module path for images).
    pub path: Option<String>,
    /// Pseudo name when there is no file (`[heap]`, `[vdso]`, `[anon:…]`).
    pub label: Option<String>,
}

impl MemoryRegion {
    /// A private anonymous region with the given protection.
    pub fn new(start: u64, size: u64, protection: Protection) -> Self {
        Self {
            start,
            size,
            protection,
            kind: RegionKind::Private,
            shared: false,
            path: None,
            label: None,
        }
    }

    /// Exclusive end address (saturating).
    pub fn end(&self) -> u64 {
        self.start.saturating_add(self.size)
    }

    pub fn range(&self) -> Range<u64> {
        self.start..self.end()
    }

    pub fn contains(&self, address: u64) -> bool {
        address >= self.start && address < self.end()
    }

    /// File name of the backing file.
    pub fn file_name(&self) -> Option<&str> {
        self.path
            .as_deref()
            .map(|p| p.rsplit(['/', '\\']).next().unwrap_or(p))
    }
}

/// Region selection used by scans and dumps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RegionFilter {
    /// Only regions that are writable (the default for value scans).
    pub writable_only: bool,
    /// Also include executable regions.
    pub include_executable: bool,
    /// Include file mappings that are not images.
    pub include_mapped: bool,
    /// Restrict to `[start, end)`.
    pub address_range: Option<(u64, u64)>,
}

impl Default for RegionFilter {
    fn default() -> Self {
        Self {
            writable_only: true,
            include_executable: true,
            include_mapped: true,
            address_range: None,
        }
    }
}

impl RegionFilter {
    /// Every readable region.
    pub fn all() -> Self {
        Self {
            writable_only: false,
            ..Self::default()
        }
    }

    pub fn matches(&self, r: &MemoryRegion) -> bool {
        if !r.protection.read
            || (self.writable_only && !r.protection.write)
            || (!self.include_executable && r.protection.execute)
            || (!self.include_mapped && r.kind == RegionKind::Mapped)
        {
            return false;
        }
        match self.address_range {
            Some((s, e)) => r.start < e && r.end() > s,
            None => true,
        }
    }

    /// The parts of `regions` selected by this filter, clipped to
    /// `address_range`.
    pub fn select(&self, regions: &[MemoryRegion]) -> Vec<Range<u64>> {
        regions
            .iter()
            .filter(|r| self.matches(r))
            .map(|r| match self.address_range {
                Some((s, e)) => r.start.max(s)..r.end().min(e),
                None => r.range(),
            })
            .filter(|r| !r.is_empty())
            .collect()
    }
}

/// A loaded module: an executable image and all its sections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Module {
    pub name: String,
    pub path: String,
    /// Lowest address of any of its regions.
    pub base: u64,
    /// `end of last region - base`.
    pub size: u64,
}

impl Module {
    pub fn end(&self) -> u64 {
        self.base.saturating_add(self.size)
    }

    pub fn contains(&self, address: u64) -> bool {
        address >= self.base && address < self.end()
    }
}

/// Groups image regions by path into modules, sorted by base address.
pub fn modules(regions: &[MemoryRegion]) -> Vec<Module> {
    let mut by_path: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
    for r in regions.iter().filter(|r| r.kind == RegionKind::Image) {
        let Some(path) = r.path.as_deref() else {
            continue;
        };
        let e = by_path.entry(path).or_insert((r.start, r.end()));
        e.0 = e.0.min(r.start);
        e.1 = e.1.max(r.end());
    }
    let mut out: Vec<Module> = by_path
        .into_iter()
        .map(|(path, (base, end))| Module {
            name: path.rsplit(['/', '\\']).next().unwrap_or(path).to_string(),
            path: path.to_string(),
            base,
            size: end - base,
        })
        .collect();
    out.sort_by_key(|m| m.base);
    out
}

/// Finds a module by file name or full path (case-insensitive file name).
pub fn find_module<'a>(modules: &'a [Module], name: &str) -> Option<&'a Module> {
    modules
        .iter()
        .find(|m| m.path == name)
        .or_else(|| modules.iter().find(|m| m.name.eq_ignore_ascii_case(name)))
}

/// Marks file-backed regions as [`RegionKind::Image`] when any mapping of
/// the same file is executable (shared libraries, the main binary); other
/// file mappings become [`RegionKind::Mapped`].
pub fn classify_images(regions: &mut [MemoryRegion]) {
    let exec: HashSet<String> = regions
        .iter()
        .filter(|r| r.protection.execute)
        .filter_map(|r| r.path.clone())
        .collect();
    for r in regions.iter_mut() {
        if let Some(p) = &r.path {
            if exec.contains(p) {
                r.kind = RegionKind::Image;
            } else if r.kind == RegionKind::Private || r.kind == RegionKind::Unknown {
                r.kind = RegionKind::Mapped;
            }
        }
    }
}

/// Parses Linux `/proc/<pid>/maps` (PROC-02).
pub fn parse_proc_maps(text: &str) -> Vec<MemoryRegion> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut it = line.split_ascii_whitespace();
        let (Some(range), Some(perms), Some(_off), Some(_dev), Some(inode)) =
            (it.next(), it.next(), it.next(), it.next(), it.next())
        else {
            continue;
        };
        let rest: Vec<&str> = it.collect();
        let name = rest.join(" ");
        let Some((s, e)) = range.split_once('-') else {
            continue;
        };
        let (Ok(start), Ok(end)) = (u64::from_str_radix(s, 16), u64::from_str_radix(e, 16)) else {
            continue;
        };
        if end <= start {
            continue;
        }
        let shared = perms.as_bytes().get(3) == Some(&b's');
        let mut region = MemoryRegion {
            start,
            size: end - start,
            protection: Protection::from_rwx(perms),
            kind: RegionKind::Private,
            shared,
            path: None,
            label: None,
        };
        let name = name.trim();
        if name.starts_with('[') {
            region.label = Some(name.to_string());
            region.kind = match name {
                "[heap]" => RegionKind::Heap,
                n if n.starts_with("[stack") => RegionKind::Stack,
                "[vdso]" | "[vsyscall]" => RegionKind::Image,
                "[vvar]" | "[vvar_vclock]" => RegionKind::Mapped,
                n if n.starts_with("[anon:") && n.contains("heap") => RegionKind::Heap,
                n if n.starts_with("[anon:") && n.contains("stack") => RegionKind::Stack,
                _ => RegionKind::Private,
            };
        } else if !name.is_empty() && inode != "0" {
            region.path = Some(name.trim_end_matches(" (deleted)").to_string());
            region.kind = RegionKind::Mapped;
        } else if !name.is_empty() {
            // `/dev/zero (deleted)`, `/SYSV00000000 (deleted)`, memfd: …
            region.label = Some(name.to_string());
            region.kind = if shared {
                RegionKind::Mapped
            } else {
                RegionKind::Private
            };
        } else if shared {
            region.kind = RegionKind::Mapped;
        }
        out.push(region);
    }
    classify_images(&mut out);
    out
}

/// One span of the provider's address space: either a mapped region or an
/// unmapped gap the UI folds as a "data fold" (PROC-03).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddressSpan {
    pub start: u64,
    pub end: u64,
    pub mapped: bool,
    /// Index into the region list for mapped spans.
    pub region: Option<usize>,
}

/// Splits `[0, limit)` into mapped spans and unmapped gaps (PROC-03).
/// `regions` must be sorted and non-overlapping.
pub fn address_spans(regions: &[MemoryRegion], limit: u64) -> Vec<AddressSpan> {
    let mut out = Vec::with_capacity(regions.len() * 2 + 1);
    let mut pos = 0u64;
    for (i, r) in regions.iter().enumerate() {
        if r.start >= limit {
            break;
        }
        if r.start > pos {
            out.push(AddressSpan {
                start: pos,
                end: r.start,
                mapped: false,
                region: None,
            });
        }
        let end = r.end().min(limit);
        if end > r.start.max(pos) {
            out.push(AddressSpan {
                start: r.start.max(pos),
                end,
                mapped: true,
                region: Some(i),
            });
        }
        pos = pos.max(end);
    }
    if pos < limit {
        out.push(AddressSpan {
            start: pos,
            end: limit,
            mapped: false,
            region: None,
        });
    }
    out
}

/// Unmapped gaps of `[0, limit)` (PROC-03).
pub fn gaps(regions: &[MemoryRegion], limit: u64) -> Vec<Range<u64>> {
    address_spans(regions, limit)
        .into_iter()
        .filter(|s| !s.mapped)
        .map(|s| s.start..s.end)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAPS: &str = "\
55d0c0a00000-55d0c0a02000 r--p 00000000 08:01 1234  /usr/bin/cat
55d0c0a02000-55d0c0a06000 r-xp 00002000 08:01 1234  /usr/bin/cat
55d0c0a08000-55d0c0a09000 rw-p 00007000 08:01 1234  /usr/bin/cat
55d0c1000000-55d0c1021000 rw-p 00000000 00:00 0     [heap]
7f0000000000-7f0000100000 r--p 00000000 08:01 999   /usr/lib/locale/locale-archive
7f0000200000-7f0000201000 rw-s 00000000 00:05 77    /SYSV00000000 (deleted)
7f0000300000-7f0000310000 rw-p 00000000 00:00 0
7ffd00000000-7ffd00021000 rw-p 00000000 00:00 0     [stack]
ffffffffff600000-ffffffffff601000 --xp 00000000 00:00 0  [vsyscall]
";

    #[test]
    fn parses_maps() {
        let r = parse_proc_maps(MAPS);
        assert_eq!(r.len(), 9);
        assert_eq!(r[0].kind, RegionKind::Image);
        assert_eq!(r[1].protection, Protection::RX);
        assert_eq!(r[2].kind, RegionKind::Image);
        assert_eq!(r[3].kind, RegionKind::Heap);
        assert_eq!(r[3].label.as_deref(), Some("[heap]"));
        assert_eq!(r[4].kind, RegionKind::Mapped);
        assert!(r[5].shared);
        assert_eq!(r[5].kind, RegionKind::Mapped);
        assert_eq!(r[6].kind, RegionKind::Private);
        assert_eq!(r[7].kind, RegionKind::Stack);
        assert_eq!(r[0].protection.to_string(), "r--");

        let m = modules(&r);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].name, "cat");
        assert_eq!(m[0].base, 0x55d0_c0a0_0000);
        assert_eq!(m[0].size, 0x9000);
        assert!(find_module(&m, "CAT").is_some());
    }

    #[test]
    fn spans_and_gaps() {
        let regions = vec![
            MemoryRegion::new(0x1000, 0x1000, Protection::RW),
            MemoryRegion::new(0x3000, 0x2000, Protection::R),
        ];
        let spans = address_spans(&regions, 0x10000);
        assert_eq!(spans.len(), 5);
        assert_eq!(
            gaps(&regions, 0x10000),
            vec![0..0x1000, 0x2000..0x3000, 0x5000..0x10000]
        );
        assert_eq!(spans[1].region, Some(0));
    }

    #[test]
    fn filter_selects() {
        let regions = vec![
            MemoryRegion::new(0x1000, 0x1000, Protection::RW),
            MemoryRegion::new(0x3000, 0x2000, Protection::R),
        ];
        assert_eq!(
            RegionFilter::default().select(&regions),
            vec![0x1000..0x2000]
        );
        let f = RegionFilter {
            address_range: Some((0x3800, 0x4000)),
            ..RegionFilter::all()
        };
        assert_eq!(f.select(&regions), vec![0x3800..0x4000]);
    }
}
