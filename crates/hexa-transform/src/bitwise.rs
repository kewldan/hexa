//! Bitwise operations (XFRM-01): logic ops with an operand, NOT, shifts,
//! rotates and bit-order reversal, all per element of a given width.
//!
//! A trailing partial element (data length not a multiple of the width) is
//! left unchanged.

use serde::{Deserialize, Serialize};

use crate::types::{Endian, Width, map_elems};

/// Two-operand logic operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BitOp {
    Xor,
    And,
    Or,
    Nand,
    Nor,
}

/// Shift / rotate direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Direction {
    #[default]
    Left,
    Right,
}

impl Direction {
    /// The opposite direction.
    pub const fn opposite(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }
}

/// Applies `op` with `operand` (masked to `width`) to every element.
pub fn bit_op(data: &[u8], op: BitOp, operand: u64, width: Width, endian: Endian) -> Vec<u8> {
    let operand = operand & width.mask();
    if width == Width::W8 {
        let k = operand as u8;
        return data
            .iter()
            .map(|&b| match op {
                BitOp::Xor => b ^ k,
                BitOp::And => b & k,
                BitOp::Or => b | k,
                BitOp::Nand => !(b & k),
                BitOp::Nor => !(b | k),
            })
            .collect();
    }
    map_elems(data, width, endian, |v| match op {
        BitOp::Xor => v ^ operand,
        BitOp::And => v & operand,
        BitOp::Or => v | operand,
        BitOp::Nand => !(v & operand),
        BitOp::Nor => !(v | operand),
    })
}

/// Inverts every bit. Width does not matter for NOT.
pub fn not(data: &[u8]) -> Vec<u8> {
    data.iter().map(|b| !b).collect()
}

/// Logical shift of every element; bits shifted out are lost, zeros are
/// shifted in. `amount >= width` yields zero.
pub fn shift(data: &[u8], dir: Direction, amount: u32, width: Width, endian: Endian) -> Vec<u8> {
    map_elems(data, width, endian, |v| {
        if amount >= width.bits() {
            0
        } else {
            match dir {
                Direction::Left => v << amount,
                Direction::Right => v >> amount,
            }
        }
    })
}

/// Rotates every element by `amount` bits (taken modulo the width).
pub fn rotate(data: &[u8], dir: Direction, amount: u32, width: Width, endian: Endian) -> Vec<u8> {
    let bits = width.bits();
    let n = amount % bits;
    if n == 0 {
        return data.to_vec();
    }
    let n = match dir {
        Direction::Left => n,
        Direction::Right => bits - n,
    };
    map_elems(data, width, endian, |v| (v << n) | (v >> (bits - n)))
}

/// Reverses the bit order inside every element (bit 0 <-> bit width-1).
/// With width 8 this mirrors each byte; byte order is irrelevant because a
/// full mirror also reverses the bytes.
pub fn reverse_bits(data: &[u8], width: Width) -> Vec<u8> {
    let w = width.bytes();
    let mut out = data.to_vec();
    for chunk in out.chunks_exact_mut(w) {
        chunk.reverse();
        for b in chunk.iter_mut() {
            *b = b.reverse_bits();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xor_with_wide_operand_respects_endianness() {
        let data = [0u8; 6];
        assert_eq!(
            bit_op(&data, BitOp::Xor, 0x1122_3344, Width::W32, Endian::Little),
            [0x44, 0x33, 0x22, 0x11, 0, 0]
        );
        assert_eq!(
            bit_op(&data, BitOp::Xor, 0x1122_3344, Width::W32, Endian::Big),
            [0x11, 0x22, 0x33, 0x44, 0, 0]
        );
    }

    #[test]
    fn logic_ops_bytes() {
        let d = [0b1100_1010u8];
        assert_eq!(
            bit_op(&d, BitOp::And, 0x0F, Width::W8, Endian::Little),
            [0b0000_1010]
        );
        assert_eq!(
            bit_op(&d, BitOp::Or, 0x0F, Width::W8, Endian::Little),
            [0b1100_1111]
        );
        assert_eq!(
            bit_op(&d, BitOp::Nand, 0x0F, Width::W8, Endian::Little),
            [0b1111_0101]
        );
        assert_eq!(
            bit_op(&d, BitOp::Nor, 0x0F, Width::W8, Endian::Little),
            [0b0011_0000]
        );
        assert_eq!(not(&d), [0b0011_0101]);
    }

    #[test]
    fn nand_16_bit() {
        let d = [0xFF, 0x00];
        assert_eq!(
            bit_op(&d, BitOp::Nand, 0xFFFF, Width::W16, Endian::Big),
            [0x00, 0xFF]
        );
    }

    #[test]
    fn shifts_and_rotates() {
        let d = [0x81u8, 0x01];
        assert_eq!(
            shift(&d, Direction::Left, 1, Width::W8, Endian::Little),
            [0x02, 0x02]
        );
        assert_eq!(
            shift(&d, Direction::Right, 1, Width::W8, Endian::Little),
            [0x40, 0x00]
        );
        assert_eq!(
            rotate(&d, Direction::Left, 1, Width::W8, Endian::Little),
            [0x03, 0x02]
        );
        assert_eq!(
            rotate(&d, Direction::Right, 1, Width::W8, Endian::Little),
            [0xC0, 0x80]
        );
        // 16-bit LE value 0x0181 rotated left by 4 -> 0x1810
        assert_eq!(
            rotate(&d, Direction::Left, 4, Width::W16, Endian::Little),
            [0x10, 0x18]
        );
        // 16-bit BE value 0x8101 shifted right by 8 -> 0x0081
        assert_eq!(
            shift(&d, Direction::Right, 8, Width::W16, Endian::Big),
            [0x00, 0x81]
        );
        assert_eq!(
            shift(&d, Direction::Left, 16, Width::W16, Endian::Big),
            [0, 0]
        );
        assert_eq!(rotate(&d, Direction::Left, 16, Width::W16, Endian::Big), d);
        // 64-bit rotate
        let q = 0x0123_4567_89AB_CDEFu64.to_be_bytes();
        assert_eq!(
            rotate(&q, Direction::Left, 8, Width::W64, Endian::Big),
            0x2345_6789_ABCD_EF01u64.to_be_bytes()
        );
    }

    #[test]
    fn partial_tail_untouched() {
        let d = [1u8, 2, 3];
        assert_eq!(
            bit_op(&d, BitOp::Xor, 0xFFFF, Width::W16, Endian::Little),
            [0xFE, 0xFD, 3]
        );
    }

    #[test]
    fn bit_reversal() {
        assert_eq!(
            reverse_bits(&[0x01, 0x80, 0xF0], Width::W8),
            [0x80, 0x01, 0x0F]
        );
        let v = 0x0000_0001u32.to_le_bytes();
        assert_eq!(reverse_bits(&v, Width::W32), 0x8000_0000u32.to_le_bytes());
        assert_eq!(reverse_bits(&[0x12, 0x34], Width::W16), [0x2C, 0x48]);
    }
}
