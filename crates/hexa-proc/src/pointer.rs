//! Pointer scan (PROC-07).
//!
//! Finds chains `module+offset → [+o1] → [+o2] … → target` that survive
//! restarts because they start at a static address inside a loaded module.
//!
//! 1. [`PointerMap::build`] reads the selected regions once and records every
//!    aligned pointer-sized value that points into mapped memory, sorted by
//!    value — a *reverse pointer map*: "who points near X?" is a binary
//!    search.
//! 2. [`PointerMap::find_chains`] walks backwards from the target: every
//!    pointer whose value lies in `[x - max_offset, x]` is a candidate; if it
//!    sits inside a module it ends a chain, otherwise the walk continues from
//!    its address until `max_depth` dereferences.
//! 3. After the target moved (new allocation, restarted process),
//!    [`rescan`] keeps the chains that still resolve to the new address.

use std::fmt;

use hexa_core::Cancel;
use hexa_core::task::Progress;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::region::{MemoryRegion, Module, RegionFilter, find_module, modules};
use crate::source::MemorySource;

/// Bytes read per step while building the pointer map.
const BLOCK: u64 = 1 << 20;

/// Deepest supported chain.
pub const MAX_DEPTH: u8 = 5;

/// Pointer scan settings (PROC-07).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PointerScanOptions {
    /// Maximum number of dereferences (1..=5).
    pub max_depth: u8,
    /// Largest offset added after a dereference.
    pub max_offset: u64,
    /// Alignment of pointer locations; `None` = pointer size.
    pub alignment: Option<u32>,
    /// Stop after this many chains.
    pub max_results: usize,
    /// Regions searched for pointers.
    pub regions: RegionFilter,
}

impl Default for PointerScanOptions {
    fn default() -> Self {
        Self {
            max_depth: 3,
            max_offset: 0x1000,
            alignment: None,
            max_results: 10_000,
            regions: RegionFilter::default(),
        }
    }
}

/// A pointer path from a static module address to a target (PROC-07).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PointerChain {
    /// Module file name (or full path).
    pub module: String,
    /// Offset of the first pointer from the module base.
    pub module_offset: u64,
    /// Offsets added after each dereference; the last yields the target.
    pub offsets: Vec<u64>,
}

impl fmt::Display for PointerChain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}+{:#x}]", self.module, self.module_offset)?;
        let n = self.offsets.len();
        for (i, o) in self.offsets.iter().enumerate() {
            if i + 1 < n {
                write!(f, " → [+{o:#x}]")?;
            } else {
                write!(f, " → +{o:#x}")?;
            }
        }
        Ok(())
    }
}

fn read_ptr(source: &dyn MemorySource, address: u64, size: u8) -> Result<u64> {
    let mut b = [0u8; 8];
    source.read(address, &mut b[..usize::from(size)])?;
    Ok(u64::from_le_bytes(b))
}

/// Follows `chain` in `source`; returns the final address.
pub fn resolve_chain(
    source: &dyn MemorySource,
    chain: &PointerChain,
    modules: &[Module],
) -> Result<u64> {
    let module = find_module(modules, &chain.module)
        .ok_or_else(|| Error::invalid(format!("module {} is not loaded", chain.module)))?;
    let size = source.pointer_size();
    let mut location = module.base.wrapping_add(chain.module_offset);
    for &off in &chain.offsets {
        location = read_ptr(source, location, size)?.wrapping_add(off);
    }
    Ok(location)
}

/// Keeps the chains that now resolve to `new_target` (PROC-07 rescan).
pub fn rescan(
    source: &dyn MemorySource,
    chains: &[PointerChain],
    new_target: u64,
    cancel: &Cancel,
) -> Result<Vec<PointerChain>> {
    let modules = modules(&source.regions()?);
    let mut out = Vec::new();
    for (i, c) in chains.iter().enumerate() {
        if i % 1024 == 0 {
            cancel.check()?;
        }
        if resolve_chain(source, c, &modules).is_ok_and(|a| a == new_target) {
            out.push(c.clone());
        }
    }
    Ok(out)
}

/// Reverse pointer map of a target (PROC-07).
#[derive(Debug, Clone)]
pub struct PointerMap {
    pointer_size: u8,
    /// `(pointer value, location)`, sorted by value then location.
    entries: Vec<(u64, u64)>,
    modules: Vec<Module>,
}

/// Index of the region containing `address` in sorted `regions`.
fn mapped(regions: &[MemoryRegion], address: u64) -> bool {
    let i = regions.partition_point(|r| r.start <= address);
    i > 0 && regions[i - 1].contains(address)
}

impl PointerMap {
    /// Reads the selected regions and indexes every value that points into
    /// mapped memory.
    pub fn build(
        source: &dyn MemorySource,
        options: &PointerScanOptions,
        cancel: &Cancel,
        progress: Progress<'_>,
    ) -> Result<Self> {
        let regions = source.regions()?;
        let pointer_size = source.pointer_size();
        let size = u64::from(pointer_size);
        if size != 4 && size != 8 {
            return Err(Error::invalid(format!("unsupported pointer size {size}")));
        }
        let align = u64::from(options.alignment.unwrap_or(u32::from(pointer_size))).max(1);
        let ranges = options.regions.select(&regions);
        let total: u64 = ranges.iter().map(|r| r.end - r.start).sum();
        let mut done = 0u64;
        let mut entries = Vec::new();
        let mut buf = Vec::new();
        progress(0, total);
        for r in &ranges {
            let mut s = r.start;
            while s < r.end {
                cancel.check()?;
                let end = (s + BLOCK).min(r.end);
                buf.resize((end - s) as usize, 0);
                for v in source.read_partial(s, &mut buf) {
                    let mut p = v.start.div_ceil(align) * align;
                    while p + size <= v.end {
                        let off = (p - s) as usize;
                        let mut b = [0u8; 8];
                        b[..size as usize].copy_from_slice(&buf[off..off + size as usize]);
                        let value = u64::from_le_bytes(b);
                        if value != 0 && mapped(&regions, value) {
                            entries.push((value, p));
                        }
                        p += align;
                    }
                }
                done += end - s;
                progress(done, total);
                s = end;
            }
        }
        entries.sort_unstable();
        Ok(Self {
            pointer_size,
            entries,
            modules: modules(&regions),
        })
    }

    /// Number of pointers indexed.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn pointer_size(&self) -> u8 {
        self.pointer_size
    }

    pub fn modules(&self) -> &[Module] {
        &self.modules
    }

    /// Pointers whose value lies in `[lo, hi]`.
    pub fn pointers_to(&self, lo: u64, hi: u64) -> &[(u64, u64)] {
        let a = self.entries.partition_point(|e| e.0 < lo);
        let b = self.entries.partition_point(|e| e.0 <= hi);
        &self.entries[a..b.max(a)]
    }

    fn static_module(&self, address: u64) -> Option<&Module> {
        let i = self.modules.partition_point(|m| m.base <= address);
        (i > 0 && self.modules[i - 1].contains(address)).then(|| &self.modules[i - 1])
    }

    /// All chains of at most `max_depth` dereferences ending at `target`,
    /// shortest first.
    pub fn find_chains(
        &self,
        target: u64,
        options: &PointerScanOptions,
        cancel: &Cancel,
    ) -> Result<Vec<PointerChain>> {
        if options.max_depth == 0 || options.max_depth > MAX_DEPTH {
            return Err(Error::invalid(format!("max depth must be 1..={MAX_DEPTH}")));
        }
        let mut walk = Walk {
            map: self,
            options,
            cancel,
            out: Vec::new(),
            offsets: Vec::new(),
            path: Vec::new(),
            visits: 0,
        };
        // Iterative deepening gives shortest chains first and avoids
        // exploring deep paths when max_results is reached early.
        for depth in 1..=options.max_depth {
            walk.search(target, depth)?;
            if walk.out.len() >= options.max_results {
                break;
            }
        }
        let mut out = walk.out;
        out.dedup();
        out.truncate(options.max_results);
        Ok(out)
    }
}

struct Walk<'a> {
    map: &'a PointerMap,
    options: &'a PointerScanOptions,
    cancel: &'a Cancel,
    out: Vec<PointerChain>,
    /// Offsets from the target backwards.
    offsets: Vec<u64>,
    /// Locations on the current path (cycle guard).
    path: Vec<u64>,
    visits: u64,
}

impl Walk<'_> {
    /// Finds chains with exactly `left` more dereferences ending at `target`.
    fn search(&mut self, target: u64, left: u8) -> Result<()> {
        self.visits += 1;
        if self.visits.is_multiple_of(4096) {
            self.cancel.check()?;
        }
        let lo = target.saturating_sub(self.options.max_offset);
        for &(value, location) in self.map.pointers_to(lo, target) {
            if self.out.len() >= self.options.max_results {
                return Ok(());
            }
            if self.path.contains(&location) {
                continue;
            }
            self.offsets.push(target - value);
            if left == 1 {
                if let Some(m) = self.map.static_module(location) {
                    self.out.push(PointerChain {
                        module: m.name.clone(),
                        module_offset: location - m.base,
                        offsets: self.offsets.iter().rev().copied().collect(),
                    });
                }
            } else if self.map.static_module(location).is_none() {
                self.path.push(location);
                self.search(location, left - 1)?;
                self.path.pop();
            }
            self.offsets.pop();
        }
        Ok(())
    }
}

/// Builds the pointer map and finds chains to `target` in one go.
pub fn pointer_scan(
    source: &dyn MemorySource,
    target: u64,
    options: &PointerScanOptions,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<Vec<PointerChain>> {
    PointerMap::build(source, options, cancel, progress)?.find_chains(target, options, cancel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::region::{Protection, RegionKind};
    use crate::source::MemorySnapshot;
    use hexa_core::task::no_progress;

    const MODULE: u64 = 0x40_0000;
    const HEAP: u64 = 0x1000_0000;

    fn image(start: u64, size: usize) -> MemoryRegion {
        MemoryRegion {
            kind: RegionKind::Image,
            path: Some("/opt/game/game.bin".into()),
            ..MemoryRegion::new(start, size as u64, Protection::RW)
        }
    }

    /// game+0x100 → A, [A+0x18] → B, B+0x30 = target.
    fn world(a: u64, b: u64) -> (MemorySnapshot, u64) {
        let mut s = MemorySnapshot::new("w", 8);
        s.add_region(
            MemoryRegion {
                protection: Protection::RX,
                ..image(MODULE - 0x1000, 0x1000)
            },
            vec![0; 0x1000],
        )
        .unwrap();
        s.add_region(image(MODULE, 0x1000), vec![0; 0x1000])
            .unwrap();
        s.add_region(MemoryRegion::new(HEAP, 0, Protection::RW), vec![0; 0x10000])
            .unwrap();
        s.write(MODULE + 0x100, &a.to_le_bytes()).unwrap();
        s.write(a + 0x18, &b.to_le_bytes()).unwrap();
        // A decoy pointer near the target that is not reachable statically.
        s.write(HEAP + 0xff00, &(b + 0x10).to_le_bytes()).unwrap();
        (s, b + 0x30)
    }

    #[test]
    fn finds_and_rescans_chain() {
        let (s, target) = world(HEAP + 0x100, HEAP + 0x2000);
        let opts = PointerScanOptions {
            max_offset: 0x100,
            ..PointerScanOptions::default()
        };
        let map = PointerMap::build(&s, &opts, &Cancel::new(), &no_progress).unwrap();
        assert_eq!(map.len(), 3);
        assert_eq!(map.modules().len(), 1);
        assert_eq!(map.modules()[0].base, MODULE - 0x1000);
        let chains = map.find_chains(target, &opts, &Cancel::new()).unwrap();
        let expected = PointerChain {
            module: "game.bin".into(),
            module_offset: 0x1100,
            offsets: vec![0x18, 0x30],
        };
        assert_eq!(chains, vec![expected.clone()]);
        assert_eq!(expected.to_string(), "[game.bin+0x1100] → [+0x18] → +0x30");
        let mods = modules(&s.regions().unwrap());
        assert_eq!(resolve_chain(&s, &expected, &mods).unwrap(), target);

        // The target moved: the chain still resolves, a stale one does not.
        let (moved, new_target) = world(HEAP + 0x5000, HEAP + 0x8000);
        let stale = PointerChain {
            offsets: vec![0x18, 0x40],
            ..expected.clone()
        };
        let kept = rescan(
            &moved,
            &[expected.clone(), stale],
            new_target,
            &Cancel::new(),
        )
        .unwrap();
        assert_eq!(kept, vec![expected]);
    }

    #[test]
    fn depth_limits_and_validation() {
        let (s, target) = world(HEAP + 0x100, HEAP + 0x2000);
        let opts = PointerScanOptions {
            max_depth: 1,
            ..PointerScanOptions::default()
        };
        let chains = pointer_scan(&s, target, &opts, &Cancel::new(), &no_progress).unwrap();
        assert_eq!(chains.len(), 0);
        let direct = pointer_scan(&s, HEAP + 0x120, &opts, &Cancel::new(), &no_progress).unwrap();
        assert_eq!(direct.len(), 1);
        assert_eq!(direct[0].offsets, vec![0x20]);
        let bad = PointerScanOptions {
            max_depth: 6,
            ..PointerScanOptions::default()
        };
        assert!(pointer_scan(&s, target, &bad, &Cancel::new(), &no_progress).is_err());
    }
}
