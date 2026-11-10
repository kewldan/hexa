//! Helpers shared by unit tests.

use hexa_core::Tree;
use hexa_core::piece::Piece;

pub(crate) fn tree(data: &[u8]) -> Tree {
    Tree::from_piece(Piece::bytes(data.to_vec()))
}

pub(crate) fn noop(_: u64, _: u64) {}

/// Deterministic pseudo-random bytes (xorshift64*).
pub(crate) fn random_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 56) as u8
        })
        .collect()
}
