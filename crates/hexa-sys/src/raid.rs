//! RAID reconstruction (DISK-11): RAID 0 and RAID 5 (left/right ×
//! symmetric/asymmetric) over member providers in a chosen order, with one
//! missing RAID 5 member rebuilt from parity.

use std::io;
use std::sync::Arc;

use hexa_core::Result;
use hexa_core::provider::{Capabilities, Provider};
use serde::{Deserialize, Serialize};

use crate::util::{check_range, invalid};

/// RAID 5 parity rotation (Linux md naming).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Raid5Layout {
    LeftAsymmetric,
    LeftSymmetric,
    RightAsymmetric,
    RightSymmetric,
}

/// RAID level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "level")]
pub enum RaidLevel {
    Raid0,
    Raid5 { layout: Raid5Layout },
}

/// Array geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RaidConfig {
    pub level: RaidLevel,
    /// Chunk (stripe unit) size in bytes.
    pub stripe_size: u64,
    /// Where array data starts on every member (e.g. after md metadata).
    pub data_offset: u64,
}

/// A virtual disk assembled from member providers (in array order).
#[derive(Debug)]
pub struct RaidProvider {
    members: Vec<Option<Arc<dyn Provider>>>,
    config: RaidConfig,
    member_len: u64,
    len: u64,
}

impl RaidProvider {
    /// `members[i] = None` marks a missing disk (RAID 5 only, at most one).
    /// Usable bytes per member (whole stripes after `data_offset`).
    pub fn member_size(&self) -> u64 {
        self.member_len
    }

    pub fn new(members: Vec<Option<Arc<dyn Provider>>>, config: RaidConfig) -> Result<Self> {
        let n = members.len() as u64;
        let missing = members.iter().filter(|m| m.is_none()).count();
        if config.stripe_size == 0 {
            return Err(invalid("stripe size must be positive"));
        }
        match config.level {
            RaidLevel::Raid0 if n < 1 || missing > 0 => {
                return Err(invalid("RAID 0 needs all members"));
            }
            RaidLevel::Raid5 { .. } if n < 3 || missing > 1 => {
                return Err(invalid(
                    "RAID 5 needs at least 3 members with at most one missing",
                ));
            }
            _ => {}
        }
        let min = members
            .iter()
            .flatten()
            .map(|m| m.len().saturating_sub(config.data_offset))
            .min()
            .unwrap_or(0);
        let member_len = min / config.stripe_size * config.stripe_size;
        let data_disks = match config.level {
            RaidLevel::Raid0 => n,
            RaidLevel::Raid5 { .. } => n - 1,
        };
        Ok(Self {
            members,
            config,
            member_len,
            len: member_len * data_disks,
        })
    }

    /// `(member, offset on member)` of array byte `pos`, and the parity
    /// member for RAID 5.
    fn locate(&self, pos: u64) -> (usize, u64, Option<usize>) {
        let n = self.members.len() as u64;
        let ss = self.config.stripe_size;
        let chunk = pos / ss;
        let within = pos % ss;
        match self.config.level {
            RaidLevel::Raid0 => {
                let disk = chunk % n;
                let row = chunk / n;
                (
                    disk as usize,
                    self.config.data_offset + row * ss + within,
                    None,
                )
            }
            RaidLevel::Raid5 { layout } => {
                let data = n - 1;
                let row = chunk / data;
                let d = chunk % data;
                let parity = match layout {
                    Raid5Layout::LeftAsymmetric | Raid5Layout::LeftSymmetric => n - 1 - row % n,
                    Raid5Layout::RightAsymmetric | Raid5Layout::RightSymmetric => row % n,
                };
                let disk =
                    match layout {
                        Raid5Layout::LeftAsymmetric | Raid5Layout::RightAsymmetric => {
                            if d < parity { d } else { d + 1 }
                        }
                        Raid5Layout::LeftSymmetric | Raid5Layout::RightSymmetric => {
                            (parity + 1 + d) % n
                        }
                    };
                (
                    disk as usize,
                    self.config.data_offset + row * ss + within,
                    Some(parity as usize),
                )
            }
        }
    }

    /// Rebuilds bytes of a missing member by XOR of all the others.
    fn rebuild(&self, missing: usize, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        buf.fill(0);
        let mut tmp = vec![0u8; buf.len()];
        for (i, m) in self.members.iter().enumerate() {
            if i == missing {
                continue;
            }
            let m = m
                .as_ref()
                .ok_or_else(|| io::Error::other("two RAID members are missing"))?;
            m.read_at(offset, &mut tmp)?;
            for (b, t) in buf.iter_mut().zip(&tmp) {
                *b ^= t;
            }
        }
        Ok(())
    }
}

impl Provider for RaidProvider {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        check_range(offset, buf.len(), self.len)?;
        let ss = self.config.stripe_size;
        let mut done = 0usize;
        while done < buf.len() {
            let pos = offset + done as u64;
            let n = ((ss - pos % ss) as usize).min(buf.len() - done);
            let (disk, at, _) = self.locate(pos);
            let dst = &mut buf[done..done + n];
            match &self.members[disk] {
                Some(m) => m.read_at(at, dst)?,
                None => self.rebuild(disk, at, dst)?,
            }
            done += n;
        }
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::READ_ONLY
    }

    fn name(&self) -> String {
        let level = match self.config.level {
            RaidLevel::Raid0 => "RAID 0".to_string(),
            RaidLevel::Raid5 { layout } => format!("RAID 5 ({layout:?})"),
        };
        format!("{level}, {} disks", self.members.len())
    }

    fn invalidate(&self) {
        for m in self.members.iter().flatten() {
            m.invalidate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hexa_core::provider::MemoryProvider;

    const SS: usize = 16;

    /// Lays `data` out on `n` members the way md would.
    fn build(data: &[u8], n: usize, level: RaidLevel) -> Vec<Vec<u8>> {
        let data_disks = if level == RaidLevel::Raid0 { n } else { n - 1 };
        let rows = data.len() / SS / data_disks;
        let mut disks = vec![vec![0u8; rows * SS]; n];
        let probe = RaidProvider {
            members: vec![None; n],
            config: RaidConfig {
                level,
                stripe_size: SS as u64,
                data_offset: 0,
            },
            member_len: (rows * SS) as u64,
            len: data.len() as u64,
        };
        for (c, chunk) in data.chunks(SS).enumerate() {
            let (disk, at, parity) = probe.locate((c * SS) as u64);
            disks[disk][at as usize..at as usize + SS].copy_from_slice(chunk);
            if let Some(p) = parity {
                for i in 0..SS {
                    disks[p][at as usize + i] ^= chunk[i];
                }
            }
        }
        disks
    }

    fn members(disks: &[Vec<u8>], drop: Option<usize>) -> Vec<Option<Arc<dyn Provider>>> {
        disks
            .iter()
            .enumerate()
            .map(|(i, d)| {
                (Some(i) != drop)
                    .then(|| Arc::new(MemoryProvider::new("m", d.clone())) as Arc<dyn Provider>)
            })
            .collect()
    }

    #[test]
    fn raid0_and_all_raid5_layouts() {
        let data: Vec<u8> = (0..16 * 4 * 6).map(|i| (i * 7 + 3) as u8).collect();
        let r0 = build(&data, 4, RaidLevel::Raid0);
        let cfg = |level| RaidConfig {
            level,
            stripe_size: SS as u64,
            data_offset: 0,
        };
        let p = RaidProvider::new(members(&r0, None), cfg(RaidLevel::Raid0)).unwrap();
        let mut out = vec![0u8; data.len()];
        p.read_at(0, &mut out).unwrap();
        assert_eq!(out, data);
        assert_eq!(&r0[1][..SS], &data[SS..2 * SS]);

        for layout in [
            Raid5Layout::LeftAsymmetric,
            Raid5Layout::LeftSymmetric,
            Raid5Layout::RightAsymmetric,
            Raid5Layout::RightSymmetric,
        ] {
            let level = RaidLevel::Raid5 { layout };
            let data = &data[..16 * 3 * 8];
            let disks = build(data, 4, level);
            for drop in [None, Some(0), Some(2)] {
                let p = RaidProvider::new(members(&disks, drop), cfg(level)).unwrap();
                assert_eq!(p.len(), data.len() as u64);
                let mut out = vec![0u8; data.len() - 5];
                p.read_at(5, &mut out).unwrap();
                assert_eq!(out, data[5..], "{layout:?} missing {drop:?}");
            }
        }
        // Left-symmetric: row 0 parity on the last disk, data on 0,1,2;
        // row 1 parity on disk 2, data starts at disk 3.
        let ls = build(
            &data[..16 * 3 * 2],
            4,
            RaidLevel::Raid5 {
                layout: Raid5Layout::LeftSymmetric,
            },
        );
        assert_eq!(&ls[3][SS..2 * SS], &data[3 * SS..4 * SS]);
        assert!(RaidProvider::new(members(&r0, Some(1)), cfg(RaidLevel::Raid0)).is_err());
    }
}
