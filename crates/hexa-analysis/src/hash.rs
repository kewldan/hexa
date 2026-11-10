//! Fast non-cryptographic hashes (ANL-03) and a dispatcher that accepts the
//! ids of this crate and of [`hexa_core::hash`] alike.
//!
//! Ids follow the style of `hexa_core::hash` so the app can merge the lists:
//! [`algorithms`] returns this crate's entries (groups `fast` and `fuzzy`),
//! [`hash_range`] / [`hash_bytes`] take any mix of ids and return results in
//! the requested order. All seeds are 0. Values are lower-case hex of the
//! canonical big-endian integer, except `MurmurHash3` `x64_128` which is printed
//! as the 16 output bytes (`h1` then `h2`, little-endian, like `mmh3`).

use hexa_core::hash::CrcParams;
pub use hexa_core::hash::{AlgoInfo, HashResult};
use hexa_core::task::Progress;
use hexa_core::{Cancel, Result, Tree};
use xxhash_rust::xxh3::Xxh3Default;
use xxhash_rust::xxh32::Xxh32;
use xxhash_rust::xxh64::Xxh64;

use crate::fuzzy::{SsdeepHasher, TlshHasher};
use crate::util::{self, CHUNK};

const FAST: &[(&str, &str)] = &[
    ("xxh32", "xxHash32"),
    ("xxh64", "xxHash64"),
    ("xxh3-64", "XXH3-64"),
    ("xxh3-128", "XXH3-128"),
    ("murmur3-32", "MurmurHash3 x86_32"),
    ("murmur3-128", "MurmurHash3 x64_128"),
    ("fnv1-32", "FNV-1 32"),
    ("fnv1a-32", "FNV-1a 32"),
    ("fnv1-64", "FNV-1 64"),
    ("fnv1a-64", "FNV-1a 64"),
    ("city64", "CityHash64"),
];

const FUZZY: &[(&str, &str)] = &[("ssdeep", "ssdeep (CTPH)"), ("tlsh", "TLSH")];

/// Algorithms provided by this crate (ANL-03 `fast`, ANL-04 `fuzzy`).
pub fn algorithms() -> Vec<AlgoInfo> {
    let fast = FAST.iter().map(|(id, name)| AlgoInfo {
        id: (*id).into(),
        name: (*name).into(),
        group: "fast",
    });
    let fuzzy = FUZZY.iter().map(|(id, name)| AlgoInfo {
        id: (*id).into(),
        name: (*name).into(),
        group: "fuzzy",
    });
    fast.chain(fuzzy).collect()
}

/// `true` when `id` is implemented by this crate (not by `hexa_core`).
pub fn is_local(id: &str) -> bool {
    FAST.iter().chain(FUZZY).any(|(i, _)| *i == id)
}

const FNV32_OFFSET: u32 = 0x811C_9DC5;
const FNV32_PRIME: u32 = 0x0100_0193;
const FNV64_OFFSET: u64 = 0xCBF2_9CE4_8422_2325;
const FNV64_PRIME: u64 = 0x0000_0100_0000_01B3;

/// FNV-1 / FNV-1a 32-bit over a whole buffer.
pub fn fnv32(data: &[u8], alternate: bool) -> u32 {
    fnv32_update(FNV32_OFFSET, data, alternate)
}

/// FNV-1 / FNV-1a 64-bit over a whole buffer.
pub fn fnv64(data: &[u8], alternate: bool) -> u64 {
    fnv64_update(FNV64_OFFSET, data, alternate)
}

fn fnv32_update(mut h: u32, data: &[u8], alternate: bool) -> u32 {
    if alternate {
        for &b in data {
            h = (h ^ u32::from(b)).wrapping_mul(FNV32_PRIME);
        }
    } else {
        for &b in data {
            h = h.wrapping_mul(FNV32_PRIME) ^ u32::from(b);
        }
    }
    h
}

fn fnv64_update(mut h: u64, data: &[u8], alternate: bool) -> u64 {
    if alternate {
        for &b in data {
            h = (h ^ u64::from(b)).wrapping_mul(FNV64_PRIME);
        }
    } else {
        for &b in data {
            h = h.wrapping_mul(FNV64_PRIME) ^ u64::from(b);
        }
    }
    h
}

/// Streaming `MurmurHash3` `x86_32` (seed 0).
#[derive(Debug, Clone, Default)]
pub struct Murmur3x86_32 {
    h: u32,
    tail: [u8; 4],
    tail_len: usize,
    len: u64,
}

const M32_C1: u32 = 0xCC9E_2D51;
const M32_C2: u32 = 0x1B87_3593;

impl Murmur3x86_32 {
    pub fn with_seed(seed: u32) -> Self {
        Self {
            h: seed,
            ..Self::default()
        }
    }

    fn block(&mut self, k: u32) {
        let k = k.wrapping_mul(M32_C1).rotate_left(15).wrapping_mul(M32_C2);
        self.h ^= k;
        self.h = self
            .h
            .rotate_left(13)
            .wrapping_mul(5)
            .wrapping_add(0xE654_6B64);
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.len += data.len() as u64;
        if self.tail_len > 0 {
            let take = (4 - self.tail_len).min(data.len());
            self.tail[self.tail_len..self.tail_len + take].copy_from_slice(&data[..take]);
            self.tail_len += take;
            data = &data[take..];
            if self.tail_len < 4 {
                return;
            }
            self.block(u32::from_le_bytes(self.tail));
            self.tail_len = 0;
        }
        let (blocks, rest) = data.as_chunks::<4>();
        for b in blocks {
            self.block(u32::from_le_bytes(*b));
        }
        self.tail[..rest.len()].copy_from_slice(rest);
        self.tail_len = rest.len();
    }

    pub fn finish(&self) -> u32 {
        let mut h = self.h;
        if self.tail_len > 0 {
            let mut k = 0u32;
            for (i, &b) in self.tail[..self.tail_len].iter().enumerate() {
                k ^= u32::from(b) << (8 * i);
            }
            h ^= k.wrapping_mul(M32_C1).rotate_left(15).wrapping_mul(M32_C2);
        }
        h ^= self.len as u32;
        h ^= h >> 16;
        h = h.wrapping_mul(0x85EB_CA6B);
        h ^= h >> 13;
        h = h.wrapping_mul(0xC2B2_AE35);
        h ^ (h >> 16)
    }
}

/// Streaming `MurmurHash3` `x64_128` (seed 0).
#[derive(Debug, Clone, Default)]
pub struct Murmur3x64_128 {
    h1: u64,
    h2: u64,
    tail: [u8; 16],
    tail_len: usize,
    len: u64,
}

const M128_C1: u64 = 0x87C3_7B91_1142_53D5;
const M128_C2: u64 = 0x4CF5_AD43_2745_937F;

fn fmix64(mut k: u64) -> u64 {
    k ^= k >> 33;
    k = k.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    k ^= k >> 33;
    k = k.wrapping_mul(0xC4CE_B9FE_1A85_EC53);
    k ^ (k >> 33)
}

impl Murmur3x64_128 {
    pub fn with_seed(seed: u32) -> Self {
        Self {
            h1: u64::from(seed),
            h2: u64::from(seed),
            ..Self::default()
        }
    }

    fn block(&mut self, b: &[u8; 16]) {
        let mut k1 = u64::from_le_bytes(b[..8].try_into().expect("8 bytes"));
        let mut k2 = u64::from_le_bytes(b[8..].try_into().expect("8 bytes"));
        k1 = k1
            .wrapping_mul(M128_C1)
            .rotate_left(31)
            .wrapping_mul(M128_C2);
        self.h1 ^= k1;
        self.h1 = self
            .h1
            .rotate_left(27)
            .wrapping_add(self.h2)
            .wrapping_mul(5)
            .wrapping_add(0x52DC_E729);
        k2 = k2
            .wrapping_mul(M128_C2)
            .rotate_left(33)
            .wrapping_mul(M128_C1);
        self.h2 ^= k2;
        self.h2 = self
            .h2
            .rotate_left(31)
            .wrapping_add(self.h1)
            .wrapping_mul(5)
            .wrapping_add(0x3849_5AB5);
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.len += data.len() as u64;
        if self.tail_len > 0 {
            let take = (16 - self.tail_len).min(data.len());
            self.tail[self.tail_len..self.tail_len + take].copy_from_slice(&data[..take]);
            self.tail_len += take;
            data = &data[take..];
            if self.tail_len < 16 {
                return;
            }
            let t = self.tail;
            self.block(&t);
            self.tail_len = 0;
        }
        let (blocks, rest) = data.as_chunks::<16>();
        for b in blocks {
            self.block(b);
        }
        self.tail[..rest.len()].copy_from_slice(rest);
        self.tail_len = rest.len();
    }

    /// Returns `(h1, h2)`.
    pub fn finish(&self) -> (u64, u64) {
        let (mut h1, mut h2) = (self.h1, self.h2);
        let tail = &self.tail[..self.tail_len];
        if tail.len() > 8 {
            let mut k2 = 0u64;
            for (i, &b) in tail[8..].iter().enumerate() {
                k2 ^= u64::from(b) << (8 * i);
            }
            h2 ^= k2
                .wrapping_mul(M128_C2)
                .rotate_left(33)
                .wrapping_mul(M128_C1);
        }
        if !tail.is_empty() {
            let mut k1 = 0u64;
            for (i, &b) in tail.iter().take(8).enumerate() {
                k1 ^= u64::from(b) << (8 * i);
            }
            h1 ^= k1
                .wrapping_mul(M128_C1)
                .rotate_left(31)
                .wrapping_mul(M128_C2);
        }
        h1 ^= self.len;
        h2 ^= self.len;
        h1 = h1.wrapping_add(h2);
        h2 = h2.wrapping_add(h1);
        h1 = fmix64(h1);
        h2 = fmix64(h2);
        h1 = h1.wrapping_add(h2);
        h2 = h2.wrapping_add(h1);
        (h1, h2)
    }
}

/// `MurmurHash3` `x86_32` of a buffer.
pub fn murmur3_32(data: &[u8], seed: u32) -> u32 {
    let mut h = Murmur3x86_32::with_seed(seed);
    h.update(data);
    h.finish()
}

/// `MurmurHash3` `x64_128` of a buffer as `(h1, h2)`.
pub fn murmur3_128(data: &[u8], seed: u32) -> (u64, u64) {
    let mut h = Murmur3x64_128::with_seed(seed);
    h.update(data);
    h.finish()
}

// ---------------------------------------------------------------- CityHash64

const K0: u64 = 0xC3A5_C85C_97CB_3127;
const K1: u64 = 0xB492_B66F_BE98_F273;
const K2: u64 = 0x9AE1_6A3B_2F90_404F;

fn f64le(s: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(s[at..at + 8].try_into().expect("8 bytes"))
}

fn f32le(s: &[u8], at: usize) -> u64 {
    u64::from(u32::from_le_bytes(
        s[at..at + 4].try_into().expect("4 bytes"),
    ))
}

fn shift_mix(v: u64) -> u64 {
    v ^ (v >> 47)
}

fn hash_len16_mul(u: u64, v: u64, mul: u64) -> u64 {
    let mut a = (u ^ v).wrapping_mul(mul);
    a ^= a >> 47;
    let mut b = (v ^ a).wrapping_mul(mul);
    b ^= b >> 47;
    b.wrapping_mul(mul)
}

fn hash_len16(u: u64, v: u64) -> u64 {
    hash_len16_mul(u, v, 0x9DDF_EA08_EB38_2D69)
}

fn city_0_to_16(s: &[u8]) -> u64 {
    let len = s.len();
    if len >= 8 {
        let mul = K2.wrapping_add(len as u64 * 2);
        let a = f64le(s, 0).wrapping_add(K2);
        let b = f64le(s, len - 8);
        let c = b.rotate_right(37).wrapping_mul(mul).wrapping_add(a);
        let d = a.rotate_right(25).wrapping_add(b).wrapping_mul(mul);
        return hash_len16_mul(c, d, mul);
    }
    if len >= 4 {
        let mul = K2.wrapping_add(len as u64 * 2);
        let a = f32le(s, 0);
        return hash_len16_mul((len as u64).wrapping_add(a << 3), f32le(s, len - 4), mul);
    }
    if len > 0 {
        let y = u32::from(s[0]).wrapping_add(u32::from(s[len >> 1]) << 8);
        let z = (len as u32).wrapping_add(u32::from(s[len - 1]) << 2);
        return shift_mix(u64::from(y).wrapping_mul(K2) ^ u64::from(z).wrapping_mul(K0))
            .wrapping_mul(K2);
    }
    K2
}

fn city_17_to_32(s: &[u8]) -> u64 {
    let len = s.len();
    let mul = K2.wrapping_add(len as u64 * 2);
    let a = f64le(s, 0).wrapping_mul(K1);
    let b = f64le(s, 8);
    let c = f64le(s, len - 8).wrapping_mul(mul);
    let d = f64le(s, len - 16).wrapping_mul(K2);
    hash_len16_mul(
        a.wrapping_add(b)
            .rotate_right(43)
            .wrapping_add(c.rotate_right(30))
            .wrapping_add(d),
        a.wrapping_add(b.wrapping_add(K2).rotate_right(18))
            .wrapping_add(c),
        mul,
    )
}

fn city_33_to_64(s: &[u8]) -> u64 {
    let len = s.len();
    let mul = K2.wrapping_add(len as u64 * 2);
    let a = f64le(s, 0).wrapping_mul(K2);
    let b = f64le(s, 8);
    let c = f64le(s, len - 24);
    let d = f64le(s, len - 32);
    let e = f64le(s, 16).wrapping_mul(K2);
    let f = f64le(s, 24).wrapping_mul(9);
    let g = f64le(s, len - 8);
    let h = f64le(s, len - 16).wrapping_mul(mul);
    let u = a
        .wrapping_add(g)
        .rotate_right(43)
        .wrapping_add(b.rotate_right(30).wrapping_add(c).wrapping_mul(9));
    let v = (a.wrapping_add(g) ^ d).wrapping_add(f).wrapping_add(1);
    let w = u
        .wrapping_add(v)
        .wrapping_mul(mul)
        .swap_bytes()
        .wrapping_add(h);
    let x = e.wrapping_add(f).rotate_right(42).wrapping_add(c);
    let y = v
        .wrapping_add(w)
        .wrapping_mul(mul)
        .swap_bytes()
        .wrapping_add(g)
        .wrapping_mul(mul);
    let z = e.wrapping_add(f).wrapping_add(c);
    let a = x
        .wrapping_add(z)
        .wrapping_mul(mul)
        .wrapping_add(y)
        .swap_bytes()
        .wrapping_add(b);
    let b = shift_mix(
        z.wrapping_add(a)
            .wrapping_mul(mul)
            .wrapping_add(d)
            .wrapping_add(h),
    )
    .wrapping_mul(mul);
    b.wrapping_add(x)
}

fn weak32(s: &[u8], at: usize, a: u64, b: u64) -> (u64, u64) {
    let (w, x, y, z) = (
        f64le(s, at),
        f64le(s, at + 8),
        f64le(s, at + 16),
        f64le(s, at + 24),
    );
    let a = a.wrapping_add(w);
    let b = b.wrapping_add(a).wrapping_add(z).rotate_right(21);
    let c = a;
    let a = a.wrapping_add(x).wrapping_add(y);
    let b = b.wrapping_add(a.rotate_right(44));
    (a.wrapping_add(z), b.wrapping_add(c))
}

/// `CityHash64` v1.1 of a buffer.
pub fn city_hash64(data: &[u8]) -> u64 {
    let mut c = CityHash64::new(
        data.len() as u64,
        data,
        &data[data.len().saturating_sub(64)..],
    );
    c.update(data);
    c.finish()
}

/// Streaming `CityHash64` v1.1. `CityHash` reads the tail of the input before
/// the body, so the constructor takes the total length, the first 8 bytes
/// and the last 64 bytes; [`update`](Self::update) then streams the body.
#[derive(Debug, Clone)]
pub struct CityHash64 {
    len: u64,
    /// Whole input when `len <= 64`, otherwise the pending partial block.
    pending: Vec<u8>,
    /// Bytes fed so far.
    fed: u64,
    /// Number of 64-byte body blocks still to process.
    blocks_left: u64,
    x: u64,
    y: u64,
    z: u64,
    v: (u64, u64),
    w: (u64, u64),
}

impl CityHash64 {
    /// `head` needs at least 8 bytes and `tail` the last 64 bytes when
    /// `len > 64`; both are ignored otherwise.
    pub fn new(len: u64, head: &[u8], tail: &[u8]) -> Self {
        let mut s = Self {
            len,
            pending: Vec::new(),
            fed: 0,
            blocks_left: 0,
            x: 0,
            y: 0,
            z: 0,
            v: (0, 0),
            w: (0, 0),
        };
        if len > 64 {
            let t = &tail[tail.len() - 64..];
            // Offsets relative to `len - 64`.
            let x = f64le(t, 24);
            let y = f64le(t, 48).wrapping_add(f64le(t, 8));
            let z = hash_len16(f64le(t, 16).wrapping_add(len), f64le(t, 40));
            s.v = weak32(t, 0, len, z);
            s.w = weak32(t, 32, y.wrapping_add(K1), x);
            s.x = x.wrapping_mul(K1).wrapping_add(f64le(head, 0));
            s.y = y;
            s.z = z;
            s.blocks_left = (len - 1) / 64;
        }
        s
    }

    fn block(&mut self, s: &[u8]) {
        let (mut x, mut y, mut z, mut v, mut w) = (self.x, self.y, self.z, self.v, self.w);
        x = x
            .wrapping_add(y)
            .wrapping_add(v.0)
            .wrapping_add(f64le(s, 8))
            .rotate_right(37)
            .wrapping_mul(K1);
        y = y
            .wrapping_add(v.1)
            .wrapping_add(f64le(s, 48))
            .rotate_right(42)
            .wrapping_mul(K1);
        x ^= w.1;
        y = y.wrapping_add(v.0).wrapping_add(f64le(s, 40));
        z = z.wrapping_add(w.0).rotate_right(33).wrapping_mul(K1);
        v = weak32(s, 0, v.1.wrapping_mul(K1), x.wrapping_add(w.0));
        w = weak32(s, 32, z.wrapping_add(w.1), y.wrapping_add(f64le(s, 16)));
        std::mem::swap(&mut z, &mut x);
        (self.x, self.y, self.z, self.v, self.w) = (x, y, z, v, w);
        self.blocks_left -= 1;
    }

    pub fn update(&mut self, mut data: &[u8]) {
        if self.len <= 64 {
            let room = (self.len - self.fed) as usize;
            let take = room.min(data.len());
            self.pending.extend_from_slice(&data[..take]);
            self.fed += take as u64;
            return;
        }
        self.fed += data.len() as u64;
        if !self.pending.is_empty() {
            let take = (64 - self.pending.len()).min(data.len());
            self.pending.extend_from_slice(&data[..take]);
            data = &data[take..];
            if self.pending.len() < 64 {
                return;
            }
            if self.blocks_left > 0 {
                let p = std::mem::take(&mut self.pending);
                self.block(&p);
            } else {
                self.pending.clear();
            }
        }
        let (blocks, rest) = data.as_chunks::<64>();
        for b in blocks {
            if self.blocks_left == 0 {
                return;
            }
            self.block(b);
        }
        if self.blocks_left > 0 {
            self.pending.extend_from_slice(rest);
        }
    }

    pub fn finish(&self) -> u64 {
        if self.len <= 64 {
            let s = &self.pending;
            return match s.len() {
                0..=16 => city_0_to_16(s),
                17..=32 => city_17_to_32(s),
                _ => city_33_to_64(s),
            };
        }
        hash_len16(
            hash_len16(self.v.0, self.w.0)
                .wrapping_add(shift_mix(self.y).wrapping_mul(K1))
                .wrapping_add(self.z),
            hash_len16(self.v.1, self.w.1).wrapping_add(self.x),
        )
    }
}

// ---------------------------------------------------------------- dispatcher

/// One of this crate's streaming hashers.
#[derive(Clone)]
pub(crate) enum LocalHasher {
    Xxh32(Xxh32),
    Xxh64(Xxh64),
    Xxh3_64(Box<Xxh3Default>),
    Xxh3_128(Box<Xxh3Default>),
    Murmur32(Murmur3x86_32),
    Murmur128(Murmur3x64_128),
    Fnv32 { h: u32, alternate: bool },
    Fnv64 { h: u64, alternate: bool },
    City(Box<CityHash64>),
    Ssdeep(Box<SsdeepHasher>),
    Tlsh(Box<TlshHasher>),
}

impl std::fmt::Debug for LocalHasher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LocalHasher")
    }
}

impl LocalHasher {
    /// Creates a hasher for `len` input bytes. `peek(offset, n)` returns input
    /// bytes (needed by `CityHash`, which reads its tail first).
    pub(crate) fn new(
        id: &str,
        len: u64,
        peek: &mut dyn FnMut(u64, usize) -> Result<Vec<u8>>,
    ) -> Option<Result<Self>> {
        Some(Ok(match id {
            "xxh32" => Self::Xxh32(Xxh32::new(0)),
            "xxh64" => Self::Xxh64(Xxh64::new(0)),
            "xxh3-64" => Self::Xxh3_64(Box::default()),
            "xxh3-128" => Self::Xxh3_128(Box::default()),
            "murmur3-32" => Self::Murmur32(Murmur3x86_32::default()),
            "murmur3-128" => Self::Murmur128(Murmur3x64_128::default()),
            "fnv1-32" | "fnv1a-32" => Self::Fnv32 {
                h: FNV32_OFFSET,
                alternate: id == "fnv1a-32",
            },
            "fnv1-64" | "fnv1a-64" => Self::Fnv64 {
                h: FNV64_OFFSET,
                alternate: id == "fnv1a-64",
            },
            "city64" => {
                if len > 64 {
                    let head = match peek(0, 8) {
                        Ok(h) => h,
                        Err(e) => return Some(Err(e)),
                    };
                    let tail = match peek(len - 64, 64) {
                        Ok(t) => t,
                        Err(e) => return Some(Err(e)),
                    };
                    Self::City(Box::new(CityHash64::new(len, &head, &tail)))
                } else {
                    Self::City(Box::new(CityHash64::new(len, &[], &[])))
                }
            }
            "ssdeep" => Self::Ssdeep(Box::new(SsdeepHasher::new(len))),
            "tlsh" => Self::Tlsh(Box::default()),
            _ => return None,
        }))
    }

    /// Whether the hash can be finalised at any prefix (no total length needed).
    pub(crate) fn is_incremental(&self) -> bool {
        !matches!(self, Self::City(_) | Self::Ssdeep(_))
    }

    pub(crate) fn update(&mut self, data: &[u8]) {
        match self {
            Self::Xxh32(h) => h.update(data),
            Self::Xxh64(h) => h.update(data),
            Self::Xxh3_64(h) | Self::Xxh3_128(h) => h.update(data),
            Self::Murmur32(h) => h.update(data),
            Self::Murmur128(h) => h.update(data),
            Self::Fnv32 { h, alternate } => *h = fnv32_update(*h, data, *alternate),
            Self::Fnv64 { h, alternate } => *h = fnv64_update(*h, data, *alternate),
            Self::City(h) => h.update(data),
            Self::Ssdeep(h) => h.update(data),
            Self::Tlsh(h) => h.update(data),
        }
    }

    pub(crate) fn finish(&self) -> String {
        match self {
            Self::Xxh32(h) => format!("{:08x}", h.digest()),
            Self::Xxh64(h) => format!("{:016x}", h.digest()),
            Self::Xxh3_64(h) => format!("{:016x}", h.digest()),
            Self::Xxh3_128(h) => format!("{:032x}", h.digest128()),
            Self::Murmur32(h) => format!("{:08x}", h.finish()),
            Self::Murmur128(h) => {
                let (a, b) = h.finish();
                let mut bytes = [0u8; 16];
                bytes[..8].copy_from_slice(&a.to_le_bytes());
                bytes[8..].copy_from_slice(&b.to_le_bytes());
                util::hex_lower(&bytes)
            }
            Self::Fnv32 { h, .. } => format!("{h:08x}"),
            Self::Fnv64 { h, .. } => format!("{h:016x}"),
            Self::City(h) => format!("{:016x}", h.finish()),
            Self::Ssdeep(h) => h.digest(),
            Self::Tlsh(h) => h.digest().unwrap_or_else(|| "TNULL".into()),
        }
    }
}

fn locals_for(
    ids: &[String],
    len: u64,
    peek: &mut dyn FnMut(u64, usize) -> Result<Vec<u8>>,
) -> Result<Vec<(usize, LocalHasher)>> {
    let mut out = Vec::new();
    for (i, id) in ids.iter().enumerate() {
        if let Some(h) = LocalHasher::new(id, len, peek) {
            out.push((i, h?));
        }
    }
    Ok(out)
}

fn merge(
    ids: &[String],
    locals: Vec<(usize, LocalHasher)>,
    core: Vec<HashResult>,
) -> Vec<HashResult> {
    let mut slots: Vec<Option<HashResult>> = vec![None; ids.len()];
    for (i, h) in locals {
        slots[i] = Some(HashResult {
            id: ids[i].clone(),
            value: h.finish(),
        });
    }
    let mut core = core.into_iter();
    for slot in &mut slots {
        if slot.is_none() {
            *slot = core.next();
        }
    }
    slots.into_iter().flatten().collect()
}

/// Hashes bytes with any mix of this crate's and `hexa_core`'s ids.
pub fn hash_bytes(
    data: &[u8],
    ids: &[String],
    custom: Option<CrcParams>,
) -> Result<Vec<HashResult>> {
    let mut peek = |off: u64, n: usize| Ok(data[off as usize..off as usize + n].to_vec());
    let mut locals = locals_for(ids, data.len() as u64, &mut peek)?;
    for (_, h) in &mut locals {
        h.update(data);
    }
    let core_ids: Vec<String> = ids.iter().filter(|i| !is_local(i)).cloned().collect();
    let core = if core_ids.is_empty() {
        Vec::new()
    } else {
        hexa_core::hash::hash_bytes(data, &core_ids, custom)?
    };
    Ok(merge(ids, locals, core))
}

/// Hashes `[start, end)` of a snapshot (ANL-03). Accepts any mix of this
/// crate's ids and `hexa_core::hash` ids; core ids are delegated to
/// [`hexa_core::hash::hash_range`] (a second pass over the data).
pub fn hash_range(
    tree: &Tree,
    start: u64,
    end: u64,
    ids: &[String],
    custom: Option<CrcParams>,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<Vec<HashResult>> {
    let (start, end) = util::clamp(tree, start, end);
    let len = end - start;
    let mut peek = |off: u64, n: usize| Ok(tree.read_vec(start + off, n)?);
    let mut locals = locals_for(ids, len, &mut peek)?;
    let core_ids: Vec<String> = ids.iter().filter(|i| !is_local(i)).cloned().collect();
    let passes = u64::from(!locals.is_empty()) + u64::from(!core_ids.is_empty());
    let mut pass = 0;
    let core = if core_ids.is_empty() {
        Vec::new()
    } else {
        pass += 1;
        let p = |d: u64, t: u64| progress(d, t * passes);
        hexa_core::hash::hash_range(tree, start, end, &core_ids, custom, cancel, &p)?
    };
    if !locals.is_empty() {
        let base = pass * len;
        let p = |d: u64, t: u64| progress(base + d, t * passes);
        util::scan(tree, start, end, CHUNK, cancel, &p, |_, chunk| {
            for (_, h) in &mut locals {
                h.update(chunk);
            }
            Ok(true)
        })?;
    }
    Ok(merge(ids, locals, core))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{noop, random_bytes, tree};

    fn one(id: &str, data: &[u8]) -> String {
        hash_bytes(data, &[id.to_string()], None)
            .unwrap()
            .remove(0)
            .value
    }

    #[test]
    fn fnv_vectors() {
        assert_eq!(one("fnv1-32", b""), "811c9dc5");
        assert_eq!(one("fnv1-32", b"a"), "050c5d7e");
        assert_eq!(one("fnv1a-32", b"a"), "e40c292c");
        assert_eq!(one("fnv1-32", b"foobar"), "31f0b262");
        assert_eq!(one("fnv1a-32", b"foobar"), "bf9cf968");
        assert_eq!(one("fnv1-64", b"a"), "af63bd4c8601b7be");
        assert_eq!(one("fnv1a-64", b"a"), "af63dc4c8601ec8c");
        assert_eq!(one("fnv1-64", b"foobar"), "340d8765a4dda9c2");
        assert_eq!(one("fnv1a-64", b"foobar"), "85944171f73967e8");
    }

    #[test]
    fn xxhash_vectors() {
        assert_eq!(one("xxh32", b""), "02cc5d05");
        assert_eq!(one("xxh32", b"abc"), "32d153ff");
        assert_eq!(one("xxh64", b""), "ef46db3751d8e999");
        assert_eq!(one("xxh64", b"abc"), "44bc2cf5ad770999");
        assert_eq!(one("xxh3-64", b""), "2d06800538d394c2");
        assert_eq!(one("xxh3-64", b"abc"), "78af5f94892f3950");
        assert_eq!(one("xxh3-128", b""), "99aa06d3014798d86001c324468d497f");
    }

    #[test]
    fn murmur3_vectors() {
        assert_eq!(murmur3_32(b"", 0), 0);
        assert_eq!(murmur3_32(b"", 1), 0x514E_28B7);
        assert_eq!(murmur3_32(b"hello", 0), 0x248B_FA47);
        assert_eq!(
            murmur3_32(b"The quick brown fox jumps over the lazy dog", 0),
            0x2E4F_F723
        );
        assert_eq!(
            one(
                "murmur3-128",
                b"The quick brown fox jumps over the lazy dog"
            ),
            "6c1b07bc7bbc4be347939ac4a93c437a"
        );
        assert_eq!(one("murmur3-128", b""), "00000000000000000000000000000000");
        // Cross-check against an independent implementation, all tail sizes.
        let data = random_bytes(7, 300);
        for n in 0..data.len() {
            let s = &data[..n];
            assert_eq!(
                murmur3_32(s, 0),
                murmur3::murmur3_32(&mut std::io::Cursor::new(s), 0).unwrap()
            );
            let (h1, h2) = murmur3_128(s, 0);
            let r = murmur3::murmur3_x64_128(&mut std::io::Cursor::new(s), 0).unwrap();
            assert_eq!((u128::from(h2) << 64) | u128::from(h1), r, "len {n}");
        }
    }

    /// The data generator and the first column of `testdata` from Google's
    /// `city-test.cc` (`CityHash` v1.1): `CityHash64(data + i*i, i)`.
    #[test]
    fn city_hash_vectors() {
        let mut data = vec![0u8; 1 << 17];
        let (mut a, mut b) = (9u64, 777u64);
        for (i, slot) in data.iter_mut().enumerate() {
            a = a.wrapping_add(b);
            b = b.wrapping_add(a);
            a = (a ^ (a >> 41)).wrapping_mul(K0);
            b = (b ^ (b >> 41)).wrapping_mul(K0).wrapping_add(i as u64);
            *slot = (b >> 37) as u8;
        }
        let expected: &[(usize, u64)] = &[
            (0, 0x9ae1_6a3b_2f90_404f),
            (1, 0x5411_50e8_7f41_5e96),
            (2, 0x0f37_86a4_b258_27c1),
            (3, 0xef92_3a7a_1af7_8eab),
            (4, 0x11df_5925_96f4_1d88),
            (7, 0x1b5a_063f_b4c7_f9f1),
            (8, 0xa0f1_0149_a0e5_38d6),
            (9, 0xfb8d_9c70_660b_910b),
            (15, 0x4447_3e03_be30_6c88),
            (16, 0x03ea_d5f2_1d34_4056),
            (17, 0x6abb_fde3_7ee0_3b5b),
            (20, 0x4182_832b_52d6_3735),
            (31, 0x55bd_b0e7_1e3e_debd),
            (32, 0x0782_fa1b_08b4_75e7),
            (33, 0xc5dc_19b8_76d3_7a80),
            (40, 0x4ec0_b54c_f156_6aff),
            (63, 0x1280_7833_c463_737c),
            (64, 0xe884_1992_2b87_176f),
            (65, 0x1051_91e0_ec8f_7f60),
            (100, 0x6369_1635_6581_4de6),
            (127, 0xcbaa_3cb8_f64f_54e0),
            (128, 0xb2e2_3e81_16c2_ba9f),
            (129, 0x8aa7_7f52_d786_8eb9),
            (200, 0x07fc_9800_6e25_cac9),
            (298, 0x74c0_b8a6_821f_aafe),
        ];
        for &(i, want) in expected {
            let s = &data[i * i..i * i + i];
            assert_eq!(city_hash64(s), want, "len {i}");
            // Streaming in odd-sized pieces gives the same value.
            let mut h = CityHash64::new(i as u64, s, &s[i.saturating_sub(64)..]);
            for piece in s.chunks(7) {
                h.update(piece);
            }
            assert_eq!(h.finish(), want, "streamed len {i}");
        }
    }

    #[test]
    fn range_matches_bytes_and_merges_core_ids() {
        let data = random_bytes(3, 5_000_000);
        let t = tree(&data);
        let ids: Vec<String> = [
            "xxh64",
            "crc:CRC-32/ISO-HDLC",
            "city64",
            "murmur3-128",
            "md5",
            "xxh3-128",
            "fnv1a-64",
            "xxh32",
            "murmur3-32",
            "xxh3-64",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        let a = hash_range(&t, 11, 4_900_001, &ids, None, &Cancel::new(), &noop).unwrap();
        let b = hash_bytes(&data[11..4_900_001], &ids, None).unwrap();
        assert_eq!(a.len(), ids.len());
        for ((x, y), id) in a.iter().zip(&b).zip(&ids) {
            assert_eq!(&x.id, id);
            assert_eq!(x.value, y.value, "{id}");
        }
        assert_eq!(
            a[1].value,
            hexa_core::hash::hash_bytes(&data[11..4_900_001], &ids[1..2], None).unwrap()[0].value
        );
        assert_eq!(
            a[0].value,
            format!(
                "{:016x}",
                xxhash_rust::xxh64::xxh64(&data[11..4_900_001], 0)
            )
        );
    }

    #[test]
    fn listing_and_errors() {
        let algos = algorithms();
        assert_eq!(algos.len(), FAST.len() + FUZZY.len());
        assert!(algos.iter().all(|a| is_local(&a.id)));
        assert!(hash_bytes(b"x", &["nope".into()], None).is_err());
        let r = hash_range(
            &tree(b""),
            0,
            0,
            &["xxh32".into()],
            None,
            &Cancel::new(),
            &noop,
        )
        .unwrap();
        assert_eq!(r[0].value, "02cc5d05");
    }

    #[test]
    fn cancel_is_honoured() {
        let c = Cancel::new();
        c.cancel();
        let r = hash_range(&tree(&[1; 10]), 0, 10, &["xxh32".into()], None, &c, &noop);
        assert!(matches!(r, Err(hexa_core::Error::Cancelled)));
    }
}
